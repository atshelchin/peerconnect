/** Room credentials. Treat `toString()` as a secret. */
export class Room {
  private constructor();
  /** Creates a room on a relay (`ws://` or `wss://` URL). You become the inviter. */
  static create(relay: string): Room;
  /** Accepts an invite. You become the joiner. Save `toString()` before connecting: it holds the identity the inviter will pin. */
  static join(invite: string): Room;
  /** Restores a room saved with `toString()`. */
  static parse(record: string): Room;
  /** The record to persist in order to reconnect later. */
  toString(): string;
  /** The invite to hand to the other peer, until the inviter has paired. */
  readonly invite: string | undefined;
  readonly id: string;
  readonly role: "inviter" | "joiner";
  readonly relay: string;
  /** Whether pairing is complete: the room has moved to its pair ID, out of the invite's reach. */
  readonly paired: boolean;
  /** Whether the room is being ended. */
  readonly closing: boolean;
  readonly publicKey: Uint8Array;
  readonly peerKey: Uint8Array | undefined;
  /** Releases the WebAssembly memory now rather than at garbage collection. */
  free(): void;
}

export type State = "offline" | "waiting" | "connected" | "closed";
export type CloseReason = "left" | "closed" | "closed-by-peer";

export interface ConnectionEventMap {
  /** `state` (and `reason`) changed. */
  state: Event;
  /** A message from the peer, authenticated, decrypted and in order; `data` is a Uint8Array. */
  message: MessageEvent<Uint8Array>;
  /** The peer has received every message whose id is below `detail`. */
  delivered: CustomEvent<number>;
  /** Pairing is complete and the room has moved to its pair ID: save `room.toString()` again. */
  paired: Event;
  /**
   * Another connection took this peer's seat, most likely the same room opened
   * elsewhere. The connection retries in a minute; call `leave()` to let the
   * other one keep it.
   */
  replaced: Event;
}

/** A live room connection. It reconnects on its own until left or closed. */
export class Connection extends EventTarget {
  /**
   * `offline`: not on the relay (connecting or waiting to retry);
   * `waiting`: on the relay, peer not (yet) connected;
   * `connected`: end-to-end session established;
   * `closed`: stopped for good, see `reason`.
   */
  readonly state: State;
  readonly reason: CloseReason | undefined;
  /** The room as it stands now. */
  readonly room: Room;
  /** Bytes of messages waiting to be acknowledged by the peer. */
  readonly queued: number;
  /**
   * Queues a message (strings as UTF-8), up to 16 MiB, and returns its id. It
   * is delivered as soon as the peer is reachable. Throws when 64 MiB already
   * await acknowledgement, or once the room is closed or closing.
   */
  send(data: Uint8Array | ArrayBuffer | string): number;
  /** Disconnects, keeping the room for later. Unacknowledged messages are dropped. */
  leave(): void;
  /**
   * Ends the room for both peers: the connection carries on until the peer has
   * been told, then closes with reason "closed". Save `room` (it is now closing)
   * so that connecting with it later finishes the job; delete it once closed.
   */
  close(): void;

  addEventListener<K extends keyof ConnectionEventMap>(
    type: K,
    listener: (event: ConnectionEventMap[K]) => void,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions): void;
}

/** Connects to a room (a `Room` or its saved string). */
export function connect(room: Room | string): Connection;

/** A group: its relay and secret. `toString()` is the invite, a secret for the group's members. */
export class Group {
  private constructor();
  /** Creates a new group on a relay (`ws://` or `wss://` URL). */
  static create(relay: string): Group;
  /** Parses an invite. */
  static parse(invite: string): Group;
  /** The invite. */
  toString(): string;
  readonly id: string;
  readonly relay: string;
  free(): void;
}

/** A member identity (an Ed25519 key pair). Persist `seed` to be recognised again. */
export class Identity {
  private constructor();
  static generate(): Identity;
  static fromSeed(seed: Uint8Array): Identity;
  readonly seed: Uint8Array;
  /** The member ID others know this identity by. */
  readonly id: string;
  free(): void;
}

export type GroupState = "offline" | "online" | "closed";
export type Channel = "command" | "chat";
export type Data = Uint8Array | ArrayBuffer | string;

