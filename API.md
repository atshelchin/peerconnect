# PeerConnect API

PeerConnect is offered as an API: the relay protocol, which carries the end-to-end encrypted traffic between two peers ([SPEC.md](SPEC.md)), and the HTTP API at `https://peerconnect.dev`, which gives applications the tokens the public relay `wss://relay.peerconnect.dev` asks for. This document is the HTTP API, and how tokens enter the relay protocol.

There is no SDK to install. Your application implements the peer side of [SPEC.md](SPEC.md); the repository's own implementations, in Rust (`crates/peerconnect`) and in JavaScript (`js/`, the Rust core compiled to WebAssembly), are there to read and to test against, not as supported packages. [test-vectors/](test-vectors) has the values an implementation must reproduce.

## 1. Overview

| Base URL | What |
|---|---|
| `https://peerconnect.dev` | sign-in, applications, tokens (this document) |
| `wss://relay.peerconnect.dev` | the relay (SPEC §4) |

1. A developer signs in on `https://peerconnect.dev/developers` with GitHub and registers an application. It gets an **app ID** (`app_` and 16 characters; public) and an **app secret** (`pcs_` and 43 characters; keep it on a server).
2. Every connection to the relay carries a **token**. An application gets tokens on one of two paths:
   - **Connect with PeerConnect** (§4): the person using the app signs in on peerconnect.dev and confirms; the app exchanges the code it gets back for that person's token. OAuth 2.0 authorization code with PKCE (RFC 6749, RFC 7636, RFC 8252).
   - **Server-issued tokens** (§5): the app's server asks for a token per user of its own, with the app secret. The people using the app never see PeerConnect; their use counts as the developer's.
3. To try PeerConnect without an account, take a **trial token** (§3).

Tokens identify who uses the relay and nothing more: the relay never sees keys, invites or plaintext.

## 2. Tokens on the relay

A peer appends its token to the relay endpoint (SPEC §4.1):

```
wss://relay.peerconnect.dev/v1/<room_id>?token=<token>
```

- Tokens are opaque strings of at most 2,048 characters (`A–Z a–z 0–9 - _ .`). Do not parse them.
- Each peer uses its own token. A token is never part of an invite or a room record, and the two peers of a room may use tokens of different applications.
- A token lasts about a year (trial tokens, two hours) and stays the same until it is rotated or revoked. Store it like a password.
- The relay closes a connection whose token is missing, invalid, expired or revoked with **4403** before it sends HELLO. Do not reconnect with the same token: get a new one first (§3–§5), then connect again. The relay closes connections still open on a token when it is rotated or revoked, in the same way.
- **4429**: the client opens connections too often (more than 120 a minute; trial tokens, 20, and 50 MB a day). Wait at least 60 s.
- A browser reports the close code in the WebSocket `close` event; native libraries do likewise. An HTTP status would not reach a browser, which is why the relay accepts the upgrade and then closes.

## 3. Trial tokens

```
POST https://peerconnect.dev/api/trial
```

No body, no account. Answers from any origin (`access-control-allow-origin: *`).

```json
{ "token": "eyJ…", "expires_at": 1793160000, "relay_url": "wss://relay.peerconnect.dev" }
```

`expires_at` is in Unix seconds, two hours ahead. At most 10 trial tokens per address per hour; beyond that `429` with `{"error": "…"}` and `retry-after`. For anything but trying things out, register an application.

## 4. Connect with PeerConnect

### 4.1 Registration

On `https://peerconnect.dev/developers/<app_id>` the developer lists:

- **Redirect URIs**, compared exactly (no prefix match, no added query): `https://` URLs; private-use schemes in reverse-domain form for mobile apps (`com.example.app:/peerconnect`, RFC 8252 §7.1); `http://localhost:<port>/…` and `http://127.0.0.1:<port>/…` while developing. At most 10.
- **Web origins** (`https://example.com`), whose pages may exchange codes from the browser (CORS on §4.4). At most 10.
- A **loopback path** for desktop apps (RFC 8252 §7.3): then `http://127.0.0.1:<any port><path>` is accepted as a redirect URI.

### 4.2 The authorization request

The app creates a random **code verifier** (43–128 characters of `A–Z a–z 0–9 - . _ ~`; 32 random bytes in base64url is right) and its **challenge**, `base64url(SHA-256(verifier))` without padding, and a random **state**. It keeps both, then opens in the browser (the system browser for native apps):

```
GET https://peerconnect.dev/connect
      ?app=<app_id>
      &redirect_uri=<a registered redirect URI>
      &state=<8–256 characters of A–Z a–z 0–9 - . _ ~>
      &code_challenge=<43 characters>
      &code_challenge_method=S256
      &installation=<8–64 characters of A–Z a–z 0–9 - . _ ~>
      &label=<what to call this device, optional, ≤40 characters>
```

`installation` is the same for every request from one installation of the app (generate it once and keep it). Connecting again from the same installation replaces its token: the old one stops.

The person signs in with GitHub if they are not, and confirms. A request with an unknown app, an unregistered redirect URI or a missing parameter is never redirected: the page says what is wrong (RFC 6749 §4.1.2.1).

### 4.3 The redirect

