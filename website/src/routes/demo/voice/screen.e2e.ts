import { expect, test, type Page } from '@playwright/test';

// Sharing a screen, and the data saver. Chromium's fake microphone and
// camera; the screen is a canvas (a page of text and a moving bar), given
// in place of the browser's own picker.
test.use({
	launchOptions: {
		args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream']
	},
	permissions: ['microphone', 'camera']
});

/** `getDisplayMedia` gives a 1920 × 1080 canvas; `window.shared` is its track. */
async function fakeScreen(page: Page) {
	await page.addInitScript(() => {
		navigator.mediaDevices.getDisplayMedia = async () => {
			const canvas = document.createElement('canvas');
			[canvas.width, canvas.height] = [1920, 1080];
			const ctx = canvas.getContext('2d')!;
			let n = 0;
			const paint = () => {
				ctx.fillStyle = '#f4f4f4';
				ctx.fillRect(0, 0, 1920, 1080);
				ctx.fillStyle = '#1d4ed8';
				ctx.fillRect(0, 0, 1920, 120);
				ctx.fillStyle = '#222';
				ctx.font = '28px sans-serif';
				for (let y = 200; y < 980; y += 40) ctx.fillText(`A shared screen, line ${y}`, 40, y);
				ctx.fillStyle = '#e00000';
				ctx.fillRect((n++ * 17) % 1800, 1000, 120, 40);
			};
			paint();
			setInterval(paint, 66);
			const stream = canvas.captureStream(15);
			(window as unknown as { shared: MediaStreamTrack }).shared = stream.getVideoTracks()[0];
			return stream;
		};
	});
}

/** The browser's own "Stop sharing": the track ends (`stop()` alone fires no event). */
const stopFromBrowser = (page: Page) =>
	page.evaluate(() => {
		const track = (window as unknown as { shared: MediaStreamTrack }).shared;
		track.stop();
		track.dispatchEvent(new Event('ended'));
	});

/**
 * Opens a new room: on the relay `PEERCONNECT_DEMO_RELAY` names (a local
 * `quic://127.0.0.1:<port>` one, say), else on the public one. Returns its link.
 */
