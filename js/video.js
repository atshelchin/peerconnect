// PeerConnect video for browsers: camera (or screen, or canvas) → WebCodecs
// encoders, one per layer → group → a decoder per sender and track → your
// canvas.
//
// Tracks: a member may send two videos, track 0 (the camera, say) and
// track 1 (a shared screen); the relay routes each on its own. A `Video`
// sends one track (`track` option) and receives both tracks of everyone;
// to send the other track too, start a second one with `receive: false`.
//
// Simulcast: a sender encodes the same picture in up to four sizes, layers
// 0–3 (0 the smallest), and the relay forwards each receiver the one it
// subscribed to, track by track. Receivers here subscribe by the size each
// canvas is shown at, and senders encode only the layers someone wants, as
// the relay tells them ("videodemand").
//
// Each frame goes out with `sendVideo`, which cuts it into fragments that
// fit a datagram; receivers get it back whole and in order, a lost
// fragment is resent once, and a frame lost for good makes receivers ask
// for a keyframe, which this module sends at once. Each layer's bitrate
// steps down while receivers keep asking, and back up while they do not.
//
// The relay fits what each receiver gets to its link; our own uplink is
// ours to fit. While it cannot carry every layer we run, the largest stops
// (its receivers get the next one down), down to the smallest, which always
// goes on; a while after it keeps up, the layer starts again, on trial. The
// rules are the relay's for receivers (GROUP.md §3.3), see `#uplinkCheck`.
//
// Each video frame is `codec[1] ‖ encoded chunk` (GROUP.md §7.4): 1 VP8,
// 2 VP9, 3 H.264 (Annex B), 4 AV1. Each layer picks its own: a hardware
// encoder where the browser has one (H.264 for the large layers, which
// cost the most to encode in software), else VP8 in software, which every
// browser with WebCodecs has; receivers follow, frame by frame.
// Timestamps are milliseconds on the sender's clock.
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
/** H.264 for large layers: High 4.0 takes 1080p, Main 3.1 720p (and level 3.1 no more). */
/** Frames a hardware encoder may hold before we skip new ones (VideoToolbox holds three or four). */
const HARDWARE_QUEUE = 6;
/**
 * A hardware encoder that puts out nothing this long (ms) while frames wait
 * in it is taken to have failed, and the layer goes on in software. Some
 * hold their frames for seconds at a size they do not like (VideoToolbox's
 * H.264 at 1080p, measured), which looks like a frozen picture to everyone.
 */
const HARDWARE_STALL = 1000;
const AVC_1080 = "avc1.640028";
const AVC_720 = "avc1.4d401f";

/**
 * A keyframe waits while more than this much of our video is still to go
 * (a keyframe before it, say), for `KEYFRAME_HOLD` ms at most: two of them
 * at once (a 1080p one is 100–250 KB) would overflow our backlog, or the
 * relay's for a receiver (256 KiB each).
 */
const KEYFRAME_ROOM = 64 << 10;
const KEYFRAME_HOLD = 500;
/** Receivers asking for keyframes this often (in 5 s) means the path cannot keep up. */
const STRAINED = 2;
/** Tracks 0–1, layers 0–3. */
const TRACKS = 2;
const LAYERS = 4;
/** The largest video frame the protocol carries: 1000 fragments of 1000 bytes (GROUP.md §7.4). */
const MAX_FRAME = 1000 * 1000;
/**
 * After a frame too large to send, a layer's bitrate (and size, if it came
 * to that) stays down this long (ms), then comes back a step at a time.
 */
const OVERSIZED_HOLD = 30_000;
/**
 * The layer a canvas gets, by the short side it is shown at in device
 * pixels: the smallest that covers it, at the sizes layers usually are
 * (180, 360 and 720 rows: layers 0, 1 and 2), else 3 (1080 rows).
 */
const SHOWN = [180, 360, 720];
/** How long a canvas's size must hold before we subscribe by it: resizing and moving tiles would flap. */
const SETTLE = 300;
/** Keyframe requests this soon after a layer starts are receivers moving to it, not losses. */
const STARTING = 1000;
/** How soon the relay notices a layer is no longer sent, and moves its receivers to the next one down. */
const LAYER_FRESH = 1000;
/**
 * Our uplink does not keep up while more video waits than this many bytes,
 * and than leaves in `UPLINK_DRAIN` ms at the pace it has been leaving
 * (a keyframe passing through is no sign of trouble), for `CONGESTED_FOR`
 * ms; as the relay judges receivers' links (GROUP.md §3.3).
 */
const UPLINK_QUEUED = 32 << 10;
const UPLINK_DRAIN = 250;
const CONGESTED_FOR = 300;
/** After a change, the uplink's signals are ignored this long (ms): the queue takes a moment to drain. */
const UPLINK_SETTLE = 2000;
/**
 * An uplink that keeps up this long (ms) gets a stopped layer back to try;
 * the wait doubles, up to `PROBE_MAX`, each time a try fails within
 * `PROBE_TRIAL`.
 */
