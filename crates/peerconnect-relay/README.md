# peerconnect-relay

The [PeerConnect](https://peerconnect.dev) relay as a standalone server. It forwards frames between the two seats of each room, admits a connection only if it proves it holds the room's key (without learning it), and stores nothing: restart it whenever you like.

```sh
cargo install peerconnect-relay
peerconnect-relay                      # ws://0.0.0.0:8080, and QUIC on UDP 8080 for groups
```

| Variable | Default | Meaning |
|---|---|---|
| `PEERCONNECT_LISTEN` | `0.0.0.0:8080` | Address to listen on |
| `PEERCONNECT_MAX_CONNECTIONS` | `10000` | Concurrent connections (WebSocket and QUIC) |
| `PEERCONNECT_MAX_PER_ADDRESS` | `256` | Concurrent connections per client address (IPv6 counted per /64) |
| `PEERCONNECT_MAX_GROUP_MEMBERS` | `10` | Members per group, up to 32 |
| `PEERCONNECT_TRUST_PROXY` | off | Set to `1` behind a proxy that appends the client address to `X-Forwarded-For` |
| `PEERCONNECT_QUIC_LISTEN` | the TCP address | UDP address for groups over QUIC: raw QUIC, and WebTransport for browsers; `off` turns QUIC off |
| `PEERCONNECT_TLS_CERT`, `PEERCONNECT_TLS_KEY` | none | PEM certificate chain and key, read again whenever the files change: `wss://` next to `ws://` on the TCP port (told apart by their first byte), and WebTransport |

Without a certificate it speaks plain `ws://` (put a TLS-terminating proxy in front of it for `wss://`), and QUIC with the weekly certificate (GROUP.md §3.7): `quic://` members over raw QUIC do not check it, and browsers pin it by a hash they compute, so such a relay needs no domain. Groups never rely on the relay for confidentiality. Compose files for a domain (with Caddy or certbot) and for none are in [deploy/](https://github.com/atshelchin/peerconnect/tree/main/deploy). The transports are in [GROUP.md §3.6](https://github.com/atshelchin/peerconnect/blob/main/GROUP.md). `GET /healthz` answers for health checks, and `peerconnect-relay healthcheck` exits successfully if a relay answers on `PEERCONNECT_LISTEN`.

A Docker image, a Compose file with automatic HTTPS and a Cloudflare Workers relay are in the [repository](https://github.com/atshelchin/peerconnect); the [deployment guide](https://peerconnect.dev/docs#relay) covers all of them.

Licensed under either of [Apache License, Version 2.0](https://github.com/atshelchin/peerconnect/blob/main/LICENSE-APACHE) or [MIT license](https://github.com/atshelchin/peerconnect/blob/main/LICENSE-MIT) at your option.
