import { defineConfig } from '@playwright/test';

// Port 4317, not Vite's usual 4173: a preview of another project on that port
// would otherwise be reused, and tested, in place of this site.
export default defineConfig({
	webServer: {
		command: 'npm run build && npm run preview',
		url: 'http://127.0.0.1:4317/',
		reuseExistingServer: !process.env.CI,
		timeout: 180_000
	},
	use: { baseURL: 'http://127.0.0.1:4317' },
	testMatch: '**/*.e2e.{ts,js}'
});
