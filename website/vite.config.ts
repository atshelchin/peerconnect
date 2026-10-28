import { defineConfig } from 'vitest/config';
import { playwright } from '@vitest/browser-playwright';
import adapter from '@sveltejs/adapter-cloudflare';
import { sveltekit } from '@sveltejs/kit/vite';

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			adapter: adapter(),
			prerender: {
				// The Chinese pages are reached through the language switch, but list
				// them anyway so that none is missed.
				entries: [
					'*',
					'/zh',
					'/zh/docs',
					'/zh/demo',
					'/sitemap.xml',
					'/docs/api',
					'/llms.txt',
					'/llms-full.txt',
					'/spec.md',
					'/docs/api.md'
				]
			},
			// Small stylesheets go inline, saving a render-blocking request.
			inlineStyleThreshold: 16 * 1024,
			// Kit's origin check refuses any mutating request without an Origin
			// and a content type, which an app's server calling the API (a bare
			// `DELETE`) sends neither of. Every form here carries a CSRF token
			// checked on the server (#lib/server/guard.ts), the JSON endpoints
			// check the Origin themselves, and the session cookie is SameSite=Lax.
			csrf: { trustedOrigins: ['*'] }
		})
	],
	// The specification and API pages read ../SPEC.md and ../API.md.
	server: { fs: { allow: ['..'] } },
	test: {
		expect: { requireAssertions: true },
		projects: [
			{
				extends: './vite.config.ts',
				test: {
					name: 'client',
					browser: {
						enabled: true,
						provider: playwright(),
						instances: [{ browser: 'chromium', headless: true }]
					},
					include: ['src/**/*.svelte.{test,spec}.{js,ts}'],
					exclude: ['src/lib/server/**']
				}
			},

			{
				extends: './vite.config.ts',
				test: {
					name: 'server',
					environment: 'node',
					include: ['src/**/*.{test,spec}.{js,ts}'],
					exclude: ['src/**/*.svelte.{test,spec}.{js,ts}']
				}
			}
		]
	}
});
