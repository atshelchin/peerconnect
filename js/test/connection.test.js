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

async function pair() {
  const inviter = Room.create(relay);
  assert.equal(inviter.role, "inviter");
  assert.equal(inviter.paired, false);
  const a = connect(inviter);
  await state(a, "waiting");
  const paired = event(a, "paired");
  const joined = Room.join(inviter.invite);
  assert.equal(joined.id, inviter.id);
  const b = connect(joined);
  await Promise.all([paired, state(a, "connected"), state(b, "connected")]);
  return { a, b, joined };
}

test("pairs, exchanges messages, and reconnects from a saved room", async () => {
  const { a, b } = await pair();
  assert.equal(a.room.paired, true);
  assert.equal(a.room.invite, undefined);
  assert.deepEqual(a.room.peerKey, b.room.publicKey);

  b.send("hello from b");
  assert.equal(await text(a), "hello from b");
  const big = Uint8Array.from({ length: 1_000_000 }, (_, i) => i % 253);
  const received = message(b);
  a.send(big);
  assert.deepEqual(await received, big);
  assert.throws(() => a.send(new Uint8Array(16 * 1024 * 1024 + 1)), /too large/);

  // Leave, restore the inviter from its saved string, and talk again.
  const saved = a.room.toString();
  const waiting = state(b, "waiting");
  a.leave();
  assert.equal(a.state, "closed");
  assert.equal(a.reason, "left");
  await waiting;
  assert.throws(() => b.send("nobody there"), /not connected/);
  const a2 = connect(saved);
  await Promise.all([state(a2, "connected"), state(b, "connected")]);
  a2.send("back");
  assert.equal(await text(b), "back");

  const ended = state(b, "closed");
  a2.close();
  await ended;
  assert.equal(b.reason, "closed-by-peer");
});

test("a second connection to the same room replaces the first", async () => {
  const { a, b, joined } = await pair();
  const replaced = event(b, "replaced");
  const b2 = connect(joined.toString());
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
  // The joiner is connected half a round trip after the inviter: wait for
  // the Rust side's own report before asking it to send.
  const rustConnected = new Promise((resolve) =>
    createInterface({ input: rust.stderr }).on("line", (line) => line === "* Connected" && resolve()),
  );
  try {
    await Promise.all([connected, rustConnected]);
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
