// PeerConnect for JavaScript: browsers, Node.js (22+), Deno and Bun.
//
// The protocol runs in WebAssembly, compiled from the same Rust state
// machine as the native implementation; this file only supplies the
// sockets (WebSocket, and WebTransport for groups) and the timers.

import { Group, Identity, initSync, Member, Peer, Room, weeklyCertificateHashes } from "./dist/peerconnect.js";
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

// Groups (GROUP.md): up to four members, commands, chat, voice, video and
// state transfer, over WebTransport only (§3.6): a lost packet holds up only
// its own class, and voice and video wait for nothing. There is no WebSocket
// fallback; where WebTransport or UDP is not available, a member cannot join.

/** Bytes unsent on the voice stream beyond which voice frames are dropped. */
const VOICE_BACKLOG = 4 << 10;
/** Video waiting to be sent beyond which the oldest is dropped. */
const VIDEO_BACKLOG = 64 << 10;
/** WebTransport stream priorities, by class (0 command, 1 voice, 2 chat, 3 bulk, 4 video): higher goes first. */
const SEND_ORDER = [4, 3, 2, 0, 1];

/** Whether this runtime can join groups: it needs WebTransport. */
export function groupsSupported() {
  return typeof WebTransport === "function";
}

/**
 * Joins a group (a `Group` or its invite) and stays in it, reconnecting as
 * needed, until left. `identity` defaults to a new one; persist
 * `identity.seed` to be recognised later. `profile` (a string or bytes, at
 * most 1 KiB) describes this member to the others.
 *
 * It connects over WebTransport: to an `https://` relay as it is, and to a
 * `quic://` relay (one with no domain or certificate) pinning the relay's
 * weekly certificate (GROUP.md §3.7). `webTransport` is passed to the
 * `WebTransport` constructor (`serverCertificateHashes`, say). Throws a
 * "NotSupportedError" DOMException where there is no WebTransport.
 */
export function joinGroup(group, { identity, profile, webTransport } = {}) {
  if (!groupsSupported()) throw new DOMException("groups need WebTransport", "NotSupportedError");
  return new GroupConnection(
    typeof group === "string" ? Group.parse(group) : group,
    identity ?? Identity.generate(),
    bytes(profile ?? new Uint8Array()),
    webTransport,
  );
}

/**
 * The WebTransport session for a group endpoint (GROUP.md §3.6, §3.7):
 * `{ url, weekly }`, where `weekly` means pinning the weekly certificate;
 * `null` for anything but an `https://` or `quic://` URL.
 */
export function webTransportFor(url) {
  const match = /^(https|quic):\/\/([^/]+)(.*)$/.exec(url);
  if (!match) return null;
  const [, scheme, authority, path] = match;
  // quic:// is 443 by default, as https:// is.
  return { url: `https://${authority}${path}`, weekly: scheme === "quic" };
}

/** The SHA-256 hashes of the weekly certificates a relay may be serving now (GROUP.md §3.7). */
export function weeklyHashes(now = Date.now()) {
  return weeklyCertificateHashes(now).map((value) => ({ algorithm: "sha-256", value }));
}

export class GroupConnection extends EventTarget {
  /** "offline" | "online" | "closed" */
  state = "offline";
  /** The admitted members: member ID → profile (Uint8Array). */
  members = new Map();
  /** Our member ID. */
  id;
  /** Which of our video layers the relay last said are wanted (bit n: layer n), if it has. */
  videoDemand = undefined;
  /**
   * The largest video layer our link takes now, as the relay last said
   * ("videolimit"): 3, all of them, until it says otherwise; `null`, none
   * (the link is too slow: audio only).
   */
  videoLimit = 3;

  #member;
  /** The open session to the relay. */
  #link = null;
  /** The session being opened. */
  #opening = null;
  #webTransport;
  #timer = undefined;
  #epoch = performance.now();
  /** Video bytes dropped by sessions before the open one. */
  #videoDropped = 0;

