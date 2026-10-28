// An app's own server, recognised by its app secret as a bearer token
// (docs/self-serve.md, "Tokens from the developer's server").

import { error } from '@sveltejs/kit';
import type { App } from './apps.ts';
import type { SiteConfig } from './config.ts';
import { appSecret, constantTimeEqual } from './crypto.ts';
import { app as appOf } from './db.ts';

/**
 * The app named in the path, if the request carries its secret as a bearer
 * token. Server to server: no CORS, ever.
 */
export async function authorizedApp(
	config: SiteConfig,
	request: Request,
	id: string
): Promise<App> {
	const app = await appOf(config.db, id);
	const offered = (request.headers.get('authorization') ?? '').replace(/^Bearer\s+/i, '');
	if (!app || app.state !== 'active' || !offered)
		error(401, 'Unknown application or wrong app secret.');
	const expected = await appSecret(config.appSecretKey, app.id, app.secret_gen);
	if (!constantTimeEqual(offered, expected)) error(401, 'Unknown application or wrong app secret.');
	return app;
}
