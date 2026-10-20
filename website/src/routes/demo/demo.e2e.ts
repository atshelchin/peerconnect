import { expect, test } from '@playwright/test';

// Pairs two browsers through the public relay, so it needs the network:
// run with PEERCONNECT_DEMO_E2E=1.
test('two browsers pair, chat, survive a reload and end the room', async ({ browser }) => {
	test.skip(
		!process.env.PEERCONNECT_DEMO_E2E,
		'set PEERCONNECT_DEMO_E2E=1 to use the public relay'
	);
	test.setTimeout(120_000);
	const a = await (await browser.newContext()).newPage();
	const b = await (await browser.newContext({ locale: 'zh-CN' })).newPage();

	await a.goto('/demo');
	await a.getByRole('button', { name: 'Create a room' }).click();
	const link = (await a.locator('.link code').textContent()) ?? '';
	expect(link).toMatch(/\/demo#[\w-]{80,}$/);

	await b.goto(link.replace('/demo#', '/zh/demo#'));
	await b.getByRole('button', { name: '加入房间' }).click();
	await expect(a.locator('.status.connected')).toBeVisible({ timeout: 60_000 });
	await expect(b.locator('.status.connected')).toBeVisible({ timeout: 60_000 });
	await expect(a.locator('.safety')).toHaveText(/^\d{4} \d{4} \d{4}$/);
	expect(await a.locator('.safety').textContent()).toBe(await b.locator('.safety').textContent());

	await a.locator('#message').fill('hello');
	await a.locator('#message').press('Enter');
	await expect(b.locator('.line.peer .bubble')).toHaveText(['hello']);
	await expect(a.locator('.line.me .mark.delivered')).toHaveCount(1);

	await b.reload();
	await a.locator('#message').fill('while you were away');
	await a.locator('#message').press('Enter');
	await expect(b.locator('.line.peer .bubble')).toHaveText(['hello', 'while you were away'], {
		timeout: 60_000
	});

	a.on('dialog', (dialog) => dialog.accept());
	await a.getByRole('button', { name: 'End room' }).click();
	await expect(a.locator('.status.closed')).toBeVisible({ timeout: 60_000 });
	await expect(b.locator('.status.closed')).toBeVisible({ timeout: 60_000 });
});
