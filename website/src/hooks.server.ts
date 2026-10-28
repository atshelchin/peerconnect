import type { Handle } from '@sveltejs/kit/hooks';
import { LOCALE, langOf } from '#lib/i18n/index.ts';

/** The signed-in pages and the APIs: everything not prerendered. */
const PRIVATE = ['/account', '/admin', '/api', '/auth', '/connect', '/developers'];

export const handle: Handle = async ({ event, resolve }) => {
	const tag = LOCALE[langOf(event.url.pathname)].tag;
	const response = await resolve(event, {
		transformPageChunk: ({ html }) => html.replace('%lang%', tag)
	});
	// Private pages are never cached, never framed, and send a referrer only
	// within the site (a page may carry a token in its body; none is ever in
	// a URL). Not `no-referrer`: with it, browsers send a form's Origin as
	// `null`, and SvelteKit refuses the post as cross-site.
	const id = event.route.id ?? '';
	if (PRIVATE.some((p) => id === p || id.startsWith(`${p}/`))) {
		response.headers.set('cache-control', 'no-store');
		response.headers.set('referrer-policy', 'same-origin');
		response.headers.set('x-frame-options', 'DENY');
		response.headers.set('content-security-policy', "frame-ancestors 'none'");
	}
	return response;
};
