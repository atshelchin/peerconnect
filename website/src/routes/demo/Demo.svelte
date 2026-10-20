<script lang="ts">
	import { onMount, tick } from 'svelte';
	import type { Connection, Room, State } from 'peerconnect';
	import { encode } from 'uqr';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { RELAY_URL } from '#lib/site.ts';

	// Each browser tab is one peer. Its room record lives in sessionStorage,
	// which is per tab and survives reloads, so two tabs never share a seat.
	const ROOM_KEY = 'peerconnect-demo:room';
	const LINES_KEY = 'peerconnect-demo:lines';

	type Sdk = typeof import('peerconnect');
	type Notice = 'replaced' | 'closedByPeer' | 'closed' | 'left';
	interface Line {
		from: 'me' | 'peer';
		text: string;
		/** The message id in this tab's current stream (mine only). */
		id?: number;
		delivered?: boolean;
		/** Sent before a reload, never acknowledged: the queue lived in memory. */
		lost?: boolean;
	}

	const t = $derived(i18n.t.demo);
	const decoder = new TextDecoder();

	let sdk: Sdk | undefined;
	let conn: Connection | undefined;

	let phase: 'loading' | 'start' | 'invited' | 'room' | 'error' = $state('loading');
	let pendingInvite = $state('');
	let pasted = $state('');
	let invalid = $state(false);
	let status: State = $state('offline');
	let connected = $state(false); // whether `conn` is live
	let role: 'inviter' | 'joiner' = $state('inviter');
	let roomId = $state('');
	let paired = $state(false);
	let closing = $state(false);
	let inviteLink = $state('');
	let safety = $state('');
	let notice: Notice | null = $state(null);
	let lines: Line[] = $state([]);
	let draft = $state('');
	let copied = $state(false);
	let log: HTMLElement | undefined = $state();

	const qr = $derived.by(() => {
		if (!inviteLink) return null;
		const { data, size } = encode(inviteLink, { ecc: 'M', border: 0 });
		let d = '';
		data.forEach((row, y) => row.forEach((dark, x) => dark && (d += `M${x} ${y}h1v1h-1z`)));
		return { d, size };
	});

	// sessionStorage can be unavailable; the demo then simply forgets on reload.
	function read(key: string): string | null {
		try {
			return sessionStorage.getItem(key);
		} catch {
			return null;
		}
	}
	function write(key: string, value: string | null) {
		try {
			if (value === null) sessionStorage.removeItem(key);
			else sessionStorage.setItem(key, value);
		} catch {
			// not available
		}
	}
	const saveLines = () => write(LINES_KEY, JSON.stringify(lines));

	/** A shareable link: short when the invite is for the public relay. */
	function linkFor(invite: string): string {
		const prefix = `${RELAY_URL}#`;
		const fragment = invite.startsWith(prefix)
			? invite.slice(prefix.length)
			: encodeURIComponent(invite);
		return `${location.origin}${i18n.href('/demo')}#${fragment}`;
	}

	/** Accepts an invite, a demo link, or a link's fragment. */
	function inviteFrom(text: string): string {
		let value = text.trim();
		if (/^https?:\/\//i.test(value)) value = value.slice(value.indexOf('#') + 1);
		else if (value.includes('://')) return value;
		value = decodeURIComponent(value);
		return value.includes('://') ? value : `${RELAY_URL}#${value}`;
	}

	function clearHash() {
		if (location.hash) history.replaceState(history.state, '', location.pathname);
	}

	async function updateSafety(room: Room) {
		const own = room.publicKey;
		const peer = room.peerKey;
		if (!peer || !room.paired) return (safety = '');
		const [inviter, joiner] = room.role === 'inviter' ? [own, peer] : [peer, own];
		const label = new TextEncoder().encode('peerconnect demo safety code');
		const digest = new Uint8Array(
			await crypto.subtle.digest('SHA-256', new Uint8Array([...label, ...inviter, ...joiner]))
		);
		safety = [0, 2, 4]
			.map((i) => String(((digest[i] << 8) | digest[i + 1]) % 10000).padStart(4, '0'))
			.join(' ');
	}

	function show(room: Room) {
		role = room.role;
		roomId = room.id;
		paired = room.paired;
		closing = room.closing;
		inviteLink = room.invite ? linkFor(room.invite) : '';
		void updateSafety(room);
	}

	async function scrollDown() {
		await tick();
		log?.scrollTo({ top: log.scrollHeight, behavior: 'smooth' });
	}

	function start(room: Room) {
		const c = sdk!.connect(room);
		conn = c;
		connected = true;
		notice = null;
		const current = () => conn === c;
		c.addEventListener('state', () => {
			if (!current()) return;
			status = c.state;
			if (c.state === 'connected') notice = null;
			if (c.state !== 'closed') return;
			conn = undefined;
			connected = false;
			if (c.reason === 'left') {
				notice = 'left';
			} else {
				// The room is over for good.
				notice = c.reason === 'closed-by-peer' ? 'closedByPeer' : 'closed';
				write(ROOM_KEY, null);
				inviteLink = '';
			}
		});
		c.addEventListener('message', (e) => {
			if (!current()) return;
			lines.push({ from: 'peer', text: decoder.decode(e.data) });
			saveLines();
			void scrollDown();
		});
		c.addEventListener('delivered', (e) => {
			if (!current()) return;
			for (const line of lines) {
				if (line.from === 'me' && line.id !== undefined && line.id < e.detail)
					line.delivered = true;
			}
			saveLines();
		});
		c.addEventListener('paired', () => {
			if (!current()) return;
			write(ROOM_KEY, c.room.toString());
			show(c.room);
		});
		c.addEventListener('replaced', () => {
			if (current()) notice = 'replaced';
		});
		status = c.state;
		show(room);
		phase = 'room';
	}

	function begin(room: Room) {
		write(ROOM_KEY, room.toString()); // before connecting: it holds our identity
		lines = [];
		saveLines();
		clearHash();
		start(room);
	}

	function create() {
		begin(sdk!.Room.create(RELAY_URL));
	}

	function join(invite: string) {
		let room: Room;
		try {
			room = sdk!.Room.join(inviteFrom(invite));
		} catch {
			invalid = true;
			return;
		}
		invalid = false;
		begin(room);
	}

	function reconnect() {
		const record = read(ROOM_KEY);
		if (record) start(sdk!.Room.parse(record));
	}

	function send(event: SubmitEvent) {
		event.preventDefault();
		const text = draft.trim();
		if (!text || !conn) return;
		try {
			lines.push({ from: 'me', text, id: conn.send(text) });
		} catch {
			return; // closing, or too much unacknowledged
		}
		draft = '';
		saveLines();
		void scrollDown();
	}

	function end() {
		if (!conn || !confirm(t.actions.endConfirm)) return;
		conn.close();
		write(ROOM_KEY, conn.room.toString()); // closing: reconnecting finishes the job
		closing = true;
	}

	function again() {
		conn?.leave();
		conn = undefined;
		connected = false;
		write(ROOM_KEY, null);
		write(LINES_KEY, null);
		lines = [];
		notice = null;
		status = 'offline';
		inviteLink = '';
		safety = '';
		clearHash();
		phase = 'start';
	}

	async function copyLink() {
		try {
			await navigator.clipboard.writeText(inviteLink);
			copied = true;
			setTimeout(() => (copied = false), 1600);
		} catch {
			// clipboard not available
		}
	}

	onMount(() => {
		let disposed = false;
		(async () => {
			try {
				sdk = await import('peerconnect');
			} catch {
				phase = 'error';
				return;
			}
			if (disposed) return;
			try {
				const restored: Line[] = JSON.parse(read(LINES_KEY) ?? '[]');
				// Message ids belonged to the previous stream, gone with the reload.
				lines = restored.map(({ id, ...line }) =>
					line.from === 'me' && !line.delivered && id !== undefined ? { ...line, lost: true } : line
				);
			} catch {
				lines = [];
			}
			const fragment = location.hash.slice(1);
			const record = read(ROOM_KEY);
			if (record) {
				clearHash();
				try {
					start(sdk.Room.parse(record));
				} catch {
					again();
				}
			} else if (fragment) {
				pendingInvite = fragment;
				phase = 'invited';
			} else {
				phase = 'start';
			}
			void scrollDown();
		})();
		return () => {
			disposed = true;
			conn?.leave();
			conn = undefined;
		};
	});
</script>

{#if phase === 'loading'}
	<div class="card panel center" aria-busy="true">
		<span class="spinner" aria-hidden="true"></span>
		<span>{t.loading}</span>
	</div>
{:else if phase === 'error'}
	<div class="card panel center">{t.noscript}</div>
{:else if phase === 'start'}
	<div class="card panel start">
		<div class="choice">
			<h2>{t.start.title}</h2>
			<button class="button primary" type="button" onclick={create}>{t.start.create}</button>
			<p class="hint">{t.start.createHint}</p>
		</div>
		<form
			class="choice"
			onsubmit={(e) => {
				e.preventDefault();
				join(pasted);
			}}
		>
			<label for="invite" class="label">{t.start.or}</label>
			<div class="row">
				<input
					id="invite"
					type="text"
					autocomplete="off"
					spellcheck="false"
					placeholder={t.start.placeholder}
					bind:value={pasted}
					aria-invalid={invalid}
				/>
				<button class="button secondary" type="submit" disabled={!pasted.trim()}
					>{t.start.join}</button
				>
			</div>
			{#if invalid}<p class="error" role="alert">{t.start.invalid}</p>{/if}
		</form>
	</div>
{:else if phase === 'invited'}
	<div class="card panel invited">
		<h2>{t.invited.title}</h2>
		<p>{t.invited.text}</p>
		{#if invalid}<p class="error" role="alert">{t.start.invalid}</p>{/if}
		<div class="row">
			<button class="button primary" type="button" onclick={() => join(pendingInvite)}>
				{t.invited.join}
			</button>
			<button class="button ghost" type="button" onclick={again}>{t.invited.cancel}</button>
		</div>
	</div>
{:else}
	<div class="room" class:inviting={!!qr}>
		{#if qr}
			<section class="card invite" aria-labelledby="invite-title">
				<div class="qr">
					<svg
						viewBox="-2 -2 {qr.size + 4} {qr.size + 4}"
						shape-rendering="crispEdges"
						role="img"
						aria-label={t.invite.title}
					>
						<rect x="-2" y="-2" width={qr.size + 4} height={qr.size + 4} fill="#fff" />
						<path d={qr.d} fill="#11151a" />
					</svg>
				</div>
				<div class="invite-text">
					<h2 id="invite-title">{t.invite.title}</h2>
					<p>{t.invite.text}</p>
					<div class="link">
						<code>{inviteLink}</code>
						<button class="button secondary" type="button" onclick={copyLink}>
							{copied ? t.invite.copied : t.invite.copy}
						</button>
					</div>
				</div>
			</section>
		{/if}

		<section class="card chat" aria-label={t.eyebrow}>
			<header class="chat-head">
				<span class="status {status}">
					<span class="dot" aria-hidden="true"></span>
					{t.state[status]}
				</span>
				<span class="role">{t.role[role]}</span>
			</header>
			<div class="log" bind:this={log} role="log" aria-live="polite">
				{#if lines.length === 0}
					<p class="empty">{t.chat.empty}</p>
				{/if}
				{#each lines as line, i (i)}
					<div class="line {line.from}">
						<span class="visually-hidden">{line.from === 'me' ? t.chat.you : t.chat.peer}:</span>
						<p class="bubble">{line.text}</p>
						{#if line.from === 'me'}
							{#if line.lost}
								<span class="mark lost" title={t.chat.lost}
									>! <span class="visually-hidden">{t.chat.lost}</span></span
								>
							{:else if line.delivered}
								<span class="mark delivered" title={t.chat.delivered}
									>✓ <span class="visually-hidden">{t.chat.delivered}</span></span
								>
							{:else}
								<span class="mark queued" title={t.chat.queued}
									>◷ <span class="visually-hidden">{t.chat.queued}</span></span
								>
							{/if}
						{/if}
					</div>
				{/each}
			</div>
			{#if notice}
				<p class="notice" role="status">{t.notices[notice]}</p>
			{/if}
			<form class="composer" onsubmit={send}>
				<label for="message" class="visually-hidden">{t.chat.placeholder}</label>
				<input
					id="message"
					type="text"
					autocomplete="off"
					placeholder={t.chat.placeholder}
					bind:value={draft}
					disabled={!connected || closing}
					maxlength="4000"
				/>
				<button
					class="button primary"
					type="submit"
					disabled={!connected || closing || !draft.trim()}
				>
					{t.chat.send}
				</button>
			</form>
		</section>

		<aside class="card details" aria-label={t.details.title}>
			<dl>
				<div>
					<dt>{t.details.safety}</dt>
					<dd>
						{#if safety}
							<span class="safety">{safety}</span>
							<span class="hint">{t.details.safetyHint}</span>
						{:else}
							<span class="hint">{t.details.safetyPending}</span>
						{/if}
					</dd>
				</div>
				<div>
					<dt>{t.details.roomId}</dt>
					<dd><code class="id">{roomId}</code></dd>
					<dd class="hint">{paired ? t.details.paired : t.details.unpaired}</dd>
				</div>
			</dl>
			<div class="actions">
				{#if connected}
					<button class="button ghost" type="button" onclick={() => conn?.leave()}
						>{t.actions.leave}</button
					>
					<button class="button danger" type="button" onclick={end} disabled={closing}
						>{t.actions.end}</button
					>
				{:else if notice === 'left'}
					<button class="button secondary" type="button" onclick={reconnect}
						>{t.actions.reconnect}</button
					>
				{/if}
				{#if !connected}
					<button class="button ghost" type="button" onclick={again}>{t.actions.again}</button>
				{/if}
			</div>
		</aside>
	</div>
{/if}

<style>
	h2 {
		font-size: 1.2rem;
	}

	.panel {
		padding: clamp(24px, 4vw, 36px);
	}

	.center {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 12px;
		min-height: 220px;
		color: var(--muted);
	}

	.spinner {
		width: 18px;
		height: 18px;
		border: 2px solid var(--line-strong);
		border-top-color: var(--accent);
		border-radius: 50%;
		animation: spin 0.8s linear infinite;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	.start {
		display: grid;
		grid-template-columns: 1fr 1.3fr;
		gap: 0;
		padding: 0;
	}

	.choice {
		display: grid;
		align-content: start;
		justify-items: start;
		gap: 14px;
		padding: clamp(24px, 4vw, 36px);
	}

	.choice + .choice {
		border-left: 1px solid var(--line);
	}

	.label {
		font-weight: 650;
		font-size: 1.05rem;
	}

	.hint {
		color: var(--muted);
		font-size: 0.9rem;
	}

	.row {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
		width: 100%;
	}

	input {
		flex: 1;
		min-width: 0;
		min-height: 46px;
		padding: 0 16px;
		border: 1px solid var(--line-strong);
		border-radius: 999px;
		background: var(--bg);
		color: var(--ink);
		font: inherit;
		font-size: 0.95rem;
	}

	input:focus {
		outline: none;
		border-color: var(--accent);
		box-shadow: 0 0 0 3px var(--accent-soft);
	}

	input[aria-invalid='true'] {
		border-color: var(--danger);
	}

	.error {
		color: var(--danger);
		font-size: 0.92rem;
	}

	.invited {
		display: grid;
		gap: 14px;
		max-width: 640px;
	}

	.invited p {
		color: var(--ink-2);
	}

	.room {
		display: grid;
		grid-template-columns: minmax(0, 1fr) 300px;
		grid-template-areas: 'chat details';
		gap: 20px;
		align-items: start;
	}

	.room.inviting {
		grid-template-areas:
			'invite invite'
			'chat details';
	}

	.invite {
		grid-area: invite;
		display: flex;
		align-items: center;
		gap: clamp(20px, 4vw, 40px);
		padding: clamp(20px, 3vw, 28px);
		border-color: var(--accent);
	}

	.qr {
		flex: none;
		width: 196px;
		padding: 10px;
		border-radius: 14px;
		background: #fff;
		box-shadow: 0 0 0 1px var(--line);
	}

	.invite-text {
		display: grid;
		gap: 10px;
		min-width: 0;
	}

	.invite-text p {
		color: var(--ink-2);
	}

	.link {
		display: flex;
		align-items: center;
		gap: 10px;
		min-width: 0;
		margin-top: 4px;
	}

	.link code {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
		padding: 10px 12px;
		font-size: 0.8rem;
	}

	.chat {
		grid-area: chat;
		display: flex;
		flex-direction: column;
		height: min(560px, 70vh);
		overflow: hidden;
	}

	.chat-head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
		padding: 14px 18px;
		border-bottom: 1px solid var(--line);
		font-size: 0.9rem;
	}

	.role {
		color: var(--muted);
	}

	.status {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		font-weight: 600;
	}

	.dot {
		width: 9px;
		height: 9px;
		border-radius: 50%;
		background: var(--cipher);
	}

	.status.offline .dot {
		animation: pulse 1.2s ease-in-out infinite;
	}

	.status.connected .dot {
		background: var(--accent);
		box-shadow: 0 0 0 4px var(--accent-soft);
	}

	.status.closed .dot {
		background: var(--line-strong);
	}

	@keyframes pulse {
		50% {
			opacity: 0.3;
		}
	}

	.log {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 8px;
		padding: 18px;
		overflow-y: auto;
		background: var(--bg);
	}

	.empty {
		margin: auto;
		max-width: 340px;
		color: var(--muted);
		font-size: 0.92rem;
		text-align: center;
	}

	.line {
		display: flex;
		align-items: flex-end;
		gap: 6px;
		max-width: 80%;
	}

	.line.me {
		align-self: flex-end;
	}

	.bubble {
		padding: 8px 14px;
		border-radius: 16px 16px 16px 4px;
		background: var(--surface);
		border: 1px solid var(--line);
		overflow-wrap: anywhere;
		white-space: pre-wrap;
	}

	.me .bubble {
		order: 1;
		border-radius: 16px 16px 4px 16px;
		background: var(--accent);
		border-color: var(--accent);
		color: var(--accent-ink);
	}

	.mark {
		order: 0;
		font-size: 0.78rem;
		line-height: 1;
		padding-bottom: 4px;
	}

	.delivered {
		color: var(--accent);
	}

	.queued {
		color: var(--muted);
	}

	.lost {
		color: var(--danger);
		font-weight: 700;
	}

	.notice {
		padding: 10px 18px;
		border-top: 1px solid var(--line);
		background: var(--cipher-soft);
		color: var(--ink-2);
		font-size: 0.88rem;
	}

	.composer {
		display: flex;
		gap: 10px;
		padding: 12px;
		border-top: 1px solid var(--line);
	}

	.details {
		grid-area: details;
		display: grid;
		gap: 20px;
		padding: 22px;
	}

	dl {
		display: grid;
		gap: 18px;
		margin: 0;
	}

	dt {
		margin-bottom: 6px;
		font-family: var(--font-mono);
		font-size: 0.72rem;
		font-weight: 600;
		letter-spacing: 0.08em;
		text-transform: uppercase;
		color: var(--muted);
	}

	dd {
		display: grid;
		gap: 6px;
		margin: 0;
	}

	.safety {
		font-family: var(--font-mono);
		font-size: 1.45rem;
		font-weight: 600;
		letter-spacing: 0.06em;
		color: var(--accent);
	}

	.id {
		display: block;
		font-size: 0.74rem;
		word-break: break-all;
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}

	.actions .button {
		min-height: 38px;
		padding: 0 14px;
		font-size: 0.88rem;
	}

	@media (max-width: 860px) {
		.room,
		.room.inviting {
			grid-template-columns: 1fr;
			grid-template-areas: 'invite' 'chat' 'details';
		}

		.room:not(.inviting) {
			grid-template-areas: 'chat' 'details';
		}

		.start {
			grid-template-columns: 1fr;
		}

		.choice + .choice {
			border-left: 0;
			border-top: 1px solid var(--line);
		}
	}

	@media (max-width: 560px) {
		.invite {
			flex-direction: column;
			align-items: stretch;
		}

		.qr {
			align-self: center;
			width: min(240px, 70vw);
		}

		.link {
			flex-direction: column;
			align-items: stretch;
		}

		.line {
			max-width: 90%;
		}
	}
</style>
