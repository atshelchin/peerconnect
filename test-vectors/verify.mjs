// An independent check of v1.json, written from SPEC.md alone with nothing
// but node:crypto: room derivation, invite, JOIN signatures, the Noise
// IKpsk1 handshake and transport messages, byte for byte.
//
//   node test-vectors/verify.mjs

import assert from "node:assert/strict";
import crypto from "node:crypto";
import { readFileSync } from "node:fs";

const v = JSON.parse(readFileSync(new URL("./v1.json", import.meta.url)));
const hex = (s) => Buffer.from(s, "hex");
const cat = (...parts) => Buffer.concat(parts.map((p) => Buffer.from(p)));
const ascii = (s) => Buffer.from(s, "ascii");
const sha256 = (data) => crypto.createHash("sha256").update(data).digest();
const hmac = (key, ...parts) => {
  const mac = crypto.createHmac("sha256", key);
  for (const p of parts) mac.update(p);
  return mac.digest();
};

// Raw 32-byte keys wrapped in the fixed PKCS#8 / SPKI DER prefixes.
const der = (prefix, raw) => cat(hex(prefix), raw);
const ed25519Private = (seed) =>
  crypto.createPrivateKey({ key: der("302e020100300506032b657004220420", seed), format: "der", type: "pkcs8" });
const x25519Private = (raw) =>
  crypto.createPrivateKey({ key: der("302e020100300506032b656e04220420", raw), format: "der", type: "pkcs8" });
const x25519PublicKey = (raw) =>
  crypto.createPublicKey({ key: der("302a300506032b656e032100", raw), format: "der", type: "spki" });
const rawPublic = (privateKey) => crypto.createPublicKey(privateKey).export({ format: "der", type: "spki" }).subarray(-32);
const dh = (raw, publicRaw) => {
  const out = crypto.diffieHellman({ privateKey: x25519Private(raw), publicKey: x25519PublicKey(publicRaw) });
  assert.ok(out.some((b) => b !== 0), "all-zero DH output");
  return out;
};

// §3.1 Room secret and derived values.
const S = hex(v.room_secret);
const seed = hmac(S, ascii("peerconnect/v1 room"));
assert.deepEqual(seed, hex(v.room_seed));
const roomKey = ed25519Private(seed);
const roomId = rawPublic(roomKey);
assert.deepEqual(roomId, hex(v.room_id));
assert.equal(roomId.toString("base64url"), v.room_id_text);
const psk = hmac(S, ascii("peerconnect/v1 psk"));
assert.deepEqual(psk, hex(v.psk));
assert.equal(v.endpoint, `${v.relay_url}/v1/${v.room_id_text}`);

// §3.3 Invite.
const inviterPublic = rawPublic(x25519Private(hex(v.inviter_identity_private)));
assert.deepEqual(inviterPublic, hex(v.inviter_identity_public));
assert.equal(v.invite, `${v.relay_url}#${cat(S, inviterPublic).toString("base64url")}`);

// §4.3 HELLO and JOIN (Ed25519 signatures are deterministic).
const nonce = hex(v.hello_nonce);
assert.deepEqual(hex(v.hello_frame), cat([0x80], nonce));
for (const [role, frame] of [[0, v.join_frame_inviter], [1, v.join_frame_joiner]]) {
  const signed = cat(ascii("peerconnect/v1 join"), nonce, [role]);
  assert.deepEqual(hex(frame), cat([0x81, role], crypto.sign(null, signed, roomKey)));
  assert.ok(crypto.verify(null, signed, crypto.createPublicKey(roomKey), hex(frame).subarray(2)));
}

// §5.1 Noise_IKpsk1_25519_ChaChaPoly_SHA256, written out token by token.
class CipherState {
  constructor(k) {
    this.k = k;
    this.n = 0n;
  }
  nonce() {
    const nonce = Buffer.alloc(12);
    nonce.writeBigUInt64LE(this.n++, 4);
    return nonce;
  }
  encrypt(ad, plaintext) {
    const c = crypto.createCipheriv("chacha20-poly1305", this.k, this.nonce(), { authTagLength: 16 });
    c.setAAD(ad, { plaintextLength: plaintext.length });
    return cat(c.update(plaintext), c.final(), c.getAuthTag());
  }
  decrypt(ad, ciphertext) {
    const d = crypto.createDecipheriv("chacha20-poly1305", this.k, this.nonce(), { authTagLength: 16 });
    d.setAAD(ad, { plaintextLength: ciphertext.length - 16 });
    d.setAuthTag(ciphertext.subarray(-16));
    return cat(d.update(ciphertext.subarray(0, -16)), d.final());
  }
}

const hkdf = (ck, ikm, n) => {
  const prk = hmac(ck, ikm);
  const out = [];
  for (let i = 1, prev = Buffer.alloc(0); i <= n; i++) out.push((prev = hmac(prk, prev, Buffer.from([i]))));
  return out;
};

