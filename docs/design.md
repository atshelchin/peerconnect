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
- **Positions are kept per stream.** A receiver remembers its place in each of the last 16 streams that delivered something, and computes where the sender will resume by the sender's own rule. A first version kept one position; review showed that two instances of the same peer taking turns, or a relay replaying an old handshake, then moved it backwards and caused duplicates. A second version recorded a stream as soon as its handshake arrived; review showed that a relay replaying handshakes from 16 old instances could then push the live stream out of the list. Positions are now recorded only on an authenticated arrival, which a replayed handshake never reaches.
- **ACKs are cumulative and batched** (50 ms, or 32 messages, or 1 MiB), with a progress ACK every 10 s while a long message is arriving. A sender that hears nothing for 30 s with data in flight reconnects: a relay that silently drops the last frame of a session can otherwise stall the stream forever.
- **A 4 MiB window** caps what is unacknowledged in flight. That bounds what any relay has to hold, including Cloudflare's, which cannot apply backpressure.

What it cannot do: survive the sender's own process ending. The queue is in memory, as any persistence would make the SDK opinionated about storage. Applications that need that keep their own copies until they see the message acknowledged. Symmetrically, a *receiver* that restarts forgets what it had received but not yet acknowledged (at most 50 ms' worth), and gets it again: exactly once while both keep running, at least once across a receiver's restart.

## 5. Close is end-to-end; leave is just closing the socket

The draft lists `leave` and `close` as relay operations. But a stateless relay cannot *enforce* that a room is over: it would forget on restart, and either peer could simply rejoin. So:

- **leave** = close the WebSocket. The partner sees `PRESENCE(0)`. Nothing is lost.
- **close** = a `CLOSE` at the end of the stream, which the partner trusts because it is authenticated. Being part of the stream, it is resent until acknowledged, and the record says "closing" so that even a restarted peer carries on. The partner acknowledges, then lingers (at most 30 s, and 5 s more once the closer leaves), so that an acknowledgement lost with a dropped connection, on either side, is repeated by the next handshake rather than lost for good. Finishing the moment the closer left was the first version; it stranded a closer whose connection dropped just before the acknowledgement arrived. An inviter closing before pairing completes simply stays at the invite ID until it is told.

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
| More than two peers in a room | Rooms stay two-peer; groups (§11) are a separate protocol for more. |
| RPC, pub/sub | Out of scope, as in the draft. |

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
| Linux relay | `relay::Conn` and `relay::GroupConn`, frames, auth | hyper + tungstenite. Per connection, one reader task that forwards directly into the partner's byte-budgeted outbox, and one writer task. A per-room mutex protects the seats. Reserving budget *before* checking the seats gives backpressure without ever letting a replaced connection's frame through. |
| Cloudflare relay | `relay::Conn`, frames, auth | One Durable Object per room. Sockets are held through the Hibernation API; each socket's seat lives in its attachment, so an evicted object rebuilds its view from the sockets on the next event. Nothing is written to storage. An alarm exists only while a connection has yet to join; idle timeouts are checked when the room wakes. Unjoined connections are capped per client address. |

Because both clients run the same state machine, the deterministic simulation in `crates/peerconnect/tests/simulation.rs` covers the JavaScript SDK's protocol behaviour too. The same end-to-end suite runs against both relays.

## 11. Groups

Groups ([GROUP.md](../GROUP.md)) came later, for game squads and shared worlds: up to four members, commands, chat, voice, video, and newcomers that must catch up from the others. Rooms did not change. A group is a different protocol on a different endpoint (`/g1/`), because almost every choice that suits two peers is wrong for several talking at once.

**Four at most.** Groups began with 32 slots, and relays admitted 10. With voice and video, every member sends to every other, so what each one receives grows with the group, and on a phone link ten cameras do not fit. Squads and small calls are what groups are for; four is part of the protocol (GROUP.md §3.2), so every relay and every client agrees on it, and the WELCOME bitmap kept its four bytes so that the wire format did not change.

**Why not a Noise handshake per pair.** Pairwise sessions grow with the square of the group (ten members would need 45), each voice frame would be encrypted once per receiver, and the relay would fan nothing out. Instead every connection draws a random **sender key** and announces it once, under a key derived from the group secret and signed by the member's identity. Each frame is then encrypted once and copied by the relay to everyone, unchanged. A per-type counter in each frame makes its nonce, so a relay can drop and delay frames, and deliver one class ahead of another, but not replay or reorder within a class.

**Why a challenge in every admission.** A KEY frame proves who sent it, but not when: a relay could replay a whole recorded session to a member that has just started. So each connection has a fresh random challenge, and a member is admitted only once it has echoed it (KEY, KEY with echo, CONFIRM: one and a half round trips). The relay can still pretend someone is present, but cannot make a member accept a single frame from them.

