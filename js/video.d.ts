import type { GroupConnection } from "./index.js";

/** One size of our picture that we send (simulcast): receivers each get the layer that suits them. */
export interface VideoLayer {
  /** 0–3, from the smallest picture up; the relay forwards each receiver the layer it subscribed to, or the nearest. */
  layer: number;
  /**
   * The largest size to encode this layer at, turned to the picture's shape: a phone held
   * upright sends 360 × 640. Never more than the picture: a 1440 × 900 screen goes out as
   * it is in a 1920 × 1080 layer.
   */
  width: number;
  height: number;
  /** Bits per second to aim for, and the floor it may step down to while its receivers keep losing frames. */
  bitrate: number;
  minBitrate?: number;
  /** A WebCodecs codec string for this layer only, overriding `VideoOptions.codec` and the choice by hardware. */
  codec?: string;
}

export interface VideoOptions {
  /** A stream whose video track to send (camera, screen, `canvas.captureStream()`); without one, only receive. */
  stream?: MediaStream;
  /**
   * Which of our two tracks this Video sends: 0 (the default, a camera) or 1 (a shared
   * screen, say). The relay routes and subscribes each on its own.
   */
  track?: number;
  /**
   * Whether this Video receives the others' video (default true). Only one Video per
   * connection should: start the one that sends our second track with `receive: false`.
   */
  receive?: boolean;
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
  /** Frames per second (default 15), for every layer; 10–15 suits a screen. */
  framerate?: number;
  /** Without `layers`, bits per second to aim for (default 500000), and the floor it may step down to (default 100000). */
  bitrate?: number;
  minBitrate?: number;
  /**
   * A WebCodecs codec string for every layer: "vp8", "avc1.42001f" (any `avc1.` profile and
   * level), "vp09.00.10.08" or "av01.0.04M.08", in hardware where the browser has it, else
   * in software. By default each layer picks its own: a hardware encoder first (H.264 for
   * layers 2 and 3: "avc1.640028" above 1280 × 720, else "avc1.4d401f"; VP8 below), else
   * VP8 in software, else whatever the browser encodes. A hardware encoder that fails falls
   * back to software.
   */
  codec?: string;
  /** The source track's content hint, also given to the encoders: "detail" (or "text") for a screen, "motion" for a camera. */
  contentHint?: "" | "motion" | "detail" | "text";
  /** Milliseconds between keyframes at most (default 2000); receivers that lose one ask for another sooner. */
  keyframeInterval?: number;
  /**
   * The layer to receive of members' tracks with no canvas attached: `null` (none) by
   * default, 0 when `onFrame` is given, which sees every member's frames.
   */
  unattached?: number | null;
  /** Called with every decoded frame (its track, and the layer it came in) instead of closing it: close it when done. */
  onFrame?: (member: string, frame: VideoFrame, track: number, layer: number) => void;
}

export interface VideoLayerStats {
  layer: number;
  /** Whether it is being encoded now: someone wants it. */
  running: boolean;
  /** The codec it encodes with (null until sending starts), and whether a hardware encoder was asked for. */
  codec: string | null;
  hardware: boolean;
  /** The size it is encoded at (the configured one until the first frame). */
  width: number;
  height: number;
  /** Bits per second it runs at now. */
  bitrate: number;
  /**
   * Frames encoded, sent, keyframes among them, frames skipped (encoder behind), frames
   * too large for the protocol (255,000 bytes: not sent, and the bitrate stepped down),
   * keyframe requests heard.
   */
  encoded: number;
  sent: number;
  keyframes: number;
  skipped: number;
  oversized: number;
  requests: number;
}

