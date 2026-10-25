// Groups over WebTransport in a real browser (GROUP.md §3.6): headless
// Chromium members against the Rust relay, with a certificate made for the
// occasion and pinned by its hash (`serverCertificateHashes`, no domain, no
// certificate authority), in one group with a Rust member on raw QUIC.
// Then the fallback: a relay whose UDP does not answer is reached over its
// `wss://` WebSocket instead.
//
//   cargo build -p peerconnect-relay && cargo build -p peerconnect --features client --examples
//   node test/webtransport.browser.mjs
//
// Uses Playwright from ../website and `openssl` to make the certificate.

import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createHash, X509Certificate } from "node:crypto";
import { createReadStream, mkdtempSync, readFileSync } from "node:fs";
import { createServer } from "node:http";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { createInterface } from "node:readline";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { Group } from "../index.js";

const root = fileURLToPath(new URL("..", import.meta.url));
const target = join(root, "../target/debug/");
const require = createRequire(new URL("../../website/package.json", import.meta.url));
const { chromium } = require("playwright");

// A certificate browsers accept by hash: ECDSA P-256, valid under 14 days.
const dir = mkdtempSync(join(tmpdir(), "peerconnect-wt-"));
const cert = join(dir, "relay.crt");
const key = join(dir, "relay.key");
execFileSync("openssl", [
  "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-days", "10",
  "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
  "-keyout", key, "-out", cert,
], { stdio: "ignore" });
const hash = [...createHash("sha256").update(new X509Certificate(readFileSync(cert)).raw).digest()];

/** Starts a relay; resolves to its port. */
function startRelay(env) {
  const child = spawn(`${target}peerconnect-relay`, {
    env: { ...process.env, PEERCONNECT_LISTEN: "127.0.0.1:0", PEERCONNECT_TLS_CERT: cert, PEERCONNECT_TLS_KEY: key, ...env },
  });
  const port = new Promise((resolve, reject) => {
    child.on("error", reject);
    child.stderr.on("data", (data) => {
      const match = /listening on 127\.0\.0\.1:(\d+)/.exec(data);
      if (match) resolve(Number(match[1]));
    });
  });
  return { child, port };
}
const relay = startRelay({});
const noUdp = startRelay({ PEERCONNECT_QUIC_LISTEN: "off" });
// No certificate at all: the weekly one (GROUP.md §3.7).
const bare = startRelay({ PEERCONNECT_TLS_CERT: "", PEERCONNECT_TLS_KEY: "" });
const [port, noUdpPort, barePort] = [await relay.port, await noUdp.port, await bare.port];

