# Deploying the relay

The relay is one static binary, in an image of about 3 MB (`ghcr.io/atshelchin/peerconnect-relay`, amd64 and arm64). It keeps nothing: restart or replace it whenever you like. It serves two-peer rooms over WebSockets on its TCP port, and groups over QUIC only, on the UDP port of the same number: raw QUIC for native members, WebTransport for browsers ([GROUP.md §3.6](../GROUP.md)). There is no WebSocket for groups, so **the UDP port must be open**. What to set up depends on whether it has a domain.

| | Group relay URL in invites | Browsers | Native (Rust, C) | Open |
|---|---|---|---|---|
| [Domain, Caddy](#domain-with-caddy) | `https://relay.example.com` | WebTransport | raw QUIC | TCP 80, 443; UDP 443 |
| [Domain, certbot](#domain-with-certbot) | `https://relay.example.com` | WebTransport | raw QUIC | TCP 80, 443; UDP 443 |
| [Domain, systemd](#domain-with-systemd) | `https://relay.example.com` | WebTransport | raw QUIC | TCP 80, 443; UDP 443 |
| [No domain](#no-domain) | `quic://203.0.113.9:8080` | WebTransport | raw QUIC | TCP and UDP 8080 |

Rooms use `wss://relay.example.com` (or `ws://…:8080` without a domain) on the same relay. The Cloudflare Worker ([`crates/peerconnect-worker`](../crates/peerconnect-worker)) serves rooms only: Workers cannot accept QUIC.

A domain behind Cloudflare must not be proxied (the orange cloud): Cloudflare's proxy does not carry QUIC to your server. Point a plain DNS record (grey cloud) at the relay.

## Domain, with Caddy

[`docker-compose.yml`](docker-compose.yml): Caddy obtains and renews the certificate and terminates TLS on TCP; UDP 443 goes straight to the relay, which reads Caddy's certificate.

```sh
cd deploy
DOMAIN=relay.example.com docker compose up -d
```

## Domain, with certbot

[`certbot/docker-compose.yml`](certbot/docker-compose.yml): no proxy. The relay serves `wss://` for rooms (and plain `ws://`, told apart by the first byte) on TCP 443 and QUIC for groups on UDP 443 itself, with a Let's Encrypt certificate that certbot obtains (HTTP-01 on port 80) and renews twice a day. [`certbot.sh`](certbot/certbot.sh) copies it, readable by the relay's user, whenever it changes; the relay loads the new one within a minute, without dropping anyone.

```sh
cd deploy/certbot
DOMAIN=relay.example.com EMAIL=you@example.com docker compose up -d
```

On the first start the relay exits until certbot has the certificate, and is restarted. With certificate files from anywhere else, mount them and point the relay at them:

```sh
docker run -d --restart unless-stopped -p 443:443 -p 443:443/udp -v /etc/relay-certs:/certs:ro \
  -e PEERCONNECT_LISTEN=0.0.0.0:443 \
  -e PEERCONNECT_TLS_CERT=/certs/fullchain.pem -e PEERCONNECT_TLS_KEY=/certs/privkey.pem \
  ghcr.io/atshelchin/peerconnect-relay:main
```

The relay runs as user 65534: the key file must be readable by it (`certbot.sh` takes care of that). Any certificate browsers trust works, including a Let's Encrypt certificate for an IP address, which needs no domain.

## Domain, with systemd

[`systemd/`](systemd): for a server without Docker. The static binary runs as a service under its own user, with only the right to bind port 443, and certbot on the host obtains the certificate and renews it; a deploy hook hands each new one to the relay, which loads it within a minute. On a RHEL-like system (AlmaLinux, Rocky; certbot from EPEL), as root:

```sh
useradd --system --no-create-home --shell /sbin/nologin peerconnect
install -d -m 750 -o root -g peerconnect /etc/peerconnect /etc/peerconnect/tls
install -m 755 peerconnect-relay /usr/local/bin/   # e.g. cargo zigbuild --release --target x86_64-unknown-linux-musl
install -m 755 systemd/certbot-deploy-hook.sh /etc/letsencrypt/renewal-hooks/deploy/peerconnect.sh
certbot certonly --standalone -d relay.example.com --deploy-hook /etc/letsencrypt/renewal-hooks/deploy/peerconnect.sh
install -m 644 systemd/peerconnect-relay.service /etc/systemd/system/
systemctl daemon-reload && systemctl enable --now peerconnect-relay certbot-renew.timer
```

The unit also sets the server's traffic budget (`PEERCONNECT_MONTHLY_GB`, `PEERCONNECT_MAX_MBPS`): as the relay nears it, everyone's video shrinks, never voice, and the month's usage is kept in `/var/lib/peerconnect/usage` across restarts and logged every ten minutes (`journalctl -u peerconnect-relay`). Leave room in the monthly figure for anything else the server sends.

QUIC wants larger UDP buffers than most kernels allow by default:

```sh
printf "net.core.rmem_max = 7500000\nnet.core.wmem_max = 7500000\n" > /etc/sysctl.d/90-peerconnect.conf && sysctl --system
```

## No domain

[`no-domain/docker-compose.yml`](no-domain/docker-compose.yml), or simply:

```sh
docker run -d --restart unless-stopped -p 8080:8080 -p 8080:8080/udp ghcr.io/atshelchin/peerconnect-relay:main
```

Nothing to configure, and any port. Invites use `quic://<address>:8080`:

- Native members connect over raw QUIC and do not check the relay's certificate.
- Browsers connect over WebTransport. Without a certificate of its own, the relay serves the **weekly certificate** ([GROUP.md §3.7](../GROUP.md)): one that changes every week and that anyone can compute, so the browser SDK pins its hash without being told anything. Tested in Chromium. Browsers without WebTransport, or without certificate hashes, cannot join.
- Where UDP does not get through, members cannot join: they stay offline and keep trying.

Nothing members send depends on trusting the relay: everything is end-to-end encrypted and authenticated by the members. What you give up without a verified certificate is that someone on the path could pose as the relay, and see what a relay sees (who is in which group, when, how much), as the relay operator does anyway.

## Checking it

The relay's log says what it serves:

```
peerconnect relay 0.1.0 listening on 0.0.0.0:443 for ws:// and wss:// (…)
groups over QUIC on UDP 0.0.0.0:443: raw QUIC and WebTransport
reloaded the certificate from /certs/fullchain.pem
```

In a browser, `conn.connected` is true while a WebTransport session is open; in Rust, `conn.connected()`; in C, `pc_client_connected`. A member that never connects usually means UDP is not published (`-p 443:443/udp`), not open in the firewall, or, for a browser, that the browser goes through an HTTP or SOCKS proxy, which cannot carry QUIC: add the relay's host to the proxy's exceptions. The relay answers a WebSocket to a group path with HTTP 404 ("groups are served over QUIC and WebTransport only").

`GET /healthz` answers on the TCP port, over plain HTTP or TLS, and `peerconnect-relay healthcheck` (the image's health check) uses it.
