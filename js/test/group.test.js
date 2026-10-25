// End-to-end tests of JavaScript groups against a real relay, and against
// the Rust client. Uses PEERCONNECT_RELAY if set, otherwise starts the Rust
// relay from ../target/debug (run `cargo build` first).

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { Group, Identity, joinGroup } from "../index.js";

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

const decoder = new TextDecoder();

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

/** Resolves once `conn` has admitted `n` members. */
function admitted(conn, n) {
  if (conn.members.size >= n) return Promise.resolve();
  return event(conn, "joined", () => conn.members.size >= n);
}

/** Collects the next `n` stream messages from `from` as text. */
function texts(conn, from, n, channel) {
  const got = [];
  return event(conn, "message", (e) => {
    const { detail } = e;
    if (detail.from !== from || detail.direct || (channel && detail.channel !== channel)) return false;
    return got.push(decoder.decode(detail.data)) === n;
  }).then(() => got);
}

test("members admit each other and exchange commands, chat, voice and datagrams", async () => {
  const group = Group.create(relay);
  assert.equal(Group.parse(group.toString()).id, group.id);
  const alice = Identity.generate();
  assert.equal(Identity.fromSeed(alice.seed).id, alice.id);
  const a = joinGroup(group, { identity: alice, profile: "alice" });
  const b = joinGroup(group.toString(), { profile: "bob" });
  const c = joinGroup(group, { profile: "carol" });
  assert.equal(a.id, alice.id);
  await Promise.all([admitted(a, 2), admitted(b, 2), admitted(c, 2)]);
  assert.equal(decoder.decode(b.members.get(a.id)), "alice");
  assert.equal(a.state, "online");

  const atB = texts(b, a.id, 30);
  const atC = texts(c, a.id, 30);
  const own = texts(a, a.id, 30);
  for (let i = 0; i < 15; i++) {
    a.send("command", `build ${i}`);
    a.send("chat", `hi ${i}`);
  }
  for (const got of [await atB, await atC, await own]) {
    assert.deepEqual(
      got.filter((t) => t.startsWith("build")),
      Array.from({ length: 15 }, (_, i) => `build ${i}`),
    );
    assert.deepEqual(
      got.filter((t) => t.startsWith("hi")),
      Array.from({ length: 15 }, (_, i) => `hi ${i}`),
    );
  }

  const direct = event(c, "message", (e) => e.detail.direct);
  b.sendTo(c.id, "command", "psst");
  assert.equal(decoder.decode((await direct).detail.data), "psst");

  const voice = event(b, "voice");
  assert.equal(a.sendVoice(65537, new Uint8Array([1, 2, 3])), true);
  const { detail } = await voice;
  assert.deepEqual([detail.from, detail.seq, [...detail.data]], [a.id, 1, [1, 2, 3]]);

  const datagram = event(a, "datagram");
  c.sendDatagram("pose", a.id);
  assert.equal(decoder.decode((await datagram).detail.data), "pose");

  const left = event(a, "left");
  b.leave();
  assert.equal((await left).detail.member, b.id);
  assert.equal(b.state, "closed");
  a.leave();
  c.leave();
});

test("a newcomer syncs the state from a member, then gets every later command once", async () => {
  const group = Group.create(relay);
  const host = joinGroup(group, { profile: "host" });
  const builder = joinGroup(group, { profile: "builder" });
  await Promise.all([admitted(host, 1), admitted(builder, 1)]);

  // The application: a list of applied commands, its snapshot the JSON of it.
  const world = (conn) => {
    const state = { applied: [] };
    conn.addEventListener("message", (e) => {
      if (e.detail.channel === "command" && !e.detail.direct) state.applied.push(decoder.decode(e.detail.data));
    });
    conn.addEventListener("snapshot", (e) => {
      state.applied = JSON.parse(decoder.decode(e.detail.data));
    });
    conn.addEventListener("syncrequest", (e) => {
      // Taken now, answered whenever: here, a moment later.
      const snapshot = JSON.stringify(state.applied);
      setTimeout(() => conn.provideSync(e.detail.from, e.detail.transfer, snapshot), 50);
    });
    return state;
  };
  const hostWorld = world(host);
  world(builder);
  const done = texts(host, builder.id, 50, "command");
  for (let i = 0; i < 50; i++) builder.send("command", `op ${i}`);
  await done;

  const visitor = joinGroup(group, { profile: "visitor" });
  const visitorWorld = world(visitor);
  await admitted(visitor, 2);
  const snapshot = event(visitor, "snapshot");
  visitor.requestSync(host.id);
  for (let i = 50; i < 60; i++) builder.send("command", `op ${i}`);
  await snapshot;
  await new Promise((resolve) => setTimeout(resolve, 300));
  assert.deepEqual([...visitorWorld.applied].sort(), [...hostWorld.applied].sort());
  assert.equal(visitorWorld.applied.length, 60);
  assert.equal(new Set(visitorWorld.applied).size, 60);
  for (const conn of [host, builder, visitor]) conn.leave();
});

test("a JavaScript member and a Rust member talk", async () => {
  const group = Group.create(relay);
  const js = joinGroup(group, { profile: "js" });
  const rust = spawn(`${target}examples/group`, ["join", group.toString(), "rust"]);
  const lines = createInterface({ input: rust.stdout });
  const line = () => new Promise((resolve) => lines.once("line", resolve));
  try {
    await admitted(js, 1);
    const [rustId] = js.members.keys();
    assert.equal(decoder.decode(js.members.get(rustId)), "rust");

    const heard = line();
    js.send("chat", "hello from js");
    assert.equal(await heard, "js chat: hello from js");
    const command = line();
    js.send("command", "place block");
    assert.equal(await command, "js command: place block");

    // Different channels: the command may well overtake the chat line.
    const chat = texts(js, rustId, 1, "chat");
    const order = texts(js, rustId, 1, "command");
    rust.stdin.write("hello from rust\n!jump\n");
    assert.deepEqual([await chat, await order], [["hello from rust"], ["jump"]]);
  } finally {
    rust.kill();
    js.leave();
  }
});