export interface GroupEventMap {
  state: Event;
  /** A member was admitted (its identity is proven). */
  joined: CustomEvent<{ member: string; profile: Uint8Array }>;
  left: CustomEvent<{ member: string }>;
  /** A message, in its stream's order (our own included), or to us alone (`direct`). */
  message: CustomEvent<{ from: string; channel: Channel; data: Uint8Array; direct: boolean }>;
  voice: CustomEvent<{ from: string; seq: number; data: Uint8Array }>;
  datagram: CustomEvent<{ from: string; data: Uint8Array }>;
  /**
   * A whole video frame, in order and decodable (it follows a keyframe without loss). A
   * track's frames come from one `layer` at a time; another starts with a keyframe.
   */
  video: CustomEvent<{ from: string; track: number; layer: number; keyframe: boolean; timestamp: number; data: Uint8Array }>;
  /** Members need a keyframe of `layer` of our `track` (they lost some, or are moving to it): send one soon. */
  keyframerequest: CustomEvent<{ from: string; track: number; layer: number }>;
  /**
   * Which of our video layers someone wants (bit n: layer n). Encode only those; for a
   * wanted layer we do not send, the nearest below it, or else our lowest.
   */
  videodemand: CustomEvent<{ layers: number }>;
  /** `count` messages of `from`'s stream were lost (it no longer had them). */
  missed: CustomEvent<{ from: string; channel: Channel; count: number }>;
  /**
   * `from` asks for a snapshot: take it now, in this handler, so that it holds
   * exactly the commands delivered so far, then call `provideSync` (or `declineSync`).
   */
  syncrequest: CustomEvent<{ from: string; transfer: number }>;
  /** The snapshot asked for: replace the application state with it. The commands that follow are exactly those it lacks. */
  snapshot: CustomEvent<{ from: string; data: Uint8Array }>;
  syncfailed: CustomEvent<{ from: string }>;
  /** Our identity joined from elsewhere and took our slot; we retry in 30–60 s (call `leave()` to stop). */
  replaced: CustomEvent<{}>;
  /** The group is full on this relay; we retry later. */
  full: CustomEvent<{}>;
}

/** A group membership. It reconnects on its own until left. */
export class GroupConnection extends EventTarget {
  readonly state: GroupState;
  /** Our member ID. */
  readonly id: string;
  /** The admitted members: member ID → profile. */
  readonly members: Map<string, Uint8Array>;
  /** The latest round trip to the relay in milliseconds. */
  readonly rtt: number | undefined;
  /** Whether a WebTransport session to the relay is open now. */
  readonly connected: boolean;
  /** The latest "videodemand" (bit n: layer n is wanted), undefined until the relay says. */
  readonly videoDemand: number | undefined;
  /** Broadcasts a message (up to 16,352 bytes) and returns its id. */
  send(channel: Channel, data: Data): number;
  /** Sends to one member only; not resent if the connection drops. */
  sendTo(member: string, channel: Channel, data: Data): void;
  /** Sends a voice frame; false if not sent. */
  sendVoice(seq: number, data: Data): boolean;
  /** Sends data that is dropped rather than delayed; false if not sent. */
  sendDatagram(data: Data, to?: string): boolean;
  /**
   * Sends a video frame of `layer` (0–3, smallest picture first) of `track` (up to
   * 255,000 bytes); the relay forwards each member the layer it subscribed to, which
   * gets it whole and in order ("video"), or asks for a keyframe ("keyframerequest").
   * False if not sent.
   */
  sendVideo(track: number, layer: number, keyframe: boolean, timestamp: number, data: Data): boolean;
  /**
   * Asks the relay for `layer` (0–3) of `member`'s video from now on, `null` for none.
   * It forwards the nearest layer `member` sends, switching at a keyframe; layer 0 (or
   * the nearest) until asked.
   */
  subscribe(member: string, layer: number | null | undefined): void;
  requestSync(from: string): void;
  provideSync(to: string, transfer: number, snapshot: Data): void;
  declineSync(to: string, transfer: number): void;
  leave(): void;

  addEventListener<K extends keyof GroupEventMap>(
    type: K,
    listener: (event: GroupEventMap[K]) => void,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions): void;
}

/** Whether this runtime can join groups: they travel over WebTransport only. */
export function groupsSupported(): boolean;

/**
 * Joins a group (a `Group` or its invite: an `https://` or `quic://` relay)
 * over WebTransport, pinning the weekly certificate for a `quic://` relay.
 * There is no WebSocket fallback. Throws a "NotSupportedError" DOMException
 * where the runtime has no WebTransport (see `groupsSupported`).
 */
export function joinGroup(
  group: Group | string,
  options?: {
    identity?: Identity;
    profile?: Data;
    /** Passed to the `WebTransport` constructor: `serverCertificateHashes`, say. */
    webTransport?: WebTransportOptions;
  },
): GroupConnection;

/** The WebTransport session for a group endpoint (GROUP.md §3.6, §3.7); null for anything but `https://` and `quic://`. */
export function webTransportFor(url: string): { url: string; weekly: boolean } | null;

/** The hashes of the weekly certificates a relay without certificate files may be serving (GROUP.md §3.7). */
export function weeklyHashes(now?: number): { algorithm: "sha-256"; value: Uint8Array }[];
