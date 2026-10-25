# PeerConnect

**End-to-end encrypted rooms and groups over an untrusted, stateless WebSocket relay: two peers with reliable delivery, or up to 32 members with commands, chat, voice and state transfer.**

PeerConnect connects two applications — a phone and a laptop, a web app and a wallet, two services in different networks — through a relay that cannot read, forge or replay what they say, and that needs no database. One side creates a room and hands the other an invite (a QR code, a link); from then on the two can reconnect whenever they like, from wherever they are, and messages sent while one of them is away arrive when it comes back.

```
  App A ── PeerConnect ─┐                      ┌─ PeerConnect ── App B
                        │   Noise IKpsk1 E2EE  │
                        └──── WebSocket relay ─┘
                              (two seats per room,
                               no storage, no keys)
```

- **Small protocol.** Six frame types, one handshake, seven close codes. The [specification](SPEC.md) fits on a few pages.
- **Untrusted relay.** Messages are encrypted and authenticated end to end with the Noise handshake `IKpsk1`. The relay sees sizes and timing, never contents or identities.
- **Stateless relay.** A room exists on a relay only while someone is connected. A relay can restart, or be replaced by another one, and peers simply reconnect: the room is defined by keys the peers hold, not by anything stored on the server.
- **Reliable, in order, exactly once.** Messages are numbered and kept by their sender until acknowledged, then resent after any disconnection, so nothing is lost or duplicated while both peers keep running. The sender is the offline queue; the relay stores nothing.
- **Invites expire by design.** Once paired, the peers move to a room ID derived from their two identities. A leaked invite reaches nobody.
- **Groups for games and teams.** Up to 32 members per group (10 by default) with [their own protocol](GROUP.md): commands, chat and voice that never get in each other's way, members known by their own keys, and state transfer that brings a newcomer up to date from the members already there. See [Groups](#groups).
- **One codebase.** A Rust core with no I/O runs the Linux relay, the Cloudflare relay, the Rust client, the C library for game engines and, compiled to WebAssembly, the JavaScript client.

## Quick start

### Run a relay

**Docker**, behind any TLS-terminating proxy:

```sh
docker build -t peerconnect-relay .
docker run -p 8080:8080 peerconnect-relay          # ws://localhost:8080
```

[`deploy/docker-compose.yml`](deploy/docker-compose.yml) adds Caddy for automatic HTTPS: `DOMAIN=relay.example.com docker compose -f deploy/docker-compose.yml up -d` gives you `wss://relay.example.com`.

**Cloudflare Workers** (one Durable Object per room, using WebSocket Hibernation, so idle rooms cost nothing):

```sh
cargo install worker-build
cd crates/peerconnect-worker && npx wrangler deploy
```

**Binary**: `cargo install peerconnect-relay`, then run `peerconnect-relay`, configured with `PEERCONNECT_LISTEN`, `PEERCONNECT_MAX_CONNECTIONS`, `PEERCONNECT_MAX_PER_ADDRESS` (default 256, IPv6 counted per /64) and `PEERCONNECT_TRUST_PROXY=1` behind a proxy that sets `X-Forwarded-For`.

### JavaScript (browsers, Node.js 22+, Deno, Bun)

```sh
npm install peerconnect
```

```js
import { connect, Room } from "peerconnect";

// Peer A creates a room and shows the invite (e.g. as a QR code).
const room = Room.create("wss://relay.example.com");
showQrCode(room.invite);

const conn = connect(room);
conn.addEventListener("paired", () => save(conn.room.toString())); // persist: it is a secret
conn.addEventListener("state", () => console.log(conn.state));     // offline | waiting | connected | closed
conn.addEventListener("message", (e) => console.log(new TextDecoder().decode(e.data)));

// Peer B scans it, and saves its room right away (it holds the identity A will pin).
const joined = Room.join(scannedInvite);
save(joined.toString());
const connB = connect(joined);
connB.addEventListener("paired", () => save(connB.room.toString()));
connB.send("hello");                  // queued; delivered as soon as A is reachable
connB.addEventListener("delivered", (e) => console.log("A has everything below", e.detail));

// Later, after a restart: connect(savedString) and carry on. Both sides reconnect on their own.
// To end the room: conn.close(), save conn.room (it is now "closing"), delete it once closed.
```

