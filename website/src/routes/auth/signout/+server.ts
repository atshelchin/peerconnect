import { error, redirect } from '@sveltejs/kit';
import { siteConfig } from '#lib/server/config.ts';
import { csrfValid, endSession, readSession } from '#lib/server/session.ts';
import type { RequestHandler } from './$types';

export const prerender = false;

export const POST: RequestHandler = async ({ url, cookies, request }) => {
	const config = siteConfig(url);
	const session = await readSession(cookies, config.sessionKey);
	if (
		session &&
		!(await csrfValid(config.sessionKey, session, (await request.formData()).get('csrf')))
	) {
		error(403, 'This form has expired. Please reload the page.');
	}
	endSession(cookies, config.secureCookies);
	redirect(303, '/');
};
