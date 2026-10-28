// Sign-in with GitHub (OAuth web flow). The app asks for no scope: only the
// person's public identity — numeric ID, login, avatar — is read, and no
// email address. The two base URLs are configuration so that the tests can
// stand in a fake GitHub.

export interface GitHubConfig {
	clientId: string;
	clientSecret: string;
	/** `https://github.com` unless a test says otherwise. */
	oauthBase: string;
	/** `https://api.github.com` unless a test says otherwise. */
	apiBase: string;
}

export interface GitHubUser {
	id: string;
	login: string;
	avatarUrl: string;
}

export function authorizeUrl(config: GitHubConfig, redirectUri: string, state: string): string {
	const url = new URL('/login/oauth/authorize', config.oauthBase);
	url.searchParams.set('client_id', config.clientId);
	url.searchParams.set('redirect_uri', redirectUri);
	url.searchParams.set('state', state);
	url.searchParams.set('allow_signup', 'true');
	return url.toString();
}

/** The person behind an authorization `code`, or `null` if GitHub says no. */
export async function userFor(
	config: GitHubConfig,
	code: string,
	redirectUri: string
): Promise<GitHubUser | null> {
	const exchanged = await fetch(new URL('/login/oauth/access_token', config.oauthBase), {
		method: 'POST',
		headers: { accept: 'application/json', 'content-type': 'application/json' },
		body: JSON.stringify({
			client_id: config.clientId,
			client_secret: config.clientSecret,
			code,
			redirect_uri: redirectUri
		})
	});
	if (!exchanged.ok) return null;
	const { access_token: token } = (await exchanged.json()) as { access_token?: string };
	if (!token) return null;
	const user = await fetch(new URL('/user', config.apiBase), {
		headers: {
			accept: 'application/vnd.github+json',
			authorization: `Bearer ${token}`,
			'user-agent': 'peerconnect.dev'
		}
	});
	if (!user.ok) return null;
	const body = (await user.json()) as { id?: number; login?: string; avatar_url?: string };
	if (typeof body.id !== 'number' || typeof body.login !== 'string') return null;
	return { id: String(body.id), login: body.login, avatarUrl: body.avatar_url ?? '' };
}
