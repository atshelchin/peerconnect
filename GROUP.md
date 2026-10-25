# PeerConnect Groups, version 1

Up to 32 members, one group, end-to-end encryption over an untrusted, stateless WebSocket relay: commands, chat, voice and state transfer, each with its own priority.

Status: draft for review. It extends [SPEC.md](SPEC.md) (two-peer rooms) without changing it: groups use their own endpoint, frames and keys, and a relay may serve both. Sections of SPEC.md referenced here (base64url, primitives, keepalive) apply unchanged.

## 1. Overview

A **group** is defined by a 32-byte **group secret**. Whoever holds it can enter the group on a relay; nobody else can, and the relay never learns it. Each member also holds an **identity**, an Ed25519 key pair chosen by the application (one per player account, for example). Members know each other by identity public key, which is how an application decides who may do what: which member owns which object, who is only visiting.

The relay:

- admits a connection that proves it holds the group key, without learning the key;
- gives every connection a **slot** (0–31) and tells everyone who is present;
- forwards each frame to one member or to all, by priority, without reading it;
- keeps no state beyond its open connections.

The members:

- authenticate each other by identity, and agree on fresh per-connection **sender keys**, so that a relay cannot read, forge or replay anything, even from an earlier session;
- send on four **traffic classes**, from most to least urgent: *commands*, *voice*, *chat* and *bulk*. A class never waits for a less urgent one, anywhere along the way;
- carry two reliable broadcast **streams** per member, commands and chat: numbered messages, kept by the sender and resent after reconnections, delivered to every member in each sender's order, exactly once while both keep running;
- send **voice** as real-time frames that anyone along the way drops rather than delays;
- bring a newcomer up to date with **state transfer**: a member already present sends an application snapshot together with the exact set of commands it contains, and the newcomer receives every later command, and only those.

PeerConnect does not define what commands mean, does not order commands from different senders against each other, and keeps nothing on the relay. Applications choose their own consistency model (§7.3).

### 1.1 Terminology

| Term | Meaning |
|---|---|
| Group | The relationship of everyone holding one group secret. Not an object on the relay. |
| Group ID | Where the group meets on a relay: an Ed25519 public key derived from the secret (§2.1). |
| Member | A participant, known by its identity public key (its **member ID**). |
| Slot | The number, 0–31, a relay gives a connection while it is in the group. |
| Incarnation | A random non-zero 64-bit number a member picks whenever it starts. It names that run's streams. |
| Stream | The numbered sequence of messages one incarnation broadcasts on one channel (commands or chat). |
| Sender key | A random key a member uses to encrypt everything it sends on one connection. |

The keywords MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are to be interpreted as described in RFC 2119.

## 2. Credentials

### 2.1 Group secret

```
group_seed = HMAC-SHA256(key = S, "peerconnect/g1 group")
group_key  = the Ed25519 key pair with seed group_seed
group_id   = the group_key public key (32 bytes)
key_secret = HMAC-SHA256(key = S, "peerconnect/g1 keys")
```

The text form of a group ID is its base64url encoding.

### 2.2 Invite

```
invite = relay_url "#" base64url(S)
```

`relay_url` is as in SPEC.md §3.3. The fragment MUST decode to exactly 32 bytes, which a version 1 two-peer implementation rejects (it expects 64), and vice versa.

The invite is the whole credential: there is nothing else to store, and no pairing. Whoever holds it can enter the group, read everything sent there and send under its own identity. Give it only to members, over a confidential channel; a game server that forms a squad would typically generate `S` and hand it to each player. To exclude someone, move the group to a new secret.

### 2.3 Identities

An identity is an Ed25519 key pair. Applications SHOULD persist it, since it is how other members recognise a member across restarts, and MAY use one identity in many groups. The relay never sees identity keys: they travel encrypted (§4.1).

The **join token** of an identity in a group is

```
token = first 16 bytes of HMAC-SHA256(key = identity seed, "peerconnect/g1 token" ‖ group_id)
```

It lets the relay recognise a member's new connection as a replacement for its old one, and is unlinkable across groups.

## 3. Relay protocol

### 3.1 Endpoint and joining

A member opens a WebSocket to

```
relay_url "/g1/" group_id
```

