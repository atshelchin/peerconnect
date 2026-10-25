import { expect, test } from '@playwright/test';

// Chromium's fake microphone (a beep) and camera (a moving pattern), and no
// permission prompt.
test.use({
	launchOptions: {
		args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream']
	},
	permissions: ['microphone', 'camera']
});

test('the voice demo starts a room and gives a link to join it', async ({ page }) => {
	await page.goto('/demo/voice');
	await page.getByRole('button', { name: 'Create a room' }).click();
	// The room key is in the fragment, which never leaves the browser.
	await expect(page).toHaveURL(/\/demo\/voice#[\w-]{43}$/);
	await expect(page.getByLabel('Your name')).toBeVisible();
	await expect(page.getByRole('button', { name: 'Join with microphone' })).toBeDisabled();
	await page.getByLabel('Your name').fill('Alice');
	await expect(page.getByRole('button', { name: 'Join with microphone' })).toBeEnabled();
});

test('a damaged link says so, in Chinese too', async ({ page }) => {
	await page.goto('/zh/demo/voice#not-a-room');
	await expect(page.getByText('这不是一个有效的房间链接。')).toBeVisible();
	await expect(page.getByRole('button', { name: '创建房间' })).toBeVisible();
});

// Talks through the public relay, so it needs the network: run with
// PEERCONNECT_DEMO_E2E=1.
test('three browsers hear each other and chat', async ({ browser }) => {
	test.skip(
		!process.env.PEERCONNECT_DEMO_E2E,
		'set PEERCONNECT_DEMO_E2E=1 to use the public relay'
	);
	test.setTimeout(120_000);
	const open = async (locale?: string) =>
		(await browser.newContext({ locale, permissions: ['microphone'] })).newPage();
	const [a, b, c] = [await open(), await open('zh-CN'), await open()];

	await a.goto('/demo/voice');
	await a.getByRole('button', { name: 'Create a room' }).click();
	await expect(a).toHaveURL(/#[\w-]{43}$/);
	const link = a.url();
	await a.getByLabel('Your name').fill('Alice');
	await a.getByRole('button', { name: 'Join with microphone' }).click();

	await b.goto(link.replace('/demo/voice#', '/zh/demo/voice#'));
	await b.getByLabel('你的名字').fill('小明');
	await b.getByRole('button', { name: '开启麦克风并加入' }).click();
	await c.goto(link);
	await c.getByLabel('Your name').fill('Carol');
	await c.getByRole('button', { name: 'Join with microphone' }).click();

	for (const page of [a, b, c]) {
		await expect(page.locator('.person')).toHaveCount(3, { timeout: 60_000 });
		await expect(page.locator('.status.online')).toBeVisible();
	}
	// Ourselves first, then the others (in the order they happened to arrive).
	await expect(a.locator('.person').first()).toHaveClass(/mine/);
	await expect(a.locator('.person').first()).toContainText('Alice');
	for (const name of ['小明', 'Carol'])
		await expect(a.locator('.person', { hasText: name })).toHaveCount(1);

	// The fake microphones beep: our own card lights up as we speak, and so
	// do the others' as we hear them.
	await expect(a.locator('.person.mine.talking')).toBeVisible({ timeout: 30_000 });
	await expect(a.locator('.person:not(.mine).talking')).not.toHaveCount(0, { timeout: 30_000 });

	// Carol turns her microphone off, then stops listening: everyone sees it on her card.
	const carol = a.locator('.person', { hasText: 'Carol' });
	await c.getByRole('button', { name: 'Mute your microphone: nobody hears you' }).click();
	await expect(c.locator('.person.mine')).toHaveClass(/off/);
	// Off is a flat line: every bar of the waveform at its minimum.
	await c.waitForTimeout(2000);
	const heights = await c
		.locator('.person.mine .wave rect')
		.evaluateAll((bars) => bars.map((bar) => Number(bar.getAttribute('height'))));
	expect(Math.max(...heights)).toBe(2);
	await expect(carol.locator('.state')).toHaveCount(1, { timeout: 30_000 });
	await expect(carol).toHaveClass(/off/);
	await c.getByRole('button', { name: 'Turn your headphones off: you hear nobody' }).click();
	await expect(carol.locator('.state')).toHaveCount(2, { timeout: 30_000 });
	await c.getByRole('button', { name: 'Unmute your microphone' }).click();
	await expect(carol.locator('.state')).toHaveCount(1, { timeout: 30_000 });

	// Muting someone for oneself is on their card.
	await a.getByRole('button', { name: 'Stop hearing 小明 (only for you)' }).click();
	await expect(a.locator('.person', { hasText: '小明' })).toHaveClass(/off/);

	await b.locator('#voice-message').fill('你好');
	await b.locator('#voice-message').press('Enter');
	for (const page of [a, b, c]) await expect(page.locator('.line')).toContainText(['小明 你好']);

	await c.getByRole('button', { name: 'Leave' }).click();
	await expect(a.locator('.person')).toHaveCount(2, { timeout: 30_000 });
});

test('the camera turns on and off from our own card', async ({ page }) => {
	await page.goto('/demo/voice');
	await page.getByRole('button', { name: 'Create a room' }).click();
	await page.getByLabel('Your name').fill('Alice');
	await page.getByRole('button', { name: 'Join with microphone' }).click();
	const mine = page.locator('.person.mine');
	await expect(mine.locator('.screen')).toHaveCount(0); // off until asked
	await page.getByRole('button', { name: 'Turn your camera on: everyone sees you' }).click();
	// What we send, as the others will see it: the fake camera is wide.
	const preview = mine.locator('canvas.screen');
	await expect(preview).toBeVisible();
	const size = () => preview.evaluate((c: HTMLCanvasElement) => [c.width, c.height]);
	await expect.poll(size, { timeout: 10_000 }).toEqual([480, 270]);
	// Turned a quarter turn, for everyone: tall.
	await page
		.getByRole('button', { name: 'Turn your picture a quarter turn, for everyone' })
		.click();
	await expect.poll(size, { timeout: 10_000 }).toEqual([270, 480]);
	await page.getByRole('button', { name: 'Turn your camera off' }).click();
	await expect(mine.locator('.screen')).toHaveCount(0);
});

// Through the public relay too: run with PEERCONNECT_DEMO_E2E=1.
test('two browsers see each other on their cards', async ({ browser }) => {
	test.skip(
		!process.env.PEERCONNECT_DEMO_E2E,
		'set PEERCONNECT_DEMO_E2E=1 to use the public relay'
	);
	test.setTimeout(120_000);
	const open = async () =>
		(await browser.newContext({ permissions: ['microphone', 'camera'] })).newPage();
	const [a, b] = [await open(), await open()];
	await a.goto('/demo/voice');
	await a.getByRole('button', { name: 'Create a room' }).click();
	await expect(a).toHaveURL(/#[\w-]{43}$/);
	await a.getByLabel('Your name').fill('Alice');
	await a.getByRole('button', { name: 'Join with microphone' }).click();
	await b.goto(a.url());
	await b.getByLabel('Your name').fill('Bob');
	await b.getByRole('button', { name: 'Join with microphone' }).click();
	for (const page of [a, b])
		await expect(page.locator('.person')).toHaveCount(2, { timeout: 60_000 });

	await a.getByRole('button', { name: 'Turn your camera on: everyone sees you' }).click();
	// Bob sees Alice's video on her card: a canvas with a picture in it.
	const screen = b.locator('.person', { hasText: 'Alice' }).locator('canvas.screen');
	await expect(screen).toBeVisible({ timeout: 30_000 });
	await expect
		.poll(
			() =>
				screen.evaluate((c: HTMLCanvasElement) => {
					const { data } = c.getContext('2d')!.getImageData(0, 0, c.width, c.height);
					let lit = 0;
					for (let i = 0; i < data.length; i += 4)
						if (data[i] + data[i + 1] + data[i + 2] > 60) lit++;
					return lit / (data.length / 4);
				}),
			{ timeout: 30_000 }
		)
		.toBeGreaterThan(0.05);
	// And nothing on Bob's own card, nor on Alice's view of Bob.
	await expect(b.locator('.person.mine .screen')).toHaveCount(0);
	await expect(a.locator('.person', { hasText: 'Bob' }).locator('.screen')).toHaveCount(0);

	// Bob turns Alice's picture for himself: tall on his page.
	await b
		.getByRole('button', { name: 'Turn Alice’s picture a quarter turn (only for you)' })
		.click();
	await expect
		.poll(() => screen.evaluate((c: HTMLCanvasElement) => c.height > c.width), { timeout: 10_000 })
		.toBe(true);

	// When she turns it off, her video goes from Bob's page.
	await a.getByRole('button', { name: 'Turn your camera off' }).click();
	await expect(screen).toHaveCount(0, { timeout: 30_000 });
});