async function newRoom(page: Page): Promise<string> {
	const relay = process.env.PEERCONNECT_DEMO_RELAY;
	if (relay) {
		const { Group } = await import('peerconnect');
		await page.goto(`/demo/voice#${encodeURIComponent(Group.create(relay).toString())}`);
	} else {
		await page.goto('/demo/voice');
		await page.getByRole('button', { name: 'Create a room' }).click();
		await expect(page).toHaveURL(/#[\w-]{43}$/);
	}
	return page.url();
}

async function join(page: Page, name: string) {
	await page.getByLabel('Your name').fill(name);
	await page.getByRole('button', { name: 'Join', exact: true }).click();
}

test('a screen is shared, and stops with the browser’s own button', async ({ page }) => {
	await fakeScreen(page);
	await newRoom(page);
	await join(page, 'Alice');
	const share = page.getByRole('button', { name: 'Share your screen' });
	await share.click();
	const sharing = page.getByRole('status').filter({ hasText: 'You are sharing your screen' });
	await expect(sharing).toBeVisible();
	await expect(page.getByRole('button', { name: 'Stop sharing your screen' })).toBeVisible();
	// Our own screen is not shown back to us: the indicator says it instead.
	await expect(page.locator('.screen-tile')).toHaveCount(0);
	// Its own stop button.
	await sharing.getByRole('button', { name: 'Stop' }).click();
	await expect(sharing).toHaveCount(0);
	await expect(share).toBeVisible();
	// Again, then stopped from the browser: we stop with it.
	await share.click();
	await expect(sharing).toBeVisible();
	await stopFromBrowser(page);
	await expect(sharing).toHaveCount(0);
	await expect(share).toBeVisible();
});

test('the data saver is a switch, and is remembered', async ({ page }) => {
	await newRoom(page);
	await join(page, 'Alice');
	const saver = page.getByRole('button', { name: 'Data saver: receive video at 360p at most' });
	await expect(saver).toHaveAttribute('aria-pressed', 'false');
	await saver.click();
	await expect(saver).toHaveAttribute('aria-pressed', 'true');
	// Another visit, in Chinese: still on.
	await page.goto('/zh/demo/voice');
	await page.getByRole('button', { name: '创建房间' }).click();
	await page.getByLabel('你的名字').fill('小明');
	await page.getByRole('button', { name: '加入', exact: true }).click();
	await expect(page.getByRole('button', { name: '省流量：接收的视频最高 360p' })).toHaveAttribute(
		'aria-pressed',
		'true'
	);
});

test('no screen sharing on a phone', async ({ browser }) => {
	const phone = await browser.newContext({
		userAgent:
			'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Mobile Safari/537.36',
		viewport: { width: 390, height: 844 },
		permissions: ['microphone', 'camera']
	});
	const page = await phone.newPage();
	await newRoom(page);
	await join(page, 'Alice');
	await expect(
		page.getByRole('button', { name: 'Turn your camera on: everyone sees you' })
	).toBeVisible();
	await expect(page.getByRole('button', { name: 'Share your screen' })).toHaveCount(0);
	await phone.close();
});

// Two browsers through a relay: the public one, or PEERCONNECT_DEMO_RELAY's.
test('a shared screen is shown large, at 1080p across the whole window', async ({ browser }) => {
	test.skip(
		!process.env.PEERCONNECT_DEMO_E2E,
		'set PEERCONNECT_DEMO_E2E=1 to use the public relay (or PEERCONNECT_DEMO_RELAY too, for another)'
	);
	test.setTimeout(120_000);
	const open = async () =>
		(
			await browser.newContext({
				permissions: ['microphone', 'camera'],
				viewport: { width: 1920, height: 1080 }
			})
		).newPage();
	const [a, b] = [await open(), await open()];
	await fakeScreen(a);
	const link = await newRoom(a);
	await join(a, 'Alice');
	await b.goto(link);
	await join(b, 'Bob');
	for (const page of [a, b])
		await expect(page.locator('.person')).toHaveCount(2, { timeout: 60_000 });
	// Bob's camera on, so that the screen has someone to be shown above.
	await b.getByRole('button', { name: 'Turn your camera on: everyone sees you' }).click();

	await a.getByRole('button', { name: 'Share your screen' }).click();
	// Bob: the speaker view, Alice's screen large, with its own label.
	const large = b.locator('.large-one .screen-tile');
	await expect(large).toBeVisible({ timeout: 30_000 });
	await expect(large).toContainText('Alice’s screen');
	await expect(b.getByRole('button', { name: 'Speaker' })).toHaveAttribute('aria-pressed', 'true');
	const canvas = large.locator('canvas.screen');
	const size = () => canvas.evaluate((c: HTMLCanvasElement) => [c.width, c.height]);
	// Large: by its size (some 600 rows here), 720p.
	await expect.poll(size, { timeout: 30_000 }).toEqual([1280, 720]);
	// Across the whole window (1080 rows): 1080p.
	await large.getByRole('button', { name: 'Watch Alice’s screen on the whole screen' }).click();
	const whole = b.locator('.full canvas.screen');
	await expect
		.poll(() => whole.evaluate((c: HTMLCanvasElement) => [c.width, c.height]), {
			timeout: 30_000
		})
		.toEqual([1920, 1080]);
	await b.getByRole('button', { name: 'Leave the whole screen' }).click();

	// Data saver on: 360p at most.
	await b.getByRole('button', { name: 'Data saver: receive video at 360p at most' }).click();
	await expect.poll(size, { timeout: 30_000 }).toEqual([640, 360]);
	await b.getByRole('button', { name: 'Data saver: receive video at 360p at most' }).click();

	// Alice stops from her browser: the screen goes from Bob's page, and the gallery is back.
	await stopFromBrowser(a);
	await expect(b.locator('.screen-tile')).toHaveCount(0, { timeout: 30_000 });
	await expect(b.getByRole('button', { name: 'Gallery' })).toHaveAttribute('aria-pressed', 'true');
});
