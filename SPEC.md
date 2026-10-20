# PeerConnect Protocol, version 1

Two peers, one room, end-to-end encryption and reliable delivery over an untrusted, stateless WebSocket relay.

Status: draft for review; the wire format may still change before it is declared stable. Reference implementation: this repository. Test vectors: [`test-vectors/v1.json`](test-vectors/v1.json).

## 1. Overview

PeerConnect lets two applications talk securely through a relay that neither of them needs to trust. One peer, the **inviter**, creates a room and hands an **invite** to the other, the **joiner**, over some out-of-band channel (QR code, deep link, pasted text). From then on both peers can reconnect to the room at any time, from any network, and exchange messages that only they can read.

The relay:

- forwards opaque frames between the two connections of a room;
- admits a connection only if it proves it holds the room's key, without learning that key;
- keeps no state beyond its open connections. It can restart at any time; peers reconnect and carry on.

The peers:

- authenticate each other and agree on session keys with the Noise handshake `IKpsk1`;
- pin each other's identity on first contact, then move the room to a **pair ID** that only the two of them can compute, so that once paired, a leaked invite reaches nobody;
- carry a reliable, ordered stream of messages across any number of connections: messages are numbered, acknowledged, kept by the sender until acknowledged and resent after a reconnection, so none is lost or duplicated while both peers keep running;
- encrypt every message with ChaCha20-Poly1305, so the relay can drop, delay or cut connections, but cannot read, forge, replay or reorder messages without the peers noticing.

PeerConnect does not define what messages mean, keeps nothing on the relay, and supports exactly two peers per room.

### 1.1 Terminology

| Term | Meaning |
|---|---|
| Peer | One of the two endpoints of a room. |
| Inviter | The peer that created the room. Holds seat 0. Noise responder. |
| Joiner | The peer that accepted the invite. Holds seat 1. Noise initiator. |
| Relay | A WebSocket server that admits peers to rooms and forwards their frames. |
| Room | The relationship between two peers, defined by its room secret and their identities. It is not an object on the relay. |
| Room ID | Where on a relay the peers of a room meet. A room has two: its invite ID and its pair ID (§3.1). |
| Seat | One of the two places at a room ID on a relay: 0 for the inviter, 1 for the joiner. |
| Session | A handshake and the transport keys it produced. Lasts as long as both peers' current connections. |
| Stream | The sequence of messages one peer sends to the other, numbered from 0, across sessions. |
| Message | An application payload of 0 to 16 MiB. |

The keywords MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are to be interpreted as described in RFC 2119.

## 2. Conventions

- `a ‖ b` is byte concatenation. Multi-byte integers are big-endian unless stated otherwise.
- `"text"` is the ASCII bytes of the string, without a terminator.
- **base64url** is the unpadded URL-safe alphabet of RFC 4648 §5. Decoders MUST reject padding, characters outside the alphabet and non-zero trailing bits, so that every value has exactly one textual form.
- Primitives:
  - X25519 (RFC 7748). Implementations MUST reject an all-zero shared secret, and public keys that produce one.
  - Ed25519 (RFC 8032), keys derived from 32-byte seeds.
  - ChaCha20-Poly1305 (RFC 8439).
  - SHA-256 and HMAC-SHA256.

## 3. Rooms

### 3.1 Room secret and room IDs

The inviter generates the **room secret** `S`: 32 bytes from a cryptographically secure random number generator. Each peer also has a static X25519 key pair, its **identity** (§3.2). From these:

```
psk         = HMAC-SHA256(key = S, "peerconnect/v1 psk")

invite_seed = HMAC-SHA256(key = S, "peerconnect/v1 room")
invite_key  = the Ed25519 key pair with seed invite_seed
invite_id   = the invite_key public key (32 bytes)

shared      = X25519(own identity private key, other identity public key)
pair_seed   = HMAC-SHA256(key = S, "peerconnect/v1 pair" ‖ shared)
pair_key    = the Ed25519 key pair with seed pair_seed
pair_id     = the pair_key public key (32 bytes)
```

Both peers compute the same `shared`, and nobody else can: it needs one of the two identity private keys. The text form of a room ID is its base64url encoding (43 characters).