class SymmetricState {
  constructor() {
    this.h = sha256(ascii("Noise_IKpsk1_25519_ChaChaPoly_SHA256"));
    this.ck = this.h;
    this.mixHash(ascii("peerconnect/v1"));
  }
  mixHash(data) {
    this.h = sha256(cat(this.h, data));
  }
  mixKey(ikm) {
    const [ck, k] = hkdf(this.ck, ikm, 2);
    this.ck = ck;
    this.cipher = new CipherState(k);
  }
  mixKeyAndHash(ikm) {
    const [ck, h, k] = hkdf(this.ck, ikm, 3);
    this.ck = ck;
    this.mixHash(h);
    this.cipher = new CipherState(k);
  }
  encryptAndHash(plaintext) {
    const c = this.cipher.encrypt(this.h, plaintext);
    this.mixHash(c);
    return c;
  }
  decryptAndHash(c) {
    const p = this.cipher.decrypt(this.h, c);
    this.mixHash(c);
    return p;
  }
  split() {
    return hkdf(this.ck, Buffer.alloc(0), 2).map((k) => new CipherState(k));
  }
}

const joinerS = hex(v.joiner_identity_private);
const joinerE = hex(v.joiner_ephemeral_private);
const inviterS = hex(v.inviter_identity_private);
const inviterE = hex(v.inviter_ephemeral_private);
const joinerPublic = rawPublic(x25519Private(joinerS));
assert.deepEqual(joinerPublic, hex(v.joiner_identity_public));

// Joiner writes message 1:  -> e, es, s, ss, psk
const i = new SymmetricState();
i.mixHash(inviterPublic); // pre-message <- s
const ie = rawPublic(x25519Private(joinerE));
i.mixHash(ie);
i.mixKey(ie); // psk handshakes also MixKey the ephemeral
i.mixKey(dh(joinerE, inviterPublic));
const encryptedStatic = i.encryptAndHash(joinerPublic);
i.mixKey(dh(joinerS, inviterPublic));
i.mixKeyAndHash(psk);
const message1 = cat(ie, encryptedStatic, i.encryptAndHash(Buffer.alloc(0)));
assert.deepEqual(hex(v.init_frame), cat([0x01], message1), "INIT");

// Inviter reads message 1 and writes message 2:  <- e, ee, se
const r = new SymmetricState();
r.mixHash(inviterPublic);
const re = message1.subarray(0, 32);
r.mixHash(re);
r.mixKey(re);
r.mixKey(dh(inviterS, re));
const rs = r.decryptAndHash(message1.subarray(32, 80));
assert.deepEqual(rs, joinerPublic, "the inviter learns the joiner's identity");
r.mixKey(dh(inviterS, rs));
r.mixKeyAndHash(psk);
assert.equal(r.decryptAndHash(message1.subarray(80)).length, 0);
const ee = rawPublic(x25519Private(inviterE));
r.mixHash(ee);
r.mixKey(ee);
r.mixKey(dh(inviterE, re));
r.mixKey(dh(inviterE, rs));
const message2 = cat(ee, r.encryptAndHash(Buffer.alloc(0)));
assert.deepEqual(hex(v.reply_frame), cat([0x02], message2), "REPLY");

// Joiner reads message 2.
i.mixHash(ee);
i.mixKey(ee);
i.mixKey(dh(joinerE, ee));
i.mixKey(dh(joinerS, ee));
assert.equal(i.decryptAndHash(message2.subarray(32)).length, 0);
assert.deepEqual(i.h, r.h, "handshake hashes agree");

// §5.3 Transport: kind byte, chunks of at most 65,518 bytes.
const [joinerToInviter, inviterToJoiner] = i.split();
const sending = { "joiner->inviter": joinerToInviter, "inviter->joiner": inviterToJoiner };
const [inviterReceives, joinerReceives] = r.split();
const receiving = { "joiner->inviter": inviterReceives, "inviter->joiner": joinerReceives };

for (const m of v.messages) {
  let data;
  let kinds;
  if (m.message === "CLOSE") {
    data = [Buffer.alloc(0)];
    kinds = [0x02];
  } else {
    const whole = m.message.startsWith("utf8:")
      ? Buffer.from(m.message.slice(5))
      : Buffer.from(Array.from({ length: 100_000 }, (_, i) => i % 251));
    data = [];
    for (let at = 0; at < whole.length || data.length === 0; at += 65518) data.push(whole.subarray(at, at + 65518));
    kinds = data.map((_, i) => (i === data.length - 1 ? 0x00 : 0x01));
  }
  const frames = data.map((chunk, n) => cat([0x03], sending[m.direction].encrypt(Buffer.alloc(0), cat([kinds[n]], chunk))));
  if (m.frames) assert.deepEqual(frames.map((f) => f.toString("hex")), m.frames, m.message);
  else assert.deepEqual(frames.map((f) => sha256(f).toString("hex")), m.frames_sha256, m.message);
  for (const [n, f] of frames.entries()) {
    assert.deepEqual(receiving[m.direction].decrypt(Buffer.alloc(0), f.subarray(1)), cat([kinds[n]], data[n]));
  }
}

console.log("v1.json: all values reproduced independently");
