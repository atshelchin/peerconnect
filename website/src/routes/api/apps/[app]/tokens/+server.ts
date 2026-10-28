import { error, json } from '@sveltejs/kit';
import { isUserId } from '#lib/server/apps.ts';
import { authorizedApp } from '#lib/server/bearer.ts';
import { siteConfig } from '#lib/server/config.ts';
import { userKey } from '#lib/server/crypto.ts';
import { LimitReached, createUserClient, userClient } from '#lib/server/db.ts';
import { RelayError, mint, rotate } from '#lib/server/relay.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

/**
 * A token for one of the app's own users (docs/self-serve.md, "Tokens from
 * the developer's server"): `{ user, rotate? }`. The same user gets the same
 * token until it is rotated; the use counts as the developer's.
 */
export const POST: RequestHandler = async ({ url, request, params }) => {
	const config = siteConfig(url);
	const app = await authorizedApp(config, request, params.app);
	const body = (await request.json().catch(() => ({}))) as Record<string, unknown>;
	if (!isUserId(body.user)) error(400, '`user` is 1-128 characters of A-Z a-z 0-9 . _ - : @');
	const key = await userKey(config.appSecretKey, app.id, body.user);
	try {
		const existing = await userClient(config.db, app.id, key);
		const client = existing?.id ?? (await createUserClient(config.db, app, key));
		const actor = `app:${app.id}`;
		const minted =
			existing && body.rotate === true
				? await rotate(config.relay, client, actor)
				: await mint(config.relay, client, app.id, actor);
		return json(
			{
				token: minted.token,
				expires_at: minted.exp,
				relay_url: config.relayPublicUrl,
				client_id: client
			},
			{ headers: { 'cache-control': 'no-store' } }
		);
	} catch (e) {
		if (e instanceof LimitReached)
			error(409, `This application can issue tokens for ${app.user_limit} users.`);
		if (e instanceof RelayError) error(503, 'The relay could not issue a token. Please try again.');
		throw e;
	}
};
