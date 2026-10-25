// Voice in a real browser (headless Chromium with its fake microphone):
// two members in one page, through a real relay, measuring the time from
// the end of a frame's capture to the start of its playout.
//
//   node test/voice.browser.mjs        (needs ../target/debug/peerconnect-relay
//                                       and Playwright's Chromium)
//
// It also plays speech-like audio through the fake microphone and checks
// how much of it goes out and plays, over a clean path, one with TCP
// stalls, and one losing UDP packets, over WebTransport (`openssl` makes
// the relay a certificate the browser pins by hash). VOICE_REMOTE=wss://relay.example.com measures only that, over a
// real relay (VOICE_MAX_DELAY sets the jitter buffer's ceiling in ms).

import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash, X509Certificate } from "node:crypto";
import { createSocket } from "node:dgram";
import { createReadStream, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { createServer } from "node:http";
import { createServer as createTcpServer, connect as tcpConnect } from "node:net";
import { createRequire } from "node:module";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const require = createRequire(new URL("../../website/package.json", import.meta.url));
const { chromium } = require("playwright");

// A certificate WebTransport accepts by its hash (ECDSA P-256, under 14
// days), so the relay serves WebTransport too; ws:// works as before.
const certDir = mkdtempSync(join(tmpdir(), "peerconnect-voice-cert-"));
const [certFile, keyFile] = [join(certDir, "relay.crt"), join(certDir, "relay.key")];
execFileSync("openssl", [
  "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-days", "10",
  "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1", "-keyout", keyFile, "-out", certFile,
], { stdio: "ignore" });
const certHash = [...createHash("sha256").update(new X509Certificate(readFileSync(certFile)).raw).digest()];
const relayProcess = spawn(join(root, "../target/debug/peerconnect-relay"), {
  env: { ...process.env, PEERCONNECT_LISTEN: "127.0.0.1:0", PEERCONNECT_TLS_CERT: certFile, PEERCONNECT_TLS_KEY: keyFile },
});
const relay = await new Promise((resolve, reject) => {
  relayProcess.on("error", reject);
  relayProcess.stderr.on("data", (data) => {
    const match = /listening on (\S+)/.exec(data);
    if (match) resolve(`ws://${match[1]}`);
  });
});

// A proxy in front of the relay that delays what it forwards like a jittery
// TCP path: each chunk by up to `network.jitter` ms more, never reordered,
// and holds everything back while `network.stallUntil` is in the future.
const network = { jitter: 0, stallUntil: 0, lossChance: 0 };
// Seeded, so that runs see the same stalls and can be compared.
let state = 1;
const random = () => ((state = (state * 48271) % 2147483647) / 2147483647);
const reseed = () => (state = 1);
const [relayHost, relayPort] = relay.slice("ws://".length).split(":");
const proxy = createTcpServer((client) => {
  const upstream = tcpConnect(Number(relayPort), relayHost);
  // One queue per direction, released strictly in order (timers alone could
  // reorder bytes, which TCP never does).
  const delayed = (from, to) => {
    const queue = [];
    let last = 0;
    let timer = null;
    const release = () => {
      timer = null;
      while (queue.length && queue[0].at <= Date.now()) to.write(queue.shift().chunk);
      if (queue.length) timer = setTimeout(release, queue[0].at - Date.now());
    };
    from.on("data", (chunk) => {
      // A lost segment on a TCP path: everything waits for its retransmission.
      const stall = random() < network.lossChance ? 100 + random() * 200 : 0;
      const at = Math.max(last, Date.now() + random() * network.jitter + stall, network.stallUntil);
      last = at;
      queue.push({ at, chunk });
      if (!timer) timer = setTimeout(release, at - Date.now());
    });
  };
  delayed(client, upstream);
  delayed(upstream, client);
  const end = () => (client.destroy(), upstream.destroy());
  client.on("error", end).on("close", end);
  upstream.on("error", end).on("close", end);
});
await new Promise((resolve) => proxy.listen(0, "127.0.0.1", resolve));
const proxied = `ws://127.0.0.1:${proxy.address().port}`;

// The same for UDP, for WebTransport: each datagram dropped with
// `udpNetwork.loss`, and delayed by up to `udpNetwork.jitter` ms (so they
// may also arrive out of order, as on a real path).
const udpNetwork = { loss: 0, jitter: 0 };
const udpProxy = createSocket("udp4");
const upstreams = new Map();
udpProxy.on("message", (message, from) => {
  const key = `${from.address}:${from.port}`;
  let upstream = upstreams.get(key);
  if (!upstream) {
    upstream = createSocket("udp4");
    upstream.on("message", (reply) => {
      if (random() < udpNetwork.loss) return;
      setTimeout(() => udpProxy.send(reply, from.port, from.address), random() * udpNetwork.jitter);
    });
    upstreams.set(key, upstream);
  }
  if (random() < udpNetwork.loss) return;
  setTimeout(() => upstream.send(message, Number(relayPort), relayHost), random() * udpNetwork.jitter);
});
await new Promise((resolve) => udpProxy.bind(0, "127.0.0.1", resolve));
const proxiedQuic = `https://127.0.0.1:${udpProxy.address().port}`;

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
  const run = (options) => page.evaluate((options) => window.run(options), { seconds: 5, minDelay: 30, ...options });
  const report = (name, r) =>
    console.log(
      `${name}: ${r.frames} frames, capture to playout p50 ${r.p50.toFixed(1)} ms, p90 ${r.p90.toFixed(1)}, ` +
        `p99 ${r.p99.toFixed(1)}; tail p50 ${r.tail?.toFixed(1)}; jitter buffer ${(r.stats.target * 1000).toFixed(0)} ms; ` +
        `concealed ${r.stats.concealed}, dropped ${r.stats.dropped}, late ${r.stats.late}; sent ${r.sent}, received ${r.received}`,
    );

  const direct = await run({ relay, vad: false });
  report("direct", direct);
  const vad = await run({ relay, vad: true });
  report("direct, sending only speech", vad);
  network.jitter = 60;
  const jitter = await run({ relay: proxied, vad: false });
  report("0-60 ms jitter each way", jitter);
  network.jitter = 10;
  setTimeout(() => (network.stallUntil = Date.now() + 1000), 2000);
  const stall = await run({ relay: proxied, vad: false, seconds: 8 });
  report("1 s stall, then 0-10 ms jitter", stall);

  // Speech: how much of it goes out, and how smoothly it plays, on a clean
  // path and on one where TCP stalls for a retransmission now and then.
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
    const r = await p.evaluate((o) => window.run(o), { seconds: 12, minDelay: 30, vad: true, ...options });
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
  // VOICE_REMOTE=wss://… measures speech over a real relay instead, and stops.
  if (process.env.VOICE_REMOTE) {
    const maxDelay = Number(process.env.VOICE_MAX_DELAY) || undefined;
    for (let i = 0; i < 2; i++) await quality(`speech via ${process.env.VOICE_REMOTE}`, { relay: process.env.VOICE_REMOTE, seconds: 25, maxDelay });
    await speech.close();
    await browser.close();
    server.close();
    proxy.close();
    udpProxy.close();
    relayProcess.kill();
    process.exit(0);
  }
  const clean = await quality("speech, clean path", { relay });
  network.jitter = 20;
  network.lossChance = 0.02; // about one stall every two seconds at this traffic
  reseed();
  const lossy = await quality("speech, TCP stalls", { relay: proxied, seconds: 25 });
  network.lossChance = 0;
  // The same loss on UDP, over WebTransport: a lost packet costs only its
  // own frame, which the decoder conceals, instead of holding up the rest.
  udpNetwork.jitter = 20;
  udpNetwork.loss = 0.02;
  reseed();
  const quic = await quality("speech, 2% UDP loss each way, WebTransport", {
    relay: proxiedQuic,
    seconds: 25,
    webTransport: { serverCertificateHashes: [{ algorithm: "sha-256", value: new Uint8Array(certHash) }] },
  });
  udpNetwork.loss = 0;
  await speech.close();

  if (!process.env.NO_ASSERT) {
    assert.ok(direct.frames > 150 && direct.p50 < 40, "direct");
    assert.ok(vad.frames > 5 && vad.p50 < 80, "only speech");
    assert.ok(jitter.frames > 150 && jitter.p50 < 250 && jitter.stats.target < 0.25, "jitter");
    // After the stall the buffer catches up: latency at the end is back down.
    assert.ok(stall.tail < 150, `tail after a stall: ${stall.tail} ms`);
    // Speech goes out whole, and plays whole: catching up after stalls
    // shortens pauses and drops quiet frames, not words.
    assert.ok(clean.sender.soundSent >= clean.sender.sound * 0.99, "speech sent on a clean path");
    assert.equal(clean.lostSound, 0, "speech lost on a clean path");
    assert.ok(lossy.lostSound <= lossy.soundSent * 0.02, `speech lost to stalls: ${lossy.lostSound}/${lossy.soundSent}`);
    assert.deepEqual(quic.via, ["webtransport", "webtransport"]);
    // About 4% of frames lost on two lossy hops, but none late.
    assert.ok(quic.lostSound <= quic.soundSent * 0.08, `speech lost over UDP: ${quic.lostSound}/${quic.soundSent}`);
    assert.ok(quic.p99 < lossy.p99 / 2, `p99 over WebTransport ${quic.p99} ms, over TCP ${lossy.p99} ms`);
  }
} finally {
  await browser.close();
  server.close();
  proxy.close();
  udpProxy.close();
  for (const upstream of upstreams.values()) upstream.close();
  relayProcess.kill();
}
