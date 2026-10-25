// Voice in a real browser (headless Chromium with its fake microphone):
// two members in one page, through a real relay, measuring the time from
// the end of a frame's capture to the start of its playout.
//
//   node test/voice.browser.mjs        (needs ../target/debug/peerconnect-relay
//                                       and Playwright's Chromium)

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createReadStream } from "node:fs";
import { createServer } from "node:http";
import { createServer as createTcpServer, connect as tcpConnect } from "node:net";
import { createRequire } from "node:module";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const require = createRequire(new URL("../../website/package.json", import.meta.url));
const { chromium } = require("playwright");

const relayProcess = spawn(join(root, "../target/debug/peerconnect-relay"), {
  env: { ...process.env, PEERCONNECT_LISTEN: "127.0.0.1:0" },
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
const network = { jitter: 0, stallUntil: 0 };
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
      const at = Math.max(last, Date.now() + Math.random() * network.jitter, network.stallUntil);
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

  if (!process.env.NO_ASSERT) {
    assert.ok(direct.frames > 150 && direct.p50 < 40, "direct");
    assert.ok(vad.frames > 5 && vad.p50 < 80, "only speech");
    assert.ok(jitter.frames > 150 && jitter.p50 < 250 && jitter.stats.target < 0.25, "jitter");
    // After the stall the buffer catches up: latency at the end is back down.
    assert.ok(stall.tail < 150, `tail after a stall: ${stall.tail} ms`);
  }
} finally {
  await browser.close();
  server.close();
  proxy.close();
  relayProcess.kill();
}
