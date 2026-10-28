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
