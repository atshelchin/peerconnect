# Design notes

Why the protocol looks the way it does, and where it departs from the original draft ([`draft.md`](../draft.md), v0.1.0). The normative text is [SPEC.md](../SPEC.md).

## The constraint that shapes everything

The draft asks for a relay that is untrusted, keeps no persistent state, and still lets two peers rebuild their room after the relay forgets it (§7: *"协议也必须定义可验证的重建授权"*). Taken seriously, this rules out any design where the relay *remembers* who may enter a room. Authorization must be something the relay can **verify from the request alone**.

That single requirement drives the main changes.

## 1. The room ID is a public key

**Draft:** Peer A sends `create {room}`, B sends `join {room, inviteToken}`, and the relay checks the token.

**Problem:** To check a token, the relay must have stored it at `create` time. That state disappears on restart, so after a restart nobody can prove anything, and the rebuild story falls apart. It also hands the token to the relay in the clear.

**Now:** the room secret `S` deterministically yields an Ed25519 key pair; the room ID *is* the public key. To enter, a peer signs a fresh relay challenge. The relay verifies the signature against the room ID in the URL. That needs nothing stored, survives restarts, and reveals nothing the relay could replay elsewhere. The room ID still only locates the room (draft §4.1); the private key is the access credential.

Consequences:

- `create` and `join` collapse into one `JOIN`. A room exists on the relay exactly while someone sits in it; a relay restart is indistinguishable from everyone reconnecting.
- The draft's room states (CREATED / ACTIVE / WAITING / CLOSED) disappear from the relay. What remains is two seats, each empty or not. Room lifecycle is a matter between the peers.
- The challenge costs no round trip: the relay sends `HELLO` in the same flight as the `101 Switching Protocols`, and the client could not have sent anything earlier anyway.

## 2. One handshake, pinning, and a second room ID

The relay credential is derived from the invite, so whoever holds the invite can enter the room at its invite ID. What about the end-to-end channel? The obvious ideas both fail:

- **Use the invite secret alone (e.g. `NNpsk0`) forever.** Then an invite leaked later lets a malicious relay man-in-the-middle every future session. Unacceptable.
- **Pair once, then switch both peers to a key derived from the handshake.** The switch is a two-generals problem: if the last handshake message is lost, one peer switches and the other does not, and with no relay state there is nobody to reconcile them.

**Every handshake is `Noise_IKpsk1_25519_ChaChaPoly_SHA256`,** the first one and every reconnection:

- the joiner knows the inviter's static key from the invite;
- the joiner's static key travels encrypted in message 1;
- both prove knowledge of the room secret through the psk.

The inviter **pins** the joiner's key and refuses any other afterwards. Because the joiner's key is fixed *before* it ever speaks, pinning is idempotent: if anything is lost, the joiner retries with the same key and nothing is out of sync. After pairing, a leaked invite can no longer read, forge or impersonate.

That still left relay access: anyone with the invite could take either seat at the invite ID and knock a peer off. A stateless relay cannot remember that an invite was used, so the room has to **move**. Once paired, the peers meet at a **pair ID** whose key is derived from the room secret *and* the Diffie-Hellman of their two identities. Only they can compute it; the invite does not lead there, even to someone who has learned both public keys.

The move is exactly the two-generals situation the first design avoided, so it is built to tolerate any loss:

- The inviter moves when pairing completes, which is when it first receives an authenticated message from the joiner. The joiner acknowledges at once on every new session.
- An unpaired joiner does not try to know whether the inviter has moved. It alternates between the two IDs: on every reconnection, after 20 s alone, and at once when the inviter leaves a session. Because the inviter is at exactly one of the two, the joiner finds it within one round.

So there is no moment at which one side's lost message strands the other; the simulation drops the confirmation on purpose, and restarts the inviter mid-pairing, to show it. One more case needed care: an inviter that crashes after moving but before its app persisted the completion comes back unpaired, at the invite ID, while the joiner, now paired, waits at the pair ID. So a paired joiner that has been alone for five minutes looks at the invite ID for 20 s, completes pairing again with the same identities, and both move back. In practice the invite ID carries one round trip of traffic, and the room then lives where the invite cannot follow.

`IK` over `XX`: one round trip instead of one and a half; the inviter's key is in the invite anyway; and the joiner gets assurance of the inviter's identity even if the invite was copied. `psk1` over `psk2`: the psk must be mixed into message 1, or the inviter could be made to pin a key from someone who never had the invite.

## 3. Two fixed seats; newest connection wins

**Draft:** max two peers per room, "simultaneous reconnect conflicts" left open.

