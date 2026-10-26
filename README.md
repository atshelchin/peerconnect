# PeerConnect

**End-to-end encrypted rooms and groups over an untrusted, stateless relay: two peers with reliable delivery over WebSockets, or up to four members with commands, chat, voice, video and state transfer over QUIC.**

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
- **Groups for games and teams.** Up to four members per group with [their own protocol](GROUP.md), over QUIC only: commands, chat, voice and video that never get in each other's way, members known by their own keys, and state transfer that brings a newcomer up to date from the members already there. See [Groups](#groups).
- **One codebase.** A Rust core with no I/O runs the Linux relay, the Cloudflare relay (rooms), the Rust client, the C library for game engines and, compiled to WebAssembly, the JavaScript client.

## Quick start

### Run a relay

**Docker**:

```sh
docker run -p 8080:8080 -p 8080:8080/udp ghcr.io/atshelchin/peerconnect-relay:main   # rooms on ws://localhost:8080, groups on quic://localhost:8080
```

The image (amd64 and arm64) is built and published by [`.github/workflows/docker.yml`](.github/workflows/docker.yml) on every push to `main` (`:main`, `:sha-…`) and every `v*` tag (`:1.2.3`, `:1.2`, `:latest`); `docker build -t peerconnect-relay .` builds it locally.

