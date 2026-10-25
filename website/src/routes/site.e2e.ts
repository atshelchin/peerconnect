import { expect, test, type Page } from '@playwright/test';

const SITE = 'https://peerconnect.dev';
const PAGES = [
	{ path: '/', zh: '/zh' },
	{ path: '/docs', zh: '/zh/docs' },
	{ path: '/groups', zh: '/zh/groups' },
	{ path: '/demo', zh: '/zh/demo' }
];
const ENGLISH_ONLY = ['/spec', '/spec/groups'];

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

for (const [path, section] of [
	['/spec', '#sec-5-3'],
	['/spec/groups', '#sec-6']
] as const) {
	test(`${path} is English only, with no alternates`, async ({ page }) => {
		const response = await page.goto(path);
		expect(response?.status()).toBe(200);
		const h = await head(page);
		expect(h.lang).toBe('en');
		expect(h.h1).toBe(1);
		expect(h.title.length).toBeGreaterThan(10);
		expect(h.description.length).toBeGreaterThan(30);
		expect(h.canonical).toBe(SITE + path);
		expect(h.alternates).toEqual({});
		for (const json of h.jsonLd) expect(() => JSON.parse(json)).not.toThrow();
		await expect(page.locator(section)).toBeVisible();
	});
}

test('the groups specification links into the room specification', async ({ page }) => {
	await page.goto('/spec/groups');
	await expect(page.locator('h1')).toHaveText('PeerConnect Groups, version 1');
	await page.locator('a[href="/spec#sec-4-1"]').first().click();
	await expect(page).toHaveURL(/\/spec#sec-4-1$/);
	await expect(page.locator('#sec-4-1')).toBeVisible();
});

test('the sitemap lists every page, and each one exists', async ({ request }) => {
	const xml = await (await request.get('/sitemap.xml')).text();
	const locs = [...xml.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]);
	expect(locs.sort()).toEqual(
		[
			'/',
			'/zh',
			'/docs',
			'/zh/docs',
			'/groups',
			'/zh/groups',
			'/demo',
			'/zh/demo',
			'/spec',
			'/spec/groups'
		]
			.map((p) => SITE + p)
			.sort()
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

test('the header leads to the groups guide, in the reader’s language', async ({ page }) => {
	await page.setViewportSize({ width: 1280, height: 800 });
	await page.goto('/');
	await page.locator('header nav.desktop').getByRole('link', { name: 'Groups' }).click();
	await expect(page).toHaveURL(/\/groups$/);
	await expect(page.locator('h1')).toHaveText('Groups');
	await expect(page.locator('header nav.desktop a[aria-current=page]')).toHaveText('Groups');
	await page.locator('header nav.desktop a.lang').click();
	await expect(page).toHaveURL(/\/zh\/groups$/);
	await expect(page.locator('h1')).toHaveText('群组');
	await page.locator('header nav.desktop').getByRole('link', { name: '规范' }).click();
	await expect(page).toHaveURL(/\/spec$/);
});

test('the mobile menu lists the groups guide', async ({ page }) => {
	await page.setViewportSize({ width: 390, height: 800 });
	await page.goto('/zh');
	await page.locator('header details.mobile summary').click();
	await page.locator('header details.mobile').getByRole('link', { name: '群组' }).click();
	await expect(page).toHaveURL(/\/zh\/groups$/);
	await expect(page.locator('html')).toHaveAttribute('lang', 'zh-CN');
});

test('the home page and the guide link to the groups pages', async ({ page }) => {
	await page.goto('/');
	await page.locator('#groups').getByRole('link', { name: 'Read the groups guide' }).click();
	await expect(page).toHaveURL(/\/groups$/);
	await page.goto('/zh/docs');
	await page.getByRole('link', { name: '阅读群组指南 →' }).click();
	await expect(page).toHaveURL(/\/zh\/groups$/);
	await page.goto('/groups');
	await page.locator('.next a[href="/spec/groups"]').click();
	await expect(page.locator('h1')).toHaveText('PeerConnect Groups, version 1');
});

test('no page links to a page or a section that does not exist', async ({ request }) => {
	const pages = [...PAGES.flatMap(({ path, zh }) => [path, zh]), ...ENGLISH_ONLY];
	const html = new Map<string, string>();
	const fetchPage = async (path: string) => {
		if (!html.has(path)) {
			const response = await request.get(path);
			expect(response.status(), path).toBe(200);
			html.set(path, await response.text());
		}
		return html.get(path) ?? '';
	};
	let checked = 0;
	for (const from of pages) {
		const body = await fetchPage(from);
		const hrefs = [...body.matchAll(/<a\s[^>]*?href="([^"]+)"/g)].map((m) => m[1]);
		for (const href of new Set(hrefs)) {
			if (/^[a-z]+:/i.test(href)) continue; // leaves the site
			const url = new URL(href.replace(/&amp;/g, '&'), `http://site${from}`);
			if (url.pathname.match(/\.\w+$/)) {
				expect((await request.get(url.pathname)).status(), `${from} → ${href}`).toBe(200);
				continue;
			}
			const target = await fetchPage(url.pathname);
			if (url.hash) {
				expect(target, `${from} → ${href}`).toContain(
					`id="${decodeURIComponent(url.hash.slice(1))}"`
				);
			}
			checked++;
		}
	}
	expect(checked).toBeGreaterThan(100);
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