**Now:** seat 0 is the inviter, seat 1 the joiner; the role is part of the signed `JOIN`. A new connection to an occupied seat replaces the old one (close code 4409). This settles every race:

- A phone that switches networks reconnects at once, instead of waiting for its old, dead connection to time out.
- There is no "room full": an extra connection always lands in its own seat.
- Handshake initiation is never ambiguous: the joiner always initiates, triggered by `PRESENCE(1)`, which the relay sends whenever a *new* partner connection appears (including a replacement). Both sides drop any previous session on every `PRESENCE`.
- A replaced client steps back for a minute and tells its application, which can let it leave (two tabs with the same room), instead of evicting the other in a tight loop. It does not stop for good. An earlier version did, until a review pointed out that a single forged 4409 would then knock a peer out permanently, which is a far worse outcome than any other relay misbehaviour.

The relay guarantees an order: the partner sees `PRESENCE(1)` before any frame from the new connection, and never sees a frame from a replaced one after it. Stale frames that slip through before that point are recognisably stale (they fail authentication, or arrive with no session) and are ignored.

## 4. A reliable stream, kept by the peers

The draft promised no delivery across disconnections, because the relay keeps no messages. But the relay is not the only place a message can wait: the **sender** has it. So each peer numbers what it sends, keeps every message until the other acknowledges it, and on every new session resends from where the other says it got to. The relay stays stateless, and the sender's own memory is the offline queue.

Details that keep it small:

- **Resume state rides in the handshake payloads,** which Noise encrypts and authenticates and which were empty before: 32 bytes each way, so resuming costs no extra round trip.
- **Message ids never travel.** Both sides compute the resume point from the two payloads; within a session, Noise's nonces already guarantee order.
- **A random stream id per process start** tells a resumed stream from a new one. A peer that restarted lost its queue; its partner sees the new id and starts counting afresh, rather than discarding the new messages as duplicates.
- **Positions are kept per stream.** A receiver remembers its place in each of the last 16 streams it saw from the peer, and computes where the sender will resume by the sender's own rule. A first version kept one position; review showed that two instances of the same peer taking turns, or a relay replaying an old handshake, then moved it backwards and caused duplicates.
- **ACKs are cumulative and batched** (50 ms, or 32 messages, or 1 MiB), with a progress ACK every 10 s while a long message is arriving. A sender that hears nothing for 30 s with data in flight reconnects: a relay that silently drops the last frame of a session can otherwise stall the stream forever.
- **A 4 MiB window** caps what is unacknowledged in flight. That bounds what any relay has to hold, including Cloudflare's, which cannot apply backpressure.

What it cannot do: survive the sender's own process ending. The queue is in memory, as any persistence would make the SDK opinionated about storage. Applications that need that keep their own copies until they see the message acknowledged. Symmetrically, a *receiver* that restarts forgets what it had received but not yet acknowledged (at most 50 ms' worth), and gets it again: exactly once while both keep running, at least once across a receiver's restart.

## 5. Close is end-to-end; leave is just closing the socket

The draft lists `leave` and `close` as relay operations. But a stateless relay cannot *enforce* that a room is over: it would forget on restart, and either peer could simply rejoin. So:

- **leave** = close the WebSocket. The partner sees `PRESENCE(0)`. Nothing is lost.
- **close** = a `CLOSE` at the end of the stream, which the partner trusts because it is authenticated. Being part of the stream, it is resent until acknowledged, and the record says "closing" so that even a restarted peer carries on. The partner acknowledges, then lingers until the closer leaves (at most 30 s), so that an acknowledgement lost with a dropped connection is repeated by the next handshake rather than lost for good. An inviter closing before pairing completes simply stays at the invite ID until it is told.

A peer that is never online at the same time as the closer, after the closer gave up, is never told. That limit is inherent and stated in the spec, rather than papered over with a "closed rooms" list the relay would lose anyway.

## 6. Wire format

