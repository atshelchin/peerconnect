// PeerConnect voice for browsers: microphone → Opus (WebCodecs) → group →
// a jitter buffer per speaker → speakers.
//
// Latency, from mouth to ear, is one 20 ms frame to capture, about a
// millisecond to encode and decode, the network, and a jitter buffer that
// waits only as long as the network's jitter requires (30 ms on a good
// one) and drops audio to catch up after a stall instead of playing late.
// Nothing is sent while the speaker is silent.
//
// Each voice frame is `hold[1] ‖ Opus packet` (48 kHz, mono): `hold` is how
// many milliseconds the sender kept the frame before sending it (frames
// kept to avoid clipping the start of a word, say), so that receivers can
// tell that delay from the network's jitter. Other implementations that
// want to talk to this one use the same format.

const RATE = 48_000;

// Runs in the audio thread: cuts the microphone into frames, and mixes the
// speakers through per-speaker jitter buffers.
const WORKLET = String.raw`
const RATE = 48000;

class Capture extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.size = options.processorOptions.frame;
    this.buffer = new Float32Array(this.size);
    this.filled = 0;
  }
  process(inputs) {
    const input = inputs[0];
    if (!input || !input[0]) return true;
    const [left, right] = input;
    for (let i = 0; i < left.length; i++) {
      this.buffer[this.filled++] = right ? (left[i] + right[i]) / 2 : left[i];
      if (this.filled === this.size) {
        let sum = 0;
        for (const s of this.buffer) sum += s * s;
        const rms = Math.sqrt(sum / this.size);
        this.port.postMessage({ pcm: this.buffer, rms, time: currentTime }, [this.buffer.buffer]);
        this.buffer = new Float32Array(this.size);
        this.filled = 0;
      }
    }
    return true;
  }
}

// One speaker: frames by sequence number, played out after an adaptive delay.
class Speaker {
  constructor(frame, config) {
    this.frame = frame; // samples per frame, as this speaker sends them
    this.seconds = frame / RATE;
    this.config = config;
    this.frames = new Map();
    this.next = null; // the frame to play next, while playing
    this.current = null;
    this.position = 0;
    this.offsets = []; // arrival time minus send time, recent frames
    this.target = config.min;
    this.gain = 1;
    this.energy = 0;
    this.samples = 0;
    this.stats = { late: 0, concealed: 0, dropped: 0, played: 0 };
  }
  push(seq, pcm, hold) {
    if (this.next !== null && seq < this.next) {
      this.stats.late++;
      return;
    }
    this.frames.set(seq, pcm);
    // Jitter: the spread of (arrival - seq × frame - time the sender held
    // it) over the last 2 s.
    this.offsets.push(currentTime - seq * this.seconds - hold);
    if (this.offsets.length > 2 / this.seconds) this.offsets.shift();
    const spread = Math.max(...this.offsets) - Math.min(...this.offsets);
    this.target = Math.min(this.config.max, Math.max(this.config.min, spread + this.seconds));
  }
  buffered() {
    return this.frames.size * this.seconds;
  }
  mix(out, played) {
    if (this.next === null) {
      if (this.frames.size === 0 || this.buffered() < this.target) return;
      this.next = Math.min(...this.frames.keys());
    }
    let i = 0;
    while (i < out.length) {
      if (!this.current) {
        const pcm = this.frames.get(this.next);
        if (pcm) {
          this.frames.delete(this.next);
          this.current = pcm;
          this.position = 0;
          this.stats.played++;
          played.push(this.next);
        } else if (this.frames.size === 0) {
          // Underrun: the talk spurt is over, or the network stalled.
          // Wait to rebuffer.
          this.next = null;
          return;
        } else {
          // A frame lost on the way: play silence in its place.
          this.current = new Float32Array(this.frame);
          this.position = 0;
          this.stats.concealed++;
        }
      }
      const n = Math.min(out.length - i, this.frame - this.position);
      for (let k = 0; k < n; k++) {
        const s = this.current[this.position + k] * this.gain;
        out[i + k] += s;
        this.energy += s * s;
      }
      this.samples += n;
      i += n;
      this.position += n;
      if (this.position === this.frame) {
        this.current = null;
        this.next++;
      }
    }
    // Behind after a stall: skip ahead rather than play late.
    while (this.buffered() > this.target + 2 * this.seconds) {
      const oldest = Math.min(...this.frames.keys());
      this.frames.delete(oldest);
      this.stats.dropped++;
      if (this.next !== null && oldest >= this.next) this.next = oldest + 1;
    }
  }
}

class Playback extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.config = options.processorOptions.delay;
    this.speakers = new Map();
    this.reported = 0;
    this.port.onmessage = ({ data }) => {
      if (data.type === "frame") {
        let speaker = this.speakers.get(data.member);
        if (!speaker || speaker.frame !== data.pcm.length) {
          // New, or it changed its frame duration: start over.
          speaker = new Speaker(data.pcm.length, this.config);
          this.speakers.set(data.member, speaker);
        }
        speaker.push(data.seq, data.pcm, data.hold);
      } else if (data.type === "gain") {
        const speaker = this.speakers.get(data.member);
        if (speaker) speaker.gain = data.gain;
      } else if (data.type === "remove") {
        this.speakers.delete(data.member);
      }
    };
  }
  process(inputs, outputs) {
    const out = outputs[0][0];
    out.fill(0);
    const started = [];
    for (const [member, speaker] of this.speakers) {
      const played = [];
      speaker.mix(out, played);
      for (const seq of played) started.push({ member, seq });
    }
    for (const channel of outputs[0].slice(1)) channel.set(out);
    if (started.length) this.port.postMessage({ type: "played", started, time: currentTime });
    if (currentTime - this.reported >= 0.1) {
      this.reported = currentTime;
      const levels = [];
      for (const [member, s] of this.speakers) {
        const rms = s.samples ? Math.sqrt(s.energy / s.samples) : 0;
        levels.push({ member, rms, delay: s.buffered(), target: s.target, ...s.stats });
        s.energy = 0;
        s.samples = 0;
      }
      this.port.postMessage({ type: "levels", levels });
    }
    return true;
  }
}

registerProcessor("peerconnect-capture", Capture);
registerProcessor("peerconnect-playback", Playback);
`;

