# Self-serve access to the PeerConnect relay

**Status:** approved by the owner (2026-10-28), with server-issued tokens added
(below). Phase 1 in progress.

Today anyone can open rooms on `wss://relay.peerconnect.dev`, anonymously.
Nobody can tell who uses it, how much, or stop one abuser without stopping
everyone. This design puts the public relay behind credentials, the way
clientmcp.dev does it (`clientmcp/docs/self-serve.md`), without changing what
the relay is: an untrusted forwarder that never sees content.

- **Developers** sign in on peerconnect.dev with GitHub and register their
  applications. Each gets an **app ID** (public) and an **app secret**.
- **People using those applications** connect them to PeerConnect in one step:
  the app opens peerconnect.dev, they sign in with GitHub and confirm, and the
  browser comes back to the app with a code that the app exchanges for a relay
  token — what Vela does with clientmcp.dev.
- **Or the developer's own server** issues tokens to its users with the app
  secret, for apps whose users have no reason to own a GitHub account (a game,
  a chat app). The people never see PeerConnect; the use counts as the
  developer's.
- **The relay** accepts a connection only with a valid token, can revoke any
  token at once, and meters use per token.

Self-hosted relays are untouched: credentials are something a relay MAY
require, off unless configured.

## Who is who

- **Account:** the platform's own record (`a-` + 20 base32), linked to sign-in
  identities as clientmcp's are: `(provider, provider_user_id)` → account,
  GitHub's numeric ID, no scope asked, no email stored. One account can be a
  developer, a user, or both.
- **App:** registered by a developer's account, any number of them (a limit per
  account, default 10).
  - `app_id`: `app_` + 16 base32, public; it is the OAuth client ID.
  - `app_secret`: `pcs_` + 43 base64url, confidential; it is the OAuth client
    secret, used by apps that exchange the code on a server.
  - name and icon (shown on the consent page), redirect URIs, web origins
    allowed to exchange from the browser (CORS), an optional loopback path for
    desktop apps (RFC 8252 §7.3).
  - The app secret is **not stored**: it is `HMAC(APP_SECRET_KEY, app_id ‖
    secret_gen)`, so the developer can show it again, and rotating it is
    `secret_gen + 1`. (clientmcp's "tokens shown again", applied to secrets.)
- **Client:** one per app installation per account (`c-` + 20 base32, opaque),
  or made by hand on the account page for testing. It is the token's `sub`.
- **Token:** HS256 JWT, signed by the relay alone, claims `sub` (client),
  `app`, `gen`, `exp` (about a year). One role only: there is no agent side.

Usage is attributed to the client, so to both the person (account) and the
app: the person sees their own connections, the developer sees their app's
totals and number of people, the owner sees everything.

## What a person sees

1. In an app built on PeerConnect, they tap **Connect with PeerConnect**.
2. The browser opens
   `peerconnect.dev/connect?app=<app_id>&redirect_uri=…&state=…&code_challenge=…&code_challenge_method=S256&installation=…&label=…`.
   They sign in with GitHub (first time only) and confirm "Let *<app name>* on
   *<label>* use PeerConnect as @login".
3. They are back in the app, which is now connected. Nothing to copy.

On **peerconnect.dev/account** they see their clients (app, label, last
connected, use this month) and can delete one, which stops it at once.

## What a developer sees

On **peerconnect.dev/developers**:

- **New app:** name, redirect URIs, web origins. They get the app ID and the
  app secret (with Show/Copy; Rotate stops the old secret).
- Per app: people connected, connections and traffic per day, the snippets for
  their platform with their app ID filled in.
- **A test token** for their own account (a hand-made client), to try the relay
  from a script before building the sign-in into the app.

## Returning the token to the app

OAuth 2.0 authorization code with PKCE (RFC 7636, RFC 8252), exactly as
clientmcp.dev:

- `redirect_uri` must be one registered for the app, compared exactly
  (loopback: only the port may vary). A bad one is never redirected to: the
  page says so (RFC 6749 §4.1.2.1).
- The code lives 2 minutes, works once, and is stored only as its SHA-256.
- `POST peerconnect.dev/api/token` with `grant_type=authorization_code`,
  `code`, `code_verifier`, `redirect_uri`, `client_id=<app_id>`, and
  `client_secret` if the app is confidential (a server exchanging it). JSON;
  CORS for the app's registered web origins.
- Answer: `{ token, expires_at, relay_url, client_id, login }`.
- Signing in again from the same installation rotates its token: the old one
  stops, and so does any connection still using it.

## Tokens from the developer's server

The owner's decision (2026-10-28) is to offer both paths. On the second, the
app's server holds the app secret and asks for a token per user of its own:

- `POST peerconnect.dev/api/apps/<app_id>/tokens`, `Authorization: Bearer
  <app_secret>`, JSON `{ "user": "<the app's own user ID>", "rotate": false }`.
  `user` is 1–128 characters of `A–Z a–z 0–9 . _ - : @`; it is the developer's
  identifier and is stored only as `HMAC(APP_SECRET_KEY, app_id ‖ user)`.
- The first request for a user creates a client owned by the **developer's**
  account (`app` set, `installation` null); later ones return the same
  credential (one generation, one token), unless `rotate` is true.
- `DELETE /api/apps/<app_id>/users/<user>` deletes the user's client: its
  token stops at once.
- Answer: `{ token, expires_at, relay_url, client_id }`. A server-issued
  client counts against the app's user limit (default 10,000, a setting per
  app), not against anyone's client limit.