```
<redirect_uri>?code=<code>&state=<state>
```

Check that `state` is the one you sent. The code works once, within two minutes.

### 4.4 The token request

```
POST https://peerconnect.dev/api/token
content-type: application/json

{
  "grant_type": "authorization_code",
  "client_id": "<app_id>",
  "code": "<code>",
  "code_verifier": "<verifier>",
  "redirect_uri": "<the same redirect URI>",
  "client_secret": "<app secret>"
}
```

`client_secret` is for apps that exchange the code on their server; then it must be right. From a browser, leave it out: the request must come from one of the app's web origins. JSON only: form-encoded bodies are refused.

Answer, `200`, `cache-control: no-store`:

```json
{
  "token_type": "peerconnect",
  "token": "eyJ…",
  "expires_at": 1824696000,
  "relay_url": "wss://relay.peerconnect.dev",
  "client_id": "c-…",
  "login": "octocat"
}
```

`login` is the person's GitHub login, to show "connected as @octocat". `client_id` names this connection on their account page, where they can delete it (its token then stops).

Errors (RFC 6749 §5.2), as `{"error": "…", "error_description": "…"}`:

| Status | `error` | When |
|---|---|---|
| 400 | `unsupported_grant_type` | `grant_type` is not `authorization_code` |
| 400 | `invalid_request` | a field is missing |
| 401 | `invalid_client` | unknown or disabled app, or a wrong `client_secret` |
| 400 | `invalid_grant` | the code is unknown, used, expired, for another app or redirect URI, or the verifier does not match. A failed attempt uses the code up |
| 503 | `temporarily_unavailable` | the relay could not issue the token; start again |

## 5. Server-issued tokens

From the app's server only (no CORS).

### 5.1 A token for a user

```
POST https://peerconnect.dev/api/apps/<app_id>/tokens
authorization: Bearer <app secret>
content-type: application/json

{ "user": "<your user ID>", "rotate": false }
```

`user` is your own identifier for the user, 1–128 characters of `A–Z a–z 0–9 . _ - : @`; PeerConnect stores only an HMAC of it. The first request for a user creates their client; later ones return the same token, unless `"rotate": true`, which issues a new one and stops the old one (and its connections).

```json
{ "token": "eyJ…", "expires_at": 1824696000, "relay_url": "wss://relay.peerconnect.dev", "client_id": "c-…" }
```

### 5.2 Stopping a user's token

```
DELETE https://peerconnect.dev/api/apps/<app_id>/users/<your user ID>
authorization: Bearer <app secret>
```

`200 {"deleted": "c-…"}`, or `404` if no token was issued for that user. The token stops at once, connections included; a later §5.1 request makes a new client.

### 5.3 Errors

As `{"message": "…"}`:

| Status | When |
|---|---|
| 400 | `user` is missing or malformed |
| 401 | unknown or disabled app, or a wrong app secret |
| 409 | the app has reached its user limit (10,000 unless raised) |
| 503 | the relay could not issue the token; try again |

## 6. The app secret

The app secret is shown, and can be shown again, on the app's page. **Rotate** it there if it may have leaked: the old one stops at once, for §4.4 and §5. Tokens already issued keep working. Deleting the app stops every token of its users.

## 7. What is recorded

Per client and per UTC day: connections, connected time, frames and bytes each way, and refusals. Never message contents (which are end-to-end encrypted), room IDs, keys, invites, or IP addresses beyond what Cloudflare keeps. The developer sees the totals for their app; a person sees their own connections.

## 8. Example: a browser app, without any library

```js
const SITE = "https://peerconnect.dev";
const b64url = (bytes) => btoa(String.fromCharCode(...bytes)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

// "Connect with PeerConnect"
async function start(appId, redirectUri) {
  const verifier = b64url(crypto.getRandomValues(new Uint8Array(32)));
  const challenge = b64url(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier))));
  const state = b64url(crypto.getRandomValues(new Uint8Array(16)));
  const installation = localStorage.installation ??= b64url(crypto.getRandomValues(new Uint8Array(16)));
  sessionStorage.setItem(state, verifier);
  location.assign(`${SITE}/connect?${new URLSearchParams({
    app: appId, redirect_uri: redirectUri, state, code_challenge: challenge,
    code_challenge_method: "S256", installation, label: "Browser",
  })}`);
}

// On the redirect URI's page
async function finish(appId, redirectUri) {
  const params = new URLSearchParams(location.search);
  const verifier = sessionStorage.getItem(params.get("state"));
  if (!verifier) throw new Error("not started here");
  const response = await fetch(`${SITE}/api/token`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ grant_type: "authorization_code", client_id: appId, code: params.get("code"), code_verifier: verifier, redirect_uri: redirectUri }),
  });
  const answer = await response.json();
  if (!response.ok) throw new Error(answer.error_description);
  return answer; // { token, relay_url, … }: then wss://relay.peerconnect.dev/v1/<room_id>?token=<token>
}
```

And from a server:

```sh
curl https://peerconnect.dev/api/apps/app_…/tokens \
  -H "authorization: Bearer $PEERCONNECT_APP_SECRET" \
  -H "content-type: application/json" \
  -d '{"user": "user-42"}'
```
