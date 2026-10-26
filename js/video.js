// PeerConnect video for browsers: camera (or screen, or canvas) → WebCodecs
// encoders, one per layer → group → a decoder per sender → your canvas.
//
// Simulcast: a sender encodes the same picture in up to four sizes, layers
// 0–3 (0 the smallest), and the relay forwards each receiver the one it
// subscribed to. Receivers here subscribe by the size each canvas is shown
// at, and senders encode only the layers someone wants, as the relay tells
// them ("videodemand").
//
// Each frame goes out with `sendVideo`, which cuts it into fragments that
// fit a datagram; receivers get it back whole and in order, a lost
// fragment is resent once, and a frame lost for good makes receivers ask
// for a keyframe, which this module sends at once. Each layer's bitrate
// steps down while receivers keep asking, and back up while they do not.
//
// Each video frame is `codec[1] ‖ encoded chunk` (GROUP.md §7.4): 1 VP8,
// 2 VP9, 3 H.264 (Annex B), 4 AV1. Timestamps are milliseconds on the
// sender's clock.
//
// Frames are drawn upright before they are encoded: a phone held upright
// gives the camera's sideways picture plus a note to turn it, which a
// <video> element follows and an encoder does not; drawing the element
// follows it. The encoded size follows the picture's shape (360 × 640 for
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
/** Layers 0–3. */
const LAYERS = 4;
/**
 * The layer a canvas gets, by the short side it is shown at in device
 * pixels: the smallest that covers it, at the sizes layers usually are
 * (180, 360 and 720 rows: layers 0, 1 and 2), else 3.
 */
