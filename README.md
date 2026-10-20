# PeerConnect

**Two peers, one room, end-to-end encryption and reliable delivery over an untrusted, stateless WebSocket relay.**

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
| [`test-vectors/`](test-vectors) | Conformance vectors, and an independent checker written from the spec |
| [`crates/peerconnect`](crates/peerconnect) | Protocol core (no I/O), plus the optional Tokio client |
| [`crates/peerconnect-relay`](crates/peerconnect-relay) | Relay for Linux and Docker (Tokio, hyper, tungstenite) |
| [`crates/peerconnect-worker`](crates/peerconnect-worker) | Relay for Cloudflare Workers and Durable Objects |
| [`crates/peerconnect-wasm`](crates/peerconnect-wasm), [`js/`](js) | JavaScript SDK |
| [`deploy/`](deploy) | docker-compose with automatic HTTPS |
| [`website/`](website) | [peerconnect.dev](https://peerconnect.dev): introduction, guide, specification and live demo, in English and Chinese |

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
- **Not yet independently audited.** The design has had several rounds of adversarial review, and its vectors are reproduced by a second implementation written from the spec. Neither replaces an external cryptographic audit.

## License

MIT or Apache-2.0, at your option.
