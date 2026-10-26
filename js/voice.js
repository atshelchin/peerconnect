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
//
// A lost datagram costs nothing audible most of the time: each packet also
// carries copies of the two before it, so one of the next two that arrive
// brings it back. The copies travel in the Opus packet's padding (RFC 6716
// §3.2.5), as an Opus extension (draft-ietf-mlcodec-opus-extension) with
// an experimental ID, which every Opus decoder ignores: receivers that know
// nothing of them, older versions of this one included, play the packet as
// before. What is lost for good is bridged by repeating the last pitch
// period heard, fading out over two frames, then silence.

const RATE = 48_000;

// The extension: ID 122 of those reserved for experiments, running to the
// end of the padding (L = 0), whose data starts with the experiment's
// number and version, as the draft recommends ("P", 1), and goes on with
// `back[1] ‖ length[2] ‖ packet` for each copy, of the packet `back` frames
// before this one.
const EXTENSION = (122 << 1) | 0;
const EXPERIMENT = [0x50, 1];
// Payloads stay within what any path carries as one datagram.
const MAX_PAYLOAD = 1000;

/**
 * `packet` with copies of earlier packets (`[back, packet]`, nearest
 * first) in its padding, as many as fit. A packet of any code becomes one
 * of code 3 with the same frames, which adds two bytes or so.
 */
export function withCopies(packet, copies) {
  const room = MAX_PAYLOAD - 1 - packet.length - 10; // hold, header, padding length, extension's ID and prefix
  const kept = [];
  let size = 0;
  for (const [back, copy] of copies) {
    if (size + 3 + copy.length > room) break;
    kept.push([back, copy]);
    size += 3 + copy.length;
  }
  if (!kept.length || packet.length < 1) return packet;
  const toc = packet[0];
  let head;
  let body = packet.subarray(1);
  // The frame count byte: VBR (0x80), padded (0x40), and the number of frames.
  if ((toc & 3) === 0) head = [toc | 3, 0x41];
  else if ((toc & 3) === 1) head = [toc | 3, 0x42]; // two frames of one size: CBR
  else if ((toc & 3) === 2) head = [toc | 3, 0xc2]; // the first frame's length stays where VBR expects it
  else if (packet.length < 2 || packet[1] & 0x40) return packet; // padded already: leave it be
  else (head = [toc, packet[1] | 0x40]), (body = packet.subarray(2));
  const padding = 1 + EXPERIMENT.length + size;
  // Its length: 255 for each 254 bytes, then the rest.
  const length = [];
  let n = padding;
  for (; n >= 255; n -= 254) length.push(255);
  length.push(n);
  const out = new Uint8Array(head.length + length.length + body.length + padding);
  out.set(head);
  out.set(length, head.length);
  let at = head.length + length.length;
  out.set(body, at);
  at += body.length;
  out[at++] = EXTENSION;
  out.set(EXPERIMENT, at);
  at += EXPERIMENT.length;
  for (const [back, copy] of kept) {
    out[at] = back;
    out[at + 1] = copy.length >> 8;
    out[at + 2] = copy.length & 0xff;
    out.set(copy, at + 3);
    at += 3 + copy.length;
  }
  return out;
}

/** The copies of earlier packets that `packet` carries, as `[back, packet]`. */
export function copiesIn(packet) {
  if (packet.length < 3 || (packet[0] & 3) !== 3 || !(packet[1] & 0x40)) return [];
  let at = 2;
  let padding = 0;
  for (;;) {
    if (at >= packet.length) return [];
    const b = packet[at++];
    padding += b === 255 ? 254 : b;
    if (b !== 255) break;
  }
  if (padding > packet.length - at) return [];
  // Walk the extensions, skipping those of others, for ours on the first frame.
  const end = packet.length;
  let frame = 0;
  for (at = end - padding; at < end; ) {
    const id = packet[at] >> 1;
    const long = packet[at++] & 1;
    if (id === 0) {
      if (long) continue;
      break; // the rest is padding
    }
    if (id === 1) {
      frame += long ? (packet[at++] ?? 0) : 1;
      continue;
    }
    if (id === 2) break; // "repeat these extensions": not for us
    if (id < 32) {
      at += long;
      continue;
    }
    let size = end - at;
    if (long) {
      size = 0;
      for (;;) {
        if (at >= end) return [];
        const b = packet[at++];
        size += b;
        if (b !== 255) break;
      }
    }
    if (size > end - at) break;
    if (id === EXTENSION >> 1 && frame === 0 && size >= 2 && packet[at] === EXPERIMENT[0] && packet[at + 1] === EXPERIMENT[1]) {
      const copies = [];
      for (let i = at + 2; i + 3 <= at + size; ) {
        const back = packet[i];
        const length = (packet[i + 1] << 8) | packet[i + 2];
        i += 3;
        if (i + length > at + size) break;
        if (back > 0 && length > 0) copies.push([back, packet.subarray(i, i + length)]);
        i += length;
      }
      return copies;
    }
    at += size;
  }
  return [];
}

