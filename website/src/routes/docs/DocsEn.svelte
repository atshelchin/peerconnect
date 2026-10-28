<script module lang="ts">
	export const toc = [
		{ id: 'concepts', label: 'Concepts' },
		{ id: 'access', label: 'Get access' },
		{ id: 'client', label: 'Build a client' },
		{ id: 'relay', label: 'Run a relay' },
		{ id: 'limits', label: 'Limits' },
		{ id: 'security', label: 'Security checklist' },
		{ id: 'limitations', label: 'Limitations' }
	];
</script>

<script lang="ts">
	import CodeBlock from '#lib/components/CodeBlock.svelte';
	import { RELAY_URL, REPO_URL } from '#lib/site.ts';
	import { accessSh, binary, cloudflare, docker } from '#lib/snippets.ts';
</script>

<h1>Get started</h1>
<p class="lead">
	PeerConnect gives two applications a private, reliable channel through a relay neither has to
	trust. It is offered as an API: this guide covers the ideas you need, getting access, what a
	client implements, and running your own relay. The <a href="/docs/api">API reference</a> and the
	<a href="/spec">specification</a> have every detail.
</p>

<h2 id="concepts">Concepts</h2>
<dl>
	<dt>Room</dt>
	<dd>
		The relationship between two peers. It is defined by a random room secret and the two peers’
		identity keys — not by anything stored on a relay.
	</dd>
	<dt>Inviter and joiner</dt>
	<dd>
		The <strong>inviter</strong> creates the room and hands out an <strong>invite</strong>; the
		<strong>joiner</strong> accepts it. A room has exactly these two peers.
	</dd>
	<dt>Invite</dt>
	<dd>
		The relay URL, the room secret and the inviter’s public key, as one string such as
		<code>{RELAY_URL}#…</code>. Show it as a QR code or a link. Until pairing completes, whoever
		uses it first becomes the joiner, so pass it on privately.
	</dd>
	<dt>Pairing</dt>
	<dd>
		On first contact the inviter pins the joiner’s identity and both peers move to a
		<strong>pair ID</strong> derived from their two identities. From then on the invite leads nowhere.
	</dd>
	<dt>Room record</dt>
	<dd>
		Everything a peer needs to reconnect: the room secret, its identity key, the peer’s key once
		pinned, and where the two meet (<a href="/spec#sec-3-4">§3.4</a>). It contains secrets, so store
		it like a password. Save it when the room is created or joined, when pairing completes, and when
		the room starts closing.
	</dd>
	<dt>Connection</dt>
	<dd>
		A peer’s live link to the room through the relay. A client reconnects on its own, with backoff,
		and is <em>offline</em> (connecting or waiting to retry), <em>waiting</em> (on the relay, the
		peer is not), <em>connected</em> (end-to-end session established) or <em>closed</em>.
	</dd>
	<dt>Delivery</dt>
	<dd>
		While both peers keep running, every message is delivered exactly once and in order, however
		often connections drop. Messages sent while the peer is away are kept by the sender and
		delivered once both are connected.
	</dd>
</dl>

<h2 id="access">Get access</h2>
<p>
	The public relay, <code>{RELAY_URL}</code>, requires a token on every connection, so that it knows
	which application uses it and can stop abuse; it learns nothing more from it. Each peer brings its
	own token, which is not part of the room or the invite. There are three ways to get one:
</p>
<ol>
	<li>
		<strong>The public trial</strong>, for trying PeerConnect: <code>POST /api/trial</code> answers a
		token that lasts two hours, without an account.
	</li>
	<li>
		<strong>Your users connect your app.</strong> Register it on
		<a href="/developers">the developers page</a> (sign in with GitHub) for an app ID and an app
		secret. People open <code>peerconnect.dev/connect</code> from your app, sign in, and come back with
		a code that your app exchanges for their token (OAuth 2.0 with PKCE).
	</li>
	<li>
		<strong>Your server issues the tokens</strong>, for apps whose users have no reason to see
		PeerConnect: it asks for one per user with the app secret.
	</li>
