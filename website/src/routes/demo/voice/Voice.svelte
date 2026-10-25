<script lang="ts">
	import { onMount, tick } from 'svelte';
	import type { GroupConnection } from 'peerconnect';
	import type { Voice } from 'peerconnect/voice';
	import type { Video } from 'peerconnect/video';
	import { encode } from 'uqr';
	import { i18n } from '#lib/i18n/current.svelte.ts';
	import { RELAY_URL } from '#lib/site.ts';

	// Each browser tab is one member. Its identity lives in sessionStorage,
	// which is per tab and survives reloads: two tabs are two members, and a
	// reloaded tab takes its own place back at once.
	const IDENTITY_KEY = 'peerconnect-voice:identity';
	const NAME_KEY = 'peerconnect-voice:name';
	const CAPACITY = 10;
	/** Bars in a waveform, one every 40 ms: the last 1.6 s. */
	const BARS = 40;
	/** Above this level (0–1, see `level`), someone counts as speaking. */
	const SPEAKING = 0.25;
	/** A member's video is shown while frames keep coming: at least one in this long. */
	const VIDEO_FRESH = 2000;
	/**
	 * Small video: the public relay is a Cloudflare Worker, where every
	 * fragment is a billed message (and nothing is dropped or put last).
	 */
	const VIDEO = { width: 480, height: 270, framerate: 15, bitrate: 300_000, minBitrate: 100_000 };

	type Sdk = typeof import('peerconnect');
	type VoiceSdk = typeof import('peerconnect/voice');
	type VideoSdk = typeof import('peerconnect/video');
	type Problem = 'mic' | 'camera' | 'codecs' | 'full' | 'replaced' | 'link' | 'failed';
	interface Member {
		id: string;
		name: string;
	}
	interface Line {
		name: string;
		text: string;
		me: boolean;
	}
	/** What a member says about itself, so that the others can show it. */
	interface Presence {
		muted: boolean;
		deafened: boolean;
		camera: boolean;
	}

	const t = $derived(i18n.t.voice);
	const decoder = new TextDecoder();

	let sdk: Sdk | undefined;
	let voiceSdk: VoiceSdk | undefined;
	let videoSdk: VideoSdk | undefined;
	let conn: GroupConnection | undefined;
	let voice: Voice | undefined = $state.raw();
	let video: Video | undefined = $state.raw();
	let frame = 0;
	/** When each member's latest video frame was decoded: read by the 40 ms loop, not rendered. */
	// eslint-disable-next-line svelte/prefer-svelte-reactivity -- updated 15 times a second per member
	const frames = new Map<string, number>();

	let phase: 'loading' | 'start' | 'join' | 'starting' | 'room' | 'error' = $state('loading');
	let problems: Problem[] = $state([]);
	let invite = $state('');
	let name = $state('');
	let me = $state('');
	let status: 'offline' | 'online' | 'closed' = $state('offline');
	let members: Member[] = $state([]);
	/** Recent levels (0–1) per member, newest last: the waveforms. */
	let waves: Record<string, number[]> = $state({});
	let presence: Record<string, Presence> = $state({});
	/** Members we muted for ourselves. */
	let silenced: Record<string, boolean> = $state({});
	let muted = $state(false);
	let deafened = $state(false);
	let camera = $state(false);
	let cameraStream: MediaStream | undefined = $state.raw();
	/** Members whose video is coming in now. */
	let watching: Record<string, boolean> = $state({});
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
	// Ourselves first, then the others in the order they came.
	const people = $derived([
		...members.filter((m) => m.id === me),
		...members.filter((m) => m.id !== me)
	]);

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

	/** An RMS level as 0–1 on a decibel scale: −60 dBFS is silence, −10 loud. */
	function level(rms: number): number {
		return Math.min(1, Math.max(0, (20 * Math.log10(rms + 1e-9) + 60) / 50));
	}

	function speaking(id: string): boolean {
		const wave = waves[id];
		return !!wave && wave.slice(-4).some((v) => v > SPEAKING);
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
			announce(); // so that the newcomer knows whether we are muted
		});
		c.addEventListener('left', (e) => {
			if (!current()) return;
			members = members.filter((m) => m.id !== e.detail.member);
			delete waves[e.detail.member];
			frames.delete(e.detail.member);
		});
		c.addEventListener('message', (e) => {
			if (!current()) return;
			const { from, channel, data } = e.detail;
			if (channel === 'command') {
				// Members announce whether they are muted, as a command.
				try {
					const said = JSON.parse(decoder.decode(data));
					if (said?.type === 'presence' && from !== me) {
						presence[from] = {
							muted: !!said.muted,
							deafened: !!said.deafened,
							camera: !!said.camera
						};
					}
				} catch {
					// not a presence announcement
				}
				return;
			}
			const who = members.find((m) => m.id === from)?.name ?? '?';
			lines = [...lines, { name: who, text: decoder.decode(data), me: from === me }];
			void scrollDown();
		});
		c.addEventListener('full', () => current() && raise('full'));
		c.addEventListener('replaced', () => current() && raise('replaced'));

		try {
			voice = await voiceSdk!.startVoice(c);
		} catch (error) {
			const unsupported =
				typeof AudioEncoder === 'undefined' || String(error).includes('WebCodecs');
			raise(unsupported ? 'codecs' : 'mic');
		}
		// Everyone's video, drawn into their card; ours only when the camera is on.
		if (typeof VideoDecoder === 'function') {
			try {
				video = await videoSdk!.startVideo(c, {
					...VIDEO,
					onFrame: (member, decoded) => {
						frames.set(member, performance.now());
						decoded.close();
					}
				});
			} catch {
				// no video in this browser: voice and chat go on
			}
		}
		// Every 40 ms, one more bar in everyone's waveform.
		let last = 0;
		const draw = (now: number) => {
			if (now - last >= 40) {
				const showing: Record<string, boolean> = {};
				for (const m of members) {
					const at = frames.get(m.id);
					if (m.id !== me && presence[m.id]?.camera && at && now - at < VIDEO_FRESH)
						showing[m.id] = true;
				}
				if (Object.keys(showing).join() !== Object.keys(watching).join()) watching = showing;
			}
			if (voice && now - last >= 40) {
				last = now;
				const stats = voice.stats();
				const next: Record<string, number[]> = {};
				for (const m of members) {
					// Off is a flat line: our microphone muted, theirs, or them muted for us.
					const off = m.id === me ? muted : presence[m.id]?.muted || silenced[m.id];
					const rms = off ? 0 : m.id === me ? voice.micLevel : (stats.get(m.id)?.rms ?? 0);
					next[m.id] = [...(waves[m.id] ?? Array(BARS).fill(0)).slice(1 - BARS), level(rms)];
				}
				waves = next;
			}
			frame = requestAnimationFrame(draw);
		};
		frame = requestAnimationFrame(draw);
		phase = 'room';
	}

	/** Tells the others whether we are muted or deafened. */
	function announce() {
		try {
			conn?.send('command', JSON.stringify({ type: 'presence', muted, deafened, camera }));
		} catch {
			// left
		}
	}

	function toggleMic() {
		muted = !muted;
		voice?.setMuted(muted);
		announce();
	}

	function toggleSpeaker() {
		deafened = !deafened;
		voice?.setDeafened(deafened);
		announce();
	}

	async function toggleCamera() {
		if (!video) return;
		if (camera) {
			camera = false;
			announce();
			await video.setStream(null);
			cameraStream?.getTracks().forEach((t) => t.stop());
			cameraStream = undefined;
			return;
		}
		try {
			const stream = await navigator.mediaDevices.getUserMedia({
				video: { width: VIDEO.width, height: VIDEO.height, frameRate: VIDEO.framerate }
			});
			cameraStream = stream;
			await video.setStream(stream);
			camera = true;
			problems = problems.filter((p) => p !== 'camera');
			announce();
		} catch {
			cameraStream?.getTracks().forEach((t) => t.stop());
			cameraStream = undefined;
			raise('camera');
		}
	}

	/** Draws a member's video into this canvas while it is on the page. */
	function screen(node: HTMLCanvasElement, id: string) {
		video?.attach(id, node);
		return {
			destroy() {
				video?.attach(id, null);
			}
		};
	}

	/** Shows our own camera, straight from it. */
	function preview(node: HTMLVideoElement, stream: MediaStream | undefined) {
		node.srcObject = stream ?? null;
		return {
			update(next: MediaStream | undefined) {
				node.srcObject = next ?? null;
			},
			destroy() {
				node.srcObject = null;
			}
		};
	}

	function toggleThem(id: string) {
		silenced[id] = !silenced[id];
		voice?.setVolume(id, silenced[id] ? 0 : 1);
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
		cancelAnimationFrame(frame);
		const [v, vd, c] = [voice, video, conn];
		voice = undefined;
		video = undefined;
		conn = undefined;
		vd?.stop();
		cameraStream?.getTracks().forEach((t) => t.stop());
		cameraStream = undefined;
		await v?.stop().catch(() => {});
		c?.leave();
	}

	async function leave() {
		await stop();
		members = [];
		lines = [];
		waves = {};
		presence = {};
		silenced = {};
		watching = {};
		frames.clear();
		muted = deafened = camera = false;
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
				[sdk, voiceSdk, videoSdk] = await Promise.all([
					import('peerconnect'),
					import('peerconnect/voice'),
					import('peerconnect/video')
				]);
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
		return () => {
			disposed = true;
			void stop();
		};
	});
