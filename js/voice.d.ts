import type { GroupConnection } from "./index.js";

export interface VoiceOptions {
  /** Opus bits per second (default 24000). */
  bitrate?: number;
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
  /** Frames that arrived after their time. */
  late: number;
  /** Frames missing at their time, replaced by silence. */
  concealed: number;
  /** Frames dropped to catch up after a stall. */
  dropped: number;
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
  /** Who is speaking now. */
  readonly speaking: Set<string>;
  setMuted(muted: boolean): void;
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
