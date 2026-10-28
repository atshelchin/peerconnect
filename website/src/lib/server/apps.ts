// Developers' applications and their requests (docs/self-serve.md,
// "Returning the token to the app"). A redirect URI is compared exactly: no
// prefix, no added query, no other scheme. A code is only ever handed to an
// address the app's developer registered.

export interface App {
	id: string;
	account_id: string;
	name: string;
	redirect_uris: string[];
	/** A desktop app's loopback return (RFC 8252 §7.3): `http://127.0.0.1:<port><path>`, any port. */
	loopback_path: string | null;
	secret_gen: number;
	user_limit: number;
	state: 'active' | 'disabled';
	created_at: number;
}

export interface AppFields {
	name: string;
	redirectUris: string[];
	origins: string[];
	loopbackPath: string | null;
}

export const MAX_REDIRECT_URIS = 10;
export const MAX_ORIGINS = 10;

const LOOPBACK = /^http:\/\/127\.0\.0\.1:([1-9][0-9]{0,4})(\/[A-Za-z0-9/._-]*)$/;
const LOCAL_ORIGIN = /^http:\/\/(localhost|127\.0\.0\.1):[1-9][0-9]{0,4}$/;

/** Whether `app` may be sent back to `redirectUri`. */
export function returnsTo(app: App, redirectUri: string): boolean {
	if (app.redirect_uris.includes(redirectUri)) return true;
	const loopback = LOOPBACK.exec(redirectUri);
	return (
		!!loopback &&
		app.loopback_path !== null &&
		Number(loopback[1]) <= 65535 &&
		loopback[2] === app.loopback_path
	);
}

/**
 * A redirect URI a developer may register, or why not:
 * - `https://` with a host, no fragment;
 * - a private-use scheme in reverse-domain form (RFC 8252 §7.1), such as
 *   `com.example.app:/peerconnect`;
 * - `http://localhost:<port>/…` or `http://127.0.0.1:<port>/…`, for development.
 */
export function redirectProblem(uri: string): string | null {
	if (uri.length > 512) return 'is longer than 512 characters';
	if (uri.includes('#')) return 'has a fragment';
	if (/\s/.test(uri)) return 'contains spaces';
	const scheme = /^([a-z][a-z0-9+.-]*):/.exec(uri)?.[1];
	if (!scheme) return 'has no scheme';
	if (scheme === 'https' || scheme === 'http') {
		let url: URL;
		try {
			url = new URL(uri);
		} catch {
			return 'is not a URL';
		}
		if (scheme === 'http' && !/^(localhost|127\.0\.0\.1)$/.test(url.hostname))
			return 'is http:// on a host other than localhost (use https://)';
		return url.hostname ? null : 'has no host';
	}
	if (!scheme.includes('.'))
		return 'uses a custom scheme not in reverse-domain form (com.example.app:/…)';
	if (['javascript', 'data', 'file', 'blob'].includes(scheme)) return 'uses a forbidden scheme';
	return null;
}

/** A web origin a developer may register for browser code exchange, or why not. */
export function originProblem(origin: string): string | null {
	if (LOCAL_ORIGIN.test(origin)) return null;
	let url: URL;
	try {
		url = new URL(origin);
	} catch {
		return 'is not an origin';
	}
	if (url.protocol !== 'https:') return 'is not https://';
	return url.origin === origin ? null : 'is not an origin (scheme://host[:port], no path)';
}

/** A loopback path for desktop apps, or why not. */
export function loopbackProblem(path: string): string | null {
	return /^\/[A-Za-z0-9/._-]{0,100}$/.test(path) ? null : 'must be a path such as /peerconnect';
}

/** Lines of a textarea, trimmed, without empty ones or duplicates. */
export function lines(text: FormDataEntryValue | null): string[] {
	return [
		...new Set(
			String(text ?? '')
				.split(/\r?\n/)
				.map((l) => l.trim())
				.filter(Boolean)
		)
	];
}

