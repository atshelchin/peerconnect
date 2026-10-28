// /llms.txt (https://llmstxt.org): what an AI agent needs to build a
// PeerConnect client, and where the rest is. /llms-full.txt adds the API
// reference and the specification in full.

import { RELAY_URL, REPO_URL, SITE_URL } from '#lib/site.ts';

const repo = REPO_URL ?? 'https://github.com/atshelchin/peerconnect';

export const LLMS = `# PeerConnect

> End-to-end encrypted, reliable channels between exactly two applications, through an untrusted and stateless WebSocket relay. PeerConnect is offered as an API: the relay protocol, and an HTTP API that issues the tokens the public relay requires. There is no SDK to install: implement the peer side of the specification in your application.

Key facts:

- Public relay: \`${RELAY_URL}\`. Endpoint: \`${RELAY_URL}/v1/<room_id>?token=<token>\`. Frames are binary WebSocket messages; keepalive is the text \`ping\` every 25 s, answered by \`pong\`.
- Every connection needs a token, each peer its own; tokens are never part of an invite. Close code 4403: the token is missing, invalid or revoked; get a new one before reconnecting. 4429: too many connections; wait at least 60 s.
- Tokens: \`POST ${SITE_URL}/api/trial\` (two hours, no account, for trying out); for an application, register it at ${SITE_URL}/developers for an app ID and an app secret, then either Connect with PeerConnect (OAuth 2.0 authorization code with PKCE: \`${SITE_URL}/connect\`, then \`POST ${SITE_URL}/api/token\`) or have your server call \`POST ${SITE_URL}/api/apps/<app_id>/tokens\` with the app secret.
- Cryptography: Ed25519 (room IDs are public keys derived from the room secret; relays admit by challenge signature), the Noise handshake \`Noise_IKpsk1_25519_ChaChaPoly_SHA256\`, ChaCha20-Poly1305, SHA-256 and HMAC-SHA256, unpadded base64url everywhere.
- A client must reproduce the test vectors in \`test-vectors/v1.json\` exactly before it talks to anyone.

## Docs

- [API reference](${SITE_URL}/docs/api.md): tokens on the relay, trial tokens, Connect with PeerConnect, server-issued tokens, errors.
- [Protocol specification v1](${SITE_URL}/spec.md): rooms and invites, relay frames and admission, the handshake, the reliable stream, close codes, limits and timers, security considerations. Normative.
- [Guide](${SITE_URL}/docs): concepts, getting access, running your own relay, limits, security checklist.
- [Full text](${SITE_URL}/llms-full.txt): the API reference and the specification in one file.

## Reference implementations

For reading and testing against, not to install: they are the repository's own, not supported packages. Follow the specification where they differ.

- [Rust core](${repo}/tree/main/crates/peerconnect): the protocol without I/O. \`src/peer.rs\` is the peer state machine (connect, join, handshake, reliable stream, pairing, reconnection), \`src/room.rs\` rooms and invites, \`src/noise.rs\` the handshake, \`src/wire.rs\` frames and close codes; \`src/client.rs\` drives it with Tokio WebSockets.
- [JavaScript](${repo}/tree/main/js): the same Rust core compiled to WebAssembly, with \`index.js\` supplying the WebSocket and timers (about 200 lines): a template for driving the state machine from any event loop.
- [Test vectors](${repo}/tree/main/test-vectors): \`v1.json\`, and \`verify.mjs\`, an independent implementation of every derivation with node:crypto.

## Optional

- [Live demo](${SITE_URL}/demo): two browsers pairing through the public relay.
- [Relay](${repo}/tree/main/crates/peerconnect-relay): the Linux relay, for running your own (\`PEERCONNECT_TOKEN_SECRET\` makes it require tokens).
`;

export function llmsFull(api: string, spec: string): string {
	return `${LLMS}\n\n---\n\n${api.trim()}\n\n---\n\n${spec.trim()}\n`;
}

export const TEXT = { 'content-type': 'text/plain; charset=utf-8' };
export const MARKDOWN = { 'content-type': 'text/markdown; charset=utf-8' };
