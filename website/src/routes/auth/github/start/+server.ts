import { redirect } from '@sveltejs/kit';
import { randomToken } from '#lib/server/crypto.ts';
import { siteConfig } from '#lib/server/config.ts';
import { authorizeUrl } from '#lib/server/github.ts';
import { rememberOAuth, safeNext } from '#lib/server/session.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

/** Off to GitHub, with a fresh state bound to this browser and where to come back to. */
export const GET: RequestHandler = async ({ url, cookies }) => {
	const config = siteConfig(url);
	const state = randomToken(24);
	await rememberOAuth(
		cookies,
		config.sessionKey,
		{ state, next: safeNext(url.searchParams.get('next')) },
		config.secureCookies
	);
	// GitHub's sign-in page is the one other origin this site sends anyone to.
	redirect(303, authorizeUrl(config.github, `${url.origin}/auth/github/callback`, state), {
		external: [new URL(config.github.oauthBase).origin]
	});
};
