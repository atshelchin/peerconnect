import type { GroupConnection } from "./index.js";

/** One size of our picture that we send (simulcast): receivers each get the layer that suits them. */
export interface VideoLayer {
  /** 0–3, from the smallest picture up; the relay forwards each receiver the layer it subscribed to, or the nearest. */
  layer: number;
  /** The largest size to encode this layer at, turned to the picture's shape: a phone held upright sends 360 × 640. */
  width: number;
  height: number;
  /** Bits per second to aim for, and the floor it may step down to while its receivers keep losing frames. */
  bitrate: number;
  minBitrate?: number;
}

export interface VideoOptions {
  /** A stream whose video track to send (camera, screen, `canvas.captureStream()`); without one, only receive. */
  stream?: MediaStream;
  /**
   * The layers to send, e.g. `[{ layer: 1, width: 640, height: 360, bitrate: 400_000 },
   * { layer: 2, width: 1280, height: 720, bitrate: 1_500_000 }]`. Only those someone wants
   * are encoded; until the relay says, only the lowest. Without `layers`, one: layer 0,
   * of `width` × `height` at `bitrate`.
   */
  layers?: VideoLayer[];
  /**
   * Without `layers`, the largest size to encode at (default 640 × 360), turned to the
   * picture's shape: a phone held upright sends 360 × 640.
   */
  width?: number;
  height?: number;
  /** Frames per second (default 15), for every layer. */
  framerate?: number;
  /** Without `layers`, bits per second to aim for (default 500000), and the floor it may step down to (default 100000). */
  bitrate?: number;
  minBitrate?: number;
  /** A WebCodecs codec string: "vp8", "avc1.42001f", "vp09.00.10.08" or "av01.0.04M.08"; the first supported by default. */
  codec?: string;
  /** Milliseconds between keyframes at most (default 2000); receivers that lose one ask for another sooner. */
  keyframeInterval?: number;
  /** Which of our tracks this is (default 0): camera and screen could be 0 and 1. */
  track?: number;
  /**
   * The layer to receive of members with no canvas attached: `null` (none) by default,
   * 0 when `onFrame` is given, which sees every member's frames.
   */
  unattached?: number | null;
  /** Called with every decoded frame (and the layer it came in) instead of closing it: close it when done. */
  onFrame?: (member: string, frame: VideoFrame, track: number, layer: number) => void;
}

export interface VideoLayerStats {
  layer: number;
  /** Whether it is being encoded now: someone wants it. */
  running: boolean;
  /** The size it is encoded at (the configured one until the first frame). */
  width: number;
  height: number;
  /** Bits per second it runs at now. */
  bitrate: number;
  /** Frames encoded, sent, keyframes among them, frames skipped (encoder behind), keyframe requests heard. */
  encoded: number;
  sent: number;
  keyframes: number;
  skipped: number;
  requests: number;
}

export interface VideoStats {
  /** All running layers together. */
  bitrate: number;
  codec: string | null;
  /** The layers the relay says are wanted of us (bit n: layer n), null until it says. */
  demand: number | null;
  /** The layers' numbers, added up. */
  encoded: number;
  sent: number;
  keyframes: number;
  skipped: number;
  requests: number;
  layers: VideoLayerStats[];
  members: {
    member: string;
    /** The layer we subscribed to (null: none), and the one arriving now. */
    subscribed: number | null | undefined;
    layer: number;
    received: number;
    decoded: number;
    errors: number;
    lastTimestamp: number;
    width?: number;
    height?: number;
    /** Frames received and decoded, by layer. */
    layers: { layer: number; received: number; decoded: number }[];
  }[];
}

export interface VideoEventMap {
  error: CustomEvent<unknown>;
}

/** Video in a group: started by `startVideo`. */
export class Video extends EventTarget {
  /** Bits per second our encoders run at now, all layers (each steps down while its receivers keep losing frames). */
  readonly bitrate: number;
  readonly codec: string | null;
  /**
   * Draws `member`'s video into `canvas` from now on; `null` stops. Our own ID shows what
   * we send. Unless set with `setLayer`, we receive the layer that suits the size the
   * canvas is shown at (its short side in device pixels: up to 180 layer 0, 360 layer 1,
   * 720 layer 2, else 3), none while it has no size (hidden) or the page is hidden, and
   * the largest for a canvas that is not on the page (an OffscreenCanvas, say).
   */
  attach(member: string, canvas: HTMLCanvasElement | OffscreenCanvas | null): void;
  /** Which layer of `member`'s video to receive: 0–3, `null` for none, or "auto" (the default) to follow its canvas. */
  setLayer(member: string, layer: number | null | "auto"): void;
  /** Turns what we send by 0, 90, 180 or 270 degrees clockwise, for everyone (pictures are upright by default). */
  setRotation(degrees: number): void;
  readonly rotation: number;
  /** Turns how we show `member`'s video, for us only. */
  rotateView(member: string, degrees: number): void;
  viewRotation(member: string): number;
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