/**
 * Starts voice in a group: captures the microphone, sends it while you
 * speak, and plays the others. Call it from a user gesture (a click), so
 * that the browser lets audio start.
 *
 * Options:
 * - `bitrate`: Opus bits per second (default 24000).
 * - `frameMs`: 10, 20 (default), 40 or 60. Longer frames send fewer packets
 *   and add latency.
 * - `threshold`: the level, in dBFS, above which the microphone counts as
 *   speech (default -50). `vad: false` sends all the time.
 * - `minDelay`, `maxDelay`: bounds of the jitter buffer in ms (30, 500). On a
 *   steady network it stays near the lower; the upper is how far it may grow
 *   on a bad one, trading delay for audio that does not break up.
 * - `stream`: a MediaStream to send instead of the microphone.
 * - `context`: an AudioContext to use (48 kHz).
 * - `onCapture(seq, now)`, `onPlay(member, seq, now)`: called as each of our
 *   frames is captured, and as each frame of another member starts to
 *   play (`now` from `performance.now()`), to measure latency.
 */
export async function startVoice(conn, options = {}) {
  if (typeof AudioEncoder === "undefined" || typeof AudioDecoder === "undefined") {
    throw new Error("voice needs WebCodecs audio (AudioEncoder and AudioDecoder)");
  }
  const frameMs = options.frameMs ?? 20;
  const config = {
    codec: "opus",
    sampleRate: RATE,
    numberOfChannels: 1,
    bitrate: options.bitrate ?? 24_000,
    opus: { frameDuration: frameMs * 1000, complexity: 5 },
  };
  const { supported } = await AudioEncoder.isConfigSupported(config);
  if (!supported) throw new Error("this browser cannot encode Opus");
  const voice = new Voice(conn, options, config, frameMs);
  await voice.start();
  return voice;
}

export class Voice extends EventTarget {
  /** Whether the microphone is muted (nothing is sent). */
  muted = false;
  /** Whether the others are muted (nothing is played). */
  deafened = false;

  #conn;
  #options;
  #config;
  #frame;
  #frameUs;
  #context;
  #ownsContext;
  #stream;
  #ownsStream;
  #capture;
  #playback;
  #output;
  #encoder;
  #decoders = new Map();
  #seq = 0;
  #active = 0; // frames of speech still to send (hangover)
  #preroll = [];
  #speaking = new Set();
  #levels = new Map();
  #listeners = [];