const PROBE_WAIT = 8000;
const PROBE_MAX = 60_000;
const PROBE_TRIAL = 5000;
/**
 * When any Video of a connection last changed what it sends for the uplink:
 * a camera's and a screen's share it, so that one sign of trouble stops one
 * layer, not one of each.
 */
const uplinkChanged = new WeakMap();

/**
 * Starts video in a group. With `stream`, sends its video track as
 * `track`; without, only receives. Resolves to a Video; draw others with
 * `attach`.
 */
export async function startVideo(conn, options = {}) {
  const video = new Video(conn, options);
  await video.start();
  return video;
}

export class Video extends EventTarget {
  #conn;
  #options;
  #element = null;
  /** The track we send now, and its "ended" listener. */
  #source = null;
  #timer = undefined;
  /** What we send: a layer each, smallest first (see `layerState`). */
  #layers;
  /** Which layers of our track the relay says are wanted (bit n: layer n); null until it says. */
  #demand = null;
  /** Our uplink: the largest layer it carries now (`LAYERS - 1`: all), and what it showed lately. */
  #uplink = {
    cap: LAYERS - 1,
    changedAt: 0,
    congestedAt: null,
    congestedSince: null,
    raisedAt: null,
    wait: PROBE_WAIT,
    dropped: 0,
    queued: 0,
    pace: 0,
  };
  /** Since when a keyframe has waited for room (see `#keyframeLayer`). */
  #keyframeHeld = null;
  /** Quarter turns, clockwise: of what we send, and of what we show of each member. */
  #rotation = 0;
  #views = new Map();
  #sending = true;
  // Receiving, by member and track (the key "member track"):
  /** A decoder, the codec it is configured for, and the layer arriving. */
  #decoders = new Map();
  #canvases = new Map();
  #received = new Map();
  /** Subscriptions: what we told the core, layers set by hand, and changes waiting to settle. */
  #subscribed = new Map();
  #chosen = new Map();
  #settling = new Map();
  #unattached;
  /** The largest layer we subscribe to by ourselves (data saver); null: no limit. */
  #maxLayer = null;
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
      receive: true,
      ...options,
    };
    const o = this.#options;
    if (!(o.track === 0 || o.track === 1)) throw new RangeError("a video track is 0 or 1");
    this.#layers = this.#layersFrom(o.layers);
    // Someone who sees every frame (`onFrame`) wants some video of everyone; a canvas, only what is shown.
    this.#unattached = "unattached" in o ? o.unattached : o.onFrame ? 0 : null;
  }

  /** The bitrate our encoders run at now, all layers together, in bits per second. */
  get bitrate() {
    return this.#layers.reduce((sum, l) => sum + (l.encoder ? l.bitrate : 0), 0);
  }

  /** The codec of our largest layer running (or else configured), or null when not sending. */
  get codec() {
    return (this.#layers.findLast((l) => l.encoder) ?? this.#layers.at(-1)).codec?.codec ?? null;
  }

  /** The track we send: 0 or 1. */
  get track() {
    return this.#options.track;
  }

  async start() {
    if (typeof VideoDecoder !== "function") throw new Error("this browser has no WebCodecs video");
    const on = (target, type, handler) => {
      target.addEventListener(type, handler);
      this.#listeners.push([target, type, handler]);
    };
    const conn = this.#conn;
    on(conn, "keyframerequest", (e) => this.#keyframeRequested(e.detail));
    on(conn, "videodemand", (e) => this.#demanded(e.detail));
    this.#demand = conn.videoDemand?.[this.#options.track] ?? null;
    if (this.#options.receive) {
      on(conn, "video", (e) => this.#receive(e.detail));
      on(conn, "joined", (e) => this.#settleMember(e.detail.member));
      on(conn, "left", (e) => this.#forget(e.detail.member));
      if (typeof document !== "undefined") on(document, "visibilitychange", () => this.#settleAll());
      if (typeof ResizeObserver === "function") {
        this.#observer = new ResizeObserver((entries) => {
          for (const { target } of entries) for (const [key, canvas] of this.#canvases) if (canvas === target) this.#settle(key);
        });
      }
      for (const member of conn.members.keys()) this.#settleMember(member);
    }
    const track = this.#options.stream?.getVideoTracks()[0];
    if (track) await this.#startSending(track);
  }

  /**
   * Draws `member`'s video `track` (default 0) into `canvas` from now on
   * (`null` to stop). Our own member ID shows what this Video sends, as
   * others will see it. Unless set by hand (`setLayer`), we subscribe to
   * the layer that suits the size the canvas is shown at, and to none
   * while it is not shown.
   */
  attach(member, canvas, { track = 0 } = {}) {
    const key = keyOf(member, track);
    const before = this.#canvases.get(key);
    if (before === canvas) return;
    if (before && ![...this.#canvases].some(([k, c]) => k !== key && c === before)) this.#observer?.unobserve(before);
    if (canvas) {
      this.#canvases.set(key, canvas);
      if (typeof canvas.getBoundingClientRect === "function") this.#observer?.observe(canvas);
    } else this.#canvases.delete(key);
    this.#settle(key);
  }

  /**
   * Which layer of `member`'s video `track` (default 0) to receive: 0–3,
   * `null` for none, or "auto" (the default) to follow the canvas it is
   * attached to.
   */
  setLayer(member, layer, { track = 0 } = {}) {
    const key = keyOf(member, track);
    if ((this.#chosen.has(key) ? this.#chosen.get(key) : "auto") === layer) return;
    if (layer === "auto") this.#chosen.delete(key);
    else if (isLayer(layer) || layer === null) this.#chosen.set(key, layer);
    else throw new RangeError('a layer is 0 to 3, null or "auto"');
    this.#settle(key, true);
  }

  /**
   * Data saver: the largest layer we receive by ourselves (by canvas size,
   * or `unattached`), 0–3, or `null` for no limit. Layers set by hand with
   * `setLayer` are as set.
   */
  setMaxLayer(layer) {
    if (!(isLayer(layer) || layer === null)) throw new RangeError("a layer is 0 to 3, or null");
    if (layer === this.#maxLayer) return;
    this.#maxLayer = layer;
    for (const key of this.#keys()) this.#settle(key, true);
  }

  /** The data saver's limit (`setMaxLayer`). */
  get maxLayer() {
    return this.#maxLayer;
  }

  /**
   * Sends `stream`'s video track from now on, instead of what was sent
   * before; `null` stops sending (receiving goes on). `layers` replaces the
   * layers we send (a camera that turns out to give less than 1080p, say),
   * `contentHint` the track's hint. The others' decoders are untouched.
   */
  async setStream(stream, { layers, contentHint } = {}) {
    this.#stopSending();
    if (layers) this.#layers = this.#layersFrom(layers);
    if (contentHint !== undefined) this.#options.contentHint = contentHint;
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

  /** Whether we are sending a track now (until `setStream(null)`, `stop()`, or the track ends). */
  get sending() {
    return this.#element !== null;
  }

  /** Our encoders' numbers, layer by layer, what is wanted of us, and per member and track what arrived and was decoded. */
  stats() {
    const layers = this.#layers.map((l) => ({
      layer: l.layer,
      running: !!l.encoder,
      codec: l.codec?.codec ?? null,
      hardware: !!l.codec?.hardware,
      width: l.size?.[0] ?? Math.round(l.width * l.scale),
      height: l.size?.[1] ?? Math.round(l.height * l.scale),
      bitrate: l.bitrate,
      ...l.stats,
    }));
    const total = (key) => layers.reduce((sum, l) => sum + l[key], 0);
    const u = this.#uplink;
    return {
      track: this.#options.track,
      bitrate: this.bitrate,
      codec: this.codec,
      demand: this.#demand,
      maxLayer: this.#maxLayer,
      uplink: {
        congested: u.congestedAt !== null && performance.now() - u.congestedAt < 1000,
        cappedAt: u.cap < LAYERS - 1 ? u.cap : null,
        queued: u.queued,
        dropped: u.dropped,
        pace: u.pace,
        wait: u.wait,
      },
      encoded: total("encoded"),
      sent: total("sent"),
      keyframes: total("keyframes"),
      skipped: total("skipped"),
      oversized: total("oversized"),
      requests: total("requests"),
      layers,
      members: [...this.#received]
        .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
        .map(([key, s]) => ({
          member: s.member,
          track: s.track,
          subscribed: this.#subscribed.get(key),
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
    for (const [key, layer] of this.#subscribed) if (layer !== null) this.#tell(key, null);
  }

  // Sending.

  #layersFrom(list) {
    const o = this.#options;
    // Without layers, one: layer 0, as video was before simulcast.
    const given = list?.length ? list : [{ layer: 0, width: o.width, height: o.height, bitrate: o.bitrate, minBitrate: o.minBitrate }];
    const layers = given.map((l) => layerState(l, o)).sort((a, b) => a.layer - b.layer);
    if (layers.some((l, i) => !isLayer(l.layer) || l.layer === layers[i + 1]?.layer)) {
      throw new RangeError("video layers are 0 to 3, each at most once");
    }
    return layers;
  }

  #stopSending() {
    clearTimeout(this.#timer);
    if (this.#source) {
      this.#source.track.removeEventListener("ended", this.#source.ended);
      this.#source = null;
    }
    if (this.#element) {
      this.#element.pause();
      this.#element.srcObject = null;
      this.#element = null;
    }
    for (const l of this.#layers) this.#stopLayer(l);
  }

  async #startSending(track) {
    const o = this.#options;
    if (o.contentHint !== undefined && "contentHint" in track) track.contentHint = o.contentHint;
    // Each layer's codec, chosen once per configuration (see `choose`).
    await Promise.all(
      this.#layers.map(async (l) => {
        if (l.codec) return;
        [l.codec, l.fallback] = await choose(l, o);
        l.codec ??= l.fallback;
      }),
    );
    if (this.#layers.some((l) => !l.codec)) throw new Error("no video codec this browser can encode");
    // The camera through a <video> element, which turns it upright, drawn
    // at our frame rate.
    const element = document.createElement("video");
    this.#element = element;
    element.muted = true;
    element.playsInline = true;
    element.srcObject = new MediaStream([track]);
    // The browser's own "stop sharing" ends a screen's track: we stop with it.
    const ended = () => {
      if (this.#element !== element) return;
      this.#stopSending();
      this.dispatchEvent(new Event("ended"));
    };
    this.#source = { track, ended };
    track.addEventListener("ended", ended);
    await element.play();
    if (this.#element !== element) return;
    this.#run();
    const tick = () => {
      if (this.#element !== element) return;
      if (this.#sending) this.#uplinkCheck(performance.now());
      if (element.readyState >= 2 && element.videoWidth) this.#capture(element);
      this.#timer = setTimeout(tick, 1000 / o.framerate);
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

  /**
   * Our layer that serves `layer`: the nearest at or below it, else the
   * lowest; none above what our uplink carries now, but for the lowest.
   */
  #layer(layer) {
    return this.#layers.findLast((l) => l.layer <= Math.min(layer, this.#uplink.cap)) ?? this.#layers[0];
  }

  #startLayer(l) {
    if (l.encoder) return;
    const encoder = new VideoEncoder({
      output: (chunk) => this.#output(l, chunk),
      error: (e) => this.#failed(l, encoder, e),
    });
    l.encoder = encoder;
    l.fedAt = l.outputAt = null;
    l.size = null; // configured for the first frame's shape
    l.keyframeDue = true; // receivers moving to it start there
    l.broken = false;
    l.moversUntil = performance.now() + STARTING;
  }

  #stopLayer(l) {
    if (l.encoder && l.encoder.state !== "closed") l.encoder.close();
    l.encoder = null;
  }

  /**
   * An encoder failed (a hardware one out of sessions, say, or refusing a
   * shape): the layer goes on with its software fallback; if that is what
   * failed, it stops and we say so.
   */
  #failed(l, encoder, error) {
    if (l.encoder !== encoder) return;
    l.encoder = null;
    if (l.fallback && l.codec !== l.fallback) {
      l.codec = l.fallback;
      this.#run();
      return;
    }
    l.codec = null;
    this.dispatchEvent(new CustomEvent("error", { detail: error }));
  }

  #demanded({ track, layers }) {
    if (track !== this.#options.track) return;
    this.#demand = layers;
    this.#run();
  }

  #keyframeRequested({ from, track, layer }) {
    if (track !== this.#options.track) return;
    const l = this.#layer(layer ?? 0);
    l.stats.requests++;
    l.keyframeDue = true;
    if (performance.now() >= l.moversUntil) this.#strained(l, from);
  }

  /**
   * Whether our uplink keeps up, from how far our video is backed up: it
   * does not if video was dropped, or if more than `UPLINK_QUEUED` bytes,
   * and more than leave in `UPLINK_DRAIN` at the pace video has been
   * leaving, have waited for `CONGESTED_FOR`. Then our largest running
   * layer stops at once (unless it is the only one: the smallest goes on,
   * whatever the link; the relay moves receivers that cannot take it to
   * audio only); after `PROBE_WAIT` without a sign, it starts again from a
   * keyframe, and a try that fails within `PROBE_TRIAL` doubles the next
   * wait. Signals are ignored for `UPLINK_SETTLE` after each change.
   *
   * Both of a member's tracks share the uplink: each Video sending one
   * judges it for its own layers, and each change, whoever made it, starts
   * `UPLINK_SETTLE` for all.
   */
  #uplinkCheck(now) {
    const backlog = this.#conn.videoBacklog;
    if (!backlog) return;
    const u = this.#uplink;
    const dropped = backlog.dropped > u.dropped;
    u.dropped = backlog.dropped;
    u.queued = backlog.queued;
    u.pace = backlog.pace ?? 0;
    const behind = backlog.queued > UPLINK_QUEUED && backlog.queued > (u.pace * UPLINK_DRAIN) / 1000;
    if (!dropped && !behind) {
      u.congestedSince = null;
      if (u.raisedAt !== null && now - u.raisedAt >= PROBE_TRIAL) {
        u.raisedAt = null;
        u.wait = PROBE_WAIT;
      }
      if (u.cap < LAYERS - 1 && now - Math.max(u.congestedAt ?? 0, u.changedAt) >= u.wait) {
        // The layer just above the cap back, on trial.
        const next = this.#layers.find((l) => l.layer > u.cap);
        u.cap = next && next !== this.#layers.at(-1) ? next.layer : LAYERS - 1;
        u.changedAt = u.raisedAt = now;
        uplinkChanged.set(this.#conn, now);
        this.#run();
      }
      return;
    }
    // A moment of queueing (a keyframe leaving) is no sign, and does not put
    // off a try at more either: only video dropped, or queued for long.
    u.congestedSince ??= now;
    if (!dropped && now - u.congestedSince < CONGESTED_FOR) return;
    u.congestedAt = now;
    const changed = Math.max(u.changedAt, uplinkChanged.get(this.#conn) ?? 0);
    if (changed !== 0 && now - changed < UPLINK_SETTLE) return;
    u.congestedSince = null;
    const running = this.#layers.filter((l) => l.encoder);
    if (running.length < 2) return;
    if (u.raisedAt !== null && now - u.raisedAt < PROBE_TRIAL) u.wait = Math.min(PROBE_MAX, u.wait * 2);
    u.raisedAt = null;
    u.cap = running.at(-1).layer - 1;
    u.changedAt = now;
    uplinkChanged.set(this.#conn, now);
    this.#run();
    // What of the stopped layer still waits would only hold up the rest.
    this.#conn.dropQueuedVideo?.(this.#options.track, u.cap);
    // Its receivers move to the layer serving them now once the relay sees
    // it stopped: a keyframe of that one just then spares them asking (they
    // do, if our backed-up uplink got its last frames there late).
    const serving = new Set();
    for (let layer = 0; layer < LAYERS; layer++) if (this.#demand & (1 << layer)) serving.add(this.#layer(layer));
    for (const l of serving) {
      l.keyframeAt = now + LAYER_FRESH + 100;
      l.moversUntil = l.keyframeAt + STARTING;
    }
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
    const keyed = this.#keyframeLayer(now);
    let drawn = null;
    for (const l of this.#layers) {
      const encoder = l.encoder;
      if (!encoder || encoder.state === "closed") continue;
      // Skip frames while the encoder is behind. Hardware encoders keep a
      // few frames in their pipeline before the first comes out: starved of
      // input below that, they would put out nothing at all.
      if (encoder.encodeQueueSize > (l.codec?.hardware ? HARDWARE_QUEUE : 2)) {
        l.stats.skipped++;
        if (l.codec?.hardware && l.fedAt !== null && now - Math.max(l.fedAt, l.outputAt ?? 0) > HARDWARE_STALL) {
          encoder.close();
          this.#failed(l, encoder, new Error("the hardware encoder stalled"));
        }
        continue;
      }
      const [w, h] = fit(sw, sh, { width: l.width * l.scale, height: l.height * l.scale });
      if (l.size?.[0] !== w || l.size?.[1] !== h) {
        l.size = [w, h];
        l.fresh = true;
        this.#configure(l); // a new shape: a new configuration, and a keyframe
      }
      // A new configuration's first frame is a keyframe whatever we ask: it waits its turn.
      if (l.fresh && l !== keyed) continue;
      l.fresh = false;
      const canvas = (l.canvas ??= typeof OffscreenCanvas === "function" ? new OffscreenCanvas(w, h) : document.createElement("canvas"));
      if (canvas.width !== w || canvas.height !== h) [canvas.width, canvas.height] = [w, h];
      draw(canvas.getContext("2d"), element, w, h, this.#rotation);
      const frame = new VideoFrame(canvas, { timestamp });
      const keyFrame = l === keyed;
      if (keyFrame) {
        l.keyframeDue = false;
        l.keyframeAt = 0;
        l.lastKeyframe = now;
      }
      l.fedAt ??= now;
      encoder.encode(frame, { keyFrame });
      frame.close();
      this.#raise(l, now);
      drawn = canvas;
    }
    // Ours as the others see it at best, whoever is watching: our largest
    // layer, drawn on its own while nobody wants it.
    const preview = this.#canvases.get(keyOf(this.#conn.id, this.#options.track));
    const top = this.#layers.at(-1);
    if (preview && drawn && drawn === top.canvas) copyTo(preview, drawn, drawn.width, drawn.height);
    else if (preview) {
      const [w, h] = fit(sw, sh, top);
      if (preview.width !== w || preview.height !== h) [preview.width, preview.height] = [w, h];
      draw(preview.getContext("2d"), element, w, h, this.#rotation);
    }
  }

  /**
   * The layer whose keyframe goes out with this frame: one at most, and
   * none while much of our video is still to go (see `KEYFRAME_ROOM`), so
   * that those of several layers (all starting at once, or their periodic
   * ones lined up, or a camera's and a screen's) do not arrive as one burst
   * that our backlog, and the relay's, cannot hold. Those asked for go
   * first, the longest waiting first; the others follow.
   */
  #keyframeLayer(now) {
    const running = this.#layers.filter((l) => l.encoder && l.encoder.state !== "closed" && l.encoder.encodeQueueSize <= 2);
    const asked = (l) => l.keyframeDue || l.fresh || (l.keyframeAt !== 0 && now >= l.keyframeAt);
    const periodic = (l) => now - l.lastKeyframe >= this.#options.keyframeInterval;
    const longest = (list) => list.reduce((a, l) => (!a || l.lastKeyframe < a.lastKeyframe ? l : a), null);
    const next = longest(running.filter(asked)) ?? longest(running.filter(periodic));
    if (!next) return (this.#keyframeHeld = null);
    this.#keyframeHeld ??= now;
    if ((this.#conn.videoBacklog?.queued ?? 0) > KEYFRAME_ROOM && now - this.#keyframeHeld < KEYFRAME_HOLD) return null;
    this.#keyframeHeld = null;
    return next;
  }

  #configure(l) {
    if (!l.size || !l.encoder || l.encoder.state === "closed") return;
    const [width, height] = l.size;
    l.encoder.configure(encoderConfig(l.codec, width, height, l.bitrate, this.#options));
    l.keyframeDue = true;
  }

  #output(l, chunk) {
    if (!l.encoder) return; // stopped meanwhile
    const keyframe = chunk.type === "key";
    l.stats.encoded++;
    l.outputAt = performance.now();
    if (1 + chunk.byteLength > MAX_FRAME) {
      // More than the protocol carries: not sent, and neither is what
      // follows until a keyframe (it would refer to this one), which comes
      // at once at a lower bitrate, in proportion; at the floor, a smaller
      // picture (noise, say, which no bitrate squeezes).
      l.stats.oversized++;
      l.broken = true;
      l.oversizedAt = l.raisedAt = performance.now();
      if (l.bitrate > l.minBitrate) {
        const lower = l.bitrate * Math.min(0.6, (0.75 * MAX_FRAME) / chunk.byteLength);
        l.bitrate = Math.max(l.minBitrate, Math.round(lower));
      } else l.scale = Math.max(0.25, l.scale * 0.75);
      this.#configure(l);
      return;
    }
    if (l.broken && !keyframe) return;
    l.broken = false;
    const data = new Uint8Array(1 + chunk.byteLength);
    data[0] = l.codec.id;
    chunk.copyTo(data.subarray(1));
    if (keyframe) l.stats.keyframes++;
    // Our clock, in milliseconds (wrapping at 2³²).
    const timestamp = Math.round(chunk.timestamp / 1000) >>> 0;
    if (this.#conn.sendVideo(this.#options.track, l.layer, keyframe, timestamp, data)) l.stats.sent++;
  }

  /**
   * Receivers of this layer keep losing frames: a lower bitrate, but only
   * when more than one of them does (or the only other member). One slow
   * receiver is the relay's to fit, by moving it to a smaller layer; slowing
   * the layer for everyone would make it everyone's problem.
   */
  #strained(l, from) {
    const now = performance.now();
    l.requests = l.requests.filter((r) => now - r.at < 5000);
    l.requests.push({ at: now, from });
    const askers = new Set(l.requests.map((r) => r.from)).size;
    const others = Math.max(1, this.#conn.members.size);
    if (l.requests.length >= STRAINED && askers >= Math.min(2, others) && l.encoder) {
      l.bitrate = Math.max(l.minBitrate, Math.round(l.bitrate * 0.7));
      l.requests = [];
      l.raisedAt = now;
      this.#configure(l);
    }
  }

  /**
   * Nobody has asked for a while: a little more, up to what was asked for;
   * a picture made smaller for frames too large first goes back up a step.
   */
  #raise(l, now) {
    if (now - l.raisedAt < 10_000 || now - l.oversizedAt < OVERSIZED_HOLD) return;
    if (l.scale < 1) {
      l.scale = Math.min(1, l.scale / 0.75);
      l.raisedAt = now;
      return; // configured for the new size at the next frame
    }
    if (l.bitrate >= l.target || l.requests.some((r) => now - r.at < 10_000)) return;
    l.bitrate = Math.min(l.target, Math.round(l.bitrate * 1.15));
    l.raisedAt = now;
    this.#configure(l);
  }

  // Receiving.

  #receive({ from, track, layer = 0, keyframe, timestamp, data }) {
    const key = keyOf(from, track);
    let stats = this.#received.get(key);
    if (!stats) {
      stats = { member: from, track, layer, codec: null, received: 0, decoded: 0, errors: 0, lastTimestamp: 0, layers: new Map() };
      this.#received.set(key, stats);
    }
    stats.received++;
    stats.layer = layer;
    const counts = stats.layers.get(layer) ?? { received: 0, decoded: 0 };
    stats.layers.set(layer, counts);
    counts.received++;
    const codec = CODECS.find((c) => c.id === data[0]);
    if (!codec) return;
    let d = this.#decoders.get(key);
    if (!d || d.decoder.state === "closed") {
      if (!keyframe) return; // the core asks for a keyframe; wait for it
      d = { decoder: null, id: null, codec: null, layer: null, pending: [] };
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
    // Another layer, or codec, starts at its keyframe.
    if (!keyframe && (d.id !== codec.id || d.layer !== layer)) return;
    if (keyframe) {
      // H.264's profile and level are in the keyframe itself (its SPS).
      const string = codec.id !== 3 ? codec.codec : (avcCodec(data) ?? (d.id === 3 ? d.codec : codec.codec));
      if (d.codec !== string || d.layer !== layer) {
        // Another size or codec from here on: configured afresh, behind
        // the frames queued, so that the picture goes on until the new
        // one's first.
        d.decoder.configure({ codec: string, optimizeForLatency: true });
        d.id = codec.id;
        d.codec = stats.codec = string;
        d.layer = layer;
      }
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
    const canvas = this.#canvases.get(keyOf(member, track));
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

  // Subscriptions, by member and track.

  #settleMember(member) {
    for (let track = 0; track < TRACKS; track++) this.#settle(keyOf(member, track));
  }

  /**
   * Subscribes to the layer a member's track should be in now, at once the
   * first time (and with `now`), else once it has held for a moment.
   */
  #settle(key, now = false) {
    if (!this.#options.receive || memberOf(key) === this.#conn.id) return;
    clearTimeout(this.#settling.get(key));
    this.#settling.delete(key);
    if (now || !this.#subscribed.has(key)) return this.#tell(key, this.#wanted(key));
    this.#settling.set(
      key,
      setTimeout(() => {
        this.#settling.delete(key);
        this.#tell(key, this.#wanted(key));
      }, SETTLE),
    );
  }

  #keys() {
    const keys = new Set(this.#subscribed.keys());
    for (const member of this.#conn.members.keys()) for (let track = 0; track < TRACKS; track++) keys.add(keyOf(member, track));
    return keys;
  }

  #settleAll() {
    for (const key of this.#keys()) this.#settle(key);
  }

  /**
   * The layer to receive of a member's track: as set, else by the size its
   * canvas is shown at (or `unattached`), no larger than `setMaxLayer` says.
   */
  #wanted(key) {
    if (this.#chosen.has(key)) return this.#chosen.get(key);
    if (typeof document !== "undefined" && document.visibilityState === "hidden") return null;
    const canvas = this.#canvases.get(key);
    const layer = canvas ? shownLayer(canvas) : this.#unattached;
    return layer === null || this.#maxLayer === null ? layer : Math.min(layer, this.#maxLayer);
  }

  #tell(key, layer) {
    if (this.#subscribed.has(key) && this.#subscribed.get(key) === layer) return;
    this.#subscribed.set(key, layer);
    try {
      this.#conn.subscribe(memberOf(key), trackOf(key), layer);
    } catch {
      // Left the group: nothing to subscribe to.
    }
  }
}

const keyOf = (member, track) => `${member} ${track}`;
const memberOf = (key) => key.slice(0, key.lastIndexOf(" "));
const trackOf = (key) => Number(key.slice(key.lastIndexOf(" ") + 1));
const isLayer = (layer) => Number.isInteger(layer) && layer >= 0 && layer < LAYERS;

/** The layer that suits the size `canvas` is shown at (see `SHOWN`); none while it has no size. */
function shownLayer(canvas) {
  // Not on the page (an OffscreenCanvas, say), its size tells us nothing: the largest there is.
  if (typeof canvas.getBoundingClientRect !== "function" || !canvas.isConnected) return LAYERS - 1;
  const { width, height } = canvas.getBoundingClientRect();
  const side = Math.min(width, height) * (globalThis.devicePixelRatio || 1);
  if (!side) return null;
  const layer = SHOWN.findIndex((most) => side <= most);
  return layer < 0 ? LAYERS - 1 : layer;
}

/** A layer we send, from its options: what it aims for and where it is now. */
function layerState({ layer, width, height, bitrate, minBitrate, codec }, defaults) {
  const target = bitrate ?? defaults.bitrate;
  return {
    layer,
    width: width ?? defaults.width,
    height: height ?? defaults.height,
    target,
    minBitrate: Math.min(target, minBitrate ?? defaults.minBitrate),
    bitrate: target,
    /** A codec string asked for, for this layer. */
    want: codec ?? defaults.codec,
    /** The codec it encodes with (see `choose`), and the software one to fall back on. */
    codec: null,
    fallback: null,
    encoder: null,
    /** When the encoder was first given a frame, and last put one out (to tell a stalled one). */
    fedAt: null,
    outputAt: null,
    canvas: null,
    size: null,
    /** Below 1 while frames came out too large even at the lowest bitrate. */
    scale: 1,
    /** Configured for a new size, nothing encoded since. */
    fresh: false,
    keyframeDue: true,
    /** A frame was too large to send: nothing goes until the next keyframe. */
    broken: false,
    oversizedAt: -Infinity,
    lastKeyframe: 0,
    /** A keyframe due at this time (0: none). */
    keyframeAt: 0,
    /** Keyframe requests until then are receivers moving to it. */
    moversUntil: 0,
    requests: [],
    raisedAt: 0,
    stats: { encoded: 0, sent: 0, keyframes: 0, skipped: 0, oversized: 0, requests: 0 },
  };
}

/** 0–3 quarter turns from degrees. */
function quarter(degrees) {
  return ((Math.round(degrees / 90) % 4) + 4) % 4;
}

/**
 * The size to encode a picture of `sw` × `sh` at: within the configured
 * size, turned to the picture's shape (a tall picture gets a tall size),
 * keeping its proportions, in even numbers. Never larger than the picture:
 * a 1440 × 900 screen goes out as it is in a 1920 × 1080 layer.
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

/** Our codec entry for a WebCodecs codec string (any H.264 profile, say), or null. */
function codecFor(string) {
  const known = CODECS.find((c) => c.codec === string);
  if (known) return known;
  const family = /^(avc1|avc3)\./.test(string) ? 3 : string.startsWith("vp09.") ? 2 : string.startsWith("av01.") ? 4 : null;
  const base = CODECS.find((c) => c.id === family);
  return base ? { ...base, codec: string } : null;
}

function encoderConfig(c, width, height, bitrate, { framerate, contentHint }) {
  return {
    codec: c.codec,
    width,
    height,
    bitrate,
    framerate,
    latencyMode: "realtime",
    hardwareAcceleration: c.hardware ? "prefer-hardware" : "no-preference",
    ...(c.avc ? { avc: c.avc } : {}),
    // Screens: sharp text before smooth motion, where the encoder takes the hint.
    ...(contentHint ? { contentHint } : {}),
  };
}

const supportCache = new Map();

/** Whether this browser encodes `config` (asked once per configuration). */
function supports(config) {
  const key = JSON.stringify(config);
  if (!supportCache.has(key)) {
    supportCache.set(
      key,
      VideoEncoder.isConfigSupported(config).then(
        (r) => !!r.supported,
        () => false,
      ),
    );
  }
  return supportCache.get(key);
}

/**
 * A layer's codec, and its software fallback: a hardware encoder first,
 * H.264 for layers 2 and 3 (where software encoding costs the most; its
 * level picked for the size), then VP8; failing those, VP8 in software,
 * then whatever else this browser encodes. A codec asked for (the layer's,
 * or the `codec` option) is tried in hardware and then in software.
 */
async function choose(l, o) {
  const vp8 = CODECS[0];
  const avc = codecFor(l.width * l.height > 1280 * 720 ? AVC_1080 : AVC_720);
  const asked = l.want ? codecFor(l.want) : null;
  const hardware = asked ? [asked] : [...(l.layer >= 2 ? [avc] : []), vp8];
  const software = asked ? [asked] : CODECS;
  const first = async (list, hw) => {
    for (const c of list) {
      const entry = { ...c, hardware: hw };
      if (await supports(encoderConfig(entry, l.width, l.height, l.target, o))) return entry;
    }
    return null;
  };
  const fallback = await first(software, false);
  return [await first(hardware, true), fallback];
}

/**
 * The WebCodecs codec string of an H.264 keyframe (`codec[1] ‖ Annex B`),
 * from its sequence parameter set: "avc1." and the profile, constraint and
 * level bytes; null if it has none near the start.
 */
function avcCodec(data) {
  const end = Math.min(data.length - 4, 256);
  for (let i = 1; i < end; i++) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1 && (data[i + 3] & 0x1f) === 7) {
      const hex = (b) => b.toString(16).padStart(2, "0");
      return `avc1.${hex(data[i + 4])}${hex(data[i + 5])}${hex(data[i + 6])}`;
    }
  }
  return null;
}
