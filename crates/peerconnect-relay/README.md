# peerconnect-relay

The [PeerConnect](https://peerconnect.dev) relay as a standalone server. It forwards frames between the two seats of each room, admits a connection only if it proves it holds the room's key (without learning it), and stores nothing: restart it whenever you like.

```sh
cargo install peerconnect-relay
peerconnect-relay                      # ws://0.0.0.0:8080
```

| Variable | Default | Meaning |
|---|---|---|
| `PEERCONNECT_LISTEN` | `0.0.0.0:8080` | Address to listen on |
| `PEERCONNECT_MAX_CONNECTIONS` | `10000` | Concurrent WebSockets |
| `PEERCONNECT_MAX_PER_ADDRESS` | `256` | Concurrent WebSockets per client address (IPv6 counted per /64) |
| `PEERCONNECT_TRUST_PROXY` | off | Set to `1` behind a proxy that appends the client address to `X-Forwarded-For` |

It speaks plain `ws://`; put a TLS-terminating proxy in front of it for `wss://`. `GET /healthz` answers for health checks, and `peerconnect-relay healthcheck` exits successfully if a relay answers on `PEERCONNECT_LISTEN`.

A Docker image, a Compose file with automatic HTTPS and a Cloudflare Workers relay are in the [repository](https://github.com/atshelchin/peerconnect); the [deployment guide](https://peerconnect.dev/docs#relay) covers all of them.

Licensed under either of [Apache License, Version 2.0](https://github.com/atshelchin/peerconnect/blob/main/LICENSE-APACHE) or [MIT license](https://github.com/atshelchin/peerconnect/blob/main/LICENSE-MIT) at your option.