A room ID tells a relay *which* room; only a holder of the matching key can prove it may *enter* it (§4.3). Whoever sees an ID learns nothing that lets them join. The invite ID is used until the room is paired; the pair ID afterwards (§6.2).

### 3.2 Identities

Each peer has a static X25519 key pair, its **identity** in the room. Implementations SHOULD generate a fresh identity for every room, so that rooms cannot be linked to each other by key; they MAY reuse one.

The joiner learns the inviter's identity public key from the invite. The inviter learns the joiner's from the first handshake, and **pins** it once pairing completes (§5.2).

### 3.3 Invite

```
invite = relay_url "#" base64url(S ‖ inviter_identity_public_key)
```

for example `wss://relay.example.com#q3V0…` (86 characters after `#`).

- `relay_url` is a `ws://` or `wss://` URL with a host, without query, fragment or trailing slash. Production deployments SHOULD use `wss://`.
- The fragment MUST decode to exactly 64 bytes. (Future versions will use a fragment that does not, so that version 1 implementations reject them rather than misread them.)

Until the room is paired the invite is a bearer credential: whoever presents it first pairs with the inviter (§8.3). It MUST be transferred over a channel that keeps it confidential and authentic. Once the room is paired it is useless.

### 3.4 What a peer stores

To reconnect, a peer needs and MUST keep its **room record**:

- the relay URL;
- the room secret `S`;
- its role (inviter or joiner);
- its identity private key;
- the other peer's identity public key: from the start for a joiner, once paired for an inviter;
- whether pairing is complete;
- whether the room is being closed (§5.5).

The joiner MUST persist its record before its first handshake: accepting the same invite again would produce a different identity, which the inviter refuses once paired. Each peer MUST persist its record again when pairing completes and when it starts closing the room. These values are secrets: whoever obtains them can act as that peer.

The relay URL is not bound into any cryptographic value: both peers may move a room to another relay by agreeing on a new URL.

Messages awaiting acknowledgement are kept in memory only. A peer that stops (leaves, or its process ends) loses those it had not yet had acknowledged; applications that need delivery across restarts keep their own copies until the other peer acknowledges them (§5.3).

## 4. Relay protocol

### 4.1 Endpoint

A peer opens a WebSocket (RFC 6455) to

```
relay_url "/v1/" room_id
```