</ol>
<CodeBlock code={accessSh('en')} lang="sh" title="shell" />
<p>
	A peer connects to <code>{RELAY_URL}/v1/&lt;room ID&gt;?token=&lt;token&gt;</code>. If the relay
	closes the connection with 4403, the token is missing, invalid or revoked: get a new one before
	connecting again. With 4429, wait at least a minute. Every request, answer and error is in the
	<a href="/docs/api">API reference</a>. Your own relay needs no token unless you give it a secret (<a
		href="#binary"><code>PEERCONNECT_TOKEN_SECRET</code></a
	>).
</p>

<h2 id="client">Build a client</h2>
<p>
	There is no SDK to install: your application implements the peer side of the
	<a href="/spec">specification</a>, with any WebSocket library and the usual primitives (Ed25519,
	X25519, ChaCha20-Poly1305, SHA-256). What it takes:
</p>
<ol>
	<li>
		<strong>Rooms and invites</strong> (<a href="/spec#sec-3">§3</a>): the room secret, the room ID
		derived from it, the invite, and the record each peer keeps.
	</li>
	<li>
		<strong>Joining the relay</strong> (<a href="/spec#sec-4">§4</a>): the endpoint, the HELLO
		challenge and the signed JOIN, presence, and the keepalive.
	</li>
	<li>
		<strong>The handshake and pairing</strong> (<a href="/spec#sec-5-1">§5.1</a>–<a
			href="/spec#sec-5-2">5.2</a
		>): <code>Noise_IKpsk1_25519_ChaChaPoly_SHA256</code>, the inviter pinning the joiner, the move
		to the pair ID.
	</li>
	<li>
		<strong>The reliable stream</strong> (<a href="/spec#sec-5-3">§5.3</a>–<a href="/spec#sec-5-5"
			>5.5</a
		>): numbered messages, acknowledgements, resending after a reconnection, and ending a room.
	</li>
	<li>
		<strong>The connection lifecycle</strong> (<a href="/spec#sec-6">§6</a>) and the timers (<a
			href="/spec#sec-7">§7</a
		>).
	</li>
</ol>
<p>
	Check every derivation against the
	<a href={`${REPO_URL}/tree/main/test-vectors`} rel="external">test vectors</a> before talking to
	anyone. The repository's own implementations are there to read and to test against, not to
	install:
	<a href={`${REPO_URL}/tree/main/crates/peerconnect`} rel="external">the Rust core</a>, a state
	machine without I/O (<code>src/peer.rs</code>), and
	<a href={`${REPO_URL}/tree/main/js`} rel="external">the JavaScript one</a>, the same core in
	WebAssembly driven by about 200 lines of JavaScript. For an AI agent writing the client,
	<a href="/llms.txt">/llms.txt</a> has the essentials and
	<a href="/llms-full.txt">/llms-full.txt</a>
	everything.
</p>

<h2 id="relay">Run a relay</h2>
<p>
	A relay forwards frames between the two seats of a room and stores nothing. It checks that each
	connection holds the room’s key without learning it, so it needs no accounts or configuration.
	Anyone may use the public relay <code>{RELAY_URL}</code> for testing; for anything else, run your own.
</p>
<h3 id="cloudflare">Cloudflare Workers</h3>
<p>
	Each room ID gets its own Durable Object, using the WebSocket Hibernation API, so idle rooms cost
	nothing and a misbehaving room can only exhaust itself. Add a custom domain in the Cloudflare
	dashboard to serve it as <code>wss://relay.your-domain</code>.
</p>
<CodeBlock code={cloudflare('en')} lang="sh" title="shell" />
<h3 id="docker">Docker</h3>
<p>
	The image holds a single static binary. Run it behind any proxy that terminates TLS, or use the
	bundled Compose file, which adds Caddy for automatic HTTPS.
