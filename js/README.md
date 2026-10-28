# peerconnect (JavaScript)

Two peers, one room, end-to-end encrypted, reliable messaging over an untrusted WebSocket relay. Works in browsers, Node.js 22+, Deno and Bun. The protocol runs in WebAssembly compiled from the Rust implementation (46 KB gzipped, inlined, so no bundler setup is needed). Guide, specification and a live demo: <https://peerconnect.dev>.

```sh
npm install peerconnect
```

```js
import { connect, Room } from "peerconnect";

// Inviter
const room = Room.create("wss://relay.peerconnect.dev"); // public test relay, or run your own
const invite = room.invite;                 // hand this to the other peer, e.g. as a QR code
const a = connect(room);
a.addEventListener("paired", () => save(a.room.toString()));  // a secret: store it like a password

// Joiner: save the room straight away; it holds the identity the inviter will pin.
const joined = Room.join(invite);
save(joined.toString());
const b = connect(joined);
b.addEventListener("paired", () => save(b.room.toString()));
b.send("hello"); // queued; delivered as soon as the inviter is reachable
a.addEventListener("message", (e) => console.log(new TextDecoder().decode(e.data)));

// Any time later, on either side:
const again = connect(load());
```

## API

`Room.create(relayUrl)`, `Room.join(invite)`, `Room.parse(saved)`; `room.toString()` (persist it, it is a secret), `room.invite`, `room.id`, `room.role`, `room.paired`, `room.publicKey`, `room.peerKey`.

Persist the room when you create or join it, and again after `"paired"` (inviter). A joiner that loses its saved room cannot get back in with the same invite: `Room.join` creates a new identity, and the inviter only accepts the one it paired with.

`connect(room | saved, { token })` returns a `Connection` (an `EventTarget`) that connects, reconnects with backoff, carries a reliable message stream across reconnections, and keeps going until you call:

- `leave()` to disconnect while keeping the room;
- `close()` to end the room for both peers. The connection carries on until the peer has been told; save `room` meanwhile, as it now says "closing".

| Member | |
|---|---|
| `state` | `"offline"` (not on the relay), `"waiting"` (on the relay, peer not connected), `"connected"`, `"closed"` |
| `reason` | when closed: `"left"`, `"closed"`, or `"closed-by-peer"` (delete the room) |
| `room` | the current room; save it again on `"paired"` |
| `send(data)` | `Uint8Array`, `ArrayBuffer` or string (UTF-8), up to 16 MiB; returns the message's id. Queued until delivered, even while offline; throws once 64 MiB await acknowledgement, or after `close()` |
| `queued` | bytes awaiting acknowledgement: pace large transfers by it |
| event `state` | `state` or `reason` changed |
| event `message` | `event.data` is a `Uint8Array`, authenticated and decrypted |
| event `delivered` | `detail`: the peer has every message with a lower id |
| event `paired` | pairing is complete and the room moved to its pair ID: save `room` again |
| event `replaced` | the room was opened elsewhere and took this seat; it retries in a minute unless you `leave()` |
| `setToken(token)` | replaces the relay token, from the next connection on |
| event `unauthorized` | the relay requires a token and refused this one; the connection stays offline until it has another |

`token` is for a relay that requires one, such as relay.peerconnect.dev: `trialToken` from `peerconnect/auth` to try it, or your app's ([getting one](https://peerconnect.dev/docs#tokens)): a string, or a function that returns one (or a promise of one). A function is called before the first connection and again whenever the relay refuses the token, so a short-lived token from your server renews itself. Each peer brings its own token; it is not part of the room or the invite.

Messages are delivered in order, exactly once, as long as both sides keep running; unacknowledged messages live in memory, so a page reload loses those not yet delivered.

## Building

From a clone of [the repository](https://github.com/atshelchin/peerconnect):

```sh
sh scripts/build.sh   # needs Rust with the wasm32-unknown-unknown target and wasm-bindgen-cli
npm test              # runs against the Rust relay in ../target/debug (cargo build first)
```

## License

MIT OR Apache-2.0, at your option.
