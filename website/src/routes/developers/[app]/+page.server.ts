import { error, fail, redirect } from '@sveltejs/kit';
import { langOf, localize } from '#lib/i18n/index.ts';
import { appFields } from '#lib/server/apps.ts';
import { siteConfig, type SiteConfig } from '#lib/server/config.ts';
import {
	LimitReached,
	appClients,
	appCounts,
	createClient,
	deleteApp,
	deleteClient,
	isAdmin,
	originsOf,
	ownApp,
	rotateAppSecret,
	updateApp
} from '#lib/server/db.ts';
import { signedIn, signedInForm } from '#lib/server/guard.ts';
import { RelayError, mint, remove, usageDays } from '#lib/server/relay.ts';
import type { Actions, PageServerLoad } from './$types';

export const prerender = false;

/** Clients deleted per request when an app is deleted: within a Worker's subrequests. */
const DELETE_BATCH = 200;

export const load: PageServerLoad = async ({ url, cookies, params }) => {
	const config = siteConfig(url);
	const { acct, csrf } = await signedIn(config, cookies, url);
	const app = await ownApp(config.db, acct.id, params.app);
	if (!app) error(404, 'No such application.');
	const origins = await originsOf(config.db, app.id);
	return {
		login: acct.login,
		avatar: acct.avatar_url,
		csrf,
		admin: await isAdmin(config.db, acct.id, config.adminGithubId),
		relayUrl: config.relayPublicUrl,
		app: {
			id: app.id,
			name: app.name,
			state: app.state,
			userLimit: app.user_limit,
			created: new Date(app.created_at * 1000).toISOString().slice(0, 10)
		},
		values: {
			name: app.name,
			redirect_uris: app.redirect_uris.join('\n'),
			origins: origins.join('\n'),
			loopback_path: app.loopback_path ?? ''
		},
		redirectUri: app.redirect_uris[0] ?? 'https://example.com/peerconnect',
		counts: await appCounts(config.db, app.id),
		usage: await appUsage(config, app.id).catch(() => null)
	};
};

/** The app's finished days of the last 30, all its clients together. */
async function appUsage(config: SiteConfig, app: string) {
	const ids = new Set((await appClients(config.db, app, 10_000)).map((c) => c.id));
	const rows = (await usageDays(config.relay, 30)).filter((r) => ids.has(r.client));
	const sum = (key: 'connections' | 'connected_ms' | 'bytes_in' | 'bytes_out') =>
		rows.reduce((n, r) => n + (r[key] ?? 0), 0);
	return {
		connections: sum('connections'),
		minutes: Math.round(sum('connected_ms') / 60_000),
		bytes: sum('bytes_in') + sum('bytes_out')
	};
}

async function owned(
	config: SiteConfig,
	cookies: import('@sveltejs/kit').Cookies,
	url: URL,
	request: Request,
	id: string
) {
	const { acct, form } = await signedInForm(config, cookies, url, request);
	const app = await ownApp(config.db, acct.id, id);
	if (!app) error(404, 'No such application.');
	return { acct, form, app };
}

function relayFailed(e: unknown): never {
	if (e instanceof RelayError) error(502, 'The relay could not do that. Please try again.');
	throw e;
}

export const actions: Actions = {
	save: async ({ url, cookies, request, params }) => {
		const config = siteConfig(url);
		const { acct, form, app } = await owned(config, cookies, url, request, params.app);
		const fields = appFields(form);
		if ('problems' in fields) return fail(400, { problems: fields.problems });
		await updateApp(config.db, acct.id, app.id, fields.ok);
		return { saved: true };
	},

	/** The app secret moves on: the old one stops at once. */
	rotate: async ({ url, cookies, request, params }) => {
		const config = siteConfig(url);
		const { acct, app } = await owned(config, cookies, url, request, params.app);
		await rotateAppSecret(config.db, acct.id, app.id);
		return { rotated: true };
	},

	/** A client of the developer's own for this app, and its token, to try the relay. */
	test: async ({ url, cookies, request, params }) => {
		const config = siteConfig(url);
		const { acct, app } = await owned(config, cookies, url, request, params.app);
		try {
			const client = await createClient(config.db, acct, {
				app: app.id,
				installation: null,
				label: `Test token · ${app.name}`
			});
			const minted = await mint(config.relay, client, app.id, `account:${acct.id}`).catch(
				relayFailed
			);
			return { test: { client, token: minted.token, relayUrl: config.relayPublicUrl } };
		} catch (e) {
			if (e instanceof LimitReached) return fail(409, { clientLimit: acct.client_limit });
			throw e;
		}
	},

	/**
	 * Every client of the app is deleted, a batch per request, then the app.
	 * Its tokens stop as their clients go.
	 */
	delete: async ({ url, cookies, request, params }) => {
		const config = siteConfig(url);
		const { acct, app } = await owned(config, cookies, url, request, params.app);
		const batch = await appClients(config.db, app.id, DELETE_BATCH);
		for (const client of batch) {
			await remove(config.relay, client.id, `account:${acct.id}`).catch(relayFailed);
			await deleteClient(config.db, client.id, 'developer', acct.id);
		}
		const left = (await appCounts(config.db, app.id)).clients;
		if (left > 0) return fail(409, { more: left });
		await deleteApp(config.db, acct.id, app.id);
		// Not back to this page, whose app is gone.
		redirect(303, localize('/developers', langOf(url.pathname)));
	}
};
