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
	/** Members in a group at most (GROUP.md §3.2). */
	const CAPACITY = 4;
	/** Bars in a waveform, one every 40 ms: the last 1.6 s. */
	const BARS = 40;
	/** Of which a tile's name tag shows the latest. */
	const WAVE = 14;
	/** Above this level (0–1, see `level`), someone counts as speaking. */
	const SPEAKING = 0.25;
	/** How long a message stays over the video while the chat is closed. */
	const COMMENT = 9000;
	/** The space between tiles in the gallery, as in the CSS. */
	const GAP = 10;
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
		/** In order of arrival: the key. */
		n: number;
		from: string;
		name: string;
		text: string;
		me: boolean;
		/** When it arrived (`Date.now()`). */
		time: number;
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
	/** The canvas each member's video is drawn into. */
	// eslint-disable-next-line svelte/prefer-svelte-reactivity -- bookkeeping, not rendered
	const screens = new Map<string, HTMLCanvasElement>();
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
	/** Our microphone is off (it is until we turn it on). */
	let muted = $state(true);
	/** An AudioContext playing to the chosen speakers, if one was chosen. */
	let output: AudioContext | undefined;

	// The device check before joining: nothing is turned on unless asked.
	interface Devices {
		mic: MediaDeviceInfo[];
		camera: MediaDeviceInfo[];
		speakers: MediaDeviceInfo[];
	}
	let devices: Devices = $state.raw({ mic: [], camera: [], speakers: [] });
	let micDevice = $state('');
	let cameraDevice = $state('');
	let speakerDevice = $state('');
	let micOnJoin = $state(false);
	let cameraOnJoin = $state(false);
	let testMic: MediaStream | undefined = $state.raw();
	let testCamera: MediaStream | undefined = $state.raw();
	let testLevel = $state(0);
	let testContext: AudioContext | undefined;
	let testFrame = 0;
	const canChooseSpeakers =
		typeof AudioContext === 'function' && 'setSinkId' in AudioContext.prototype;
	let deafened = $state(false);
	let camera = $state(false);
	/** Our camera faces us (shown mirrored), or away (the rear camera, shown as it is). */
	let facingUs = $state(true);
	let switching = $state(false);
	let cameraStream: MediaStream | undefined = $state.raw();
	/** Members whose video is coming in now. */
	let watching: Record<string, boolean> = $state({});
	// The meeting's layout, as in the usual video meeting apps.
	/** Everyone in a grid, or one large (the one speaking, or pinned) above the others. */
	let view: 'gallery' | 'speaker' = $state('gallery');
	/** Whom we chose to see large; otherwise the latest to speak. */
	let pinned: string | null = $state(null);
	let latest: string | null = $state(null);
	/** Whom we watch across the whole window. */
	let maximized: string | null = $state(null);
	let chatOpen = $state(false);
	let unread = $state(0);
	let inviteOpen = $state(false);
	let lines: Line[] = $state([]);
	let said = 0;
	/** Now, a second at a time: which messages are still over the video. */
	let clock = $state(Date.now());
	/** Width over height of each member's picture, as it arrives. */
	let shapes: Record<string, number> = $state({});
	/** The gallery's size, to lay the tiles out in. */
	let area = $state({ width: 0, height: 0 });
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
	const others = $derived(people.filter((m) => m.id !== me));
	/** Who is shown large in the speaker view. */
	const featured = $derived(
		[pinned, latest, others.find((m) => watching[m.id])?.id, others[0]?.id, me].find(
			(id) => id && people.some((m) => m.id === id)
		) ?? me
	);
	/**
	 * The gallery as the usual meeting apps lay it out: tiles in the shape of
	 * the pictures, as many columns as make them largest. Two wide pictures on
	 * a phone held upright go one above the other; two tall ones side by side.
	 */
	const gallery = $derived.by(() => {
		const count = Math.max(2, people.length);
		const seen = people
			.filter((m) => hasVideo(m.id) && shapes[m.id])
			.map((m) => shapes[m.id])
			.sort((a, b) => a - b);
		const middle = seen.length ? seen[seen.length >> 1] : 16 / 9;
		const aspect = Math.min(16 / 9, Math.max(9 / 16, middle));
		let best = { columns: 1, width: 0 };
		for (let columns = 1; columns <= count; columns++) {
			const rows = Math.ceil(count / columns);
			const width = Math.min(
				(area.width - GAP * (columns - 1)) / columns,
				((area.height - GAP * (rows - 1)) / rows) * aspect
			);
			if (width > best.width) best = { columns, width };
		}
		return { width: Math.floor(best.width), aspect };
	});
	/** Messages shown over the video while the chat is closed. */
	const comments = $derived(
		chatOpen ? [] : lines.filter((l) => clock - l.time < COMMENT + 1000).slice(-5)
	);

	/** A colour of each member's own, the same everywhere. */
	function hue(id: string): number {
		let h = 0;
		for (const c of id) h = (h * 31 + c.charCodeAt(0)) % 360;
		return h;
	}

	function clockTime(time: number): string {
		return new Date(time).toLocaleTimeString(i18n.lang, { hour: '2-digit', minute: '2-digit' });
	}

	/** Measures the gallery as it changes size. */
	function measure(node: HTMLElement) {
		const observer = new ResizeObserver(([entry]) => {
			const { width, height } = entry.contentRect;
			if (width !== area.width || height !== area.height) area = { width, height };
		});
		observer.observe(node);
		return { destroy: () => observer.disconnect() };
	}

	function hasVideo(id: string): boolean {
		return id === me ? camera : !!watching[id];
	}

	/** Shows someone large (or back to everyone, if they already are). */
	function enlarge(id: string) {
		if (view === 'speaker' && pinned === id) {
			pinned = null;
			view = 'gallery';
		} else {
			pinned = id;
			view = 'speaker';
		}
	}

	function toggleChat() {
		chatOpen = !chatOpen;
		if (chatOpen) {
			unread = 0;
			void scrollDown();
		}
	}

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

	async function listDevices() {
		try {
			const all = await navigator.mediaDevices.enumerateDevices();
			devices = {
				mic: all.filter((d) => d.kind === 'audioinput'),
				camera: all.filter((d) => d.kind === 'videoinput'),
				speakers: all.filter((d) => d.kind === 'audiooutput')
			};
		} catch {
			// no devices to list: the defaults it is
		}
	}

	function label(device: MediaDeviceInfo, i: number): string {
		return device.label || `${t.devices.device} ${i + 1}`;
	}

	/** Listens to the chosen microphone, showing its level, until stopped. */
	async function testMicrophone() {
		if (testMic) return stopMicTest();
		// Made within the click, so that the browser lets it run.
		const context = new AudioContext();
		try {
			testMic = await navigator.mediaDevices.getUserMedia({
				audio: micDevice ? { deviceId: { exact: micDevice } } : true
			});
		} catch {
			void context.close();
			raise('mic');
			return;
		}
		problems = problems.filter((p) => p !== 'mic');
		void listDevices();
		testContext = context;
		void context.resume();
		const analyser = new AnalyserNode(testContext, { fftSize: 1024 });
		testContext.createMediaStreamSource(testMic).connect(analyser);
		const samples = new Float32Array(analyser.fftSize);
		testFrame = window.setInterval(() => {
			analyser.getFloatTimeDomainData(samples);
			let sum = 0;
			for (const v of samples) sum += v * v;
			// Held at its peak, falling slowly, so that a short sound shows.
			testLevel = Math.max(level(Math.sqrt(sum / samples.length)), testLevel * 0.85);
		}, 50);
	}

	function stopMicTest() {
		clearInterval(testFrame);
		testMic?.getTracks().forEach((track) => track.stop());
		testMic = undefined;
		void testContext?.close();
		testContext = undefined;
		testLevel = 0;
	}

	/** Shows the chosen camera, until stopped. */
	async function testCameraNow() {
		if (testCamera) return stopCameraTest();
		try {
			testCamera = await navigator.mediaDevices.getUserMedia({
				video: cameraDevice
					? { deviceId: { exact: cameraDevice }, width: VIDEO.width, height: VIDEO.height }
					: { width: VIDEO.width, height: VIDEO.height }
			});
		} catch {
			raise('camera');
			return;
		}
		problems = problems.filter((p) => p !== 'camera');
		void listDevices();
	}

	function stopCameraTest() {
		testCamera?.getTracks().forEach((track) => track.stop());
		testCamera = undefined;
	}

	/** A short chime on the chosen speakers. */
	async function testSpeakers() {
		const context = new AudioContext();
		void context.resume();
		if (speakerDevice && canChooseSpeakers) {
			await (context as AudioContext & { setSinkId(id: string): Promise<void> })
				.setSinkId(speakerDevice)
				.catch(() => {});
		}
		const gain = new GainNode(context, { gain: 0 });
		gain.connect(context.destination);
		const now = context.currentTime;
		for (const [i, f] of [523.25, 659.25, 783.99].entries()) {
			const tone = new OscillatorNode(context, { frequency: f });
			tone.connect(gain);
			tone.start(now + i * 0.18);
			tone.stop(now + i * 0.18 + 0.16);
		}
		gain.gain.setValueAtTime(0, now);
		gain.gain.linearRampToValueAtTime(0.25, now + 0.02);
		gain.gain.setValueAtTime(0.25, now + 0.5);
		gain.gain.linearRampToValueAtTime(0, now + 0.6);
		setTimeout(() => void context.close(), 1000);
	}

	/** Shows a stream in a <video>, while it is on the page. */
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
		stopMicTest();
		stopCameraTest();

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
			if (pinned === e.detail.member) pinned = null;
			if (maximized === e.detail.member) maximized = null;
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
			lines = [
				...lines,
				{
					n: said++,
					from,
					name: who,
					text: decoder.decode(data),
					me: from === me,
					time: Date.now()
				}
			];
			clock = Date.now();
			if (!chatOpen && from !== me) unread++;
			void scrollDown();
		});
		c.addEventListener('full', () => current() && raise('full'));
		c.addEventListener('replaced', () => current() && raise('replaced'));

		// Listening at once; the microphone only if asked for.
		try {
			if (speakerDevice && canChooseSpeakers) {
				output = new AudioContext({ sampleRate: 48_000, latencyHint: 'interactive' });
				await (output as AudioContext & { setSinkId(id: string): Promise<void> })
					.setSinkId(speakerDevice)
					.catch(() => {});
			}
			voice = await voiceSdk!.startVoice(c, { microphone: false, context: output });
		} catch {
			raise('codecs');
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
				// The latest of the others to speak, for the speaker view.
				const talker = members.find((m) => m.id !== me && speaking(m.id))?.id;
				if (talker && talker !== latest) latest = talker;
				for (const [id, canvas] of screens) {
					const shape = canvas.width && canvas.height ? canvas.width / canvas.height : 0;
					if (shape && Math.abs(shape - (shapes[id] ?? 0)) > 0.01) shapes[id] = shape;
				}
				if (Date.now() - clock >= 1000) clock = Date.now();
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
		chatOpen = matchMedia('(min-width: 1000px)').matches;
		phase = 'room';
		if (micOnJoin) await toggleMic();
		if (cameraOnJoin) await toggleCamera();
	}

	/** Tells the others whether we are muted or deafened. */
	function announce() {
		try {
			conn?.send('command', JSON.stringify({ type: 'presence', muted, deafened, camera }));
		} catch {
			// left
		}
	}

	/** Turns the microphone on (asking for it) or off (letting it go). */
	async function toggleMic() {
		if (!voice) return;
		try {
			await voice.setMicrophone(muted, { deviceId: micDevice || undefined });
			muted = !muted;
			problems = problems.filter((p) => p !== 'mic');
		} catch {
			raise('mic');
		}
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
			await useCamera(cameraDevice ? { deviceId: { exact: cameraDevice } } : {});
			camera = true;
			problems = problems.filter((p) => p !== 'camera');
			announce();
			void listDevices(); // now with names, and how many there are to switch between
		} catch {
			cameraStream?.getTracks().forEach((t) => t.stop());
			cameraStream = undefined;
			raise('camera');
		}
	}

	/** Opens a camera (letting go of the one before) and sends it. */
	async function useCamera(which: MediaTrackConstraints) {
		// Phones cannot open a second camera while one is open.
		cameraStream?.getTracks().forEach((t) => t.stop());
		const stream = await navigator.mediaDevices.getUserMedia({
			video: { width: VIDEO.width, height: VIDEO.height, frameRate: VIDEO.framerate, ...which }
		});
		cameraStream = stream;
		const settings = stream.getVideoTracks()[0]?.getSettings() ?? {};
		facingUs = settings.facingMode !== 'environment';
		if (settings.deviceId) cameraDevice = settings.deviceId;
		await video!.setStream(stream);
	}

	/**
	 * The other camera: front to back and back again where the browser
	 * knows which way cameras face (phones), else the next one in the list.
	 */
	async function switchCamera() {
		if (!video || !camera || switching) return;
		switching = true;
		const before = cameraDevice;
		const facing = cameraStream?.getVideoTracks()[0]?.getSettings().facingMode;
		try {
			if (facing === 'user' || facing === 'environment') {
				try {
					await useCamera({ facingMode: { exact: facing === 'user' ? 'environment' : 'user' } });
					return;
				} catch {
					// no camera facing the other way: try the list
				}
			}
			const cameras = devices.camera.filter((d) => d.deviceId);
			const next = cameras[(cameras.findIndex((d) => d.deviceId === before) + 1) % cameras.length];
			await useCamera(next ? { deviceId: { exact: next.deviceId } } : {});
		} catch {
			// The other camera would not open: back to the one we had.
			try {
				await useCamera(before ? { deviceId: { exact: before } } : {});
			} catch {
				raise('camera');
			}
		} finally {
			switching = false;
		}
	}

	/** Draws a member's video into this canvas while it is on the page. */
	function screen(node: HTMLCanvasElement, id: string) {
		video?.attach(id, node);
		screens.set(id, node);
		return {
			destroy() {
				// Moving to another tile, the new canvas may already be attached.
				if (screens.get(id) !== node) return;
				screens.delete(id);
				video?.attach(id, null);
			}
		};
	}

	/** Turns our picture for everyone, or someone else's for us, a quarter turn at a time. */
	function rotate(id: string) {
		if (!video) return;
		if (id === me) video.setRotation(video.rotation + 90);
		else video.rotateView(id, video.viewRotation(id) + 90);
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
		void output?.close();
		output = undefined;
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
		deafened = camera = false;
		muted = true;
		facingUs = true;
		view = 'gallery';
		pinned = latest = maximized = null;
		unread = 0;
		inviteOpen = false;
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
		void listDevices();
		navigator.mediaDevices?.addEventListener('devicechange', listDevices);
		return () => {
			disposed = true;
			navigator.mediaDevices?.removeEventListener('devicechange', listDevices);
			stopMicTest();
			stopCameraTest();
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
		| 'rotate'
		| 'switch-camera'
		| 'expand'
		| 'shrink'
		| 'pin'
		| 'grid'
		| 'speaker-view'
		| 'chat'
		| 'invite'
		| 'send'
)}
	<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
		{#if name === 'expand'}
			<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5" />
		{:else if name === 'shrink'}
			<path d="M9 4v5H4M15 4v5h5M9 20v-5H4M15 20v-5h5" />
		{:else if name === 'pin'}
			<path d="M9 4h6l-1 6 4 3H6l4-3zM12 13v7" />
		{:else if name === 'grid'}
			<rect x="4" y="4" width="7" height="7" rx="1.5" /><rect
				x="13"
				y="4"
				width="7"
				height="7"
				rx="1.5"
			/>
			<rect x="4" y="13" width="7" height="7" rx="1.5" /><rect
				x="13"
				y="13"
				width="7"
				height="7"
				rx="1.5"
			/>
		{:else if name === 'speaker-view'}
			<rect x="3" y="4" width="18" height="11" rx="1.5" />
			<rect x="3" y="17" width="5" height="3" rx="1" /><rect
				x="9.5"
				y="17"
				width="5"
				height="3"
				rx="1"
			/>
			<rect x="16" y="17" width="5" height="3" rx="1" />
		{:else if name === 'chat'}
			<path d="M4 5h16v11H9l-5 4z" />
		{:else if name === 'send'}
			<path d="M4 12l16-8-6 16-2.5-6.5z" /><path d="M11.5 13.5L20 4" />
		{:else if name === 'invite'}
			<circle cx="10" cy="8" r="3.5" /><path d="M3.5 19a6.5 6.5 0 0 1 13 0M19 8v6M16 11h6" />
		{:else if name === 'switch-camera'}
			<path d="M4 8h3l1.5-2h7L17 8h3v11H4z" />
			<path d="M9.5 12.5a2.8 2.8 0 0 1 5-1.2M14.5 14.5a2.8 2.8 0 0 1-5 1.2" />
			<path d="M14.8 9.6v1.9h-1.9M9.2 17.4v-1.9h1.9" />
		{:else if name === 'rotate'}
			<path d="M20 12a8 8 0 1 1-2.34-5.66" />
			<path d="M20 4v4.5h-4.5" />
		{:else if name === 'camera' || name === 'camera-off'}
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

<svelte:window
	onkeydown={(e) => {
		if (e.key === 'Escape' && maximized) maximized = null;
	}}
/>

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

		<fieldset class="devices">
			<legend>{t.devices.title}</legend>
			<p class="hint">{t.devices.hint}</p>

			<div class="device">
				<span class="kind">{@render icon('mic')}{t.devices.mic}</span>
				<select
					bind:value={micDevice}
					aria-label={t.devices.mic}
					onchange={() => testMic && (stopMicTest(), testMicrophone())}
				>
					<option value="">{t.devices.default}</option>
					{#each devices.mic as d, i (d.deviceId)}
						{#if d.deviceId && d.deviceId !== 'default'}<option value={d.deviceId}
								>{label(d, i)}</option
							>{/if}
					{/each}
				</select>
				<button type="button" class="button secondary small" onclick={testMicrophone}
					>{testMic ? t.devices.stop : t.devices.testMic}</button
				>
				{#if testMic}
					<span
						class="meter"
						role="meter"
						aria-label={t.devices.level}
						aria-valuemin="0"
						aria-valuemax="1"
						aria-valuenow={testLevel}
						><span style:width="{Math.round(testLevel * 100)}%"></span></span
					>
				{/if}
				<label class="check"
					><input type="checkbox" bind:checked={micOnJoin} />{t.devices.micOnJoin}</label
				>
			</div>

			<div class="device">
				<span class="kind">{@render icon('camera')}{t.devices.camera}</span>
				<select
					bind:value={cameraDevice}
					aria-label={t.devices.camera}
					onchange={() => testCamera && (stopCameraTest(), testCameraNow())}
				>
					<option value="">{t.devices.default}</option>
					{#each devices.camera as d, i (d.deviceId)}
						{#if d.deviceId}<option value={d.deviceId}>{label(d, i)}</option>{/if}
					{/each}
				</select>
				<button type="button" class="button secondary small" onclick={testCameraNow}
					>{testCamera ? t.devices.stop : t.devices.testCamera}</button
				>
				{#if testCamera}
					<video class="test-preview" use:preview={testCamera} autoplay muted playsinline></video>
				{/if}
				<label class="check"
					><input type="checkbox" bind:checked={cameraOnJoin} />{t.devices.cameraOnJoin}</label
				>
			</div>

			<div class="device">
				<span class="kind">{@render icon('speaker')}{t.devices.speakers}</span>
				{#if canChooseSpeakers && devices.speakers.length}
					<select bind:value={speakerDevice} aria-label={t.devices.speakers}>
						<option value="">{t.devices.default}</option>
						{#each devices.speakers as d, i (d.deviceId)}
							{#if d.deviceId && d.deviceId !== 'default'}<option value={d.deviceId}
									>{label(d, i)}</option
								>{/if}
						{/each}
					</select>
				{/if}
				<button type="button" class="button secondary small" onclick={testSpeakers}
					>{t.devices.testSpeakers}</button
				>
			</div>
		</fieldset>
	</form>
{:else}
	{#snippet tile(person: Member, size: 'grid' | 'large' | 'small' | 'whole')}
		{@const mine = person.id === me}
		{@const off = mine ? muted : !!presence[person.id]?.muted || !!silenced[person.id]}
		{@const talking = !off && speaking(person.id)}
		{@const shown = hasVideo(person.id) && (size === 'whole' || maximized !== person.id)}
		{@const turn = mine ? t.room.rotateMine : t.room.rotateTheirs.replace('{name}', person.name)}
		<li
			class="person tile {size}"
			class:mine
			class:talking
			class:off
			ondblclick={() => size !== 'whole' && enlarge(person.id)}
		>
			{#if shown}
				<!-- Ours as we send it (mirrored, as a mirror would be); theirs as they arrive. -->
				<canvas class="screen" class:mirror={mine && facingUs} use:screen={person.id}
					>{t.room.video.replace('{name}', person.name)}</canvas
				>
			{:else}
				<span class="avatar" style:--hue={hue(person.id)} aria-hidden="true"
					>{person.name.slice(0, 1).toUpperCase()}</span
				>
			{/if}
			<span class="corner">
				{#if shown && mine && devices.camera.length > 1}
					<button
						class="keep"
						aria-label={t.room.switchCamera}
						title={t.room.switchCamera}
						disabled={switching}
						onclick={switchCamera}>{@render icon('switch-camera')}</button
					>
				{/if}
				{#if shown}
					<button aria-label={turn} title={turn} onclick={() => rotate(person.id)}
						>{@render icon('rotate')}</button
					>
				{/if}
				{#if !mine && voice}
					{@const label = (silenced[person.id] ? t.room.unmuteThem : t.room.muteThem).replace(
						'{name}',
						person.name
					)}
					<button
						class:on={silenced[person.id]}
						aria-pressed={!!silenced[person.id]}
						aria-label={label}
						title={label}
						onclick={() => toggleThem(person.id)}
						>{@render icon(silenced[person.id] ? 'speaker-off' : 'speaker')}</button
					>
				{/if}
				{#if size === 'whole'}
					<button
						aria-label={t.room.exitWhole}
						title={t.room.exitWhole}
						onclick={() => (maximized = null)}>{@render icon('shrink')}</button
					>
				{:else}
					{#if shown}
						{@const whole = t.room.whole.replace('{name}', person.name)}
						<button aria-label={whole} title={whole} onclick={() => (maximized = person.id)}
							>{@render icon('expand')}</button
						>
					{/if}
					{#if people.length > 1}
						{@const pin =
							size === 'large' && pinned === person.id
								? t.room.unpin
								: t.room.pin.replace('{name}', person.name)}
						<button
							class="keep"
							class:on={pinned === person.id}
							aria-label={pin}
							title={pin}
							onclick={() => enlarge(person.id)}>{@render icon('pin')}</button
						>
					{/if}
				{/if}
			</span>
			<span class="tag">
				{#if mine && muted}<span class="flag" title={t.room.micOff}>{@render icon('mic-off')}</span
					>{/if}
				{#if !mine && presence[person.id]?.muted}
					<span class="state" title={t.room.theirMicOff.replace('{name}', person.name)}
						>{@render icon('mic-off')}<span class="sr"
							>{t.room.theirMicOff.replace('{name}', person.name)}</span
						></span
					>
				{/if}
				{#if !mine && presence[person.id]?.deafened}
					<span class="state" title={t.room.theirHearingOff.replace('{name}', person.name)}
						>{@render icon('headphones-off')}<span class="sr"
							>{t.room.theirHearingOff.replace('{name}', person.name)}</span
						></span
					>
				{/if}
				<span class="name"
					>{person.name}{#if mine}<em>&nbsp;· {t.room.you}</em>{/if}</span
				>
				<svg class="wave" viewBox="0 0 {WAVE * 4} 24" preserveAspectRatio="none" aria-hidden="true">
					{#each (waves[person.id] ?? []).slice(-WAVE) as v, i (i)}
						{@const h = Math.max(2, v * 22)}
						<rect x={i * 4 + 0.5} y={12 - h / 2} width="3" height={h} rx="1.5" />
					{/each}
				</svg>
			</span>
		</li>
	{/snippet}

	<!-- Messages over the video, as comments over a live stream, gone after a while. -->
	{#snippet overlay(shown: Line[])}
		<ol class="comments" aria-hidden="true">
			{#each shown as line (line.n)}
				<li style:--hue={hue(line.from)}><b>{line.name}</b>{line.text}</li>
			{/each}
		</ol>
	{/snippet}

	{#snippet composer(id: string | undefined)}
		<form class="composer" onsubmit={send}>
			<input
				{id}
				bind:value={draft}
				maxlength="2000"
				autocomplete="off"
				enterkeyhint="send"
				placeholder={t.chat.placeholder}
				aria-label={t.chat.placeholder}
			/>
			<button type="submit" disabled={!draft.trim()} aria-label={t.chat.send} title={t.chat.send}
				>{@render icon('send')}</button
			>
		</form>
	{/snippet}

	{#snippet control(
		label: string,
		short: string,
		glyph: Parameters<typeof icon>[0],
		onclick: () => void,
		state: 'on' | 'off' | 'live' | '' = '',
		badge = 0
	)}
		<button class="control {state}" aria-label={label} title={label} {onclick}>
			<span class="glyph"
				>{@render icon(glyph)}{#if badge}<b class="badge">{badge}</b>{/if}</span
			>
			<span class="short">{short}</span>
		</button>
	{/snippet}

	<div class="meeting card" class:chat-open={chatOpen}>
		<header class="bar">
			<span class="status {status}">{t.state[status]}</span>
			<span class="count">{members.length} / {CAPACITY}</span>
			<span class="views" role="group" aria-label={t.room.layout}>
				<button
					class:selected={view === 'gallery'}
					aria-pressed={view === 'gallery'}
					aria-label={t.room.gallery}
					onclick={() => ((view = 'gallery'), (pinned = null))}
					>{@render icon('grid')}<span>{t.room.gallery}</span></button
				>
				<button
					class:selected={view === 'speaker'}
					aria-pressed={view === 'speaker'}
					aria-label={t.room.speakerView}
					onclick={() => (view = 'speaker')}
					>{@render icon('speaker-view')}<span>{t.room.speakerView}</span></button
				>
			</span>
		</header>

		<div class="body">
			<section class="stage" aria-label={t.room.people}>
				{#if view === 'speaker' && people.length > 1}
					{@const big = people.find((m) => m.id === featured)!}
					<ul class="large-one">{@render tile(big, 'large')}</ul>
					<ul class="strip">
						{#each people.filter((m) => m.id !== featured) as person (person.id)}
							{@render tile(person, 'small')}
						{/each}
					</ul>
				{:else}
					<ul
						class="grid"
						use:measure
						style:--tile-width={gallery.width ? `${gallery.width}px` : null}
						style:--aspect={gallery.aspect}
					>
						{#each people as person (person.id)}
							{@render tile(person, 'grid')}
						{/each}
						{#if people.length < 2}
							<li class="alone">
								<p>{t.room.alone}</p>
								{#if qr}
									<svg
										class="qr"
										viewBox="0 0 {qr.size} {qr.size}"
										role="img"
										aria-label={t.invite.title}><path d={qr.d} /></svg
									>
								{/if}
								<button class="button secondary small" onclick={copyLink}
									>{copied ? t.invite.copied : t.invite.copy}</button
								>
							</li>
						{/if}
					</ul>
				{/if}
				{@render overlay(comments)}
			</section>

			{#if chatOpen}
				<aside class="chat" aria-label={t.chat.title}>
					<header>
						<h2>{t.chat.title}</h2>
						<button
							class="close"
							aria-label={t.room.closeChat}
							title={t.room.closeChat}
							onclick={toggleChat}>×</button
						>
					</header>
					<div class="log" bind:this={log}>
						{#each lines as line, i (line.n)}
							{@const before = lines[i - 1]}
							{@const first =
								!before || before.from !== line.from || line.time - before.time > 120_000}
							<div class="message" class:me={line.me} class:first style:--hue={hue(line.from)}>
								{#if first}
									{#if !line.me}
										<span class="avatar" aria-hidden="true"
											>{line.name.slice(0, 1).toUpperCase()}</span
										>
									{/if}
									<span class="who"
										>{#if !line.me}<b>{line.name}</b>{/if}<time>{clockTime(line.time)}</time></span
									>
								{/if}
								<p class="line"><span class="sr">{line.name}</span> {line.text}</p>
							</div>
						{:else}
							<div class="empty">
								{@render icon('chat')}
								<p>{t.chat.empty}</p>
							</div>
						{/each}
					</div>
					{@render composer('voice-message')}
				</aside>
				<button class="scrim" aria-label={t.room.closeChat} onclick={toggleChat}></button>
			{/if}
		</div>

		<nav class="controls" aria-label={t.room.controls}>
			{#if voice}
				{@render control(
					muted ? t.room.micOff : t.room.micOn,
					muted ? t.room.short.unmute : t.room.short.mute,
					muted ? 'mic-off' : 'mic',
					toggleMic,
					muted ? 'off' : ''
				)}
			{/if}
			{#if video && typeof VideoEncoder === 'function'}
				{@render control(
					camera ? t.room.cameraOn : t.room.cameraOff,
					camera ? t.room.short.stopVideo : t.room.short.startVideo,
					camera ? 'camera' : 'camera-off',
					toggleCamera,
					camera ? 'live' : 'off'
				)}
			{/if}
			{#if voice}
				{@render control(
					deafened ? t.room.hearOff : t.room.hearOn,
					t.room.short.sound,
					deafened ? 'headphones-off' : 'headphones',
					toggleSpeaker,
					deafened ? 'off' : ''
				)}
			{/if}
			{@render control(
				t.room.chat,
				t.room.short.chat,
				'chat',
				toggleChat,
				chatOpen ? 'live' : '',
				unread
			)}
			{@render control(
				t.invite.title,
				t.room.short.invite,
				'invite',
				() => (inviteOpen = !inviteOpen),
				inviteOpen ? 'live' : ''
			)}
			<button class="button danger leave" onclick={leave}>{t.room.leave}</button>
			{#if inviteOpen}
				<div class="popover card" role="dialog" aria-label={t.invite.title}>
					{#if qr}
						<svg class="qr" viewBox="0 0 {qr.size} {qr.size}" role="img" aria-label={t.invite.title}
							><path d={qr.d} /></svg
						>
					{/if}
					<p class="link"><code>{link}</code></p>
					<button class="button secondary small" onclick={copyLink}
						>{copied ? t.invite.copied : t.invite.copy}</button
					>
				</div>
			{/if}
		</nav>
	</div>

	{#if maximized}
		{@const person = people.find((m) => m.id === maximized)}
		{#if person}
			<div
				class="full"
				role="dialog"
				aria-modal="true"
				aria-label={t.room.whole.replace('{name}', person.name)}
			>
				<ul>{@render tile(person, 'whole')}</ul>
				<!-- The chat, over the picture. -->
				{@render overlay(lines.filter((l) => clock - l.time < COMMENT + 1000).slice(-6))}
				<div class="say">{@render composer(undefined)}</div>
			</div>
		{/if}
	{/if}
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
		grid-template-columns: minmax(0, 1fr);
		justify-items: start;
		gap: 14px;
		max-width: 640px;
	}

	.join > label {
		font-weight: 600;
	}

	.devices {
		display: grid;
		gap: 12px;
		width: 100%;
		/* A fieldset is as wide as its content unless told otherwise. */
		min-width: 0;
		grid-template-columns: minmax(0, 1fr);
		margin: 8px 0 0;
		padding: 16px;
		border: 1px solid var(--line);
		border-radius: 16px;
	}

	.devices legend {
		padding: 0 6px;
		font-weight: 600;
	}

	.devices .hint {
		margin: 0;
	}

	.device {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px 12px;
	}

	.kind {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		min-width: 7.5em;
		font-weight: 600;
	}

	select {
		flex: 1 1 8em;
		min-width: 0;
		max-width: 100%;
		min-height: 38px;
		padding: 0 12px;
		border: 1px solid var(--line-strong);
		border-radius: 999px;
		background: var(--bg);
		color: var(--ink);
		font: inherit;
		font-size: 0.9rem;
	}

	.small {
		min-height: 38px;
		padding: 0 14px;
		font-size: 0.85rem;
	}

	.meter {
		flex: 1 0 100%;
		height: 8px;
		overflow: hidden;
		border-radius: 999px;
		background: var(--line);
	}

	.meter span {
		display: block;
		height: 100%;
		border-radius: inherit;
		background: var(--accent);
		transition: width 0.06s linear;
	}

	.test-preview {
		flex: 1 0 100%;
		width: 100%;
		min-width: 0;
		max-width: 320px;
		border-radius: 10px;
		background: #000;
		transform: scaleX(-1);
	}

	.check {
		display: inline-flex;
		flex: 1 0 100%;
		align-items: center;
		gap: 8px;
		font-size: 0.9rem;
		cursor: pointer;
	}

	.check input {
		flex: none;
		padding: 0;
		width: 18px;
		height: 18px;
		min-height: 0;
		accent-color: var(--accent);
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

	/* The meeting: a dark stage, as video looks best on, with the controls
	   below it and the chat beside it. */
	.meeting {
		--stage: #16181d;
		--tile: #24272e;
		--on-stage: #f3f4f6;
		--danger: #c2410c;
		position: relative;
		display: grid;
		grid-template-rows: auto minmax(0, 1fr) auto;
		height: min(82vh, 860px);
		min-height: 520px;
		overflow: hidden;
		background: var(--stage);
		color: var(--on-stage);
	}

	.bar {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 8px 14px;
		padding: 10px 14px;
		font-size: 0.88rem;
	}

	.status {
		font-weight: 600;
	}

	.status.online {
		color: #4ade80;
	}

	.count {
		opacity: 0.7;
	}

	.views {
		display: flex;
		gap: 4px;
		margin-left: auto;
		padding: 3px;
		border-radius: 999px;
		background: rgb(255 255 255 / 0.08);
	}

	.views button {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		min-height: 32px;
		padding: 0 12px;
		border: 0;
		border-radius: 999px;
		background: none;
		color: inherit;
		font: inherit;
		cursor: pointer;
	}

	.views button.selected {
		background: rgb(255 255 255 / 0.16);
	}

	.views .icon {
		width: 16px;
		height: 16px;
	}

	.body {
		position: relative;
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		min-height: 0;
	}

	.chat-open .body {
		grid-template-columns: minmax(0, 1fr) 320px;
	}

	.stage {
		position: relative;
		display: flex;
		flex-direction: column;
		gap: 10px;
		min-height: 0;
		padding: 4px 12px 12px;
		overflow: auto;
	}

	.stage ul {
		margin: 0;
		padding: 0;
		list-style: none;
	}

	/* Gallery: everyone the same size, the size worked out in the script
	   (`gallery`); a last row that is not full is centred. */
	.grid {
		display: flex;
		flex: 1;
		flex-wrap: wrap;
		justify-content: center;
		align-content: center;
		gap: 10px;
		min-height: 0;
		overflow: hidden;
	}

	.tile.grid,
	.alone {
		flex: none;
		width: var(--tile-width, min(100%, 640px));
		aspect-ratio: var(--aspect, 16 / 9);
	}

	/* Speaker view: one large, the others in a strip below. */
	.large-one {
		display: grid;
		flex: 1;
		min-height: 0;
	}

	.strip {
		display: flex;
		gap: 8px;
		overflow-x: auto;
		flex: none;
	}

	.tile {
		position: relative;
		display: flex;
		align-items: center;
		justify-content: center;
		overflow: hidden;
		border: 2px solid transparent;
		border-radius: 12px;
		background: var(--tile);
		aspect-ratio: 16 / 9;
		transition: border-color 0.15s;
	}

	.tile.large {
		aspect-ratio: auto;
		height: 100%;
		min-height: 220px;
	}

	.tile.small {
		flex: none;
		width: 168px;
	}

	.tile.talking {
		border-color: #4ade80;
	}

	.screen {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: contain;
		background: #000;
	}

	.screen.mirror {
		transform: scaleX(-1);
	}

	.avatar {
		display: grid;
		place-items: center;
		width: 72px;
		height: 72px;
		border-radius: 50%;
		background: hsl(var(--hue, 220) 32% 36%);
		color: #fff;
		font-size: 1.8rem;
		font-weight: 700;
	}

	.tile.talking .avatar {
		box-shadow: 0 0 0 4px rgb(74 222 128 / 0.45);
	}

	.tile.small .avatar {
		width: 36px;
		height: 36px;
		font-size: 1rem;
	}

	.tile.large .avatar,
	.tile.whole .avatar {
		width: 120px;
		height: 120px;
		font-size: 3rem;
	}

	.corner {
		position: absolute;
		top: 6px;
		right: 6px;
		display: flex;
		gap: 6px;
		opacity: 0.85;
	}

	.tile:hover .corner {
		opacity: 1;
	}

	.corner button {
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		border: 0;
		border-radius: 50%;
		background: rgb(0 0 0 / 0.5);
		color: #fff;
		cursor: pointer;
	}

	.corner button:hover {
		background: rgb(0 0 0 / 0.75);
	}

	.corner button.on {
		background: var(--danger);
	}

	.corner button:disabled {
		opacity: 0.5;
		cursor: wait;
	}

	.corner .icon {
		width: 18px;
		height: 18px;
	}

	.tile.small .corner button {
		width: 26px;
		height: 26px;
	}

	.tile.small .corner .icon {
		width: 14px;
		height: 14px;
	}

	.tag {
		position: absolute;
		bottom: 6px;
		left: 6px;
		display: flex;
		align-items: center;
		gap: 6px;
		max-width: calc(100% - 12px);
		padding: 3px 8px;
		border-radius: 8px;
		background: rgb(0 0 0 / 0.55);
		font-size: 0.82rem;
	}

	.name {
		overflow: hidden;
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.name em {
		opacity: 0.7;
		font-weight: 400;
		font-style: normal;
	}

	.wave {
		flex: none;
		width: 42px;
		height: 14px;
		fill: #9ca3af;
	}

	.tile.talking .wave {
		fill: #4ade80;
	}

	.tile.off .wave {
		opacity: 0.4;
	}

	.flag,
	.state {
		display: inline-grid;
		place-items: center;
		color: #f87171;
	}

	.flag .icon,
	.state .icon {
		width: 15px;
		height: 15px;
	}

	.tile.small .tag {
		font-size: 0.72rem;
	}

	.tile.small .wave {
		display: none;
	}

	.alone {
		display: grid;
		justify-items: center;
		align-content: center;
		gap: 10px;
		padding: 12px;
		overflow: hidden;
		border: 1px dashed rgb(255 255 255 / 0.25);
		border-radius: 12px;
		text-align: center;
		font-size: 0.9rem;
	}

	.alone .qr {
		width: clamp(64px, calc(var(--tile-width, 640px) / var(--aspect, 1.78) - 120px), 148px);
		height: auto;
		aspect-ratio: 1;
	}

	.alone p {
		margin: 0;
		opacity: 0.85;
	}

	.qr {
		width: 148px;
		height: 148px;
		flex: none;
		padding: 8px;
		border-radius: 10px;
		background: #fff;
		fill: #111;
	}

	/* Watching one member across the whole window. */
	.full {
		position: fixed;
		inset: 0;
		z-index: 50;
		display: grid;
		background: #000;
		color: #f3f4f6;
	}

	/* Messages over the video while the chat is closed: bottom left, newest
	   last, each gone after a while, as comments over a live stream. */
	.comments {
		position: absolute;
		left: 20px;
		/* Above the name tags. */
		bottom: 54px;
		z-index: 3;
		display: grid;
		gap: 6px;
		width: min(60%, 380px);
		margin: 0;
		padding: 0;
		list-style: none;
		pointer-events: none;
	}

	.stage:has(.strip) .comments {
		bottom: 160px;
	}

	.comments li {
		justify-self: start;
		max-width: 100%;
		padding: 6px 12px;
		border-radius: 16px;
		background: rgb(10 12 16 / 0.55);
		backdrop-filter: blur(6px);
		color: #fff;
		font-size: 0.9rem;
		line-height: 1.35;
		overflow-wrap: anywhere;
		text-shadow: 0 1px 2px rgb(0 0 0 / 0.5);
		animation:
			comment-in 0.3s cubic-bezier(0.2, 0.9, 0.3, 1.2),
			comment-out 0.8s ease-in 9s forwards;
	}

	.comments b {
		margin-right: 6px;
		color: hsl(var(--hue) 85% 76%);
		font-weight: 600;
	}

	@keyframes comment-in {
		from {
			opacity: 0;
			transform: translateY(14px) scale(0.96);
		}
	}

	@keyframes comment-out {
		to {
			opacity: 0;
			transform: translateY(-6px);
		}
	}

	.full .comments {
		bottom: 132px;
	}

	.say {
		position: absolute;
		left: 20px;
		right: 20px;
		bottom: 64px;
		z-index: 3;
		max-width: 420px;
	}

	.full ul {
		display: grid;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.tile.whole {
		aspect-ratio: auto;
		border: 0;
		border-radius: 0;
	}

	.tile.whole .corner {
		top: 14px;
		right: 14px;
	}

	.tile.whole .corner button {
		width: 42px;
		height: 42px;
	}

	.tile.whole .tag {
		bottom: 14px;
		left: 14px;
		font-size: 0.95rem;
	}

	/* The controls, as in any meeting app. */
	.controls {
		position: relative;
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		align-items: center;
		gap: 4px 6px;
		padding: 8px 12px 12px;
		background: rgb(0 0 0 / 0.25);
	}

	.control {
		display: grid;
		justify-items: center;
		gap: 2px;
		min-width: 64px;
		padding: 6px 8px;
		border: 0;
		border-radius: 10px;
		background: none;
		color: inherit;
		font: inherit;
		cursor: pointer;
	}

	.control:hover {
		background: rgb(255 255 255 / 0.08);
	}

	.glyph {
		position: relative;
		display: grid;
		place-items: center;
		width: 40px;
		height: 40px;
		border-radius: 50%;
		background: rgb(255 255 255 / 0.1);
	}

	.control.off .glyph {
		background: rgb(194 65 12 / 0.25);
		color: #fb923c;
	}

	.control.live .glyph {
		background: rgb(74 222 128 / 0.2);
		color: #4ade80;
	}

	.short {
		font-size: 0.72rem;
		opacity: 0.85;
		white-space: nowrap;
	}

	.badge {
		position: absolute;
		top: -2px;
		right: -4px;
		min-width: 18px;
		padding: 0 5px;
		border-radius: 999px;
		background: #ef4444;
		color: #fff;
		font-size: 0.7rem;
		line-height: 18px;
	}

	.leave {
		min-height: 40px;
		margin-left: 10px;
		padding: 0 18px;
		font-size: 0.88rem;
	}

	.popover {
		position: absolute;
		bottom: calc(100% + 8px);
		left: 50%;
		z-index: 5;
		display: grid;
		justify-items: center;
		gap: 10px;
		width: min(320px, calc(100% - 24px));
		padding: 16px;
		transform: translateX(-50%);
		border-color: rgb(255 255 255 / 0.1);
		background: #23262e;
		color: var(--on-stage);
	}

	.link {
		max-width: 100%;
		margin: 0;
		overflow-wrap: anywhere;
		font-size: 0.78rem;
	}

	/* The chat: a panel beside the stage, as dark as the stage. */
	.chat {
		display: grid;
		grid-template-rows: auto minmax(0, 1fr) auto;
		gap: 10px;
		min-height: 0;
		padding: 12px;
		border-left: 1px solid rgb(255 255 255 / 0.08);
		background: #1b1e24;
		color: var(--on-stage);
	}

	.chat header {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.chat h2 {
		font-size: 1rem;
	}

	.close {
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		border: 0;
		border-radius: 50%;
		background: none;
		color: inherit;
		font-size: 1.3rem;
		cursor: pointer;
	}

	.close:hover {
		background: rgb(255 255 255 / 0.08);
	}

	.log {
		display: flex;
		flex-direction: column;
		gap: 3px;
		overflow-y: auto;
		padding: 4px 2px;
		overscroll-behavior: contain;
	}

	/* Messages as bubbles: ours on the right, the others' on the left under
	   their name, one name for a run of messages. */
	.message {
		display: grid;
		grid-template-columns: 30px minmax(0, 1fr);
		column-gap: 8px;
		max-width: 100%;
		animation: message-in 0.2s ease-out;
	}

	.message.first {
		margin-top: 10px;
	}

	.message .avatar {
		grid-row: span 2;
		align-self: end;
		width: 30px;
		height: 30px;
		font-size: 0.8rem;
	}

	.who {
		display: flex;
		align-items: baseline;
		gap: 8px;
		margin: 0 0 3px 4px;
		font-size: 0.75rem;
		color: rgb(255 255 255 / 0.5);
	}

	.who b {
		color: hsl(var(--hue) 70% 75%);
		font-weight: 600;
	}

	.line {
		grid-column: 2;
		justify-self: start;
		max-width: 100%;
		margin: 0;
		padding: 7px 12px;
		border-radius: 4px 16px 16px 4px;
		background: #2c3039;
		line-height: 1.4;
		overflow-wrap: anywhere;
	}

	.message.first .line {
		border-top-left-radius: 16px;
	}

	.message.me {
		grid-template-columns: minmax(0, 1fr);
		justify-items: end;
	}

	.message.me .who {
		margin: 0 4px 3px 0;
	}

	.message.me .line {
		grid-column: 1;
		justify-self: end;
		border-radius: 16px 4px 4px 16px;
		background: #15803d;
		color: #fff;
	}

	.message.me.first .line {
		border-top-right-radius: 16px;
	}

	@keyframes message-in {
		from {
			opacity: 0;
			transform: translateY(6px);
		}
	}

	.empty {
		display: grid;
		justify-items: center;
		align-content: center;
		gap: 10px;
		flex: 1;
		color: rgb(255 255 255 / 0.4);
		text-align: center;
		font-size: 0.88rem;
	}

	.empty .icon {
		width: 34px;
		height: 34px;
		stroke-width: 1.4;
	}

	.empty p {
		margin: 0;
	}

	/* Where to write: one rounded field with the send button inside. */
	.composer {
		position: relative;
		display: flex;
	}

	.composer input {
		flex: 1;
		min-width: 0;
		min-height: 44px;
		padding: 0 48px 0 16px;
		border: 1px solid rgb(255 255 255 / 0.12);
		border-radius: 999px;
		background: rgb(255 255 255 / 0.06);
		color: #f3f4f6;
		font: inherit;
		transition:
			border-color 0.15s,
			background 0.15s;
	}

	.composer input::placeholder {
		color: rgb(255 255 255 / 0.45);
	}

	.composer input:focus {
		outline: none;
		border-color: rgb(74 222 128 / 0.7);
		background: rgb(255 255 255 / 0.09);
	}

	.composer button {
		position: absolute;
		top: 6px;
		right: 6px;
		display: grid;
		place-items: center;
		width: 32px;
		height: 32px;
		border: 0;
		border-radius: 50%;
		background: #22c55e;
		color: #06240f;
		cursor: pointer;
		transition:
			background 0.15s,
			transform 0.15s;
	}

	.composer button:hover:not(:disabled) {
		transform: scale(1.06);
	}

	.composer button:disabled {
		background: rgb(255 255 255 / 0.1);
		color: rgb(255 255 255 / 0.35);
		cursor: default;
	}

	.composer .icon {
		width: 16px;
		height: 16px;
		stroke-width: 2;
	}

	.say .composer input {
		background: rgb(10 12 16 / 0.5);
		backdrop-filter: blur(8px);
		border-color: rgb(255 255 255 / 0.2);
	}

	/* Behind the chat on a phone, to close it with a tap; not there otherwise. */
	.scrim {
		display: none;
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

	.sr {
		position: absolute;
		width: 1px;
		height: 1px;
		overflow: hidden;
		clip-path: inset(50%);
		white-space: nowrap;
	}

	@keyframes sheet-up {
		from {
			transform: translateY(100%);
		}
	}

	@keyframes fade-in {
		from {
			opacity: 0;
		}
	}

	/* Phones: the chat over the stage. */
	@media (max-width: 760px) {
		.meeting {
			height: min(88vh, 900px);
		}

		.tile.grid .corner button:not(.keep) {
			display: none;
		}

		.chat-open .body {
			grid-template-columns: minmax(0, 1fr);
		}

		/* The chat as a sheet from below, over the stage. */
		.chat {
			position: absolute;
			inset: 22% 0 0;
			z-index: 6;
			border: 0;
			border-radius: 18px 18px 0 0;
			box-shadow: 0 -12px 40px rgb(0 0 0 / 0.5);
			animation: sheet-up 0.25s cubic-bezier(0.2, 0.9, 0.3, 1);
		}

		.scrim {
			position: absolute;
			inset: 0;
			z-index: 5;
			display: block;
			border: 0;
			background: rgb(0 0 0 / 0.45);
			animation: fade-in 0.2s;
		}

		.comments {
			left: 14px;
			width: calc(100% - 64px);
		}

		.comments li {
			padding: 4px 10px;
			font-size: 0.8rem;
		}

		/* Three at most on a phone, to keep the pictures in sight. */
		.comments li:nth-last-child(n + 4) {
			display: none;
		}

		.views span {
			display: none;
		}

		.control {
			min-width: 52px;
			padding: 4px;
		}

		.glyph {
			width: 36px;
			height: 36px;
		}

		.leave {
			margin-left: 4px;
			padding: 0 14px;
		}
	}
</style>
