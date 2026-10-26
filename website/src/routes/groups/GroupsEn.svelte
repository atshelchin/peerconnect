<script module lang="ts">
	export const toc = [
		{ id: 'overview', label: 'What groups are for' },
		{ id: 'guarantees', label: 'Guarantees' },
		{ id: 'classes', label: 'Traffic classes' },
		{ id: 'transport', label: 'QUIC only' },
		{ id: 'identities', label: 'Identities and roles' },
		{ id: 'state-transfer', label: 'State transfer' },
		{ id: 'consistency', label: 'Consistency' },
		{ id: 'javascript', label: 'JavaScript' },
		{ id: 'rust', label: 'Rust' },
		{ id: 'engines', label: 'Game engines (C)' },
		{ id: 'relays', label: 'Relays' },
		{ id: 'latency', label: 'Latency' },
		{ id: 'limits', label: 'Limits' },
		{ id: 'limitations', label: 'Limitations' }
	];
</script>

<script lang="ts">
	import CodeBlock from '#lib/components/CodeBlock.svelte';
	import { GROUP_RELAY_URL, REPO_URL } from '#lib/site.ts';
	import {
		cBuild,
		cClient,
		installJs,
		jsGroup,
		jsRealtime,
		jsSync,
		jsVoice,
		rustDependency,
		rustGroup,
		rustMember
	} from '#lib/snippets.ts';
</script>

<h1>Groups</h1>
<p class="lead">
	A room joins exactly two peers. A group joins up to four: a squad in a game, the players of a
	shared world, a small team. Members send each other commands, chat, voice and state, end-to-end
	encrypted, over QUIC, through an untrusted, stateless relay.
</p>

<p class="try"><a href="/demo/voice">Try it: a group voice and video chat in your browser →</a></p>

<h2 id="overview">What groups are for</h2>
<ul>
	<li>
		<strong>Game squads and parties.</strong> Commands for what players do, voice while they play, chat
		in the lobby, and none of it waiting behind the rest.
	</li>
	<li>
		<strong>Shared worlds.</strong> A player who arrives late gets the world as a snapshot from someone
		already there, then every change made since.
	</li>
	<li>
		<strong>Teams and collaborative tools.</strong> Everyone sees each author’s changes in that author’s
		order, and the application decides who may change what, by member ID.
	</li>
</ul>
<p>
	Groups are a protocol of their own, <a href="/spec/groups">PeerConnect Groups</a>, on their own
	relay endpoint (<code>/g1/&lt;group id&gt;</code>). They travel over QUIC only, and the Linux
	relay serves them next to rooms (<a href="#relays">Relays</a>).
</p>
<dl>
	<dt>Group</dt>
	<dd>
		Everyone holding one 32-byte <strong>group secret</strong>. It is not an object on the relay: it
		exists there only while members are connected.
	</dd>
	<dt>Invite</dt>
	<dd>
		The relay URL and the secret, as one string: <code>https://relay…#…</code>. It is the whole
		credential — no pairing, nothing else to store. Whoever holds it can join, read everything sent
		to the group and send under their own identity, so give it only to members, over a private
		channel. A game server that forms a squad would typically create the group and hand the invite
		to each player.
	</dd>
	<dt>Member</dt>
	<dd>
		A participant, known by the public key of its identity: its <strong>member ID</strong>.
	</dd>
	<dt>Slot</dt>
	<dd>
		The number, 0–3, that the relay gives a connection while it is in the group. The relay tells
		every member who is present. A group has 4 slots, so at most four members.
	</dd>
</dl>

