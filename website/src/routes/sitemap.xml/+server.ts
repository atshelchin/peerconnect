import { LANGS, LOCALE, localize } from '#lib/i18n/index.ts';
import { SITE_URL } from '#lib/site.ts';

export const prerender = true;

const PAGES: { path: string; translated: boolean; priority: string }[] = [
	{ path: '/', translated: true, priority: '1.0' },
	{ path: '/docs', translated: true, priority: '0.9' },
	{ path: '/groups', translated: true, priority: '0.9' },
	{ path: '/demo', translated: true, priority: '0.8' },
	{ path: '/spec', translated: false, priority: '0.8' },
	{ path: '/spec/groups', translated: false, priority: '0.7' }
];

const url = (path: string) => SITE_URL + (path === '/' ? '/' : path);

export function GET() {
	const lastmod = new Date().toISOString().slice(0, 10);
	const entries = PAGES.flatMap(({ path, translated, priority }) => {
		const langs = translated ? LANGS : (['en'] as const);
		const alternates = translated
			? [
					...LANGS.map(
						(l) =>
							`<xhtml:link rel="alternate" hreflang="${LOCALE[l].tag}" href="${url(localize(path, l))}"/>`
					),
					`<xhtml:link rel="alternate" hreflang="x-default" href="${url(path)}"/>`
				].join('')
			: '';
		return langs.map(
			(lang) =>
				`<url><loc>${url(localize(path, lang))}</loc><lastmod>${lastmod}</lastmod><priority>${priority}</priority>${alternates}</url>`
		);
	});
	const xml = `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" xmlns:xhtml="http://www.w3.org/1999/xhtml">
${entries.join('\n')}
</urlset>
`;
	return new Response(xml, { headers: { 'Content-Type': 'application/xml; charset=utf-8' } });
}
