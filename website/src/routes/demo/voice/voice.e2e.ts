import { expect, test } from '@playwright/test';

// Chromium's fake microphone (a beep) and no permission prompt.
test.use({
	launchOptions: {
		args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream']
	},
	permissions: ['microphone']
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
		await expect(page.locator('.member')).toHaveCount(3, { timeout: 60_000 });
		await expect(page.locator('.status.online')).toBeVisible();
	}
	await expect(a.locator('.member .name')).toContainText(['Alice', '小明', 'Carol']);

	// The fake microphone beeps: someone else is heard speaking, with stats.
	await expect(a.locator('.member.speaking')).not.toHaveCount(0, { timeout: 30_000 });
	await expect(a.locator('.member .meta').first()).toContainText('ms', { timeout: 30_000 });

	await b.locator('#voice-message').fill('你好');
	await b.locator('#voice-message').press('Enter');
	for (const page of [a, b, c]) await expect(page.locator('.line')).toContainText(['小明 你好']);

	await c.getByRole('button', { name: 'Leave' }).click();
	await expect(a.locator('.member')).toHaveCount(2, { timeout: 30_000 });
});
