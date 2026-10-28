import { error, fail, redirect } from '@sveltejs/kit';
import { appRequest, redirectWithCode, type AppRequest } from '#lib/server/apps.ts';
import { siteConfig, type SiteConfig } from '#lib/server/config.ts';
import { randomToken, sha256 } from '#lib/server/crypto.ts';
import {
	LimitReached,
	app as appOf,
	audit,
	bindClient,
	clients as clientsOf,
	createClient,
	installationClient,
	saveCode,
	unbindInstallation
} from '#lib/server/db.ts';
import { signedIn, signedInForm } from '#lib/server/guard.ts';
import { langOf, localize } from '#lib/i18n/index.ts';
import type { Actions, PageServerLoad } from './$types';

export const prerender = false;

/** The app's request, checked before anything else: a bad redirect is never followed. */
async function request(config: SiteConfig, url: URL): Promise<AppRequest> {
	const app = await appOf(config.db, url.searchParams.get('app') ?? '');
	const parsed = appRequest(app, url.searchParams);
	if ('error' in parsed) error(400, parsed.error);
	return parsed.ok;
}

export const load: PageServerLoad = async ({ url, cookies }) => {
	const config = siteConfig(url);
	if (!url.searchParams.has('app')) redirect(303, localize('/account', langOf(url.pathname)));
	const app = await request(config, url);
	const { acct, csrf } = await signedIn(config, cookies, url);
	if (acct.state !== 'active') error(403, 'This account is suspended.');
	// An app may connect as one of the person's connections to it, or a new
	// one: the one this installation already is comes first and is chosen.
	const choices = (await clientsOf(config.db, acct.id))
		.filter((c) => c.app === app.app)
		.map((c) => ({
			id: c.id,
			label: c.label,
			here: c.installation === app.installation,
			/** Another installation holds it now. */
			held: c.installation !== null && c.installation !== app.installation
		}))
		.sort((a, b) => Number(b.here) - Number(a.here));
	return {
		login: acct.login,
		csrf,
		app: { name: app.appName, label: app.label },
		choices
	};
};

export const actions: Actions = {
	/** The person lets the app connect: a code for the app. */
	approve: async ({ url, cookies, request: req }) => {
		const config = siteConfig(url);
		const app = await request(config, url);
		const { acct, form } = await signedInForm(config, cookies, url, req);
		try {
			const existing = await installationClient(config.db, acct.id, app.app, app.installation);
			// Which client to connect as: this installation's (the default), another
			// of the person's connections to this app, or a new one.
			const chosen = String(form.get('client') ?? existing?.id ?? 'new');
			let client: string;
			let rotateToken: boolean;
			if (chosen === 'new') {
				if (existing) await unbindInstallation(config.db, acct.id, app.app, app.installation);
				client = await createClient(config.db, acct, {
					app: app.app,
					installation: app.installation,
					label: app.label
				});
				rotateToken = false;
			} else if (chosen === existing?.id) {
				client = existing.id;
				// Signing in again from the same installation rotates its token.
				rotateToken = true;
			} else {
				const other = (await clientsOf(config.db, acct.id)).find(
					(c) => c.id === chosen && c.app === app.app
				);
				if (!other) error(404, 'No such connection.');
				await bindClient(config.db, acct.id, other.id, app.app, app.installation);
				client = other.id;
				// Whatever held it before stops: its token is replaced.
				rotateToken = true;
			}
			const code = randomToken(32);
			await saveCode(config.db, await sha256(code), {
				client_id: client,
				account_id: acct.id,
				app: app.app,
				redirect_uri: app.redirectUri,
				code_challenge: app.codeChallenge,
				rotate: rotateToken ? 1 : 0
			});
			await audit(config.db, acct.id, 'app-approved', `${client} ${app.app}`, 'person');
			return {
				done: { appName: app.appName, returnTo: redirectWithCode(app.redirectUri, code, app.state) }
			};
		} catch (e) {
			if (e instanceof LimitReached) return fail(409, { limit: acct.client_limit });
			throw e;
		}
	}
};
