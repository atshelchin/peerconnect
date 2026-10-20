# PeerConnect Protocol, version 1

Two peers, one room, end-to-end encryption over an untrusted, stateless WebSocket relay.

Status: stable. Reference implementation: this repository. Test vectors: [`test-vectors/v1.json`](test-vectors/v1.json).

## 1. Overview

PeerConnect lets two applications talk securely through a relay that neither of them needs to trust. One peer, the **inviter**, creates a room and hands an **invite** to the other, the **joiner**, over some out-of-band channel (QR code, deep link, pasted text). From then on both peers can reconnect to the room at any time, from any network, and exchange messages that only they can read.

The relay:

- forwards opaque frames between the two connections of a room;
- admits a connection only if it proves it holds the room key, without learning that key;
- keeps no state beyond its open connections. It can restart at any time; peers reconnect and carry on.

The peers:

- authenticate each other and agree on session keys with the Noise handshake `IKpsk1`;
- pin each other's identity on first contact, so that only the original two peers can ever use the room again, even if the invite later leaks;
- encrypt every message with ChaCha20-Poly1305, so the relay can drop, delay or cut connections, but cannot read, forge, replay or reorder messages without the peers noticing.

PeerConnect does not define what messages mean, does not store messages for offline peers, and does not support more than two peers per room.

### 1.1 Terminology

| Term | Meaning |
|---|---|
| Peer | One of the two endpoints of a room. |
| Inviter | The peer that created the room. Holds seat 0. Noise responder. |
| Joiner | The peer that accepted the invite. Holds seat 1. Noise initiator. |
| Relay | A WebSocket server that admits peers to rooms and forwards their frames. |
| Room | The relationship between two peers, identified by its room ID and defined by its room secret. It is not an object on the relay. |
| Seat | One of the two places in a room on a relay: 0 for the inviter, 1 for the joiner. |
| Session | A handshake and the transport keys it produced. Lasts as long as both peers' current connections. |
| Message | An application payload of 0 to 16 MiB. |

The keywords MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are to be interpreted as described in RFC 2119.

## 2. Conventions

- `a ‖ b` is byte concatenation. Multi-byte integers are big-endian unless stated otherwise.
- `"text"` is the ASCII bytes of the string, without a terminator.
- **base64url** is the unpadded URL-safe alphabet of RFC 4648 §5. Decoders MUST reject padding, characters outside the alphabet and non-zero trailing bits, so that every value has exactly one textual form.
- Primitives:
  - X25519 (RFC 7748). Implementations MUST reject an all-zero shared secret.
  - Ed25519 (RFC 8032), keys derived from 32-byte seeds.
  - ChaCha20-Poly1305 (RFC 8439).
  - SHA-256 and HMAC-SHA256.

## 3. Rooms

### 3.1 Room secret

The inviter generates the **room secret** `S`: 32 bytes from a cryptographically secure random number generator. Everything else about the room is derived from it:

```
room_seed = HMAC-SHA256(key = S, "peerconnect/v1 room")
room_key  = the Ed25519 key pair with seed room_seed
room_id   = the room_key public key (32 bytes)
psk       = HMAC-SHA256(key = S, "peerconnect/v1 psk")
```

The text form of a room ID is its base64url encoding (43 characters).

The room ID tells a relay *which* room; only a holder of `S` can prove it may *enter* the room (§4.3). Relays and anyone who sees the ID learn nothing that lets them join.

### 3.2 Identities

Each peer has a static X25519 key pair, its **identity** in the room. Implementations SHOULD generate a fresh identity for every room, so that rooms cannot be linked to each other by key; they MAY reuse one.

The joiner learns the inviter's identity public key from the invite. The inviter learns the joiner's from the first successful handshake and **pins** it (§5.2).

### 3.3 Invite

```
invite = relay_url "#" base64url(S ‖ inviter_identity_public_key)
```

for example `wss://relay.example.com#q3V0…` (86 characters after `#`).

- `relay_url` is a `ws://` or `wss://` URL with a host, without query, fragment or trailing slash. Production deployments SHOULD use `wss://`.
- The fragment MUST decode to exactly 64 bytes. (Future versions will use a fragment that does not, so that version 1 implementations reject them rather than misread them.)