</p>
<CodeBlock code={docker('en')} lang="sh" title="shell" />
<h3 id="binary">Binary</h3>
<CodeBlock code={binary('en')} lang="sh" title="shell" />
<div class="table">
	<table>
		<thead><tr><th>Variable</th><th>Default</th><th>Meaning</th></tr></thead>
		<tbody>
			<tr
				><td><code>PEERCONNECT_LISTEN</code></td><td><code>0.0.0.0:8080</code></td><td
					>Address to listen on.</td
				></tr
			>
			<tr
				><td><code>PEERCONNECT_MAX_CONNECTIONS</code></td><td><code>10000</code></td><td
					>Concurrent WebSockets.</td
				></tr
			>
			<tr
				><td><code>PEERCONNECT_MAX_PER_ADDRESS</code></td><td><code>256</code></td><td
					>Concurrent WebSockets per client address (IPv6 counted per /64).</td
				></tr
			>
			<tr
				><td><code>PEERCONNECT_TRUST_PROXY</code></td><td>off</td><td
					>Set to <code>1</code> behind a proxy that appends the client address to
					<code>X-Forwarded-For</code>.</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>Every relay answers <code>GET /healthz</code> for health checks.</p>

<h2 id="limits">Limits</h2>
<div class="table">
	<table>
		<thead><tr><th>What</th><th>Limit</th></tr></thead>
		<tbody>
			<tr><td>Peers per room</td><td>2</td></tr>
			<tr><td>Message size</td><td>16 MiB, sent in frames of up to 64 KiB</td></tr>
			<tr
				><td>Unacknowledged data in flight</td><td>4 MiB (a single larger message may exceed it)</td
				></tr
			>
			<tr
				><td>Messages held for the peer</td><td
					>64 MiB in the reference implementations; beyond that a sender refuses or waits</td
				></tr
			>
			<tr
				><td>Keepalive</td><td
					>a ping every 25 s; a connection that hears nothing for 60 s is re-established</td
				></tr
			>
		</tbody>
	</table>
</div>

<h2 id="security">Security checklist</h2>
<ul>
	<li>
		Store room records like passwords — in the Keychain, the Android Keystore or equivalent
		encrypted storage. Whoever has a record can act as that peer.
	</li>
	<li>Save the joiner’s record before connecting: it holds the identity the inviter will pin.</li>
	<li>
		Send invites over a private channel. Until pairing completes, an invite is a bearer credential.
	</li>
	<li>
		Where pairing the wrong device would be costly, let users compare a short code derived from both
		identity keys (the two identity public keys) — the demo shows one.
	</li>
	<li>Delete a room’s record once the room is closed.</li>
</ul>

<h2 id="limitations">Limitations</h2>
<ul>
	<li>
		<strong>The queue lives in the sender’s memory.</strong> Messages survive disconnections and relay
		restarts, but not the sending process: keep your own copy until the peer has acknowledged it if you
		need delivery across restarts. A receiver that restarts may get again what it received in its last
		~50 ms.
	</li>
	<li>
		<strong>Ending a room needs both peers online once more.</strong> A closing peer keeps trying until
		the other peer acknowledges; a peer that never returns is never told.
	</li>
	<li>
		<strong>Before pairing, the invite is a bearer credential.</strong> Whoever uses it first pairs. The
		race is visible — the rightful joiner never connects — but not prevented.
	</li>
	<li>
		<strong>One relay per room.</strong> If it is down, peers wait. They can agree on another relay URL;
		there is no automatic failover.
	</li>
	<li>
		<strong>The Cloudflare relay</strong> enforces the idle timeout lazily and cannot apply backpressure;
		the 4 MiB window bounds honest peers.
	</li>
	<li>
		<strong>Not independently audited.</strong> The design has had several rounds of adversarial review
		and its test vectors are reproduced by an independent implementation; neither replaces an external
		cryptographic audit.
	</li>
</ul>
<p class="next">
	The full protocol, with its threat model, is in the <a href="/spec">specification</a>.
</p>