<h2 id="guarantees">Guarantees</h2>
<ul>
	<li>
		<strong>The relay cannot read, forge or replay.</strong> Every frame is encrypted with a fresh per-connection
		sender key that its sender’s identity signs for, and members admit each other only after echoing a
		fresh random challenge, so a relay cannot replay an earlier session. The relay sees the group ID,
		slots, frame sizes, classes and timing — never contents or identities.
	</li>
	<li>
		<strong>Commands and chat are reliable streams.</strong> Messages are numbered, kept by their sender
		and resent after reconnections. While sender and receiver keep running, each message reaches every
		member present, the sender included, exactly once and in the sender’s order. Messages sent while offline
		go out on reconnection.
	</li>
	<li>
		<strong>Voice and datagrams are real-time.</strong> They are dropped rather than delayed, wherever
		they would wait: by the sender when its connection backs up, by the relay when a recipient is behind.
		Late audio is worse than lost audio.
	</li>
	<li>
		<strong>Urgent traffic goes first.</strong> A less urgent class never holds up a more urgent one,
		at the sender, in the relay or in the encryption.
	</li>
	<li>
		<strong>Newcomers catch up exactly.</strong> State transfer delivers a snapshot, then precisely the
		commands it lacks, none twice.
	</li>
	<li>
		<strong>Nothing is stored on the relay.</strong> It keeps no state beyond its open connections: the
		world lives in the members’ applications.
	</li>
</ul>

<h2 id="classes">Traffic classes</h2>
<p>
	A frame’s type decides its class, and that is all the relay knows about it. Each class has its own
	queue at the sender and, for every recipient, in the relay, its own QUIC stream, and its own
	counters in the encryption. From most to least urgent:
</p>
<div class="table">
	<table>
		<thead><tr><th>Class</th><th>Delivery</th><th>Carries</th></tr></thead>
		<tbody>
			<tr
				><td><strong>Commands</strong></td><td>reliable, ordered, resent</td><td
					>Actions that change shared state: placing a block, opening a door, an edit. Key exchange
					and control travel here too.</td
				></tr
			>
			<tr
				><td><strong>Voice</strong></td><td>real-time: dropped rather than delayed</td><td
					>Voice frames, and datagrams such as positions and poses, where only the latest value
					matters.</td
				></tr
			>
			<tr
				><td><strong>Chat</strong></td><td>reliable, ordered, resent</td><td
					>Text chat, and anything else that may wait for commands and voice.</td
				></tr
			>
			<tr
				><td><strong>Video</strong></td><td>real-time: dropped rather than delayed</td><td
					>Video frames, cut into fragments that each fit a datagram and put back together whole; a
					lost fragment is resent once, and a frame lost for good brings a keyframe. After chat,
					which it could otherwise starve.</td
				></tr
			>
			<tr
				><td><strong>Bulk</strong></td><td>reliable, flow-controlled end to end</td><td
					>State transfer snapshots.</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>
	So a chat flood or a 50 MB snapshot never delays a command, voice never waits behind chat, and
	video never starves chat. In the browser, <code>startVideo</code> from
	<code>peerconnect/video</code> sends the camera through WebCodecs and draws each member into a canvas.
	Video goes in layers (simulcast): each member receives, of each other’s video, the layer that fits the
	canvas it is drawn in (the large picture for a large tile, a small one for a thumbnail, nothing for
	a hidden one), the relay switches layers at a keyframe so the picture never stops, and senders encode
	only the layers someone wants. Each message on the command and chat streams carries up to 16,352 bytes.
</p>

<h2 id="transport">QUIC only</h2>
<p>
	Groups travel over QUIC and nothing else: WebTransport (HTTP/3) in browsers, raw QUIC (ALPN
	<code>peerconnect-g1</code>) in Rust and the C library. Each class has a stream of its own, and
	voice and video travel as datagrams, so a lost packet holds up only its own class and voice never
	waits for a retransmission. Over TCP, one lost packet holds up voice, video and commands alike
	until it is resent.
</p>
<p>
	The difference is large. With 3% of packets lost on each hop, voice p99 was 138 ms over a
	WebSocket and 53 ms over QUIC. In a browser with 2% lost each way, speech reached the other side
	at p99 460–1170 ms over a WebSocket and 105–120 ms over WebTransport. So there is no WebSocket
	fallback: a member that cannot reach the relay over UDP does not join, rather than join with voice
	and video that stutter.
</p>
<h3 id="who-can-join">Who cannot join</h3>
<ul>
	<li>
		<strong>Browsers without WebTransport.</strong> <code>groupsSupported()</code> is
		<code>false</code> there, and <code>joinGroup</code> throws a <code>"NotSupportedError"</code>
		<code>DOMException</code>. Check before you offer a group, and suggest a recent Chrome, Edge or
		Firefox. The same holds for runtimes without WebTransport, such as Node.js: use the Rust client
		there.
	</li>
	<li>
		<strong>Networks that block UDP.</strong> Some company, school and hotel networks let only TCP out.
		The member needs another network.
	</li>
	<li>
		<strong>Browsers behind an HTTP or SOCKS proxy</strong> — a system proxy, Clash and the like. QUIC
		cannot go through such a proxy. Add the relay’s host to the proxy’s exceptions (its bypass list),
		so that the browser reaches the relay directly.
	</li>