An invite is a bearer credential. Anyone who holds it can pair with the inviter if they do so first, and can take the joiner's seat on the relay at any time (§8.3). It MUST be transferred over a channel that keeps it confidential and authentic, and SHOULD be discarded once the room is paired.

### 3.4 What a peer stores

To reconnect, a peer needs and MUST keep:

- the relay URL;
- the room secret `S` (from which the room ID, room key and psk follow);
- its role (inviter or joiner);
- its identity private key;
- the other peer's identity public key, once known.

The joiner MUST persist its record before its first handshake: its identity is pinned from then on, and accepting the same invite again would produce a different identity, which the inviter refuses. The inviter's record changes exactly once, when it pins the joiner's key; it MUST be persisted again at that point. These values are secrets: whoever obtains them can act as that peer.

The relay URL is not bound into any cryptographic value: both peers may move a room to another relay by agreeing on a new URL.

## 4. Relay protocol

### 4.1 Endpoint

A peer opens a WebSocket (RFC 6455) to

```
relay_url "/v1/" room_id
```

where `room_id` is the text form. A relay MUST accept such a path below any prefix (so `wss://example.com/peerconnect/v1/<id>` works when the relay is mounted at `/peerconnect`). It MUST refuse the upgrade, with HTTP 404, if the last segment is not a valid Ed25519 public key in canonical base64url.

### 4.2 Frames

Each binary WebSocket message is one **frame**: a type byte followed by a body. Frames are at most **65,536 bytes**. The keepalive uses text messages; no other text messages are allowed.

| Type | Name | Direction | Body |
|---|---|---|---|
| `0x01` | INIT | peer → peer | Noise handshake message 1 |
| `0x02` | REPLY | peer → peer | Noise handshake message 2 |
| `0x03` | MESSAGE | peer → peer | Noise transport message |
| `0x80` | HELLO | relay → peer | `nonce[32]` |
| `0x81` | JOIN | peer → relay | `role[1] ‖ signature[64]` |
| `0x82` | PRESENCE | relay → peer | `present[1]`: `0x00` or `0x01` |
| text `ping` | | peer → relay | |
| text `pong` | | relay → peer | |

Types `0x00`–`0x7F` are **peer frames**. The relay never interprets them beyond the type byte; it forwards them unchanged (§4.5). Types `0x80`–`0xFF` are **relay frames**.

### 4.3 Joining

1. On accepting the WebSocket, the relay sends HELLO with a fresh random 32-byte `nonce`. HELLO is always the first frame.
2. The peer answers with JOIN, where `role` is `0x00` (inviter) or `0x01` (joiner) and

   ```
   signature = Ed25519-Sign(room_key, "peerconnect/v1 join" ‖ nonce ‖ role)
   ```

3. The relay verifies the signature against the room ID from the URL, with strict verification (canonical encodings, no small-order points). On failure it closes the connection with code 4401. A malformed JOIN, or any other frame before JOIN, is closed with 1002. A connection that has not sent a valid JOIN within **10 s** of HELLO is closed with 4408.
4. On success the connection takes the seat `role` (§4.4), and the relay sends it PRESENCE.

Before joining, a peer may send only `ping` and JOIN.

### 4.4 Seats and presence

A room on a relay has two seats. Each holds at most one joined connection.

- When a connection joins a seat that is already held, the relay closes the older connection with code **4409 (replaced)**. The newest connection always wins: a peer that reconnects after a network change is never locked out by its own dead connection.
- After a successful JOIN, the relay sends the new connection exactly one PRESENCE: `0x01` if the other seat is held, `0x00` otherwise.
- If the other seat is held, the relay sends that connection PRESENCE `0x01`, *including when the joining connection replaced an earlier one*. PRESENCE `0x01` therefore always means "a new connection now holds the other seat"; any session with the previous one is over.
- When a joined connection ends for any reason other than being replaced, the relay sends PRESENCE `0x00` to the other seat, if held.

Ordering: a connection receives nothing before its own PRESENCE, and it receives PRESENCE `0x01` for a partner before any frame that partner sends. A relay MUST NOT deliver to a connection any frame sent by a partner connection that was replaced before the frame was forwarded.

