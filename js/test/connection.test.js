// End-to-end tests of the JavaScript SDK against a real relay, and against
// the Rust client. Uses PEERCONNECT_RELAY if set, otherwise starts the Rust
// relay from ../target/debug (run `cargo build` first).

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { connect, Room } from "../index.js";

const target = fileURLToPath(new URL("../../target/debug/", import.meta.url));
let relay;
let relayProcess;

before(async () => {
  relay = process.env.PEERCONNECT_RELAY;
  if (relay) return;
  relayProcess = spawn(`${target}peerconnect-relay`, {
    env: { ...process.env, PEERCONNECT_LISTEN: "127.0.0.1:0" },
  });
  relay = await new Promise((resolve, reject) => {
    relayProcess.on("error", reject);
    relayProcess.stderr.on("data", (data) => {
      const match = /listening on (\S+)/.exec(data);
      if (match) resolve(`ws://${match[1]}`);
    });
  });
});

after(() => relayProcess?.kill());

function event(target, type, accept = () => true) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out waiting for ${type}`)), 20_000);
    target.addEventListener(type, function listener(e) {
      if (!accept(e)) return;
      clearTimeout(timer);
      target.removeEventListener(type, listener);
      resolve(e);
    });
  });
}

const state = (conn, wanted) => event(conn, "state", () => conn.state === wanted);
const message = async (conn) => (await event(conn, "message")).data;
const text = async (conn) => new TextDecoder().decode(await message(conn));

/** The next `n` messages as text: several can arrive in one go, so listen throughout. */
function texts(conn, n) {
  const got = [];
  return event(conn, "message", (e) => got.push(new TextDecoder().decode(e.data)) === n).then(() => got);
}

async function pair() {
  const inviter = Room.create(relay);
  assert.equal(inviter.role, "inviter");
  assert.equal(inviter.paired, false);
  const a = connect(inviter);
  await state(a, "waiting");
  const joined = Room.join(inviter.invite);
  assert.equal(joined.id, inviter.id);
  const b = connect(joined);
  // Pairing completes, and both move to the pair ID.
  await Promise.all([event(a, "paired"), event(b, "paired")]);
  await Promise.all([a.state === "connected" || state(a, "connected"), b.state === "connected" || state(b, "connected")]);
  assert.equal(a.room.id, b.room.id);
  assert.notEqual(a.room.id, inviter.id);
  return { a, b };
}

test("pairs, delivers reliably, and reconnects from a saved room", async () => {
  const { a, b } = await pair();
  assert.equal(a.room.paired, true);
  assert.equal(a.room.invite, undefined);
  assert.deepEqual(a.room.peerKey, b.room.publicKey);

  const delivered = event(b, "delivered", (e) => e.detail === 1);
  assert.equal(b.send("hello from b"), 0);
  assert.equal(await text(a), "hello from b");
  await delivered;
  assert.equal(b.queued, 0);
  const big = Uint8Array.from({ length: 1_000_000 }, (_, i) => i % 253);
  const received = message(b);
  a.send(big);
  assert.deepEqual(await received, big);
  assert.throws(() => a.send(new Uint8Array(16 * 1024 * 1024 + 1)), /too large/);

  // a leaves; b's messages wait for it; a comes back from its saved room.
  const saved = a.room.toString();
  const waiting = state(b, "waiting");
  a.leave();
  assert.equal(a.state, "closed");
  assert.equal(a.reason, "left");
  await waiting;
  b.send("while you were away");
  b.send("and another");
  const a2 = connect(saved);
  assert.deepEqual(await texts(a2, 2), ["while you were away", "and another"]);
  a2.leave();
  b.leave();
});

test("closing ends the room for both", async () => {
  const { a, b } = await pair();
  const ended = state(b, "closed");
  const closed = state(a, "closed");
  a.close();
  assert.equal(a.room.closing, true);
  assert.throws(() => a.send("too late"), /closed/);
  await Promise.all([ended, closed]);
  assert.equal(b.reason, "closed-by-peer");
  assert.equal(a.reason, "closed");
});

test("a second connection to the same room replaces the first", async () => {
  const { a, b } = await pair();
  const replaced = event(b, "replaced");
  const b2 = connect(b.room.toString());
  await Promise.all([replaced, state(b2, "connected")]);
  assert.equal(b.state, "offline");
  b.leave(); // let the new instance keep the seat
  a.send("to b2");
  assert.equal(await text(b2), "to b2");
  a.leave();
  b2.leave();
});

test("interoperates with the Rust client", async () => {
  const inviter = Room.create(relay);
  const js = connect(inviter);
  await state(js, "waiting");
  const connected = state(js, "connected");
  const rust = spawn(`${target}examples/chat`, ["join", inviter.invite, "/dev/null"]);
  const lines = createInterface({ input: rust.stdout });
  const nextLine = () => new Promise((resolve) => lines.once("line", resolve));
  const rustPaired = new Promise((resolve) =>
    createInterface({ input: rust.stderr }).on("line", (line) => line === "* paired" && resolve()),
  );
  try {
    // Messages are queued until delivered, so nothing needs to wait for
    // the connection; waiting for pairing just keeps the test tidy.
    await Promise.all([connected, rustPaired]);
    const fromRust = text(js);
    rust.stdin.write("hello from rust\n");
    assert.equal(await fromRust, "hello from rust");
    const line = nextLine();
    js.send("hello from js");
    assert.equal(await line, "< hello from js");
  } finally {
    js.leave();
    rust.kill();
  }
});