const SHOWN = [180, 360, 720];
/** How long a canvas's size must hold before we subscribe by it: resizing and moving tiles would flap. */
const SETTLE = 300;
/** Keyframe requests this soon after a layer starts are receivers moving to it, not losses. */
const STARTING = 1000;

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
  #codec = null;
  #element = null;
  #timer = undefined;
  /** What we send: a layer each, smallest first (see `#layer`). */
  #layers;
  /** Which layers the relay says are wanted (bit n: layer n); null until it says. */
  #demand = null;
  /** Quarter turns, clockwise: of what we send, and of what we show of each member. */
  #rotation = 0;
  #views = new Map();
  #sending = true;
  /** By member and track ("member track"): a decoder, its codec and layer. */
  #decoders = new Map();
  #canvases = new Map();
  #received = new Map();
  /** Subscriptions: what we told the core, layers set by hand, and changes waiting to settle. */
  #subscribed = new Map();
  #chosen = new Map();
  #settling = new Map();
  #unattached;
  #observer = null;
  #listeners = [];

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
    const o = this.#options;
    // Without layers, one: layer 0, as video was before simulcast.
    const layers = o.layers?.length ? o.layers : [{ layer: 0, width: o.width, height: o.height, bitrate: o.bitrate, minBitrate: o.minBitrate }];
    this.#layers = layers.map((l) => layerState(l, o)).sort((a, b) => a.layer - b.layer);
    if (this.#layers.some((l, i) => !(l.layer >= 0 && l.layer < LAYERS) || l.layer === this.#layers[i + 1]?.layer)) {
      throw new RangeError("video layers are 0 to 3, each at most once");
    }
    // Someone who sees every frame (`onFrame`) wants some video of everyone; a canvas, only what is shown.
    this.#unattached = "unattached" in o ? o.unattached : o.onFrame ? 0 : null;
  }

  /** The bitrate our encoders run at now, all layers together, in bits per second. */
  get bitrate() {
    return this.#layers.reduce((sum, l) => sum + (l.encoder ? l.bitrate : 0), 0);
  }

  /** The codec we send with (a WebCodecs codec string), or null when receiving only. */
  get codec() {
    return this.#codec?.codec ?? null;
  }

  async start() {
    if (typeof VideoDecoder !== "function") throw new Error("this browser has no WebCodecs video");
    const on = (target, type, handler) => {
      target.addEventListener(type, handler);
      this.#listeners.push([target, type, handler]);
    };
    const conn = this.#conn;
    on(conn, "video", (e) => this.#receive(e.detail));
    on(conn, "keyframerequest", (e) => this.#keyframeRequested(e.detail));
    on(conn, "videodemand", (e) => this.#demanded(e.detail.layers));
    on(conn, "joined", (e) => this.#settle(e.detail.member));
    on(conn, "left", (e) => this.#forget(e.detail.member));
    if (typeof document !== "undefined") on(document, "visibilitychange", () => this.#settleAll());
    if (typeof ResizeObserver === "function") {
      this.#observer = new ResizeObserver((entries) => {
        for (const { target } of entries) for (const [member, canvas] of this.#canvases) if (canvas === target) this.#settle(member);
      });
    }
    this.#demand = conn.videoDemand ?? null;
    for (const member of conn.members.keys()) this.#settle(member);
    const track = this.#options.stream?.getVideoTracks()[0];
    if (track) await this.#startSending(track);
  }

  /**
   * Draws `member`'s video into `canvas` from now on (`null` to stop). Our
   * own member ID shows what we send, as others will see it. Unless set by
   * hand (`setLayer`), we subscribe to the layer that suits the size the
   * canvas is shown at, and to none while it is not shown.
   */
  attach(member, canvas) {
    const before = this.#canvases.get(member);
    if (before === canvas) return;
    if (before && ![...this.#canvases].some(([m, c]) => m !== member && c === before)) this.#observer?.unobserve(before);
    if (canvas) {
      this.#canvases.set(member, canvas);
      if (typeof canvas.getBoundingClientRect === "function") this.#observer?.observe(canvas);
    } else this.#canvases.delete(member);
    this.#settle(member);
  }

  /**
   * Which layer of `member`'s video to receive: 0–3, `null` for none, or
   * "auto" (the default) to follow the canvas it is attached to.
   */
  setLayer(member, layer) {
    if ((this.#chosen.has(member) ? this.#chosen.get(member) : "auto") === layer) return;
    if (layer === "auto") this.#chosen.delete(member);
    else if (layer === null || (Number.isInteger(layer) && layer >= 0 && layer < LAYERS)) this.#chosen.set(member, layer);
    else throw new RangeError("a layer is 0 to 3, null or \"auto\"");
    this.#settle(member, true);
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
    if (sending) for (const l of this.#layers) l.keyframeDue = true;
  }

  /** Our encoders' numbers, layer by layer, what is wanted of us, and per member what arrived and was decoded. */
  stats() {
    const layers = this.#layers.map((l) => ({
      layer: l.layer,
      running: !!l.encoder,
      width: l.size?.[0] ?? l.width,
      height: l.size?.[1] ?? l.height,
      bitrate: l.bitrate,
      ...l.stats,
    }));
    const total = (key) => layers.reduce((sum, l) => sum + l[key], 0);
    return {
      bitrate: this.bitrate,
      codec: this.codec,
      demand: this.#demand,
      encoded: total("encoded"),
      sent: total("sent"),
      keyframes: total("keyframes"),
      skipped: total("skipped"),
      requests: total("requests"),
      layers,
      members: [...this.#received].map(([member, s]) => ({
        member,
        subscribed: this.#subscribed.get(member),
        ...s,
        layers: [...s.layers].sort(([a], [b]) => a - b).map(([layer, n]) => ({ layer, ...n })),
      })),
    };
  }

  stop() {
    this.#stopSending();
    for (const [key] of this.#decoders) this.#close(key);
    for (const [target, type, handler] of this.#listeners) target.removeEventListener(type, handler);
    this.#listeners = [];
    this.#observer?.disconnect();
    for (const timer of this.#settling.values()) clearTimeout(timer);
    this.#settling.clear();
    // Nothing is shown any more: let the relay stop forwarding.
    for (const [member, layer] of this.#subscribed) if (layer !== null) this.#tell(member, null);
  }

  // Sending.

  #stopSending() {
    clearTimeout(this.#timer);
    if (this.#element) {
      this.#element.pause();
      this.#element.srcObject = null;
      this.#element = null;
    }
    for (const l of this.#layers) this.#stopLayer(l);
  }

  async #startSending(track) {
    const { framerate } = this.#options;
    const top = this.#layers.at(-1);
    this.#codec ??= await pick(this.#options.codec, top.width, top.height, top.target, framerate);
    if (!this.#codec) throw new Error("no video codec this browser can encode");
    // The camera through a <video> element, which turns it upright, drawn
    // at our frame rate.
    const element = document.createElement("video");
    this.#element = element;
    element.muted = true;
    element.playsInline = true;
    element.srcObject = new MediaStream([track]);
    await element.play();
    if (this.#element !== element) return;
    this.#run();
    const tick = () => {
      if (this.#element !== element) return;
      if (element.readyState >= 2 && element.videoWidth) this.#capture(element);
      this.#timer = setTimeout(tick, 1000 / framerate);
    };
    tick();
  }

  /**
   * Runs an encoder for each layer wanted and stops the others. A wanted
   * layer we do not have is served by the nearest below it, or our lowest;
   * until the relay says, only our lowest runs.
   */
  #run() {
    const wanted = new Set(this.#demand === null ? [this.#layers[0]] : []);
    for (let layer = 0; layer < LAYERS; layer++) if (this.#demand & (1 << layer)) wanted.add(this.#layer(layer));
    for (const l of this.#layers) {
      if (wanted.has(l) && this.#element) this.#startLayer(l);
      else this.#stopLayer(l);
    }
  }

  /** Our layer that serves `layer`: the nearest at or below it, else the lowest. */
  #layer(layer) {
    return this.#layers.findLast((l) => l.layer <= layer) ?? this.#layers[0];
  }

  #startLayer(l) {
    if (l.encoder) return;
    l.encoder = new VideoEncoder({
      output: (chunk) => this.#output(l, chunk),
      error: (e) => this.dispatchEvent(new CustomEvent("error", { detail: e })),
    });
    l.size = null; // configured for the first frame's shape
    l.keyframeDue = true; // receivers moving to it start there
    l.startedAt = performance.now();
  }

  #stopLayer(l) {
    if (l.encoder && l.encoder.state !== "closed") l.encoder.close();
    l.encoder = null;
  }

  #demanded(layers) {
    this.#demand = layers;
    this.#run();
  }

  #keyframeRequested({ track, layer }) {
    if (track !== this.#options.track) return;
    const l = this.#layer(layer ?? 0);
    l.stats.requests++;
    l.keyframeDue = true;
    if (performance.now() - l.startedAt >= STARTING) this.#strained(l);
  }

  /**
   * Draws the camera's picture upright, turned as asked, into each running
   * layer's canvas at that layer's size, and encodes it. (Chromium's
   * encoders would scale one frame to each size themselves, as WebCodecs
   * says they must, but that measured no cheaper than drawing, and drawing
   * does not rely on every browser doing so.)
   */
  #capture(element) {
    if (!this.#sending) return;
    const now = performance.now();
    const timestamp = Math.round(now * 1000);
    const turned = this.#rotation % 2 === 1;
    const [sw, sh] = turned ? [element.videoHeight, element.videoWidth] : [element.videoWidth, element.videoHeight];
    let drawn = null;
    for (const l of this.#layers) {
      const encoder = l.encoder;
      if (!encoder || encoder.state === "closed") continue;
      // Skip frames while the encoder is behind.
      if (encoder.encodeQueueSize > 2) {
        l.stats.skipped++;
        continue;
      }
      const [w, h] = fit(sw, sh, l);
      if (l.size?.[0] !== w || l.size?.[1] !== h) {
        l.size = [w, h];
        this.#configure(l); // a new shape: a new configuration, and a keyframe
      }
      const canvas = (l.canvas ??= typeof OffscreenCanvas === "function" ? new OffscreenCanvas(w, h) : document.createElement("canvas"));
      if (canvas.width !== w || canvas.height !== h) [canvas.width, canvas.height] = [w, h];
      draw(canvas.getContext("2d"), element, w, h, this.#rotation);
      const frame = new VideoFrame(canvas, { timestamp });
      const keyFrame = l.keyframeDue || now - l.lastKeyframe >= this.#options.keyframeInterval;
      if (keyFrame) {
        l.keyframeDue = false;
        l.lastKeyframe = now;
      }
      encoder.encode(frame, { keyFrame });
      frame.close();
      this.#raise(l, now);
      drawn = canvas;
    }
    // Ours as the others see it at best, whoever is watching: our largest
    // layer, drawn on its own while nobody wants it.
    const preview = this.#canvases.get(this.#conn.id);
    const top = this.#layers.at(-1);
    if (preview && drawn && drawn === top.canvas) copyTo(preview, drawn, drawn.width, drawn.height);
    else if (preview) {
      const [w, h] = fit(sw, sh, top);
      if (preview.width !== w || preview.height !== h) [preview.width, preview.height] = [w, h];
      draw(preview.getContext("2d"), element, w, h, this.#rotation);
    }
  }

  #configure(l) {
    if (!l.size || !l.encoder || l.encoder.state === "closed") return;
    const [width, height] = l.size;
    l.encoder.configure({
      codec: this.#codec.codec,
      width,
      height,
      bitrate: l.bitrate,
      framerate: this.#options.framerate,
      latencyMode: "realtime",
      ...(this.#codec.avc ? { avc: this.#codec.avc } : {}),
    });
    l.keyframeDue = true;
  }

  #output(l, chunk) {
    if (!l.encoder) return; // stopped meanwhile
    const data = new Uint8Array(1 + chunk.byteLength);
    data[0] = this.#codec.id;
    chunk.copyTo(data.subarray(1));
    const keyframe = chunk.type === "key";
    l.stats.encoded++;
    if (keyframe) l.stats.keyframes++;
    // Our clock, in milliseconds (wrapping at 2³²).
    const timestamp = Math.round(chunk.timestamp / 1000) >>> 0;
    if (this.#conn.sendVideo(this.#options.track, l.layer, keyframe, timestamp, data)) l.stats.sent++;
  }

  /** Receivers of this layer keep losing frames: a lower bitrate. */
  #strained(l) {
    const now = performance.now();
    l.requests = l.requests.filter((t) => now - t < 5000);
    l.requests.push(now);
    if (l.requests.length >= STRAINED && l.encoder) {
      l.bitrate = Math.max(l.minBitrate, Math.round(l.bitrate * 0.7));
      l.requests = [];
      l.raisedAt = now;
      this.#configure(l);
    }
  }

  /** Nobody has asked for a while: a little more, up to what was asked for. */
  #raise(l, now) {
    if (l.bitrate >= l.target || now - l.raisedAt < 10_000) return;
    if (l.requests.some((t) => now - t < 10_000)) return;
    l.bitrate = Math.min(l.target, Math.round(l.bitrate * 1.15));
    l.raisedAt = now;
    this.#configure(l);
  }

  // Receiving.

  #receive({ from, track, layer = 0, keyframe, timestamp, data }) {
    let stats = this.#received.get(from);
    if (!stats) {
      stats = { layer, received: 0, decoded: 0, errors: 0, lastTimestamp: 0, layers: new Map() };
      this.#received.set(from, stats);
    }
    stats.received++;
    stats.layer = layer;
    const counts = stats.layers.get(layer) ?? { received: 0, decoded: 0 };
    stats.layers.set(layer, counts);
    counts.received++;
    const codec = CODECS.find((c) => c.id === data[0]);
    if (!codec) return;
    const key = `${from} ${track}`;
    let d = this.#decoders.get(key);
    if (!d || d.decoder.state === "closed") {
      if (!keyframe) return; // the core asks for a keyframe; wait for it
      d = { decoder: null, codec: null, layer: null, pending: [] };
      d.decoder = new VideoDecoder({
        output: (frame) => {
          // Which layer it was, for the numbers: frames come out in order.
          while (d.pending.length && d.pending[0][0] !== frame.timestamp) d.pending.shift();
          const layer = d.pending.shift()?.[1] ?? d.layer;
          this.#show(from, track, layer, frame, stats);
        },
        error: () => {
          stats.errors++;
          this.#decoders.delete(key);
        },
      });
      this.#decoders.set(key, d);
    }
    if (d.codec !== codec.id || d.layer !== layer) {
      if (!keyframe) return; // another layer starts at its keyframe
      // Another size from here on: configured afresh, behind the frames
      // queued, so that the picture goes on until the new one's first.
      d.decoder.configure({ codec: codec.codec, optimizeForLatency: true });
      d.codec = codec.id;
      d.layer = layer;
    }
    const chunk = new EncodedVideoChunk({ type: keyframe ? "key" : "delta", timestamp: timestamp * 1000, data: data.subarray(1) });
    d.pending.push([chunk.timestamp, layer]);
    if (d.pending.length > 64) d.pending.shift();
    d.decoder.decode(chunk);
  }

  #show(member, track, layer, frame, stats) {
    stats.decoded++;
    const counts = stats.layers.get(layer);
    if (counts) counts.decoded++;
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
    if (this.#options.onFrame) this.#options.onFrame(member, frame, track, layer);
    else frame.close();
  }

  #forget(member) {
    for (const key of this.#decoders.keys()) if (key.startsWith(`${member} `)) this.#close(key);
  }

  #close(key) {
    const d = this.#decoders.get(key);
    if (d && d.decoder.state !== "closed") d.decoder.close();
    this.#decoders.delete(key);
  }

  // Subscriptions.

  /**
   * Subscribes to the layer `member`'s video should be in now, at once the
   * first time (and with `now`), else once it has held for a moment.
   */
  #settle(member, now = false) {
    if (member === this.#conn.id) return;
    clearTimeout(this.#settling.get(member));
    this.#settling.delete(member);
    if (now || !this.#subscribed.has(member)) return this.#tell(member, this.#wanted(member));
    this.#settling.set(
      member,
      setTimeout(() => {
        this.#settling.delete(member);
        this.#tell(member, this.#wanted(member));
      }, SETTLE),
    );
  }

  #settleAll() {
    for (const member of new Set([...this.#subscribed.keys(), ...this.#conn.members.keys()])) this.#settle(member);
  }

  /** The layer to receive of `member`'s video: as set, else by the size its canvas is shown at. */
  #wanted(member) {
    if (this.#chosen.has(member)) return this.#chosen.get(member);
    if (typeof document !== "undefined" && document.visibilityState === "hidden") return null;
    const canvas = this.#canvases.get(member);
    if (!canvas) return this.#unattached;
    // Not on the page (an OffscreenCanvas, say), its size tells us nothing: the largest there is.
    if (typeof canvas.getBoundingClientRect !== "function" || !canvas.isConnected) return LAYERS - 1;
    const { width, height } = canvas.getBoundingClientRect();
    const side = Math.min(width, height) * (globalThis.devicePixelRatio || 1);
    if (!side) return null;
    const layer = SHOWN.findIndex((most) => side <= most);
    return layer < 0 ? LAYERS - 1 : layer;
  }

  #tell(member, layer) {
    if (this.#subscribed.has(member) && this.#subscribed.get(member) === layer) return;
    this.#subscribed.set(member, layer);
    try {
      this.#conn.subscribe(member, layer);
    } catch {
      // Left the group: nothing to subscribe to.
    }
  }
}

/** A layer we send, from its options: what it aims for and where it is now. */
function layerState({ layer, width, height, bitrate, minBitrate }, defaults) {
  const target = bitrate ?? defaults.bitrate;
  return {
    layer,
    width: width ?? defaults.width,
    height: height ?? defaults.height,
    target,
    minBitrate: Math.min(target, minBitrate ?? defaults.minBitrate),
    bitrate: target,
    encoder: null,
    canvas: null,
    size: null,
    keyframeDue: true,
    lastKeyframe: 0,
    startedAt: 0,
    requests: [],
    raisedAt: 0,
    stats: { encoded: 0, sent: 0, keyframes: 0, skipped: 0, requests: 0 },
  };
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