export interface VideoStats {
  /** The track this Video sends. */
  track: number;
  /** All running layers together. */
  bitrate: number;
  codec: string | null;
  /** The layers of our track the relay says are wanted (bit n: layer n), null until it says. */
  demand: number | null;
  /** The data saver's limit on what we receive (`setMaxLayer`). */
  maxLayer: number | null;
  /**
   * Our uplink. While it cannot carry every layer we run (video was dropped, or more than
   * 32 KiB, and more than leaves in 250 ms at the pace video has been leaving, waited for
   * 300 ms), our largest running layer stops, down to the smallest, which always goes on;
   * its receivers get the next layer down. After 8 s without a sign it starts again, on
   * trial: one that fails within 5 s doubles the wait, up to 60 s.
   */
  uplink: {
    /** A sign of it within the last second. */
    congested: boolean;
    /** The largest layer we send now, while some are stopped for the uplink; null when none are. */
    cappedAt: number | null;
    /** Video bytes waiting to go out, dropped since we joined, and the pace it leaves at (`GroupConnection.videoBacklog`). */
    queued: number;
    dropped: number;
    pace: number;
    /** Milliseconds the uplink must keep up before a stopped layer is tried again. */
    wait: number;
  };
  /** The layers' numbers, added up. */
  encoded: number;
  sent: number;
  keyframes: number;
  skipped: number;
  oversized: number;
  requests: number;
  layers: VideoLayerStats[];
  /** What we receive, by member and track. */
  members: {
    member: string;
    track: number;
    /** The layer we subscribed to (null: none), and the one arriving now. */
    subscribed: number | null | undefined;
    layer: number;
    /** The codec the decoder is configured for (an H.264 one as its keyframes say). */
    codec: string | null;
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
  /** The track we sent ended (the browser's own "stop sharing", a camera unplugged): sending stopped. */
  ended: Event;
}

/** Video in a group: started by `startVideo`. */
export class Video extends EventTarget {
  /**
   * Bits per second our encoders run at now, all layers (each steps down while its receivers
   * keep losing frames; the largest stops while our uplink cannot carry them all, see `stats().uplink`).
   */
  readonly bitrate: number;
  /** The codec of our largest running layer (or else configured); null when not sending. */
  readonly codec: string | null;
  /** The track this Video sends. */
  readonly track: number;
  /** Whether a track is being sent now. */
  readonly sending: boolean;
  /**
   * Draws `member`'s video `track` (default 0) into `canvas` from now on; `null` stops.
   * Our own ID shows what this Video sends. Unless set with `setLayer`, we receive the
   * layer that suits the size the canvas is shown at (its short side in device pixels: up
   * to 180 layer 0, 360 layer 1, 720 layer 2, else 3), none while it has no size (hidden)
   * or the page is hidden, and the largest for a canvas that is not on the page (an
   * OffscreenCanvas, say); no more than `setMaxLayer` allows.
   */
  attach(member: string, canvas: HTMLCanvasElement | OffscreenCanvas | null, options?: { track?: number }): void;
  /** Which layer of `member`'s video `track` (default 0) to receive: 0–3, `null` for none, or "auto" (the default) to follow its canvas. */
  setLayer(member: string, layer: number | null | "auto", options?: { track?: number }): void;
  /**
   * Data saver: the largest layer we receive by ourselves (by canvas size, or `unattached`),
   * or `null` (the default) for no limit. Layers set with `setLayer` are as set.
   */
  setMaxLayer(layer: number | null): void;
  readonly maxLayer: number | null;
  /** Turns what we send by 0, 90, 180 or 270 degrees clockwise, for everyone (pictures are upright by default). */
  setRotation(degrees: number): void;
  readonly rotation: number;
  /** Turns how we show `member`'s video, for us only. */
  rotateView(member: string, degrees: number): void;
  viewRotation(member: string): number;
  /**
   * Sends this stream's video track from now on; `null` stops sending (receiving goes on).
   * `layers` replaces the layers we send, `contentHint` the track's hint.
   */
  setStream(stream: MediaStream | null, options?: { layers?: VideoLayer[]; contentHint?: VideoOptions["contentHint"] }): Promise<void>;
  /** Pauses or resumes sending. */
  setSending(sending: boolean): void;
  stats(): VideoStats;
  stop(): void;
  addEventListener<K extends keyof VideoEventMap>(type: K, listener: (event: VideoEventMap[K]) => void): void;
  addEventListener(type: string, listener: EventListenerOrEventListenerObject): void;
}

/**
 * Starts video: sends `stream`'s video track as `track` if given, and receives the others'
 * (unless `receive` is false). A screen share, say:
 * `startVideo(conn, { track: 1, stream: await navigator.mediaDevices.getDisplayMedia({ video: true }),
 * receive: false, framerate: 15, contentHint: "detail", layers: [...] })`.
 */
export function startVideo(conn: GroupConnection, options?: VideoOptions): Promise<Video>;
