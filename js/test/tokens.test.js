// The SDK against a relay that requires tokens (SPEC §4.1): the Rust relay
// from ../target/debug with PEERCONNECT_TOKEN_SECRET, tokens minted by its
// own `token` command.

import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

import { connect, Room } from "../index.js";

const binary = fileURLToPath(new URL("../../target/debug/peerconnect-relay", import.meta.url));
const env = { ...process.env, PEERCONNECT_LISTEN: "127.0.0.1:0", PEERCONNECT_TOKEN_SECRET: "s".repeat(32) };
let relay;
let relayProcess;

const mint = (client) => execFileSync(binary, ["token", client, "--ttl", "1h"], { env }).toString().trim();

before(async () => {
  relayProcess = spawn(binary, { env });
  relay = await new Promise((resolve, reject) => {
    relayProcess.on("error", reject);
    let log = "";
    relayProcess.stderr.on("data", (data) => {
      const match = /listening on (\S+) /.exec((log += data));
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

const text = async (conn) => new TextDecoder().decode((await event(conn, "message")).data);

test("each peer brings its own token, as a string or a function", async () => {
  const inviter = Room.create(relay);
  const joiner = Room.join(inviter.invite);
  let asked = 0;
  const a = connect(inviter, { token: mint("c-alice") });
  const b = connect(joiner, {
    token: async () => {
      asked++;
      return mint("c-bob");
    },
  });
  b.send("hello");
  assert.equal(await text(a), "hello");
  assert.equal(asked, 1);
  a.leave();
  b.leave();
});

test("a refused token is replaced through the function, and nothing is lost", async () => {
  const inviter = Room.create(relay);
  const joiner = Room.join(inviter.invite);
  const a = connect(inviter, { token: mint("c-alice") });
  const tokens = ["not-a-token", mint("c-bob")];
  const b = connect(joiner, { token: () => tokens.shift() });
  const refused = event(b, "unauthorized");
  b.send("sent before the relay let us in");
  await refused;
  assert.equal(await text(a), "sent before the relay let us in");
  assert.equal(tokens.length, 0);
  a.leave();
  b.leave();
});

test("without a token, setToken lets the connection in", async () => {
  const inviter = Room.create(relay);
  const joiner = Room.join(inviter.invite);
  const a = connect(inviter, { token: mint("c-alice") });
  const b = connect(joiner);
  await event(b, "unauthorized");
  assert.equal(b.state, "offline");
  b.setToken(mint("c-bob"));
  b.send("in");
  assert.equal(await text(a), "in");
  a.leave();
  b.leave();
});
