<script module lang="ts">
	export const toc = [
		{ id: 'concepts', label: 'Concepts' },
		{ id: 'install', label: 'Install' },
		{ id: 'javascript', label: 'JavaScript' },
		{ id: 'rust', label: 'Rust' },
		{ id: 'relay', label: 'Run a relay' },
		{ id: 'limits', label: 'Limits' },
		{ id: 'security', label: 'Security checklist' },
		{ id: 'limitations', label: 'Limitations' }
	];
</script>

<script lang="ts">
	import CodeBlock from '#lib/components/CodeBlock.svelte';
	import { RELAY_URL, REPO_URL } from '#lib/site.ts';
	import {
		binary,
		installJs,
		cloudflare,
		docker,
		jsFull,
		rustDependency,
		rustFull
	} from '#lib/snippets.ts';
</script>

<h1>Get started</h1>
<p class="lead">
	PeerConnect gives two applications a private, reliable channel through a relay neither has to
	trust. This guide covers the ideas you need, the JavaScript and Rust libraries, and running your
	own relay.
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
		Everything a peer needs to reconnect, as a string: <code>room.toString()</code>. It contains
		secrets, so store it like a password. Save it when you create or join a room, after
		<code>paired</code>, and after <code>close()</code>.
	</dd>
	<dt>Connection</dt>
	<dd>
		A live link to the room. It reconnects on its own, and its state is one of
		<code>offline</code> (connecting or waiting to retry), <code>waiting</code> (on the relay, the
		peer is not), <code>connected</code> (end-to-end session established) and
		<code>closed</code>.
	</dd>
	<dt>Delivery</dt>
	<dd>
		While both peers keep running, every message is delivered exactly once and in order, however
		often connections drop. Messages sent while the peer is away are kept by the sender and
		delivered once both are connected.
	</dd>
</dl>

<h2 id="install">Install</h2>
<p>
	The JavaScript SDK is on npm and the Rust crates are on crates.io, all at version 0.1 while the
	protocol is a draft.
</p>
<h3>JavaScript</h3>
<p>
	The JavaScript SDK is the Rust core compiled to WebAssembly, inlined into the package so it loads
	in browsers, Node.js 22+, Deno and Bun without bundler configuration (46 KB gzipped).
</p>
<CodeBlock code={installJs('en')} lang="sh" title="shell" />
<h3>Rust</h3>
<p>
	The crate does no I/O by default: its <code>Peer</code> is a state machine that any WebSocket
	library and event loop can drive. The <code>client</code> feature adds a ready-made Tokio client.
</p>
<CodeBlock code={rustDependency} lang="toml" title="Cargo.toml" />
<p>
	To build from source instead, clone <a href={REPO_URL} rel="external">the repository</a>: its
	README has the steps.
</p>

<h2 id="javascript">JavaScript</h2>
<p>
	One peer creates a room and shows the invite; the other joins with it. Both persist their room
	record so they can reconnect later, from anywhere.
</p>
<CodeBlock code={jsFull('en')} lang="js" title="app.js" />
<h3>Connection events</h3>
<div class="table">
	<table>
		<thead><tr><th>Event</th><th>When</th></tr></thead>
		<tbody>
			<tr
				><td><code>state</code></td><td
					><code>conn.state</code> (and <code>conn.reason</code>) changed.</td
				></tr
			>
			<tr
				><td><code>message</code></td><td
					>A message arrived: authenticated, decrypted, in order. <code>e.data</code> is a
					<code>Uint8Array</code>.</td
				></tr
			>
			<tr
				><td><code>delivered</code></td><td
					>The peer has received every message whose id is below <code>e.detail</code>.</td
				></tr
			>
			<tr
				><td><code>paired</code></td><td
					>Pairing completed; the room moved to its pair ID. Save <code>conn.room.toString()</code> again.</td
				></tr
			>
			<tr
				><td><code>replaced</code></td><td
					>Another connection took this peer’s seat — the same room open elsewhere. The connection
					retries in a minute; call <code>leave()</code> to let the other one keep it.</td
				></tr
			>
		</tbody>
	</table>
</div>
<h3>Methods</h3>
<div class="table">
	<table>
		<thead><tr><th>Call</th><th>Does</th></tr></thead>
		<tbody>
			<tr
				><td><code>Room.create(relay)</code></td><td
					>Creates a room; you are the inviter. <code>room.invite</code> is the invite.</td
				></tr
			>
			<tr
				><td><code>Room.join(invite)</code></td><td
					>Accepts an invite; you are the joiner. Save the record before connecting.</td
				></tr
			>
			<tr><td><code>Room.parse(record)</code></td><td>Restores a saved room.</td></tr>
			<tr
				><td><code>connect(room)</code></td><td
					>Connects and keeps the room connected until left or closed.</td
				></tr
			>
			<tr
				><td><code>conn.send(data)</code></td><td
					>Queues a message (string or bytes, up to 16 MiB) and returns its id. Throws when 64 MiB
					already await acknowledgement, or once the room is closing.</td
				></tr
			>
			<tr
				><td><code>conn.leave()</code></td><td
					>Disconnects, keeping the room for later. Unacknowledged messages are dropped.</td
				></tr
			>
			<tr
				><td><code>conn.close()</code></td><td
					>Ends the room for both peers. The connection carries on until the peer has been told;
					save <code>conn.room</code>, then delete it once closed.</td
				></tr
			>
		</tbody>
	</table>
</div>

<h2 id="rust">Rust</h2>
<p>
	The Tokio client mirrors the JavaScript API: <code>send</code> queues and returns an id,
	<code>next()</code> yields events, and the connection reconnects and resumes by itself.
</p>
<CodeBlock code={rustFull('en')} lang="rust" title="main.rs" />
<p>
	Try it from a terminal with the chat example:
	<code>cargo run -p peerconnect --features client --example chat -- new {RELAY_URL} room-a</code>,
	then <code>… -- join '&lt;invite&gt;' room-b</code> in a second terminal.
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
					>64 MiB; beyond that <code>send</code> throws (JavaScript) or waits (Rust)</td
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
		identity keys (<code>room.publicKey</code> and <code>room.peerKey</code>) — the demo shows one.
	</li>
	<li>Delete a room’s record once the room is closed.</li>
</ul>

<h2 id="limitations">Limitations</h2>
<ul>
	<li>
		<strong>The queue lives in the sender’s memory.</strong> Messages survive disconnections and
		relay restarts, but not the sending process: keep your own copy until you see
		<code>delivered</code> if you need delivery across restarts. A receiver that restarts may get again
		what it received in its last ~50 ms.
	</li>
	<li>
		<strong>Ending a room needs both peers online once more.</strong> <code>close()</code> keeps trying
		until the other peer acknowledges; a peer that never returns is never told.
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
