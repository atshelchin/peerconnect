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

## 2. One handshake, with pinning, instead of a second room

The relay credential is derived from the invite, so whoever holds the invite can always enter the room on the relay. What about the end-to-end channel? The obvious ideas both fail:

- **Use the invite secret alone (e.g. `NNpsk0`) forever.** Then an invite leaked later lets a malicious relay man-in-the-middle every future session. Unacceptable.
- **Pair once, then move both peers to a fresh room derived from the handshake.** This makes the invite truly single-use, but the move is a two-generals problem. If the last handshake message is lost, one peer moves and the other does not, and with no relay state there is nobody to reconcile them. Fixing that needs dual connections and timeouts: much more mechanism for a corner case.

**Now:** every handshake, the first and every reconnection, is `Noise_IKpsk1_25519_ChaChaPoly_SHA256`:

- the joiner knows the inviter's static key from the invite;
- the joiner's static key travels encrypted in message 1;
- both prove knowledge of the room secret through the psk.

The inviter **pins** the joiner's key the first time and refuses any other afterwards. Because the joiner's key is fixed *before* it ever speaks, pinning is idempotent: if the reply is lost, the joiner retries with the same key and nothing is out of sync. There is no confirmation round and no two-generals problem.

Result: after pairing, a leaked invite can no longer read, forge or impersonate. What remains is relay access: the ability to occupy either seat, which is denial of service, the same power the relay itself always has. That is the honest limit of a stateless relay (it cannot remember that an invite was used), and the spec says so (§8.3).

`IK` over `XX`: one round trip instead of one and a half; the inviter's key is in the invite anyway; and the joiner gets assurance of the inviter's identity even if the invite was copied. `psk1` over `psk2`: the psk must be mixed into message 1, or the inviter could be made to pin a key from someone who never had the invite.

## 3. Two fixed seats; newest connection wins

**Draft:** max two peers per room, "simultaneous reconnect conflicts" left open.

**Now:** seat 0 is the inviter, seat 1 the joiner; the role is part of the signed `JOIN`. A new connection to an occupied seat replaces the old one (close code 4409). This settles every race:

- A phone that switches networks reconnects at once, instead of waiting for its old, dead connection to time out.
- There is no "room full": an extra connection always lands in its own seat.
- Handshake initiation is never ambiguous: the joiner always initiates, triggered by `PRESENCE(1)`, which the relay sends whenever a *new* partner connection appears (including a replacement). Both sides drop any previous session on every `PRESENCE`.
- A replaced client steps back for a minute and tells its application, which can let it leave (two tabs with the same room), instead of evicting the other in a tight loop. It does not stop for good. An earlier version did, until a review pointed out that a single forged 4409 (from a relay, or from anyone holding the room secret) would then knock a peer out permanently, which is a far worse outcome than any other relay misbehaviour.

The relay guarantees an order: the partner sees `PRESENCE(1)` before any frame from the new connection, and never sees a frame from a replaced one after it. Stale frames that slip through before that point are recognisably stale (they fail authentication, or arrive with no session) and are ignored.

## 4. Close is end-to-end; leave is just closing the socket

The draft lists `leave` and `close` as relay operations. But a stateless relay cannot *enforce* that a room is over: it would forget on restart, and either peer could simply rejoin. So:

- **leave** = close the WebSocket. The partner sees `PRESENCE(0)`. Nothing is lost.
- **close** = an encrypted `CLOSE` inside the session, which the partner trusts because it is authenticated, followed by leaving. Both peers delete their records.

If the partner is offline at that moment it is never told. That is an inherent limit, stated in the spec, rather than something papered over with a "closed rooms" list the relay would lose anyway.

## 5. Wire format

- **Binary frames, one type byte.** Types below `0x80` are *peer frames*: the relay forwards them byte for byte without parsing, so new peer-level features never require relay changes. Types from `0x80` up are relay frames. The draft's JSON control messages became three fixed-layout frames (`HELLO`, `JOIN`, `PRESENCE`).
- **Text `ping`/`pong`.** This is the one text message, because Cloudflare can answer a fixed text message without waking a hibernating Durable Object. It costs nothing elsewhere and makes keepalives free on Cloudflare.
- **Errors are WebSocket close codes.** Every relay error ends the connection anyway, so a separate error frame would add nothing.
- **No sequence numbers on the wire.** Noise transport nonces are implicit counters. Over an ordered channel this detects replay, reordering and loss, for free. Any decryption failure means the relay misbehaved; the peer reconnects, which resets everything.
- **64 KiB frames, 16 MiB messages.** Each transport message stays within Noise's 65,535-byte limit, so any standard Noise library can implement the protocol, and relays need only small, bounded buffers. A one-byte kind (`FINAL`/`MORE`/`CLOSE`) inside the ciphertext chunks larger messages. Putting it inside the ciphertext means the relay cannot tamper with it.

## 6. Flow control

Every buffer is bounded except where unbounded is safe, and no wait is unbounded:

- **Relay outboxes are byte budgets** (256 KiB per connection), not frame counts. A sender whose partner is slow is held back through TCP.
- **A receiver that accepts nothing for 30 s is closed**, so a stalled or malicious peer cannot hold its partner hostage.
- **Writing never blocks reading**, in the relay and in the clients alike. In the Rust client a writer task owns the socket's write half, and incoming events queue without bound, as a browser WebSocket does. Otherwise two peers that send each other 16 MiB at once deadlock: each waits to write, so neither reads. An earlier version did exactly that; the review that found it is why these tests exist.
- **`send` paces the application**: it waits while more than 1 MiB is queued. Keepalives jump that queue, and leaving never waits for it. Ending a room does wait, up to 30 s, because the message that ends it must follow everything already queued: it is encrypted with the next nonce.
- **The relay keeps a backpressured connection alive.** A relay holding back a sender cannot read its pings in the meantime, so after 30 s of silence towards a connection it sends `pong` unprompted. Without this, a large upload to a slow reader would look like a dead connection after 60 s.

## 7. Liveness

Peers ping every 25 s, unconditionally; relays may drop connections silent for 90 s; peers give up on a connection that has been silent for 60 s. Pinging unconditionally, rather than only when idle, lets a relay judge liveness from pings alone. That is exactly what Cloudflare's auto-response timestamp provides.

## 8. What was left out on purpose

| Not included | Why |
|---|---|
| Offline queue | Requires relay storage; the draft excludes it. |
| In-session rekeying | Sessions end at every reconnect; long-lived ones are noted in §8.5 of the spec. |
| Relay-side rate limiting per IP | Deployment concern (proxy, firewall, Cloudflare); documented. |
| Out-of-band fingerprint UI | Both identity keys are exposed by the SDKs; comparing them is an application choice. |
| More than two peers, RPC, pub/sub | Out of scope, as in the draft. |

## 9. Implementation architecture

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