</ul>
<p>
	A member that cannot connect stays offline and keeps trying, so it joins as soon as the way is
	open.
	<code>conn.connected</code> (JavaScript), <code>conn.connected()</code> (Rust) and
	<code>pc_client_connected</code> (C) say whether a connection to the relay is open: when it stays false,
	tell the user what may be in the way.
</p>

<h2 id="identities">Identities and roles</h2>
<p>
	Every member has an <strong>identity</strong>: an Ed25519 key pair that your application chooses
	and keeps — one per player account, say. Persist its seed: it is how the others recognise the
	member after a restart. One identity may join many groups. The relay never sees it, and the token
	the relay does see is unlinkable from one group to the next.
</p>
<p>
	When a <code>joined</code> event names a member, that member’s key has signed for its connection,
	and every frame attributed to it was encrypted with that connection’s key. A member cannot pass
	itself off as another. Its <strong>profile</strong> — a display name, an avatar, up to 1,024 bytes —
	rides along, but it is only what the member claims; the ID is what it proves.
</p>
<p>
	<strong>Roles are the application’s.</strong> Anyone with the invite can join and send anything. Keep
	lists of member IDs — the owners, the host, the moderators — and check the sender of every command against
	them. To exclude someone, move the group to a new secret.
</p>
<p>
	An identity holds at most one slot. When it connects again, after a network change or from a
	second device, the new connection takes over the slot and the old one receives
	<code>replaced</code>.
</p>

<h2 id="state-transfer">State transfer</h2>
<p>
	A member that arrives late, or that missed too much, asks a member already there for a <strong
		>snapshot</strong
	>: your application’s bytes, such as a saved world, a document or a game state.
</p>
<ol>
	<li>
		The newcomer calls <code>requestSync(member)</code>. From then on, its SDK holds back the live
		commands it receives instead of delivering them.
	</li>
	<li>
		The member’s application receives <code>syncrequest</code> and takes its snapshot
		<strong>in that handler</strong>, before handling the next event. At that moment the SDK records
		the <strong>frontier</strong>: exactly which commands of every stream the application has
		applied. The snapshot itself may be sent later with <code>provideSync</code> — after compressing it
		on a worker thread, say.
	</li>
	<li>
		The snapshot travels in the bulk class, after everything else, and the sender paces it by the
		delay it adds to the path (50 ms at most by default), so voice stays smooth while it loads.
	</li>
	<li>
		The newcomer’s application receives <code>snapshot</code> and replaces its state with it. The commands
		that follow are exactly those the snapshot lacks, including any the provider had not yet received
		when it took the snapshot. Those it already contains are dropped.
	</li>
</ol>
<p>
	The result: the snapshot followed by the commands delivered after it is exactly what the provider
	had applied, followed by everything since. If the provider leaves mid-transfer, or nothing moves
	for 15 s, the newcomer receives <code>syncfailed</code> and the commands it held, and can ask another
	member. Chat is not held back: it is not part of the state.
</p>

<h2 id="consistency">Consistency</h2>
<p>
	Each member’s stream arrives everywhere in that member’s order, but two members’ streams can
	interleave differently at different receivers. If Alice and Bob move the same block at the same
	moment, two members may disagree about where it ended up. The relay could impose one order, but it
	is untrusted: it could just as well show different orders to different members. So when your
	commands do not commute, add a rule on top. Two patterns cover most applications.
</p>
<h3>Owner per object</h3>
<p>
	Give every object one owner, a member ID, and apply a command only if it comes from the owner of
	what it changes. Concurrent commands then touch different objects and commute: every member ends
	up with the same state, however the streams interleave. It needs no host and works with anyone
	offline, which suits building games where players mostly change their own things, and
	collaborative tools with an owner per section. Handing an object over is itself a command from its
	current owner.
