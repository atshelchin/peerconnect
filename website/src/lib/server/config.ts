// The site's configuration, from the Worker's bindings, secrets and vars.
// A dynamic page with any of it missing answers 503 rather than half-work:
// sign-in and issuance never run on a guess.

import { error } from '@sveltejs/kit';
import { env as workerEnv } from 'cloudflare:workers';
import type { GitHubConfig } from './github.ts';
import type { RelayConfig } from './relay.ts';

export interface SiteConfig {
	db: D1Database;
	relay: RelayConfig;
	github: GitHubConfig;
	sessionKey: string;
	/** Derives app secrets and user keys (crypto.ts). */
	appSecretKey: string;
	/** What apps are told to connect to: `wss://relay.peerconnect.dev`. */
	relayPublicUrl: string;
	/** The owner's numeric GitHub ID: the admin page opens for that identity alone (a login can change hands). */
	adminGithubId: string;
	/** Cookies are `Secure` except on a local http:// test server. */
	secureCookies: boolean;
}

export function siteConfig(url: URL): SiteConfig {
	// Kit 3's Cloudflare adapter hands the bindings over as `cloudflare:workers`'s `env`.
	const env = workerEnv as unknown as Record<string, unknown> | undefined;
	const text = (name: string) => (typeof env?.[name] === 'string' ? (env[name] as string) : '');
	const db = env?.SITE_DB as D1Database | undefined;
	const binding = env?.RELAY as Fetcher | undefined;
	const missing = [
		!db && 'SITE_DB',
		!binding && 'RELAY',
		text('SERVICE_SECRET').length < 32 && 'SERVICE_SECRET',
		!text('GITHUB_CLIENT_ID') && 'GITHUB_CLIENT_ID',
		!text('GITHUB_CLIENT_SECRET') && 'GITHUB_CLIENT_SECRET',
		text('SESSION_KEY').length < 32 && 'SESSION_KEY',
		text('APP_SECRET_KEY').length < 32 && 'APP_SECRET_KEY',
		!text('RELAY_PUBLIC_URL') && 'RELAY_PUBLIC_URL'
	].filter(Boolean);
	if (missing.length) {
		console.error(`peerconnect.dev: not configured: ${missing.join(', ')}`);
		error(503, 'Sign-in is not available right now.');
	}
	return {
		db: db!,
		relay: { binding: binding!, serviceSecret: text('SERVICE_SECRET') },
		github: {
			clientId: text('GITHUB_CLIENT_ID'),
			clientSecret: text('GITHUB_CLIENT_SECRET'),
			oauthBase: text('GITHUB_OAUTH_BASE') || 'https://github.com',
			apiBase: text('GITHUB_API_BASE') || 'https://api.github.com'
		},
		sessionKey: text('SESSION_KEY'),
		appSecretKey: text('APP_SECRET_KEY'),
		relayPublicUrl: text('RELAY_PUBLIC_URL'),
		adminGithubId: text('ADMIN_GITHUB_ID'),
		secureCookies: url.protocol === 'https:'
	};
}
