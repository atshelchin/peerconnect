// Video in a real browser (headless Chromium, its fake camera, WebCodecs):
// two members in one page through the Rust relay, over a WebSocket and
// over WebTransport, then through paths that lose packets: TCP stalls for
// the WebSocket, lost UDP datagrams for WebTransport.
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
import { createServer as createTcpServer, connect as tcpConnect } from "node:net";
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
const port = await new Promise((resolve, reject) => {
  relayProcess.on("error", reject);
  relayProcess.stderr.on("data", (d) => {
    const m = /listening on 127\.0\.0\.1:(\d+)/.exec(d);
    if (m) resolve(Number(m[1]));
  });
});

// Seeded, so that runs see the same losses.
let state = 1;
const random = () => (state = (state * 48271) % 2147483647) / 2147483647;

// TCP, in order, stalled now and then as by a lost segment's retransmission.
const tcp = { lossChance: 0 };
const proxy = createTcpServer((client) => {
  const upstream = tcpConnect(port, "127.0.0.1");
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
      const stall = random() < tcp.lossChance ? 100 + random() * 200 : 0;
      const at = Math.max(last, Date.now() + random() * 10 + stall);
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
await new Promise((r) => proxy.listen(0, "127.0.0.1", r));

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
  const pinned = { serverCertificateHashes: [{ algorithm: "sha-256", value: hash }] };
  const run = (name, options) =>
    page
      .evaluate(
        ({ webTransport, ...o }) =>
          window.run({
            ...o,
            webTransport: webTransport && {
              serverCertificateHashes: [{ algorithm: "sha-256", value: new Uint8Array(webTransport) }],
            },
          }),
        { seconds: 8, ...options, webTransport: options.webTransport ? hash : undefined },
      )
      .then((r) => {
        console.log(
          `${name} (${r.via[0]}): ${r.received.decoded} of ${r.sent.sent} frames decoded (${r.sent.codec}), ` +
            `send to decode p50 ${r.p50?.toFixed(0)} ms, p99 ${r.p99?.toFixed(0)} ms, max ${r.max?.toFixed(0)}; ` +
            `${r.sent.keyframes} keyframes, ${r.sent.requests} requested; bitrate at the end ${r.sent.bitrate / 1000} kbit/s; ` +
            `decode errors ${r.received.errors}`,
        );
        return r;
      });
  void pinned;

  // A camera held upright stays upright, and the picture turns on request.
  const shape = await page.evaluate((relay) => window.shape({ relay }), `ws://127.0.0.1:${port}`);
  console.log("tall picture:", JSON.stringify(shape));
  assert.ok(shape.upright.height > shape.upright.width, "sent tall");
  assert.equal(shape.upright.top, "red", "and upright");
  assert.ok(shape.turned.width > shape.turned.height, "turned by the sender: wide");
  assert.equal(shape.turned.left, "blue", "a quarter turn clockwise puts the top on the right");
  assert.ok(shape.viewed.width > shape.viewed.height, "turned by the receiver: wide");
  assert.equal(shape.viewed.left, "red", "three quarter turns put the top on the left");

  const ws = await run("video, WebSocket", { relay: `ws://127.0.0.1:${port}` });
  const wt = await run("video, WebTransport", { relay: `https://127.0.0.1:${port}`, webTransport: true });
  tcp.lossChance = 0.02;
  state = 1;
  const wsLossy = await run("video, WebSocket, TCP stalls", { relay: `ws://127.0.0.1:${proxy.address().port}` });
  tcp.lossChance = 0;
  udp.loss = 0.02;
  state = 1;
  const wtLossy = await run("video, WebTransport, 2% UDP loss each way", {
    relay: `https://127.0.0.1:${udpProxy.address().port}`,
    webTransport: true,
  });
  udp.loss = 0;

  if (!process.env.NO_ASSERT) {
    assert.deepEqual(ws.via, ["websocket", "websocket"]);
    assert.deepEqual(wt.via, ["webtransport", "webtransport"]);
    for (const [name, r] of [["ws", ws], ["wt", wt]]) {
      assert.ok(r.sent.sent > 100, `${name}: ${r.sent.sent} sent`);
      assert.ok(r.received.decoded >= r.sent.sent * 0.95, `${name}: ${r.received.decoded} of ${r.sent.sent} decoded`);
      assert.equal(r.received.errors, 0, `${name}: decode errors`);
      assert.ok(r.p99 < 100, `${name}: p99 ${r.p99} ms`);
    }
    assert.equal(wtLossy.received.errors, 0, "nothing undecodable is delivered");
    assert.ok(wtLossy.received.decoded >= wtLossy.sent.sent * 0.8, `lossy: ${wtLossy.received.decoded} of ${wtLossy.sent.sent}`);
    assert.equal(wsLossy.received.errors, 0);
  }
} catch (e) {
  failed = true;
  console.error(e);
} finally {
  await browser.close();
  server.close();
  proxy.close();
  udpProxy.close();
  for (const up of upstreams.values()) up.close();
  relayProcess.kill();
}
process.exit(failed ? 1 : 0);