  constructor(conn, options, config, frameMs) {
    super();
    this.#conn = conn;
    this.#options = options;
    this.#config = config;
    this.#frame = (RATE * frameMs) / 1000;
    this.#frameUs = frameMs * 1000;
  }

  async start() {
    this.#ownsContext = !this.#options.context;
    const context = (this.#context =
      this.#options.context ?? new AudioContext({ sampleRate: RATE, latencyHint: "interactive" }));
    const url = URL.createObjectURL(new Blob([WORKLET], { type: "text/javascript" }));
    try {
      await context.audioWorklet.addModule(url);
    } finally {
      URL.revokeObjectURL(url);
    }
    this.#ownsStream = !this.#options.stream;
    this.#stream =
      this.#options.stream ??
      (await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true, channelCount: 1 },
      }));

    this.#capture = new AudioWorkletNode(context, "peerconnect-capture", {
      numberOfOutputs: 1,
      processorOptions: { frame: this.#frame },
    });
    // A capture node must be pulled by the graph to run: through silence.
    const silent = new GainNode(context, { gain: 0 });
    context.createMediaStreamSource(this.#stream).connect(this.#capture).connect(silent).connect(context.destination);
    this.#capture.port.onmessage = ({ data }) => this.#captured(data);

    const delay = { min: (this.#options.minDelay ?? 30) / 1000, max: (this.#options.maxDelay ?? 500) / 1000 };
    this.#playback = new AudioWorkletNode(context, "peerconnect-playback", {
      numberOfInputs: 0,
      outputChannelCount: [context.destination.channelCount >= 2 ? 2 : 1],
      processorOptions: { delay },
    });
    this.#output = new GainNode(context, { gain: 1 });
    this.#playback.connect(this.#output).connect(context.destination);
    this.#playback.port.onmessage = ({ data }) => this.#reported(data);

    this.#encoder = new AudioEncoder({
      output: (chunk) => this.#encoded(chunk),
      error: (error) => this.dispatchEvent(new CustomEvent("error", { detail: error })),
    });
    this.#encoder.configure(this.#config);

    this.#listen("voice", (e) => this.#received(e.detail));
    this.#listen("left", (e) => this.#forget(e.detail.member));
    if (context.state === "suspended") await context.resume();
  }

  /** Mutes or unmutes the microphone. */
  setMuted(muted) {
    this.muted = muted;
    if (muted) this.#active = 0;
  }

  /** Mutes or unmutes everyone else. */
  setDeafened(deafened) {
    this.deafened = deafened;
    this.#output.gain.value = deafened ? 0 : 1;
  }

  /** Sets one member's volume (1 is normal, 0 silent). */
  setVolume(member, gain) {
    this.#playback.port.postMessage({ type: "gain", member, gain });
  }

  /** Who is speaking right now (our own ID included). */
  get speaking() {
    return new Set(this.#speaking);
  }

  /**
   * Per member: `rms` level, current `delay` and jitter buffer `target` (s),
   * frames `played`, `late`, `concealed` (lost) and `dropped` (to catch up).
   */
  stats() {
    return new Map(this.#levels);
  }

  /** Stops capturing and playing. */
  async stop() {
    for (const [type, listener] of this.#listeners) this.#conn.removeEventListener(type, listener);
    this.#listeners = [];
    if (this.#capture) this.#capture.port.onmessage = null;
    if (this.#playback) this.#playback.port.onmessage = null;
    this.#capture?.disconnect();
    this.#playback?.disconnect();
    if (this.#encoder?.state !== "closed") this.#encoder?.close();
    for (const { decoder } of this.#decoders.values()) if (decoder.state !== "closed") decoder.close();
    this.#decoders.clear();
    if (this.#ownsStream) for (const track of this.#stream?.getTracks() ?? []) track.stop();
    if (this.#ownsContext) await this.#context?.close();
  }

  #listen(type, listener) {
    this.#conn.addEventListener(type, listener);
    this.#listeners.push([type, listener]);
  }

  // Sending.

  #captured({ pcm, rms, time }) {
    if (this.#encoder.state !== "configured") return;
    const seq = this.#seq++;
    const loud = this.#options.vad === false || 20 * Math.log10(rms + 1e-9) > (this.#options.threshold ?? -50);
    const wasActive = this.#active > 0;
    if (this.muted) this.#active = 0;
    else if (loud) this.#active = Math.ceil(300 / (this.#frameUs / 1000)); // 300 ms hangover
    else if (this.#active > 0) this.#active--;
    this.#setSpeaking(this.#conn.id, this.#active > 0);
    this.#options.onCapture?.(seq & 0xffff, performance.now(), time);
    this.#encoder.encode(
      new AudioData({
        format: "f32-planar",
        sampleRate: RATE,
        numberOfFrames: this.#frame,
        numberOfChannels: 1,
        timestamp: seq * this.#frameUs,
        data: pcm,
      }),
    );
    this.#pending.set(seq, { what: this.#active > 0 ? (wasActive ? "send" : "onset") : "hold", at: performance.now() });
  }

  #pending = new Map();

  #encoded(chunk) {
    const seq = Math.round(chunk.timestamp / this.#frameUs);
    const { what, at } = this.#pending.get(seq) ?? { what: "hold", at: performance.now() };
    this.#pending.delete(seq);
    const data = new Uint8Array(1 + chunk.byteLength);
    chunk.copyTo(data.subarray(1));
    if (what === "hold") {
      // Kept briefly, so that the start of the next word is not clipped.
      this.#preroll.push([seq, data, at]);
      if (this.#preroll.length > 2) this.#preroll.shift();
      return;
    }
    const frames = what === "onset" ? [...this.#preroll.splice(0), [seq, data, at]] : [[seq, data, at]];
    const now = performance.now();
    for (const [s, d, captured] of frames) {
      d[0] = Math.min(255, Math.max(0, Math.round(now - captured)));
      this.#conn.sendVoice(s & 0xffff, d);
    }
  }

  // Receiving.

  #received({ from, seq, data }) {
    if (this.deafened || data.length < 2) return;
    let entry = this.#decoders.get(from);
    if (!entry) {
      const decoder = new AudioDecoder({
        output: (audio) => this.#decoded(from, audio),
        error: () => this.#decoders.delete(from),
      });
      decoder.configure({ codec: "opus", sampleRate: RATE, numberOfChannels: 1 });
      entry = { decoder, last: null, queue: [] };
      this.#decoders.set(from, entry);
    }
    // Unwrap the 16-bit sequence number around the last one seen.
    const last = entry.last ?? seq;
    const unwrapped = last + ((((seq - (last & 0xffff)) & 0xffff) ^ 0x8000) - 0x8000);
    entry.last = Math.max(entry.last ?? unwrapped, unwrapped);
    // Decoders may rewrite timestamps, but they output one frame per packet,
    // in order: match outputs to inputs by that.
    entry.queue.push([unwrapped, data[0] / 1000]);
    entry.decoder.decode(new EncodedAudioChunk({ type: "key", timestamp: unwrapped * 20_000, data: data.subarray(1) }));
  }

  #decoded(member, audio) {
    const pcm = new Float32Array(audio.numberOfFrames);
    audio.copyTo(pcm, { planeIndex: 0, format: "f32-planar" });
    audio.close();
    const [seq, hold] = this.#decoders.get(member)?.queue.shift() ?? [];
    if (seq === undefined) return;
    this.#playback.port.postMessage({ type: "frame", member, seq, pcm, hold }, [pcm.buffer]);
  }

  #forget(member) {
    const entry = this.#decoders.get(member);
    if (entry && entry.decoder.state !== "closed") entry.decoder.close();
    this.#decoders.delete(member);
    this.#playback.port.postMessage({ type: "remove", member });
    this.#levels.delete(member);
    this.#setSpeaking(member, false);
  }

  #reported(data) {
    if (data.type === "played") {
      const now = performance.now();
      for (const { member, seq } of data.started) this.#options.onPlay?.(member, seq & 0xffff, now, data.time);
      return;
    }
    for (const level of data.levels) {
      this.#levels.set(level.member, level);
      this.#setSpeaking(level.member, 20 * Math.log10(level.rms + 1e-9) > -60);
    }
  }

  #setSpeaking(member, speaking) {
    if (speaking === this.#speaking.has(member)) return;
    if (speaking) this.#speaking.add(member);
    else this.#speaking.delete(member);
    this.dispatchEvent(new CustomEvent("speaking", { detail: { member, speaking } }));
  }
}
