// Video in a real browser (headless Chromium, its fake camera, WebCodecs):
// two members in one page through the Rust relay over WebTransport, then
// through a path that loses UDP datagrams; then simulcast, three members,
// each receiver getting the layer that suits the size it shows the video at.
//
//   cargo build -p peerconnect-relay && node test/video.browser.mjs
//
// Uses Playwright from ../website and `openssl` (a certificate pinned by hash).

import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash, X509Certificate } from "node:crypto";
import { createSocket } from "node:dgram";
import { createReadStream, mkdtempSync, readFileSync } from "node:fs";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const require = createRequire(new URL("../../website/package.json", import.meta.url));
const { chromium } = require("playwright");

const dir = mkdtempSync(join(tmpdir(), "peerconnect-video-"));
const [cert, key] = [join(dir, "relay.crt"), join(dir, "relay.key")];
execFileSync("openssl", [
  "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-days", "10",
  "-subj", "/CN=localhost", "-addext", "subjectAltName=IP:127.0.0.1", "-keyout", key, "-out", cert,
], { stdio: "ignore" });
const hash = [...createHash("sha256").update(new X509Certificate(readFileSync(cert)).raw).digest()];

const relayProcess = spawn(join(root, "../target/debug/peerconnect-relay"), {
  env: { ...process.env, PEERCONNECT_LISTEN: "127.0.0.1:0", PEERCONNECT_TLS_CERT: cert, PEERCONNECT_TLS_KEY: key },
});
// WebTransport goes to the UDP port of the same number as the TCP one.
const port = await new Promise((resolve, reject) => {
  relayProcess.on("error", reject);
  relayProcess.on("exit", (code) => reject(new Error(`the relay exited (${code})`)));
  // Its stderr is unbuffered: the line may come in pieces.
  let log = "";
  relayProcess.stderr.on("data", (d) => {
    const m = /listening on 127\.0\.0\.1:(\d+) /.exec((log += d));
    if (m) resolve(Number(m[1]));
  });
});

// Seeded, so that runs see the same losses.
let state = 1;
const random = () => (state = (state * 48271) % 2147483647) / 2147483647;

// UDP, each datagram lost with `udp.loss` and delayed by up to 10 ms.
const udp = { loss: 0 };
const udpProxy = createSocket("udp4");
const upstreams = new Map();
udpProxy.on("message", (message, from) => {
  const k = `${from.address}:${from.port}`;
  let up = upstreams.get(k);
  if (!up) {
    up = createSocket("udp4");
    up.on("message", (reply) => {
      if (random() < udp.loss) return;
      setTimeout(() => udpProxy.send(reply, from.port, from.address), random() * 10);
    });
    upstreams.set(k, up);
  }
  if (random() < udp.loss) return;
  setTimeout(() => up.send(message, port, "127.0.0.1"), random() * 10);
});
await new Promise((r) => udpProxy.bind(0, "127.0.0.1", r));

const types = { ".html": "text/html", ".js": "text/javascript" };
const server = createServer((req, res) => {
  const path = join(root, new URL(req.url, "http://x").pathname);
  if (!path.startsWith(root)) return res.writeHead(403).end();
  res.setHeader("content-type", types[extname(path)] ?? "application/octet-stream");
  createReadStream(path)
    .on("error", () => res.writeHead(404).end())
    .pipe(res);
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));