</p>
<h3>Host authority</h3>
<p>
	One member is the <strong>host</strong>. The others send it requests, with
	<code>sendTo(host, "command", …)</code>; the host checks each one, applies it and broadcasts the
	result as one of its own commands. Everyone, the host included, applies only the host’s stream:
	one stream, one order, nothing to reconcile. State transfer from the host copies its state
	exactly.
</p>
<p>
	This is what most games do with a server, with the host as the server. It is the right choice when
	the simulation is not deterministic (physics engines, real-time clocks), since then only one
	member can run it. Choose the host either once — the world’s creator, with the world read-only
	while it is away — or deterministically: among the members present that may host, the one with the
	smallest member ID, which every member computes alike from <code>joined</code> and
	<code>left</code>. When the host changes, every member asks the new host for a snapshot, which
	brings everyone exactly level with it.
</p>
<h3>Things that move</h3>
<p>
	Positions, poses and animation states change many times a second, and only the latest value
	matters. Send them as <strong>datagrams</strong>, 10–20 a second for each moving thing, each
	carrying the full current value and a sequence number or time. Receivers keep the newest and
	interpolate; a lost datagram costs nothing, because the next one replaces it. Do not send them as
	commands: after a stall, a reliable stream would deliver every stale position in order, in front
	of the commands that matter.
</p>
<p>
	Lockstep, where every member applies everyone’s commands in an agreed order tick by tick, needs a
	simulation deterministic to the bit and stalls on the slowest member. Outside fighting and
	real-time strategy games, prefer one of the patterns above.
</p>

<h2 id="javascript">JavaScript</h2>
<p>
	Groups are in the same package as rooms. They travel over WebTransport, so they need a runtime
	that has it: check <code>groupsSupported()</code> first (<a href="#who-can-join"
		>Who cannot join</a
	>). Browser voice is a separate entry point, <code>peerconnect/voice</code>. A group connection
	reconnects on its own until you call <code>leave()</code>.
</p>
<CodeBlock code={installJs('en')} lang="sh" title="shell" />
<h3>Join and send</h3>
<CodeBlock code={jsGroup('en')} lang="js" title="squad.js" />
<h3>Positions and raw audio</h3>
<CodeBlock code={jsRealtime('en')} lang="js" title="realtime.js" />
<h3>Bringing a newcomer up to date</h3>
<CodeBlock code={jsSync('en')} lang="js" title="sync.js" />
<h3>Voice in the browser</h3>
<p>
	<code>startVoice</code> captures the microphone with the browser’s echo cancellation, noise
	suppression and gain control, encodes it as Opus through WebCodecs, sends it only while you speak,
	and plays each other member through an adaptive jitter buffer of its own, which drops frames to
	catch up after a stall rather than playing late. Each frame is <code>hold[1] ‖ Opus packet</code> (48
	kHz mono, 20 ms frames by default), the same format native clients use, so browsers and games hear each
	other.
</p>
<CodeBlock code={jsVoice('en')} lang="js" title="voice.js" />
<div class="table">
	<table>
		<thead><tr><th>Option</th><th>Default</th><th>Meaning</th></tr></thead>
		<tbody>
			<tr><td><code>bitrate</code></td><td><code>32000</code></td><td>Opus bits per second.</td></tr
			>
			<tr
				><td><code>redundancy</code></td><td><code>2</code></td><td
					>Copies of the previous frames carried in each packet (0–2), so that a lost packet is
					recovered from the next one. Receivers that do not use them play the packet as before.</td
				></tr
			>
			<tr
				><td><code>frameMs</code></td><td><code>20</code></td><td
					>10, 20, 40 or 60 ms. Longer frames send fewer packets and add latency.</td
				></tr
			>
			<tr><td><code>vad</code></td><td><code>true</code></td><td>Send only while speaking.</td></tr>
			<tr
				><td><code>threshold</code></td><td><code>-50</code></td><td
					>Level in dBFS above which the microphone counts as speech.</td
				></tr
			>
			<tr
				><td><code>minDelay</code>, <code>maxDelay</code></td><td
					><code>30</code>, <code>250</code></td
				><td>Bounds of the jitter buffer, in ms.</td></tr
			>
		</tbody>
	</table>
</div>
<p>
	It needs WebCodecs audio with Opus, and rejects where that is missing (it is tested in Chromium).
	Headphones avoid echo entirely.
