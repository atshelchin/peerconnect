// peerconnect.dev's self-serve flow end to end (docs/self-serve.md): GitHub
// sign-in (a local fake), registering an app, its secret, a person's
// /connect with PKCE and the code exchange, tokens from the app's server,
// real rooms on the minted tokens through the relay, rotation and deletion,
// and the checks that keep it safe. Run by run-self-serve.sh.

import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { after, test } from 'node:test';

import { connectRequest, exchangeCode } from '../../js/auth.js';
import { connect, Room } from '../../js/index.js';

const SITE = process.env.SITE_URL;
const GITHUB = process.env.GITHUB_URL;
const RELAY = process.env.RELAY_URL;
const REDIRECT = 'com.example.chat:/peerconnect';
const WEB_ORIGIN = 'https://chat.example.com';

const open = [];
after(() => open.forEach((c) => c.leave()));

/** A browser: cookies, no automatic redirects. */
class Browser {
	cookies = new Map();
	async fetch(path, init = {}) {
		const headers = new Headers(init.headers);
		if (this.cookies.size)
			headers.set('cookie', [...this.cookies].map(([k, v]) => `${k}=${v}`).join('; '));
		const response = await fetch(new URL(path, SITE), { ...init, headers, redirect: 'manual' });
		for (const line of response.headers.getSetCookie()) {
			const [pair, ...attrs] = line.split(';');
			const [name, ...rest] = pair.split('=');
			const value = rest.join('=');
			if (attrs.some((a) => /max-age=0/i.test(a)) || value === '') this.cookies.delete(name.trim());
			else this.cookies.set(name.trim(), value);
		}
		return response;
	}
	/** Follows redirects on this site, and GitHub's, which come back to it. */
	async follow(path) {
		let response = await this.fetch(path);
		for (let i = 0; i < 6 && response.status >= 300 && response.status < 400; i++) {
			const location = response.headers.get('location');
			const target = location.startsWith(GITHUB)
				? (await fetch(location, { redirect: 'manual' })).headers.get('location')
				: location;
			response = await this.fetch(target);
		}
		return response;
	}
	/** A form post, as the page's own form sends it. */
	submit(path, fields) {
		return this.fetch(path, {
			method: 'POST',
			headers: {
				'content-type': 'application/x-www-form-urlencoded',
				origin: SITE,
				accept: 'text/html'
			},
			body: new URLSearchParams(fields)
		});
	}
	/** A JSON post from the page's own script. */
	json(path, body, origin = SITE) {
		return this.fetch(path, {
			method: 'POST',
			headers: { 'content-type': 'application/json', origin },
			body: JSON.stringify(body)
		});
	}
}

async function signedIn(who) {
	const browser = new Browser();
	await fetch(`${GITHUB}/_as/${who}`);
	const page = await browser.follow(`/auth/github/start?next=${encodeURIComponent('/developers')}`);
	assert.equal(page.status, 200, 'signed in');
	return { browser, html: await page.text() };
}

const csrfOf = (html) => html.match(/name="csrf" value="([^"]+)"/)?.[1];
const appIdOf = (location) => location.match(/\/developers\/(app_[a-z2-7]{16})$/)?.[1];

/** Registers an app as `browser`; returns its ID and its secret. */
async function register(browser, html, fields = {}) {
	const done = await browser.submit('/developers?/create', {
		csrf: csrfOf(html),
		name: 'Chat',
		redirect_uris: `${REDIRECT}\nhttps://chat.example.com/peerconnect`,
		origins: WEB_ORIGIN,
		loopback_path: '/peerconnect',
		...fields
	});
	assert.equal(done.status, 303, await done.clone().text());
	const id = appIdOf(done.headers.get('location'));
	assert.ok(id, done.headers.get('location'));
	const page = await browser.fetch(`/developers/${id}`);
	const appHtml = await page.text();
	const secret = await browser.json(`/developers/${id}/secret`, { csrf: csrfOf(appHtml) });
	assert.equal(secret.status, 200);
	return { id, secret: (await secret.json()).secret, html: appHtml };
}

const pkce = () => {
	const verifier = randomBytes(32).toString('base64url');
	return { verifier, challenge: createHash('sha256').update(verifier).digest('base64url') };
};

const connectPath = (
	app,
	{
		challenge,
		state = 'state-' + randomBytes(6).toString('hex'),
		installation = 'inst-' + randomBytes(6).toString('hex'),
		redirect = REDIRECT
	} = {}
) =>
	`/connect?${new URLSearchParams({ app, redirect_uri: redirect, state, code_challenge: challenge, code_challenge_method: 'S256', label: 'Test phone', installation })}`;

