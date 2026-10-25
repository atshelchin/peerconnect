<script module lang="ts">
	export const toc = [
		{ id: 'overview', label: 'What groups are for' },
		{ id: 'guarantees', label: 'Guarantees' },
		{ id: 'classes', label: 'Traffic classes' },
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
	import {
		cBuild,
		cMember,
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
	A room joins exactly two peers. A group joins up to 32: a squad in a game, the players of a shared
	world, a team. Members send each other commands, chat, voice and state, end-to-end encrypted,
	through the same untrusted, stateless relays.
</p>

<p class="try"><a href="/demo/voice">Try it: talk in a group voice chat in your browser →</a></p>

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
	relay endpoint (<code>/g1/&lt;group id&gt;</code>). The reference relays serve groups and rooms
	side by side.
</p>
<dl>
	<dt>Group</dt>
	<dd>
		Everyone holding one 32-byte <strong>group secret</strong>. It is not an object on the relay: it
		exists there only while members are connected.
	</dd>
	<dt>Invite</dt>
	<dd>
		The relay URL and the secret, as one string: <code>wss://relay…#…</code>. It is the whole
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
		The number, 0–31, that the relay gives a connection while it is in the group. The relay tells
		every member who is present. A group has 32 slots; the reference relays admit 10 members by
		default.
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
		they would wait: by the sender when its socket backs up, by the relay when a recipient is behind.
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
	queue at the sender and, for every recipient, in the relay, and its own counters in the
	encryption. From most to least urgent:
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
				><td><strong>Bulk</strong></td><td>reliable, flow-controlled end to end</td><td
					>State transfer snapshots.</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>
	So a chat flood or a 50 MB snapshot never delays a command, and voice never waits behind chat.
	Each message on the command and chat streams carries up to 16,352 bytes.
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
	Groups are in the same package as rooms, for browsers, Node.js 22+, Deno and Bun. Browser voice is
	a separate entry point, <code>peerconnect/voice</code>. A group connection reconnects on its own
	until you call <code>leave()</code>.
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
			<tr><td><code>bitrate</code></td><td><code>24000</code></td><td>Opus bits per second.</td></tr
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
					>Creates a group. <code>group.toString()</code> is its invite.</td
				></tr
			>
			<tr><td><code>Group.parse(invite)</code></td><td>Reads an invite.</td></tr>
			<tr
				><td><code>Identity.generate()</code>, <code>Identity.fromSeed(seed)</code></td><td
					>A member identity; persist <code>seed</code>. <code>id</code> is the member ID.</td
				></tr
			>
			<tr
				><td><code>joinGroup(group, options)</code></td><td
					>Joins with <code>identity</code> and <code>profile</code>, and stays in the group until
					left.</td
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
				><td><code>conn.members</code>, <code>conn.id</code>, <code>conn.rtt</code></td><td
					>The admitted members (ID → profile), our member ID, the latest round trip to the relay.</td
				></tr
			>
			<tr><td><code>conn.leave()</code></td><td>Leaves the group.</td></tr>
		</tbody>
	</table>
</div>

<h2 id="rust">Rust</h2>
<p>
	The <code>client</code> feature adds <code>client::group</code>, a Tokio driver. The connection
	dereferences to a <code>GroupSender</code>, and <code>conn.sender()</code> gives you a clone of it to
	send from other tasks.
</p>
<CodeBlock code={rustDependency} lang="toml" title="Cargo.toml" />
<CodeBlock code={rustGroup('en')} lang="rust" title="main.rs" />
<h3>Your own event loop</h3>
<p>
	Without the <code>client</code> feature the crate does no I/O. <code>group::Member</code> is the
	whole member as a state machine: you feed it what happens on one WebSocket at a time and carry out
	what it asks for. Engines with their own loop poll it once per frame — Godot’s
	<code>WebSocketPeer</code> from a GDExtension, for example — and the same machine runs in the browser
	through the JavaScript SDK. Times are milliseconds on any monotonic clock.
</p>
<CodeBlock code={rustMember('en')} lang="rust" title="engine.rs" />
<p>
	A terminal group chat to try it with:
	<code>cargo run -p peerconnect --features client --example group -- new &lt;relay&gt; alice</code
	>, then <code>… -- join '&lt;invite&gt;' bob</code> in a second terminal.
</p>

<h2 id="engines">Game engines (C)</h2>
<p>
	<code>peerconnect-ffi</code> is the same state machine as a C library, for Unity, Unreal, Godot (through
	GDExtension or GDNative) and any language with a C FFI. It has no I/O and no threads: your engine owns
	the WebSocket, and once per frame you feed the member what happened and carry out what it asks for.
</p>
<CodeBlock code={cBuild('en')} lang="sh" title="shell" />
<CodeBlock code={cMember('en')} lang="c" title="game.c" />
<p>
	When the socket closes, call <code>pc_member_handle_close</code> with its close code (or -1); the
	member reconnects on its own by asking for <code>PC_CONNECT</code> again. Pointers in a
	<code>pc_output</code> are valid until the next call on that member, so copy what you keep. Voice
	frames interoperate with browsers when they use the SDK’s format,
	<code>hold[1] ‖ Opus packet</code>.
</p>

<h2 id="relays">Relays</h2>
<p>
	The Linux relay, its Docker image and the Cloudflare Worker serve groups next to rooms, with
	nothing extra to set up (see <a href="/docs#relay">Run a relay</a>). They admit 10 members per
	group; on the Linux relay, <code>PEERCONNECT_MAX_GROUP_MEMBERS</code> sets the limit, up to 32.
</p>
<p>
	The Linux relay keeps a queue per class for every recipient. It drops voice for a recipient that
	has fallen behind, holds back for at most 5 s a sender whose commands or chat a recipient cannot
	keep up with (then closes the stuck recipient), and caps what each connection sends at 2 MiB/s, in
	bursts of up to 4 MiB. The Cloudflare Worker cannot see what it has buffered, so it forwards every
	frame at once: it can neither drop late voice nor put commands first, and every voice frame is a
	billed Durable Object message. For voice at scale, use the Linux relay.
</p>

<h2 id="latency">Latency and load</h2>
<p>
	Measured on one Apple M2 Pro with the game load test: every player is a real end-to-end encrypted
	WebSocket client sending its position 20 times a second, a command twice a second and a chat line
	every 10 s, three players per group talking at once, and a newcomer in every group loading a 2 MB
	snapshot halfway through. Latency is from the sender’s <code>send</code> to the receiver’s event.
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
				><td>10 groups, 100 players</td><td>0.95 / 2.6 ms</td><td>0.80 / 2.4 ms</td><td>6.0 ms</td
				><td>0.4 cores, 6 MB</td></tr
			>
			<tr
				><td>50 groups, 500 players</td><td>0.92 / 3.5 ms</td><td>0.77 / 3.3 ms</td><td>2.7 ms</td
				><td>1.7 cores, 19 MB</td></tr
			>
			<tr
				><td>100 groups, 1,000 players</td><td>1.6 / 17 ms</td><td>1.4 / 15 ms</td><td>19 ms</td><td
					>4.1 cores, 35 MB</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>
	Everything reliable arrived exactly once. At 100 groups the machine is saturated: the load
	generator itself uses five to six of its cores.
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
				><td>Members per group</td><td
					>32 slots; the reference relays admit 10 by default, and turn away more with close code
					4429</td
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
		<strong>Voice runs over TCP.</strong> Over a WebSocket, a lost packet holds up what follows it until
		it is resent. The jitter buffer, the dropping of late frames and small kernel buffers keep that to
		a brief glitch, but on a lossy link UDP-based calls (WeChat, Telegram, WebRTC) degrade more gracefully.
		WebTransport datagrams would lift this; they are not used yet.
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
		<strong>The Cloudflare relay</strong> forwards every frame at once: no dropping of late voice, no
		commands first, and a billed Durable Object message per voice frame.
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
