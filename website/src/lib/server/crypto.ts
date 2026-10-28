// The site's cryptography: signed values (sessions, the sign-in state),
// PKCE (RFC 7636), and random identifiers. WebCrypto only, so it runs the
// same in the Worker and in Node's tests.

const encoder = new TextEncoder();

export function base64url(bytes: Uint8Array): string {
	let binary = '';
	for (const b of bytes) binary += String.fromCharCode(b);
	return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export function fromBase64url(text: string): Uint8Array | null {
	if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
	const padded =
		text.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (text.length % 4)) % 4);
	try {
		return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
	} catch {
		return null;
	}
}

/** `n` random bytes, base64url. */
export function randomToken(n = 32): string {
	return base64url(crypto.getRandomValues(new Uint8Array(n)));
}

const BASE32 = 'abcdefghijklmnopqrstuvwxyz234567';

/** `n` random lower-case base32 characters. */
export function randomBase32(n: number): string {
	const bytes = crypto.getRandomValues(new Uint8Array(n));
	return Array.from(bytes, (b) => BASE32[b & 31]).join('');
}

/** An opaque client ID (docs/self-serve.md): `c-` and 20 base32 characters. */
export function newClientId(): string {
	return `c-${randomBase32(20)}`;
}

/** An opaque account ID. */
export function newAccountId(): string {
	return `a-${randomBase32(20)}`;
}

/** An application ID (the OAuth client ID): `app_` and 16 base32 characters. */
export function newAppId(): string {
	return `app_${randomBase32(16)}`;
}

/**
 * An app's secret at generation `gen` (docs/self-serve.md): derived, never
 * stored, so the developer can show it again, and rotating it is `gen + 1`.
 */
export async function appSecret(key: string, app: string, gen: number): Promise<string> {
	return `pcs_${await hmac(key, `app-secret:${app}:${gen}`)}`;
}

/** What is stored of an app's own user ID: never the ID itself. */
export async function userKey(key: string, app: string, user: string): Promise<string> {
	return hmac(key, `app-user:${app}:${user}`);
}

export async function sha256(text: string): Promise<string> {
	return base64url(new Uint8Array(await crypto.subtle.digest('SHA-256', encoder.encode(text))));
}

async function hmacKey(secret: string): Promise<CryptoKey> {
	return crypto.subtle.importKey(
		'raw',
		encoder.encode(secret),
		{ name: 'HMAC', hash: 'SHA-256' },
		false,
		['sign']
	);
}

export async function hmac(secret: string, data: string): Promise<string> {
	const signature = await crypto.subtle.sign('HMAC', await hmacKey(secret), encoder.encode(data));
	return base64url(new Uint8Array(signature));
}

/** Equal strings, in time that does not depend on where they differ. */
export function constantTimeEqual(a: string, b: string): boolean {
	if (a.length !== b.length) return false;
	let diff = 0;
	for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
	return diff === 0;
}

/**
 * `value` as JSON, signed: `<payload>.<signature>`. `expiresAt` (epoch
 * seconds) is inside the signature, so a signed value cannot outlive it.
 */
export async function sign(
	secret: string,
	purpose: string,
	value: unknown,
	expiresAt: number
): Promise<string> {
	const payload = base64url(encoder.encode(JSON.stringify({ p: purpose, v: value, e: expiresAt })));
	return `${payload}.${await hmac(secret, payload)}`;
}

/** The value of a [`sign`]ed string for `purpose`, or `null` if forged, for another purpose, or expired. */
export async function verify<T>(
	secret: string,
	purpose: string,
	signed: string,
	now: number
): Promise<T | null> {
	const dot = signed.lastIndexOf('.');
	if (dot <= 0) return null;
	const payload = signed.slice(0, dot);
	if (!constantTimeEqual(signed.slice(dot + 1), await hmac(secret, payload))) return null;
	const bytes = fromBase64url(payload);
	if (!bytes) return null;
	try {
		const { p, v, e } = JSON.parse(new TextDecoder().decode(bytes));
		if (p !== purpose || typeof e !== 'number' || now >= e) return null;
		return v as T;
	} catch {
		return null;
	}
}

/** RFC 7636: a verifier is 43–128 characters of `[A-Za-z0-9-._~]`. */
export function isCodeVerifier(text: string): boolean {
	return /^[A-Za-z0-9\-._~]{43,128}$/.test(text);
}

/** RFC 7636 S256: a challenge is the base64url SHA-256 of the verifier, 43 characters. */
export function isCodeChallenge(text: string): boolean {
	return /^[A-Za-z0-9_-]{43}$/.test(text);
}

/** Whether `verifier` answers `challenge` (S256). */
export async function pkceMatches(verifier: string, challenge: string): Promise<boolean> {
	if (!isCodeVerifier(verifier) || !isCodeChallenge(challenge)) return false;
	return constantTimeEqual(await sha256(verifier), challenge);
}
