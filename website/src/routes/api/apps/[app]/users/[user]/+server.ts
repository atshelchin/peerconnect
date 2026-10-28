import { error, json } from '@sveltejs/kit';
import { isUserId } from '#lib/server/apps.ts';
import { authorizedApp } from '#lib/server/bearer.ts';
import { siteConfig } from '#lib/server/config.ts';
import { userKey } from '#lib/server/crypto.ts';
import { deleteClient, userClient } from '#lib/server/db.ts';
import { RelayError, remove } from '#lib/server/relay.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

/** The user's token stops at once, and their client is gone; a later request makes a new one. */
export const DELETE: RequestHandler = async ({ url, request, params }) => {
	const config = siteConfig(url);
	const app = await authorizedApp(config, request, params.app);
	if (!isUserId(params.user)) error(400, 'Not a user ID.');
	const client = await userClient(
		config.db,
		app.id,
		await userKey(config.appSecretKey, app.id, params.user)
	);
	if (!client) error(404, 'No token was issued for this user.');
	try {
		await remove(config.relay, client.id, `app:${app.id}`);
	} catch (e) {
		if (e instanceof RelayError) error(503, 'The relay could not do that. Please try again.');
		throw e;
	}
	await deleteClient(config.db, client.id, `app:${app.id}`, app.account_id);
	return json({ deleted: client.id });
};