</p>
<h3>Events</h3>
<div class="table">
	<table>
		<thead><tr><th>Event</th><th>When</th></tr></thead>
		<tbody>
			<tr
				><td><code>state</code></td><td
					><code>conn.state</code> changed: <code>offline</code>, <code>online</code> or
					<code>closed</code>.</td
				></tr
			>
			<tr
				><td><code>joined</code></td><td
					>A member was admitted, its identity proven: <code>member</code>,
					<code>profile</code>.</td
				></tr
			>
			<tr><td><code>left</code></td><td>A member left.</td></tr>
			<tr
				><td><code>message</code></td><td
					>A command or chat message, in its stream’s order, our own included; <code>direct</code> if
					it was sent to us alone.</td
				></tr
			>
			<tr
				><td><code>voice</code>, <code>datagram</code></td><td
					>Real-time data from <code>from</code>; voice carries its <code>seq</code>.</td
				></tr
			>
			<tr
				><td><code>missed</code></td><td
					><code>count</code> messages of a member’s stream were lost: the sender no longer had them.</td
				></tr
			>
			<tr
				><td><code>syncrequest</code></td><td
					>A member asks for a snapshot. Take it in the handler, then <code>provideSync</code> or
					<code>declineSync</code>.</td
				></tr
			>
			<tr
				><td><code>snapshot</code></td><td
					>The snapshot asked for: replace the application state with it.</td
				></tr
			>
			<tr
				><td><code>syncfailed</code></td><td
					>The transfer failed; the held commands follow. Ask another member.</td
				></tr
			>
			<tr
				><td><code>replaced</code></td><td
					>Our identity joined from elsewhere and took our slot. We retry in 30–60 s; call
					<code>leave()</code> to stop.</td
				></tr
			>
			<tr><td><code>full</code></td><td>The group is full on this relay; we retry later.</td></tr>
		</tbody>
	</table>
</div>
<h3>Methods</h3>
<div class="table">
	<table>
		<thead><tr><th>Call</th><th>Does</th></tr></thead>
		<tbody>
			<tr
				><td><code>Group.create(relay)</code></td><td
					>Creates a group on an <code>https://</code> or <code>quic://</code> relay; it throws for
					<code>ws://</code> and <code>wss://</code>. <code>group.toString()</code> is its invite.</td
				></tr
			>
			<tr><td><code>Group.parse(invite)</code></td><td>Reads an invite.</td></tr>
			<tr
				><td><code>groupsSupported()</code></td><td
					>Whether this runtime can join groups, that is, has WebTransport.</td
				></tr
			>
			<tr
				><td><code>Identity.generate()</code>, <code>Identity.fromSeed(seed)</code></td><td
					>A member identity; persist <code>seed</code>. <code>id</code> is the member ID.</td
				></tr
			>
			<tr
				><td><code>joinGroup(group, options)</code></td><td
					>Joins over WebTransport with <code>identity</code> and <code>profile</code>, and stays in
					the group until left; <code>webTransport</code> is passed to the <code>WebTransport</code>
					constructor. Throws a <code>"NotSupportedError"</code> where there is no WebTransport.</td
				></tr
			>
			<tr
				><td><code>conn.send(channel, data)</code></td><td
					>Broadcasts on <code>"command"</code> or <code>"chat"</code> and returns the message id.</td
				></tr
			>
			<tr
				><td><code>conn.sendTo(member, channel, data)</code></td><td
					>Sends to one member, in order with that channel; not resent if the connection drops.</td
				></tr
			>
			<tr
				><td><code>conn.sendVoice(seq, data)</code>, <code>conn.sendDatagram(data, to?)</code></td
				><td>Real-time data; <code>false</code> if it was dropped.</td></tr
			>
			<tr><td><code>conn.requestSync(member)</code></td><td>Asks a member for a snapshot.</td></tr>
			<tr
				><td
					><code>conn.provideSync(to, transfer, snapshot)</code>,
					<code>conn.declineSync(to, transfer)</code></td
				><td>Answers a <code>syncrequest</code>.</td></tr
			>
			<tr
				><td
					><code>conn.members</code>, <code>conn.id</code>, <code>conn.rtt</code>,
					<code>conn.connected</code></td
				><td
					>The admitted members (ID → profile), our member ID, the latest round trip to the relay,
					and whether a WebTransport session to it is open.</td
				></tr
			>
			<tr><td><code>conn.leave()</code></td><td>Leaves the group.</td></tr>
		</tbody>
	</table>
