import { error, redirect } from '@sveltejs/kit';
import { constantTimeEqual, randomToken } from '#lib/server/crypto.ts';
import { siteConfig } from '#lib/server/config.ts';
import { signIn } from '#lib/server/db.ts';
import { userFor } from '#lib/server/github.ts';
import { startSession, takeOAuth } from '#lib/server/session.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

/**
 * GitHub's answer. The state must be the one this browser was sent off with
 * (a forged or replayed callback is refused), and it is used once.
 */
export const GET: RequestHandler = async ({ url, cookies }) => {
	const config = siteConfig(url);
	const remembered = await takeOAuth(cookies, config.sessionKey, config.secureCookies);
	const state = url.searchParams.get('state') ?? '';
	const code = url.searchParams.get('code') ?? '';
	if (!remembered || !state || !constantTimeEqual(state, remembered.state)) {
		error(400, 'This sign-in link has expired or was not started here. Please sign in again.');
	}
	if (!code) error(400, 'GitHub did not sign you in.');
	const user = await userFor(config.github, code, `${url.origin}/auth/github/callback`);
	if (!user) error(502, 'GitHub could not confirm who you are. Please try again.');
	const account = await signIn(config.db, user);
	await startSession(
		cookies,
		config.sessionKey,
		{ account, sid: randomToken(16) },
		config.secureCookies
	);
	redirect(303, remembered.next);
};
