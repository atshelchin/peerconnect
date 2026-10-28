import { error } from '@sveltejs/kit';
import { siteConfig } from '#lib/server/config.ts';
import {
	app as appOf,
	client as clientOf,
	clients,
	deleteClient,
	isAdmin
} from '#lib/server/db.ts';
import { signedIn, signedInForm } from '#lib/server/guard.ts';
import { RelayError, remove, usage, type UsageDay } from '#lib/server/relay.ts';
import type { Actions, PageServerLoad } from './$types';

export const prerender = false;

const today = () => new Date().toISOString().slice(0, 10);

function summary(days: UsageDay[]) {
	const d = days.find((day) => day.day === today());
	return {
		connections: d?.connections ?? 0,
		minutes: Math.round((d?.connected_ms ?? 0) / 60_000),
		bytes: (d?.bytes_in ?? 0) + (d?.bytes_out ?? 0)
	};
}

export const load: PageServerLoad = async ({ url, cookies }) => {
	const config = siteConfig(url);
	const { acct, csrf } = await signedIn(config, cookies, url);
	const rows = await clients(config.db, acct.id);
	const names = new Map<string, string>();
	for (const row of rows) {
		if (row.app && !names.has(row.app))
			names.set(row.app, (await appOf(config.db, row.app))?.name ?? row.app);
	}
	return {
		login: acct.login,
		avatar: acct.avatar_url,
		suspended: acct.state !== 'active',
		limit: acct.client_limit,
		csrf,
		admin: await isAdmin(config.db, acct.id, config.adminGithubId),
		clients: await Promise.all(
			rows.map(async (row) => ({
				id: row.id,
				label: row.label,
				app: row.app ? (names.get(row.app) ?? row.app) : null,
				manual: row.installation === null,
				created: new Date(row.created_at * 1000).toISOString().slice(0, 10),
				today: await usage(config.relay, row.id).then(summary, () => null)
			}))
		)
	};
};

export const actions: Actions = {
	/** The connection is gone: its token stops, at once, for good. */
	delete: async ({ url, cookies, request }) => {
		const config = siteConfig(url);
		const { acct, form } = await signedInForm(config, cookies, url, request);
		const row = await clientOf(config.db, acct.id, String(form.get('client') ?? ''));
		if (!row || row.external_user !== null) error(404, 'No such connection.');
		try {
			await remove(config.relay, row.id, `account:${acct.id}`);
		} catch (e) {
			if (e instanceof RelayError) error(502, 'The relay could not do that. Please try again.');
			throw e;
		}
		await deleteClient(config.db, row.id, 'person', acct.id);
		return { deleted: row.label };
	}
};