export interface AppRequest {
	app: string;
	appName: string;
	redirectUri: string;
	state: string;
	codeChallenge: string;
	/** What the person sees this installation called, e.g. "iPhone". */
	label: string;
	/** Stable per app installation: signing in again from it rotates its token. */
	installation: string;
}

const STATE = /^[A-Za-z0-9\-._~]{8,256}$/;
const INSTALLATION = /^[A-Za-z0-9\-._~]{8,64}$/;

/**
 * A `/connect` request from `app`, validated, or why not. An invalid
 * redirect URI is never redirected to: the page says so instead
 * (RFC 6749 §4.1.2.1).
 */
export function appRequest(
	app: App | null,
	params: URLSearchParams
): { ok: AppRequest } | { error: string } {
	if (!app || app.state !== 'active')
		return { error: 'This application is not registered with peerconnect.dev.' };
	const redirectUri = params.get('redirect_uri') ?? '';
	if (!returnsTo(app, redirectUri)) {
		return { error: `This return address is not registered for ${app.name}.` };
	}
	const state = params.get('state') ?? '';
	const codeChallenge = params.get('code_challenge') ?? '';
	if (
		params.get('code_challenge_method') !== 'S256' ||
		!/^[A-Za-z0-9_-]{43}$/.test(codeChallenge)
	) {
		return { error: 'The request is missing its PKCE challenge (S256).' };
	}
	if (!STATE.test(state)) return { error: 'The request is missing its state.' };
	const installation = params.get('installation') ?? '';
	if (!INSTALLATION.test(installation))
		return { error: 'The request does not name its installation.' };
	return {
		ok: {
			app: app.id,
			appName: app.name,
			redirectUri,
			state,
			codeChallenge,
			label: clean(params.get('label'), app.name),
			installation
		}
	};
}

/** A label as shown: printable, at most 40 characters, `fallback` when empty. */
export function clean(label: string | null, fallback: string): string {
	const text = (label ?? '')
		// Control characters and angle brackets: a label is plain text.
		// eslint-disable-next-line no-control-regex
		.replace(/[\u0000-\u001f\u007f<>]/g, '')
		.trim()
		.slice(0, 40);
	return text || fallback;
}

/** `redirectUri` with `code` and `state` added. */
export function redirectWithCode(redirectUri: string, code: string, state: string): string {
	const separator = redirectUri.includes('?') ? '&' : '?';
	return `${redirectUri}${separator}code=${encodeURIComponent(code)}&state=${encodeURIComponent(state)}`;
}

/** An app's own user ID, as its server names it (docs/self-serve.md): 1–128 of `A–Z a–z 0–9 . _ - : @`. */
export function isUserId(user: unknown): user is string {
	return typeof user === 'string' && /^[A-Za-z0-9._:@-]{1,128}$/.test(user);
}

/**
 * An app's settings from its form, or the problems with them. Shared by
 * registration and the app's settings form.
 */
export function appFields(form: FormData): { ok: AppFields } | { problems: string[] } {
	const name = clean(form.get('name') as string | null, '');
	const redirectUris = lines(form.get('redirect_uris'));
	const origins = lines(form.get('origins'));
	const loopback = String(form.get('loopback_path') ?? '').trim();
	const problems: string[] = [];
	if (!name) problems.push('The application needs a name.');
	if (redirectUris.length > MAX_REDIRECT_URIS)
		problems.push(`At most ${MAX_REDIRECT_URIS} redirect URIs.`);
	if (origins.length > MAX_ORIGINS) problems.push(`At most ${MAX_ORIGINS} web origins.`);
	for (const uri of redirectUris) {
		const problem = redirectProblem(uri);
		if (problem) problems.push(`${uri} ${problem}.`);
	}
	for (const origin of origins) {
		const problem = originProblem(origin);
		if (problem) problems.push(`${origin} ${problem}.`);
	}
	if (loopback) {
		const problem = loopbackProblem(loopback);
		if (problem) problems.push(`The loopback path ${problem}.`);
	}
	if (problems.length) return { problems };
	return { ok: { name, redirectUris, origins, loopbackPath: loopback || null } };
}