- **Binary frames, one type byte.** Types below `0x80` are *peer frames*: the relay forwards them byte for byte without parsing, so new peer-level features never require relay changes. Types from `0x80` up are relay frames. The draft's JSON control messages became three fixed-layout frames (`HELLO`, `JOIN`, `PRESENCE`).
- **Text `ping`/`pong`.** This is the one text message, because Cloudflare can answer a fixed text message without waking a hibernating Durable Object. It costs nothing elsewhere and makes keepalives free on Cloudflare.
- **Errors are WebSocket close codes.** Every relay error ends the connection anyway, so a separate error frame would add nothing.
- **No sequence numbers on the wire.** Noise transport nonces are implicit counters. Over an ordered channel this detects replay, reordering and loss within a session, for free; message ids across sessions are derived from the handshake (§4). Any decryption failure means the relay misbehaved; the peer reconnects, and the stream resumes.
- **64 KiB frames, 16 MiB messages.** Each transport message stays within Noise's 65,535-byte limit, so any standard Noise library can implement the protocol, and relays need only small, bounded buffers. A one-byte kind (`FINAL`/`MORE`/`CLOSE`/`ACK`) inside the ciphertext chunks larger messages and carries the stream's control. Putting it inside the ciphertext means the relay cannot tamper with it.

## 7. Flow control

Every buffer is bounded except where unbounded is safe, and no wait is unbounded:

- **Relay outboxes are byte budgets** (256 KiB per connection), not frame counts. A sender whose partner is slow is held back through TCP.
- **A receiver that accepts nothing for 30 s is closed**, so a stalled or malicious peer cannot hold its partner hostage.
- **Writing never blocks reading**, in the relay and in the clients alike. In the Rust client a writer task owns the socket's write half, and incoming events queue without bound, as a browser WebSocket does. Otherwise two peers that send each other 16 MiB at once deadlock: each waits to write, so neither reads. An earlier version did exactly that; the review that found it is why these tests exist.
- **The window and the outbox pace the application.** At most 4 MiB is unacknowledged on the wire; at most 64 MiB is queued, beyond which `send` waits (Rust) or throws (JavaScript). Keepalives jump the socket's queue, and leaving never waits for anything.
- **The relay keeps a backpressured connection alive.** A relay holding back a sender cannot read its pings in the meantime, so after 30 s of silence towards a connection it sends `pong` unprompted. Without this, a large upload to a slow reader would look like a dead connection after 60 s.

## 8. Liveness

Peers ping every 25 s, unconditionally; relays may drop connections silent for 90 s; peers give up on a connection that has been silent for 60 s. Pinging unconditionally, rather than only when idle, lets a relay judge liveness from pings alone. That is exactly what Cloudflare's auto-response timestamp provides.

## 9. What was left out on purpose

| Not included | Why |
|---|---|
| Relay-side offline storage | The draft excludes it; senders queue instead (§4). |
| Persistent outgoing queue | The SDK would have to own storage; applications keep copies until acknowledged. |
| Several relays per room | Both peers must meet on the same one; failover without coordination needs more than a URL list. Peers can move a room by agreeing on a new relay URL. |
| In-session rekeying | Sessions end at every reconnect; long-lived ones are noted in §8.5 of the spec. |
| Rate limiting beyond connection counts | The Linux relay limits connections per address (IPv6 per /64); request-rate limits belong to the proxy or Cloudflare rules. |
| Out-of-band fingerprint UI | Both identity keys are exposed by the SDKs; comparing them is an application choice. |
| More than two peers, RPC, pub/sub | Out of scope, as in the draft. |

## 10. Implementation architecture

One Rust crate, `peerconnect`, holds the whole protocol **without I/O**:

- `Room` / `Invite`: credentials and derivations;
- `noise`: the one handshake and its transport, under 300 lines with comments, tested byte-for-byte against `snow`;
- `Peer`: the client state machine, which is driven by events, emits outputs, and asks for timer callbacks (the "sans-I/O" style);
- `relay`: every relay decision about a single message.

Drivers:

| Driver | Shares | Owns |
|---|---|---|
| `peerconnect::client` (Tokio) | `Peer` | WebSocket via tungstenite + rustls, timers |
| JavaScript SDK | `Peer`, compiled to WebAssembly | `WebSocket`, `setTimeout` (about 100 lines of JS) |
| Linux relay | `relay::Conn`, frames, auth | hyper + tungstenite. Per connection, one reader task that forwards directly into the partner's byte-budgeted outbox, and one writer task. A per-room mutex protects the seats. Reserving budget *before* checking the seats gives backpressure without ever letting a replaced connection's frame through. |
| Cloudflare relay | `relay::Conn`, frames, auth | One Durable Object per room. Sockets are held through the Hibernation API; each socket's seat lives in its attachment, so an evicted object rebuilds its view from the sockets on the next event. Nothing is written to storage. An alarm exists only while a connection has yet to join; idle timeouts are checked when the room wakes. Unjoined connections are capped per client address. |

Because both clients run the same state machine, the deterministic simulation in `crates/peerconnect/tests/simulation.rs` covers the JavaScript SDK's protocol behaviour too. The same end-to-end suite runs against both relays.
