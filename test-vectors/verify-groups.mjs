// An independent check of g1.json, written from GROUP.md alone with nothing
// but node:crypto: the group's keys and IDs, join tokens and JOIN
// signatures, KEY frames, and every sealed frame's plaintext, counter,
// nonce and ciphertext, byte for byte.
//
//   node test-vectors/verify-groups.mjs

import assert from "node:assert/strict";
import crypto from "node:crypto";
import { readFileSync } from "node:fs";

const v = JSON.parse(readFileSync(new URL("./g1.json", import.meta.url)));
const hex = (s) => Buffer.from(s, "hex");
const cat = (...parts) => Buffer.concat(parts.map((p) => Buffer.from(p)));
const ascii = (s) => Buffer.from(s, "ascii");
const hmac = (key, ...parts) => {
  const mac = crypto.createHmac("sha256", key);
  for (const p of parts) mac.update(p);
  return mac.digest();
};
const u64 = (n) => {
  const b = Buffer.alloc(8);
  b.writeBigUInt64BE(BigInt(n));
  return b;
};
const u32 = (n) => {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(n);
  return b;
};
const u16 = (n) => {
  const b = Buffer.alloc(2);
  b.writeUInt16BE(n);
  return b;
};

const ed25519 = (seed) =>
  crypto.createPrivateKey({ key: cat(hex("302e020100300506032b657004220420"), seed), format: "der", type: "pkcs8" });
const publicOf = (key) => crypto.createPublicKey(key).export({ format: "der", type: "spki" }).subarray(-32);
const sign = (key, message) => crypto.sign(null, message, key);

function chacha(key, nonce, ad, plaintext) {
  const cipher = crypto.createCipheriv("chacha20-poly1305", key, nonce, { authTagLength: 16 });
  cipher.setAAD(ad, { plaintextLength: plaintext.length });
  return cat(cipher.update(plaintext), cipher.final(), cipher.getAuthTag());
}

// §2.1 Group secret.
const S = hex(v.group_secret);
const groupSeed = hmac(S, ascii("peerconnect/g1 group"));
assert.deepEqual(groupSeed, hex(v.group_seed));
const groupKey = ed25519(groupSeed);
const groupId = publicOf(groupKey);
assert.deepEqual(groupId, hex(v.group_id));
assert.equal(groupId.toString("base64url"), v.group_id_text);
const keySecret = hmac(S, ascii("peerconnect/g1 keys"));
assert.deepEqual(keySecret, hex(v.key_secret));

// §2.2 Invite and §3.1 endpoint.
assert.equal(v.invite, `${v.relay_url}#${S.toString("base64url")}`);
assert.equal(Buffer.from(v.invite.split("#")[1], "base64url").length, 32);
assert.equal(v.endpoint, `${v.relay_url}/g1/${v.group_id_text}`);

// §3.2 Relay frames.
assert.deepEqual(hex(v.welcome_s), cat([0x83, 2], u32(1 << 0)));
assert.deepEqual(hex(v.welcome_r), cat([0x83, 0], u32(1 << 2)));
// SUBSCRIBE (slot, layer; 0xFF for none) and DEMAND (a bitmap of layers), §3.2.
assert.deepEqual(hex(v.subscribe_r), Buffer.from([0x85, 2, 1, 2]));
assert.deepEqual(hex(v.subscribe_r_none), Buffer.from([0x85, 2, 0, 0xff]));
assert.deepEqual(hex(v.demand_s), Buffer.from([0x86, 1, 0b110]));
// LIMIT: the largest layer the member's link takes now, 0xFF for none.
assert.deepEqual(hex(v.limit_r), Buffer.from([0x87, 1]));
assert.deepEqual(hex(v.limit_r_none), Buffer.from([0x87, 0xff]));
assert.deepEqual(hex(v.member_arrived), Buffer.from([0x84, 2, 1]));
assert.deepEqual(hex(v.member_left), Buffer.from([0x84, 2, 0]));

