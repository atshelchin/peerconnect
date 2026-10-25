// PeerConnect for JavaScript: browsers, Node.js (22+), Deno and Bun.
//
// The protocol runs in WebAssembly, compiled from the same Rust state
// machine as the native implementation; this file only supplies the
// WebSocket and the timers.

import { Group, Identity, initSync, Member, Peer, Room } from "./dist/peerconnect.js";
import module from "./dist/module.js";

initSync({ module: Uint8Array.from(atob(module), (c) => c.charCodeAt(0)) });

export { Group, Identity, Room };

/** Connects to a room and keeps it connected until left or closed. A room
 * that is closing carries on ending. */
export function connect(room) {
  return new Connection(typeof room === "string" ? Room.parse(room) : room);
}

const encoder = new TextEncoder();

export class Connection extends EventTarget {
  /** "offline" | "waiting" | "connected" | "closed" */
  state = "offline";
  /** Why the connection closed: "left" | "closed" | "closed-by-peer". */
  reason = undefined;

  #peer;
  #socket = null;
  #timer = undefined;
  #epoch = performance.now();

  constructor(room) {
    super();
    this.#peer = new Peer(room, this.#now());
    // Let the caller attach listeners before anything happens.
    queueMicrotask(() => this.#pump());
  }

  /** Bytes of messages waiting to be acknowledged by the peer. */
  get queued() {
    return this.#peer.queued;
  }

  /** The room as it stands now; save `room.toString()` after "paired" and after `close()`. */
  get room() {
    return this.#peer.room();
  }

  /**
   * Queues a message (bytes, or a string as UTF-8) and returns its id. It is
   * delivered as soon as the peer is reachable; "delivered" events report
   * progress. Throws if it is over 16 MiB, if 64 MiB are already waiting for
   * acknowledgement, or if the room is closed or closing.
   */
  send(data) {
    const bytes =
      typeof data === "string" ? encoder.encode(data) : data instanceof Uint8Array ? data : new Uint8Array(data);
    const id = this.#peer.send(bytes);
    this.#pump();
    return id;
  }

  /** Disconnects, keeping the room for later. Unacknowledged messages are dropped. */
  leave() {
    this.#peer.leave();
    this.#pump();
  }

  /**
   * Ends the room for both peers. The connection carries on until the peer
   * has been told, then its state becomes "closed" with reason "closed".
   * The room now says it is closing: save it, so that connecting with it
   * later finishes the job if this does not.
   */
  close() {
    this.#peer.close();
    this.#pump();
  }

  #now() {
    return performance.now() - this.#epoch;
  }

  #pump() {
    for (let out; (out = this.#peer.pollOutput()); ) {
      switch (out.type) {
        case "connect":
          this.#open(out.url);
          break;
        case "send":
          this.#socket?.send(out.data);
          break;
        case "ping":
          this.#socket?.send("ping");
          break;
        case "disconnect":
          this.#disconnect();
          break;
        case "state":
          this.state = out.state;
          this.reason = out.reason;
          this.dispatchEvent(new Event("state"));
          break;
        case "message":
          this.dispatchEvent(new MessageEvent("message", { data: out.data }));
          break;
        case "delivered":
          this.dispatchEvent(new CustomEvent("delivered", { detail: out.id }));
          break;
        case "paired":
        case "replaced":
          this.dispatchEvent(new Event(out.type));
          break;
      }
    }
    clearTimeout(this.#timer);
    const at = this.#peer.pollTimeout();
    if (at !== undefined) {
      this.#timer = setTimeout(() => {
        this.#peer.handleTimeout(this.#now());
        this.#pump();
      }, Math.max(0, at - this.#now()));
    }
  }

  #open(url) {
    const socket = new WebSocket(url);
    socket.binaryType = "arraybuffer";
    this.#socket = socket;
    const live = (handler) => (event) => {
      if (this.#socket !== socket) return;
      handler(event);
      this.#pump();
    };
    socket.onopen = live(() => this.#peer.handleOpen(this.#now()));
    socket.onmessage = live((event) =>
      typeof event.data === "string"
        ? this.#peer.handleText(this.#now(), event.data)
        : this.#peer.handleBinary(this.#now(), new Uint8Array(event.data)),
    );
    socket.onclose = live((event) => {
      this.#socket = null;
      this.#peer.handleClose(this.#now(), event.code);
    });
  }

  #disconnect() {
    const socket = this.#socket;
    this.#socket = null;
    if (socket) {
      socket.onopen = socket.onmessage = socket.onclose = null;
      socket.close(1000);
    }
  }
}

// Groups (GROUP.md): up to 32 members, commands, chat, voice and state transfer.

/** Bytes unsent on the socket beyond which voice frames are dropped. */
const VOICE_BACKLOG = 4 << 10;
/** Chat and state transfer are handed to the socket only while it has less than this unsent. */
const LOW_BACKLOG = 16 << 10;

/**
 * Joins a group (a `Group` or its invite) and stays in it, reconnecting as
 * needed, until left. `identity` defaults to a new one; persist
 * `identity.seed` to be recognised later. `profile` (a string or bytes, at
 * most 1 KiB) describes this member to the others.
 */
export function joinGroup(group, { identity, profile } = {}) {
  return new GroupConnection(
    typeof group === "string" ? Group.parse(group) : group,
    identity ?? Identity.generate(),
    bytes(profile ?? new Uint8Array()),
  );
}

export class GroupConnection extends EventTarget {
  /** "offline" | "online" | "closed" */
  state = "offline";
  /** The admitted members: member ID → profile (Uint8Array). */
  members = new Map();
  /** Our member ID. */
  id;