</div>

<h2 id="rust">Rust</h2>
<p>
	The <code>client</code> feature adds <code>client::group</code>, a Tokio driver. The connection
	connects over raw QUIC and dereferences to a <code>GroupSender</code>;
	<code>conn.sender()</code> gives you a clone of it to send from other tasks, and
	<code>conn.connected()</code> says whether a connection to the relay is open.
</p>
<CodeBlock code={rustDependency} lang="toml" title="Cargo.toml" />
<CodeBlock code={rustGroup('en')} lang="rust" title="main.rs" />
<h3>Your own event loop</h3>
<p>
	Without the <code>client</code> feature the crate does no I/O. <code>group::Member</code> is the
	whole member as a state machine, for an application with a QUIC stack of its own: you feed it what
	happens on one connection at a time and carry out what it asks for. It frames nothing itself: each
	class goes on a stream of its own as length-prefixed messages, voice and video as datagrams where
	they fit, and <code>ping</code> and <code>pong</code> are the empty message (<a
		href="/spec/groups#sec-3-6">§3.6</a
	>). <code>group::transport::target(url)</code> gives the host, port and path to connect to, and whether
	to verify the certificate. Times are milliseconds on any monotonic clock.
</p>
<CodeBlock code={rustMember('en')} lang="rust" title="engine.rs" />
<p>
	A terminal group chat to try it with:
	<code
		>cargo run -p peerconnect --features client --example group -- new quic://localhost:8080 alice</code
	>, then <code>… -- join '&lt;invite&gt;' bob</code> in a second terminal.
</p>

<h2 id="engines">Game engines (C)</h2>
<p>
	<code>peerconnect-ffi</code> is the C library, for Godot (through GDExtension), Unity, Unreal and
	any language with a C FFI. <code>pc_client_join</code> joins the group and connects over QUIC by
	itself, on two threads of the library’s own, reconnecting as needed. Your game loop calls
	<code>pc_client_poll</code> once per frame for events, which never blocks, and the
	<code>pc_client_send</code> functions whenever it likes.
</p>
<CodeBlock code={cBuild('en')} lang="sh" title="shell" />
<CodeBlock code={cClient('en')} lang="c" title="game.c" />
<p>
	Use a client from one thread at a time. Pointers in a <code>pc_output</code> are valid until the
	next call on that client, so copy what you keep. <code>pc_client_connected</code> is 1 while a
	QUIC connection to the relay is open; where UDP does not get through, the client stays offline and
	keeps trying. Voice frames interoperate with browsers when they use the SDK’s format,
	<code>hold[1] ‖ Opus packet</code>.
</p>
<p>
	For an application with a QUIC stack of its own, <code>pc_member_*</code> is the state machine
	alone, without I/O or threads: <code>PC_CONNECT</code> gives an <code>https://</code> or
	<code>quic://</code> URL to open, <code>PC_SEND</code> a message for a class’s stream, and
	<code>PC_PING</code> means send the empty message; pass <code>"pong"</code> to
	<code>pc_member_handle_text</code> when the relay’s empty message arrives. Frame messages as
	<a href="/spec/groups#sec-3-6">§3.6</a>
	says. Engines should use <code>pc_client</code>.
</p>

