// PeerConnect video for browsers: camera (or screen, or canvas) → WebCodecs
// encoder → group → a decoder per sender → your canvas.
//
// Each frame goes out with `sendVideo`, which cuts it into fragments that
// fit a datagram; receivers get it back whole and in order, a lost
// fragment is resent once, and a frame lost for good makes receivers ask
// for a keyframe, which this module sends at once. Bitrate steps down
// while receivers keep asking, and back up while they do not.
//
// Each video frame is `codec[1] ‖ encoded chunk` (GROUP.md §7.4): 1 VP8,
// 2 VP9, 3 H.264 (Annex B), 4 AV1. Timestamps are milliseconds on the
// sender's clock.

const CODECS = [
  { id: 1, codec: "vp8" },
  { id: 3, codec: "avc1.42001f", avc: { format: "annexb" } },
  { id: 2, codec: "vp09.00.10.08" },
  { id: 4, codec: "av01.0.04M.08" },
];

/** Receivers asking for keyframes this often (in 5 s) means the path cannot keep up. */
const STRAINED = 2;

/**
 * Starts video in a group. With `stream`, sends its video track; without,
 * only receives. Resolves to a Video; draw others with `attach`.
 */
export async function startVideo(conn, options = {}) {
  const video = new Video(conn, options);
  await video.start();
  return video;
}

export class Video extends EventTarget {
  #conn;
  #options;
  #track;
  #codec = null;
  #encoder = null;
  #reader = null;
  #element = null;
  #timer = undefined;
  #sending = true;
  #keyframeDue = true;
  #lastKeyframe = 0;
  #lastFrame = -Infinity;
  #bitrate;
  #requests = [];
  #raisedAt = 0;
  #decoders = new Map();
  #canvases = new Map();
  #listeners = [];
  #stats = { encoded: 0, sent: 0, keyframes: 0, skipped: 0, requests: 0 };
  #received = new Map();

