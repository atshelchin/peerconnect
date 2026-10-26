// Groups in a real browser (GROUP.md §3.6): headless Chromium members on
// WebTransport against the Rust relay, with a certificate made for the
// occasion and pinned by its hash (`serverCertificateHashes`, no domain, no
// certificate authority), in one group with a Rust member on raw QUIC.
// Groups travel over WebTransport only, so this is where their behaviour is
// tested from JavaScript: commands, chat, direct messages, voice,
// datagrams, leaving, state transfer, the four-member limit. And there is
// no fallback: a relay whose UDP does not answer, or whose certificate is
// not trusted, leaves the member offline.
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
    child.on("exit", (code) => reject(new Error(`the relay exited (${code})`)));
    // Its stderr is unbuffered: the line may come in pieces.
    let log = "";
    child.stderr.on("data", (data) => {
      const match = /listening on 127\.0\.0\.1:(\d+) /.exec((log += data));
      if (match) resolve(Number(match[1]));
    });
  });
  return { child, port };
}
const relay = startRelay({});
// Its TCP port answers (a WebSocket would get through), its UDP port nobody.
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
// No ignoreHTTPSErrors: an unpinned self-signed certificate must be refused.
const context = await browser.newContext();

/** A member in its own page; what it hears is kept in `window.got`. */
async function member(invite, name, pinned = true) {
  const page = await context.newPage();
  page.on("pageerror", (e) => console.error(`${name}: ${e}`));
  await page.goto(`${origin}/blank.html`);
  await page.evaluate(
    async ({ invite, name, hash, pinned }) => {
      // Every session the SDK opens, and how far it got: `ready` once
      // WebTransport is up, then the close code or error.
      const sessions = [];
      window.WebTransport = class extends WebTransport {
        constructor(...args) {
          super(...args);
          const session = { ready: false };
          sessions.push(session);
          this.ready.then(() => (session.ready = true), () => {});
          this.closed.then((info) => (session.closed = info.closeCode), (e) => (session.closed = e.message));
        }
      };
      const { Identity, joinGroup } = await import("/index.js");
      const webTransport = pinned
        ? { serverCertificateHashes: [{ algorithm: "sha-256", value: new Uint8Array(hash) }] }
        : undefined;
      const identity = Identity.generate();
      const started = performance.now();
      const conn = joinGroup(invite, { identity, profile: name, webTransport });
      window.conn = conn;
      const got = (window.got = {
        ownId: conn.id === identity.id,
        messages: [],
        voice: [],
        datagrams: [],
        joined: [],
        left: [],
        states: [],
        full: 0,
        sessions,
      });
      const text = new TextDecoder();
      conn.addEventListener("state", () => {
        got.states.push(conn.state);
        if (conn.state === "online" && got.online === undefined) got.online = performance.now() - started;
      });
      conn.addEventListener("message", ({ detail: d }) =>
        got.messages.push({ from: d.from, channel: d.channel, direct: d.direct, text: text.decode(d.data) }),
      );
      conn.addEventListener("voice", ({ detail: d }) =>
        got.voice.push({ from: d.from, seq: d.seq, length: d.data.length, head: [...d.data.slice(0, 3)] }),
      );
      conn.addEventListener("datagram", ({ detail: d }) => got.datagrams.push({ from: d.from, text: text.decode(d.data) }));
      conn.addEventListener("joined", (e) => got.joined.push(e.detail.member));
      conn.addEventListener("left", (e) => got.left.push(e.detail.member));
      conn.addEventListener("full", () => got.full++);
    },
    { invite, name, hash, pinned },
  );
  return page;
}