- Never from a browser: the endpoint sends no CORS headers.

The developer's usage page then shows both kinds of clients together, people
who connected themselves and users their server issued tokens for.

## Tokens on the relay

### The protocol (SPEC §4.1, §4.7)

- A peer passes its token as a query parameter: `relay_url "/v1/" room_id
  "?token=" token`. A browser cannot set headers on a WebSocket, so a query
  parameter is the one way that works everywhere. The token is never part of
  an invite or a saved room: each peer brings its own, so two people on two
  different apps can share a room.
- A relay that requires tokens accepts the upgrade, then closes a connection
  whose token is missing, invalid, expired or revoked with the new code
  **4403**, before HELLO. (An HTTP 401 would be invisible to browsers, which
  report a failed upgrade without its status.)
- SDKs stop retrying on 4403 and raise an `unauthorized` event; the app gets a
  new token and calls `setToken`.

### Cloudflare (relay.peerconnect.dev)

- The Worker verifies the signature and expiry, then asks the token's
  **Client** Durable Object (one per client, beside the existing Room objects)
  whether it admits it: current generation, client not deleted, account not
  suspended, app not disabled, and the protective rate limit. One extra
  request per connection, none per message.
- The Room object tags each socket with its client ID (in the attachment) and
  counts its bytes and messages each way, in memory. It hands the counts to
  the client's object when the socket closes, and 5 s after its first uncounted message
  while it is active (through the alarm it already has). A room is evicted
  from memory only after 10 s without events, so no counts are lost.
- The Client object keeps exact daily counters (connections, connection
  seconds, bytes in and out, messages, rejections) and writes one row per
  client per day to the relay's D1 database when the day closes. It also
  remembers which rooms its connections are in, so that a rotation or deletion
  closes them (4403). It keeps the 64 most recent rooms, which is enough: a
  room it no longer lists is told nothing, and its sockets are refused at
  their next reconnection.
- Rate limit: at most 120 new connections per client per minute. There is no
  limit on connections at once (counting them would mean waking idle rooms).
- Generations as in clientmcp (`generations.rs`): one current generation per
  client; rotate +1, revoke and delete stop everything at once; at most 30 new
  generations per client per hour.

Never recorded: room IDs in usage rows (only in the Client object while the
connection lasts), frame contents (they are encrypted anyway), invites, keys,
IP addresses beyond what Cloudflare keeps.

