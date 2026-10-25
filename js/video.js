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
//
// Frames are drawn upright before they are encoded: a phone held upright
// gives the camera's sideways picture plus a note to turn it, which a
// <video> element follows and an encoder does not; drawing the element
// follows it. The encoded size follows the picture's shape (270 × 480 for
// a phone held upright), and `setRotation` turns what we send by hand,
// `rotateView` what we show of someone else.

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
  #element = null;
  #timer = undefined;
  /** What we draw each frame on before encoding it, and its size. */
  #canvas = null;
  #size = null;
  /** Quarter turns, clockwise: of what we send, and of what we show of each member. */
  #rotation = 0;
  #views = new Map();
  #sending = true;
  #keyframeDue = true;
  #lastKeyframe = 0;
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

  /**
   * Draws `member`'s video into `canvas` from now on (`null` to stop). Our
   * own member ID shows what we send, as others will see it.
   */
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

  /** Turns what we send by `degrees` clockwise (0, 90, 180 or 270), for everyone. */
  setRotation(degrees) {
    this.#rotation = quarter(degrees);
  }

  /** The rotation of what we send, in degrees. */
  get rotation() {
    return this.#rotation * 90;
  }

  /** Turns how we show `member`'s video by `degrees` clockwise, for us only. */
  rotateView(member, degrees) {
    const turns = quarter(degrees);
    if (turns) this.#views.set(member, turns);
    else this.#views.delete(member);
  }

  /** How we show `member`'s video, in degrees. */
  viewRotation(member) {
    return (this.#views.get(member) ?? 0) * 90;
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
    this.#size = null; // configured for the first frame's shape
    // The camera through a <video> element, which turns it upright, drawn
    // at our frame rate.
    const element = document.createElement("video");
    this.#element = element;
    element.muted = true;
    element.playsInline = true;
    element.srcObject = new MediaStream([track]);
    await element.play();
    this.#canvas ??= typeof OffscreenCanvas === "function" ? new OffscreenCanvas(2, 2) : document.createElement("canvas");
    const tick = () => {
      if (this.#element !== element) return;
      if (element.readyState >= 2 && element.videoWidth) this.#capture(element);
      this.#timer = setTimeout(tick, 1000 / framerate);
    };
    tick();
  }

  /** Draws the camera's picture upright, turned as asked, and encodes it. */
  #capture(element) {
    const now = performance.now();
    const encoder = this.#encoder;
    // Skip frames while paused, and while the encoder is behind.
    if (!this.#sending || !encoder || encoder.state === "closed" || encoder.encodeQueueSize > 2) {
      if (this.#sending) this.#stats.skipped++;
      return;
    }
    const turned = this.#rotation % 2 === 1;
    const [sw, sh] = turned ? [element.videoHeight, element.videoWidth] : [element.videoWidth, element.videoHeight];
    const [w, h] = fit(sw, sh, this.#options);
    if (this.#size?.[0] !== w || this.#size?.[1] !== h) {
      this.#size = [w, h];
      this.#configure(); // a new shape: a new configuration, and a keyframe
    }
    const canvas = this.#canvas;
    if (canvas.width !== w || canvas.height !== h) [canvas.width, canvas.height] = [w, h];
    draw(canvas.getContext("2d"), element, w, h, this.#rotation);
    const frame = new VideoFrame(canvas, { timestamp: Math.round(now * 1000) });
    const preview = this.#canvases.get(this.#conn.id);
    if (preview) copyTo(preview, canvas, w, h);
    const keyFrame = this.#keyframeDue || now - this.#lastKeyframe >= this.#options.keyframeInterval;
    if (keyFrame) {
      this.#keyframeDue = false;
      this.#lastKeyframe = now;
    }
    encoder.encode(frame, { keyFrame });
    frame.close();
    this.#raise(now);
  }

  #configure() {
    if (!this.#size || !this.#encoder || this.#encoder.state === "closed") return;
    const [width, height] = this.#size;
    this.#encoder.configure({
      codec: this.#codec.codec,
      width,
      height,
      bitrate: this.#bitrate,
      framerate: this.#options.framerate,
      latencyMode: "realtime",
      ...(this.#codec.avc ? { avc: this.#codec.avc } : {}),
    });
    this.#keyframeDue = true;
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
      const turns = this.#views.get(member) ?? 0;
      const [w, h] = turns % 2 ? [frame.displayHeight, frame.displayWidth] : [frame.displayWidth, frame.displayHeight];
      if (canvas.width !== w || canvas.height !== h) [canvas.width, canvas.height] = [w, h];
      draw(canvas.getContext("2d"), frame, w, h, turns);
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

/** 0–3 quarter turns from degrees. */
function quarter(degrees) {
  return ((Math.round(degrees / 90) % 4) + 4) % 4;
}

/**
 * The size to encode a picture of `sw` × `sh` at: within the configured
 * size, turned to the picture's shape (a tall picture gets a tall size),
 * keeping its proportions, in even numbers.
 */
function fit(sw, sh, { width, height }) {
  const long = Math.max(width, height);
  const short = Math.min(width, height);
  const scale = Math.min(1, sw >= sh ? Math.min(long / sw, short / sh) : Math.min(short / sw, long / sh));
  const even = (v) => Math.max(2, Math.round((v * scale) / 2) * 2);
  return [even(sw), even(sh)];
}

/** Draws `source` into a `w` × `h` context, turned `turns` quarter turns clockwise. */
function draw(ctx, source, w, h, turns) {
  ctx.save();
  ctx.translate(w / 2, h / 2);
  ctx.rotate((turns * Math.PI) / 2);
  const [dw, dh] = turns % 2 ? [h, w] : [w, h];
  ctx.drawImage(source, -dw / 2, -dh / 2, dw, dh);
  ctx.restore();
}

function copyTo(target, source, w, h) {
  if (target.width !== w || target.height !== h) [target.width, target.height] = [w, h];
  target.getContext("2d").drawImage(source, 0, 0);
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