/** Through /connect as `browser`: the code the app gets back. */
async function approve(browser, path, fields = {}) {
	const page = await browser.fetch(path);
	assert.equal(page.status, 200, 'the consent page');
	const html = await page.text();
	const done = await browser.submit(`${path}&/approve`, { csrf: csrfOf(html), ...fields });
	const body = await done.text();
	const returnTo = body
		.match(/href="([^"]+[?&](?:amp;)?code=[^"]+)"/)?.[1]
		?.replaceAll('&amp;', '&');
	return {
		status: done.status,
		returnTo,
		code: returnTo && new URL(returnTo.replace(/^[^:]+:/, 'x://h')).searchParams.get('code')
	};
}

async function exchange(fields, origin) {
	const response = await fetch(`${SITE}/api/token`, {
		method: 'POST',
		headers: { 'content-type': 'application/json', ...(origin ? { origin } : {}) },
		body: JSON.stringify({ grant_type: 'authorization_code', redirect_uri: REDIRECT, ...fields })
	});
	return { status: response.status, json: await response.json(), headers: response.headers };
}

const serverToken = (app, secret, body) =>
	fetch(`${SITE}/api/apps/${app}/tokens`, {
		method: 'POST',
		headers: { authorization: `Bearer ${secret}`, 'content-type': 'application/json' },
		body: JSON.stringify(body)
	});

/** The close code a raw WebSocket on `token` gets first, or "hello" if the relay lets it in. */
function admitted(token) {
	const room = Room.create(RELAY);
	return new Promise((resolve) => {
		const ws = new WebSocket(`${RELAY}/v1/${room.id}?token=${token}`);
		ws.onmessage = () => {
			ws.close();
			resolve('hello');
		};
		ws.onclose = (e) => resolve(e.code);
	});
}

function event(target, type) {
	return new Promise((resolve, reject) => {
		const timer = setTimeout(() => reject(new Error(`timed out waiting for ${type}`)), 20_000);
		target.addEventListener(type, (e) => (clearTimeout(timer), resolve(e)), { once: true });
	});
}

test('signing in, and the developer pages are private', async () => {
	const anonymous = await fetch(`${SITE}/developers`, { redirect: 'manual' });
	assert.equal(anonymous.status, 303);
	assert.match(anonymous.headers.get('location'), /^\/auth\/github\/start\?next=%2Fdevelopers/);
	const { html } = await signedIn('alice');
	assert.match(html, /@alice/);
	const page = await fetch(`${SITE}/developers`, { redirect: 'manual' });
	assert.equal(page.headers.get('cache-control'), 'no-store');
	// The landing page is still prerendered.
	assert.match(await (await fetch(SITE)).text(), /PeerConnect/);
});

test('an app is registered with checked redirect URIs, and its secret is shown again', async () => {
	const { browser, html } = await signedIn('alice');
	const bad = await browser.submit('/developers?/create', {
		csrf: csrfOf(html),
		name: 'Bad',
		redirect_uris: 'http://evil.example/cb\nmyapp:/cb',
		origins: 'https://ok.example/path'
	});
	assert.equal(bad.status, 400);
	const text = await bad.text();
	assert.match(text, /http:\/\/evil\.example\/cb is http:\/\/ on a host other than localhost/);
	assert.match(text, /myapp:\/cb uses a custom scheme not in reverse-domain form/);
	assert.match(text, /https:\/\/ok\.example\/path is not an origin/);

	const app = await register(browser, html);
	assert.match(app.secret, /^pcs_[A-Za-z0-9_-]{43}$/);
	const again = await browser.json(`/developers/${app.id}/secret`, { csrf: csrfOf(app.html) });
	assert.equal((await again.json()).secret, app.secret, 'derived, the same each time');
	assert.equal(
		(await browser.json(`/developers/${app.id}/secret`, { csrf: 'forged' })).status,
		403
	);
	assert.equal(
		(
			await browser.json(
				`/developers/${app.id}/secret`,
				{ csrf: csrfOf(app.html) },
				'https://evil.example'
			)
		).status,
		403
	);
	// Someone else's app is not there for bob.
	const { browser: bob } = await signedIn('bob');
	assert.equal((await bob.fetch(`/developers/${app.id}`)).status, 404);
});