  constructor(group, identity, profile, webTransport) {
    super();
    this.#member = new Member(group, identity, profile, this.#now());
    this.id = this.#member.id;
    this.#webTransport = webTransport;
    queueMicrotask(() => this.#pump());
  }

  /** Whether a WebTransport session to the relay is open now. */
  get connected() {
    return this.#link !== null;
  }

  /** The latest round trip to the relay, in milliseconds. */
  get rtt() {
    return this.#member.rtt;
  }

  /**
   * How far our own video is backed up on the way out: bytes waiting to be
   * sent now, and bytes dropped (the oldest, past the backlog) since we
   * joined. Cheap to read: `peerconnect/video` reads it every frame to stop
   * its largest layer while the uplink cannot carry them all.
   */
  get videoBacklog() {
    const link = this.#link;
    return { queued: link?.videoQueued ?? 0, dropped: this.#videoDropped + (link?.videoDropped ?? 0) };
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

  /**
   * Sends a video frame of `layer` (0–3, from the smallest picture up) of
   * `track` (up to 255,000 bytes), cut into fragments that each fit a
   * datagram; the relay forwards each member the layer it subscribed to,
   * and receivers get it whole, in order, as a "video" event, and ask for a
   * keyframe ("keyframerequest") when they lose one for good. Dropped,
   * oldest first, when the connection backs up. Send only the layers that
   * "videodemand" says are wanted. See `startVideo` in `peerconnect/video`
   * for camera video.
   */
  sendVideo(track, layer, keyframe, timestamp, data) {
    return this.#do(() => this.#member.sendVideo(track, layer, keyframe, timestamp >>> 0, bytes(data)));
  }

  /**
   * Asks the relay for `layer` of `member`'s video from now on (`null`: none
   * of it), also after either of us reconnects. It forwards the nearest layer
   * `member` sends, switching at a keyframe; until asked, layer 0 or the
   * nearest.
   */
  subscribe(member, layer) {
    this.#do(() => this.#member.subscribe(member, layer ?? undefined));
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
          this.#link?.ping();
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
        case "videodemand":
          this.videoDemand = out.layers;
          this.#emit(out);
          break;
        case "videolimit":
          this.#limit(out.layer);
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

  #limit(layer) {
    if (layer === this.videoLimit) return;
    this.videoLimit = layer;
    this.dispatchEvent(new CustomEvent("videolimit", { detail: { layer } }));
  }

  #send(frame, kind) {
    this.#link?.send(frame, kind);
  }

  /** Opens a WebTransport session to the group's endpoint. */
  #open(url) {
    const target = webTransportFor(url);
    const handlers = {
      open: (link) => {
        if (link !== this.#opening) return link.close();
        this.#opening = null;
        this.#link = link;
        // The relay starts each session with no limit, and says only when it sets one.
        this.#limit(3);
        this.#member.handleOpen(this.#now());
        this.#pump();
      },
      binary: (link, frame) => {
        if (link !== this.#link) return;
        this.#member.handleBinary(this.#now(), frame);
        this.#pump();
      },
      text: (link, text) => {
        if (link !== this.#link) return;
        this.#member.handleText(this.#now(), text);
        this.#pump();
      },
      close: (link, code) => {
        if (link === this.#link) {
          this.#link = null;
          this.#videoDropped += link.videoDropped;
        }
        else if (link === this.#opening) this.#opening = null;
        else return;
        this.#member.handleClose(this.#now(), code);
        this.#pump();
      },
    };
    if (!target) {
      queueMicrotask(() => {
        this.#member.handleClose(this.#now(), undefined);
        this.#pump();
      });
      return;
    }
    const options = target.weekly ? { serverCertificateHashes: weeklyHashes(), ...this.#webTransport } : this.#webTransport;
    this.#opening = new WebTransportLink(target.url, handlers, options);
  }

  #disconnect() {
    const [opening, link] = [this.#opening, this.#link];
    this.#opening = this.#link = null;
    this.#videoDropped += link?.videoDropped ?? 0;
    opening?.close();
    link?.close();
  }
}

/**
 * A WebTransport session to the relay (GROUP.md §3.6): each class on a
 * stream of its own, a byte naming the class and then `length[2] ‖ frame`
 * messages, so that a lost packet holds up only its class; voice as
 * datagrams, which wait for nothing. The empty message is ping, and pong.
 */
class WebTransportLink {
  #session;
  #on;
  #closed = false;
  #writers = [null, null, null, null, null];
  #queues = [[], [], [], [], []];
  #queued = [0, 0, 0, 0, 0];
  #writing = [false, false, false, false, false];
  #datagrams = null;
  /** Video frames waiting for room among the datagrams. */
  #video = [];
  #videoBytes = 0;
  /** Video bytes dropped from that queue, for falling behind. */
  #videoDropped = 0;
  #pacing = false;

  constructor(url, on, options) {
    this.#on = on;
    try {
      this.#session = new WebTransport(url, { requireUnreliable: true, ...options });
    } catch {
      queueMicrotask(() => this.#end(undefined));
      return;
    }
    const session = this.#session;
    session.ready.then(
      () => {
        if (this.#closed) return;
        const datagrams = session.datagrams;
        // Voice older than this is worth nothing: let the browser drop it.
        if ("outgoingMaxAge" in datagrams) datagrams.outgoingMaxAge = 200;
        this.#datagrams = (datagrams.createWritable?.() ?? datagrams.writable).getWriter();
        this.#readStreams();
        this.#readDatagrams();
        this.#on?.open(this);
        this.#paceVideo();
      },
      () => {},
    );
    session.closed.then(
      (info) => this.#end(info?.closeCode),
      () => this.#end(undefined),
    );
  }

  send(frame, kind) {
    if (kind === 4) {
      this.#video.push(frame);
      this.#videoBytes += frame.length;
      while (this.#videoBytes > VIDEO_BACKLOG) {
        const dropped = this.#video.shift().length;
        this.#videoBytes -= dropped;
        this.#videoDropped += dropped;
      }
      this.#paceVideo();
      return;
    }
    if (kind === 1 && this.#datagrams && frame.length <= (this.#session.datagrams.maxDatagramSize ?? 1024)) {
      this.#datagrams.write(frame).catch(() => {});
      return;
    }
    // Late audio is worse than lost audio.
    if (kind === 1 && this.#queued[1] > VOICE_BACKLOG) return;
    this.#queue(kind, frame);
  }

  ping() {
    this.#queue(0, new Uint8Array(0));
  }

  /** Video bytes waiting for room among the datagrams (or on the video stream). */
  get videoQueued() {
    return this.#videoBytes + this.#queued[4];
  }

  get videoDropped() {
    return this.#videoDropped;
  }

  /**
   * Hands video to the browser's datagram queue only when that is empty,
   * so that voice, which joins the same queue, waits behind one video
   * datagram at most; video too big for a datagram goes on its stream.
   */
  async #paceVideo() {
    if (this.#pacing) return;
    this.#pacing = true;
    try {
      while (this.#video.length && !this.#closed) {
        const writer = this.#datagrams;
        if (!writer) return; // not open yet: sent once it is
        const frame = this.#video[0];
        if (frame.length > (this.#session.datagrams.maxDatagramSize ?? 1024)) {
          this.#videoBytes -= this.#video.shift().length;
          this.#queue(4, frame);
          continue;
        }
        if (writer.desiredSize !== null && writer.desiredSize <= 0) {
          await Promise.race([writer.ready, new Promise((resolve) => setTimeout(resolve, 5))]);
          continue;
        }
        this.#videoBytes -= this.#video.shift().length;
        writer.write(frame).catch(() => {});
      }
    } finally {
      this.#pacing = false;
    }
  }

  /** Closes it, without telling `on`. */
  close() {
    this.#on = null;
    if (this.#closed) return;
    this.#closed = true;
    try {
      this.#session?.close({ closeCode: 1000, reason: "" });
    } catch {
      // Not open yet, or already gone.
    }
  }

  #end(code) {
    const on = this.#on;
    this.#on = null;
    this.#closed = true;
    on?.close(this, code);
  }

  #queue(kind, frame) {
    const message = new Uint8Array(2 + frame.length);
    message[0] = frame.length >> 8;
    message[1] = frame.length & 0xff;
    message.set(frame, 2);
    this.#queues[kind].push(message);
    this.#queued[kind] += message.length;
    this.#drain(kind);
  }

  /** Writes a class's queue to its stream, opened when first needed, as fast as the stream takes it. */
  async #drain(kind) {
    if (this.#writing[kind]) return;
    this.#writing[kind] = true;
    try {
      let writer = this.#writers[kind];
      if (!writer) {
        const stream = await this.#session.createUnidirectionalStream({ sendOrder: SEND_ORDER[kind] });
        writer = this.#writers[kind] = stream.getWriter();
        writer.write(new Uint8Array([kind])).catch(() => {});
      }
      while (this.#queues[kind].length && !this.#closed) {
        await writer.ready;
        const batch = concat(this.#queues[kind].splice(0));
        this.#queued[kind] = 0;
        writer.write(batch).catch(() => {});
      }
    } catch {
      // The session is gone: `closed` says so.
    } finally {
      this.#writing[kind] = false;
    }
  }

  async #readStreams() {
    const streams = this.#session.incomingUnidirectionalStreams.getReader();
    for (;;) {
      const { value, done } = await streams.read().catch(() => ({ done: true }));
      if (done || this.#closed) return;
      this.#readStream(value);
    }
  }

  async #readStream(stream) {
    const reader = stream.getReader();
    let buf = new Uint8Array(0);
    let headed = false;
    for (;;) {
      const { value, done } = await reader.read().catch(() => ({ done: true }));
      if (done || this.#closed) return;
      buf = buf.length ? concat([buf, value]) : value;
      let at = 0;
      if (!headed && buf.length) {
        headed = true;
        at = 1;
      }
      while (buf.length - at >= 2) {
        const length = (buf[at] << 8) | buf[at + 1];
        if (buf.length - at - 2 < length) break;
        const message = buf.slice(at + 2, at + 2 + length);
        at += 2 + length;
        if (length === 0) this.#on?.text(this, "pong");
        else this.#on?.binary(this, message);
      }
      buf = buf.subarray(at);
    }
  }

  async #readDatagrams() {
    const reader = this.#session.datagrams.readable.getReader();
    for (;;) {
      const { value, done } = await reader.read().catch(() => ({ done: true }));
      if (done || this.#closed) return;
      this.#on?.binary(this, value);
    }
  }
}

function concat(parts) {
  let length = 0;
  for (const part of parts) length += part.length;
  const out = new Uint8Array(length);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

function bytes(data) {
  return typeof data === "string" ? encoder.encode(data) : data instanceof Uint8Array ? data : new Uint8Array(data);
}