  constructor(conn, options) {
    super();
    this.#conn = conn;
    this.#options = {
      width: 640,
      height: 360,
      framerate: 15,
      bitrate: 500_000,
      minBitrate: 100_000,
      keyframeInterval: 2000,
      track: 0,
      ...options,
    };
    this.#bitrate = this.#options.bitrate;
  }

  /** The bitrate the encoder runs at now, in bits per second. */
  get bitrate() {
    return this.#bitrate;
  }

  /** The codec we send with (a WebCodecs codec string), or null when receiving only. */
  get codec() {
    return this.#codec?.codec ?? null;
  }

  async start() {
    if (typeof VideoDecoder !== "function") throw new Error("this browser has no WebCodecs video");
    const on = (type, handler) => {
      this.#conn.addEventListener(type, handler);
      this.#listeners.push([type, handler]);
    };
    on("video", (e) => this.#receive(e.detail));
    on("keyframerequest", (e) => {
      if (e.detail.track !== this.#options.track) return;
      this.#stats.requests++;
      this.#keyframeDue = true;
      this.#strained();
    });
    on("left", (e) => this.#forget(e.detail.member));
    const track = this.#options.stream?.getVideoTracks()[0];
    if (track) await this.#startSending(track);
  }

  /** Draws `member`'s video into `canvas` from now on (`null` to stop). */
  attach(member, canvas) {
    if (canvas) this.#canvases.set(member, canvas);
    else this.#canvases.delete(member);
  }

  /**
   * Sends `stream`'s video track from now on, instead of what was sent
   * before; `null` stops sending (receiving goes on). The others' decoders
   * are untouched.
   */
  async setStream(stream) {
    this.#stopSending();
    const track = stream?.getVideoTracks()[0];
    if (track) await this.#startSending(track);
  }

  /** Pauses or resumes sending (the camera stays open). */
  setSending(sending) {
    this.#sending = sending;
    if (sending) this.#keyframeDue = true;
  }

  /** Our encoder's numbers, and per member what arrived and was decoded. */
  stats() {
    return {
      bitrate: this.#bitrate,
      codec: this.codec,
      ...this.#stats,
      members: [...this.#received].map(([member, s]) => ({ member, ...s })),
    };
  }

  stop() {
    this.#stopSending();
    for (const [member] of this.#decoders) this.#forget(member);
    for (const [type, handler] of this.#listeners) this.#conn.removeEventListener(type, handler);
    this.#listeners = [];
  }

  // Sending.

  #stopSending() {
    clearTimeout(this.#timer);
    this.#reader?.cancel().catch(() => {});
    this.#reader = null;
    if (this.#element) {
      this.#element.pause();
      this.#element.srcObject = null;
      this.#element = null;
    }
    if (this.#encoder && this.#encoder.state !== "closed") this.#encoder.close();
    this.#encoder = null;
  }

  async #startSending(track) {
    const { width, height, framerate } = this.#options;
    this.#codec ??= await pick(this.#options.codec, width, height, this.#bitrate, framerate);
    if (!this.#codec) throw new Error("no video codec this browser can encode");
    this.#encoder = new VideoEncoder({
      output: (chunk) => this.#output(chunk),
      error: (e) => this.dispatchEvent(new CustomEvent("error", { detail: e })),
    });
    this.#configure();
    if (typeof MediaStreamTrackProcessor === "function") {
      // Frames straight from the camera (Chromium).
      const reader = new MediaStreamTrackProcessor({ track }).readable.getReader();
      this.#reader = reader;
      (async () => {
        for (;;) {
          const { value, done } = await reader.read().catch(() => ({ done: true }));
          if (done) return;
          if (this.#reader === reader) this.#encode(value);
          else value.close();
        }
      })();
    } else {
      // Elsewhere: through a <video> element, at our frame rate.
      const element = document.createElement("video");
      this.#element = element;
      element.muted = true;
      element.playsInline = true;
      element.srcObject = new MediaStream([track]);
      await element.play();
      const tick = () => {
        if (this.#element !== element) return;
        if (element.readyState >= 2) this.#encode(new VideoFrame(element, { timestamp: performance.now() * 1000 }));
        this.#timer = setTimeout(tick, 1000 / framerate);
      };
      tick();
    }
  }

  #configure() {
    const { width, height, framerate } = this.#options;
    this.#encoder.configure({
      codec: this.#codec.codec,
      width,
      height,
      bitrate: this.#bitrate,
      framerate,
      latencyMode: "realtime",
      ...(this.#codec.avc ? { avc: this.#codec.avc } : {}),
    });
    this.#keyframeDue = true;
  }

  #encode(frame) {
    const now = performance.now();
    const due = now - this.#lastFrame >= 1000 / this.#options.framerate - 2;
    // Skip frames over our rate, while paused, and while the encoder is behind.
    const encoder = this.#encoder;
    if (!this.#sending || !due || encoder?.state !== "configured" || encoder.encodeQueueSize > 2) {
      if (due && this.#sending) this.#stats.skipped++;
      frame.close();
      return;
    }
    this.#lastFrame = now;
    const keyFrame = this.#keyframeDue || now - this.#lastKeyframe >= this.#options.keyframeInterval;
    if (keyFrame) {
      this.#keyframeDue = false;
      this.#lastKeyframe = now;
    }
    this.#encoder.encode(frame, { keyFrame });
    frame.close();
    this.#raise(now);
  }

  #output(chunk) {
    const data = new Uint8Array(1 + chunk.byteLength);
    data[0] = this.#codec.id;
    chunk.copyTo(data.subarray(1));
    const keyframe = chunk.type === "key";
    this.#stats.encoded++;
    if (keyframe) this.#stats.keyframes++;
    // Our clock, in milliseconds (wrapping at 2³²).
    const timestamp = Math.round(chunk.timestamp / 1000) >>> 0;
    if (this.#conn.sendVideo(this.#options.track, keyframe, timestamp, data)) this.#stats.sent++;
  }

  /** Receivers keep losing frames: a lower bitrate. */
  #strained() {
    const now = performance.now();
    this.#requests = this.#requests.filter((t) => now - t < 5000);
    this.#requests.push(now);
    if (this.#requests.length >= STRAINED && this.#encoder) {
      this.#bitrate = Math.max(this.#options.minBitrate, Math.round(this.#bitrate * 0.7));
      this.#requests = [];
      this.#raisedAt = now;
      this.#configure();
    }
  }

  /** Nobody has asked for a while: a little more, up to what was asked for. */
  #raise(now) {
    if (this.#bitrate >= this.#options.bitrate || now - this.#raisedAt < 10_000) return;
    if (this.#requests.some((t) => now - t < 10_000)) return;
    this.#bitrate = Math.min(this.#options.bitrate, Math.round(this.#bitrate * 1.15));
    this.#raisedAt = now;
    this.#configure();
  }

  // Receiving.

  #receive({ from, track, keyframe, timestamp, data }) {
    const stats = this.#received.get(from) ?? { received: 0, decoded: 0, errors: 0, lastTimestamp: 0 };
    this.#received.set(from, stats);
    stats.received++;
    const codec = CODECS.find((c) => c.id === data[0]);
    if (!codec) return;
    let decoder = this.#decoders.get(from);
    if (!decoder || decoder.state === "closed") {
      if (!keyframe) return; // the core asks for a keyframe; wait for it
      decoder = new VideoDecoder({
        output: (frame) => this.#show(from, track, frame, stats),
        error: () => {
          stats.errors++;
          this.#decoders.delete(from);
        },
      });
      decoder.configure({ codec: codec.codec, optimizeForLatency: true });
      this.#decoders.set(from, decoder);
    }
    decoder.decode(
      new EncodedVideoChunk({ type: keyframe ? "key" : "delta", timestamp: timestamp * 1000, data: data.subarray(1) }),
    );
  }

  #show(member, track, frame, stats) {
    stats.decoded++;
    stats.lastTimestamp = frame.timestamp / 1000;
    stats.width = frame.displayWidth;
    stats.height = frame.displayHeight;
    const canvas = this.#canvases.get(member);
    if (canvas) {
      if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
        canvas.width = frame.displayWidth;
        canvas.height = frame.displayHeight;
      }
      canvas.getContext("2d").drawImage(frame, 0, 0);
    }
    if (this.#options.onFrame) this.#options.onFrame(member, frame, track);
    else frame.close();
  }

  #forget(member) {
    const decoder = this.#decoders.get(member);
    if (decoder && decoder.state !== "closed") decoder.close();
    this.#decoders.delete(member);
  }
}

/** The first codec of the preference (or the default order) that this browser encodes. */
async function pick(preferred, width, height, bitrate, framerate) {
  const order = preferred ? CODECS.filter((c) => c.codec === preferred) : CODECS;
  for (const c of order) {
    const config = { codec: c.codec, width, height, bitrate, framerate, latencyMode: "realtime", ...(c.avc ? { avc: c.avc } : {}) };
    try {
      const { supported } = await VideoEncoder.isConfigSupported(config);
      if (supported) return c;
    } catch {
      // Not this one.
    }
  }
  return null;
}