### Issuance: only the relay signs

The site never holds the token secret. As in clientmcp, it asks the relay over
a **Service Binding**, with a service secret, on `/internal/…`: mint, rotate,
revoke, delete, the usage of a client, the finished days, the issuance audit.
Anything else on `/internal/` answers 404.

### Linux relay

`PEERCONNECT_TOKEN_SECRET` (whitespace-separated, newest first, as clientmcp's
`CLIENTMCP_SECRET`): with it, the relay requires tokens and checks signature
and expiry; revocation is by rotating the secret. `peerconnect-relay token
<client> [--ttl 30d]` mints one. Without it, nothing changes.

## The public trial

The demo on peerconnect.dev, and anyone trying PeerConnect for five minutes,
should not need an account. The site's `/api/trial` hands out a token of the
built-in app `trial`, valid 2 hours, to anyone, rate-limited per IP address
(10 an hour); trial clients have tighter limits (4 connections at once, 50 MB a
day). The demo fetches one before connecting. Docs quick starts use it too,
with a pointer to registering an app.

Anonymous connections are refused once this is live (owner's decision,
2026-10-28: "强制，留一个公开试用通道"). Old SDK versions then fail with 4403
until updated; npm 0.1.0 only knows close codes it already has, so it keeps
retrying. The switch is `REQUIRE_TOKEN=1` on the Worker, so it can go live
after the SDK release.

## The admin page

`peerconnect.dev/admin`, for the owner's GitHub ID only (404 to anyone else):
accounts (suspend, app and client limits), apps (disable), clients (revoke),
usage per day with the estimated Cloudflare cost, alerts, both audit logs.

## Metering and alerts

As clientmcp: exact daily counts in the Client objects (billing-grade), Workers
Analytics Engine for the live view and alerts, a scheduled check every 10
minutes against thresholds (connections per minute, bytes per hour, rejections
from one client), alerts on the admin page. No quota yet: rate limits only
against blatant abuse; thresholds are configuration.

## Where it runs

- **Site (`peerconnect-website`):** the landing page, docs and spec stay
  prerendered; `/connect`, `/account`, `/developers`, `/admin`, `/auth/*` and
  `/api/*` become server routes. D1 `peerconnect-site` (accounts, identities,
  apps, clients, codes, audit); a Service Binding to `peerconnect-relay`;
  secrets `SERVICE_SECRET`, `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`,
  `SESSION_KEY`, `APP_SECRET_KEY`.
- **Relay (`peerconnect-relay`):** the Client object (migration v4), token
  verification, the internal API, metering; D1 `peerconnect-relay` (daily
  usage, issuance audit); secrets `PEERCONNECT_TOKEN_SECRET`,
  `PEERCONNECT_SERVICE_SECRET`.
- **SDKs:** `connect(room, { token })` and `setToken` in JS and Rust;
  `peerconnect/auth` in JS (`startConnect` / `finishConnect`: PKCE, state,
  installation ID, the exchange) for web apps; the same steps documented for
  native apps.

## Phases

| # | What | Exit |
|---|---|---|
| 1 | Protocol and relays: tokens, 4403, Client object with generations, internal API, metering; Linux relay's secret; SDK `token` | Rust, JS and Worker e2e, with and without tokens |
| 2 | Site: GitHub sign-in, D1, `/developers` (apps), `/connect`, `/api/token`, `/account`; `peerconnect/auth` | e2e against local workerd and a fake GitHub |
| 3 | Trial tokens and the demo on them; admin page, alerts, cost estimate | e2e |
| 4 | Rollout: the GitHub OAuth app, D1 databases, secrets, deploy, then `REQUIRE_TOKEN=1` after the SDK release | the owner signs in for real |

## What the owner needs to provide

- A GitHub OAuth app for peerconnect.dev: callback
  `https://peerconnect.dev/auth/github/callback`. Its client ID and secret are
  entered as Worker secrets, never committed.
- The OK for each deploy, and for the switch to required tokens.
