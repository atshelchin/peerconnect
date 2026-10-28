import { fail, redirect } from '@sveltejs/kit';
import { appFields } from '#lib/server/apps.ts';
import { siteConfig } from '#lib/server/config.ts';
import { LimitReached, appsOf, appCounts, createApp, isAdmin } from '#lib/server/db.ts';
import { signedIn, signedInForm } from '#lib/server/guard.ts';
import { localize, langOf } from '#lib/i18n/index.ts';
import type { Actions, PageServerLoad } from './$types';

export const prerender = false;

export const load: PageServerLoad = async ({ url, cookies }) => {
	const config = siteConfig(url);
	const { acct, csrf } = await signedIn(config, cookies, url);
	const apps = await appsOf(config.db, acct.id);
	return {
		login: acct.login,
		avatar: acct.avatar_url,
		csrf,
		limit: acct.app_limit,
		admin: await isAdmin(config.db, acct.id, config.adminGithubId),
		apps: await Promise.all(
			apps.map(async (a) => ({
				id: a.id,
				name: a.name,
				state: a.state,
				created: new Date(a.created_at * 1000).toISOString().slice(0, 10),
				...(await appCounts(config.db, a.id))
			}))
		)
	};
};

export const actions: Actions = {
	create: async ({ url, cookies, request }) => {
		const config = siteConfig(url);
		const { acct, form } = await signedInForm(config, cookies, url, request);
		const fields = appFields(form);
		const values = {
			name: String(form.get('name') ?? ''),
			redirect_uris: String(form.get('redirect_uris') ?? ''),
			origins: String(form.get('origins') ?? ''),
			loopback_path: String(form.get('loopback_path') ?? '')
		};
		if ('problems' in fields) return fail(400, { problems: fields.problems, values });
		let id: string;
		try {
			id = await createApp(config.db, acct, fields.ok);
		} catch (e) {
			if (e instanceof LimitReached) return fail(409, { limit: acct.app_limit, values });
			throw e;
		}
		redirect(303, localize(`/developers/${id}`, langOf(url.pathname)));
	}
};