The relay's entire state for a room is these two seats. It MUST NOT need anything else to admit a connection: rooms appear when a connection joins and disappear when the last one leaves.

### 4.5 Forwarding

A peer frame from a joined connection is forwarded unchanged to the connection in the other seat, in order. If the other seat is empty the frame is dropped; the relay does not queue frames for absent peers.

A relay closes a joined connection that sends a relay frame, an empty frame or a text message other than `ping`, with 1002; one that sends a frame over 65,536 bytes, with 1009.

A slow or stalled reader must not make the relay buffer without bound. A relay SHOULD bound what it buffers per connection and, when a partner is not keeping up, either apply backpressure (stop reading from the sender until there is room) or close the slow connection. A relay that applies backpressure MUST NOT wait indefinitely: a connection that accepts nothing forwarded to it for **30 s** is not reading, and the relay closes it with 4408 (and tells its partner with PRESENCE `0x00`), so that one stalled peer cannot hold the other hostage. A relay on a platform that buffers outgoing messages itself and does not report how much (Cloudflare Workers, for one) MUST at least confine the consequences to the room concerned: the Cloudflare implementation runs every room in its own Durable Object, whose memory limit then resets only that room.

### 4.6 Keepalive

A peer MUST send the text message `ping` every **25 s** while connected, whatever else it sends; it SHOULD send it ahead of any data it has queued. The relay answers each with `pong`. Both serve to detect connections that died without closing:

- A relay MAY close, with 4408, a joined connection from which it has received nothing for **90 s**, and SHOULD then send PRESENCE `0x00` to its partner.
- A peer SHOULD treat a connection on which it has received nothing for **60 s** as dead.
- A relay that applies backpressure may leave a peer's `ping` unread behind data for a long time. Such a relay MUST send `pong` unprompted to any connection it has sent nothing to for **30 s**, so that a peer whose upload is merely slow is not mistaken for a dead one. Peers treat every `pong` alike.

(The fixed `ping`/`pong` text lets relays answer keepalives without waking up; see the Cloudflare implementation.)

### 4.7 Close codes

| Code | Meaning |
|---|---|
| 1000 | Normal closure. |
| 1001 | The relay is going away (restart, deploy). |
| 1002 | Protocol violation. |
| 1009 | Frame too large. |
| 4401 | JOIN signature did not verify. |
| 4408 | No JOIN in time, idle for too long, or not reading what is forwarded. |
| 4409 | Replaced by a newer connection to the same seat. |

## 5. Peer protocol

### 5.1 Handshake

Peers run the Noise Protocol Framework (revision 34) handshake

```
Noise_IKpsk1_25519_ChaChaPoly_SHA256

  <- s
  ...
  -> e, es, s, ss, psk
  <- e, ee, se
```

with:

- **prologue**: `"peerconnect/v1"`;
- **initiator**: the joiner; **responder**: the inviter;
- **pre-message**: the inviter's identity public key;
- **psk**: the room's `psk` (§3.1);
- **payloads**: both empty. Receivers MUST ignore payload contents, which future revisions may use.

The joiner sends handshake message 1 in an INIT frame; the inviter replies with message 2 in a REPLY frame. Both then hold a pair of transport cipher states from `Split()`: the first encrypts joiner → inviter, the second inviter → joiner.

This one handshake serves both the first contact and every reconnection. It authenticates both identities, requires knowledge of the room secret, binds the session to the room, and gives forward secrecy from the ephemeral keys. The joiner's identity travels encrypted, so the relay never sees either identity key.

### 5.2 When handshakes happen, and pinning

On every PRESENCE, a peer discards any session it has and any partially received message.

- On PRESENCE `0x01`, the **joiner** starts a new handshake and sends INIT. The **inviter** waits for INIT.
- On PRESENCE `0x00`, both wait.

The inviter processes INIT whenever the other seat is held, even if it already has a session (the new handshake replaces it). On a successful read of message 1 it obtains the joiner's identity public key `rs` and:

- if it has not pinned a key yet, pins `rs` (this is **pairing**, and happens once per room) and persists its record;
- if it has pinned a key and `rs` differs, ignores the INIT entirely;
- otherwise proceeds.

It then sends REPLY. The inviter's session is established once REPLY is sent; the joiner's, once REPLY is read.

