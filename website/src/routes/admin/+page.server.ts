import { error, fail } from '@sveltejs/kit';
import { siteConfig, type SiteConfig } from '#lib/server/config.ts';
import { estimate } from '#lib/server/cost.ts';
import {
	accountClients,
	adminAccounts,
	adminApps,
	appClients,
	clientOwners,
	isAdmin,
	recentAudit,
	setAccountLimits,
	setAccountState,
	setAppState
} from '#lib/server/db.ts';
import { signedIn, signedInForm } from '#lib/server/guard.ts';
import { RelayError, issuanceAudit, revoke, usageDays } from '#lib/server/relay.ts';
import type { Actions, PageServerLoad } from './$types';

export const prerender = false;

/**
 * Tokens revoked per request when an account is suspended or an app
 * disabled: within a Worker's subrequests. Beyond it, the page offers to go
 * on from where it stopped.
 */
const REVOKE_BATCH = 900;

/** The owner, or a 404 for anyone else: nothing says this page exists. */
async function owner(config: SiteConfig, cookies: import('@sveltejs/kit').Cookies, url: URL) {
	const who = await signedIn(config, cookies, url);
	if (!(await isAdmin(config.db, who.acct.id, config.adminGithubId))) error(404, 'Not found');
	return who;
}

export const load: PageServerLoad = async ({ url, cookies }) => {
	const config = siteConfig(url);
	const { acct, csrf } = await owner(config, cookies, url);
	const [accounts, apps, owners, audit, issuance, days] = await Promise.all([
		adminAccounts(config.db),
		adminApps(config.db),
		clientOwners(config.db),
		recentAudit(config.db, 100),
		issuanceAudit(config.relay, 100).catch(() => []),
		usageDays(config.relay, 30).catch(() => [])
	]);
	// Finished days of the last 30, per account that answers for the client.
	const perAccount = new Map<
		string,
		{ connections: number; minutes: number; bytes: number; cost: number }
	>();
	const perDay = new Map<string, { connections: number; bytes: number; cost: number }>();
	for (const row of days) {
		const cost = estimate(row);
		const account = owners.get(row.client) ?? (row.client.startsWith('t-') ? 'trial' : 'deleted');
		const a = perAccount.get(account) ?? { connections: 0, minutes: 0, bytes: 0, cost: 0 };
		a.connections += row.connections;
		a.minutes += row.connected_ms / 60_000;
		a.bytes += row.bytes_in + row.bytes_out;
		a.cost += cost;
		perAccount.set(account, a);
		const d = perDay.get(row.day) ?? { connections: 0, bytes: 0, cost: 0 };
		d.connections += row.connections;
		d.bytes += row.bytes_in + row.bytes_out;
		d.cost += cost;
		perDay.set(row.day, d);
	}
	const logins = new Map(accounts.map((a) => [a.id, a.login]));
	return {
		login: acct.login,
		avatar: acct.avatar_url,
		csrf,
		accounts: accounts.map((a) => ({ ...a, usage: perAccount.get(a.id) ?? null })),
		apps,
		top: [...perAccount.entries()]
			.map(([id, u]) => ({ id, login: logins.get(id) ?? id, ...u }))
			.sort((a, b) => b.cost - a.cost)
			.slice(0, 20),
		days: [...perDay.entries()]
			.map(([day, u]) => ({ day, ...u }))
			.sort((a, b) => b.day.localeCompare(a.day)),
		audit,
		issuance
	};
};

async function checked(
	config: SiteConfig,
	cookies: import('@sveltejs/kit').Cookies,
	url: URL,
	request: Request
) {
	const { acct, form } = await signedInForm(config, cookies, url, request);
	if (!(await isAdmin(config.db, acct.id, config.adminGithubId))) error(404, 'Not found');
	return { actor: `admin:${acct.id}`, form };
}

/** Revokes up to a batch of `clients`' tokens; how many are left is the caller's to say. */
async function revokeAll(config: SiteConfig, clients: { id: string }[], actor: string) {
	for (const client of clients) {
		try {
			await revoke(config.relay, client.id, actor);
		} catch (e) {
			// A client deleted on the relay already accepts nothing.
			if (!(e instanceof RelayError && e.status === 410)) throw e;
		}
	}
}

export const actions: Actions = {
	/** Suspending stops every token the account answers for, and blocks new ones. */
	account: async ({ url, cookies, request }) => {
		const config = siteConfig(url);
		const { actor, form } = await checked(config, cookies, url, request);
		const id = String(form.get('account') ?? '');
		const state = form.get('state');
		if (state === 'suspended' || state === 'active') {
			await setAccountState(config.db, id, state, actor);
			if (state === 'suspended') {
				const offset = Number(form.get('offset') ?? 0) || 0;
				const clients = await accountClients(config.db, id, REVOKE_BATCH, offset);
				await revokeAll(config, clients, actor);
				if (clients.length === REVOKE_BATCH)
					return fail(409, { more: { kind: 'account', id, offset: offset + REVOKE_BATCH } });
			}
		}
		const clientLimit = Number(form.get('client_limit'));
		const appLimit = Number(form.get('app_limit'));
		if (
			Number.isInteger(clientLimit) &&
			Number.isInteger(appLimit) &&
			clientLimit >= 0 &&
			appLimit >= 0
		) {
			await setAccountLimits(config.db, id, clientLimit, appLimit, actor);
		}
		return { done: id };
	},

	/** Disabling an app stops its users' tokens and its secret; enabling lets them get new ones. */
	app: async ({ url, cookies, request }) => {
		const config = siteConfig(url);
		const { actor, form } = await checked(config, cookies, url, request);
		const id = String(form.get('app') ?? '');
		const state = form.get('state');
		if (state !== 'active' && state !== 'disabled') error(400, 'Say active or disabled.');
		await setAppState(config.db, id, state, actor);
		if (state === 'disabled') {
			const offset = Number(form.get('offset') ?? 0) || 0;
			const clients = await appClients(config.db, id, REVOKE_BATCH, offset);
			await revokeAll(config, clients, actor);
			if (clients.length === REVOKE_BATCH)
				return fail(409, { more: { kind: 'app', id, offset: offset + REVOKE_BATCH } });
		}
		return { done: id };
	}
};