under the same rules as SPEC.md §4.1 (any prefix; HTTP 404 if the last segment is not a canonical Ed25519 public key).

1. The relay sends HELLO (`0x80`, `nonce[32]`) as its first frame.
2. The member answers with JOIN: `0x81 ‖ token[16] ‖ signature[64]`, where
   `signature = Ed25519-Sign(group_key, "peerconnect/g1 join" ‖ nonce ‖ token)`.
3. The relay verifies the signature strictly against the group ID. On failure it closes with 4401; a malformed JOIN or any other frame before it, with 1002; no valid JOIN within 10 s, with 4408.
4. On success the relay seats the connection (§3.2) and sends it WELCOME.

### 3.2 Slots

A group on a relay has 32 slots, each holding at most one connection. A relay MAY admit fewer members (the reference relays: 10 by default); a connection beyond that limit is closed with **4429** (full).

- A connection whose token matches a seated connection's takes **its** slot, and the relay closes the older one with **4409** (replaced). A member that reconnects after a network change is never stuck behind its own dead connection, and a member's identity holds at most one slot.
- Otherwise the connection takes the lowest free slot.

Relay frames:

| Type | Name | Direction | Body |
|---|---|---|---|
| `0x80` | HELLO | relay → member | `nonce[32]` |
| `0x81` | JOIN | member → relay | `token[16] ‖ signature[64]` |
| `0x83` | WELCOME | relay → member | `slot[1] ‖ present[4]`: own slot, and a u32 bitmap (bit *n* = slot *n*) of the other occupied slots |
| `0x84` | MEMBER | relay → member | `slot[1] ‖ present[1]` (`0x01`: a new connection now holds the slot, `0x00`: the slot is free) |

On seating a connection, the relay sends it WELCOME, then sends every other connection MEMBER (`slot`, `0x01`), including when the new connection replaced an older one in that slot. When a seated connection ends for any reason other than replacement, the relay sends every other connection MEMBER (`slot`, `0x00`).

Ordering: a connection receives nothing before its WELCOME, and receives MEMBER (`s`, `0x01`) before any frame from the new connection in slot `s`. A relay MUST NOT deliver any frame from a connection after the MEMBER that announced its replacement or departure: frames still queued from it are discarded.

### 3.3 Peer frames and classes

Every other binary message is a **peer frame**:

```
type[1] ‖ from[1] ‖ to[1] ‖ body
```

- `from` MUST be the sender's slot, and `to` either a slot other than the sender's, or `0xFF` for every other member. A relay closes a connection that breaks this, sends a frame shorter than 3 bytes, a relay frame, or a text message other than `ping`, with 1002, and one that sends a frame over **16,384 bytes**, with 1009. A frame to an unoccupied slot is dropped.
- The relay forwards the frame unchanged to its recipients. It never echoes a frame to its sender.

The type determines the frame's **class**. The relay knows nothing else about it:

| Types | Class | Priority | Delivery |
|---|---|---|---|
| `0x00`–`0x1F` | command | 0 (most urgent) | reliable |
| `0x20`–`0x3F` | voice | 1 | real-time: dropped rather than delayed |
| `0x40`–`0x5F` | chat | 2 | reliable |
| `0x60`–`0x7F` | bulk | 3 | reliable, flow-controlled end to end |

A relay MUST deliver the frames of one class from one connection to another in the order sent. It SHOULD hold a queue per class for each recipient and always send from the most urgent non-empty queue, so that a less urgent class never delays a more urgent one beyond the frame being written. Relay frames go with the command class.

Real-time frames: a relay SHOULD drop a voice frame, instead of queueing it, when more than a few kilobytes (the reference relay: 8 KiB) of command and voice frames are already waiting for that recipient. Late audio is worse than lost audio.

Reliable frames are never dropped. When a recipient falls behind on commands and chat (the reference relay: 256 KiB queued), the relay stops reading from the sender of the frame until that recipient catches up, but for **5 s** at most: a recipient that has not caught up by then is not reading, and the relay closes it with 4408. A recipient whose reliable queue overflows outright (the reference relay: 2 MiB) is closed at once. So a burst of commands slows its sender to the pace of the slowest member for a moment, while one stuck member cannot hold the group up for longer than that. Bulk does not count towards falling behind: its window (§6) bounds it. A relay SHOULD also bound each connection's sending rate (the reference relay: 2 MiB/s, bursts of 4 MiB), by dropping voice and delaying the rest.