test('a person connects an app with PKCE, and the token works on the relay', async () => {
	const { browser: dev, html } = await signedIn('alice');
	const app = await register(dev, html);
	const { browser } = await signedIn('bob');
	const { verifier, challenge } = pkce();
	const { returnTo, code } = await approve(browser, connectPath(app.id, { challenge }));
	assert.ok(returnTo.startsWith(`${REDIRECT}?code=`), returnTo);

	assert.equal(
		(await exchange({ client_id: app.id, code, code_verifier: pkce().verifier })).json.error,
		'invalid_grant',
		'a wrong verifier'
	);
	const reused = await exchange({ client_id: app.id, code, code_verifier: verifier });
	assert.equal(reused.json.error, 'invalid_grant', 'the failed attempt used the code up');

	const second = await approve(browser, connectPath(app.id, { challenge }));
	const ok = await exchange({ client_id: app.id, code: second.code, code_verifier: verifier });
	assert.equal(ok.status, 200, JSON.stringify(ok.json));
	assert.equal(ok.json.login, 'bob');
	assert.equal(ok.json.relay_url, RELAY);
	assert.equal(ok.headers.get('cache-control'), 'no-store');
	assert.equal(await admitted(ok.json.token), 'hello');

	// A real room: bob's token on one side, a server-issued one on the other.
	const issued = await (await serverToken(app.id, app.secret, { user: 'carol-42' })).json();
	const inviter = Room.create(RELAY);
	const a = connect(inviter, { token: ok.json.token });
	const b = connect(Room.join(inviter.invite), { token: issued.token });
	open.push(a, b);
	b.send('hello through the relay');
	assert.equal(
		new TextDecoder().decode((await event(a, 'message')).data),
		'hello through the relay'
	);
});

test('bad requests never reach a redirect', async () => {
	const { browser: dev, html } = await signedIn('alice');
	const app = await register(dev, html);
	const { browser } = await signedIn('bob');
	const { challenge } = pkce();
	const wrongRedirect = await browser.fetch(
		connectPath(app.id, { challenge, redirect: 'com.evil.app:/steal' })
	);
	assert.equal(wrongRedirect.status, 400);
	assert.equal(wrongRedirect.headers.get('location'), null);
	assert.equal(
		(await browser.fetch(connectPath('app_aaaaaaaaaaaaaaaa', { challenge }))).status,
		400
	);
	// The loopback path, any port.
	assert.equal(
		(
			await browser.fetch(
				connectPath(app.id, { challenge, redirect: 'http://127.0.0.1:53111/peerconnect' })
			)
		).status,
		200
	);
	assert.equal(
		(
			await browser.fetch(
				connectPath(app.id, { challenge, redirect: 'http://127.0.0.1:53111/other' })
			)
		).status,
		400
	);
});

test("a confidential app's secret is checked, and browsers exchange only from its origins", async () => {
	const { browser: dev, html } = await signedIn('alice');
	const app = await register(dev, html);
	const { browser } = await signedIn('bob');
	const { verifier, challenge } = pkce();
	const wrong = await approve(browser, connectPath(app.id, { challenge }));
	assert.equal(
		(
			await exchange({
				client_id: app.id,
				code: wrong.code,
				code_verifier: verifier,
				client_secret: 'pcs_wrong'
			})
		).status,
		401
	);
	const right = await approve(browser, connectPath(app.id, { challenge }));
	assert.equal(
		(
			await exchange({
				client_id: app.id,
				code: right.code,
				code_verifier: verifier,
				client_secret: app.secret
			})
		).status,
		200
	);

	const preflight = (origin) =>
		fetch(`${SITE}/api/token`, {
			method: 'OPTIONS',
			headers: { origin, 'access-control-request-method': 'POST' }
		});
	const allowed = await preflight(WEB_ORIGIN);
	assert.equal(allowed.status, 204);
	assert.equal(allowed.headers.get('access-control-allow-origin'), WEB_ORIGIN);
	assert.equal((await preflight('https://evil.example')).status, 403);
	const fromPage = await approve(browser, connectPath(app.id, { challenge }));
	const answer = await exchange(
		{ client_id: app.id, code: fromPage.code, code_verifier: verifier },
		WEB_ORIGIN
	);
	assert.equal(answer.headers.get('access-control-allow-origin'), WEB_ORIGIN);
});

test('connecting again from the same installation replaces its token', async () => {
	const { browser: dev, html } = await signedIn('alice');
	const app = await register(dev, html);
	const { browser } = await signedIn('dave');
	const { verifier, challenge } = pkce();
	const installation = 'inst-' + randomBytes(6).toString('hex');
	const first = await approve(browser, connectPath(app.id, { challenge, installation }));
	const old = (await exchange({ client_id: app.id, code: first.code, code_verifier: verifier }))
		.json;
	const again = await approve(browser, connectPath(app.id, { challenge, installation }));
	const next = (await exchange({ client_id: app.id, code: again.code, code_verifier: verifier }))
		.json;
	assert.equal(next.client_id, old.client_id, 'the same connection');
	assert.notEqual(next.token, old.token);
	assert.equal(await admitted(old.token), 4403);
	assert.equal(await admitted(next.token), 'hello');

	// On the account page, and deleted from there.
	const account = await browser.fetch('/account');
	const page = await account.text();
	assert.match(page, new RegExp(old.client_id));
	const deleted = await browser.submit('/account?/delete', {
		csrf: csrfOf(page),
		client: old.client_id
	});
	assert.equal(deleted.status, 200);
	assert.equal(await admitted(next.token), 4403);
});

