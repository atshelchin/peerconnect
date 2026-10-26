// Voice in a real browser (headless Chromium with its fake microphone):
// two members in one page, through a real relay over WebTransport (`openssl`
// makes the relay a certificate the browser pins by hash), measuring the
// time from the end of a frame's capture to the start of its playout.
//
//   node test/voice.browser.mjs        (needs ../target/debug/peerconnect-relay
//                                       and Playwright's Chromium)
//
// Through a proxy in front of the relay's UDP port, it measures the same
// over a jittery path and one that stalls, then plays speech-like audio
// through the fake microphone and checks how much of it goes out and plays,
// over a clean path and paths losing UDP packets, alone or in bursts.
// VOICE_REMOTE=https://relay.example.com (or a quic:// relay) measures only
// that, over a real relay (VOICE_MAX_DELAY sets the jitter buffer's ceiling
// in ms). VOICE_REDUNDANCY=0, 1 or 2 sets how many earlier packets each
// carries a copy of (0: none, as before there were copies).
//
// Before all that, it checks the copies' format and the jitter buffer here,
// without a browser.

import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash, X509Certificate } from "node:crypto";
import { createSocket } from "node:dgram";
import { createReadStream, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { copiesIn, withCopies } from "../voice.js";

const root = fileURLToPath(new URL("..", import.meta.url));

// The copies' format: found behind others' extensions, and nothing found
// in what is not one of ours.
{
  const packet = new Uint8Array([0x78, 1, 2, 3]);
  const copy = new Uint8Array([0x78, 9, 9]);
  const padded = withCopies(packet, [[1, copy]]);
  assert.deepEqual(copiesIn(padded), [[1, copy]]);
  assert.equal(withCopies(packet, []), packet, "no copies: the packet as it was");
  // An extension of someone else's (ID 33, two bytes long) ahead of ours.
  const ours = padded.subarray(padded.length - padded[2]);
  const other = new Uint8Array([0x7b, 0x41, ours.length + 4, 1, 2, 3, (33 << 1) | 1, 2, 0xaa, 0xbb, ...ours]);
  assert.deepEqual(copiesIn(other), [[1, copy]], "behind another extension");
  for (const junk of [[], [0x7b], [0x7b, 0x41], [0x7b, 0x41, 200, 1], [0x78, 0x41, 3, 0xf4, 0x50, 1], [0x7b, 0x41, 4, 0xf4, 0x50, 1, 1]]) {
    assert.deepEqual(copiesIn(new Uint8Array(junk)), [], `nothing in ${junk}`);
  }
}

// The jitter buffer, run here on the worklet's own code: a tone with
// frames lost (one alone, two in a row), one brought back by a copy, one
// late. Each frame plays at most once, the stats say what happened, and
// what stands in for lost frames joins on without a click, where silence
// would jump by the tone's whole amplitude.
{
  const source = readFileSync(join(root, "voice.js"), "utf8");
  const worklet = source.slice(source.indexOf("String.raw`") + 11, source.indexOf("registerProcessor(\"peerconnect-capture"));
  globalThis.currentTime = 0;
  const { Speaker } = new Function("AudioWorkletProcessor", `${worklet}; return { Speaker };`)(class {});
  const speaker = new Speaker(960, { min: 0.08, max: 0.5 });
  const tone = (seq) => Float32Array.from({ length: 960 }, (_, i) => 0.5 * Math.sin((2 * Math.PI * 210 * (seq * 960 + i)) / 48_000));
  const arrivals = new Map(); // block on which each frame arrives: [seq, hold]
  const at = (seq) => Math.ceil((seq * 960) / 128);
  for (let seq = 0; seq < 60; seq++) if (![10, 20, 21, 30, 40].includes(seq)) arrivals.set(at(seq), [[seq, 0]]);
  arrivals.get(at(31)).push([30, null]); // a copy, in the packet after
  arrivals.set(at(45) + 1, [[40, 0]]); // too late
  const out = [];
  const played = [];
  for (let block = 0; block < at(60) + 200; block++) {
    globalThis.currentTime = (block * 128) / 48_000;
    for (const [seq, hold] of arrivals.get(block) ?? []) speaker.push(seq, tone(seq), hold);
    const buffer = new Float32Array(128);
    speaker.mix(buffer, played);
    out.push(...buffer);
  }
  delete globalThis.currentTime;
  assert.equal(new Set(played).size, played.length, "no frame played twice");
  assert.deepEqual(played.filter((s) => [10, 20, 21, 30, 40].includes(s)), [30], "the copy plays, the late frame not");
  const { concealed, recovered, late } = speaker.stats;
  assert.deepEqual({ concealed, recovered, late }, { concealed: 4, recovered: 1, late: 1 });
  let jump = 0;
  for (let i = 1; i < out.length; i++) jump = Math.max(jump, Math.abs(out[i] - out[i - 1]));
  assert.ok(jump < 0.03, `the biggest step between samples: ${jump.toFixed(3)}`);
  // From the first sound to the last, no stretch of silence (a frame is 960 samples).
  const first = out.findIndex((v) => Math.abs(v) > 0.01);
  const last = out.findLastIndex((v) => Math.abs(v) > 0.01);
  let quiet = 0;
  for (let i = first, run = 0; i <= last; i++) quiet = Math.max(quiet, (run = Math.abs(out[i]) < 0.01 ? run + 1 : 0));
  assert.ok(quiet < 200, `silent for ${quiet} samples`);
}
const require = createRequire(new URL("../../website/package.json", import.meta.url));
const { chromium } = require("playwright");

if (process.env.VOICE_REMOTE && !/^(https|quic):\/\//.test(process.env.VOICE_REMOTE)) {
  throw new Error("VOICE_REMOTE is an https:// or quic:// relay: groups travel over WebTransport");
}

// A certificate WebTransport accepts by its hash (ECDSA P-256, under 14 days).
const certDir = mkdtempSync(join(tmpdir(), "peerconnect-voice-cert-"));
const [certFile, keyFile] = [join(certDir, "relay.crt"), join(certDir, "relay.key")];
execFileSync("openssl", [
  "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-days", "10",
  "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1", "-keyout", keyFile, "-out", certFile,
], { stdio: "ignore" });
const hash = [...createHash("sha256").update(new X509Certificate(readFileSync(certFile)).raw).digest()];
const relayProcess = spawn(join(root, "../target/debug/peerconnect-relay"), {
  env: { ...process.env, PEERCONNECT_LISTEN: "127.0.0.1:0", PEERCONNECT_TLS_CERT: certFile, PEERCONNECT_TLS_KEY: keyFile },
});
// WebTransport goes to the UDP port of the same number as the TCP one.
const relayPort = await new Promise((resolve, reject) => {
  relayProcess.on("error", reject);
  relayProcess.on("exit", (code) => reject(new Error(`the relay exited (${code})`)));
  // Its stderr is unbuffered: the line may come in pieces.
  let log = "";
  relayProcess.stderr.on("data", (data) => {
    const match = /listening on 127\.0\.0\.1:(\d+) /.exec((log += data));
    if (match) resolve(Number(match[1]));
  });
});
const relay = `https://127.0.0.1:${relayPort}`;

// Seeded, so that runs see the same losses and can be compared.
let state = 1;
const random = () => ((state = (state * 48271) % 2147483647) / 2147483647);
const reseed = () => (state = 1);

// A proxy in front of the relay's UDP port: each datagram dropped with
// `network.loss`, delayed by up to `network.jitter` ms (so they may also
// arrive out of order, as on a real path), and all held back while
// `network.stallUntil` is in the future. With `network.burst`, losses come
// in bursts: that is the chance that a datagram lost is followed by another
// on the same path (mean burst 1 / (1 - burst)), the average staying `loss`.
const network = { loss: 0, burst: 0, jitter: 0, stallUntil: 0, up: 0 }; // `up`: bytes towards the relay
const delay = () => Math.max(random() * network.jitter, network.stallUntil - Date.now());
const drop = (path) => {
  const { loss, burst } = network;
  const p = !burst ? loss : path.lost ? burst : (loss * (1 - burst)) / (1 - loss);
  return (path.lost = random() < p);
};
const udpProxy = createSocket("udp4");
const upstreams = new Map();
udpProxy.on("message", (message, from) => {
  network.up += message.length + 28; // and the IPv4 and UDP headers
  const key = `${from.address}:${from.port}`;
  let upstream = upstreams.get(key);
  if (!upstream) {
    upstream = createSocket("udp4");
    upstream.paths = { up: { lost: false }, down: { lost: false } };
    upstream.on("message", (reply) => {
      if (drop(upstream.paths.down)) return;
      setTimeout(() => udpProxy.send(reply, from.port, from.address), delay());
    });
    upstreams.set(key, upstream);
  }
  if (drop(upstream.paths.up)) return;
  setTimeout(() => upstream.send(message, relayPort, "127.0.0.1"), delay());
});
await new Promise((resolve) => udpProxy.bind(0, "127.0.0.1", resolve));
const proxied = `https://127.0.0.1:${udpProxy.address().port}`;

const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript" };
const server = createServer((req, res) => {
  const path = join(root, new URL(req.url, "http://x").pathname);
  if (!path.startsWith(root)) return res.writeHead(403).end();
  res.setHeader("content-type", types[extname(path)] ?? "application/octet-stream");
  createReadStream(path)
    .on("error", () => res.writeHead(404).end())
    .pipe(res);
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const page_url = `http://127.0.0.1:${server.address().port}/test/browser-voice/index.html`;

// Speech-like audio for the fake microphone: syllables at levels from loud
// to barely audible, with pauses, over faint background noise.
function speechWav() {
  const rate = 48_000;
  const samples = new Float32Array(rate * 12);
  let seed = 7;
  const random = () => ((seed = (seed * 1103515245 + 12345) % 2 ** 31) / 2 ** 31);
  let t = 0.3;
  while (t < 11.5) {
    const words = 2 + Math.floor(random() * 5);
    for (let w = 0; w < words && t < 11.5; w++) {
      const length = 0.15 + random() * 0.25;
      const db = [-18, -24, -30, -36, -42, -48][Math.floor(random() * 6)];
      const amplitude = 10 ** (db / 20) * Math.SQRT2;
      const f0 = 110 + random() * 110;
      for (let i = 0; i < length * rate; i++) {
        const x = i / rate;
        const envelope = Math.min(1, x / 0.02, (length - x) / 0.04);
        let v = 0;
        for (let h = 1; h <= 6; h++) v += Math.sin(2 * Math.PI * f0 * h * x * (1 + 0.01 * Math.sin(6 * x))) / h;
        samples[Math.floor((t + x) * rate)] += (amplitude * envelope * v) / 1.6;
      }
      t += length + 0.05 + random() * 0.2;
    }
    t += 0.4 + random() * 0.8; // between sentences
  }
  for (let i = 0; i < samples.length; i++) samples[i] += (random() - 0.5) * 2 * 10 ** (-66 / 20);
  const data = Buffer.alloc(44 + samples.length * 2);
  data.write("RIFF", 0);
  data.writeUInt32LE(36 + samples.length * 2, 4);
  data.write("WAVEfmt ", 8);
  data.writeUInt32LE(16, 16);
  data.writeUInt16LE(1, 20); // PCM
  data.writeUInt16LE(1, 22); // mono
  data.writeUInt32LE(rate, 24);
  data.writeUInt32LE(rate * 2, 28);
  data.writeUInt16LE(2, 32);
  data.writeUInt16LE(16, 34);
  data.write("data", 36);
  data.writeUInt32LE(samples.length * 2, 40);
  samples.forEach((v, i) => data.writeInt16LE(Math.round(Math.max(-1, Math.min(1, v)) * 32767), 44 + i * 2));
  const path = join(mkdtempSync(join(tmpdir(), "peerconnect-voice-")), "speech.wav");
  writeFileSync(path, data);
  return path;
}

const browser = await chromium.launch({
  args: [
    "--use-fake-ui-for-media-stream",
    "--use-fake-device-for-media-stream",
    "--autoplay-policy=no-user-gesture-required",
  ],
});
try {
  const page = await browser.newPage();
  page.on("console", (m) => console.log("page:", m.text()));
  page.on("pageerror", (e) => console.log("page error:", e));
  await page.goto(page_url);
  await page.waitForFunction(() => window.ready);
  const run = (options) => page.evaluate((options) => window.run(options), { seconds: 5, minDelay: 30, hash, ...options });
  const report = (name, r) =>
    console.log(
      `${name}: ${r.frames} frames, capture to playout p50 ${r.p50.toFixed(1)} ms, p90 ${r.p90.toFixed(1)}, ` +
        `p99 ${r.p99.toFixed(1)}; tail p50 ${r.tail?.toFixed(1)}; jitter buffer ${(r.stats.target * 1000).toFixed(0)} ms; ` +
        `concealed ${r.stats.concealed}, dropped ${r.stats.dropped}, late ${r.stats.late}; sent ${r.sent}, received ${r.received}`,
    );

  // Copies in the Opus padding leave the packet as it was to any decoder.
  const padding = await page.evaluate(() => window.padding());
  console.log("copies in the padding:", JSON.stringify(padding));
  assert.ok(padding.identical && padding.samples >= 48_000 * 0.9, "a padded packet decodes to the same samples");
  assert.ok(padding.copies, "copies come back out of the padding");

  // The microphone only when asked for, and let go when turned off.
  const mic = await page.evaluate((o) => window.microphone(o), { relay, hash });
  console.log("microphone:", JSON.stringify(mic));
  assert.deepEqual(mic.before, { asked: 0, sent: 0, on: false }, "listening only: no microphone asked for");
  assert.ok(mic.during.asked === 1 && mic.during.sent > 10 && mic.during.on, "on when turned on");
  assert.ok(mic.after.sentSinceOff <= 1 && !mic.after.on && mic.after.ended === "ended", "released when turned off");

  const direct = await run({ relay, vad: false });
  report("direct", direct);
  const vad = await run({ relay, vad: true });
  report("direct, sending only speech", vad);
  network.jitter = 60;
  reseed();
  const jitter = await run({ relay: proxied, vad: false });
  report("0-60 ms jitter each way", jitter);
  network.jitter = 10;
  setTimeout(() => (network.stallUntil = Date.now() + 1000), 2000);
  const stall = await run({ relay: proxied, vad: false, seconds: 8 });
  report("1 s stall, then 0-10 ms jitter", stall);

  // Speech: how much of it goes out, and how smoothly it plays, on a clean
  // path and on one that loses packets.
  const speech = await chromium.launch({
    args: [
      "--use-fake-ui-for-media-stream",
      "--use-fake-device-for-media-stream",
      `--use-file-for-fake-audio-capture=${speechWav()}`,
      "--autoplay-policy=no-user-gesture-required",
    ],
  });
  const quality = async (name, options) => {
    const p = await speech.newPage();
    await p.goto(page_url);
    await p.waitForFunction(() => window.ready);
    const redundancy = process.env.VOICE_REDUNDANCY ? Number(process.env.VOICE_REDUNDANCY) : undefined;
    const r = await p.evaluate((o) => window.run(o), { seconds: 12, minDelay: 30, vad: true, hash, redundancy, ...options });
    const s = r.stats ?? {};
    console.log(
      `${name}: speech frames sent ${r.sender.soundSent}/${r.sender.sound} ` +
        `(${((100 * r.sender.soundSent) / Math.max(1, r.sender.sound)).toFixed(1)}%); ` +
        `sound sent but never played ${r.lostSound}/${r.soundSent} ` +
        `(${((100 * r.lostSound) / Math.max(1, r.soundSent)).toFixed(2)}%); played ${s.played}, ` +
        `recovered ${s.recovered ?? 0}, concealed ${s.concealed}, dropped ${s.dropped}, late ${s.late}, ` +
        `underruns ${s.underruns}; latency p50 ${r.p50?.toFixed(0)} ms, p99 ${r.p99?.toFixed(0)} ms; ` +
        `${(r.bytes / Math.max(1, r.sent)).toFixed(0)} bytes a frame`,
    );
    await p.close();
    return r;
  };
  network.jitter = 0;
  network.stallUntil = 0;
  // VOICE_REMOTE=https://… or quic://… measures speech over a real relay instead, and stops.
  if (process.env.VOICE_REMOTE) {
    const maxDelay = Number(process.env.VOICE_MAX_DELAY) || undefined;
    // Its own certificate: a real one, or for quic:// the weekly one, pinned by the SDK.
    for (let i = 0; i < 2; i++) {
      const relay = process.env.VOICE_REMOTE;
      await quality(`speech via ${relay}`, { relay, hash: undefined, seconds: 25, maxDelay });
    }
    await speech.close();
    await browser.close();
    server.close();
    udpProxy.close();
    relayProcess.kill();
    process.exit(0);
  }
  const clean = await quality("speech, clean path", { relay });
  // A lost packet costs at most its own frame instead of holding up the
  // rest, and the next packet's copy of it usually brings it back.
  const lossy = async (name, loss, burst = 0) => {
    Object.assign(network, { jitter: 20, loss, burst });
    reseed();
    network.up = 0;
    const r = await quality(name, { relay: proxied, seconds: 25 });
    // Both members speak (the same audio), and the run lasts about 25 s.
    console.log(`  ${((network.up * 8) / 2 / 25 / 1000).toFixed(0)} kbit/s a member towards the relay, IP headers included`);
    Object.assign(network, { jitter: 0, loss: 0, burst: 0 });
    return r;
  };
  const lossy2 = await lossy("speech, 2% UDP loss each way", 0.02);
  const lossy5 = await lossy("speech, 5% UDP loss each way", 0.05);
  const bursty = await lossy("speech, 5% UDP loss each way in bursts (mean 2)", 0.05, 0.5);
  await speech.close();

  if (!process.env.NO_ASSERT) {
    for (const r of [direct, vad, jitter, stall, clean, lossy2, lossy5, bursty]) assert.deepEqual(r.connected, [true, true], "over WebTransport");
    assert.ok(direct.frames > 150 && direct.p50 < 40, "direct");
    assert.ok(vad.frames > 5 && vad.p50 < 80, "only speech");
    assert.ok(jitter.frames > 150 && jitter.p50 < 250 && jitter.stats.target < 0.25, "jitter");
    // After the stall the buffer catches up: latency at the end is back down.
    assert.ok(stall.tail < 150, `tail after a stall: ${stall.tail} ms`);
    // Speech goes out whole, and plays whole on a clean path.
    assert.ok(clean.sender.soundSent >= clean.sender.sound * 0.99, "speech sent on a clean path");
    assert.equal(clean.lostSound, 0, "speech lost on a clean path");
    // About 4% of packets lost over two hops at 2% each, 10% at 5%, but
    // nearly all brought back by the copies in the packets after (without
    // them, 4-6%, 10-11% and 10-14% of speech never played), and the rest
    // neither late nor held up behind them (it is about 100 ms; a
    // retransmission stall would add hundreds).
    for (const [r, most] of [[lossy2, 0.01], [lossy5, 0.02], [bursty, 0.05]]) {
      assert.ok(r.lostSound <= r.soundSent * most, `speech lost over UDP: ${r.lostSound}/${r.soundSent}`);
      assert.ok(r.stats.recovered > 0, "frames recovered from the copies");
      assert.ok(r.stats.late <= r.stats.played * 0.01, `late over UDP: ${r.stats.late}`);
      assert.ok(r.p99 < 300, `p99 with UDP loss: ${r.p99} ms`);
    }
  }
} finally {
  await browser.close();
  server.close();
  udpProxy.close();
  for (const upstream of upstreams.values()) upstream.close();
  relayProcess.kill();
}
