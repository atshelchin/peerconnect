<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { SvelteSet } from 'svelte/reactivity';
	import type { GroupConnection } from 'peerconnect';
	import type { Voice } from 'peerconnect/voice';
	import { encode } from 'uqr';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { RELAY_URL } from '#lib/site.ts';

	// Each browser tab is one member. Its identity lives in sessionStorage,
	// which is per tab and survives reloads: two tabs are two members, and a
	// reloaded tab takes its own place back at once.
	const IDENTITY_KEY = 'peerconnect-voice:identity';
	const NAME_KEY = 'peerconnect-voice:name';
	const CAPACITY = 10;

	type Sdk = typeof import('peerconnect');
	type VoiceSdk = typeof import('peerconnect/voice');
	type Problem = 'mic' | 'codecs' | 'full' | 'replaced' | 'link' | 'failed';
	interface Member {
		id: string;
		name: string;
	}
	interface Line {
		from: string;
		name: string;
		text: string;
		me: boolean;
	}
	interface Stats {
		target: number;
		concealed: number;
		dropped: number;
	}

	const t = $derived(i18n.t.voice);
	const decoder = new TextDecoder();

	let sdk: Sdk | undefined;
	let voiceSdk: VoiceSdk | undefined;
	let conn: GroupConnection | undefined;
	let voice: Voice | undefined = $state.raw();
	let statsTimer: ReturnType<typeof setInterval> | undefined;

	let phase: 'loading' | 'start' | 'join' | 'starting' | 'room' | 'error' = $state('loading');
	let problems: Problem[] = $state([]);
	let invite = $state('');
	let name = $state('');
	let me = $state('');
	let status: 'offline' | 'online' | 'closed' = $state('offline');
	let members: Member[] = $state([]);
	const speaking = new SvelteSet<string>();
	let stats: Record<string, Stats> = $state({});
	let volumes: Record<string, number> = $state({});
	let muted = $state(false);
	let deafened = $state(false);
	let ptt = $state(false);
	let holding = $state(false);
	let rtt: number | undefined = $state();
	let lines: Line[] = $state([]);
	let draft = $state('');
	let copied = $state(false);
	let log: HTMLElement | undefined = $state();

	const link = $derived(invite ? linkFor(invite) : '');
	const qr = $derived.by(() => {
		if (!link) return null;
		const { data, size } = encode(link, { ecc: 'M', border: 0 });
		let d = '';
		data.forEach((row, y) => row.forEach((dark, x) => dark && (d += `M${x} ${y}h1v1h-1z`)));
		return { d, size };
	});
	const others = $derived(members.filter((m) => m.id !== me));

	function read(key: string): string | null {
		try {
			return sessionStorage.getItem(key);
		} catch {
			return null;
		}
	}
	function write(key: string, value: string) {
		try {
			sessionStorage.setItem(key, value);
		} catch {
			// not available: the identity just lasts as long as the page
		}
	}

	/** A shareable link: the room key after `#`, short for the public relay. */
	function linkFor(groupInvite: string): string {
		const prefix = `${RELAY_URL}#`;
		const fragment = groupInvite.startsWith(prefix)
			? groupInvite.slice(prefix.length)
			: encodeURIComponent(groupInvite);
		return `${location.origin}${i18n.href('/demo/voice')}#${fragment}`;
	}

	function inviteFrom(hash: string): string {
		const value = decodeURIComponent(hash.replace(/^#/, ''));
		return value.includes('://') ? value : `${RELAY_URL}#${value}`;
	}

	function identity() {
		const saved = read(IDENTITY_KEY);
		if (saved) {
			try {
				return sdk!.Identity.fromSeed(Uint8Array.from(atob(saved), (c) => c.charCodeAt(0)));
			} catch {
				// a damaged value: start afresh
			}
		}
		const fresh = sdk!.Identity.generate();
		write(IDENTITY_KEY, btoa(String.fromCharCode(...fresh.seed)));
		return fresh;
	}

	function nameOf(profile: Uint8Array): string {
		try {
			const value = JSON.parse(decoder.decode(profile)).name;
			if (typeof value === 'string' && value.trim()) return value.trim().slice(0, 40);
		} catch {
			// not ours
		}
		return '?';
	}

	function raise(problem: Problem) {
		if (!problems.includes(problem)) problems = [...problems, problem];
	}

	function create() {
		const group = sdk!.Group.create(RELAY_URL);
		invite = group.toString();
		history.replaceState(history.state, '', linkFor(invite));
		phase = 'join';
	}

	async function join(event: SubmitEvent) {
		event.preventDefault();
		const chosen = name.trim().slice(0, 40);
		if (!chosen) return;
		write(NAME_KEY, chosen);
		phase = 'starting';
		problems = problems.filter((p) => p === 'codecs');

		const c = sdk!.joinGroup(invite, {
			identity: identity(),
			profile: JSON.stringify({ name: chosen })
		});
		conn = c;
		me = c.id;
		members = [{ id: c.id, name: chosen }];
		const current = () => conn === c;
		c.addEventListener('state', () => {
			if (!current()) return;
			status = c.state;
			if (c.state === 'online') problems = problems.filter((p) => p !== 'full' && p !== 'replaced');
		});
		c.addEventListener('joined', (e) => {
			if (!current()) return;
			const { member, profile } = e.detail;
			members = [...members.filter((m) => m.id !== member), { id: member, name: nameOf(profile) }];
		});
		c.addEventListener('left', (e) => {
			if (!current()) return;
			members = members.filter((m) => m.id !== e.detail.member);
		});
		c.addEventListener('message', (e) => {
			if (!current() || e.detail.channel !== 'chat') return;
			const from = e.detail.from;
			const who = members.find((m) => m.id === from)?.name ?? '?';
			lines = [...lines, { from, name: who, text: decoder.decode(e.detail.data), me: from === me }];
			void scrollDown();
		});
		c.addEventListener('full', () => current() && raise('full'));
		c.addEventListener('replaced', () => current() && raise('replaced'));

		try {
			voice = await voiceSdk!.startVoice(c);
			voice.addEventListener('speaking', (e) => {
				if (e.detail.speaking) speaking.add(e.detail.member);
				else speaking.delete(e.detail.member);
			});
			applyMute();
		} catch (error) {
			const unsupported =
				typeof AudioEncoder === 'undefined' || String(error).includes('WebCodecs');
			raise(unsupported ? 'codecs' : 'mic');
		}
		statsTimer = setInterval(() => {
			rtt = conn?.rtt;
			if (voice) stats = Object.fromEntries(voice.stats());
		}, 500);
		phase = 'room';
	}

	function applyMute() {
		voice?.setMuted(muted || (ptt && !holding));
	}

	function toggleMute() {
		muted = !muted;
		applyMute();
	}

	function toggleDeafen() {
		deafened = !deafened;
		voice?.setDeafened(deafened);
	}

	function togglePtt() {
		ptt = !ptt;
		holding = false;
		applyMute();
	}

	function hold(down: boolean) {
		if (!ptt || holding === down) return;
		holding = down;
		applyMute();
	}

	function onKey(event: KeyboardEvent, down: boolean) {
		if (event.code !== 'Space' || !ptt || phase !== 'room') return;
		const target = event.target as HTMLElement | null;
		if (target && ['INPUT', 'TEXTAREA', 'BUTTON'].includes(target.tagName)) return;
		event.preventDefault();
		hold(down);
	}

	function setVolume(id: string, value: number) {
		volumes[id] = value;
		voice?.setVolume(id, value);
	}

	async function scrollDown() {
		await tick();
		log?.scrollTo({ top: log.scrollHeight, behavior: 'smooth' });
	}

	function send(event: SubmitEvent) {
		event.preventDefault();
		const text = draft.trim();
		if (!text || !conn) return;
		try {
			conn.send('chat', text);
		} catch {
			return;
		}
		draft = '';
	}

	async function stop() {
		clearInterval(statsTimer);
		const [v, c] = [voice, conn];
		voice = undefined;
		conn = undefined;
		await v?.stop().catch(() => {});
		c?.leave();
	}

	async function leave() {
		await stop();
		members = [];
		lines = [];
		speaking.clear();
		stats = {};
		status = 'offline';
		problems = [];
		invite = '';
		history.replaceState(history.state, '', location.pathname);
		phase = 'start';
	}

	async function copyLink() {
		try {
			await navigator.clipboard.writeText(link);
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
				[sdk, voiceSdk] = await Promise.all([import('peerconnect'), import('peerconnect/voice')]);
			} catch {
				phase = 'error';
				raise('failed');
				return;
			}
			if (disposed) return;
			name = read(NAME_KEY) ?? '';
			if (typeof AudioEncoder === 'undefined') raise('codecs');
			if (location.hash.length > 1) {
				try {
					invite = sdk.Group.parse(inviteFrom(location.hash)).toString();
					phase = 'join';
				} catch {
					raise('link');
					phase = 'start';
				}
			} else {
				phase = 'start';
			}
		})();
		const down = (e: KeyboardEvent) => onKey(e, true);
		const up = (e: KeyboardEvent) => onKey(e, false);
		window.addEventListener('keydown', down);
		window.addEventListener('keyup', up);
		return () => {
			disposed = true;
			window.removeEventListener('keydown', down);
			window.removeEventListener('keyup', up);
			void stop();
		};
	});
