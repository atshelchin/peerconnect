// The checks every signed-in page makes: who is signed in (or off to sign in
// and back), and that a form was sent by this site's own page (CSRF).

import { error, redirect, type Cookies } from '@sveltejs/kit';
import type { SiteConfig } from './config.ts';
import { account as accountOf, type Account } from './db.ts';
import { csrfToken, csrfValid, readSession, type Session } from './session.ts';

/** The signed-in account, or off to sign in and back to exactly this page. */
export async function signedIn(
	config: SiteConfig,
	cookies: Cookies,
	url: URL
): Promise<{ session: Session; acct: Account; csrf: string }> {
	const session = await readSession(cookies, config.sessionKey);
	const acct = session && (await accountOf(config.db, session.account));
	if (!session || !acct)
		redirect(303, `/auth/github/start?next=${encodeURIComponent(url.pathname + url.search)}`);
	return { session, acct, csrf: await csrfToken(config.sessionKey, session) };
}

/** [`signedIn`], for a form that changes something: its CSRF token checked, the account active. */
export async function signedInForm(
	config: SiteConfig,
	cookies: Cookies,
	url: URL,
	request: Request
): Promise<{ session: Session; acct: Account; form: FormData }> {
	const { session, acct } = await signedIn(config, cookies, url);
	const form = await request.formData();
	if (!(await csrfValid(config.sessionKey, session, form.get('csrf')))) {
		error(403, 'This form has expired. Please reload the page.');
	}
	if (acct.state !== 'active') error(403, 'This account is suspended.');
	return { session, acct, form };
}

/** For a JSON request from this site's own page: the Origin and the page's CSRF token. */
export async function signedInJson(
	config: SiteConfig,
	cookies: Cookies,
	url: URL,
	request: Request
): Promise<{ acct: Account; body: Record<string, unknown> }> {
	if (request.headers.get('origin') !== url.origin) error(403, 'Cross-site request.');
	const session = await readSession(cookies, config.sessionKey);
	const acct = session && (await accountOf(config.db, session.account));
	if (!session || !acct) error(401, 'Sign in again.');
	if (acct.state !== 'active') error(403, 'This account is suspended.');
	const body = (await request.json().catch(() => ({}))) as Record<string, unknown>;
	const csrf = typeof body.csrf === 'string' ? body.csrf : null;
	if (!(await csrfValid(config.sessionKey, session, csrf))) {
		error(403, 'This page has expired. Please reload it.');
	}
	return { acct, body };
}
