import type { GroupConnection } from "./index.js";

export interface VoiceOptions {
  /** Opus bits per second (default 32000). */
  bitrate?: number;
  /**
   * How many earlier packets each one carries a copy of, so that the packets after bring back
   * those lost (default 2). 1 covers lone losses; 2, bursts as well, for about a fifth more
   * bandwidth; 0 sends none. Receivers that know nothing of the copies play the packets as
   * before.
   */
  redundancy?: 0 | 1 | 2;
  /** Frame duration: 10, 20 (default), 40 or 60 ms. Longer frames send fewer packets and add latency. */
  frameMs?: 10 | 20 | 40 | 60;
  /** Send only while speaking (default true). */
  vad?: boolean;
  /** Level in dBFS above which the microphone counts as speech (default -50). */
  threshold?: number;
  /**
   * Bounds of the adaptive jitter buffer, in ms (default 30 and 500). It stays near the lower
   * bound on a steady network; the upper one is what it may grow to on a bad one, trading
   * delay for audio that does not break up.
   */
  minDelay?: number;
  maxDelay?: number;
  /** A stream to send instead of the microphone. */
  stream?: MediaStream;
  /** `false`: start listening only, without asking for the microphone (`setMicrophone(true)` later). */
  microphone?: boolean;
  /** An AudioContext to use (48 kHz); one is created otherwise. */
  context?: AudioContext;
  /** Called as each of our frames is captured (`now` from `performance.now()`). */
  onCapture?: (seq: number, now: number) => void;
  /** Called as a frame of another member starts to play. */
  onPlay?: (member: string, seq: number, now: number) => void;
}

export interface SpeakerStats {
  member: string;
  /** Recent level (RMS, 0–1). */
  rms: number;
  /** Audio buffered, and the jitter buffer's current target, in seconds. */
  delay: number;
  target: number;
  played: number;
  /** Frames played from a copy in a later packet, the frame itself having been lost (or not yet come). */
  recovered: number;
  /** Frames that arrived after their time (copies too late to help are not counted). */
  late: number;
  /** Frames missing at their time, copies and all: made up from the last pitch period heard, fading out. */
  concealed: number;
  /** Frames dropped to catch up after a stall. */
  dropped: number;
  /** Times the buffer ran dry (at the end of each talk spurt, as a rule) and waited to fill again. */
  underruns: number;
}

export interface VoiceEventMap {
  /** Someone (our own member ID included) started or stopped speaking. */
  speaking: CustomEvent<{ member: string; speaking: boolean }>;
  error: CustomEvent<unknown>;
}

/** Voice in a group: started by `startVoice`. */
export class Voice extends EventTarget {
  readonly muted: boolean;
  readonly deafened: boolean;
  /** The microphone's level over the latest frame (RMS, 0–1), muted or not: for a level meter. */
  readonly micLevel: number;
  /** Whether our voice is being sent right now: unmuted, and speaking (or `vad: false`). */
  readonly transmitting: boolean;
  /** Who is speaking now. */
  readonly speaking: Set<string>;
  setMuted(muted: boolean): void;
  /** Whether a microphone (or stream) is being captured. */
  readonly microphone: boolean;
  /** Turns the microphone on (asking for it, from `deviceId` if given) or off (releasing it). */
  setMicrophone(on: boolean, options?: { deviceId?: string }): Promise<void>;
  /** Sends `stream` instead of the microphone; `null` sends nothing. */
  setStream(stream: MediaStream | null): void;
  setDeafened(deafened: boolean): void;
  /** One member's volume: 1 is normal, 0 silent. */
  setVolume(member: string, gain: number): void;
  stats(): Map<string, SpeakerStats>;
  stop(): Promise<void>;
  addEventListener<K extends keyof VoiceEventMap>(
    type: K,
    listener: (event: VoiceEventMap[K]) => void,
    options?: boolean | AddEventListenerOptions,
  ): void;
  addEventListener(type: string, listener: EventListenerOrEventListenerObject, options?: boolean | AddEventListenerOptions): void;
}

/**
 * Starts voice in a group: captures the microphone (with echo cancellation,
 * noise suppression and gain control), sends it as Opus while you speak, and
 * plays everyone else through adaptive jitter buffers. Call it from a user
 * gesture. Needs WebCodecs audio with Opus; rejects where it is missing.
 */
export function startVoice(conn: GroupConnection, options?: VoiceOptions): Promise<Voice>;

/**
 * The voice payload's packet (GROUP.md §7.2) with copies of earlier packets, `[back, packet]`
 * (the packet `back` frames before), in its Opus padding: for implementations that want to
 * send them too. Copies that would take the payload past 1,000 bytes are left out.
 */
export function withCopies(packet: Uint8Array, copies: [number, Uint8Array][]): Uint8Array;
/** The copies of earlier packets that an Opus packet carries in its padding, as `[back, packet]`. */
export function copiesIn(packet: Uint8Array): [number, Uint8Array][];
