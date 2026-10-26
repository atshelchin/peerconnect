import { expect, test } from '@playwright/test';

// Two fake cameras (no permission prompt): switching between them.
test.use({
	launchOptions: {
		args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream=device-count=2']
	},
	permissions: ['microphone', 'camera']
});

test('the camera switches to the other one in a click', async ({ page }) => {
	await page.addInitScript(() => {
		const real = navigator.mediaDevices.getUserMedia.bind(navigator.mediaDevices);
		const opened: MediaStream[] = [];
		(window as unknown as { opened: MediaStream[] }).opened = opened;
		navigator.mediaDevices.getUserMedia = async (c) => {
			const stream = await real(c);
			opened.push(stream);
			return stream;
		};
	});
	await page.goto('/demo/voice');
	await page.getByRole('button', { name: 'Create a room' }).click();
	await page.getByLabel('Your name').fill('Alice');
	await page.getByRole('button', { name: 'Join', exact: true }).click();
	await page.getByRole('button', { name: 'Turn your camera on: everyone sees you' }).click();
	const switcher = page.getByRole('button', { name: 'Switch camera (front and back)' });
	await expect(switcher).toBeVisible();
	const cameras = () =>
		page.evaluate(() =>
			(window as unknown as { opened: MediaStream[] }).opened.map((s) => {
				const track = s.getVideoTracks()[0];
				return [track.label, track.readyState];
			})
		);
	await expect.poll(cameras).toEqual([['fake_device_0', 'live']]);
	await switcher.click();
	// The first let go of, the other one open, and still sending.
	await expect.poll(cameras).toEqual([
		['fake_device_0', 'ended'],
		['fake_device_1', 'live']
	]);
	await expect(page.locator('.person.mine canvas.screen')).toBeVisible();
	await switcher.click();
	await expect.poll(async () => (await cameras()).at(-1)).toEqual(['fake_device_0', 'live']);
});
