import type { Handle } from '@sveltejs/kit/hooks';
import { LOCALE, langOf } from '#lib/i18n/index.ts';

export const handle: Handle = ({ event, resolve }) => {
	const tag = LOCALE[langOf(event.url.pathname)].tag;
	return resolve(event, {
		transformPageChunk: ({ html }) => html.replace('%lang%', tag)
	});
};
