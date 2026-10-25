import type { GroupConnection } from "./index.js";

export interface VideoOptions {
  /** A stream whose video track to send (camera, screen, `canvas.captureStream()`); without one, only receive. */
  stream?: MediaStream;
  /** Encoded size (default 640 × 360) and frames per second (default 15). */
  width?: number;
  height?: number;
  framerate?: number;
  /** Bits per second to aim for (default 500000), and the floor it may step down to (default 100000). */
  bitrate?: number;
  minBitrate?: number;
  /** A WebCodecs codec string: "vp8", "avc1.42001f", "vp09.00.10.08" or "av01.0.04M.08"; the first supported by default. */
  codec?: string;
  /** Milliseconds between keyframes at most (default 2000); receivers that lose one ask for another sooner. */
  keyframeInterval?: number;
  /** Which of our tracks this is (default 0): camera and screen could be 0 and 1. */
  track?: number;
  /** Called with every decoded frame instead of closing it: close it when done. */
  onFrame?: (member: string, frame: VideoFrame, track: number) => void;
}

export interface VideoStats {
  bitrate: number;
  codec: string | null;
  /** Frames encoded, sent, keyframes among them, frames skipped (encoder behind), keyframe requests heard. */
  encoded: number;
  sent: number;
  keyframes: number;
  skipped: number;
  requests: number;
  members: { member: string; received: number; decoded: number; errors: number; lastTimestamp: number; width?: number; height?: number }[];
}

export interface VideoEventMap {
  error: CustomEvent<unknown>;
}

/** Video in a group: started by `startVideo`. */
export class Video extends EventTarget {
  /** Bits per second the encoder runs at now (it steps down while receivers keep losing frames). */
  readonly bitrate: number;
  readonly codec: string | null;
  /** Draws `member`'s video into `canvas` from now on; `null` stops. */
  attach(member: string, canvas: HTMLCanvasElement | OffscreenCanvas | null): void;
  /** Sends this stream's video track from now on; `null` stops sending (receiving goes on). */
  setStream(stream: MediaStream | null): Promise<void>;
  /** Pauses or resumes sending. */
  setSending(sending: boolean): void;
  stats(): VideoStats;
  stop(): void;
  addEventListener<K extends keyof VideoEventMap>(type: K, listener: (event: VideoEventMap[K]) => void): void;
}

/** Starts video: sends `stream`'s video track if given, and receives the others'. */
export function startVideo(conn: GroupConnection, options?: VideoOptions): Promise<Video>;
