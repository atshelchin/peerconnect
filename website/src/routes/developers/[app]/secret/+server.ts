import { error, json } from '@sveltejs/kit';
import { siteConfig } from '#lib/server/config.ts';
import { appSecret } from '#lib/server/crypto.ts';
import { ownApp } from '#lib/server/db.ts';
import { signedInJson } from '#lib/server/guard.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

/**
 * The app secret, for the page's Show and Copy buttons: fetched only then,
 * so the page itself never holds it. Derived, not stored (crypto.ts): the
 * same secret each time until it is rotated.
 */
export const POST: RequestHandler = async ({ url, cookies, request, params }) => {
	const config = siteConfig(url);
	const { acct } = await signedInJson(config, cookies, url, request);
	const app = await ownApp(config.db, acct.id, params.app);
	if (!app) error(404, 'No such application.');
	return json(
		{ secret: await appSecret(config.appSecretKey, app.id, app.secret_gen) },
		{ headers: { 'cache-control': 'no-store' } }
	);
};