test("an app's server issues tokens per user, rotates and deletes them", async () => {
	const { browser, html } = await signedIn('alice');
	const app = await register(browser, html);
	assert.equal((await serverToken(app.id, 'pcs_wrong', { user: 'u1' })).status, 401);
	assert.equal((await serverToken(app.id, app.secret, { user: 'not valid!' })).status, 400);
	const first = await (await serverToken(app.id, app.secret, { user: 'u1' })).json();
	const same = await (await serverToken(app.id, app.secret, { user: 'u1' })).json();
	assert.equal(same.token, first.token, 'one user, one token');
	const other = await (await serverToken(app.id, app.secret, { user: 'u2' })).json();
	assert.notEqual(other.client_id, first.client_id);
	const rotated = await (
		await serverToken(app.id, app.secret, { user: 'u1', rotate: true })
	).json();
	assert.notEqual(rotated.token, first.token);
	assert.equal(await admitted(first.token), 4403);
	assert.equal(await admitted(rotated.token), 'hello');

	const removed = await fetch(`${SITE}/api/apps/${app.id}/users/u1`, {
		method: 'DELETE',
		headers: { authorization: `Bearer ${app.secret}` }
	});
	assert.equal(removed.status, 200);
	assert.equal(await admitted(rotated.token), 4403);

	// The developer's page counts them; the users are not on anyone's account page.
	const page = await (await browser.fetch(`/developers/${app.id}`)).text();
	assert.match(page, /<strong>1<\/strong>\s*users your server issued tokens for/);

	// Rotating the app secret stops the old one at once.
	const rotatedSecret = await browser.submit(`/developers/${app.id}?/rotate`, {
		csrf: csrfOf(page)
	});
	assert.equal(rotatedSecret.status, 200);
	assert.equal((await serverToken(app.id, app.secret, { user: 'u2' })).status, 401);
});

test('a test token, and deleting an app stops its tokens', async () => {
	const { browser, html } = await signedIn('alice');
	const app = await register(browser, html);
	const made = await browser.submit(`/developers/${app.id}?/test`, { csrf: csrfOf(app.html) });
	const token = (await made.text()).match(/data-value="(eyJ[^"]+)"/)?.[1];
	assert.ok(token, 'the test token is on the page');
	assert.equal(await admitted(token), 'hello');
	const issued = await (await serverToken(app.id, app.secret, { user: 'u9' })).json();
	const deleted = await browser.submit(`/developers/${app.id}?/delete`, { csrf: csrfOf(app.html) });
	assert.equal(deleted.status, 303);
	assert.equal(deleted.headers.get('location'), '/developers');
	assert.equal(await admitted(token), 4403);
	assert.equal(await admitted(issued.token), 4403);
	assert.equal((await serverToken(app.id, app.secret, { user: 'u9' })).status, 401);
	assert.equal((await browser.fetch(`/developers/${app.id}`)).status, 404);
});

test("the SDK's own helpers take an app through the same flow", async () => {
	const { browser: dev, html } = await signedIn('alice');
	const app = await register(dev, html);
	const { browser } = await signedIn('bob');
	const request = await connectRequest({
		appId: app.id,
		redirectUri: REDIRECT,
		installation: 'sdk-installation-1',
		label: 'SDK test',
		site: SITE
	});
	const url = new URL(request.url);
	assert.equal(url.origin, SITE);
	const { returnTo, code } = await approve(browser, url.pathname + url.search);
	assert.equal(
		new URL(returnTo.replace(/^[^:]+:/, 'x://h')).searchParams.get('state'),
		request.state
	);
	const connected = await exchangeCode({
		appId: app.id,
		code,
		verifier: request.verifier,
		redirectUri: REDIRECT,
		site: SITE
	});
	assert.equal(connected.login, 'bob');
	assert.equal(await admitted(connected.token), 'hello');
	await assert.rejects(
		exchangeCode({
			appId: app.id,
			code,
			verifier: request.verifier,
			redirectUri: REDIRECT,
			site: SITE
		}),
		/unknown, used or expired/
	);
});
