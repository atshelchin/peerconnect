// A stand-in for GitHub's OAuth web flow, for the site's end-to-end tests.
// Authorize always approves (as whoever `?as=` names, alice by default), and
// checks that the client ID and secret are the test's.
import { createServer } from 'node:http';

export function fakeGitHub({ clientId, clientSecret, port = 0 }) {
	const users = {
		alice: { id: 1001, login: 'alice' },
		bob: { id: 1002, login: 'bob' },
		carol: { id: 1003, login: 'carol' },
		dave: { id: 1004, login: 'dave' }
	};
	let next = 'alice';
	const server = createServer(async (req, res) => {
		const url = new URL(req.url, 'http://localhost');
		// Test control: who the next sign-in is.
		if (url.pathname.startsWith('/_as/')) {
			next = url.pathname.slice(5);
			return res.writeHead(204).end();
		}
		if (url.pathname === '/login/oauth/authorize') {
			if (url.searchParams.get('client_id') !== clientId)
				return res.writeHead(400).end('bad client_id');
			const back = new URL(url.searchParams.get('redirect_uri'));
			back.searchParams.set('code', `code-${next}`);
			back.searchParams.set('state', url.searchParams.get('state'));
			return res.writeHead(302, { location: back.toString() }).end();
		}
		if (url.pathname === '/login/oauth/access_token' && req.method === 'POST') {
			let body = '';
			for await (const chunk of req) body += chunk;
			const { client_id, client_secret, code } = JSON.parse(body);
			if (client_id !== clientId || client_secret !== clientSecret) {
				return res
					.writeHead(401, { 'content-type': 'application/json' })
					.end(JSON.stringify({ error: 'bad_client' }));
			}
			return res
				.writeHead(200, { 'content-type': 'application/json' })
				.end(JSON.stringify({ access_token: `token-${code.replace('code-', '')}` }));
		}
		if (url.pathname === '/user') {
			const who = (req.headers.authorization ?? '').replace('Bearer token-', '');
			const user = users[who];
			if (!user) return res.writeHead(401).end();
			return res
				.writeHead(200, { 'content-type': 'application/json' })
				.end(JSON.stringify({ ...user, avatar_url: `https://avatars.example/${user.id}` }));
		}
		res.writeHead(404).end();
	});
	return new Promise((resolve) =>
		server.listen(port, '127.0.0.1', () =>
			resolve({
				url: `http://127.0.0.1:${server.address().port}`,
				signInAs: (who) => (next = who),
				close: () => server.close()
			})
		)
	);
}

// Run on its own: `node fake-github.mjs <port> <client id> <client secret>`.
if (import.meta.url === `file://${process.argv[1]}`) {
	const [port, clientId, clientSecret] = process.argv.slice(2);
	const gh = await fakeGitHub({ clientId, clientSecret, port: Number(port) });
	console.log(`fake GitHub on ${gh.url}`);
}