Relays that cannot see what they have buffered (Cloudflare Workers, for one) can neither drop late voice nor hold senders back; they forward everything at once. Members cope, but such a relay gives up most of what the classes are for.

### 3.4 Keepalive and timeouts

As SPEC.md §4.6, with shorter times, since a game notices a dead voice connection in seconds:

- members send `ping` every **10 s**; the relay answers `pong`;
- a member treats a connection on which it received nothing for **25 s** as dead;
- a relay MAY close a seated connection it received nothing from for **40 s**, with 4408.

### 3.5 Close codes

As SPEC.md §4.7, with 4409 meaning a newer connection of the same identity took the slot, and **4429**: the group is full on this relay.

## 4. Keys

### 4.1 KEY

On every connection, a member generates a fresh random 32-byte **sender key** and a fresh random 16-byte **challenge**. It announces them in KEY frames (type `0x01`, command class):

```
0x01 ‖ from ‖ to ‖ salt[16] ‖ ChaCha20-Poly1305(k, nonce = 0, ad = type ‖ from ‖ to, plaintext) ‖ tag[16]
k = HMAC-SHA256(key = key_secret, salt)       salt: 16 fresh random bytes per KEY frame
```

```
plaintext = identity[32] ‖ signature[64] ‖ body
body      = sender_key[32] ‖ challenge[16] ‖ echo[16] ‖ incarnation[8]
            ‖ command_next[8] ‖ chat_next[8] ‖ profile[0..1024]
signature = Ed25519-Sign(identity, "peerconnect/g1 key" ‖ group_id ‖ from ‖ to ‖ body)
```

`echo` is the recipient's challenge, or 16 zero bytes when replying to nobody; `command_next` and `chat_next` are the ids the sender's streams will give their next messages (§5); `profile` is application data describing the member (a display name, say), at most 1,024 bytes. Receivers MUST ignore a KEY that fails to decrypt or whose signature does not verify.

### 4.2 Admitting a member

Each member keeps, for every slot, what it knows of the connection there:

1. On WELCOME, a member broadcasts KEY with a zero `echo`.
2. On receiving a KEY with a zero `echo` from a slot it knows nothing about since that slot's last MEMBER (or WELCOME), it records the sender as **pending**, with the KEY's sender key, and replies with a KEY to that slot, echoing the sender's challenge.
3. On receiving a KEY whose `echo` is its own challenge from a slot whose sender it has not yet admitted, it **admits** the sender, and sends that slot CONFIRM (§4.3), echoing the sender's challenge. A KEY with a non-zero `echo` that is not its own challenge is ignored.
4. On receiving CONFIRM from a pending sender, encrypted with that sender's key and echoing its own challenge, it admits the sender.

