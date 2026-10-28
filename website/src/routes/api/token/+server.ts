import { json } from '@sveltejs/kit';
import { siteConfig, type SiteConfig } from '#lib/server/config.ts';
import { appSecret, constantTimeEqual, pkceMatches, sha256 } from '#lib/server/crypto.ts';
import {
	account as accountOf,
	app as appOf,
	audit,
	originAllowed,
	takeCode
} from '#lib/server/db.ts';
import { RelayError, mint, rotate } from '#lib/server/relay.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

const NO_STORE = { 'cache-control': 'no-store', pragma: 'no-cache' };

/**
 * A registered web app's page exchanges its code from the browser: CORS for
 * an origin its developer registered, without credentials (the code and the
 * verifier are the proof). The preflight cannot say which app it is for, so
 * it is answered for any app's origin, and the exchange checks the app's own.
 */
async function cors(config: SiteConfig, request: Request, app: string | null) {
	const origin = request.headers.get('origin');
	const allowed = origin !== null && (await originAllowed(config.db, origin, app));
	return allowed
		? { 'access-control-allow-origin': origin, vary: 'origin' }
		: ({ vary: 'origin' } as Record<string, string>);
}

export const OPTIONS: RequestHandler = async ({ url, request }) => {
	const headers = await cors(siteConfig(url), request, null);
	if (!headers['access-control-allow-origin']) return new Response(null, { status: 403, headers });
	return new Response(null, {
		status: 204,
		headers: {
			...headers,
			'access-control-allow-methods': 'POST',
			'access-control-allow-headers': 'content-type',
			'access-control-max-age': '600'
		}
	});
};

/**
 * The app's half of the PKCE exchange (RFC 7636): the code from the redirect
 * and the verifier only the app holds, for the person's relay token. An app
 * exchanging on its server sends its app secret too (`client_secret`). The
 * code is used up by the statement that reads it, so a second attempt — or
 * one with the wrong verifier — finds nothing to try again with.
 *
 * The body is JSON. (Form-encoded bodies, RFC 6749's usual, are refused by
 * SvelteKit's cross-site check before this runs.)
 */
export const POST: RequestHandler = async ({ url, request }) => {
	const config = siteConfig(url);
	const raw: unknown = await request.json().catch(() => ({}));
	const params: Record<string, string> = {};
	if (raw && typeof raw === 'object') {
		for (const [key, value] of Object.entries(raw))
			if (typeof value === 'string') params[key] = value;
	}
	const headers = { ...NO_STORE, ...(await cors(config, request, params.client_id ?? '')) };
	/** RFC 6749 §5.2. */
	const refuse = (error: string, description: string, status = 400) =>
		json({ error, error_description: description }, { status, headers });
	if (params.grant_type !== 'authorization_code') {
		return refuse('unsupported_grant_type', 'Only authorization_code is supported.');
	}
	const code = params.code ?? '';
	const verifier = params.code_verifier ?? '';
	const redirectUri = params.redirect_uri ?? '';
	const appId = params.client_id ?? '';
	if (!code || !verifier || !redirectUri || !appId)
		return refuse(
			'invalid_request',
			'code, code_verifier, redirect_uri and client_id (the app ID) are required.'
		);
	const app = await appOf(config.db, appId);
	if (!app || app.state !== 'active') return refuse('invalid_client', 'Unknown application.', 401);
	if (params.client_secret !== undefined) {
		const expected = await appSecret(config.appSecretKey, app.id, app.secret_gen);
		if (!constantTimeEqual(params.client_secret, expected))
			return refuse('invalid_client', 'The app secret is wrong.', 401);
	}

	const row = await takeCode(config.db, await sha256(code));
	if (!row) return refuse('invalid_grant', 'The code is unknown, used or expired.');
	if (
		row.app !== app.id ||
		row.redirect_uri !== redirectUri ||
		!(await pkceMatches(verifier, row.code_challenge))
	) {
		await audit(config.db, row.account_id, 'token-refused', row.client_id, 'app');
		return refuse('invalid_grant', 'The code does not match this request.');
	}
	const acct = await accountOf(config.db, row.account_id);
	if (!acct || acct.state !== 'active')
		return refuse('invalid_grant', 'The account is not active.');

	try {
		const actor = `account:${acct.id}`;
		// The same installation signing in again: its old token stops.
		const minted = row.rotate
			? await rotate(config.relay, row.client_id, actor)
			: await mint(config.relay, row.client_id, app.id, actor);
		await audit(config.db, acct.id, 'token-exchanged', `${row.client_id} ${app.id}`, 'app');
		return json(
			{
				token_type: 'peerconnect',
				token: minted.token,
				expires_at: minted.exp,
				relay_url: config.relayPublicUrl,
				client_id: row.client_id,
				login: acct.login
			},
			{ headers }
		);
	} catch (e) {
		if (e instanceof RelayError)
			return refuse('temporarily_unavailable', 'The relay could not issue a token.', 503);
		throw e;
	}
};
