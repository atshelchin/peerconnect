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
// over a clean path and one losing UDP packets. VOICE_REMOTE=https://relay.example.com
// (or a quic:// relay) measures only that, over a real relay (VOICE_MAX_DELAY
// sets the jitter buffer's ceiling in ms).

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

const root = fileURLToPath(new URL("..", import.meta.url));
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
// `network.stallUntil` is in the future.
const network = { loss: 0, jitter: 0, stallUntil: 0 };
const delay = () => Math.max(random() * network.jitter, network.stallUntil - Date.now());
const udpProxy = createSocket("udp4");
const upstreams = new Map();
udpProxy.on("message", (message, from) => {
  const key = `${from.address}:${from.port}`;
  let upstream = upstreams.get(key);
  if (!upstream) {
    upstream = createSocket("udp4");
    upstream.on("message", (reply) => {
      if (random() < network.loss) return;
      setTimeout(() => udpProxy.send(reply, from.port, from.address), delay());
    });
    upstreams.set(key, upstream);
  }
  if (random() < network.loss) return;
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
    const r = await p.evaluate((o) => window.run(o), { seconds: 12, minDelay: 30, vad: true, hash, ...options });
    const s = r.stats ?? {};
    console.log(
      `${name}: speech frames sent ${r.sender.soundSent}/${r.sender.sound} ` +
        `(${((100 * r.sender.soundSent) / Math.max(1, r.sender.sound)).toFixed(1)}%); ` +
        `sound sent but never played ${r.lostSound}/${r.soundSent}; played ${s.played}, ` +
        `concealed ${s.concealed}, dropped ${s.dropped}, late ${s.late}, underruns ${s.underruns}; ` +
        `latency p50 ${r.p50?.toFixed(0)} ms, p99 ${r.p99?.toFixed(0)} ms`,
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
  // A lost packet costs only its own frame, which the decoder conceals,
  // instead of holding up the rest.
  network.jitter = 20;
  network.loss = 0.02;
  reseed();
  const lossy = await quality("speech, 2% UDP loss each way", { relay: proxied, seconds: 25 });
  network.loss = 0;
  await speech.close();

  if (!process.env.NO_ASSERT) {
    for (const r of [direct, vad, jitter, stall, clean, lossy]) assert.deepEqual(r.connected, [true, true], "over WebTransport");
    assert.ok(direct.frames > 150 && direct.p50 < 40, "direct");
    assert.ok(vad.frames > 5 && vad.p50 < 80, "only speech");
    assert.ok(jitter.frames > 150 && jitter.p50 < 250 && jitter.stats.target < 0.25, "jitter");
    // After the stall the buffer catches up: latency at the end is back down.
    assert.ok(stall.tail < 150, `tail after a stall: ${stall.tail} ms`);
    // Speech goes out whole, and plays whole on a clean path.
    assert.ok(clean.sender.soundSent >= clean.sender.sound * 0.99, "speech sent on a clean path");
    assert.equal(clean.lostSound, 0, "speech lost on a clean path");
    // About 4% of frames lost on two lossy hops, but the rest neither late
    // nor held up behind them (it is about 100 ms; a retransmission stall
    // would add hundreds).
    assert.ok(lossy.lostSound <= lossy.soundSent * 0.08, `speech lost over UDP: ${lossy.lostSound}/${lossy.soundSent}`);
    assert.ok(lossy.stats.late <= lossy.stats.played * 0.01, `late over UDP: ${lossy.stats.late}`);
    assert.ok(lossy.p99 < 300, `p99 with UDP loss: ${lossy.p99} ms`);
  }
} finally {
  await browser.close();
  server.close();
  udpProxy.close();
  for (const upstream of upstreams.values()) upstream.close();
  relayProcess.kill();
}