  #member;
  #socket = null;
  #timer = undefined;
  #epoch = performance.now();
  /** Chat and bulk frames waiting for the socket to drain. */
  #backlog = [[], []];
  #flushTimer = undefined;

  constructor(group, identity, profile) {
    super();
    this.#member = new Member(group, identity, profile, this.#now());
    this.id = this.#member.id;
    queueMicrotask(() => this.#pump());
  }

  /** The latest round trip to the relay, in milliseconds. */
  get rtt() {
    return this.#member.rtt;
  }

  /**
   * Broadcasts a message on "command" or "chat" and returns its id. It is
   * delivered to everyone present (and to us, as a "message" event), in
   * order, now or when we are back online.
   */
  send(channel, data) {
    return this.#do(() => this.#member.send(channel, bytes(data)));
  }

  /** Sends a message to one member only; not resent if the connection drops. */
  sendTo(member, channel, data) {
    this.#do(() => this.#member.sendTo(member, channel, bytes(data)));
  }

  /** Sends a voice frame (`seq` counts frames, silent ones included); dropped if the connection is backed up. */
  sendVoice(seq, data) {
    return this.#do(() => this.#member.sendVoice(seq & 0xffff, bytes(data)));
  }

  /** Sends data that is dropped rather than delayed, to one member or (no `to`) everyone. */
  sendDatagram(data, to) {
    return this.#do(() => this.#member.sendDatagram(to, bytes(data)));
  }

  /** Asks `from` for a snapshot; commands are held until the "snapshot" or "syncfailed" event. */
  requestSync(from) {
    this.#do(() => this.#member.requestSync(from));
  }

  /** Answers a "syncrequest" event with the snapshot taken when it was dispatched. */
  provideSync(to, transfer, snapshot) {
    this.#do(() => this.#member.provideSync(to, transfer, bytes(snapshot)));
  }

  declineSync(to, transfer) {
    this.#do(() => this.#member.declineSync(to, transfer));
  }

  /** Leaves the group. */
  leave() {
    this.#do(() => this.#member.leave());
  }

  #do(action) {
    const result = action();
    this.#pump();
    return result;
  }

  #now() {
    return performance.now() - this.#epoch;
  }

  #pump() {
    for (let out; (out = this.#member.pollOutput()); ) {
      switch (out.type) {
        case "connect":
          this.#open(out.url);
          break;
        case "send":
          this.#send(out.data, out.class);
          break;
        case "ping":
          this.#socket?.send("ping");
          break;
        case "disconnect":
          this.#disconnect();
          break;
        case "state":
          this.state = out.state;
          this.dispatchEvent(new Event("state"));
          break;
        case "joined":
          this.members.set(out.member, out.profile);
          this.#emit(out);
          break;
        case "left":
          this.members.delete(out.member);
          this.#emit(out);
          break;
        default:
          this.#emit(out);
      }
    }
    clearTimeout(this.#timer);
    const at = this.#member.pollTimeout();
    if (at !== undefined) {
      this.#timer = setTimeout(
        () => {
          this.#member.handleTimeout(this.#now());
          this.#pump();
        },
        Math.max(0, at - this.#now()),
      );
    }
  }

  #emit(out) {
    const { type, ...detail } = out;
    this.dispatchEvent(new CustomEvent(type, { detail }));
  }

  /** Commands go at once; voice only if the socket keeps up; chat and bulk as it drains. */
  #send(frame, kind) {
    const socket = this.#socket;
    if (!socket) return;
    if (kind === 0) socket.send(frame);
    else if (kind === 1) {
      if (socket.bufferedAmount <= VOICE_BACKLOG) socket.send(frame);
    } else {
      this.#backlog[kind - 2].push(frame);
      this.#flush();
    }
  }

  #flush() {
    clearTimeout(this.#flushTimer);
    const socket = this.#socket;
    if (!socket) return;
    while (socket.bufferedAmount < LOW_BACKLOG) {
      const frame = this.#backlog[0].shift() ?? this.#backlog[1].shift();
      if (!frame) return;
      socket.send(frame);
    }
    // WebSocket has no event for a drained buffer: look again shortly.
    this.#flushTimer = setTimeout(() => this.#flush(), 5);
  }

  #open(url) {
    const socket = new WebSocket(url);
    socket.binaryType = "arraybuffer";
    this.#socket = socket;
    const live = (handler) => (event) => {
      if (this.#socket !== socket) return;
      handler(event);
      this.#pump();
    };
    socket.onopen = live(() => this.#member.handleOpen(this.#now()));
    socket.onmessage = live((event) =>
      typeof event.data === "string"
        ? this.#member.handleText(this.#now(), event.data)
        : this.#member.handleBinary(this.#now(), new Uint8Array(event.data)),
    );
    socket.onclose = live((event) => {
      this.#socket = null;
      this.#backlog = [[], []];
      this.#member.handleClose(this.#now(), event.code);
    });
  }

  #disconnect() {
    const socket = this.#socket;
    this.#socket = null;
    this.#backlog = [[], []];
    if (socket) {
      socket.onopen = socket.onmessage = socket.onclose = null;
      socket.close(1000);
    }
  }
}

function bytes(data) {
  return typeof data === "string" ? encoder.encode(data) : data instanceof Uint8Array ? data : new Uint8Array(data);
}
