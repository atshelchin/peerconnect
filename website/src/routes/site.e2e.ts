import { expect, test, type Page } from '@playwright/test';

const SITE = 'https://peerconnect.dev';
const PAGES = [
	{ path: '/', zh: '/zh' },
	{ path: '/docs', zh: '/zh/docs' },
	{ path: '/demo', zh: '/zh/demo' }
];

async function head(page: Page) {
	return page.evaluate(() => ({
		lang: document.documentElement.lang,
		title: document.title,
		description: document.querySelector('meta[name=description]')?.getAttribute('content') ?? '',
		canonical: document.querySelector('link[rel=canonical]')?.getAttribute('href'),
		alternates: Object.fromEntries(
			[...document.querySelectorAll('link[rel=alternate][hreflang]')].map((l) => [
				l.getAttribute('hreflang'),
				l.getAttribute('href')
			])
		),
		image: document.querySelector('meta[property="og:image"]')?.getAttribute('content'),
		h1: document.querySelectorAll('h1').length,
		jsonLd: [...document.querySelectorAll('script[type="application/ld+json"]')].map(
			(s) => s.textContent ?? ''
		)
	}));
}

for (const { path, zh } of PAGES) {
	for (const [lang, url, tag] of [
		['en', path, 'en'],
		['zh', zh, 'zh-CN']
	] as const) {
		test(`${url} is complete for search engines`, async ({ page, request }) => {
			const response = await page.goto(url);
			expect(response?.status()).toBe(200);
			const h = await head(page);
			expect(h.lang).toBe(tag);
			expect(h.h1).toBe(1);
			expect(h.title.length).toBeGreaterThan(10);
			expect(h.description.length).toBeGreaterThan(30);
			expect(h.canonical).toBe(SITE + (url === '/' ? '/' : url));
			// Every language version lists all of them, itself included.
			expect(h.alternates).toEqual({
				en: SITE + (path === '/' ? '/' : path),
				'zh-CN': SITE + zh,
				'x-default': SITE + (path === '/' ? '/' : path)
			});
			expect(h.image).toBe(`${SITE}/og/${lang}.png`);
			expect((await request.get(`/og/${lang}.png`)).status()).toBe(200);
			for (const json of h.jsonLd) expect(() => JSON.parse(json)).not.toThrow();
		});
	}
}

test('the specification is English only, with no alternates', async ({ page }) => {
	await page.goto('/spec');
	const h = await head(page);
	expect(h.lang).toBe('en');
	expect(h.h1).toBe(1);
	expect(h.alternates).toEqual({});
	await expect(page.locator('#sec-5-3')).toBeVisible();
});

test('the sitemap lists every page, and each one exists', async ({ request }) => {
	const xml = await (await request.get('/sitemap.xml')).text();
	const locs = [...xml.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
	expect(locs.sort()).toEqual(
		['/', '/zh', '/docs', '/zh/docs', '/demo', '/zh/demo', '/spec'].map((p) => SITE + p).sort()
	);
	for (const loc of locs) expect((await request.get(loc.replace(SITE, ''))).status()).toBe(200);
});

test('robots.txt points at the sitemap', async ({ request }) => {
	expect(await (await request.get('/robots.txt')).text()).toContain(`Sitemap: ${SITE}/sitemap.xml`);
});

test('the language switch leads to the same page in the other language', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	await page.goto('/docs');
	await page.locator('header nav.desktop a.lang').click();
	await expect(page).toHaveURL(/\/zh\/docs$/);
	await expect(page.locator('h1')).toHaveText('快速上手');
	await page.locator('header nav.desktop a.lang').click();
	await expect(page).toHaveURL(/\/docs$/);
});

test('a missing page is a localized 404, kept out of the index', async ({ page }) => {
	const response = await page.goto('/zh/missing');
	expect(response?.status()).toBe(404);
	await expect(page.locator('h1')).toHaveText('页面不存在');
	await expect(page.locator('meta[name=robots]')).toHaveAttribute('content', 'noindex');
});

test('only the demo loads the WebAssembly SDK', async ({ page }) => {
	const scripts: number[] = [];
	page.on('response', async (r) => {
		if (r.request().resourceType() === 'script') scripts.push((await r.body()).length);
	});
	await page.goto('/', { waitUntil: 'networkidle' });
	expect(Math.max(...scripts)).toBeLessThan(100_000);
	scripts.length = 0;
	await page.goto('/demo', { waitUntil: 'networkidle' });
	await expect(page.getByRole('button', { name: 'Create a room' })).toBeVisible();
	expect(Math.max(...scripts)).toBeGreaterThan(150_000);
});