[`deploy/docker-compose.yml`](deploy/docker-compose.yml) adds Caddy for automatic HTTPS: `DOMAIN=relay.example.com docker compose -f deploy/docker-compose.yml up -d` gives you `wss://relay.example.com` for rooms, and `https://relay.example.com` for groups over WebTransport and QUIC ([Transports](#transports)). For a server without Docker, [`deploy/systemd`](deploy/systemd) runs the binary as a service with a certificate from certbot.

**Cloudflare Workers**, for rooms only (one Durable Object per room, using WebSocket Hibernation, so idle rooms cost nothing; groups need the Linux relay, since Workers cannot accept QUIC):

```sh
cargo install worker-build
cd crates/peerconnect-worker && npx wrangler deploy
```

**Binary**: `cargo install peerconnect-relay`, then run `peerconnect-relay`, configured with `PEERCONNECT_LISTEN`, `PEERCONNECT_MAX_CONNECTIONS`, `PEERCONNECT_MAX_PER_ADDRESS` (default 256, IPv6 counted per /64) and `PEERCONNECT_TRUST_PROXY=1` behind a proxy that sets `X-Forwarded-For`. For groups over QUIC: `PEERCONNECT_QUIC_LISTEN` (the UDP address; by default the same as the TCP one, `off` to turn it off), and `PEERCONNECT_TLS_CERT` and `PEERCONNECT_TLS_KEY` (PEM files, re-read when they change) for `wss://` and WebTransport. Groups need that UDP port open.

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

A group is a squad, a team, a shared world: everyone holding its invite can join, up to four members at once. It is a separate protocol, [GROUP.md](GROUP.md), on its own endpoint (`/g1/<id>`), served by the Linux relay over QUIC.

- **Members are keys.** Each member has an Ed25519 identity that the application keeps (one per player, say). Others see it, proven, as the member's ID, so the application can decide who owns what and who is only visiting. The relay never sees identities.
- **Five classes of traffic, most urgent first:** commands, voice, chat, video, bulk. Each has its own queue on the sender, in the relay, and its own counters in the encryption. A burst of chat or a state transfer never holds up a command, voice never waits behind chat, and video, which can fill a link, never starves chat.
- **Commands and chat are reliable streams.** Numbered, kept by the sender, resent after a reconnection, delivered to everyone (the sender included) in order and exactly once while both keep running. Messages sent while offline go out on reconnection.
- **Voice, video and datagrams are real-time.** Anyone along the way drops them rather than delay them: the sender when its connection backs up, the relay when a recipient is behind. Late audio is worse than lost audio. Datagrams carry things like positions, where only the latest value matters.
- **Video in whole frames.** A frame is cut into fragments that each fit a datagram; receivers get it back whole and in order, ask for a lost fragment once, and when a frame is lost for good, drop what cannot be decoded and ask for a keyframe.
- **State transfer.** A newcomer asks a member for a snapshot of the application's state. The SDK records exactly which commands the snapshot contains, holds back the live ones meanwhile, and then delivers the snapshot followed by every later command, and nothing twice. The snapshot is the application's bytes: a saved world, a document, a game state.
- **Untrusted relay, again.** Every frame is encrypted with a fresh per-connection key that its sender's identity signs for; admission uses a fresh challenge, so a relay cannot replay an earlier session. The relay sees slots, sizes and timing.
- **QUIC only.** Each class has a stream of its own and voice and video travel as datagrams, so a lost packet holds up nothing but its own class and voice never waits for a retransmission. There is no WebSocket fallback: over TCP every loss stalls everything behind it, and a member that cannot use UDP does not join rather than join with stuttering voice and video ([Transports](#transports)).

How to build a multiplayer game or collaborative app on top, including who decides what when two members change the same thing, is in [docs/multiplayer.md](docs/multiplayer.md).

### Transports

The relay URL in the invite says how members reach the relay ([GROUP.md §3.6](GROUP.md)):

| Relay URL | Browsers | Rust, the C library | Relay |
|---|---|---|---|
| `https://relay.example.com` | WebTransport | raw QUIC | Linux relay with a certificate, on UDP 443 |
| `quic://203.0.113.9:8080` | WebTransport, pinning the weekly certificate | raw QUIC | Linux relay, with no domain and no certificate |

There is no WebSocket transport for groups, so some members cannot join, by design: browsers without WebTransport (`groupsSupported()` says so), networks that block UDP, and browsers behind an HTTP or SOCKS proxy (a system proxy, Clash and the like), which QUIC cannot go through: add the relay's host to the proxy's exceptions. Such a member stays offline and keeps trying. Nothing in a group depends on trusting the relay, so a relay reachable only by its IP address works, on any port, with nobody's signature on anything: raw QUIC does not check its certificate, and browsers, which insist on one, pin the **weekly certificate** such a relay serves, which anyone can compute ([GROUP.md §3.7](GROUP.md)). With a domain, the relay serves your certificate (`PEERCONNECT_TLS_CERT`, `PEERCONNECT_TLS_KEY`). [deploy/README.md](deploy/README.md) has a setup for each case: Caddy, certbot, systemd, and no domain.

The Cloudflare Worker serves rooms only: Workers cannot accept QUIC, and cannot drop late voice or put commands first.

### JavaScript

```js
import { Group, Identity, joinGroup } from "peerconnect";
import { startVoice } from "peerconnect/voice";

const group = Group.create("https://relay.example.com"); // group.toString() is the invite: hand it to the members
const me = Identity.generate();                         // persist me.seed to be recognised next time
const conn = joinGroup(group, { identity: me, profile: "alice" }); // over WebTransport: check groupsSupported() first

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

// Video: the camera through WebCodecs (VP8 by default), in two sizes, each member drawn into a canvas.
import { startVideo } from "peerconnect/video";
const camera = await navigator.mediaDevices.getUserMedia({ video: { width: 1280, height: 720 } });
const video = await startVideo(conn, {
  stream: camera,
  framerate: 30,
  layers: [
    { layer: 1, width: 640, height: 360, bitrate: 400_000 },
    { layer: 2, width: 1280, height: 720, bitrate: 1_500_000 },
  ],
});
conn.addEventListener("joined", (e) => video.attach(e.detail.member, canvasFor(e.detail.member)));
```

Video is sent in **layers** (simulcast): each member gets, of each other member's video, the layer that fits the canvas it is drawn in, as the meeting apps do: a large tile the large picture, a thumbnail the small one, a hidden one nothing. `attach` subscribes by the canvas's size, and moves when it changes; the relay forwards each member its layer and switches at a keyframe, so the picture never stops; and senders encode only the layers someone wants. So in a call of four, each member receives one large picture and small ones, and the relay forwards half what it would otherwise. Each link is fitted on its own: a member whose downlink cannot keep up is moved to smaller layers by the relay, down to voice alone, and given more again once it keeps up (`videolimit` says how far); a sender whose uplink cannot keep up stops its largest layer first. One slow member costs its own pictures, not everyone's. A relay with a traffic budget (`PEERCONNECT_MONTHLY_GB`, `PEERCONNECT_MAX_MBPS`) shrinks everyone's video as it nears it, and never voice. A member may send two tracks, a camera and a screen (`startVideo(conn, { track: 1, stream: screen, contentHint: "detail" })`), each layered and subscribed to on its own: the screen large in 1080p, the presenter's camera beside it as a thumbnail. Large layers prefer a hardware encoder (H.264 where the device has one, else VP8). `video.setMaxLayer(1)` caps what a member receives at 360p, to save data. The bitrate of each layer steps down while receivers keep losing frames and back up while they do not. Other codecs and engines send any encoded frames with `conn.sendVideo(track, layer, keyframe, timestamp, data)` (in Rust, `send_video`; in C, `pc_client_send_video`), choose what to receive with `conn.subscribe(member, track, layer)`, get frames back as `video` events, send a keyframe on `keyframerequest`, and encode the layers `videodemand` says are wanted ([GROUP.md §7.4](GROUP.md)).

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

Game engines and other languages (Godot through a GDExtension, Unity, Unreal, C, C++) use the C library, [`crates/peerconnect-ffi`](crates/peerconnect-ffi) ([header](crates/peerconnect-ffi/include/peerconnect.h)): `pc_client_join` connects over QUIC by itself, on two threads of the library's own, and the game loop calls `pc_client_poll` once per frame. Applications that have a QUIC stack of their own can drive the state machine, [`group::Member`](crates/peerconnect/src/group/member.rs) (in C, `pc_member_*`), without the library's I/O. The terminal group chat: `cargo run -p peerconnect --features client --example group -- new quic://localhost:8080 alice`.

### Latency and load

Measured on one Apple M2 Pro with the [game load test](crates/peerconnect-relay/examples/game_load.rs): every player is a real end-to-end encrypted client (measured over WebSockets, before groups became QUIC-only: the relay's queues and fan-out are the same, and on Linux QUIC costs the relay about 1.8 times the CPU, as below); groups of three players, each sending its position 20 times a second, a game command twice a second and a chat line every 10 s, two per group talking at once (50 voice frames a second); halfway through, a newcomer joins every group, making it four, and loads a 2 MB snapshot. Latency is from the sender's `send` to the receiver's event.

| Load (Linux relay, release build) | Commands p50 / p99 | Voice p50 / p99 | Voice p99 while snapshots load | Relay |
|---|---|---|---|---|
| 34 groups, 102 players | 0.38 / 0.91 ms | 0.31 / 0.84 ms | 6.5 ms | 0.23 cores, 13 MB |
| 167 groups, 501 players | 0.67 / 2.6 ms | 0.56 / 2.4 ms | 1.1 ms | 1.0 cores, 29 MB |
| 334 groups, 1,002 players | 1.1 / 38 ms | 0.87 / 30 ms | 58 ms | 2.3 cores, 50 MB |

Everything reliable arrived, exactly once (save what was still in flight when the run stopped); so did the voice and positions, and every snapshot. At 334 groups the machine is saturated, the load generator running on it too; that forwarding is about 55,000 frames a second in and 107,000 out.

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
| Speech lost, 2% / 5% of packets lost each way (copies of the two previous frames in each packet) | 0–0.2% / 0–0.5%, against 4–6% / 10–11% without; bursty 5%: 1–3%, against 14–15% |

Lossy networks, with the Linux kernel dropping packets (`tc netem` on loopback in a 2-core arm64 VM: 20 ms and the given loss on each hop, so 40 ms from player to player; two groups of ten with the game load above, measured before groups were limited to four, so more traffic per player than a group now carries; WebSocket against QUIC through the same relay; this comparison is why groups now use QUIC only):

| Loss per hop | | Voice p50 / p99 / p99.9 / max | Commands p99 / max | Voice arrived |
|---|---|---|---|---|
| 0% | WebSocket | 41 / 44 / 60 / 69 ms | 45 / 62 ms | 100% |
| | QUIC | 41 / 43 / 46 / 52 ms | 43 / 48 ms | 100% |
| 1% | WebSocket | 41 / 101 / 135 / 201 ms | 103 / 442 ms | 100% |
| | QUIC | 41 / **56** / 119 / 149 ms | 103 / **149** ms | 98% |
| 3% | WebSocket | 44 / 138 / 201 / 421 ms | 149 / 422 ms | 100% |
| | QUIC | 41 / **53** / **71** / **110** ms | **110** / **210** ms | 94% |

Over TCP, every loss holds up everything behind it until the retransmission; over QUIC, a lost voice frame is just gone (the decoder conceals it) and only the class that lost a packet waits. In a browser (headless Chromium, speech through the fake microphone, the same loss of 2% each way): over a WebSocket, capture to playout p50 320 ms and p99 460–1170 ms, as the jitter buffer grows to ride out the stalls; over WebTransport, p50 75 ms and p99 105–120 ms, with about 5% of frames concealed. On Linux the relay spends about 1.8 times the CPU over QUIC that it does over WebSockets for the same traffic (27% against 15% of a core for 100 players), since every datagram is a packet of its own.

Browser video (headless Chromium, its fake camera, VP8 at 800 kbit/s and 20 frames a second, through the relay; time from handing a frame to the group to decoding it; the WebSocket rows are from before groups became QUIC-only):

| Path | Frames decoded | p50 / p99 | |
|---|---|---|---|
| WebSocket | 132 of 132 | 1 / 4 ms | |
| WebTransport | 141 of 141 | 1 / 2 ms | |
| WebSocket, TCP stalling as on 2% loss | 94–100% | 14–15 / 194–265 ms | everything waits for each retransmission; video that backs up behind a stall is dropped, and a keyframe asked for |
| WebTransport, 2% of UDP lost each way | 94–100% | 13–15 / 80–94 ms | lost fragments resent in time; now and then a frame lost for good costs the frames until the next keyframe |

No undecodable frame ever reaches the decoder. Through the emulated lossy path in the Rust tests (3% each way, 15 ms each way, 1 Mbit/s of video), every frame arrived, at p50 130–175 ms. Over a real path, from China to a relay in San Jose (round trip about 140 ms), two browsers over WebTransport decoded 286 of 289 frames at 1.5 Mbit/s, p50 139 ms and p99 146 ms from one to the other; through the Cloudflare Worker (served from Amsterdam for that network) the same test gave p50 217 ms and p99 about 500 ms, and lost a quarter of the frames at 800 kbit/s.

Mouth to ear adds the 20 ms frame being captured and the audio hardware's own latency (about 10–30 ms), plus the network.

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
| [`crates/peerconnect-relay`](crates/peerconnect-relay) | Relay for Linux and Docker: rooms over WebSockets, groups over QUIC and WebTransport (Tokio, hyper, tungstenite, quinn) |
| [`crates/peerconnect-worker`](crates/peerconnect-worker) | Relay for Cloudflare Workers and Durable Objects (rooms only) |
| [`crates/peerconnect-wasm`](crates/peerconnect-wasm), [`js/`](js) | JavaScript SDK, and browser voice ([`js/voice.js`](js/voice.js)) |
| [`crates/peerconnect-ffi`](crates/peerconnect-ffi) | C library for game engines and other languages |
| [`deploy/`](deploy) | docker-compose with automatic HTTPS, and a systemd service |
| [`website/`](website) | [peerconnect.dev](https://peerconnect.dev): introduction, guide, specification and live demo, in English and Chinese |

## Testing

```sh
cargo test                              # unit tests, deterministic simulation, end-to-end over real sockets
node test-vectors/verify.mjs            # re-derives every test vector with node:crypto only
node test-vectors/verify-groups.mjs     # the same for groups (g1.json)
PEERCONNECT_RELAY=ws://127.0.0.1:8787 cargo test -p peerconnect-relay --test e2e   # same suite against any relay
cd js && npm run build && npm test      # JavaScript SDK, including interop with the Rust client
cd js && npm run test:voice             # browser voice in headless Chromium: latency, jitter, stalls, lossy UDP
node js/test/webtransport.browser.mjs   # browsers over WebTransport, with a Rust member on raw QUIC
cd js && npm run test:video             # browser video in headless Chromium over WebTransport, lossy paths too
for s in $(seq 1 200); do PEERCONNECT_SIM_QUIC=$s cargo test -q -p peerconnect --test group || break; done  # as over QUIC
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
- **Groups.** A deterministic simulation (`crates/peerconnect/tests/group.rs`) runs groups up to the limit of four members through admission, reconnection, backlogs resent in bursts, messages beyond what a sender keeps, replacement, a full group, voice floods, state transfer while commands flow (also from a member that is behind), a failed transfer, a relay that loses the end of a stream, and a relay replaying an earlier session. End-to-end tests (`crates/peerconnect-relay/tests/group_e2e.rs`) run members over QUIC against the Linux relay, in-process or any deployed one: a full group of four talking, a relay restart, a cut path, a 4 MiB state transfer, the relay's wire behaviour over raw QUIC, a member that stops reading, commands overtaking a backlog of chat, and a WebSocket to a group path refused. The C library's `pc_client` is driven through its C functions against a real relay, and its header compiled from C. Every kind of group frame is in the test vectors (`test-vectors/g1.json`), produced by the implementation and reproduced by an independent one written from GROUP.md (`verify-groups.mjs`). Line coverage of the group code by these tests: the member state machine 97%, the relay 99%, the Tokio driver 97%, the C library 92% (`cargo llvm-cov`). The [game load test](crates/peerconnect-relay/examples/game_load.rs) runs up to a thousand players over real sockets, optionally behind emulated access links.
- **Video.** Unit tests of fragmenting, reassembly, resending and keyframe requests; members over a relay that drops 8% of the video in the simulation (whole frames, in order, only decodable ones, also in the QUIC mode); real sockets over QUIC and lossy QUIC; the C library; and in headless Chromium with WebCodecs over WebTransport, lossy paths too. Every video frame kind is in the test vectors.
- **QUIC.** `group_quic.rs` speaks the relay protocol by hand over raw QUIC and WebTransport (every class, datagrams, voice too big for a datagram, full groups, replacement across transports, refused paths and streams, the join timeout), runs members on raw QUIC and WebTransport in one group, through a path losing 5% of packets each way, through a change of address (no reconnection), and with UDP refused or silently dropped (the member stays offline, and joins by itself once UDP gets through), and serves WebTransport for groups and `wss://` for rooms from one certificate. The simulation runs every group test as over QUIC too, over hundreds of seeds: classes overtaking each other at random, voice out of order, frames from a replaced connection still arriving. In headless Chromium, browsers join over WebTransport (certificate pinned by hash) with a Rust member on raw QUIC, and stay offline when UDP is closed or the certificate is not trusted.

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
  - Groups travel over QUIC only, by choice: members that cannot use UDP do not join, rather than join with stuttering voice and video. That leaves out runtimes without WebTransport (`groupsSupported()` tells; Node.js has none), networks that block UDP, and browsers behind an HTTP or SOCKS proxy unless the relay's host is in the proxy's exceptions ([Transports](#transports)).
  - Groups need the Linux relay, with its UDP port open; the Cloudflare relay serves rooms only.
  - A `quic://` relay's certificate is not checked: someone on the path can pose as the relay (and learn what a relay learns), though not read or forge anything members send.
  - State transfer yields to everything else: on long, jittery paths it slows down rather than delay voice. Through a proxied path to San Jose (300–450 ms round trips with heavy jitter), 512 KB took 24 s. Keep snapshots compressed (a few hundred kilobytes), and raise `Config::transfer_delay` on the providing side where loading fast matters more.
  - The relay does not order commands from different members against each other: each member's stream arrives in order, but two members' streams interleave differently at different receivers. Applications whose commands do not commute need a rule on top ([docs/multiplayer.md](docs/multiplayer.md)).
  - Any member can read and send everything; roles are the application's to enforce, by member ID. Excluding someone means moving the group to a new secret.
  - Sender keys are fresh per connection but travel under a key derived from the group secret: no forward secrecy against someone who records the traffic and later learns the secret.
  - Browser voice needs WebCodecs audio with Opus (`startVoice` checks, and throws where it is missing; tested in Chromium). Echo cancellation is the browser's; headphones avoid echo entirely.
- **Not yet independently audited.** The design has had several rounds of adversarial review, and its vectors are reproduced by a second implementation written from the spec. Neither replaces an external cryptographic audit.

## License

MIT or Apache-2.0, at your option.