The WebAssembly is inlined, so the package loads without bundler configuration (46 KB gzipped). To build it from source: `sh js/scripts/build.sh` (needs `wasm-bindgen-cli`).

### Rust

```toml
peerconnect = { version = "0.1", features = ["client"] }
```

```rust
use peerconnect::{client, Event, Room, State};

let room = Room::join(&invite.parse()?);
save(&room.to_string());               // before connecting: it holds the identity the inviter pins
let mut conn = client::connect(room);
conn.send(b"hello").await?;            // queued until the inviter is reachable
while let Some(event) = conn.next().await {
    match event {
        Event::Message(data) => println!("{}", String::from_utf8_lossy(&data)),
        Event::Paired => save(&conn.room().to_string()),
        Event::State(State::Closed(_)) => break,
        _ => {}
    }
}
```

Without the `client` feature the crate does no I/O: [`Peer`](crates/peerconnect/src/peer.rs) is a state machine that any WebSocket library and event loop can drive. Try the terminal chat:

```sh
cargo run -p peerconnect --features client --example chat -- new ws://localhost:8080 room-a
cargo run -p peerconnect --features client --example chat -- join '<invite>' room-b
```

## Groups

A group is a squad, a team, a shared world: everyone holding its invite can join, up to 32 members at once (the relays admit 10 by default). It is a separate protocol, [GROUP.md](GROUP.md), on its own endpoint (`/g1/<id>`), served by the same relays.

- **Members are keys.** Each member has an Ed25519 identity that the application keeps (one per player, say). Others see it, proven, as the member's ID, so the application can decide who owns what and who is only visiting. The relay never sees identities.
- **Four classes of traffic, most urgent first:** commands, voice, chat, bulk. Each has its own queue on the sender, in the relay, and its own counters in the encryption. A burst of chat or a state transfer never holds up a command, and voice never waits behind chat.
- **Commands and chat are reliable streams.** Numbered, kept by the sender, resent after a reconnection, delivered to everyone (the sender included) in order and exactly once while both keep running. Messages sent while offline go out on reconnection.
- **Voice and datagrams are real-time.** Anyone along the way drops them rather than delay them: the sender when its socket backs up, the relay when a recipient is behind. Late audio is worse than lost audio. Datagrams carry things like positions, where only the latest value matters.
- **State transfer.** A newcomer asks a member for a snapshot of the application's state. The SDK records exactly which commands the snapshot contains, holds back the live ones meanwhile, and then delivers the snapshot followed by every later command, and nothing twice. The snapshot is the application's bytes: a saved world, a document, a game state.
- **Untrusted relay, again.** Every frame is encrypted with a fresh per-connection key that its sender's identity signs for; admission uses a fresh challenge, so a relay cannot replay an earlier session. The relay sees slots, sizes and timing.

How to build a multiplayer game or collaborative app on top, including who decides what when two members change the same thing, is in [docs/multiplayer.md](docs/multiplayer.md).

### JavaScript