const browser = await chromium.launch({ args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream"] });
let failed = false;
try {
  const page = await browser.newPage();
  page.on("pageerror", (e) => console.log("page error:", e));
  await page.goto(`http://127.0.0.1:${server.address().port}/test/browser-video/index.html`);
  await page.waitForFunction(() => window.ready);
  const run = (name, options) =>
    page.evaluate((o) => window.run(o), { seconds: 8, hash, ...options }).then((r) => {
      console.log(
        `${name}: ${r.received.decoded} of ${r.sent.sent} frames decoded (${r.sent.codec}), ` +
          `send to decode p50 ${r.p50?.toFixed(0)} ms, p99 ${r.p99?.toFixed(0)} ms, max ${r.max?.toFixed(0)}; ` +
          `${r.sent.keyframes} keyframes, ${r.sent.requests} requested; bitrate at the end ${r.sent.bitrate / 1000} kbit/s; ` +
          `decode errors ${r.received.errors}`,
      );
      return r;
    });

  // A camera held upright stays upright, and the picture turns on request.
  const shape = await page.evaluate((o) => window.shape(o), { relay: `https://127.0.0.1:${port}`, hash });
  console.log("tall picture:", JSON.stringify(shape));
  assert.ok(shape.upright.height > shape.upright.width, "sent tall");
  assert.equal(shape.upright.top, "red", "and upright");
  assert.ok(shape.turned.width > shape.turned.height, "turned by the sender: wide");
  assert.equal(shape.turned.left, "blue", "a quarter turn clockwise puts the top on the right");
  assert.ok(shape.viewed.width > shape.viewed.height, "turned by the receiver: wide");
  assert.equal(shape.viewed.left, "red", "three quarter turns put the top on the left");

  const wt = await run("video, WebTransport", { relay: `https://127.0.0.1:${port}` });
  udp.loss = 0.02;
  state = 1;
  const wtLossy = await run("video, WebTransport, 2% UDP loss each way", { relay: `https://127.0.0.1:${udpProxy.address().port}` });
  udp.loss = 0;

  // Simulcast: what each receiver shows decides what it gets, and what the sender encodes.
  const sim = await page.evaluate((o) => window.simulcast(o), { relay: `https://127.0.0.1:${port}`, hash, seconds: 6 });
  const layer = (s, n) => s.layers.find((l) => l.layer === n) ?? { encoded: 0, decoded: 0, received: 0, running: false };
  const decoded = (m) => m.layers.map((l) => `layer ${l.layer} ${l.decoded}`).join(", ");
  const latency = Object.entries(sim.latency)
    .map(([k, q]) => `${k[0]} layer ${k.slice(1)} p50 ${q.p50?.toFixed(0)} ms p99 ${q.p99?.toFixed(0)} ms (${q.count})`)
    .join("; ");
  const encoders = (s) => s.a.layers.map((l) => `${l.layer}: ${l.running ? "on" : "off"} ${l.encoded} (${l.width}×${l.height})`).join(", ");
  const demand = (s) => `0b${(s.a.demand ?? 0).toString(2)}`;
  console.log(`simulcast, both shown: demand ${demand(sim.both)}, encoders ${encoders(sim.both)}; b got layer ${sim.both.b.layer} (${decoded(sim.both.b)}), c got layer ${sim.both.c.layer} (${decoded(sim.both.c)})`);
  console.log(`simulcast, c detached: demand ${demand(sim.cGone)}, encoders ${encoders(sim.cGone)}; c received ${sim.cStopped.c.received} then ${sim.cGone.c.received}`);
  console.log(`simulcast, b small: demand ${demand(sim.bSmall)}, encoders ${encoders(sim.bSmall)}; b now layer ${sim.bSmall.b.layer} (${decoded(sim.bSmall.b)}), longest gap ${sim.gap.toFixed(0)} ms`);
  console.log(`simulcast latency: ${latency}`);

  if (!process.env.NO_ASSERT) {
    assert.deepEqual(sim.connected, [true, true, true], "simulcast over WebTransport");
    // Large for b, small for c: both layers, each to its receiver.
    assert.equal(sim.both.a.demand, 0b110, "layers 1 and 2 wanted");
    assert.ok(layer(sim.both.a, 1).running && layer(sim.both.a, 2).running, "both encoders run");
    assert.equal(layer(sim.both.a, 2).width, 1280, "layer 2 at 1280 × 720");
    assert.equal(layer(sim.both.a, 1).width, 640, "layer 1 at 640 × 360");
    assert.equal(sim.both.b.subscribed, 2);
    assert.equal(sim.both.c.subscribed, 1);
    assert.equal(sim.both.b.layer, 2, "b gets layer 2");
    assert.equal(sim.both.c.layer, 1, "c gets layer 1");
    assert.equal(sim.both.b.width, 1280, "b decodes 1280 wide");
    assert.equal(sim.both.c.width, 640, "c decodes 640 wide");
    assert.ok(layer(sim.both.b, 2).decoded > 60, `b decoded ${layer(sim.both.b, 2).decoded} of layer 2`);
    assert.ok(layer(sim.both.c, 1).decoded > 60, `c decoded ${layer(sim.both.c, 1).decoded} of layer 1`);
    assert.equal(layer(sim.both.c, 2).received, 0, "c never gets layer 2");
    // c detached: nothing more reaches it, and only b's layer 2 is encoded.
    assert.equal(sim.cGone.c.received, sim.cStopped.c.received, "nothing forwarded to c");
    assert.equal(sim.cGone.c.subscribed, null);
    assert.equal(sim.cGone.a.demand, 0b100, "only layer 2 wanted");
    assert.ok(!layer(sim.cGone.a, 1).running, "layer 1 encoder stopped");
    assert.equal(layer(sim.cGone.a, 1).encoded, layer(sim.cStopped.a, 1).encoded, "layer 1 encoder idle");
    // b small: on layer 1 without a gap, and layer 2 idle.
    assert.equal(sim.bSmall.b.layer, 1, "b moved to layer 1");
    assert.equal(sim.bSmall.b.width, 640, "b decodes 640 wide");
    assert.ok(layer(sim.bSmall.b, 1).decoded > layer(sim.bMoved.b, 1).decoded + 30, "b keeps decoding layer 1");
    assert.ok(sim.gap < 400, `longest gap while b moved: ${sim.gap} ms`);
    assert.equal(sim.bSmall.a.demand, 0b010, "only layer 1 wanted");
    assert.equal(layer(sim.bSmall.a, 2).encoded, layer(sim.bMoved.a, 2).encoded, "layer 2 encoder idle");
    assert.equal(sim.bSmall.b.errors, 0, "decode errors");
    for (const q of Object.values(sim.latency)) assert.ok(q.p99 < 150, `simulcast p99 ${q.p99} ms`);

    for (const r of [wt, wtLossy]) assert.deepEqual(r.connected, [true, true], "over WebTransport");
    assert.ok(wt.sent.sent > 100, `${wt.sent.sent} sent`);
    assert.ok(wt.received.decoded >= wt.sent.sent * 0.95, `${wt.received.decoded} of ${wt.sent.sent} decoded`);
    assert.equal(wt.received.errors, 0, "decode errors");
    assert.ok(wt.p99 < 100, `p99 ${wt.p99} ms`);
    // A lost fragment costs its frame (and a keyframe request), not the stream.
    assert.equal(wtLossy.received.errors, 0, "nothing undecodable is delivered");
    assert.ok(wtLossy.received.decoded >= wtLossy.sent.sent * 0.8, `lossy: ${wtLossy.received.decoded} of ${wtLossy.sent.sent}`);
  }
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  await browser.close();
  server.close();
  udpProxy.close();
  for (const up of upstreams.values()) up.close();
  relayProcess.kill();
}
process.exit(failed ? 1 : 0);
