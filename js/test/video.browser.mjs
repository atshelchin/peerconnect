// Video in a real browser (headless Chromium, its fake camera, WebCodecs):
// two members in one page through the Rust relay over WebTransport, then
// through a path that loses UDP datagrams; then simulcast, three members,
// each receiver getting the layer that suits the size it shows the video at;
// then a receiver behind a slow downlink, which the relay moves to a smaller
// layer and back, and a sender behind a slow uplink, which stops its largest
// layer and starts it again; then two tracks from one member (a camera and
// a screen), each receiver getting the layer of each that suits it; and
// 1080p, with keyframes far over the 64 KiB a member's video once had.
//
//   cargo build -p peerconnect-relay && node test/video.browser.mjs
//
// ONLY=older,tracks,fullhd runs some of it (older: all but the last two);
// PEERCONNECT_CHROME=<path> runs another Chromium (Chrome, say, which may
// have hardware encoders). Uses Playwright from ../website and `openssl`
// (a certificate pinned by hash).

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

// UDP, each datagram lost with `udp.loss` and delayed by up to 10 ms; or,
// with `udp.up` or `udp.down` (bits per second, towards the relay and back),
// through a link of that rate with a buffer of `BUFFERED` ms, beyond which
// datagrams are dropped, as a slow link's router would.
const udp = { loss: 0, up: 0, down: 0 };
const BUFFERED = 500;
const udpProxy = createSocket("udp4");
const upstreams = new Map();
/** Delivers datagrams as the proxy's settings say, `rate()` being this direction's. */
function pipe(rate, deliver) {
  let free = 0; // when the link is next free to start a datagram
  return (datagram) => {
    if (random() < udp.loss) return;
    const bps = rate();
    if (!bps) return setTimeout(() => deliver(datagram), random() * 10);
    const now = performance.now();
    const at = Math.max(now, free);
    if (at - now > BUFFERED) return;
    free = at + (datagram.length * 8 * 1000) / bps;
    setTimeout(() => deliver(datagram), free - now);
  };
}
udpProxy.on("message", (message, from) => {
  const k = `${from.address}:${from.port}`;
  let up = upstreams.get(k);
  if (!up) {
    const socket = createSocket("udp4");
    const back = pipe(() => udp.down, (reply) => udpProxy.send(reply, from.port, from.address));
    socket.on("message", back);
    up = { socket, send: pipe(() => udp.up, (datagram) => socket.send(datagram, port, "127.0.0.1")) };
    upstreams.set(k, up);
  }
  up.send(message);
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

const browser = await chromium.launch({
  args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream"],
  executablePath: process.env.PEERCONNECT_CHROME || undefined,
});
const only = process.env.ONLY?.split(",");
const want = (section) => !only || only.includes(section);
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

  const relay = `https://127.0.0.1:${port}`;
  console.log("default codecs by layer:", JSON.stringify(await page.evaluate((o) => window.codecs(o), { relay, hash })));
  const layer = (s, n) => s.layers.find((l) => l.layer === n) ?? { encoded: 0, decoded: 0, received: 0, running: false };
  /** Whether this browser encodes 1080p H.264 (High 4.0), in hardware or not. */
  const avc = await page.evaluate(() =>
    VideoEncoder.isConfigSupported({ codec: "avc1.640028", width: 1920, height: 1080, bitrate: 3_500_000, avc: { format: "annexb" } }).then((r) => r.supported),
  );

  if (want("older")) {
    // A camera held upright stays upright, and the picture turns on request.
    const shape = await page.evaluate((o) => window.shape(o), { relay: `https://127.0.0.1:${port}`, hash });
    console.log("tall picture:", JSON.stringify(shape));
    assert.ok(shape.upright.height > shape.upright.width, "sent tall");
    assert.equal(shape.upright.top, "red", "and upright");
    assert.ok(shape.turned.width > shape.turned.height, "turned by the sender: wide");
    assert.equal(shape.turned.left, "blue", "a quarter turn clockwise puts the top on the right");
    assert.ok(shape.viewed.width > shape.viewed.height, "turned by the receiver: wide");
    assert.equal(shape.viewed.left, "red", "three quarter turns put the top on the left");

    const wt = await run("video, WebTransport", { relay });
    udp.loss = 0.02;
    state = 1;
    const wtLossy = await run("video, WebTransport, 2% UDP loss each way", { relay: `https://127.0.0.1:${udpProxy.address().port}` });
    udp.loss = 0;

    // Simulcast: what each receiver shows decides what it gets, and what the sender encodes.
    const sim = await page.evaluate((o) => window.simulcast(o), { relay, hash, seconds: 6 });
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

    // Adapting to slow links: the proxy's rate, limited and lifted while the page runs.
    const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
    const live = (fn, arg) => page.evaluate(fn, arg);
    /** Polls the page until `fn` (run there, on `window.live`) is true, for up to `ms`; false if it never was. */
    const until = async (fn, arg, ms) => {
      for (const end = Date.now() + ms; Date.now() < end; await sleep(100)) {
        if (await live(fn, arg)) return true;
      }
      return false;
    };
    const adapt = async (slow, steps) => {
      const done = page.evaluate((o) => window.adaptation(o), {
        relay: `https://127.0.0.1:${port}`,
        proxied: `https://127.0.0.1:${udpProxy.address().port}`,
        hash,
        slow,
      });
      await page.waitForFunction(() => window.live);
      try {
        await steps();
      } finally {
        udp.up = udp.down = 0;
        await page.evaluate(() => window.finish());
      }
      return done;
    };
    const mark = (label) => live((l) => window.mark(l), label);
    const markAt = (r, label) => r.marks.find((m) => m.label === label)?.t;
    /** The decoded frames of `who` from `from` to `to` ms: the layers among them, and the longest gap. */
    const frames = (r, who, from, to = Infinity) => {
      const list = r.frames[who].filter((f) => f.t >= from && f.t < to);
      let gap = 0;
      for (let i = 1; i < list.length; i++) gap = Math.max(gap, list[i].t - list[i - 1].t);
      return { count: list.length, layers: [...new Set(list.map((f) => f.layer))], gap, first: (layer) => list.find((f) => f.layer === layer)?.t };
    };

    const decodedSince = (who, layer, label) => until(([w, l, m]) => window.decodedSince(w, l, m), [who, layer, label], 30_000);

    // A receiver behind a slow downlink: the relay moves it to layer 1, c keeps layer 2.
    let downOk = {};
    const down = await adapt("b", async () => {
      await sleep(4000);
      udp.down = 1_000_000;
      await mark("limited");
      downOk.moved = await until(() => window.live.limits.b.some((e) => e.layer === 1) && window.decodedSince("b", 1, "limited"), undefined, 15_000);
      await mark("moved");
      await sleep(3000);
      udp.down = 0;
      await mark("lifted");
      downOk.back = await decodedSince("b", 2, "lifted");
      await sleep(1000);
    });
    {
      const [limited, lifted] = [markAt(down, "limited"), markAt(down, "lifted")];
      const toOne = down.limits.b.find((e) => e.t > limited && e.layer === 1)?.t;
      const firstOne = frames(down, "b", limited).first(1);
      const backTwo = frames(down, "b", lifted).first(2);
      const backLimit = down.limits.b.find((e) => e.t > lifted && e.layer === 2)?.t;
      const cDuring = frames(down, "c", limited);
      console.log(
        `slow downlink (1 Mbit/s): b videolimit ${JSON.stringify(down.limits.b)}; ` +
          `limit after ${toOne - limited} ms, b's first layer 1 frame after ${firstOne - limited} ms; ` +
          `lifted: limit 2 after ${backLimit - lifted} ms, b's first layer 2 frame after ${backTwo - lifted} ms; ` +
          `c: layers ${cDuring.layers} (${cDuring.count} frames, longest gap ${cDuring.gap} ms), videolimit ${JSON.stringify(down.limits.c)}`,
      );
      downOk = { ...downOk, toOne, firstOne, backTwo, cDuring, limited, lifted };
    }

    // A sender behind a slow uplink: it stops layer 2, and b, which asked for it, gets layer 1.
    let upOk = {};
    const capped = () => window.live.uplink.at(-1)?.cappedAt === 1;
    const up = await adapt("a", async () => {
      await sleep(4000);
      udp.up = 800_000;
      await mark("limited");
      upOk.capped = await until(capped, undefined, 15_000);
      upOk.moved = await decodedSince("b", 1, "limited");
      await sleep(3000);
      udp.up = 0;
      await mark("lifted");
      upOk.back = await decodedSince("b", 2, "lifted");
      await sleep(1000);
    });
    {
      const [limited, lifted] = [markAt(up, "limited"), markAt(up, "lifted")];
      const cap = up.uplink.find((u) => u.t > limited && u.cappedAt === 1)?.t;
      const restart = up.uplink.find((u) => u.t > lifted && u.cappedAt === null)?.t;
      const firstOne = frames(up, "b", cap ?? limited).first(1);
      const backTwo = frames(up, "b", lifted).first(2);
      const bMove = frames(up, "b", (cap ?? limited) - 500, (cap ?? limited) + 4000);
      console.log(
        `slow uplink (800 kbit/s): a's uplink ${JSON.stringify(up.uplink)}; ` +
          `capped after ${cap - limited} ms, b's first layer 1 frame ${firstOne - cap} ms after that, longest gap in b's frames around it ${bMove.gap} ms; ` +
          `lifted: layer 2 again after ${restart - lifted} ms, b's first layer 2 frame after ${backTwo - lifted} ms; ` +
          `a's videolimit ${JSON.stringify(up.limits.a)}, b's ${JSON.stringify(up.limits.b)}`,
      );
      upOk = { ...upOk, cap, restart, firstOne, backTwo, bMove, limited, lifted };
    }

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
      // c never gets layer 2, but for a frame or two at the start: b's subscription may reach
      // the relay first, and until a's layer 1 runs the relay forwards c the lowest a sends.
      assert.ok(layer(sim.both.c, 2).received <= 3, `c got ${layer(sim.both.c, 2).received} frames of layer 2`);
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

      // The slow downlink: b moves to layer 1 and back to 2, c is untouched.
      assert.deepEqual(down.connected, [true, true, true], "slow downlink: still connected");
      assert.ok(downOk.moved, "b was limited to layer 1 and got it");
      assert.ok(downOk.firstOne - downOk.limited < 10_000, "b on layer 1 within 10 s");
      assert.ok(downOk.back, "b back on layer 2 after the limit was lifted");
      assert.ok(downOk.backTwo - downOk.lifted < 15_000, `b on layer 2 again ${downOk.backTwo - downOk.lifted} ms after`);
      assert.deepEqual(downOk.cDuring.layers, [2], "c keeps layer 2");
      assert.ok(downOk.cDuring.gap < 1000, `c's longest gap ${downOk.cDuring.gap} ms`);
      assert.deepEqual(down.limits.c, [], "c is never limited");
      // The slow uplink: a stops layer 2 and starts it again; b follows.
      assert.deepEqual(up.connected, [true, true, true], "slow uplink: still connected");
      assert.ok(upOk.capped, "a capped its uplink at layer 1");
      assert.ok(upOk.moved, "b got layer 1 once a stopped layer 2");
      assert.ok(upOk.firstOne - upOk.cap < 3000, `b on layer 1 ${upOk.firstOne - upOk.cap} ms after the cap`);
      // The relay moves b a second after layer 2's last frames reach it, late through the
      // full buffer; those may be lost too, and b asks for a keyframe if ours came too early.
      assert.ok(upOk.bMove.gap < 2500, `b's longest gap while it moved: ${upOk.bMove.gap} ms`);
      assert.ok(upOk.back, "b back on layer 2 after the limit was lifted");
      assert.ok(upOk.restart - upOk.lifted < 15_000, `a restarted layer 2 ${upOk.restart - upOk.lifted} ms after`);
      assert.equal(up.stats.a.uplink.cappedAt, null, "a's uplink uncapped at the end");

      for (const r of [wt, wtLossy]) assert.deepEqual(r.connected, [true, true], "over WebTransport");
      assert.ok(wt.sent.sent > 100, `${wt.sent.sent} sent`);
      assert.ok(wt.received.decoded >= wt.sent.sent * 0.95, `${wt.received.decoded} of ${wt.sent.sent} decoded`);
      assert.equal(wt.received.errors, 0, "decode errors");
      assert.ok(wt.p99 < 100, `p99 ${wt.p99} ms`);
      // A lost fragment costs its frame (and a keyframe request), not the stream.
      assert.equal(wtLossy.received.errors, 0, "nothing undecodable is delivered");
      assert.ok(wtLossy.received.decoded >= wtLossy.sent.sent * 0.8, `lossy: ${wtLossy.received.decoded} of ${wtLossy.sent.sent}`);
    }
  }

  if (want("tracks")) {
    // Two tracks from one member: each receiver gets, of each, the layer that suits its
    // canvas. The screen's layer 1 in H.264 where this browser encodes it (the default
    // picks H.264 only in hardware, which headless Chromium has none of), the rest in
    // VP8, so that b also moves from one codec to another.
    const screenCodec = avc ? "avc1.640028" : undefined;
    const tr = await page.evaluate((o) => window.tracks(o), { relay, hash, seconds: 6, screenCodec });
    const of = (m) => m.layers.map((l) => `${l.layer}: ${l.received}/${l.decoded}`).join(", ");
    const encoders = (s) =>
      s.layers
        .map((l) => `${l.layer} ${l.running ? "on" : "off"} ${l.codec}${l.hardware ? " (hw)" : ""} ${l.encoded} encoded, ${l.skipped} skipped (${l.width}×${l.height})`)
        .join(", ");
    const { both, bSmall } = tr;
    console.log(
      `two tracks: demand camera 0b${both.demand[0]?.toString(2)}, screen 0b${both.demand[1]?.toString(2)}; ` +
        `camera encoders ${encoders(both.camera)}; screen encoders ${encoders(both.screen)}; ` +
        `b: screen subscribed ${both.b.screen.subscribed}, received/decoded by layer ${of(both.b.screen)} (${both.b.screen.width}×${both.b.screen.height}, ${both.b.screen.codec}), ` +
        `camera subscribed ${both.b.camera.subscribed}, ${of(both.b.camera)} (${both.b.camera.width}×${both.b.camera.height}); ` +
        `c: screen ${of(both.c.screen)} (${both.c.screen.width}×${both.c.screen.height}), camera ${of(both.c.camera)} (${both.c.camera.width}×${both.c.camera.height})`,
    );
    console.log(
      `two tracks, b's screen small: demand screen 0b${bSmall.demand[1]?.toString(2)}; b's screen now layer ${bSmall.b.screen.layer} ` +
        `(${bSmall.b.screen.codec}; ${of(bSmall.b.screen)}), longest gap ${tr.gap.toFixed(0)} ms; ` +
        `decode errors b ${bSmall.b.screen.errors}+${bSmall.b.camera.errors}, c ${bSmall.c.screen.errors}+${bSmall.c.camera.errors}`,
    );
    const kib = (list) => list.map((n) => (n / 1024).toFixed(0)).join(" ");
    console.log(
      `two tracks, keyframes (KiB) by track and layer: ${Object.entries(tr.keyframes).map(([k, l]) => `${k}: ${kib(l)}`).join("; ")}; videolimit ${JSON.stringify(tr.limits)}`,
    );
    if (!process.env.NO_ASSERT) {
      assert.deepEqual(tr.connected, [true, true, true], "two tracks: still connected");
      assert.deepEqual(tr.limits, { a: [], b: [], c: [] }, "nobody's link was limited");
      assert.equal(both.demand[0], 0b0011, "camera: layers 0 (b) and 1 (c) wanted");
      assert.equal(both.demand[1], 0b1010, "screen: layers 1 (c) and 3 (b) wanted");
      assert.deepEqual(both.camera.layers.map((l) => l.running), [true, true], "both camera layers run");
      assert.deepEqual(both.screen.layers.map((l) => l.running), [true, false, true], "screen layers 1 and 3 run, not 2");
      assert.equal(both.b.screen.subscribed, 3);
      assert.equal(both.b.camera.subscribed, 0);
      assert.equal(both.c.screen.subscribed, 1);
      assert.equal(both.c.camera.subscribed, 1);
      assert.equal(both.b.screen.layer, 3, "b gets the screen's layer 3");
      assert.equal(both.b.screen.width, 1920, "at 1920 × 1080");
      assert.equal(both.b.camera.layer, 0, "and the camera's layer 0");
      assert.equal(both.b.camera.width, 320, "at 320 × 180");
      assert.equal(both.c.screen.layer, 1, "c gets the screen's layer 1");
      assert.equal(both.c.screen.width, 640);
      assert.equal(both.c.camera.layer, 1, "and the camera's layer 1");
      // Nothing above what each subscribed to, track by track (below, at the start, before
      // the layer asked for runs: the relay forwards the nearest below).
      assert.ok(layer(both.b.camera, 1).received <= 3, "b never gets the camera's layer 1 (but at the start, see simulcast)");
      // The screen has no layer below 1, so until layer 1 runs (H.264 starts later than VP8)
      // the relay forwards c the lowest that does.
      assert.ok(layer(both.c.screen, 2).received + layer(both.c.screen, 3).received <= 6, "c never gets the screen's layers 2 or 3 (but at the start)");
      assert.ok(layer(both.b.screen, 3).decoded > 60, `b decoded ${layer(both.b.screen, 3).decoded} of the screen's layer 3`);
      assert.ok(layer(both.b.camera, 0).decoded > 80, `b decoded ${layer(both.b.camera, 0).decoded} of the camera's layer 0`);
      assert.ok(layer(both.c.screen, 1).decoded > 60 && layer(both.c.camera, 1).decoded > 80, "c decodes both");
      assert.equal(both.b.screen.codec, "vp8");
      if (screenCodec) assert.match(both.c.screen.codec, /^avc1\./, "c decodes the screen's layer 1 as H.264");
      // b's screen small: layer 1 (in H.264, if so) from its keyframe; layer 3 no longer wanted.
      assert.equal(bSmall.demand[1], 0b0010, "screen: only layer 1 wanted");
      assert.equal(bSmall.b.screen.layer, 1);
      assert.equal(bSmall.b.screen.width, 640);
      assert.equal(bSmall.b.screen.codec, screenCodec ? bSmall.c.screen.codec : "vp8");
      assert.ok(tr.gap < 600, `b's longest gap in the screen while it moved: ${tr.gap} ms`);
      for (const m of [bSmall.b.screen, bSmall.b.camera, bSmall.c.screen, bSmall.c.camera]) assert.equal(m.errors, 0, "decode errors");
    }
  }

  if (want("fullhd")) {
    // 1080p with real keyframes, from a busy picture: each arrives whole and decodes.
    const runs = [
      ["vp8", { noise: 0.1 }],
      ["vp8, noisier", { noise: 0.6 }],
      ...(avc ? [["h.264", { noise: 0.1, codec: "avc1.640028" }]] : []),
    ];
    const results = [];
    for (const [name, options] of runs) {
      const r = await page.evaluate((o) => window.fullHd(o), { relay, hash, seconds: 8, ...options });
      const l = r.stats.a.layers[0];
      const b = r.stats.b ?? {};
      console.log(
        `1080p, ${name}: ${l.codec}${l.hardware ? " (hw)" : ""}, keyframes (KiB) ${r.keyframes.map((k) => (k.size / 1024).toFixed(0)).join(" ")}, ` +
          `${r.keyframes.filter((k) => k.decoded?.width === 1920).length} of ${r.keyframes.length} decoded at 1920 wide; ` +
          `largest delta ${(r.deltaMax / 1024).toFixed(0)} KiB; ${r.decodedSent} of ${r.sent} frames decoded; ` +
          `b: ${b.decoded} decoded, ${b.errors} errors, ${b.width}×${b.height} (${b.codec}); ` +
          `a: ${l.oversized} too large, ${l.requests} keyframe requests, bitrate at the end ${Math.round(l.bitrate / 1000)} kbit/s, ` +
          `${l.width}×${l.height}; its backlog emptied ${r.drains.map((d) => `${(d.bytes / 1024).toFixed(0)} KiB in ${d.ms} ms`).join(", ")}; ${r.dropped} bytes dropped`,
      );
      results.push(r);
    }
    if (!process.env.NO_ASSERT) {
      for (const r of results) {
        assert.deepEqual(r.connected, [true, true], "1080p: still connected");
        assert.equal(r.stats.b.errors, 0, "1080p: decode errors");
        assert.ok(r.keyframes.length >= 2, `${r.keyframes.length} keyframes`);
        assert.ok(r.keyframes.every((k) => k.decoded), "every keyframe arrived whole and decoded");
        assert.ok(r.decodedSent >= r.sent * 0.9, `${r.decodedSent} of ${r.sent} decoded`);
        assert.ok(r.stats.a.layers[0].sent > 0, "frames too large were skipped, not stuck on");
      }
      // A busy screen: keyframes far over 64 KiB, whole at 1920 × 1080.
      const [first] = results;
      assert.equal(first.stats.b.width, 1920, "decoded at 1920 wide");
      assert.ok(first.keyframes.every((k) => k.size > 64 << 10 && k.decoded?.width === 1920), "keyframes over 64 KiB, at 1920 wide");
      // Keyframes over the 255,000 bytes a frame once had (a 1080p screen's, in H.264 above
      // all) arrive whole, and none is too large.
      assert.ok(results.some((r) => r.keyframes.some((k) => k.size > 255_000 && k.decoded?.width === 1920)), "a keyframe over 255,000 bytes, decoded");
      for (const r of results) assert.equal(r.stats.a.layers[0].oversized, 0, "no frame too large");
    }
  }
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  await browser.close();
  server.close();
  udpProxy.close();
  for (const up of upstreams.values()) up.socket.close();
  relayProcess.kill();
}
process.exit(failed ? 1 : 0);
