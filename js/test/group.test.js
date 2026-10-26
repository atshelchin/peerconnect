// Groups in Node: what needs no relay. Groups travel over WebTransport only
// (GROUP.md §3.6), which Node does not have, so members talking, state
// transfer and a Rust member in the same group are in
// test/webtransport.browser.mjs, against the Rust relay in Chromium.

import assert from "node:assert/strict";
import { test } from "node:test";

import { Group, Identity, groupsSupported, joinGroup, webTransportFor, weeklyHashes } from "../index.js";

test("invites and identities round-trip", () => {
  const group = Group.create("https://r.example");
  assert.equal(Group.parse(group.toString()).id, group.id);
  assert.equal(Group.parse(group.toString()).relay, "https://r.example");
  const alice = Identity.generate();
  assert.equal(Identity.fromSeed(alice.seed).id, alice.id);
});

test("relays are https:// or quic://, never a WebSocket", () => {
  assert.equal(Group.create("https://r.example:8443/p").relay, "https://r.example:8443/p");
  assert.equal(Group.create("quic://203.0.113.9:4433").relay, "quic://203.0.113.9:4433");
  for (const relay of ["wss://r.example", "ws://127.0.0.1:8080", "http://r.example"]) {
    assert.throws(() => Group.create(relay), /invalid relay URL/, relay);
  }
  // Nor from an old invite that names one.
  const invite = Group.create("https://r.example").toString();
  assert.throws(() => Group.parse(invite.replace("https://", "wss://")), /invalid relay URL/);
});

test("relay URLs map to a WebTransport session as in Rust (GROUP.md §3.6, §3.7)", () => {
  assert.deepEqual(webTransportFor("https://r.example:8443/p/g1/x"), { url: "https://r.example:8443/p/g1/x", weekly: false });
  // Raw QUIC is for native clients; JavaScript takes WebTransport, pinning the weekly certificate.
  assert.deepEqual(webTransportFor("quic://203.0.113.9:4433/g1/x"), { url: "https://203.0.113.9:4433/g1/x", weekly: true });
  assert.deepEqual(webTransportFor("quic://[2001:db8::1]:4433/g1/x"), { url: "https://[2001:db8::1]:4433/g1/x", weekly: true });
  assert.deepEqual(webTransportFor("quic://r.example/g1/x"), { url: "https://r.example/g1/x", weekly: true });
  for (const url of ["wss://r.example/g1/x", "ws://127.0.0.1:8080/g1/x", "http://r.example/g1/x", "nonsense", ""]) {
    assert.equal(webTransportFor(url), null, url);
  }
});

test("without WebTransport there is no joining, and no fallback", () => {
  assert.equal(groupsSupported(), false);
  const group = Group.create("https://r.example");
  for (const invite of [group, group.toString()]) {
    assert.throws(
      () => joinGroup(invite, { profile: "node" }),
      (e) => e instanceof DOMException && e.name === "NotSupportedError",
    );
  }
});

test("weekly certificate hashes follow the week", () => {
  const week = 7 * 24 * 3600 * 1000;
  const now = 2965 * week + 1000;
  const [last, current, next] = weeklyHashes(now);
  assert.equal(current.algorithm, "sha-256");
  assert.equal(current.value.length, 32);
  assert.deepEqual(weeklyHashes(now + week)[1], next);
  assert.deepEqual(weeklyHashes(now - week)[1], last);
  assert.notDeepEqual(current.value, next.value);
});