where `room_id` is the text form. A relay MUST accept such a path below any prefix (so `wss://example.com/peerconnect/v1/<id>` works when the relay is mounted at `/peerconnect`). It MUST refuse the upgrade, with HTTP 404, if the last segment is not a valid Ed25519 public key in canonical base64url. The relay cannot tell an invite ID from a pair ID, and does not need to.

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
   signature = Ed25519-Sign(room ID's key, "peerconnect/v1 join" ‖ nonce ‖ role)
   ```

   with `invite_key` at the invite ID and `pair_key` at the pair ID.
3. The relay verifies the signature against the room ID from the URL, with strict verification (canonical encodings, no small-order points). On failure it closes the connection with code 4401. A malformed JOIN, or any other frame before JOIN, is closed with 1002. A connection that has not sent a valid JOIN within **10 s** of HELLO is closed with 4408.
4. On success the connection takes the seat `role` (§4.4), and the relay sends it PRESENCE.

Before joining, a peer may send only `ping` and JOIN.

### 4.4 Seats and presence

A room ID on a relay has two seats. Each holds at most one joined connection.

- When a connection joins a seat that is already held, the relay closes the older connection with code **4409 (replaced)**. The newest connection always wins: a peer that reconnects after a network change is never locked out by its own dead connection.
- After a successful JOIN, the relay sends the new connection exactly one PRESENCE: `0x01` if the other seat is held, `0x00` otherwise.
- If the other seat is held, the relay sends that connection PRESENCE `0x01`, *including when the joining connection replaced an earlier one*. PRESENCE `0x01` therefore always means "a new connection now holds the other seat"; any session with the previous one is over.
- When a joined connection ends for any reason other than being replaced, the relay sends PRESENCE `0x00` to the other seat, if held.

Ordering: a connection receives nothing before its own PRESENCE, and it receives PRESENCE `0x01` for a partner before any frame that partner sends. A relay MUST NOT deliver to a connection any frame sent by a partner connection that was replaced before the frame was forwarded.

The relay's entire state for a room ID is these two seats. It MUST NOT need anything else to admit a connection: room IDs appear when a connection joins and disappear when the last one leaves.

### 4.5 Forwarding

A peer frame from a joined connection is forwarded unchanged to the connection in the other seat, in order. If the other seat is empty the frame is dropped; the relay does not queue frames for absent peers. (Peers do: §5.3.)

A relay closes a joined connection that sends a relay frame, an empty frame or a text message other than `ping`, with 1002; one that sends a frame over 65,536 bytes, with 1009.

Peers bound what they have in flight (§5.3), so honest peers never make a relay buffer more than about a window per direction. Against dishonest ones, a relay SHOULD bound what it buffers per connection and, when a partner is not keeping up, either apply backpressure (stop reading from the sender until there is room) or close the slow connection. A relay that applies backpressure MUST NOT wait indefinitely: a connection that accepts nothing forwarded to it for **30 s** is not reading, and the relay closes it with 4408 (and tells its partner with PRESENCE `0x00`). A relay on a platform that buffers outgoing messages itself and does not report how much (Cloudflare Workers, for one) MUST at least confine the consequences to the room concerned: the Cloudflare implementation runs every room ID in its own Durable Object, whose memory limit then resets only that room.

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
- **payloads**: each side's **resume state**, 32 bytes (§5.3):

  ```
  stream ‖ base ‖ seen_stream ‖ seen_next      (four u64, big-endian)
  ```

  Receivers MUST ignore bytes after the first 32, which future revisions may use, and MUST ignore a handshake message whose payload is shorter.

The joiner sends handshake message 1 in an INIT frame; the inviter replies with message 2 in a REPLY frame. Both then hold a pair of transport cipher states from `Split()`: the first encrypts joiner → inviter, the second inviter → joiner.

This one handshake serves both the first contact and every reconnection. It authenticates both identities, requires knowledge of the room secret, binds the session to the room, and gives forward secrecy from the ephemeral keys. The joiner's identity travels encrypted, so the relay never sees either identity key.

### 5.2 Handshakes, pinning and pairing

On every PRESENCE, a peer discards any session it has and any partially received message.

- On PRESENCE `0x01`, the **joiner** starts a new handshake and sends INIT. The **inviter** waits for INIT.
- On PRESENCE `0x00`, both wait.

The inviter processes INIT whenever the other seat is held, even if it already has a session (the new handshake replaces it). On a successful read of message 1 it obtains the joiner's identity public key `rs` and accepts it if it is the pinned key, or, before pairing completes, if it is the key of the joiner it is pairing with (the first one it accepted since it started); it ignores the INIT entirely otherwise. It then sends REPLY. The inviter's session is established once REPLY is sent; the joiner's, once REPLY is read.

Immediately on establishing a session, the joiner sends an ACK (§5.3). The first authenticated transport message the inviter receives from a joiner it is pairing with completes **pairing**: the inviter pins that joiner's key, marks the room paired, and persists its record. The joiner completes pairing when it first establishes a session at the pair ID (§6.2). Neither step needs a further confirmation: a joiner's identity is fixed before it first sends INIT, so if anything is lost on the way, the same handshake simply happens again.

A handshake message that fails to decrypt, or arrives when not expected, MUST be ignored: it is either stale (sent to a connection that has since been replaced) or forged. A joiner that has not received a valid REPLY within **10 s** of sending INIT SHOULD close its connection and reconnect (§6), which gives both sides a fresh PRESENCE.

### 5.3 The stream

Each MESSAGE frame body is one Noise transport message (at most 65,535 bytes), encrypted with the sender's cipher state and empty associated data. Noise's implicit nonces make every message's position part of its authentication: within a session, a dropped, duplicated, reordered or modified frame fails to decrypt.

The plaintext is a kind byte followed by data:

| Kind | Name | Data |
|---|---|---|
| `0x00` | FINAL | the last (or only) chunk of a message |
| `0x01` | MORE | a chunk of a message, more follow |
| `0x02` | CLOSE | empty: the sender ends the room (§5.5) |
| `0x03` | ACK | `next[8]` (u64): the id of the next message expected from the receiver of this ACK |

A message is sent as zero or more MORE chunks followed by one FINAL chunk, each carrying 0 to 65,518 bytes (the largest plaintext, 65,519, minus the kind byte). An empty message is a single empty FINAL. The receiver concatenates chunks and delivers the message on FINAL. Messages are at most **16 MiB** (16,777,216 bytes); a receiver whose reassembled message would exceed that MUST treat it as a session failure. Receivers MUST ignore plaintexts with an unknown kind, or with no kind byte.

**Numbering.** Each peer numbers what it sends, messages and its CLOSE alike, 0, 1, 2, … in its **stream**, which has a random non-zero 64-bit **stream id** chosen afresh whenever the peer starts. ACKs are not numbered. A sender keeps every message until the peer acknowledges it; its **base** is the id of the oldest one it still holds. A receiver remembers, for each of the peer's streams it has seen recently (at least the last 16), its **position**: the id of the next message to deliver. Remembering several matters when two instances of the same peer take turns, or when a relay replays an old handshake: one stream's arrival must not move another's position.

**Resuming.** Each side's handshake payload carries its `stream` and `base`, and one position: `seen_stream` and `seen_next`. The joiner, which cannot know the inviter's stream yet, gives its position in the inviter stream it saw most recently; the inviter, having read message 1, gives its position in the joiner's stream (or `seen_stream = 0` if it has none). When a session is established, each side:

- *as a sender*, resumes from `max(theirs.seen_next, own base)` if `theirs.seen_stream` is its own stream id (and no further than its next unused id), and from its own `base` otherwise. Everything below that point is acknowledged.
- *as a receiver*, numbers the messages that arrive from the point the sender resumes from, which it computes by the same rule from the same two payloads: `max(own seen_next, theirs.base)` if its own payload's `seen_stream` is `theirs.stream`, and `theirs.base` otherwise. It delivers only messages at or above its position in `theirs.stream` (`theirs.base` if it has none), and drops the rest as duplicates.

So message ids need not travel with messages: in a session, messages arrive in id order from a point both sides agree on.

**Acknowledging.** A receiver sends ACK with its position in the current stream within **50 ms** of delivering a message, at once after delivering 32 messages or 1 MiB since its last ACK, and at least every **10 s** while it is receiving the chunks of a message. The joiner also sends ACK at once on every new session. A sender drops every message below an ACK's `next` and may report them as delivered.

**Liveness.** A sender that has data in flight in a session and has received no ACK for **30 s** treats the session as broken (a frame lost, or the peer's side of it gone), closes its connection and reconnects; the stream resumes in the next session.

**Flow control.** A sender transmits a message only while less than a **window** of **4 MiB** of what it has sent in the current session is unacknowledged (a single message may overrun the window). The window bounds what is ever in flight between two peers, whatever the relay does.

**Failures.** A MESSAGE that arrives without an established session is stale; ignore it. A MESSAGE that fails to decrypt within a session means the relay did not deliver the stream faithfully; the session is unusable, and the peer MUST close its connection and reconnect. Nothing is lost by this: unacknowledged messages are resent in the next session.

### 5.4 Delivery guarantee

While both peers keep running, every message one sends is delivered to the other exactly once and in order, however often connections drop, relays restart or sessions are replaced; it reaches the other peer as soon as both are connected to the same relay at the same time. The relay is never trusted with this: it only ever sees ciphertext in flight.

A peer that stops loses the messages it had not had acknowledged (§3.4). A peer that restarts gets a new stream id; the other peer then starts a new position for it, so nothing is mistaken for a duplicate. A restarted peer has also forgotten what it had received: messages it had received but not yet acknowledged (within the acknowledgement delay) are resent to it, so across a receiver's restart delivery is at least once. Applications that cannot tolerate that number their own messages.

### 5.5 Ending a room

A peer ends a room by marking its record as closing, persisting it, and queueing a CLOSE in its stream, after any messages already queued. It accepts no further messages to send. Like any message, the CLOSE is resent until acknowledged: the peer keeps connecting until the other peer acknowledges it, and a peer started from a closing record queues the CLOSE again. Once acknowledged, the room is over for this peer.

A peer that receives CLOSE acknowledges it at once, then stays until the other peer leaves, or for at most **30 s**, before it stops reconnecting and reports the room as ended; if its connection drops meanwhile, it reconnects, and the position in its next handshake acknowledges the CLOSE in case the ACK was lost. Both peers then SHOULD delete their records.

CLOSE reaches the other peer only once both are connected at the same time: a closing peer that gives up, or deletes its record, before that never tells it. And a closing peer whose acknowledgement was lost beyond recovery waits in vain; applications SHOULD bound how long they keep trying.

Leaving a room for now, without ending it, is simply closing the WebSocket with 1000; the partner sees PRESENCE `0x00`.

## 6. Connection lifecycle

### 6.1 The loop

A peer keeps at most one connection to the relay per room and runs this loop:

1. Choose a room ID (§6.2), connect to its endpoint (§4.1), join (§4.3).
2. Follow PRESENCE, handshake (§5.2) and carry the stream (§5.3), sending `ping` every 25 s.
3. When the connection closes, fails, or must be abandoned (no frame for 60 s, no REPLY within 10 s, a MESSAGE that fails to decrypt, a relay protocol violation), wait and go to 1.

Reconnection SHOULD use exponential backoff with jitter: the reference implementation waits a uniformly random time between half and all of `min(30 s, 0.5 s × 2^n)`, where `n` counts the attempts since a connection last stayed joined for at least 30 s. (Resetting on anything earlier, such as a successful JOIN, would let a fault that strikes right after joining cause a tight reconnection loop.)

A peer whose connection is closed with **4409** reports this to the application and MUST wait at least **30 s** (the reference implementation: 60 to 70 s) before reconnecting. The usual cause is the same room in use elsewhere, on another device, tab or process; the wait stops two instances from evicting each other in a loop, and the application can let one of them leave. A peer does not give up for good: a 4409 can also come from a relay, and must not turn a moment's interference into a lasting outage.

### 6.2 From the invite ID to the pair ID

A paired peer always uses the pair ID. Before pairing completes:

- The **inviter** uses the invite ID. When pairing completes (§5.2), it closes its connection at once and reconnects at the pair ID; the session in progress ends, and its unacknowledged messages are resent there.
- The **joiner** alternates between the invite ID and the pair ID, starting with the invite ID: it moves to the other one whenever it reconnects, whenever it has waited **20 s** alone in a room, and at once when the inviter leaves a session it had with it at the invite ID. Since the inviter is at the invite ID until paired and at the pair ID afterwards, the joiner finds it within one round of alternation, whichever of its messages were lost.

A paired joiner uses the pair ID, but after **5 minutes** alone there it looks at the invite ID for 20 s. An inviter that crashed before persisting the completion of pairing comes back unpaired, at the invite ID; this is where the joiner finds it, completes pairing with it again, and follows it back to the pair ID.

So the invite ID serves for pairing only, typically for one round trip, and the room then lives at an ID that the invite does not lead to.

## 7. Limits and timers

| Name | Value | Enforced by |
|---|---|---|
| Frame size | ≤ 65,536 bytes | relay (1009) |
| Message size | ≤ 16 MiB | peers |
| Window | 4 MiB unacknowledged per session | senders |
| ACK delay | ≤ 50 ms, or at once after 32 messages / 1 MiB; ≥ every 10 s during a message | receivers |
| ACK timeout | 30 s with data in flight | senders reconnect |
| Linger after CLOSE | until the peer leaves, ≤ 30 s | the peer told to close |
| `ping` interval | 25 s | peers send |
| Join timeout | 10 s | relay (4408) |
| Idle timeout | 90 s without any frame | relay (4408), optional |
| Stall timeout | 30 s without accepting forwarded data | relays that apply backpressure (4408) |
| Relay keepalive | `pong` after 30 s without sending | relays that apply backpressure |
| Wait after 4409 | at least 30 s | peers |
| Receive timeout | 60 s without any frame | peers |
| Handshake timeout | 10 s for REPLY | joiner |
| Rendezvous dwell | 20 s alone before trying the other ID (5 min at the pair ID once paired) | joiner |

Implementations also bound how much they queue for sending; the reference implementation refuses new messages while 64 MiB await acknowledgement.

## 8. Security considerations

### 8.1 Threat model

The relay, and the network between it and the peers, are untrusted. PeerConnect guarantees, against them:

- **Confidentiality** of messages.
- **Integrity and authenticity**: a peer accepts a message only if the other peer sent it. Within a session, replays and reordering are detected cryptographically; across sessions, the stream ids and resume rules (§5.3) deliver each message exactly once.
- **Mutual authentication**: after pairing, only the two pinned identities can establish sessions in the room, and only they can reach it.
- **Forward secrecy**: compromising identity keys or the room secret later does not reveal past sessions' messages.
- **Identity hiding**: identity keys never appear on the wire in the clear.

It does not guarantee availability. A relay can refuse service, drop or delay frames, and cut connections; peers detect this (as failed decryption, timeouts or closed connections), recover from it by reconnecting and resending, but cannot prevent it. A peer's own device is trusted: whoever extracts a peer's record can impersonate that peer.

### 8.2 What the relay learns

Room IDs, connection times and network addresses, frame sizes and timing, which seat each connection holds, and when sessions are (re)established. Not message contents, not identity keys, not the room secret, not message ids. Room IDs are unlinkable to each other and, with per-room identities, so are rooms belonging to the same peer. A relay that sees both IDs of a room in use can link them by timing.

### 8.3 Invites

The invite carries the room secret and the inviter's identity.

- **Before pairing**, whoever presents the invite first pairs with the inviter. If an attacker races the intended joiner, the joiner's handshakes are ignored and it never connects, so the race is visible, but it is not prevented. Applications for which this matters can have users compare a short fingerprint of both identity keys (available from both peers' records) out of band.
- **After pairing**, a leaked invite is useless. It gives no access to messages and does not allow impersonating either peer: the inviter only accepts the pinned joiner, and the joiner only the inviter's key. It does not even give relay access to the room, because the peers no longer meet at the invite ID, and the pair ID's key needs an identity private key (§3.1). Someone holding the invite finds an empty room ID.

A stateless relay cannot make an invite single-use, as that would require remembering it was used. PeerConnect gets the same effect by moving the room: the invite only ever leads to the invite ID.

### 8.4 Handshake properties

`IKpsk1` gives the joiner explicit authentication of the inviter on receipt of REPLY, and the inviter authentication of the joiner on receipt of INIT (through `ss` and the psk). INIT can be replayed by the relay. A replayed INIT makes the inviter run a handshake nobody can complete, replacing its working session; it acts on the stale payload only for the stream that payload names, whose position it keeps separately from the current one (§5.3), so nothing is delivered twice. The broken session is noticed when either side next sends, or by the acknowledgement timeout, and the next handshake resumes the stream. The joiner's identity is protected against passive observers and against active attackers who do not hold the inviter's identity key.

Both peers require knowledge of `S` (through the psk) in addition to their identity keys, so an attacker needs both a peer's identity key *and* the room secret to impersonate it.

The handshake payloads (stream ids and counters) are encrypted, but message 1's payload is not forward-secret: it is protected by the inviter's identity key and the psk. It carries no secrets.

### 8.5 Session lifetime

Transport keys live as long as a session, which ends whenever either connection ends. There is no in-session rekeying: a session that lasts for days keeps its keys for days. Nonces are 64-bit counters and never wrap in practice.

### 8.6 Denial of service

Relays bound frame size, unauthenticated connection time, idle time, and buffered data per connection (§4.5). Connections SHOULD be limited per client address, overall and while not yet joined, so that knowing a room ID (which grants nothing else) is not enough to crowd out a room's peers. Ed25519 verification costs the relay one signature check per JOIN, and an unauthenticated connection can send only a single JOIN before being closed or seated.

Flow control is end to end: the window bounds what an honest sender has in flight, and a relay that holds back a fast sender never stalls it for longer than the stall timeout. Implementations MUST keep reading from the relay while they wait to write to it; otherwise two peers sending large messages to each other at once can deadlock.

## 9. Conformance

[`test-vectors/v1.json`](test-vectors/v1.json) gives, for fixed inputs, every derived value and every byte on the wire:

- the invite ID, pair ID and psk, and the invite itself;
- JOIN signatures at both IDs;
- both handshake messages, with resume payloads that exercise resumption;
- ACK, message and CLOSE frames in both directions.

[`test-vectors/verify.mjs`](test-vectors/verify.mjs) is an independent implementation of all of that, written from this document using only Node's built-in cryptography. An implementation that reproduces the vectors, and follows the behaviour above, interoperates with this one.