A handshake message that fails to decrypt, or arrives when not expected, MUST be ignored: it is either stale (sent to a connection that has since been replaced) or forged. A joiner that has not received a valid REPLY within **10 s** of sending INIT SHOULD close its connection and reconnect (§6), which gives both sides a fresh PRESENCE.

Because the joiner's identity is fixed before it first sends INIT and pinning is idempotent, pairing needs no confirmation round: if REPLY is lost, the joiner simply sends INIT again from the same identity.

### 5.3 Transport messages

Each MESSAGE frame body is one Noise transport message (at most 65,535 bytes), encrypted with the sender's cipher state and empty associated data. Noise's implicit nonces make every message's position part of its authentication: a dropped, duplicated, reordered or modified message fails to decrypt.

The plaintext is a kind byte followed by data:

| Kind | Name | Data |
|---|---|---|
| `0x00` | FINAL | the last (or only) chunk of a message |
| `0x01` | MORE | a chunk of a message, more follow |
| `0x02` | CLOSE | empty: the sender has ended the room (§5.4) |

A message is sent as zero or more MORE chunks followed by one FINAL chunk, each carrying 0 to 65,518 bytes (the largest plaintext, 65,519, minus the kind byte). An empty message is a single empty FINAL. The receiver concatenates chunks and delivers the message on FINAL.

Messages are at most **16 MiB** (16,777,216 bytes). A receiver whose reassembled message would exceed that MUST treat it as a session failure. Senders MUST NOT send larger messages. Receivers MUST ignore plaintexts with an unknown kind, or with no kind byte.

A MESSAGE that arrives without an established session is stale; ignore it. A MESSAGE that fails to decrypt within a session means the relay did not deliver the stream faithfully; the session is unusable, and the peer MUST close its connection and reconnect.

Messages are delivered only while both peers are connected. A message sent while the partner is absent is lost, and there is no acknowledgement; applications that need delivery guarantees build them on top.

### 5.4 Ending a room

A peer ends a room by sending CLOSE in its session, then closing its connection with 1000. On receiving CLOSE, a peer closes its connection, stops reconnecting, and reports the room as ended. Both peers then SHOULD delete their records.

CLOSE reaches the other peer only if it is connected at that moment. The relay keeps no state, so a peer that was offline is never told; it will keep finding the other seat empty. Applications that care should time out on their own.

Leaving a room for now, without ending it, is simply closing the WebSocket with 1000; the partner sees PRESENCE `0x00`.

## 6. Connection lifecycle

A peer keeps at most one connection to the relay per room and runs this loop:

1. Connect to the endpoint (§4.1), join (§4.3).
2. Follow PRESENCE, handshake (§5.2) and exchange messages (§5.3), sending `ping` every 25 s.
3. When the connection closes, fails, or must be abandoned (no frame for 60 s, no REPLY within 10 s, a MESSAGE that fails to decrypt, a relay protocol violation), wait and go to 1.

Reconnection SHOULD use exponential backoff with jitter: the reference implementation waits a uniformly random time between half and all of `min(30 s, 0.5 s × 2^n)`, where `n` counts the attempts since a connection last stayed joined for at least 30 s. (Resetting on anything earlier, such as a successful JOIN, would let a fault that strikes right after joining cause a tight reconnection loop.)

A peer whose connection is closed with **4409** reports this to the application and MUST wait at least **30 s** (the reference implementation: 60 to 70 s) before reconnecting. The usual cause is the same room in use elsewhere, on another device, tab or process; the wait stops two instances from evicting each other in a loop, and the application can let one of them leave. A peer does not give up for good: a 4409 can also come from a relay, or from someone holding the room secret (§8.3), and must not turn a moment's interference into a lasting outage.

## 7. Limits and timers

| Name | Value | Enforced by |
|---|---|---|
| Frame size | ≤ 65,536 bytes | relay (1009) |
| Message size | ≤ 16 MiB | peers |
| `ping` interval | 25 s | peers send |
| Join timeout | 10 s | relay (4408) |
| Idle timeout | 90 s without any frame | relay (4408), optional |
| Stall timeout | 30 s without accepting forwarded data | relay (4408) |
| Relay keepalive | `pong` after 30 s without sending | relays that apply backpressure |
| Wait after 4409 | at least 30 s | peers |
| Receive timeout | 60 s without any frame | peers |
| Handshake timeout | 10 s for REPLY | joiner |

