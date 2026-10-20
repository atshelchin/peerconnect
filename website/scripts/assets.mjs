// Renders the site's raster images (icons and Open Graph cards) into static/.
// Run after changing the logo or the headline: `bun scripts/assets.mjs`.

import { mkdir, writeFile } from 'node:fs/promises';
import { chromium } from 'playwright';

const out = new URL('../static/', import.meta.url);
const ACCENT = '#0a7a5e';

const logo = (size, { radius = 9, background = ACCENT } = {}) => `
<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 32 32">
  <rect width="32" height="32" rx="${radius}" fill="${background}"/>
  <circle cx="9.5" cy="16" r="4.25" fill="#fff"/>
  <circle cx="22.5" cy="16" r="4.25" fill="#fff"/>
  <circle cx="16" cy="16" r="1.4" fill="#fff" fill-opacity="0.75"/>
</svg>`;

const card = (lang) => {
	const zh = lang === 'zh';
	const title = zh
		? '两个应用，一个房间。<br>中继什么也读不到。'
		: 'Two apps. One room.<br>Nothing for the relay to read.';
	const sub = zh
		? '端到端加密 · 无状态中继 · 可靠送达 · Rust 与 JavaScript'
		: 'End-to-end encrypted · stateless relay · reliable delivery';
	return `<!doctype html><html lang="${zh ? 'zh-CN' : 'en'}"><head><meta charset="utf-8"><style>
  * { margin: 0; box-sizing: border-box; }
  body { width: 1200px; height: 630px; padding: 72px 80px; display: flex; flex-direction: column;
    justify-content: space-between; background: #0b0e12; color: #eaeef2; overflow: hidden;
    font-family: -apple-system, 'SF Pro Display', 'PingFang SC', 'Helvetica Neue', sans-serif; position: relative; }
  body::before { content: ''; position: absolute; inset: 0;
    background-image: radial-gradient(#26303a 1.5px, transparent 1.5px); background-size: 28px 28px;
    mask-image: radial-gradient(ellipse 80% 90% at 85% 10%, #000 20%, transparent 70%); }
  .brand { display: flex; align-items: center; gap: 18px; font-size: 34px; font-weight: 700; position: relative; }
  h1 { font-size: ${zh ? 74 : 72}px; line-height: ${zh ? 1.25 : 1.06}; letter-spacing: ${zh ? '0' : '-0.035em'}; font-weight: 750; position: relative; }
  .foot { display: flex; justify-content: space-between; align-items: center; gap: 40px; font-size: 26px; color: #929ca7; position: relative; }
  .domain { color: #3ccf9f; font-family: ui-monospace, Menlo, monospace; font-size: 26px; }
  .cipher { position: absolute; right: 80px; top: 86px; font-family: ui-monospace, Menlo, monospace; font-size: 22px;
    color: #f0b44c; opacity: .85; padding: 8px 16px; border-radius: 10px; background: #2a2010; }
</style></head><body>
  <div class="brand">${logo(64)}PeerConnect</div>
  <div class="cipher">9f2c e107 3ab4 77d0 …</div>
  <h1>${title}</h1>
  <div class="foot"><span>${sub}</span><span class="domain">peerconnect.dev</span></div>
</body></html>`;
};

/** A minimal .ico holding one PNG image. */
function ico(png) {
	const header = Buffer.alloc(22);
	header.writeUInt16LE(0, 0); // reserved
	header.writeUInt16LE(1, 2); // type: icon
	header.writeUInt16LE(1, 4); // one image
	header.writeUInt8(32, 6); // width
	header.writeUInt8(32, 7); // height
	header.writeUInt16LE(1, 10); // colour planes
	header.writeUInt16LE(32, 12); // bits per pixel
	header.writeUInt32LE(png.length, 14);
	header.writeUInt32LE(22, 18); // offset of the image
	return Buffer.concat([header, png]);
}

const browser = await chromium.launch();
const page = await browser.newPage();

async function render(html, width, height) {
	await page.setViewportSize({ width, height });
	await page.setContent(html);
	return page.screenshot({ omitBackground: true });
}

const svgPage = (svg, size) =>
	`<html><body style="margin:0;width:${size}px;height:${size}px">${svg}</body></html>`;

await mkdir(new URL('og/', out), { recursive: true });
await writeFile(new URL('favicon.svg', out), logo(32).trim() + '\n');
await writeFile(new URL('favicon.ico', out), ico(await render(svgPage(logo(32), 32), 32, 32)));
// iOS rounds the corners itself, and wants no transparency.
await writeFile(
	new URL('apple-touch-icon.png', out),
	await render(svgPage(logo(180, { radius: 0 }), 180), 180, 180)
);
for (const size of [192, 512]) {
	await writeFile(
		new URL(`icon-${size}.png`, out),
		await render(svgPage(logo(size), size), size, size)
	);
}
// Maskable: full-bleed background, content within the safe zone.
await writeFile(
	new URL('icon-maskable-512.png', out),
	await render(
		`<html><body style="margin:0;width:512px;height:512px;background:${ACCENT};display:grid;place-items:center">${logo(340, { radius: 0 })}</body></html>`,
		512,
		512
	)
);
for (const lang of ['en', 'zh']) {
	await page.setViewportSize({ width: 1200, height: 630 });
	await page.setContent(card(lang));
	await writeFile(new URL(`og/${lang}.png`, out), await page.screenshot({ type: 'png' }));
}
await browser.close();
console.log('wrote icons and Open Graph images to static/');