// Runs in the audio thread: cuts the microphone into frames, and mixes the
// speakers through per-speaker jitter buffers.
const WORKLET = String.raw`
const RATE = 48000;
// A frame quieter than this (RMS, about -50 dBFS) can be dropped unheard.
const QUIET = 0.0032;

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

// The pitch period of a frame, in samples (2.5 to 15 ms): the lag at which
// its last 5 ms best match what came before. Every other sample will do.
function period(pcm) {
  const n = pcm.length;
  const w = Math.min(240, n >> 2);
  let best = 0;
  let lag = Math.min(720, n - w);
  for (let l = 120; l <= Math.min(720, n - w); l += 2) {
    let xy = 0;
    let yy = 0;
    for (let i = n - w; i < n; i += 2) {
      xy += pcm[i] * pcm[i - l];
      yy += pcm[i - l] * pcm[i - l];
    }
    const score = yy > 0 ? xy / Math.sqrt(yy) : 0;
    if (score > best) (best = score), (lag = l);
  }
  return lag;
}

// One speaker: frames by sequence number, played out after an adaptive delay.
class Speaker {
  constructor(frame, config) {
    this.frame = frame; // samples per frame, as this speaker sends them
    this.seconds = frame / RATE;
    this.config = config;
    this.frames = new Map();
    this.quiet = new Set(); // buffered frames with little sound in them
    this.copies = new Set(); // buffered frames that came as copies in a later packet
    this.skip = new Set(); // frames dropped to catch up: pass over them without a gap
    this.next = null; // the frame to play next, while playing
    this.current = null;
    this.position = 0;
    this.last = null; // the last frame played that was not made up
    this.lost = 0; // frames made up since
    this.offsets = []; // arrival time minus send time, recent frames
    this.target = config.min;
    this.gain = 1;
    this.energy = 0;
    this.samples = 0;
    this.stats = { late: 0, concealed: 0, dropped: 0, played: 0, recovered: 0, underruns: 0 };
  }
  // A frame, or with hold null a copy of one that came in a later packet.
  // Each comes once: the main thread passes on only the first copy.
  push(seq, pcm, hold) {
    if (this.next !== null && (seq < this.next || (seq === this.next && this.current))) {
      // Its time has passed (or it is being made up right now). A copy too
      // late to help is no news: the frame was concealed, and counted so.
      if (hold !== null) this.stats.late++;
      return;
    }
    this.frames.set(seq, pcm);
    if (hold === null) this.copies.add(seq);
    let energy = 0;
    for (const v of pcm) energy += v * v;
    if (Math.sqrt(energy / pcm.length) < QUIET) this.quiet.add(seq);
    // A copy came when the packet after it did: that says nothing of jitter.
    if (hold === null) return;
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
  // The made-up continuation of what was last heard, sample by sample: its
  // last pitch period over and over, fading out over two frames, as in
  // G.711 Appendix I. Speech is close to periodic over that time, so this
  // bridges a lost frame where silence would be heard as a break.
  synth() {
    const t = this.t++;
    const g = 1 - t / (2 * this.frame);
    return g > 0 ? g * this.last[this.last.length - this.period + (t % this.period)] : 0;
  }
  conceal() {
    const pcm = new Float32Array(this.frame);
    if (this.lost === 0) (this.period = period(this.last)), (this.t = 0);
    if (this.lost < 2) for (let i = 0; i < pcm.length; i++) pcm[i] = this.synth();
    this.lost++;
    return pcm;
  }
  mix(out, played) {
    if (this.next === null) {
      if (this.frames.size === 0 || this.buffered() < this.target) return;
      this.next = Math.min(...this.frames.keys());
    }
    let i = 0;
    while (i < out.length) {
      if (!this.current) {
        if (this.skip.delete(this.next)) {
          this.next++; // dropped to catch up: no gap
          continue;
        }
        const pcm = this.frames.get(this.next);
        const behind = this.buffered() > this.target + 2 * this.seconds;
        if (!pcm && behind && this.frames.size) {
          // A pause between words (the sender sent nothing) while we are
          // behind: shorten it, which nobody hears, rather than play it out.
          this.next = Math.min(...this.frames.keys());
          this.stats.shortened = (this.stats.shortened ?? 0) + 1;
          continue;
        }
        if (pcm) {
          this.frames.delete(this.next);
          this.quiet.delete(this.next);
          if (this.copies.delete(this.next)) this.stats.recovered++;
          if (this.lost && this.last) {
            // Back from a made-up stretch: cross-fade into the real thing
            // over 5 ms rather than jump.
            const n = Math.min(240, pcm.length);
            for (let k = 0; k < n; k++) pcm[k] = (pcm[k] * k + this.synth() * (n - k)) / n;
          }
          this.lost = 0;
          this.last = pcm;
          this.current = pcm;
          this.position = 0;
          this.stats.played++;
          played.push(this.next);
        } else if (this.frames.size === 0) {
          // Underrun: the talk spurt is over, or the network stalled.
          // Wait to rebuffer.
          this.next = null;
          this.lost = 0;
          this.stats.underruns++;
          return;
        } else {
          // A frame lost on the way, copies and all: make it up.
          this.current = this.conceal();
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
    // Behind after a stall: catch up rather than play late. Drop quiet
    // frames first, wherever they are; speech only when far behind.
    if (this.next !== null) for (const seq of this.skip) if (seq < this.next) this.skip.delete(seq);
    while (this.buffered() > this.target + 2 * this.seconds) {
      let victim = null;
      for (const seq of this.quiet) if (victim === null || seq < victim) victim = seq;
      if (victim === null) {
        if (this.buffered() <= this.target + 10 * this.seconds) break;
        victim = Math.min(...this.frames.keys());
      }
      this.frames.delete(victim);
      this.quiet.delete(victim);
      this.copies.delete(victim);
      this.skip.add(victim);
      this.stats.dropped++;
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
    if (currentTime - this.reported >= 0.04) {
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
 * - `bitrate`: Opus bits per second (default 32000).
 * - `redundancy`: how many earlier packets each one carries a copy of, so
 *   that the next packets bring back those lost (0, 1 or 2; default 2). One
 *   covers lone losses; two, bursts as well (on Wi-Fi, say), for about a
 *   fifth more bandwidth than one.
 * - `frameMs`: 10, 20 (default), 40 or 60. Longer frames send fewer packets
 *   and add latency.
 * - `threshold`: the level, in dBFS, above which the microphone counts as
 *   speech (default -50). `vad: false` sends all the time.
 * - `minDelay`, `maxDelay`: bounds of the jitter buffer in ms (30, 500). On a
 *   steady network it stays near the lower; the upper is how far it may grow
 *   on a bad one, trading delay for audio that does not break up.
 * - `stream`: a MediaStream to send instead of the microphone.
 * - `microphone: false`: start listening only, without asking for the
 *   microphone; `setMicrophone(true)` turns it on later.
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
    bitrate: options.bitrate ?? 32_000,
    // Complexity 10, the best Opus does, costs next to nothing more (0.19
    // ms to encode a frame against 0.16 at 5, on a laptop). "voip" favours
    // intelligibility, as WebRTC does for speech. Variable bitrate, the
    // default, spends bits where the sound needs them.
    opus: { frameDuration: frameMs * 1000, complexity: 10, application: "voip" },
  };
  const { supported } = await AudioEncoder.isConfigSupported(config);
  if (!supported) throw new Error("this browser cannot encode Opus");
  const voice = new Voice(conn, options, config, frameMs);
  await voice.start();
  return voice;
}

function microphone(deviceId) {
  return navigator.mediaDevices.getUserMedia({
    audio: {
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      channelCount: 1,
      ...(deviceId ? { deviceId: { exact: deviceId } } : {}),
    },
  });
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
  #stream = null;
  #ownsStream = false;
  #source = null;
  #capture;
  #playback;
  #output;
  #encoder;
  #decoders = new Map();
  #seq = 0;
  #active = 0; // frames of speech still to send (hangover)
  #micLevel = 0;
  #transmitting = false;
  #preroll = [];
  #sent = []; // the latest packets sent, [seq, packet], for copies in the next
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
    this.#capture = new AudioWorkletNode(context, "peerconnect-capture", {
      numberOfOutputs: 1,
      processorOptions: { frame: this.#frame },
    });
    // A capture node must be pulled by the graph to run: through silence.
    const silent = new GainNode(context, { gain: 0 });
    this.#capture.connect(silent).connect(context.destination);
    this.#capture.port.onmessage = ({ data }) => this.#captured(data);
    if (this.#options.stream) this.#attach(this.#options.stream, false);
    else if (this.#options.microphone !== false) this.#attach(await microphone(), true);

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

  /** Whether a microphone (or stream) is being captured. */
  get microphone() {
    return this.#stream !== null;
  }

  /**
   * Turns the microphone on (asking for it, from `deviceId` if given) or
   * off (releasing it: the browser stops showing it in use).
   */
  async setMicrophone(on, { deviceId } = {}) {
    if (!on) return this.#detach();
    this.#attach(await microphone(deviceId), true);
  }

  /** Sends `stream` instead of the microphone; `null` sends nothing. */
  setStream(stream) {
    if (stream) this.#attach(stream, false);
    else this.#detach();
  }

  #attach(stream, owned) {
    this.#detach();
    this.#stream = stream;
    this.#ownsStream = owned;
    this.#source = this.#context.createMediaStreamSource(stream);
    this.#source.connect(this.#capture);
  }

  #detach() {
    this.#source?.disconnect();
    this.#source = null;
    if (this.#ownsStream) for (const track of this.#stream?.getTracks() ?? []) track.stop();
    this.#stream = null;
    this.#ownsStream = false;
    this.#active = 0;
    this.#micLevel = 0;
    this.#transmitting = false;
    this.#setSpeaking(this.#conn.id, false);
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

  /** The microphone's level over the latest frame (RMS, 0–1), muted or not. */
  get micLevel() {
    return this.#micLevel;
  }

  /** Whether our voice is being sent right now: unmuted, and speaking (or `vad: false`). */
  get transmitting() {
    return this.#transmitting;
  }

  /** Who is speaking right now (our own ID included). */
  get speaking() {
    return new Set(this.#speaking);
  }

  /**
   * Per member: `rms` level, current `delay` and jitter buffer `target` (s),
   * frames `played` (`recovered` of them from a copy in a later packet),
   * `late`, `concealed` (lost) and `dropped` (to catch up).
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
    this.#detach();
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
    this.#micLevel = rms;
    this.#transmitting = this.#active > 0;
    this.#options.onFrame?.({ seq: seq & 0xffff, rms, sending: this.#transmitting });
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
    const packet = new Uint8Array(chunk.byteLength);
    chunk.copyTo(packet);
    if (what === "hold") {
      // Kept briefly, so that the start of the next word is not clipped.
      this.#preroll.push([seq, packet, at]);
      if (this.#preroll.length > 2) this.#preroll.shift();
      return;
    }
    const frames = what === "onset" ? [...this.#preroll.splice(0), [seq, packet, at]] : [[seq, packet, at]];
    const now = performance.now();
    const redundancy = Math.min(2, this.#options.redundancy ?? 2);
    for (const [s, p, captured] of frames) {
      // Copies of the packets just before, if they went out.
      const copies = [];
      for (const [earlier, copy] of this.#sent) {
        if (s - earlier >= 1 && s - earlier <= redundancy) copies.push([s - earlier, copy]);
      }
      copies.sort((x, y) => x[0] - y[0]);
      const packed = withCopies(p, copies);
      const data = new Uint8Array(1 + packed.length);
      data[0] = Math.min(255, Math.max(0, Math.round(now - captured)));
      data.set(packed, 1);
      this.#conn.sendVoice(s & 0xffff, data);
      this.#sent.push([s, p]);
      if (this.#sent.length > 2) this.#sent.shift();
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
      entry = { decoder, last: null, queue: [], seen: new Set() };
      this.#decoders.set(from, entry);
    }
    // Unwrap the 16-bit sequence number around the last one seen.
    const last = entry.last ?? seq;
    const unwrapped = last + ((((seq - (last & 0xffff)) & 0xffff) ^ 0x8000) - 0x8000);
    entry.last = Math.max(entry.last ?? unwrapped, unwrapped);
    for (const s of entry.seen) if (s < entry.last - 64) entry.seen.delete(s);
    // The copies first, oldest first, so that the decoder goes in order.
    // Whichever of a frame and its copies comes first is played; the
    // others are not even decoded.
    const packet = data.subarray(1);
    const copies = copiesIn(packet).sort((x, y) => y[0] - x[0]);
    for (const [back, copy] of copies) this.#decode(entry, unwrapped - back, copy, null);
    this.#decode(entry, unwrapped, packet, data[0] / 1000);
  }

  // `hold` is null for a copy.
  #decode(entry, seq, packet, hold) {
    if (entry.seen.has(seq)) return;
    entry.seen.add(seq);
    // Decoders may rewrite timestamps, but they output one frame per packet,
    // in order: match outputs to inputs by that.
    entry.queue.push([seq, hold]);
    entry.decoder.decode(new EncodedAudioChunk({ type: "key", timestamp: seq * 20_000, data: packet }));
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