## 8. Security considerations

### 8.1 Threat model

The relay, and the network between it and the peers, are untrusted. PeerConnect guarantees, against them:

- **Confidentiality** of messages.
- **Integrity and authenticity**: a peer accepts a message only if the other peer sent it, in that session, in that position. Replays and reordering are detected.
- **Mutual authentication**: after pairing, only the two pinned identities can establish sessions in the room.
- **Forward secrecy**: compromising identity keys or the room secret later does not reveal past sessions' messages.
- **Identity hiding**: identity keys never appear on the wire in the clear.

It does not guarantee availability. A relay can refuse service, drop or delay frames, and cut connections; peers detect this (as failed decryption, timeouts or closed connections) but cannot prevent it. A peer's own device is trusted: whoever extracts a peer's record can impersonate that peer.

### 8.2 What the relay learns

Room IDs, connection times and network addresses, frame sizes and timing, which seat each connection holds, and when sessions are (re)established. Not message contents, not identity keys, not the room secret. Room IDs are unlinkable to each other and, with per-room identities, so are rooms belonging to the same peer.

### 8.3 Invites

The invite carries the room secret and the inviter's identity.

- **Before pairing**, whoever presents the invite first pairs with the inviter. If an attacker races the intended joiner, the joiner's handshakes are ignored and it never connects, so the race is visible, but it is not prevented. Applications for which this matters can have users compare a short fingerprint of both identity keys (available from both peers' records) out of band.
- **After pairing**, a leaked invite gives no access to messages and does not allow impersonating either peer: the inviter only accepts the pinned joiner, and the joiner only the inviter's key. It does give relay access: whoever holds the room secret can sign a JOIN for *either* seat, evicting the peer there (the relay cannot tell the difference without storing state). That is a denial of service, of the same kind the relay itself can always mount. Evicted peers see close code 4409, report it, and come back after their wait (§6), so the interference lasts only as long as the attacker keeps it up, and no session is ever established with the intruder.

A stateless relay cannot make an invite single-use; that would require remembering it was used. PeerConnect instead makes a used invite harmless to confidentiality and authenticity, and limits what remains to availability. Treat invites as secrets and discard them once paired.

### 8.4 Handshake properties

`IKpsk1` gives the joiner explicit authentication of the inviter on receipt of REPLY, and the inviter authentication of the joiner on receipt of INIT (through `ss` and the psk). INIT can be replayed by the relay; a replayed INIT only makes the inviter run a handshake that nobody can complete, after which the real joiner's next INIT restores the session. The joiner's identity is protected against passive observers and active attackers who do not hold the inviter's identity key.

Both peers require knowledge of `S` (through the psk) in addition to their identity keys, so an attacker needs both a peer's identity key *and* the room secret to impersonate it.

### 8.5 Session lifetime

Transport keys live as long as a session, which ends whenever either connection ends. There is no in-session rekeying: a session that lasts for days keeps its keys for days. Nonces are 64-bit counters and never wrap in practice.

### 8.6 Denial of service

Relays bound frame size, unauthenticated connection time, idle time, and buffered data per connection (closing connections that stop reading, §4.5). Connections that have not joined yet SHOULD be limited per client address, so that knowing a room ID (which grants nothing else) is not enough to crowd out a room's peers. Relay operators SHOULD additionally limit connections per address and overall. Ed25519 verification costs the relay one signature check per JOIN, and an unauthenticated connection can send only a single JOIN before being closed or seated.

Flow control is end to end: a peer that sends faster than its partner reads is held back by the relay, never the other way round, and neither peer can stall the other for longer than the stall timeout. Implementations MUST keep reading from the relay while they wait to write to it; otherwise two peers sending large messages to each other at once can deadlock.

## 9. Conformance

[`test-vectors/v1.json`](test-vectors/v1.json) gives, for fixed inputs, every derived value and every byte on the wire: room ID, psk, invite, JOIN signature, both handshake messages and transport messages in both directions. An implementation that reproduces them, and passes the behaviour above, interoperates with this one.
