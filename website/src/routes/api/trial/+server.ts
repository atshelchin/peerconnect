import { json } from '@sveltejs/kit';
import { siteConfig } from '#lib/server/config.ts';
import { hmac, randomBase32 } from '#lib/server/crypto.ts';
import { takeTrial } from '#lib/server/db.ts';
import { RelayError, TRIAL_APP, TRIAL_HOURS, mint } from '#lib/server/relay.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

/** Trial tokens per address per hour. */
const PER_HOUR = 10;

const HEADERS = { 'cache-control': 'no-store', 'access-control-allow-origin': '*' };

/**
 * A token of the public trial (docs/self-serve.md, "The public trial"):
 * anyone may have one, from any page, without an account. It lasts two
 * hours, and the relay gives it tighter limits than an app's.
 */
export const POST: RequestHandler = async ({ url, request }) => {
	const config = siteConfig(url);
	const address = request.headers.get('cf-connecting-ip') ?? 'local';
	const key = await hmac(config.appSecretKey, `trial-address:${address}`);
	if (!(await takeTrial(config.db, key, PER_HOUR, 3600))) {
		return json(
			{ error: 'Too many trial tokens from this address. Register an app on peerconnect.dev.' },
			{ status: 429, headers: { ...HEADERS, 'retry-after': '3600' } }
		);
	}
	try {
		const minted = await mint(
			config.relay,
			`t-${randomBase32(20)}`,
			TRIAL_APP,
			'trial',
			TRIAL_HOURS * 3600
		);
		return json(
			{ token: minted.token, expires_at: minted.exp, relay_url: config.relayPublicUrl },
			{ headers: HEADERS }
		);
	} catch (e) {
		if (e instanceof RelayError)
			return json(
				{ error: 'The relay could not issue a token.' },
				{ status: 503, headers: HEADERS }
			);
		throw e;
	}
};