**Why identities.** In a room, the two peers are the room. In a group, applications need to know who did what: which player owns this block, who may command this agent, who is only visiting. Members are Ed25519 keys that the application keeps, and the relay never sees them. Their join token (an HMAC of the group ID under the identity's seed) lets the relay put a reconnecting member back in its own slot, replacing its dead connection at once, without learning who it is or linking it across groups.

**Classes, and no backpressure by default.** In a room, a slow peer holds back its partner, and that is what flow control is for. In a group, a slow member must not hold back nine others. So the relay never waits on a frame's behalf: voice for a member that is behind is dropped, and reliable frames are queued. A member that falls more than 256 KiB behind on commands and chat stops the relay reading from whoever is sending to it, for at most 5 s, and is then dropped. The first version dropped such a member as soon as 1 MiB queued; a test that sent a fast member 3 MB in a burst showed that this disconnected healthy members too. Four queues per recipient, drained most urgent first, are what keep commands ahead of chat and state transfer. They only help if the queue is in the relay, not in the kernel, so the relay sets `TCP_NOTSENT_LOWAT`, and the deployment sets it for its TLS proxy.

**Streams without acknowledgements.** Rooms acknowledge every message, to free the sender's queue and pace the window. Every member acknowledging everything to everyone would be a large part of the traffic. Group streams have no ACKs: senders keep a bounded history of what they sent, and receivers that see a gap ask for it again (RESEND, answered in bursts of 128 KiB, so that one catching-up member never floods the relay or delays other commands for long). A gap shows up as a message beyond the expected one, or as a head announced in KEY, CONFIRM or HEADS. HEADS was added after the simulation showed that a relay that silently drops the last messages of a stream would otherwise go unnoticed until the sender spoke again.

**Snapshot and frontier.** A newcomer has to start from somewhere. Logs grow without bound, and the relay keeps nothing, so a member that already has the state sends it. The difficulty is the seam: commands keep coming while the snapshot is taken and sent. The answer is that the snapshot records the **frontier**, the provider's position in every command stream at the moment its application took it, and the newcomer holds back live commands until the snapshot arrives. Then each stream restarts at the frontier: held commands below it are in the snapshot, those above it are delivered, and any the newcomer had already passed are asked for again. The simulation runs this with commands from three members in flight throughout, and with a provider that had itself lost some of them.

**Voice over TCP, at first.** Groups began on WebSockets. Over a WebSocket, a lost packet holds up the stream until TCP resends it. Everything else is arranged so that this costs a glitch rather than lasting delay: frames are 20 ms of Opus at 24 kbps, nothing is sent during silence, senders drop frames their socket cannot take, relays drop frames a recipient cannot take, and the receiver's jitter buffer drops whole frames to catch up after a stall instead of playing them late. Each frame carries how long its sender held it (frames kept to avoid clipping the first syllable), because otherwise those frames look like network jitter, and in the first browser test they inflated the buffer from 30 ms to 120 ms.

**QUIC, and what it took.** On a lossy path, TCP turns every lost packet into a stall for everything behind it. Measured with the kernel dropping 3% of packets on each hop, voice p99 was 138 ms over a WebSocket against 53 ms over QUIC, where each class has a stream of its own and voice goes in datagrams, which wait for nothing. The Linux relay serves both on one UDP port (WebTransport for browsers, raw QUIC for everything else) and keeps the WebSocket: the Cloudflare Worker has no UDP, and some networks block it, so members give QUIC a 1.5 s head start and take whichever opens. Raw QUIC does not check a `quic://` relay's certificate, since the protocol never trusted the relay, which is what lets a relay run on a bare IP address. The relay itself barely changed: its outbox, per-class queues and backpressure stayed as they were, with a writer per class instead of one per socket.

The members had to change, because a WebSocket had been giving them an order that QUIC does not: one class after another, from a relay that sent nothing before WELCOME and nothing from a connection after its replacement was announced. With a stream per class, a voice datagram can beat WELCOME (it used to force a reconnection), voice frames can overtake each other (the strict counter check discarded them as replays: it is now a 64-frame window, as in IPsec and DTLS, for the voice class only), and snapshot data can beat its SYNC_BEGIN (it was dropped, and the transfer stalled: in the first load test over QUIC, one or two snapshots in five never arrived). The other cases were already safe: frames from a replaced connection fail to decrypt under the new key, and stream gaps are resent. The deterministic simulation now has a mode that delivers as QUIC might, classes overtaking each other at random and nothing purged, and every group test passes in it over hundreds of seeds; with the snapshot fix undone it fails 47 seeds in 50.

The cost is CPU: every datagram is a packet to encrypt and acknowledge, where a WebSocket coalesces many frames into one write. On Linux, with segmentation offload, the relay uses about 1.8 times the CPU for the same game traffic.

**QUIC only, in the end.** The WebSocket stayed for a while as the fallback, and the Cloudflare Worker as a second relay. Real use settled it. From China, the Worker was served from Amsterdam (215 ms to connect over TCP): browser video took p50 217 ms and p99 about 500 ms from one member to another, and lost a quarter of its frames at 800 kbit/s, while voice stuttered now and then. A Linux relay in San Jose, over WebTransport, gave p50 139 ms and p99 146 ms at 1.5 Mbit/s with nothing lost, which is the round trip and no more. A fallback that works is also a fallback that hides: a member on it gets the stalls TCP brings, and blames the product. So groups are QUIC only (GROUP.md §3.6), and a member that cannot use UDP does not join, which the owner preferred to joining with stuttering voice and video. Who that leaves out is known: runtimes without WebTransport (the SDK says so up front), networks that block UDP, and, the case met first, browsers behind an HTTP or SOCKS proxy, since QUIC does not go through one: the same Chromium opened a WebTransport session in 429 ms directly and failed through the system proxy, so the demo tells such users to add the relay's host to the proxy's exceptions. The Worker serves rooms only now; it could never take QUIC, drop late voice or put commands first, and it billed every video fragment as a message. Native engines lost the option of feeding the state machine from their own WebSocket, so the C library gained `pc_client`, which runs the Tokio client and its QUIC on threads of its own.

**Video in layers.** With one picture per sender, everyone receives everyone's picture at the size it was sent, though in a gallery of four each tile shows a quarter of the screen and a 720p picture there wastes most of its pixels, and one member on a slow link holds every sender's bitrate down. Meeting apps answer with simulcast and a selective forwarding unit, and so do groups, without giving up end-to-end encryption: a sender encodes the same picture in up to four layers, the layer and whether it is a keyframe are in the frame's type (authenticated, but readable by the relay), and each member tells the relay which layer of whose video it wants (SUBSCRIBE), from the size of the tile it draws it in. The relay forwards each member that layer, or the nearest one sent, and moves a member to another layer only at a keyframe of it, forwarding the old one until then, so that the picture never stops; and it tells each sender which of its layers anyone wants (DEMAND), so that nobody encodes a large picture for nobody. A receiver plays one layer per track and moves at the new layer's keyframe. The decisions are one function of what the relay has seen, `VideoRouter`, shared by the Linux relay and the simulation, which caught that a layer newly wanted must start with a keyframe: without one, members waited for the next periodic keyframe, two seconds, before seeing the larger picture. In a call of four at 720p, each member now receives one large picture and two small ones, and the relay forwards about half what it did.

The same router fits each member's link. The relay already queues video per member and drops the oldest past a budget, so that queue is the signal, with nothing to estimate: video dropped, or video queued for 300 ms beyond both 32 KiB and a quarter of a second at the pace it leaves (the second test came with 1080p: a keyframe of 150 KB fills the queue of a perfectly good link for a moment, and a fixed byte threshold read that as trouble), and the member is moved below the largest layer it gets, that layer stopping at once, down to no video at all; it is told so (LIMIT), and asks the senders for keyframes of what it will get now. After 8 s of keeping up it is given one more layer, and a try that fails makes the next wait twice as long, so that a link at the edge does not flap. Through an emulated 1 Mbit/s downlink, a member watching 720p was moved to 360p within half a second, while a member on a fast link kept 720p, and went back to 720p by itself once the limit was lifted. Senders apply the same rules to their own uplink, dropping their largest layer first.

**Two tracks, and a budget.** A member who shares a screen still wants to be seen, so a member may send two videos, and they must be routed apart: someone watching the screen large wants its 1080p, while the presenter's camera beside it wants a thumbnail. The track (0 or 1) went into the frame type too, which the sixteen video types had room for, and subscriptions, demand and the router became per track; two tracks is what meetings need, and keeps the type within its range. The relay's own server has limits as well: a month's transfer and a link. Rather than refuse anyone, the relay lowers a ceiling on everyone's video as it nears them (layers up to 1 from 80% of the month, layer 0 from 95%, none at 100%, a layer at a time while over its rate), through the same LIMIT members already handle; voice, commands and chat are never touched, so a call over budget turns into a voice call, not a dead one.

**A screen that stuttered.** The first shared 1080p screen played like slides, and real Chrome against the relay in San Jose showed four causes, each found by measuring rather than guessing. The Mac's hardware H.264 encoder sometimes produced nothing for seconds at 1080p, so the sender now gives a hardware encoder a second before falling back to software. A 1080p screen's keyframe is 200–450 KB, over the 255 fragments a frame then had, so it was never sent and receivers waited for the next one; fragments are now numbered in two bytes, up to 1 MB a frame. A receiver asked for a frame's missing fragments twice per frame, but such a keyframe takes longer than the 280 ms round trip to arrive, so what was lost late was never asked for, and the sender dropped resends while video was queued anyway; each fragment is now asked for twice, and resends go first. When the relay moved a member back to a layer it had had before, the member waited for the frames of that layer it never got, one hold time each, four seconds in all; it now starts at the layer's keyframe. Two smaller things went with them: a screen's keyframes came every 5 s and the picture paused for the 300 ms each takes to send, so they come every 20 s (receivers ask when they need one); and the camera paused with it, waiting behind the screen's keyframe in one queue, so the two tracks now take turns. On an emulated 140 ms path, the screen now plays from its first seconds, at 1080p once the relay tries it again, pausing only at a keyframe. One thing stays as it was. A new connection on a 140 ms path, its congestion window still small, cannot take the first keyframes of every layer at once, so the relay takes the link for slow and holds it at 720p for its first eight seconds or so. Both remedies tried were worse for someone. BBR instead of CUBIC avoided the move, but on a 1 Mbit/s link it kept sending into the loss rather than queueing, so the relay never saw that the link was slow, and its member lost frames instead of getting smaller ones. Trying such a link again after three seconds instead of eight gave a link that really is slow two more seconds of video it could not take, a stall of seconds, to spare a fast one some seconds of 720p.

**A certificate anyone can compute.** Browsers will not use WebTransport without a certificate they trust, which normally means a domain, and a domain can mean paperwork. They do accept one nobody signed if told its hash in advance and if it is valid for at most 14 days. Since nothing in a group depends on the relay's certificate, a relay without one serves a certificate that is a pure function of the week: its key is derived from public data and its signature is deterministic (RFC 6979), so the browser SDK computes the hashes of last week's, this week's and next week's and pins them. A relay reachable only by its IP address then works from any browser page, on any port, with nothing to configure or distribute. The key being public changes nothing: anyone on the path could already pose as the relay to raw QUIC members. It costs about 50 KB of WebAssembly, for P-256.

**Video as whole frames over datagrams.** Video is real-time like voice, but its frames are large and depend on each other: a keyframe of 20 KB, then deltas that decode only after every frame since. So the core cuts frames into fragments that each fit a datagram, and receivers put them back together and deliver only whole frames, in order, and only decodable ones. A missing fragment is asked for once (NACK, after 5 ms) and resent from the sender's last half second; a frame still incomplete after 300 ms is given up, and the receiver drops what follows until a keyframe, which it asks for. Video gets a class of its own, after chat: it is the one real-time class that can fill a link, and above chat it would starve it. On QUIC, video and voice share the datagram queue, which leaves in order, so video only goes in while that leaves room for voice; the rest waits in a video queue that drops its oldest. In headless Chromium with 2% of UDP lost each way, every frame decoded, at p99 94 ms; over TCP stalling as with the same loss, 94% of them, at p99 194 ms. The first version waited 20 ms before asking for a fragment and started with an empty resend allowance, so the first loss could not be answered: then only 82% decoded. The test vectors caught the second.

**State machine first, again.** As with rooms, everything is in `group::Member`, a state machine without I/O. The Tokio client, the JavaScript SDK and the C library are thin drivers over it. A game engine with its own loop drives it directly, which is how a Godot GDExtension uses it without a second runtime.

**State transfer must not fill the queues voice waits in.** The first version sent snapshots with a fixed 256 KiB window. On an emulated 2 Mbit/s access link with a deep buffer, as real home and mobile links have, that put a second of snapshot ahead of every voice frame from the member sending it: voice p99 went from 89 ms to 920 ms. Priority queues cannot help once the data has left them for the link. A larger window was faster on long paths and worse on slow ones (over 2 s). What works is to measure the delay the transfer adds and keep it small, as LEDBAT does for background downloads: acknowledgements every 16 KiB (at 64 KiB, one acknowledgement already measured 256 ms of the transfer's own serialisation on that link), 4 KiB chunks paced over the round trip instead of bursts, a window that starts at 32 KiB, grows by an eighth per round trip (doubling overshot by a whole round trip of queue before the delay showed) and shrinks by a quarter once per round trip when the queue exceeds 50 ms. Voice p99 while loading is now 144 ms on 2 Mbit/s, 59 ms on 20 Mbit/s (47 ms without a transfer), and the transfer runs at 80–90% of the link. The price is on long, jittery paths, where the delay signal is noisy and the transfer slows down; the target is configurable for applications that would rather load fast.