const BROADCAST = 0xff;
const members = {};
for (const [who, slot, to, echo] of [
  ["s", 2, BROADCAST, Buffer.alloc(16)],
  ["r", 0, 2, hex(v.s_challenge)],
]) {
  const seed = hex(v[`${who}_identity_seed`]);
  const identity = ed25519(seed);
  const id = publicOf(identity);
  assert.deepEqual(id, hex(v[`${who}_member_id`]));

  // §2.3 Join token, §3.1 JOIN.
  const token = hmac(seed, ascii("peerconnect/g1 token"), groupId).subarray(0, 16);
  assert.deepEqual(token, hex(v[`${who}_token`]));
  const join = cat([0x81], token, sign(groupKey, cat(ascii("peerconnect/g1 join"), hex(v.hello_nonce), token)));
  assert.deepEqual(join, hex(v[`${who}_join`]));

  // §4.1 KEY: S broadcasts, R answers S echoing its challenge. Neither has
  // sent a stream message yet.
  const body = cat(
    hex(v[`${who}_sender_key`]),
    hex(v[`${who}_challenge`]),
    echo,
    hex(v[`${who}_incarnation`]),
    u64(0),
    u64(0),
    ascii(v[`${who}_profile_text`]),
  );
  const signature = sign(identity, cat(ascii("peerconnect/g1 key"), groupId, [slot, to], body));
  const salt = hex(v[`${who}_key_salt`]);
  const header = Buffer.from([0x01, slot, to]);
  const k = hmac(keySecret, salt);
  const keyFrame = cat(header, salt, chacha(k, Buffer.alloc(12), header, cat(id, signature, body)));
  assert.deepEqual(keyFrame, hex(v[`${who}_key`]), `${who}_key`);
  members[slot] = { key: hex(v[`${who}_sender_key`]), counters: {} };
}

// §4.3, §5, §6: every sealed frame, rebuilt from its fields.
const CONTROL = 0x02, COMMAND = 0x03, VOICE = 0x21, DATAGRAM = 0x22, CHAT = 0x41, BULK = 0x61;
// Video fragments carry their track, layer and whether they are part of a keyframe in the type (§7.4).
const video = (f) => 0x30 | (Number(f.track) << 3) | (Number(f.layer) << 1) | (f.keyframe ? 1 : 0);
// §7.4: track[1] frame[4] index[1] count[1] flags[1] timestamp[4] ‖ data.
const fragment = (f) => cat([f.track], u32(f.frame), u16(f.index), u16(f.count), [f.keyframe ? 1 : 0], u32(f.timestamp), hex(f.data));
const message = (f) => cat([0x00], u64(f.id), ascii(f.payload));
const builders = {
  //            type      from to         plaintext
  confirm: (f) => [CONTROL, 2, 0, cat([0x00], hex(f.echo), u64(0), u64(0))],
  command_message: (f) => [COMMAND, 2, BROADCAST, message(f)],
  command_message_lost: (f) => [COMMAND, 2, BROADCAST, message(f)],
  chat_message: (f) => [CHAT, 2, BROADCAST, message(f)],
  command_direct: (f) => [COMMAND, 2, 0, cat([0x01], ascii(f.payload))],
  voice: (f) => [VOICE, 2, BROADCAST, cat(u16(f.seq), hex(f.audio))],
  datagram: (f) => [DATAGRAM, 2, BROADCAST, ascii(f.payload)],
  heads: (f) => [CONTROL, 2, BROADCAST, cat([0x02], u64(f.command_next), u64(f.chat_next))],
  resend: (f) => [CONTROL, 0, 2, cat([0x01, f.channel], hex(f.incarnation), u64(f.from))],
  resent_message: (f) => [COMMAND, 2, 0, message(f)],
  resent: (f) => [COMMAND, 2, 0, cat([0x02], u64(f.next))],
  sync_request: (f) => [CONTROL, 0, 2, cat([0x10], u32(f.transfer))],
  sync_request_declined: (f) => [CONTROL, 0, 2, cat([0x10], u32(f.transfer))],
  sync_begin: (f) => [
    CONTROL,
    2,
    0,
    cat([0x11], u32(f.transfer), u64(f.length), [f.frontier.length], ...f.frontier.map(([inc, next]) => cat(hex(inc), u64(next)))),
  ],
  bulk: (f) => [BULK, 2, 0, cat(u32(f.transfer), ascii(f.data))],
  sync_ack: (f) => [CONTROL, 0, 2, cat([0x12], u32(f.transfer), u64(f.received))],
  sync_abort: (f) => [CONTROL, 2, 0, cat([0x13], u32(f.transfer))],
  video_fragment: (f) => [video(f), 2, BROADCAST, fragment(f)],
  video_fragment_lost: (f) => [video(f), 2, BROADCAST, fragment(f)],
  video_nack: (f) => [CONTROL, 0, 2, cat([0x20, f.track, f.layer], u32(f.frame), ...f.indices.map(u16))],
  video_resent: (f) => [video(f), 2, 0, fragment(f)],
  video_delta: (f) => [video(f), 2, BROADCAST, fragment(f)],
  keyframe_request: (f) => [CONTROL, 0, 2, cat([0x21, f.track, f.layer])],
};

