# PeerConnect

**Two peers, one room, end-to-end encryption over an untrusted, stateless WebSocket relay.**

PeerConnect connects two applications — a phone and a laptop, a web app and a wallet, two services in different networks — through a relay that cannot read, forge or replay what they say, and that needs no database. One side creates a room and hands the other an invite (a QR code, a link); from then on the two can reconnect whenever they like, from wherever they are.

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
- **One codebase.** A Rust core with no I/O runs the Linux relay, the Cloudflare relay, the Rust client and, compiled to WebAssembly, the JavaScript client.

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

**Binary**: `cargo run --release -p peerconnect-relay` (configured with `PEERCONNECT_LISTEN` and `PEERCONNECT_MAX_CONNECTIONS`).

### JavaScript (browsers, Node.js 22+, Deno, Bun)

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
connB.addEventListener("state", () => connB.state === "connected" && connB.send("hello"));

// Later, after a restart: connect(savedString) and carry on. Both sides reconnect on their own.
```

Build the package with `sh js/scripts/build.sh` (needs `wasm-bindgen-cli`); the WebAssembly is inlined, so it loads without bundler configuration. 43 KB gzipped.

### Rust

```toml
peerconnect = { version = "1", features = ["client"] }
```

```rust
use peerconnect::{client, Event, Room, State};

let room = Room::join(&invite.parse()?);
let mut conn = client::connect(room);
while let Some(event) = conn.next().await {
    match event {
        Event::State(State::Connected) => conn.send(b"hello").await?,
        Event::Message(data) => println!("{}", String::from_utf8_lossy(&data)),
        _ => {}
    }
}
```

Without the `client` feature the crate does no I/O: [`Peer`](crates/peerconnect/src/peer.rs) is a state machine that any WebSocket library and event loop can drive. Try the terminal chat:

```sh
cargo run -p peerconnect --features client --example chat -- new ws://localhost:8080 room-a
cargo run -p peerconnect --features client --example chat -- join '<invite>' room-b
```

## How it works

1. **Room.** The inviter draws a random 32-byte room secret. From it derive the *room key* (Ed25519), whose public half is the room ID, and a pre-shared key for the handshake. The invite is `relay URL # base64url(secret ‖ inviter's public key)`.
2. **Joining the relay.** A peer connects to `<relay>/v1/<room id>`, receives a random challenge and answers with a signature by the room key. The relay checks the signature against the room ID itself; it stores nothing and never sees the secret. Each room has two seats, inviter and joiner; a newer connection to a seat replaces the older one, and the relay tells each side whether the other is present.
3. **Handshake.** Whenever both seats are filled, the joiner starts a Noise `IKpsk1` handshake. It authenticates both identities, requires the room secret, and gives forward secrecy. The first time, the inviter learns and **pins** the joiner's identity; from then on nobody else can connect as the joiner, even with the invite.
4. **Messages.** ChaCha20-Poly1305 with counter nonces, so the relay cannot reorder, replay or drop messages unnoticed. Messages of up to 16 MiB are split into 64 KiB frames.
5. **Reconnection.** Any failure (network, relay restart, timeout, tampering) leads to reconnecting with backoff and a fresh handshake. Peers keep pinging the relay every 25 s so dead connections are noticed on both sides.

The design decisions, and how they differ from the original draft, are explained in [docs/design.md](docs/design.md).

## Repository

| Path | What |
|---|---|
| [`SPEC.md`](SPEC.md) | The protocol, version 1 |
| [`test-vectors/`](test-vectors) | Conformance vectors, and an independent checker written from the spec |
| [`crates/peerconnect`](crates/peerconnect) | Protocol core (no I/O), plus the optional Tokio client |
| [`crates/peerconnect-relay`](crates/peerconnect-relay) | Relay for Linux and Docker (Tokio, hyper, tungstenite) |
| [`crates/peerconnect-worker`](crates/peerconnect-worker) | Relay for Cloudflare Workers and Durable Objects |
| [`crates/peerconnect-wasm`](crates/peerconnect-wasm), [`js/`](js) | JavaScript SDK |
| [`deploy/`](deploy) | docker-compose with automatic HTTPS |

## Testing

```sh
cargo test                              # unit tests, deterministic simulation, end-to-end over real sockets
node test-vectors/verify.mjs            # re-derives every test vector with node:crypto only
PEERCONNECT_RELAY=ws://127.0.0.1:8787 cargo test -p peerconnect-relay --test e2e   # same suite against any relay
cd js && npm run build && npm test      # JavaScript SDK, including interop with the Rust client
```

What is covered:

- **Noise.** The handshake and transport are byte-identical to the [`snow`](https://github.com/mcginty/snow) library.
- **Test vectors.** Every value and wire byte is reproduced by an independent implementation built only from `SPEC.md` (`test-vectors/verify.mjs`).
- **Simulation.** A deterministic simulation drives two peers and a relay through pairing, relay restarts, replacement, keepalive loss, lost handshakes, tampering and reconnection storms.
- **Flow control.** Regression tests send 16 MiB both ways at once, echo from inside the event loop, starve a sender with a peer that stops reading, and (opt-in, 80 s) trickle 16 MiB to a slow reader.
- **End-to-end suite.** The same tests pass against the Linux relay and against the Cloudflare Worker running in local `workerd`, including a long idle test that confirms Durable Object hibernation and recovery.
- **JavaScript SDK.** Tested in Node 22, Deno 2.7, Bun 1.4 and headless Chrome 155, against both relays, and talking to the Rust client.

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
| End-to-end encrypted round trip, 1 room | p50 76 µs |
| Pairing (both connect, join, handshake) | p50 0.7 ms |
| End-to-end encrypted bulk transfer, 1 room | 640 MB/s |

These are loopback numbers: they show the relay's own overhead, which is far below any real network's. Reproduce with `cargo run --release -p peerconnect-relay --example bench -- ws://127.0.0.1:8080 <scenario>`.

## Limitations

- **No offline delivery.** Messages flow only while both peers are connected: `send` fails while the other peer is away, and a message in flight when it drops is lost. Acknowledgement and retry belong to the application.
- **Ending a room is best effort.** If the other peer is offline when you close a room, it is never told, because the relay keeps no state. It simply finds the room empty from then on.
- **A leaked invite still allows denial of service.** After pairing it cannot read, forge or impersonate anything, but whoever holds it can take either seat on the relay and knock a peer off for as long as they keep at it. Treat invites as secrets and discard them once paired.
- **Availability depends on the relay.** Rate limiting per client address is left to the deployment (proxy, firewall, Cloudflare).
- **Cloudflare relay.**
  - It enforces the idle timeout lazily (when a room next wakes up), to keep hibernating rooms free.
  - It cannot apply backpressure: the runtime buffers outgoing messages and does not say how much. A peer that floods a partner who never reads can exhaust its own room's Durable Object, which resets that room only.
  - Throughput per room is about 40,000 frames/s under local `workerd`, bounded by the single-threaded Durable Object, which is ample for two peers.
  - Unjoined connections are capped per client address (4) and per room (64). Someone who knows a room ID and controls many addresses can still delay the room's peers; add Cloudflare rate-limiting rules if that matters.

## License

MIT or Apache-2.0, at your option.