<h2 id="relays">Relays</h2>
<p>
	Groups are served by the Linux relay — the binary, its Docker image or a systemd service — next to
	rooms: rooms over WebSockets on its TCP port, groups over QUIC on the UDP port of the same number.
	That UDP port must be open, in the firewall and in Docker (<code>-p 8080:8080/udp</code>). See
	<a href="/docs#relay">Run a relay</a>{#if REPO_URL}, and the
		<a href="{REPO_URL}/blob/main/deploy/README.md" rel="external">deployment guide</a>, which has a
		setup for each case{/if}.
</p>
<p>The relay URL in the invite says how members reach the relay:</p>
<ul>
	<li>
		<code>https://relay.example.com</code>: WebTransport in browsers and raw QUIC elsewhere, on UDP
		443, with the relay’s certificate verified. The relay needs a certificate browsers trust (<code
			>PEERCONNECT_TLS_CERT</code
		>
		and <code>PEERCONNECT_TLS_KEY</code>).
	</li>
	<li>
		<code>quic://203.0.113.9:8080</code>: no domain and no certificate needed. Native clients use
		raw QUIC without checking the relay’s certificate; browsers use WebTransport, pinning the weekly
		certificate such a relay serves, which anyone can compute (<a href="/spec/groups#sec-3-7"
			>§3.7</a
		>). Members do not rely on the relay for secrecy either way.
	</li>
</ul>
<p>
	<code>ws://</code> and <code>wss://</code> are not group relay URLs:
	<code>Group.create("wss://…")</code> throws. The Cloudflare Worker serves rooms only: Workers
	cannot accept QUIC, and could neither drop late voice nor put commands first. A domain behind
	Cloudflare must not be proxied (the orange cloud), since Cloudflare’s proxy does not carry QUIC to
	your server. The public group relay <code>{GROUP_RELAY_URL}</code>, which the
	<a href="/demo/voice">live demo</a>
	uses, is there for testing.
</p>
<p>
	The relay admits four members per group, the protocol’s limit;
	<code>PEERCONNECT_MAX_GROUP_MEMBERS</code> can lower it. It keeps a queue per class for every recipient.
	It drops voice for a recipient that has fallen behind, holds back for at most 5 s a sender whose commands
	or chat a recipient cannot keep up with (then closes the stuck recipient), and caps what each connection
	sends at 2 MiB/s, in bursts of up to 4 MiB.
</p>

<h2 id="latency">Latency and load</h2>
<p>
	Measured on one Apple M2 Pro with the game load test, over WebSockets before groups became
	QUIC-only (the relay’s queues and fan-out are the same; on Linux, QUIC costs the relay about 1.8
	times the CPU): every player is a real end-to-end encrypted client, in groups of three, sending
	its position 20 times a second, a command twice a second and a chat line every 10 s, two players
	per group talking at once, and a newcomer in every group loading a 2 MB snapshot halfway through.
	Latency is from the sender’s <code>send</code> to the receiver’s event.
</p>
<div class="table">
	<table>
		<thead
			><tr
				><th>Load (Linux relay)</th><th>Commands p50 / p99</th><th>Voice p50 / p99</th><th
					>Voice p99 while snapshots load</th
				><th>Relay</th></tr
			></thead
		>
		<tbody>
			<tr
				><td>34 groups, 102 players</td><td>0.38 / 0.91 ms</td><td>0.31 / 0.84 ms</td><td>6.5 ms</td
				><td>0.23 cores, 13 MB</td></tr
			>
			<tr
				><td>167 groups, 501 players</td><td>0.67 / 2.6 ms</td><td>0.56 / 2.4 ms</td><td>1.1 ms</td
				><td>1.0 cores, 29 MB</td></tr
			>
			<tr
				><td>334 groups, 1,002 players</td><td>1.1 / 38 ms</td><td>0.87 / 30 ms</td><td>58 ms</td
				><td>2.3 cores, 50 MB</td></tr
			>
		</tbody>
	</table>
</div>
<p>
	Everything reliable arrived exactly once, and so did the voice, the positions and every snapshot.
	At 334 groups the machine is saturated, with the load generator running on it too.
</p>
<p>
	Real networks, emulated: each player behind its own access link with a deep buffer, three players
	and a newcomer loading 2 MB.
</p>
<div class="table">
	<table>
		<thead
			><tr
				><th>Link per player</th><th>Voice p50</th><th>Voice p99</th><th
					>Voice p99 while the snapshot loads</th
				><th>Snapshot</th></tr
			></thead
		>
		<tbody>
			<tr
				><td>20 Mbit/s, 20 ms each way</td><td>43 ms</td><td>47 ms</td><td>59 ms</td><td>2.1 s</td
				></tr
			>
			<tr
				><td>2 Mbit/s, 40 ms each way</td><td>84 ms</td><td>89 ms</td><td>144 ms</td><td>10.4 s</td
				></tr
			>
			<tr
				><td>1 Mbit/s, 60 ms each way</td><td>125 ms</td><td>131 ms</td><td>197 ms</td><td>21 s</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>
	The path’s own delay is the floor; the relay and the SDK add about a millisecond. State transfer
	paces itself by the delay it adds: with a fixed window instead, voice p99 on the 2 Mbit/s link was
	920 ms while the snapshot loaded.
</p>
<div class="table">
	<table>
		<thead><tr><th>Browser voice (Chromium, 20 ms Opus frames)</th><th>Result</th></tr></thead>
		<tbody>
			<tr><td>End of capture to start of playout</td><td>p50 18–29 ms</td></tr>
			<tr
				><td>Through a link adding 0–60 ms of jitter each way</td><td
					>p50 110–140 ms; the jitter buffer adapts to about 120 ms; nothing lost</td
				></tr
			>
			<tr
				><td>After a 1 s stall</td><td
					>back to p50 50–70 ms within 2 s (frames dropped to catch up, not played late)</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>
	Mouth to ear adds the 20 ms frame being captured and the audio hardware’s own latency (about 10–30
	ms), plus the network.
</p>

<h2 id="limits">Limits</h2>
<div class="table">
	<table>
		<thead><tr><th>What</th><th>Limit</th></tr></thead>
		<tbody>
			<tr
				><td>Members per group</td><td>4 (4 slots); relays turn away more with close code 4429</td
				></tr
			>
			<tr><td>Command or chat message</td><td>16,352 bytes</td></tr>
			<tr><td>Profile</td><td>1,024 bytes</td></tr>
			<tr
				><td>Messages kept for resending</td><td>the last 8,192 messages or 8 MiB, per channel</td
				></tr
			>
			<tr
				><td>Snapshot</td><td
					>16 KiB – 1 MiB in flight, paced by the delay it adds; up to 64 MiB accepted by default</td
				></tr
			>
			<tr
				><td>Keepalive</td><td
					>a ping every 10 s; a connection that hears nothing for 25 s is re-established</td
				></tr
			>
			<tr
				><td>Gaps in a stream</td><td
					>asked for again after 3 s; given up after 15 s and reported as <code>missed</code></td
				></tr
			>
			<tr><td>Stalled state transfer</td><td>fails after 15 s</td></tr>
		</tbody>
	</table>
</div>

<h2 id="limitations">Limitations</h2>
<ul>
	<li>
		<strong>Some members cannot join.</strong> Groups need UDP to the relay and, in browsers,
		WebTransport, with no fallback: networks that block UDP, browsers behind an HTTP or SOCKS proxy
		(unless the relay’s host is in its exceptions) and browsers without WebTransport stay out (<a
			href="#who-can-join">Who cannot join</a
		>).
	</li>
	<li>
		<strong>No order between members.</strong> The relay does not order one member’s commands
		against another’s. Applications whose commands do not commute need a rule on top (<a
			href="#consistency">Consistency</a
		>).
	</li>
	<li>
		<strong>Any member can read and send everything.</strong> Roles are the application’s to enforce,
		by member ID. Excluding someone means moving the group to a new secret.
	</li>
	<li>
		<strong>No forward secrecy against the group secret.</strong> Sender keys are fresh per connection,
		but they travel under a key derived from the group secret: someone who records the traffic and later
		learns the secret can read it. Rotate the secret when that matters.
	</li>
	<li>
		<strong>Streams live in memory.</strong> A sender that restarts starts a new stream without its old
		messages, and a receiver that restarts starts afresh: ask for a snapshot.
	</li>
	<li>
		<strong>A <code>quic://</code> relay is not authenticated.</strong> Anyone on the path can pose
		as it and learn what a relay learns, though not read or forge anything members send. Use an
		<code>https://</code> relay where that matters.
	</li>
	<li>
		<strong>Browser voice</strong> needs WebCodecs audio with Opus, and is tested in Chromium. Echo cancellation
		is the browser’s.
	</li>
	<li>
		<strong>Not independently audited.</strong> The design has had several rounds of adversarial review,
		and its test vectors are reproduced by an implementation written from the specification alone; neither
		replaces an external cryptographic audit.
	</li>
</ul>
<p class="next">
	The full protocol is in the <a href="/spec/groups">groups specification</a>. For two-peer rooms,
	see <a href="/docs">Get started</a>.
</p>
