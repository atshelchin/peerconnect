// "Connect with PeerConnect" for web apps (docs/self-serve.md, "Returning
// the token to the app"): the OAuth 2.0 authorization code flow with PKCE
// (RFC 7636) against peerconnect.dev. No WebAssembly, no dependencies.
//
//   startConnect({ appId, redirectUri })        → off to peerconnect.dev
//   await finishConnect({ appId })              → on the redirect page: the token
//
// Native apps take the same steps with `connectRequest` and `exchangeCode`,
// opening the URL in the system browser and receiving the redirect on their
// own scheme or loopback port.
//
// To try PeerConnect without an account: `connect(room, { token: trialToken })`.

const SITE = "https://peerconnect.dev";
const PENDING = "peerconnect-auth:";
const INSTALLATION = "peerconnect-installation";

const encoder = new TextEncoder();

function base64url(bytes) {
  let binary = "";
  for (const b of bytes) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

const random = (n) => base64url(crypto.getRandomValues(new Uint8Array(n)));

/** This installation's stable ID (signing in again from it replaces its token), kept in localStorage. */
export function installationId() {
  try {
    let id = localStorage.getItem(INSTALLATION);
    if (!id) localStorage.setItem(INSTALLATION, (id = random(16)));
    return id;
  } catch {
    return random(16); // storage unavailable: a new installation each time
  }
}

/**
 * The URL to open, with the state and the PKCE verifier to keep until the
 * redirect comes back. `installation` is stable per installation of the app,
 * `label` names it for the person ("Chrome on a Mac").
 */
export async function connectRequest({ appId, redirectUri, installation, label, site = SITE }) {
  const verifier = random(32);
  const challenge = base64url(new Uint8Array(await crypto.subtle.digest("SHA-256", encoder.encode(verifier))));
  const state = random(16);
  const url = new URL("/connect", site);
  url.search = new URLSearchParams({
    app: appId,
    redirect_uri: redirectUri,
    state,
    code_challenge: challenge,
    code_challenge_method: "S256",
    installation,
    ...(label ? { label } : {}),
  }).toString();
  return { url: url.toString(), state, verifier };
}

/**
 * Exchanges the redirect's `code` for the person's token. From a server,
 * pass `appSecret`; from a browser, the page's origin must be one of the
 * app's web origins. Resolves to `{ token, expires_at, relay_url, client_id, login }`.
 */
export async function exchangeCode({ appId, code, verifier, redirectUri, appSecret, site = SITE }) {
  const response = await fetch(new URL("/api/token", site), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      grant_type: "authorization_code",
      client_id: appId,
      code,
      code_verifier: verifier,
      redirect_uri: redirectUri,
      ...(appSecret ? { client_secret: appSecret } : {}),
    }),
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error_description ?? `peerconnect.dev answered ${response.status}`);
  return body;
}

/** Sends the browser to peerconnect.dev to connect this app; it comes back to `redirectUri`. */
export async function startConnect({ appId, redirectUri, label, site = SITE }) {
  const request = await connectRequest({ appId, redirectUri, installation: installationId(), label, site });
  sessionStorage.setItem(PENDING + request.state, JSON.stringify({ verifier: request.verifier, redirectUri }));
  location.assign(request.url);
}

/**
 * On the redirect URI's page: checks the state, exchanges the code, and
 * resolves to the token and the rest (see `exchangeCode`). Store the token
 * like a password. Rejects if the page was not reached from `startConnect`.
 */
export async function finishConnect({ appId, url = location.href, site = SITE }) {
  const params = new URL(url).searchParams;
  const state = params.get("state");
  const code = params.get("code");
  const pending = state && sessionStorage.getItem(PENDING + state);
  if (!code || !pending) throw new Error("This page was not opened by startConnect, or was opened twice.");
  sessionStorage.removeItem(PENDING + state);
  const { verifier, redirectUri } = JSON.parse(pending);
  return exchangeCode({ appId, code, verifier, redirectUri, site });
}

let trial;

/**
 * A token of the public trial: no account, two hours, tighter limits than an
 * app's. Pass the function itself as `connect`'s `token`: it is asked again
 * when the relay refuses an expired one. The same token is reused until it
 * is about to expire.
 */
export async function trialToken({ site = SITE } = {}) {
  const now = Date.now() / 1000;
  if (trial && trial.site === site && trial.expires_at - 60 > now) return trial.token;
  const response = await fetch(new URL("/api/trial", site), { method: "POST" });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error ?? `peerconnect.dev answered ${response.status}`);
  trial = { ...body, site };
  return body.token;
}
