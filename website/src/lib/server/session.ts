// Who is signed in: a signed, HTTP-only, SameSite=Lax cookie naming the
// account (docs/self-serve.md, "Where it runs"). Nothing is stored for it.
// Every form that changes something carries a CSRF token derived from the
// session, so a page elsewhere cannot submit one on the person's behalf.

import type { Cookies } from '@sveltejs/kit';
import { constantTimeEqual, hmac, sign, verify } from './crypto.ts';

export const SESSION_COOKIE = 'pc_session';
const SESSION_DAYS = 30;
const OAUTH_COOKIE = 'pc_oauth';
const OAUTH_MINUTES = 10;

export interface Session {
	account: string;
	/** A random ID per sign-in, which the CSRF token is bound to. */
	sid: string;
}

const now = () => Math.floor(Date.now() / 1000);

function cookieOptions(secure: boolean, maxAge: number) {
	return { path: '/', httpOnly: true, sameSite: 'lax' as const, secure, maxAge };
}

export async function startSession(
	cookies: Cookies,
	key: string,
	session: Session,
	secure: boolean
) {
	const maxAge = SESSION_DAYS * 24 * 3600;
	cookies.set(
		SESSION_COOKIE,
		await sign(key, 'session', session, now() + maxAge),
		cookieOptions(secure, maxAge)
	);
}

export async function readSession(cookies: Cookies, key: string): Promise<Session | null> {
	const raw = cookies.get(SESSION_COOKIE);
	return raw ? verify<Session>(key, 'session', raw, now()) : null;
}

export function endSession(cookies: Cookies, secure: boolean) {
	cookies.delete(SESSION_COOKIE, cookieOptions(secure, 0));
}

export async function csrfToken(key: string, session: Session): Promise<string> {
	return hmac(key, `csrf:${session.sid}`);
}

export async function csrfValid(
	key: string,
	session: Session,
	offered: FormDataEntryValue | null
): Promise<boolean> {
	return typeof offered === 'string' && constantTimeEqual(offered, await csrfToken(key, session));
}

/** The GitHub round trip's state, and where to go after it, for ten minutes. */
export interface OAuthState {
	state: string;
	next: string;
}

export async function rememberOAuth(
	cookies: Cookies,
	key: string,
	value: OAuthState,
	secure: boolean
) {
	const maxAge = OAUTH_MINUTES * 60;
	cookies.set(
		OAUTH_COOKIE,
		await sign(key, 'oauth', value, now() + maxAge),
		cookieOptions(secure, maxAge)
	);
}

export async function takeOAuth(
	cookies: Cookies,
	key: string,
	secure: boolean
): Promise<OAuthState | null> {
	const raw = cookies.get(OAUTH_COOKIE);
	cookies.delete(OAUTH_COOKIE, cookieOptions(secure, 0));
	return raw ? verify<OAuthState>(key, 'oauth', raw, now()) : null;
}

/** Where to go after signing in: a path on this site, never another origin. */
export function safeNext(next: string | null): string {
	if (!next || !next.startsWith('/') || next.startsWith('//') || next.includes('\\'))
		return '/account';
	return next;
}