</script>

{#if problems.length}
	<div class="problems" role="status">
		{#each problems as problem (problem)}
			<p class="card problem">{t.problems[problem]}</p>
		{/each}
	</div>
{/if}

{#if phase === 'loading'}
	<div class="card panel center"><span class="spinner" aria-hidden="true"></span>{t.loading}</div>
{:else if phase === 'error'}
	<div class="card panel center">{t.problems.failed}</div>
{:else if phase === 'start'}
	<div class="card panel start">
		<h2>{t.start.title}</h2>
		<p class="hint">{t.start.text}</p>
		<button class="button primary" onclick={create}>{t.start.create}</button>
	</div>
{:else if phase === 'join' || phase === 'starting'}
	<form class="card panel join" onsubmit={join}>
		<h2>{t.join.title}</h2>
		<label for="voice-name">{t.join.name}</label>
		<div class="row">
			<input
				id="voice-name"
				bind:value={name}
				maxlength="40"
				autocomplete="nickname"
				placeholder={t.join.namePlaceholder}
				required
			/>
			<button class="button primary" type="submit" disabled={phase === 'starting' || !name.trim()}>
				{phase === 'starting' ? t.join.starting : t.join.button}
			</button>
		</div>
		<p class="hint">{t.join.hint}</p>
	</form>
{:else}
	<div class="room">
		<section class="card panel people" aria-label={t.room.people}>
			<header class="bar">
				<span class="status {status}">{t.state[status]}</span>
				<span class="count">{members.length} / {CAPACITY}</span>
				{#if rtt !== undefined}<span class="rtt" title={t.room.rtt}>{t.room.rtt}: {rtt} ms</span
					>{/if}
			</header>

			<ul class="members">
				{#each members as member (member.id)}
					{@const isMe = member.id === me}
					{@const s = stats[member.id]}
					<li class="member" class:speaking={speaking.has(member.id)}>
						<span class="avatar" aria-hidden="true">{member.name.slice(0, 1).toUpperCase()}</span>
						<span class="who">
							<span class="name"
								>{member.name}{#if isMe}<em>&nbsp;· {t.room.you}</em>{/if}</span
							>
							{#if !isMe && s}
								<span class="meta"
									>{t.room.buffer}
									{Math.round(s.target * 1000)} ms · {t.room.lost}
									{s.concealed} · {t.room.skipped}
									{s.dropped}</span
								>
							{/if}
						</span>
						{#if !isMe && voice}
							<input
								class="volume"
								type="range"
								min="0"
								max="2"
								step="0.05"
								value={volumes[member.id] ?? 1}
								aria-label={t.room.volume.replace('{name}', member.name)}
								oninput={(e) => setVolume(member.id, Number(e.currentTarget.value))}
							/>
						{/if}
					</li>
				{/each}
			</ul>
			{#if !others.length}<p class="hint">{t.room.alone}</p>{/if}

			<div class="controls">
				{#if voice}
					<button
						class="button secondary"
						class:active={muted}
						aria-pressed={muted}
						onclick={toggleMute}
					>
						{muted ? t.controls.unmute : t.controls.mute}
					</button>
					<button
						class="button secondary"
						class:active={deafened}
						aria-pressed={deafened}
						onclick={toggleDeafen}
					>
						{deafened ? t.controls.undeafen : t.controls.deafen}
					</button>
					<button
						class="button secondary"
						class:active={ptt}
						aria-pressed={ptt}
						onclick={togglePtt}
					>
						{t.controls.ptt}
					</button>
				{/if}
				<button class="button danger" onclick={leave}>{t.controls.leave}</button>
			</div>
			{#if voice && ptt}
				<button
					class="button primary hold"
					class:holding
					onpointerdown={() => hold(true)}
					onpointerup={() => hold(false)}
					onpointerleave={() => hold(false)}
					onpointercancel={() => hold(false)}>{t.controls.hold}</button
				>
				<p class="hint">{t.controls.pttHint}</p>
			{/if}
		</section>

		<section class="card panel chat" aria-label={t.chat.title}>
			<h2>{t.chat.title}</h2>
			<div class="log" bind:this={log}>
				{#each lines as line, i (i)}
					<p class="line" class:me={line.me}><b>{line.name}</b> {line.text}</p>
				{:else}
					<p class="hint">{t.chat.empty}</p>
				{/each}
			</div>
			<form class="row" onsubmit={send}>
				<input
					id="voice-message"
					bind:value={draft}
					maxlength="2000"
					placeholder={t.chat.placeholder}
				/>
				<button class="button primary" type="submit" disabled={!draft.trim()}>{t.chat.send}</button>
			</form>
		</section>

		<section class="card panel invite" aria-label={t.invite.title}>
			<h2>{t.invite.title}</h2>
			{#if qr}
				<svg class="qr" viewBox="0 0 {qr.size} {qr.size}" role="img" aria-label={t.invite.title}>
					<path d={qr.d} />
				</svg>
			{/if}
			<p class="link"><code>{link}</code></p>
			<button class="button secondary" onclick={copyLink}
				>{copied ? t.invite.copied : t.invite.copy}</button
			>
		</section>
	</div>
{/if}

<style>
	h2 {
		font-size: 1.15rem;
	}

	.panel {
		padding: clamp(20px, 4vw, 32px);
	}

	.center {
		display: flex;
		align-items: center;
		justify-content: center;
		gap: 12px;
		min-height: 200px;
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

	.start,
	.join {
		display: grid;
		justify-items: start;
		gap: 14px;
		max-width: 640px;
	}

	.join label {
		font-weight: 600;
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

	input:not([type='range']) {
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

	.problems {
		display: grid;
		gap: 10px;
		margin-bottom: 16px;
	}

	.problem {
		padding: 14px 18px;
		border-color: var(--accent);
	}

	.room {
		display: grid;
		grid-template-columns: 1.2fr 1fr;
		grid-template-areas: 'people chat' 'invite chat';
		gap: 18px;
		align-items: start;
	}

	.people {
		grid-area: people;
		display: grid;
		gap: 16px;
	}

	.chat {
		grid-area: chat;
		display: grid;
		gap: 12px;
	}

	.invite {
		grid-area: invite;
		display: grid;
		justify-items: start;
		gap: 12px;
	}

	.bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px 14px;
		font-size: 0.9rem;
	}

	.status {
		font-weight: 600;
	}

	.status.online {
		color: var(--accent);
	}

	.count,
	.rtt {
		color: var(--muted);
	}

	.members {
		display: grid;
		gap: 8px;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.member {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 10px 12px;
		border: 1px solid var(--line);
		border-radius: 14px;
	}

	.avatar {
		display: grid;
		place-items: center;
		flex: none;
		width: 38px;
		height: 38px;
		border-radius: 50%;
		background: var(--line);
		font-weight: 700;
		transition: box-shadow 0.15s;
	}

	.member.speaking .avatar {
		background: var(--accent);
		color: var(--bg);
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 35%, transparent);
	}

	.who {
		display: grid;
		flex: 1;
		min-width: 0;
	}

	.name {
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.name em {
		color: var(--muted);
		font-weight: 400;
		font-style: normal;
	}

	.meta {
		color: var(--muted);
		font-size: 0.8rem;
	}

	.volume {
		width: 96px;
		accent-color: var(--accent);
	}

	.controls {
		display: flex;
		flex-wrap: wrap;
		gap: 10px;
	}

	.button.active {
		border-color: var(--accent);
		color: var(--accent);
	}

	.hold {
		width: 100%;
		min-height: 64px;
		touch-action: none;
		user-select: none;
	}

	.hold.holding {
		filter: brightness(1.15);
	}

	.log {
		display: grid;
		align-content: start;
		gap: 8px;
		height: 320px;
		overflow-y: auto;
		padding: 4px 2px;
	}

	.line {
		margin: 0;
		overflow-wrap: anywhere;
	}

	.line.me b {
		color: var(--accent);
	}

	.qr {
		width: 168px;
		height: 168px;
		padding: 10px;
		border-radius: 12px;
		background: #fff;
		fill: #111;
	}

	.link {
		max-width: 100%;
		margin: 0;
		overflow-wrap: anywhere;
		font-size: 0.8rem;
	}

	@media (max-width: 760px) {
		.room {
			grid-template-columns: 1fr;
			grid-template-areas: 'people' 'chat' 'invite';
		}
	}
</style>