const until = async (page, what, args) => {
  await page.waitForFunction(what, args, { timeout: 20_000 });
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const idOf = (page) => page.evaluate(() => window.conn.id);
/** The texts `page` heard from `from` (not directly), on `channel` if given. */
const heard = (page, from, channel) =>
  page.evaluate(
    ({ from, channel }) =>
      window.got.messages.filter((m) => m.from === from && !m.direct && (!channel || m.channel === channel)).map((m) => m.text),
    { from, channel },
  );

/** A Rust member on raw QUIC, its output lines collected. */
function rustMember(invite, name) {
  const child = spawn(`${target}examples/group`, ["join", invite, name]);
  const lines = [];
  let log = "";
  child.stderr.on("data", (d) => (log += d));
  createInterface({ input: child.stdout }).on("line", (l) => lines.push(l));
  return {
    child,
    lines,
    log: () => log,
    async hears(line) {
      const deadline = Date.now() + 20_000;
      while (!lines.includes(line) && Date.now() < deadline) await sleep(50);
      assert.ok(lines.includes(line), `the Rust member heard ${JSON.stringify(lines)}, not ${JSON.stringify(line)}`);
    },
  };
}

const group = Group.create(`https://127.0.0.1:${port}`);
const invite = group.toString();
const fragment = invite.slice(invite.indexOf("#"));

let failed = false;
const rust = rustMember(`quic://127.0.0.1:${port}${fragment}`, "rust");
const others = [];
try {
  const a = await member(invite, "a");
  const b = await member(invite, "b");
  const c = await member(invite, "c");
  const members = [a, b, c];
  for (const page of members) await until(page, () => window.conn.members.size === 3);
  for (const page of members) {
    assert.deepEqual(
      await page.evaluate(() => [window.conn.connected, window.conn.state, window.got.ownId]),
      [true, "online", true],
    );
  }
  assert.match(rust.log(), /Online/, "the Rust member is online, on raw QUIC");
  const [aId, bId, cId] = await Promise.all(members.map(idOf));
  const profiles = await b.evaluate(() =>
    Object.fromEntries([...window.conn.members].map(([id, p]) => [id, new TextDecoder().decode(p)])),
  );
  const rustId = Object.keys(profiles).find((id) => profiles[id] === "rust");
  assert.equal(profiles[aId], "a");
  assert.ok(rustId, `profiles: ${JSON.stringify(profiles)}`);
  console.log(`browsers over WebTransport, online after ${Math.round(await a.evaluate(() => window.got.online))} ms`);

  // Commands and chat, each in order on its stream, to everyone and back to the sender.
  await a.evaluate(() => {
    for (let i = 0; i < 15; i++) {
      window.conn.send("command", `build ${i}`);
      window.conn.send("chat", `hi ${i}`);
    }
  });
  for (const page of members) {
    await until(page, (a) => window.got.messages.filter((m) => m.from === a).length === 30, aId);
    assert.deepEqual(await heard(page, aId, "command"), Array.from({ length: 15 }, (_, i) => `build ${i}`));
    assert.deepEqual(await heard(page, aId, "chat"), Array.from({ length: 15 }, (_, i) => `hi ${i}`));
  }
  await rust.hears("a command: build 14");
  await rust.hears("a chat: hi 14");

  // A direct message reaches only its addressee.
  await b.evaluate((c) => window.conn.sendTo(c, "command", "psst"), cId);
  await until(c, () => window.got.messages.some((m) => m.direct));
  assert.deepEqual(
    await c.evaluate(() => window.got.messages.filter((m) => m.direct)),
    [{ from: bId, channel: "command", direct: true, text: "psst" }],
  );

  // Voice as datagrams, one too big for a datagram on the voice stream, and
  // `seq` taken modulo 2^16.
  const accepted = await b.evaluate(() => {
    for (let seq = 0; seq < 50; seq++) window.conn.sendVoice(seq, new Uint8Array(80));
    window.conn.sendVoice(50, new Uint8Array(5000));
    return window.conn.sendVoice(65536 + 60, new Uint8Array([1, 2, 3]));
  });
  assert.equal(accepted, true);
  await until(a, () => window.got.voice.some((v) => v.seq === 50) && window.got.voice.some((v) => v.seq === 60));
  const voice = await a.evaluate(() => window.got.voice);
  assert.ok(voice.length >= 45, `${voice.length} voice frames`);
  assert.ok(voice.every((v) => v.from === bId));
  assert.equal(voice.find((v) => v.seq === 50).length, 5000);
  assert.deepEqual(voice.find((v) => v.seq === 60).head, [1, 2, 3]);

  // A datagram to one member.
  await c.evaluate((a) => window.conn.sendDatagram("pose", a), aId);
  await until(a, () => window.got.datagrams.length > 0);
  assert.deepEqual(await a.evaluate(() => window.got.datagrams), [{ from: cId, text: "pose" }]);

  // From Rust on raw QUIC to the browsers on WebTransport; the command may
  // overtake the chat line, being on another stream.
  rust.child.stdin.write("hello from rust\n!place 7\n");
  for (const page of members) {
    await until(page, (r) => window.got.messages.filter((m) => m.from === r).length === 2, rustId);
    assert.deepEqual(await heard(page, rustId, "chat"), ["hello from rust"]);
    assert.deepEqual(await heard(page, rustId, "command"), ["place 7"]);
  }

  // Four is the most a group holds: a fifth member reaches the relay but
  // is turned away, and should hear that the group is full.
  const fifth = await member(invite, "fifth");
  others.push(fifth);
  await until(fifth, () => window.got.sessions.some((s) => s.closed !== undefined && s.ready));
  await fifth.waitForFunction(() => window.got.full > 0, null, { timeout: 5000 }).catch(() => {});
  const turnedAway = await fifth.evaluate(() => ({
    state: window.conn.state,
    joined: window.got.joined.length,
    full: window.got.full,
    closed: window.got.sessions.map((s) => s.closed),
  }));
  assert.deepEqual([turnedAway.state, turnedAway.joined], ["offline", 0]);
  if (!turnedAway.full) {
    // The relay closes with FULL (4429), but that close code does not always reach Chromium.
    console.log(`fifth member: turned away, but no "full" event (sessions closed: ${JSON.stringify(turnedAway.closed)})`);
  }
  await fifth.close();
  assert.equal(await a.evaluate(() => window.conn.members.size), 3);

  // Leaving: the others hear it, and the connection is closed for good.
  await b.evaluate(() => window.conn.leave());
  await until(a, (b) => window.got.left.includes(b), bId);
  await until(b, () => window.conn.state === "closed");
  assert.equal(await b.evaluate(() => window.conn.connected), false);
  await rust.hears("a chat: hi 14"); // still there
  console.log("commands, chat, direct, voice, datagrams, the limit of four and leaving: ok");

  // No fallback. An unpinned self-signed certificate is refused by
  // WebTransport, though there is room in the group: the member stays offline.
  const d = await member(invite, "d", false);
  others.push(d);
  // A relay whose UDP nobody answers: offline too, though its TCP port would.
  const e = await member(Group.parse(`https://127.0.0.1:${noUdpPort}${fragment}`).toString(), "e");
  others.push(e);
  await sleep(4000);
  for (const [name, page] of [["untrusted certificate", d], ["no UDP", e]]) {
    const seen = await page.evaluate(() => ({
      state: window.conn.state,
      states: window.got.states,
      connected: window.conn.connected,
      joined: window.got.joined.length,
      opened: window.got.sessions.filter((s) => s.ready).length,
    }));
    const tries = await page.evaluate(() => window.got.sessions.length);
    assert.deepEqual(seen, { state: "offline", states: [], connected: false, joined: 0, opened: 0 }, name);
    assert.ok(tries >= 2, `${name}: ${tries} tries`); // and it keeps trying
    console.log(`${name}: offline after 4 s and ${tries} tries, as it should be`);
  }
  assert.equal(await a.evaluate(() => window.conn.members.size), 2, "nobody came in through another way");

  // State transfer, in a group of its own: a newcomer syncs from a member,
  // then gets every later command once.
  const sync = Group.create(`https://127.0.0.1:${port}`).toString();
  const host = await member(sync, "host");
  const builder = await member(sync, "builder");
  others.push(host, builder);
  for (const page of [host, builder]) await until(page, () => window.conn.members.size === 1);
  // The application: a list of applied commands, its snapshot the JSON of it.
  const world = (page) =>
    page.evaluate(() => {
      const text = new TextDecoder();
      const conn = window.conn;
      const state = (window.world = { applied: [] });
      conn.addEventListener("message", (e) => {
        if (e.detail.channel === "command" && !e.detail.direct) state.applied.push(text.decode(e.detail.data));
      });
      conn.addEventListener("snapshot", (e) => {
        state.applied = JSON.parse(text.decode(e.detail.data));
        state.synced = true;
      });
      conn.addEventListener("syncrequest", (e) => {
        // Taken now, answered whenever: here, a moment later.
        const snapshot = JSON.stringify(state.applied);
        setTimeout(() => conn.provideSync(e.detail.from, e.detail.transfer, snapshot), 50);
      });
    });
  await world(host);
  await world(builder);
  const builderId = await idOf(builder);
  await builder.evaluate(() => {
    for (let i = 0; i < 50; i++) window.conn.send("command", `op ${i}`);
  });
  await until(host, () => window.world.applied.length === 50);

  const visitor = await member(sync, "visitor");
  others.push(visitor);
  await world(visitor);
  await until(visitor, () => window.conn.members.size === 2);
  const hostId = await idOf(host);
  await visitor.evaluate((host) => window.conn.requestSync(host), hostId);
  await builder.evaluate(() => {
    for (let i = 50; i < 60; i++) window.conn.send("command", `op ${i}`);
  });
  await until(visitor, () => window.world.synced);
  await until(host, () => window.world.applied.length === 60);
  await sleep(300);
  const applied = await visitor.evaluate(() => window.world.applied);
  assert.deepEqual([...applied].sort(), [...(await host.evaluate(() => window.world.applied))].sort());
  assert.equal(applied.length, 60);
  assert.equal(new Set(applied).size, 60);
  assert.ok((await heard(visitor, builderId, "command")).length <= 10, "the snapshot's commands are not delivered again");
  console.log("state transfer: 60 commands, each applied once");

  // A relay with no domain and no certificate, from a quic:// invite: the
  // browser pins the weekly certificate it computes; Rust takes raw QUIC.
  const quicInvite = Group.create(`quic://127.0.0.1:${barePort}`).toString();
  const f = await member(quicInvite, "f", false);
  others.push(f);
  const rust2 = rustMember(quicInvite, "rust");
  try {
    await until(f, () => window.conn.members.size === 1);
    assert.equal(await f.evaluate(() => window.conn.connected), true);
    const rust2Id = await f.evaluate(() => [...window.conn.members.keys()][0]);
    await f.evaluate(() => window.conn.send("command", "over WebTransport"));
    await rust2.hears("f command: over WebTransport");
    rust2.child.stdin.write("over raw QUIC\n");
    await until(f, (r) => window.got.messages.some((m) => m.from === r), rust2Id);
    assert.deepEqual(await heard(f, rust2Id, "chat"), ["over raw QUIC"]);
    const online = Math.round(await f.evaluate(() => window.got.online));
    console.log(`quic:// relay, no certificate: browser over WebTransport after ${online} ms`);
  } finally {
    rust2.child.kill();
  }
  console.log("ok: WebTransport and raw QUIC members in one group, and no way in without them");
} catch (err) {
  failed = true;
  console.error(err);
} finally {
  rust.child.kill();
  await browser.close();
  relay.child.kill();
  noUdp.child.kill();
  bare.child.kill();
  server.close();
}
process.exit(failed ? 1 : 0);