```js
import { Group, Identity, joinGroup } from "peerconnect";
import { startVoice } from "peerconnect/voice";

const group = Group.create("wss://relay.example.com"); // group.toString() is the invite: hand it to the members
const me = Identity.generate();                         // persist me.seed to be recognised next time
const conn = joinGroup(group, { identity: me, profile: "alice" });

conn.addEventListener("joined", (e) => console.log("joined:", e.detail.member));
conn.addEventListener("message", (e) => {
  const { from, channel, data } = e.detail;             // channel: "command" | "chat"; our own messages too
  if (channel === "command") applyCommand(from, data); else showChat(from, data);
});
conn.send("command", JSON.stringify({ op: "place", block: 7 }));
conn.send("chat", "on my way");

// Bring a newcomer up to date: members answer with a snapshot of their state…
conn.addEventListener("syncrequest", (e) => conn.provideSync(e.detail.from, e.detail.transfer, saveWorld()));
// …and the newcomer asks one of them.
conn.addEventListener("snapshot", (e) => loadWorld(e.detail.data)); // later commands follow, exactly once
conn.requestSync(hostId);

// Voice (call from a click): Opus over WebCodecs, sent only while speaking.
const voice = await startVoice(conn);
voice.addEventListener("speaking", (e) => highlight(e.detail.member, e.detail.speaking));
voice.setMuted(true);
```

### Rust and game engines

```rust
use peerconnect::client::group;
use peerconnect::group::{Channel, Config, Event, Identity};

let config = Config { profile: b"alice".to_vec(), ..Config::default() };
let mut conn = group::join(invite.parse()?, identity, config)?;
conn.send(Channel::Command, b"place 7".to_vec()).await?;
while let Some(event) = conn.next().await {
    match event {
        Event::Message { from, channel, data, .. } => apply(from, channel, &data),
        Event::Voice { from, seq, data } => jitter_buffer.push(from, seq, data),
        _ => {}
    }
}
```