A member that has seen a slot occupied for **5 s** without admitting it SHOULD send that slot its KEY again (zero `echo` if it knows nothing of the slot, the slot's challenge if pending).

Admission proves freshness: the sender has seen this connection's random challenge, so a relay cannot admit a member by replaying frames from an earlier session. Until a sender is admitted, everything from it except KEY and CONFIRM is ignored. On MEMBER for a slot, a member forgets what it knew of the slot (§3.2 guarantees nothing more from the old connection follows).

A member reports another as **joined** when it admits it, and as **left** when a MEMBER for its slot arrives. Two admitted slots with the same identity cannot occur through an honest relay (§3.2); the newer replaces the older.

### 4.3 Encrypted frames

Every other peer frame is encrypted with the sender's sender key:

```
type ‖ from ‖ to ‖ counter[4] ‖ ChaCha20-Poly1305(sender_key, nonce, ad = type ‖ from ‖ to ‖ counter, plaintext) ‖ tag[16]
nonce = type ‖ 0x00 × 7 ‖ counter              (12 bytes)
```

Each type has its own counter on each connection, starting at 0 and increasing by one per frame. A receiver accepts a frame only if its counter exceeds the last one it accepted of that type from that connection; so a relay may drop frames, or delay one class behind another, but cannot replay or reorder within a class. A frame that fails to decrypt is ignored. A sender that would need counter 2³² reconnects (which gives it a new sender key).

| Type | Name | Class | Plaintext |
|---|---|---|---|
| `0x02` | CONTROL | command | `kind[1] ‖ data` (below) |
| `0x03` | COMMAND | command | a message (§5) |
| `0x21` | VOICE | voice | `seq[2] ‖ audio` |
| `0x22` | DATAGRAM | voice | application data that is dropped rather than delayed (positions, poses) |
| `0x41` | CHAT | chat | a message (§5) |
| `0x61` | BULK | bulk | `transfer[4] ‖ data` (§6) |

CONTROL kinds:

| Kind | Name | Data |
|---|---|---|
| `0x00` | CONFIRM | `echo[16] ‖ command_next[8] ‖ chat_next[8]` |
| `0x01` | RESEND | `channel[1] ‖ incarnation[8] ‖ from[8]` |
| `0x02` | HEADS | `command_next[8] ‖ chat_next[8]` |
| `0x10` | SYNC_REQUEST | `transfer[4]` |
| `0x11` | SYNC_BEGIN | `transfer[4] ‖ length[8] ‖ count[1] ‖ (incarnation[8] ‖ next[8]) × count` |
| `0x12` | SYNC_ACK | `transfer[4] ‖ received[8]` |
| `0x13` | SYNC_ABORT | `transfer[4]` |

Channel `0x00` is commands, `0x01` is chat. Receivers MUST ignore unknown types, kinds and channels, so that later revisions can add them.

## 5. Streams

Each incarnation numbers what it broadcasts on each channel, commands and chat separately, 0, 1, 2, …. A COMMAND or CHAT plaintext is one of:

| Kind | Name | Data |
|---|---|---|
| `0x00` | MESSAGE | `id[8] ‖ payload`: message `id` of the sender's stream on this channel |
| `0x01` | DIRECT | `payload`: a message to one member, outside any stream |
| `0x02` | RESENT | `next[8]`: a resend went up to `next` (exclusive) |
| `0x03` | SKIP | `next[8]`: the sender no longer has messages below `next` |

A sender broadcasts each MESSAGE (`to = 0xFF`) and keeps it, the most recent at least (the reference implementation: 8,192 messages or 8 MiB per channel), to resend. Payloads are at most **16,352 bytes**. DIRECT messages go to one slot, in order with that channel's messages, while both stay connected; they are not resent.

**Positions.** A receiver remembers, for each stream it has heard of (at least the last 64), its **position**: the id of the next message to deliver. It learns a stream from the sender's KEY or CONFIRM:

- A new stream starts at the sender's `*_next` in its first KEY. Messages sent before that were not meant for this receiver.
- If the receiver's position is below the sender's announced `*_next`, it has missed messages: it sends the sender RESEND (`channel`, `incarnation`, position).
- A sender also announces its `*_next` in HEADS, broadcast **1 s** after its latest stream message, so that a receiver that lost the end of a stream notices even if nothing follows.

**Delivery.** A MESSAGE with id equal to the position is delivered, and the position advances, followed by any held messages that are now in order. One below is a duplicate and dropped. One above is **held** (the reference implementation holds up to 1,024 messages or 4 MiB per stream), and the receiver sends RESEND from its position, unless one is outstanding, again after **3 s** without progress. A gap that does not close within **15 s**, or that overflows the hold, is given up: the receiver reports how many messages it missed and moves to the lowest held id.

**Resending.** On RESEND for one of its current streams, a sender sends that member (`to` its slot) MESSAGE frames with their original ids, from `from` on, up to about **128 KiB**, then RESENT with the id after the last one sent. A receiver still behind asks again on RESENT, so that a long backlog goes in bursts, one per round trip, and never floods the relay's queue or holds up other commands for long. If the sender no longer has some of the messages, it first sends SKIP with the lowest id it still has; the receiver reports the difference as missed and moves its position there. SKIP and RESENT travel in the channel's own class, in order with its messages.

A sender also delivers its own broadcast messages to its own application, in order, as they are sent, so that every member, the sender included, sees each stream the same way.

So: while sender and receiver keep running, every message of a stream reaches every member present when it was sent, or that was present and came back, exactly once and in order; messages sent while the sender was offline go out when it reconnects. What does not survive is a process ending: a sender that restarts has a new incarnation and none of its old messages, and a receiver that restarts has forgotten its positions and starts afresh.

## 6. State transfer

A newcomer with no state, or one that missed too much, asks a member for a **snapshot**: application data that represents everything that member's application has applied so far.

1. The newcomer picks a transfer id and sends the member SYNC_REQUEST. From now until the transfer ends, it holds every COMMAND message it receives, instead of delivering it. Chat is not held.
2. The member's implementation reports the request to its application, and records the **frontier** at that point: its positions in every command stream it knows, its own included. The application answers with a snapshot of its state *as of that report*, having applied exactly the commands delivered before it.
3. The member sends SYNC_BEGIN with the snapshot's length and the frontier, then the snapshot in BULK frames, in order. The newcomer sends SYNC_ACK with the bytes received after every **16 KiB** and at the end. The member keeps at most a **window** of data sent but not acknowledged, between **16 KiB and 1 MiB**, and SHOULD adapt it to the queueing delay the transfer adds to the path, since voice and commands wait in the same queues. A fixed window is either slow on a long path or, on a slow one, fills those queues for seconds.

   The reference implementation, after LEDBAT (RFC 6817): chunks of 4 KiB (so that a voice frame behind one on a 2 Mbit/s link waits 16 ms), paced evenly over the round trip rather than sent in bursts; the delay is the smallest of the last three acknowledgements' round trips, less the smallest ever seen; the window starts at 32 KiB, grows by an eighth per round trip while that delay is under half the target, by a chunk per round trip below the target, and shrinks by a quarter, at most once per round trip, above it. The target is 50 ms by default (`Config::transfer_delay`). Measured on emulated access links with deep buffers, three members talking while one sends a 2 MB snapshot: on 20 Mbit/s and 20 ms each way, voice p99 48 ms against 45 ms without the transfer; on 2 Mbit/s and 40 ms, 140 ms against 87 ms (a fixed 256 KiB window: 920 ms); the snapshot takes 2 s and 10 s.
4. Once the snapshot is complete, the newcomer delivers it to its application, which replaces its state with it. Then, for every stream in the frontier, it sets its position to the frontier's: held messages below it are dropped as already contained in the snapshot, those from it on are delivered. If its own position in a stream was beyond the frontier (it had delivered messages the snapshot does not contain), it sends RESEND from the frontier to get them again. Streams absent from the frontier keep their positions.

Either side may send SYNC_ABORT: the member to decline, the newcomer to cancel. A transfer also fails when either side leaves, or after **15 s** without progress; the newcomer then delivers what it held and can ask another member. A receiver MAY refuse snapshots above a size of its choosing (the reference implementation: 64 MiB) by aborting.

The guarantee: the snapshot, followed by the command messages the newcomer delivers after it, is exactly what the member's application had applied, followed by everything since. Applying them gives the newcomer the member's state.

## 7. Behaviour

### 7.1 Connection loop

As SPEC.md §6.1: one connection at a time; reconnect with exponential backoff and jitter (the reference implementation: uniformly between half and all of `min(10 s, 250 ms × 2ⁿ)`); after 4409, report it and wait at least 30 s; after 4429, wait at least 10 s. Streams, positions, the incarnation and held messages survive reconnection; sender keys, challenges, counters and slots do not.

### 7.2 Sending without delay

A member SHOULD keep its own queue per class in front of its WebSocket and hand the socket frames from the most urgent class first. It SHOULD hand over chat and bulk frames only while the socket has little unsent (a browser's `bufferedAmount`, say under 16 KiB), and drop a voice frame when that is already over a few kilobytes. Then a command is never stuck behind more than one bulk frame, and a stalled link loses audio instead of building up delay.

Voice frames carry `seq`, the sender's count of audio frames of fixed duration (wrapping at 2¹⁶). A sender MAY skip silent frames, still counting them. Receivers use `seq` to place frames in time, and SHOULD play them out through an adaptive jitter buffer that discards frames to catch up after a stall rather than playing late.

The audio format is the application's. Implementations that want to interoperate with the reference SDK's voice use `hold[1] ‖ packet`, where `packet` is one Opus packet (48 kHz, mono, 20 ms frames by default; receivers take the duration from the packet) and `hold` is how many milliseconds (0–255) the sender kept the frame between capturing and sending it. A sender that skips silence keeps the last frames or two and sends them when speech starts, so that the start of a word is not clipped; `hold` lets receivers tell that delay from network jitter.

### 7.3 Consistency

Every member sees each stream in the same order, but streams from different members interleave differently at different receivers. Applications whose commands do not commute need a rule on top, such as:

- **Owner per object**: only one member may change a given object, so concurrent commands touch different objects and commute.
- **Authority**: one member (the host) applies commands that others send it as DIRECT requests, and broadcasts the results as its own commands. Everyone applies the host's stream; state transfer copies the host's state.

Commands are signed by nobody but arrive from an admitted sender, whose identity the application knows: that is what lets it check that a command comes from someone allowed to give it.

## 8. Limits and timers

| Name | Value | Enforced by |
|---|---|---|
| Members | ≤ 32 slots; the reference relays admit 10 | relay (4429) |
| Frame size | ≤ 16,384 bytes | relay (1009) |
| Message payload | ≤ 16,352 bytes | senders |
| Profile | ≤ 1,024 bytes | senders |
| Snapshot window | 16 KiB – 1 MiB unacknowledged, adapting to the delay it adds (50 ms by default) | providers |
| HEADS | 1 s after a member's latest stream message | senders |
| Waiting for a recipient that is behind | ≤ 5 s, then it is closed (4408) | relays that apply backpressure |
| `ping` interval | 10 s | members |
| Receive timeout | 25 s | members |
| Idle timeout | 40 s | relay (4408), optional |
| Join timeout | 10 s | relay (4408) |
| KEY retry | 5 s without admission | members |
| RESEND retry / give up | 3 s / 15 s | receivers |
| Transfer stall | 15 s | both sides |
| Wait after 4409 / 4429 | ≥ 30 s / ≥ 10 s | members |

## 9. Security considerations

**Against the relay and the network**, groups provide confidentiality and integrity of everything members send, authentication of every frame's sender (its sender key was signed by its identity), replay protection within a connection (counters) and across connections (fresh challenges), and identity hiding (identities, profiles and stream positions travel encrypted). The relay can drop, delay and cut connections, deliver one class ahead of another, and refuse service; members detect the gaps and recover from them as far as the senders still hold the messages.

**What the relay learns**: the group ID, slots, connection times and addresses, frame sizes, classes and timing, and so who is talking when, but not what is said. Real-time voice cannot hide that.

**Against members**: every member can read everything sent to the group and can send anything under its own identity. It cannot impersonate another member: frames are encrypted with keys only their sender has, announced under the sender's identity signature. Roles (owner, visitor) are the application's to enforce, by checking the sender of each command.

**Forward secrecy**: sender keys are fresh per connection, but they travel encrypted under a key derived from the group secret. Someone who records the traffic and later obtains the group secret can read it. Rotate the secret (move the group) when that matters.

**Invites** are bearer credentials for as long as the group lives (§2.2). A relay cannot tell members apart beyond their join tokens, so it cannot exclude anyone; exclusion means a new secret.

**Denial of service**: relays bound frame size, unauthenticated time, queues and per-connection rate (§3.3), and SHOULD limit connections per address as for rooms. A member can waste others' bandwidth up to the relay's rate limit, and no further.

## 10. Conformance

[`test-vectors/g1.json`](test-vectors/g1.json) gives, for fixed inputs, the group's derived keys and IDs, join tokens and JOIN frames, the KEY frames of two members, and one frame of every sealed kind: CONFIRM, stream messages, a direct message, voice, a datagram, HEADS, RESEND and the resend that answers it, RESENT, and a state transfer from request to acknowledgement, plus a declined one. Each frame was produced by a member of the reference implementation, and checked to have its effect at the other.

[`test-vectors/verify-groups.mjs`](test-vectors/verify-groups.mjs) rebuilds all of it from this document using only Node's built-in cryptography.

