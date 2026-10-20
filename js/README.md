# peerconnect (JavaScript)

Two peers, one room, end-to-end encrypted over an untrusted WebSocket relay. Works in browsers, Node.js 22+, Deno and Bun. The protocol runs in WebAssembly compiled from the Rust implementation (43 KB gzipped, inlined, so no bundler setup is needed). See the [repository README](../README.md) and the [specification](../SPEC.md).

```js
import { connect, Room } from "peerconnect";

// Inviter
const room = Room.create("wss://relay.example.com");
const invite = room.invite;                 // hand this to the other peer, e.g. as a QR code
const a = connect(room);
a.addEventListener("paired", () => save(a.room.toString()));  // a secret: store it like a password

// Joiner: save the room straight away; it holds the identity the inviter will pin.
const joined = Room.join(invite);
save(joined.toString());
const b = connect(joined);
b.addEventListener("state", () => {
  if (b.state === "connected") b.send("hello");
});
a.addEventListener("message", (e) => console.log(new TextDecoder().decode(e.data)));

// Any time later, on either side:
const again = connect(load());
```

## API

`Room.create(relayUrl)`, `Room.join(invite)`, `Room.parse(saved)`; `room.toString()` (persist it, it is a secret), `room.invite`, `room.id`, `room.role`, `room.paired`, `room.publicKey`, `room.peerKey`.

Persist the room when you create or join it, and again after `"paired"` (inviter). A joiner that loses its saved room cannot get back in with the same invite: `Room.join` creates a new identity, and the inviter only accepts the one it paired with.

`connect(room | saved)` returns a `Connection` (an `EventTarget`) that connects, reconnects with backoff, and keeps the session alive until you call:

- `leave()` to disconnect while keeping the room;
- `close()` to end the room for both peers.

| Member | |
|---|---|
| `state` | `"offline"` (not on the relay), `"waiting"` (on the relay, peer not connected), `"connected"`, `"closed"` |
| `reason` | when closed: `"left"`, `"closed"`, or `"closed-by-peer"` (delete the room) |
| `room` | the current room; save it again on `"paired"` |
| `send(data)` | `Uint8Array`, `ArrayBuffer` or string (UTF-8), up to 16 MiB; throws unless connected |
| `bufferedAmount` | bytes queued but not yet sent; `send` never blocks, so pace large transfers by this |
| event `state` | `state` or `reason` changed |
| event `message` | `event.data` is a `Uint8Array`, authenticated and decrypted |
| event `paired` | the inviter learned the joiner's identity |
| event `replaced` | the room was opened elsewhere and took this seat; it retries in a minute unless you `leave()` |

Messages are delivered only while both sides are connected: there is no offline queue.

## Building

```sh
sh scripts/build.sh   # needs Rust with the wasm32-unknown-unknown target and wasm-bindgen-cli
npm test              # runs against the Rust relay in ../target/debug (cargo build first)
```