const types = { ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript" };
const server = createServer((req, res) => {
  const pathname = new URL(req.url, "http://x").pathname;
  if (pathname === "/blank.html") return res.writeHead(200, { "content-type": "text/html" }).end("<!doctype html>");
  const path = join(root, pathname);
  if (!path.startsWith(root)) return res.writeHead(403).end();
  res.setHeader("content-type", types[extname(path)] ?? "application/octet-stream");
  createReadStream(path)
    .on("error", () => res.writeHead(404).end())
    .pipe(res);
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const browser = await chromium.launch();
// The fallback's wss:// uses the same certificate, which no authority signed.
const context = await browser.newContext({ ignoreHTTPSErrors: true });

/** A member in its own page; events are kept in `window.got`. */
async function member(invite, name, pinned = true) {
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`${name}: ${e}`));
  await page.goto(`${origin}/blank.html`);
  await page.evaluate(
    async ({ invite, name, hash, pinned }) => {
      const { joinGroup } = await import("/index.js");
      const webTransport = pinned
        ? { serverCertificateHashes: [{ algorithm: "sha-256", value: new Uint8Array(hash) }] }
        : undefined;
      const started = performance.now();
      const conn = joinGroup(invite, { profile: name, webTransport });
      window.conn = conn;
      window.got = { messages: [], voice: [] };
      conn.addEventListener("state", () => {
        if (conn.state === "online") window.got.online = performance.now() - started;
      });
      const text = new TextDecoder();
      conn.addEventListener("message", (e) =>
        window.got.messages.push(`${e.detail.channel}:${text.decode(e.detail.data)}`),
      );
      conn.addEventListener("voice", (e) => window.got.voice.push([e.detail.seq, e.detail.data.length]));
    },
    { invite, name, hash, pinned },
  );
  return page;
}

const until = async (page, what, args) => {
  await page.waitForFunction(what, args, { timeout: 20_000 });
};

const group = Group.create(`https://127.0.0.1:${port}`);
const invite = group.toString();
const fragment = invite.slice(invite.indexOf("#"));

let failed = false;
const rust = spawn(`${target}examples/group`, ["join", `quic://127.0.0.1:${port}${fragment}`, "rust"]);
let rustLog = "";
rust.stderr.on("data", (d) => (rustLog += d));
const rustLines = createInterface({ input: rust.stdout });
const rustHeard = [];
rustLines.on("line", (l) => rustHeard.push(l));
try {
  const a = await member(invite, "a");
  const b = await member(invite, "b");
  for (const page of [a, b]) await until(page, () => window.conn.members.size === 2);
  for (const page of [a, b]) assert.equal(await page.evaluate(() => window.conn.via), "webtransport");
  assert.match(rustLog, /Online over Quic/, "the Rust member is on raw QUIC");
  console.log(`browsers over WebTransport, online after ${Math.round(await a.evaluate(() => window.got.online))} ms`);

  // Commands and chat, each on its stream, to the other browser and to Rust.
  await a.evaluate(() => {
    for (let i = 0; i < 20; i++) window.conn.send("command", `a${i}`);
    window.conn.send("chat", "hello from a browser");
  });
  await until(b, () => window.got.messages.filter((m) => m.startsWith("command:a")).length === 20);
  assert.deepEqual(
    (await b.evaluate(() => window.got.messages)).filter((m) => m.startsWith("command:a")),
    Array.from({ length: 20 }, (_, i) => `command:a${i}`),
  );
  const deadline = Date.now() + 20_000;
  while (!rustHeard.includes("a chat: hello from a browser") && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 50));
  }
  assert.ok(rustHeard.includes("a chat: hello from a browser"), `the Rust member heard: ${JSON.stringify(rustHeard)}`);

  // Voice as datagrams, and one too big for a datagram on the voice stream.
  await b.evaluate(() => {
    for (let seq = 0; seq < 50; seq++) window.conn.sendVoice(seq, new Uint8Array(80));
    window.conn.sendVoice(50, new Uint8Array(5000));
  });
  await until(a, () => window.got.voice.some(([seq]) => seq === 50));
  const voice = await a.evaluate(() => window.got.voice);
  assert.ok(voice.length >= 45, `${voice.length} voice frames`);
  assert.deepEqual(voice.find(([seq]) => seq === 50), [50, 5000]);

  // From Rust on raw QUIC to the browsers on WebTransport.
  rust.stdin.write("hello from rust\n!place 7\n");
  for (const page of [a, b]) {
    await until(page, () => window.got.messages.includes("chat:hello from rust") && window.got.messages.includes("command:place 7"));
  }

  // Without UDP: the WebSocket, at once (Chromium hears that nobody listens).
  const c = await member(Group.parse(`https://127.0.0.1:${noUdpPort}${fragment}`).toString(), "c");
  await until(c, () => window.conn.state === "online");
  assert.equal(await c.evaluate(() => window.conn.via), "websocket");
  console.log(`no UDP: over wss:// after ${Math.round(await c.evaluate(() => window.got.online))} ms`);

  // An unpinned self-signed certificate is refused by WebTransport: wss:// it is.
  const d = await member(invite, "d", false);
  await until(d, () => window.conn.members.size === 3);
  assert.equal(await d.evaluate(() => window.conn.via), "websocket");
  console.log(`untrusted certificate: over wss:// after ${Math.round(await d.evaluate(() => window.got.online))} ms`);
  await until(a, () => window.conn.members.size === 3);
  await d.evaluate(() => window.conn.send("chat", "from the fallback"));
  await until(a, () => window.got.messages.includes("chat:from the fallback"));
  // A relay with no domain and no certificate, from a quic:// invite: the
  // browser pins the weekly certificate it computes; Rust takes raw QUIC.
  const quicInvite = Group.create(`quic://127.0.0.1:${barePort}`).toString();
  const e = await member(quicInvite, "e", false);
  const rust2 = spawn(`${target}examples/group`, ["join", quicInvite, "rust"]);
  try {
    await until(e, () => window.conn.members.size === 1);
    assert.equal(await e.evaluate(() => window.conn.via), "webtransport");
    console.log(`quic:// relay, no certificate: browser over WebTransport after ${Math.round(await e.evaluate(() => window.got.online))} ms`);
  } finally {
    rust2.kill();
  }
  console.log("ok: WebTransport, raw QUIC and WebSocket members in one group");
} catch (err) {
  failed = true;
  console.error(err);
} finally {
  rust.kill();
  await browser.close();
  relay.child.kill();
  noUdp.child.kill();
  bare.child.kill();
  server.close();
}
process.exit(failed ? 1 : 0);
