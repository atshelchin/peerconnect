// The Cloudflare relay with tokens required (docs/self-serve.md): issuance
// over the internal API, admission, rotation and deletion closing live
// connections, and metering. Run by tests/run.sh against `wrangler dev`,
// with RELAY, SERVICE_SECRET set.

import assert from "node:assert/strict";
import { test } from "node:test";

import { connect, Room } from "../../../js/index.js";

const RELAY = process.env.RELAY; // ws://127.0.0.1:8787
const HTTP = RELAY.replace(/^ws/, "http");
const SERVICE = process.env.SERVICE_SECRET;

const unique = (prefix) => `${prefix}-${Math.random().toString(36).slice(2, 10)}`;
const exp = () => Math.floor(Date.now() / 1000) + 3600;

async function internal(path, body, secret = SERVICE) {
  const response = await fetch(`${HTTP}/internal/${path}`, {
    method: body === undefined ? "GET" : "POST",
    headers: { "content-type": "application/json", "x-peerconnect-service": secret },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  return { status: response.status, body: response.headers.get("content-type")?.includes("json") ? await response.json() : await response.text() };
}

const mint = async (client, app = "app_test") =>
  (await internal(`clients/${client}/mint`, { exp: exp(), app, actor: "test" })).body.token;

function event(target, type, accept = () => true, ms = 20_000) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${type}`)), ms);
    target.addEventListener(type, function listener(e) {
      if (!accept(e)) return;
      clearTimeout(timer);
      target.removeEventListener(type, listener);
      resolve(e);
    });
  });
}

const text = async (conn) => new TextDecoder().decode((await event(conn, "message")).data);

/** The close code a raw WebSocket to `url` gets before anything else, or "open" if it gets HELLO. */
function firstFrame(url) {
  return new Promise((resolve) => {
    const ws = new WebSocket(url);
    ws.binaryType = "arraybuffer";
    ws.onmessage = () => {
      ws.close();
      resolve("hello");
    };
    ws.onclose = (e) => resolve(e.code);
  });
}

async function pair(tokenA, tokenB) {
  const inviter = Room.create(RELAY);
  const joiner = Room.join(inviter.invite);
  const a = connect(inviter, { token: tokenA });
  const b = connect(joiner, { token: tokenB });
  b.send("ready?");
  assert.equal(await text(a), "ready?");
  return { a, b };
}

test("the internal API is closed without the service secret", async () => {
  assert.equal((await internal("clients/c-x/mint", { exp: exp(), actor: "x" }, "wrong".repeat(10))).status, 404);
  assert.equal((await internal("audit", undefined, "")).status, 404);
});

test("one generation is one token, shown again however often", async () => {
  const client = unique("c");
  const first = await internal(`clients/${client}/mint`, { exp: exp(), app: "app_test", actor: "test" });
  assert.equal(first.status, 200);
  assert.equal(first.body.gen, 0);
  const again = await internal(`clients/${client}/mint`, { exp: exp() + 99, actor: "test" });
  assert.equal(again.body.token, first.body.token);
  const other = await internal(`clients/${client}/mint`, { exp: exp(), app: "app_other", actor: "test" });
  assert.equal(other.status, 409, "a client stays with its application");
  const audit = await internal("audit?limit=20");
  const actions = audit.body.filter((r) => r.client === client).map((r) => r.action);
  assert.deepEqual(actions.sort(), ["mint", "reveal"]);
});

test("connections without a valid token are closed with 4403", async () => {
  const room = Room.create(RELAY);
  const url = `${RELAY}/v1/${room.id}`;
  assert.equal(await firstFrame(url), 4403);
  assert.equal(await firstFrame(`${url}?token=a.b.c`), 4403);
  assert.equal(await firstFrame(`${url}?token=${await mint(unique("c"))}`), "hello");
});

test("peers of different apps pair, and their use is counted", async () => {
  const alice = unique("c");
  const bob = unique("c");
  const { a, b } = await pair(await mint(alice, "app_chat"), await mint(bob, "app_game"));
  a.send("x".repeat(10_000));
  assert.equal((await text(b)).length, 10_000);
  // Rooms report 5 s after their first uncounted frame.
  await new Promise((r) => setTimeout(r, 7_000));
  a.leave();
  b.leave();
  await new Promise((r) => setTimeout(r, 1_000));
  const [usage] = (await internal(`clients/${alice}/usage`)).body;
  assert.ok(usage.connections >= 1, JSON.stringify(usage));
  assert.ok(usage.bytes_in > 10_000 && usage.bytes_out > 0, JSON.stringify(usage));
  assert.ok(usage.connected_ms > 0, JSON.stringify(usage));
});

test("rotating a token closes its connections, and the new one works", async () => {
  const alice = unique("c");
  const old = await mint(alice);
  // The connection is closed before the rotation's answer arrives: it waits for it.
  let handOver;
  const next = new Promise((resolve) => (handOver = resolve));
  const tokens = [old];
  const { a, b } = await pair(() => tokens.shift() ?? next, await mint(unique("c")));
  const refused = event(a, "unauthorized");
  const rotated = await internal(`clients/${alice}/rotate`, { exp: exp(), actor: "test" });
  assert.equal(rotated.status, 200);
  assert.equal(rotated.body.gen, 1);
  handOver(rotated.body.token);
  await refused;
  // The connection fetched the new token and carries on.
  b.send("after rotation");
  assert.equal(await text(a), "after rotation");
  const room = Room.create(RELAY);
  assert.equal(await firstFrame(`${RELAY}/v1/${room.id}?token=${old}`), 4403, "the old token is refused");
  a.leave();
  b.leave();
});

test("a deleted client is refused everywhere", async () => {
  const alice = unique("c");
  const token = await mint(alice);
  const { a, b } = await pair(token, await mint(unique("c")));
  const refused = event(a, "unauthorized");
  assert.equal((await internal(`clients/${alice}/delete`, { actor: "test" })).status, 200);
  await refused;
  assert.equal((await internal(`clients/${alice}/mint`, { exp: exp(), actor: "test" })).status, 410);
  const room = Room.create(RELAY);
  assert.equal(await firstFrame(`${RELAY}/v1/${room.id}?token=${token}`), 4403);
  a.leave();
  b.leave();
});

test("the trial is rate limited with 4429", async () => {
  const token = await mint(unique("t"), "trial");
  const room = Room.create(RELAY);
  const codes = [];
  for (let i = 0; i < 22; i++) codes.push(await firstFrame(`${RELAY}/v1/${room.id}?token=${token}`));
  assert.deepEqual(codes.slice(0, 20), Array(20).fill("hello"));
  assert.equal(codes[21], 4429);
});