for (const frame of v.frames) {
  const [type, from, to, plaintext] = builders[frame.name](frame.fields);
  assert.deepEqual(plaintext, hex(frame.plaintext), `${frame.name} plaintext`);
  // Each type has its own counter on each connection, from 0.
  const sender = members[from];
  const counter = sender.counters[type] ?? 0;
  sender.counters[type] = counter + 1;
  const header = cat([type, from, to], u32(counter));
  const nonce = cat([type], Buffer.alloc(7), u32(counter));
  assert.deepEqual(cat(header, chacha(sender.key, nonce, header, plaintext)), hex(frame.frame), frame.name);
}

// A video frame is cut into fragments of at most 1,000 bytes of data (§7.4).
assert.equal(hex(v.frames.find((f) => f.name === "video_fragment").fields.data).length, 1000);
// The voice payload is the reference format: hold[1] ‖ Opus packet (§7.2).
assert.equal(hex(v.frames.find((f) => f.name === "voice").fields.audio)[0], 12);

console.log(`g1.json: all values and ${v.frames.length} frames reproduced independently`);

// The weekly certificate (GROUP.md §3.7): rebuilt from the spec, with the
// signature checked rather than recomputed (node:crypto signs ECDSA with a
// random nonce; the reference uses RFC 6979, which only makes it repeatable).
{
  const w = JSON.parse(readFileSync(new URL("./g1-certificate.json", import.meta.url)));
  const tlv = (tag, value) => {
    const n = value.length;
    const len = n < 0x80 ? [n] : n < 0x100 ? [0x81, n] : [0x82, n >> 8, n & 0xff];
    return cat([tag], len, value);
  };
  const seq = (...parts) => tlv(0x30, cat(...parts));
  const oid = (h) => tlv(0x06, hex(h));
  const integer = (bytes) => {
    let b = Buffer.from(bytes);
    while (b.length > 1 && b[0] === 0) b = b.subarray(1);
    return tlv(0x02, b[0] & 0x80 ? cat([0], b) : b);
  };
  const utc = (seconds) => tlv(0x17, ascii(new Date(seconds * 1000).toISOString().replace(/[-:T]/g, "").slice(2, 14) + "Z"));
  const ecdsaSha256 = seq(oid("2a8648ce3d040302"));
  const name = seq(tlv(0x31, seq(oid("550403"), tlv(0x0c, ascii("peerconnect")))));
  const WEEK = 604800;
  for (const { week, private_key, certificate, sha256 } of w.weeks) {
    const scalar = crypto.createHash("sha256").update(ascii(w.context)).update(u64(week)).digest();
    assert.deepEqual(scalar, hex(private_key), `week ${week}: key`);
    const ecdh = crypto.createECDH("prime256v1");
    ecdh.setPrivateKey(scalar);
    const point = ecdh.getPublicKey(); // uncompressed
    const start = week * WEEK;
    const tbs = seq(
      tlv(0xa0, integer([2])),
      integer(u64(week + 1)),
      ecdsaSha256,
      name,
      seq(utc(start - 86400), utc(start + 12 * 86400)),
      name,
      seq(seq(oid("2a8648ce3d0201"), oid("2a8648ce3d030107")), tlv(0x03, cat([0], point))),
      tlv(0xa3, seq(seq(oid("551d13"), tlv(0x04, seq())))),
    );
    const der = hex(certificate);
    // Certificate = SEQUENCE { tbs, algorithm, BIT STRING signature }.
    const header = der[1] === 0x82 ? 4 : der[1] === 0x81 ? 3 : 2;
    assert.deepEqual(der.subarray(header, header + tbs.length), tbs, `week ${week}: certificate body`);
    const rest = der.subarray(header + tbs.length);
    assert.deepEqual(rest.subarray(0, ecdsaSha256.length), ecdsaSha256, `week ${week}: signature algorithm`);
    const bits = rest.subarray(ecdsaSha256.length);
    assert.equal(bits[0], 0x03);
    const signature = bits.subarray(bits[1] === 0x81 ? 4 : 3);
    const key = crypto.createPublicKey({
      key: { kty: "EC", crv: "P-256", x: point.subarray(1, 33).toString("base64url"), y: point.subarray(33).toString("base64url") },
      format: "jwk",
    });
    assert.ok(crypto.verify("sha256", tbs, { key, dsaEncoding: "der" }, signature), `week ${week}: signature`);
    assert.equal(crypto.createHash("sha256").update(der).digest("hex"), sha256, `week ${week}: hash`);
    new crypto.X509Certificate(der); // and it parses as a certificate
  }
  console.log(`g1-certificate.json: ${w.weeks.length} weekly certificates rebuilt and verified independently`);
}