Engines with their own event loop drive [`group::Member`](crates/peerconnect/src/group/member.rs) directly, without Tokio: it is a state machine, fed by any WebSocket and polled once per frame (Godot's `WebSocketPeer` from a GDExtension, for example). Other engines and languages (Unity, Unreal, C, C++) use the same machine through the C library, [`crates/peerconnect-ffi`](crates/peerconnect-ffi) ([header](crates/peerconnect-ffi/include/peerconnect.h)). The terminal group chat: `cargo run -p peerconnect --features client --example group -- new ws://localhost:8080 alice`.

### Latency and load

Measured on one Apple M2 Pro with the [game load test](crates/peerconnect-relay/examples/game_load.rs): every player is a real end-to-end encrypted WebSocket client; each sends its position 20 times a second, a game command twice a second and a chat line every 10 s; three players per group talk at once (50 voice frames a second); halfway through, a newcomer joins every group and loads a 2 MB snapshot. Latency is from the sender's `send` to the receiver's event.

| Load (Linux relay, release build) | Commands p50 / p99 | Voice p50 / p99 | Voice p99 while snapshots load | Relay |
|---|---|---|---|---|
| 10 groups, 100 players | 0.95 / 2.6 ms | 0.80 / 2.4 ms | 6.0 ms | 0.4 cores, 6 MB |
| 50 groups, 500 players | 0.92 / 3.5 ms | 0.77 / 3.3 ms | 2.7 ms | 1.7 cores, 19 MB |
| 100 groups, 1,000 players | 1.6 / 17 ms | 1.4 / 15 ms | 19 ms | 4.1 cores, 35 MB |

Everything reliable arrived, exactly once (save what was still in flight when the run stopped); so did the voice and positions. At 100 groups the machine is saturated: the load generator itself uses five to six of its cores. That forwarding is about 70,000 frames a second in and 335,000 out.

Real networks, emulated per player: each player behind its own access link of the given rate and one-way delay, with a deep buffer like real home and mobile links (`--shape`), three players and a newcomer loading 2 MB:

| Link per player | Voice p50 | Voice p99 | Voice p99 while the snapshot loads | Snapshot |
|---|---|---|---|---|
| 20 Mbit/s, 20 ms each way | 43 ms | 47 ms | 59 ms | 2.1 s |
| 2 Mbit/s, 40 ms each way | 84 ms | 89 ms | 144 ms | 10.4 s |
| 1 Mbit/s, 60 ms each way | 125 ms | 131 ms | 197 ms | 21 s |

The path's own delay is the floor (40, 80 and 120 ms there); the relay and SDK add about a millisecond. State transfer paces itself by the delay it adds ([GROUP.md §6](GROUP.md)): with a fixed 256 KiB window instead, voice p99 on the 2 Mbit/s link was 920 ms while the snapshot loaded.

Browser voice (headless Chromium, 20 ms Opus frames, through the relay):

| Measurement | Result |
|---|---|
| End of capture to start of playout | p50 18–29 ms |
| Through a link adding 0–60 ms of jitter each way | p50 110–140 ms; the jitter buffer adapts to about 120 ms; nothing lost |
| After a 1 s stall | back to p50 50–70 ms within 2 s (frames dropped to catch up, not played late) |

Mouth to ear adds the 20 ms frame being captured and the audio hardware's own latency (about 10–30 ms), plus the network. Through the Cloudflare Worker in local `workerd`, a voice frame takes p50 1.7 ms, p99 5.9 ms; deployed, a Durable Object sits in one location, so players far from it pay the distance, and it can neither put commands first nor drop late voice (see Limitations).

## How it works

1. **Room.** The inviter draws a random 32-byte room secret. From it derive an Ed25519 key whose public half is the **invite ID**, and a pre-shared key for the handshake. The invite is `relay URL # base64url(secret ‖ inviter's public key)`.
2. **Joining the relay.** A peer connects to `<relay>/v1/<room id>`, receives a random challenge and answers with a signature by that ID's key. The relay checks the signature against the ID itself; it stores nothing and never sees the secret. Each room ID has two seats, inviter and joiner; a newer connection to a seat replaces the older one, and the relay tells each side whether the other is present.
3. **Handshake.** Whenever both seats are filled, the joiner starts a Noise `IKpsk1` handshake. It authenticates both identities, requires the room secret, and gives forward secrecy. The first time, the inviter **pins** the joiner's identity.
4. **Pairing.** As soon as the joiner's first message arrives, both peers move to the **pair ID**, whose key comes from the room secret and the Diffie-Hellman of their identities. Nobody else can compute it, so the invite now leads nowhere. The move tolerates any lost message: an unpaired joiner simply looks at both IDs in turn.
5. **Messages.** A reliable stream. Messages are numbered, encrypted with ChaCha20-Poly1305, acknowledged, and resent after reconnection from where the other side says it got to, with resume state carried inside the handshake. At most 4 MiB is unacknowledged in flight. Messages of up to 16 MiB are split into 64 KiB frames.
6. **Reconnection.** Any failure (network, relay restart, timeout, tampering) leads to reconnecting with backoff, a fresh handshake, and resuming the stream. Peers ping the relay every 25 s so dead connections are noticed on both sides.

The design decisions, and how they differ from the original draft, are explained in [docs/design.md](docs/design.md).

## Repository

| Path | What |
|---|---|
| [`SPEC.md`](SPEC.md) | The protocol, version 1 |
| [`GROUP.md`](GROUP.md) | Groups, version 1 |
| [`test-vectors/`](test-vectors) | Conformance vectors, and an independent checker written from the spec |
| [`crates/peerconnect`](crates/peerconnect) | Protocol core (no I/O), plus the optional Tokio client |
| [`crates/peerconnect-relay`](crates/peerconnect-relay) | Relay for Linux and Docker (Tokio, hyper, tungstenite) |
| [`crates/peerconnect-worker`](crates/peerconnect-worker) | Relay for Cloudflare Workers and Durable Objects |
| [`crates/peerconnect-wasm`](crates/peerconnect-wasm), [`js/`](js) | JavaScript SDK, and browser voice ([`js/voice.js`](js/voice.js)) |
| [`crates/peerconnect-ffi`](crates/peerconnect-ffi) | C library for game engines and other languages |
| [`deploy/`](deploy) | docker-compose with automatic HTTPS |
| [`website/`](website) | [peerconnect.dev](https://peerconnect.dev): introduction, guide, specification and live demo, in English and Chinese |

## Testing

```sh
cargo test                              # unit tests, deterministic simulation, end-to-end over real sockets
node test-vectors/verify.mjs            # re-derives every test vector with node:crypto only
node test-vectors/verify-groups.mjs     # the same for groups (g1.json)
PEERCONNECT_RELAY=ws://127.0.0.1:8787 cargo test -p peerconnect-relay --test e2e   # same suite against any relay
cd js && npm run build && npm test      # JavaScript SDK, including interop with the Rust client
cd js && npm run test:voice             # browser voice in headless Chromium: latency, jitter, stalls
cargo build --release -p peerconnect-relay --examples && target/release/examples/game_load \
  --spawn target/release/peerconnect-relay --groups 100 --seconds 20   # a game's traffic, over real sockets
```

What is covered:

- **Noise.** The handshake and transport are byte-identical to the [`snow`](https://github.com/mcginty/snow) library.
- **Test vectors.** Every value and wire byte is reproduced by an independent implementation built only from `SPEC.md` (`test-vectors/verify.mjs`).
- **Simulation.** A deterministic simulation drives two peers and a relay through:
  - pairing and the move to the pair ID, including a lost confirmation and an inviter restarted mid-pairing;
  - messages queued while apart, and no loss or duplication when connections are cut every few frames, frames vanish silently, or old handshakes are replayed;
  - randomized chaos (cuts, relay restarts, tampering, peers restarted from their records) checked against delivery properties over hundreds of seeds;
  - the window and the outbox limit;
  - closing a room while the other peer is away;
  - relay restarts, replacement, keepalive loss, lost handshakes, tampering and reconnection storms.
- **Flow control.** Regression tests send 16 MiB both ways at once, echo from inside the event loop, starve a sender with a peer that stops reading, and (opt-in, 80 s) trickle 16 MiB to a slow reader.
- **End-to-end suite.** The same tests pass against the Linux relay and against the Cloudflare Worker running in local `workerd`, including a long idle test that confirms Durable Object hibernation and recovery.
- **JavaScript SDK.** Tested in Node 22, Deno 2.7, Bun 1.4 and headless Chrome 155, against both relays, and talking to the Rust client.
- **Groups.** A deterministic simulation (`crates/peerconnect/tests/group.rs`) runs up to ten members through admission, reconnection, backlogs resent in bursts, messages beyond what a sender keeps, replacement, a full group, voice floods, state transfer while commands flow (also from a member that is behind), a failed transfer, a relay that loses the end of a stream, and a relay replaying an earlier session. End-to-end tests (`crates/peerconnect-relay/tests/group_e2e.rs`) pass against both relays: ten members talking, a relay restart, a 4 MiB state transfer, the relay's wire behaviour, a member that stops reading, and commands overtaking a backlog of chat. The C library is driven through its C functions against a real relay, and its header compiled from C. Every kind of group frame is in the test vectors (`test-vectors/g1.json`), produced by the implementation and reproduced by an independent one written from GROUP.md (`verify-groups.mjs`). Line coverage of the group code by these tests: the member state machine 97%, the relay 99%, the Tokio driver 97%, the C library 92% (`cargo llvm-cov`). The [game load test](crates/peerconnect-relay/examples/game_load.rs) runs up to a thousand players over real sockets, optionally behind emulated access links.

## Performance

Linux relay, release build, measured on one Apple M2 Pro (12 cores) with the load generator ([`examples/bench.rs`](crates/peerconnect-relay/examples/bench.rs)) running on the same machine:

| Measurement | Result |
|---|---|
| Relay round trip, 1 room, 64 B | p50 53 µs, p99 135 µs |
| Relay round trips, 100 rooms concurrently | 75,000 /s, p50 1.3 ms, p99 2.5 ms |
| Forwarding, 1 KB frames, 100 rooms | 1.7 M frames/s (1.8 GB/s) |
| Forwarding, 64 KB frames, 1 room | 3.1 GB/s |
| New connections (TCP + WebSocket + Ed25519 check) | 16,000 /s, p50 3.7 ms |
| Memory per idle connection | 8 KB (82 MB for 10,000) |
| End-to-end encrypted round trip, 1 room | p50 65 µs |
| End-to-end round trips, 100 rooms concurrently | 76,000 /s, p50 1.3 ms |
| Pairing (both connect, join, handshake) | p50 0.9 ms |
| End-to-end reliable bulk transfer, 1 room (1 MiB messages) | 430 MB/s |

These are loopback numbers: they show the protocol's and the relay's own overhead, which is far below any real network's. Over a real network, one room's throughput is at most the 4 MiB window per round trip (40 MB/s at 100 ms). Reproduce with `cargo run --release -p peerconnect-relay --example bench -- ws://127.0.0.1:8080 <scenario>` (raise `PEERCONNECT_MAX_PER_ADDRESS` first: the load comes from one address).

## Limitations

- **The queue lives in the sender's memory.** Messages survive disconnections, relay restarts and any number of reconnections, but not the sending process: messages not yet acknowledged when it stops are lost. Applications that need delivery across restarts keep a copy until they see `Delivered`. Likewise a receiver that restarts may get again what it had received in its last ~50 ms: exactly once while both run, at least once across restarts.
- **Ending a room needs both peers online once more.** `close()` keeps trying (and a closing record resumes after a restart) until the other peer acknowledges. A peer that never comes back while the closer is still trying is never told.
- **Before pairing, the invite is a bearer credential.** Whoever uses it first pairs; the race is visible, since the rightful joiner never connects, but it is not prevented. Send invites over a private channel. After pairing they are useless.
- **One relay per room.** If it is down, peers wait. They can move a room to another relay by agreeing on a new URL; there is no automatic failover.
- **Cloudflare relay.**
  - It enforces the idle timeout lazily (when a room next wakes up), to keep hibernating rooms free.
  - It cannot apply backpressure (the runtime does not say how much it buffers). The 4 MiB window bounds honest peers; a dishonest one can at worst exhaust its own room's Durable Object.
  - Unjoined connections are capped per client address (4) and per room (64). Someone who knows a room ID and controls many addresses can still delay the room's peers; add Cloudflare rate-limiting rules if that matters.
- **Groups.**
  - Voice runs over TCP (WebSocket), where a lost packet holds up what follows it until it is resent. The jitter buffer, the dropping of late frames and small kernel buffers keep that to a brief glitch, but on a lossy link UDP-based calls (WeChat, Telegram, WebRTC) degrade more gracefully. WebTransport datagrams would lift this; they are not used yet.
  - State transfer yields to everything else: on long, jittery paths it slows down rather than delay voice. From this machine to the Cloudflare relay (a proxied path to San Jose, 300–450 ms round trips with heavy jitter), 512 KB took 24 s. Keep snapshots compressed (a few hundred kilobytes), and raise `Config::transfer_delay` on the providing side where loading fast matters more.
  - The relay does not order commands from different members against each other: each member's stream arrives in order, but two members' streams interleave differently at different receivers. Applications whose commands do not commute need a rule on top ([docs/multiplayer.md](docs/multiplayer.md)).
  - Any member can read and send everything; roles are the application's to enforce, by member ID. Excluding someone means moving the group to a new secret.
  - Sender keys are fresh per connection but travel under a key derived from the group secret: no forward secrecy against someone who records the traffic and later learns the secret.
  - The Cloudflare relay forwards every frame at once: it cannot drop late voice or put commands first, and each voice frame is a billed Durable Object message. For voice at scale, use the Linux relay.
  - Browser voice needs WebCodecs audio with Opus (`startVoice` checks, and throws where it is missing; tested in Chromium). Echo cancellation is the browser's; headphones avoid echo entirely.
- **Not yet independently audited.** The design has had several rounds of adversarial review, and its vectors are reproduced by a second implementation written from the spec. Neither replaces an external cryptographic audit.

## License

MIT or Apache-2.0, at your option.