</script>

{#snippet icon(
	name:
		| 'mic'
		| 'mic-off'
		| 'headphones'
		| 'headphones-off'
		| 'speaker'
		| 'speaker-off'
		| 'camera'
		| 'camera-off'
)}
	<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
		{#if name === 'camera' || name === 'camera-off'}
			<rect x="3" y="6.5" width="13" height="11" rx="2.5" />
			<path d="M16 10.5l5-3v9l-5-3z" />
			{#if name === 'camera-off'}<path d="M4 4l16 16" />{/if}
		{:else if name === 'mic' || name === 'mic-off'}
			<rect x="9" y="3" width="6" height="11" rx="3" />
			<path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21M8.5 21h7" />
			{#if name === 'mic-off'}<path d="M4 4l16 16" />{/if}
		{:else if name === 'headphones' || name === 'headphones-off'}
			<path d="M4 15v-3a8 8 0 0 1 16 0v3" />
			<path d="M4 15h3v5H5a1 1 0 0 1-1-1zM20 15h-3v5h2a1 1 0 0 0 1-1z" />
			{#if name === 'headphones-off'}<path d="M4 4l16 16" />{/if}
		{:else}
			<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" />
			{#if name === 'speaker'}
				<path d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11" />
			{:else}
				<path d="M16 9.5l5 5M21 9.5l-5 5" />
			{/if}
		{/if}
	</svg>
{/snippet}

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
				<button class="button danger leave" onclick={leave}>{t.room.leave}</button>
			</header>

			<ul class="members">
				{#each people as person (person.id)}
					{@const mine = person.id === me}
					{@const off = mine ? muted : !!presence[person.id]?.muted || !!silenced[person.id]}
					{@const talking = !off && speaking(person.id)}
					<li class="person" class:mine class:talking class:off>
						{#if mine && camera && cameraStream}
							<video class="screen mirror" use:preview={cameraStream} autoplay muted playsinline
							></video>
						{:else if !mine && watching[person.id]}
							<canvas class="screen" use:screen={person.id}
								>{t.room.video.replace('{name}', person.name)}</canvas
							>
						{/if}
						<span class="avatar" aria-hidden="true">{person.name.slice(0, 1).toUpperCase()}</span>
						<span class="who">
							<span class="name"
								>{person.name}{#if mine}<em>&nbsp;· {t.room.you}</em>{/if}</span
							>
							<svg
								class="wave"
								viewBox="0 0 {BARS * 4} 24"
								preserveAspectRatio="none"
								aria-hidden="true"
							>
								{#each waves[person.id] ?? [] as v, i (i)}
									{@const h = Math.max(2, v * 22)}
									<rect x={i * 4 + 0.5} y={12 - h / 2} width="3" height={h} rx="1.5" />
								{/each}
							</svg>
						</span>
						<span class="actions">
							{#if mine}
								{#if voice}
									<button
										class="toggle"
										class:on={muted}
										aria-pressed={muted}
										aria-label={muted ? t.room.micOff : t.room.micOn}
										title={muted ? t.room.micOff : t.room.micOn}
										onclick={toggleMic}>{@render icon(muted ? 'mic-off' : 'mic')}</button
									>
									<button
										class="toggle"
										class:on={deafened}
										aria-pressed={deafened}
										aria-label={deafened ? t.room.hearOff : t.room.hearOn}
										title={deafened ? t.room.hearOff : t.room.hearOn}
										onclick={toggleSpeaker}
										>{@render icon(deafened ? 'headphones-off' : 'headphones')}</button
									>
								{/if}
								{#if video && typeof VideoEncoder === 'function'}
									<button
										class="toggle camera"
										class:live={camera}
										aria-pressed={camera}
										aria-label={camera ? t.room.cameraOn : t.room.cameraOff}
										title={camera ? t.room.cameraOn : t.room.cameraOff}
										onclick={toggleCamera}>{@render icon(camera ? 'camera' : 'camera-off')}</button
									>
								{/if}
							{:else}
								{#if presence[person.id]?.muted}
									<span class="state" title={t.room.theirMicOff.replace('{name}', person.name)}
										>{@render icon('mic-off')}<span class="sr"
											>{t.room.theirMicOff.replace('{name}', person.name)}</span
										></span
									>
								{/if}
								{#if presence[person.id]?.deafened}
									<span class="state" title={t.room.theirHearingOff.replace('{name}', person.name)}
										>{@render icon('headphones-off')}<span class="sr"
											>{t.room.theirHearingOff.replace('{name}', person.name)}</span
										></span
									>
								{/if}
								{#if voice}
									{@const label = (
										silenced[person.id] ? t.room.unmuteThem : t.room.muteThem
									).replace('{name}', person.name)}
									<button
										class="toggle"
										class:on={silenced[person.id]}
										aria-pressed={!!silenced[person.id]}
										aria-label={label}
										title={label}
										onclick={() => toggleThem(person.id)}
										>{@render icon(silenced[person.id] ? 'speaker-off' : 'speaker')}</button
									>
								{/if}
							{/if}
						</span>
					</li>
				{/each}
			</ul>
			{#if people.length < 2}<p class="hint">{t.room.alone}</p>{/if}
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

	.count {
		color: var(--muted);
	}

	.leave {
		margin-left: auto;
		min-height: 36px;
		padding: 0 16px;
		font-size: 0.85rem;
	}

	.members {
		display: grid;
		gap: 10px;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	/* One card per person. Grey when quiet, green while speaking, red when
	   their microphone is off; our own card has a tinted background. */
	.person {
		--state: var(--muted);
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 12px;
		padding: 10px 12px;
		border: 1px solid var(--line);
		border-radius: 16px;
		transition: border-color 0.15s;
	}

	.person.mine {
		background: color-mix(in srgb, var(--accent) 7%, transparent);
	}

	.person.talking {
		--state: var(--accent);
		border-color: var(--accent);
	}

	.person.off {
		--state: #c2410c;
	}

	/* Video, when there is any, across the top of the card. */
	.screen {
		display: block;
		flex: 1 0 100%;
		width: 100%;
		aspect-ratio: 16 / 9;
		border-radius: 10px;
		background: #000;
		object-fit: cover;
	}

	.screen.mirror {
		transform: scaleX(-1);
	}

	.toggle.camera.live {
		border-color: var(--accent);
		color: var(--accent);
	}

	.avatar {
		display: grid;
		place-items: center;
		flex: none;
		width: 40px;
		height: 40px;
		border-radius: 50%;
		background: var(--line);
		font-weight: 700;
		transition: box-shadow 0.15s;
	}

	.person.talking .avatar {
		background: var(--accent);
		color: var(--bg);
		box-shadow: 0 0 0 4px color-mix(in srgb, var(--accent) 30%, transparent);
	}

	.who {
		display: grid;
		flex: 1;
		gap: 4px;
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

	.wave {
		width: 100%;
		height: 24px;
		fill: var(--state);
		transition: fill 0.15s;
	}

	.person.off .wave {
		opacity: 0.4;
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 6px;
	}

	.icon {
		width: 20px;
		height: 20px;
		fill: none;
		stroke: currentColor;
		stroke-width: 1.8;
		stroke-linecap: round;
		stroke-linejoin: round;
	}

	.toggle,
	.state {
		display: grid;
		place-items: center;
		width: 40px;
		height: 40px;
		border-radius: 50%;
	}

	.toggle {
		border: 1px solid var(--line-strong);
		background: var(--bg);
		color: var(--ink);
		cursor: pointer;
	}

	.toggle:hover {
		border-color: var(--accent);
	}

	.toggle.on {
		border-color: #c2410c;
		background: color-mix(in srgb, #c2410c 12%, var(--bg));
		color: #c2410c;
	}

	.state {
		width: 28px;
		height: 28px;
		color: #c2410c;
	}

	.state .icon {
		width: 18px;
		height: 18px;
	}

	.sr {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
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
