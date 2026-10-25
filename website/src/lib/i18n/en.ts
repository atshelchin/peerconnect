// English strings. `zh.ts` must have exactly the same shape (see `Dict`).
// Strings may contain `{relay}`-style placeholders and, where rendered with
// {@html}, simple inline markup.

export const en = {
	meta: {
		siteName: 'PeerConnect',
		home: {
			title: 'PeerConnect — end-to-end encrypted channels between two apps',
			description:
				'Pair two apps with a QR code, then talk end-to-end encrypted through a stateless WebSocket relay that cannot read or forge a thing. Rust, JavaScript, self-hosted.'
		},
		docs: {
			title: 'Get started — PeerConnect',
			description:
				'Build with PeerConnect in JavaScript or Rust, and run your own relay on Cloudflare Workers, Docker or a single binary.'
		},
		groups: {
			title: 'Groups: multiplayer, voice and state sync — PeerConnect',
			description:
				'End-to-end encrypted groups of up to 32 members: commands, chat, voice and state transfer over an untrusted relay. Built for games and teams: JavaScript, Rust and C.'
		},
		demo: {
			title: 'Live demo: pair two browsers — PeerConnect',
			description:
				'Pair two browsers with a QR code and chat end-to-end encrypted through a public PeerConnect relay. Runs the real protocol in WebAssembly.'
		},
		voice: {
			title: 'Live demo: group voice chat — PeerConnect',
			description:
				'Talk with up to ten people, end-to-end encrypted, through a public PeerConnect relay: Opus voice with an adaptive jitter buffer, and text chat. Open it on two devices.'
		},
		spec: {
			title: 'Protocol specification v1 — PeerConnect',
			description:
				'The PeerConnect v1 specification: rooms and invites, stateless relay admission, the Noise IKpsk1 handshake, the reliable stream and its security considerations.'
		},
		specGroups: {
			title: 'Groups specification v1 — PeerConnect',
			description:
				'The PeerConnect Groups v1 specification: group secrets and identities, slot admission, sender keys, four traffic classes, reliable streams and state transfer.'
		},
		notFound: {
			title: 'Page not found — PeerConnect',
			description: 'This page does not exist.'
		}
	},
	nav: {
		home: 'Home',
		docs: 'Docs',
		groups: 'Groups',
		demo: 'Demo',
		spec: 'Spec',
		github: 'GitHub',
		skip: 'Skip to content',
		language: 'Language',
		menu: 'Menu'
	},
	banner: {
		text: 'This page is available in English.',
		action: 'Switch to English',
		dismiss: 'Dismiss'
	},
	home: {
		hero: {
			eyebrow: 'Open protocol · v1 draft',
			title: 'Two apps. One room.<br />Nothing for the relay to read.',
			lead: 'PeerConnect pairs two applications — a phone and a laptop, a web app and a wallet, two services on different networks — through a WebSocket relay that can’t read, forge or replay what they say, and that stores nothing.',
			primary: 'Try the live demo',
			secondary: 'Get started',
			facts: [
				['6', 'frame types'],
				['1', 'handshake'],
				['0', 'bytes stored on the relay']
			]
		},
		diagram: {
			label: 'How a message travels',
			peerA: 'Peer A',
			peerB: 'Peer B',
			relay: 'Relay',
			plaintext: 'hello',
			sees: 'sees only',
			caption:
				'The relay forwards ciphertext between the two seats of a room. It never holds a key.'
		},
		features: {
			eyebrow: 'Why PeerConnect',
			title: 'A small protocol that does one thing well',
			intro:
				'A private, reliable channel between exactly two peers. No accounts, no message store, no trust in the server in the middle.',
			items: [
				{
					title: 'Untrusted relay',
					text: 'Every message is encrypted and authenticated end to end, keyed by a Noise <code>IKpsk1</code> handshake. The relay sees sizes and timing — never contents, keys or identities.'
				},
				{
					title: 'Stateless by design',
					text: 'No database, no accounts. A room exists on a relay only while someone is connected; the relay can restart, or be replaced by another, and peers simply reconnect.'
				},
				{
					title: 'Reliable, in order, exactly once',
					text: 'Messages are numbered, acknowledged and resent after any disconnection. Send while the other side is away; it arrives when it comes back. The sender is the queue.'
				},
				{
					title: 'Invites that expire',
					text: 'After first contact, both peers move to a room ID derived from their two identities. A leaked QR code or link leads nowhere.'
				},
				{
					title: 'Runs where you are',
					text: 'One Rust core with no I/O powers the Linux relay, the Cloudflare Workers relay, the Rust client and — compiled to WebAssembly — the JavaScript SDK for browsers, Node.js, Deno and Bun.'
				},
				{
					title: 'Specified and tested',
					text: 'A short written specification; test vectors reproduced by an independent implementation; deterministic simulations and chaos tests that cut, replay and tamper with traffic.'
				}
			]
		},
		how: {
			eyebrow: 'How it works',
			title: 'From a QR code to a lasting channel',
			intro: 'Four steps. The relay takes part in none of the secrets.',
			steps: [
				{
					title: 'Create a room',
					text: 'The inviter draws a random 32-byte room secret. From it come the room’s ID on the relay — an Ed25519 public key — and a pre-shared key for the handshake.',
					detail: 'Room.create(relay)'
				},
				{
					title: 'Hand over the invite',
					text: 'The invite is the relay URL, the secret and the inviter’s public key. Show it as a QR code, send a link, paste it — any private channel will do.',
					detail: 'wss://relay…#secret‖key'
				},
				{
					title: 'Pair',
					text: 'The joiner proves to the relay that it holds the room key without revealing it, then runs the Noise handshake. The inviter pins its identity, and both move to a pair ID only they can compute.',
					detail: 'invite ID → pair ID'
				},
				{
					title: 'Talk, for good',
					text: 'Messages flow both ways, encrypted with ChaCha20-Poly1305. Networks drop, relays restart, laptops sleep: peers reconnect, handshake again and resume exactly where they stopped.',
					detail: 'reconnect · resume · exactly once'
				}
			]
		},
		code: {
			eyebrow: 'Developer experience',
			title: 'A private channel in a few lines',
			intro:
				'The same protocol and the same core in the browser and on the server. Connections reconnect and resume on their own.',
			note: '<code>npm install peerconnect</code> · <code>cargo add peerconnect</code> — see <a href="{docs}#install">Install</a>.'
		},
		relay: {
			eyebrow: 'Self-hosting',
			title: 'Run your own relay',
			intro:
				'Nothing to configure, nothing to back up. The relay forwards frames between two seats and forgets them.',
			cards: [
				{
					title: 'Cloudflare Workers',
					text: 'One Durable Object per room with WebSocket Hibernation: idle rooms cost nothing, and it runs on Cloudflare’s network.'
				},
				{
					title: 'Docker',
					text: 'A static binary in an empty image. Put it behind any TLS proxy, or use the bundled Caddy setup for automatic HTTPS.'
				},
				{
					title: 'Single binary',
					text: 'One command: <code>cargo install peerconnect-relay</code>. Five environment variables, connection limits per client address included.'
				}
			],
			public:
				'Just experimenting? Use the public relay <code>{relay}</code> — the one the demo uses. It is for testing, with no uptime guarantee.',
			guide: 'Deployment guide'
		},
		security: {
			eyebrow: 'Security',
			title: 'What the relay can and cannot see',
			seesTitle: 'The relay learns',
			sees: [
				'Room IDs, and when they are in use',
				'Network addresses and connection times',
				'Frame sizes and timing'
			],
			notTitle: 'The relay never learns',
			not: [
				'Message contents',
				'Identity keys or the room secret',
				'Which rooms belong to the same person, when each room uses its own identities'
			],
			status:
				'<strong>Status:</strong> the v1 specification is a draft for review, and the code has not had an external audit. Build on it, test it, try to break it — but do not protect high-value assets with it yet.',
			limitations: 'Known limitations'
		},
		uses: {
			eyebrow: 'Use cases',
			title: 'Made for two',
			items: [
				{
					title: 'Wallet ↔ web app',
					text: 'Approve on your phone what a site on your laptop asks for, through a relay that cannot see what you signed.'
				},
				{
					title: 'Phone ↔ desktop',
					text: 'Pair a mobile app with a desktop app once; sync, control or share whenever both are online.'
				},
				{
					title: 'Device to device',
					text: 'Move files, clipboard or settings between your own devices, without an account or a copy in the cloud.'
				},
				{
					title: 'You ↔ your service',
					text: 'Reach a service or agent behind NAT from your phone, without opening ports or trusting a broker.'
				}
			]
		},
		groups: {
			eyebrow: 'Groups',
			title: 'More than two? Make it a group.',
			intro:
				'Squads, shared worlds, teams: up to 32 members in one end-to-end encrypted group, on the same relays. Built for games, where a command must never wait behind a chat message.',
			items: [
				{
					title: 'Members are keys',
					text: 'Each member is an Ed25519 identity that the app keeps, one per player say. The others see it, proven, so the app decides who owns what and who is only visiting.'
				},
				{
					title: 'Four classes of traffic',
					text: 'Commands, voice, chat and bulk, most urgent first, each with its own queue at the sender and in the relay. A chat flood or a snapshot never holds up a command.'
				},
				{
					title: 'Newcomers catch up exactly',
					text: 'A member sends a snapshot of the app’s state, marked with the commands it contains. The newcomer gets the snapshot, then every later command, none twice.'
				},
				{
					title: 'Voice, in browsers and engines',
					text: 'Opus over WebCodecs with an adaptive jitter buffer, sent only while you speak. Rust and a C library for Unity, Unreal and Godot use the same frame format.'
				}
			],
			guide: 'Read the groups guide',
			spec: 'Groups specification'
		},
		faq: {
			eyebrow: 'FAQ',
			title: 'Questions',
			items: [
				{
					q: 'Can the relay read or change my messages?',
					a: 'No. Messages are encrypted and authenticated with keys only the two peers hold. A relay can drop, delay or cut connections — peers notice and recover — but it cannot read, forge, replay or reorder messages without being detected.'
				},
				{
					q: 'How is this different from WebRTC?',
					a: 'WebRTC connects peers directly, for real-time media, and needs a signaling server plus STUN/TURN to get there. PeerConnect always goes through a relay over plain WebSockets, so it works wherever the web does, and it adds what WebRTC leaves to you: pairing, persistent identities and reliable delivery across reconnections.'
				},
				{
					q: 'How is it different from WalletConnect?',
					a: 'WalletConnect is an ecosystem built around crypto wallets and its own network. PeerConnect is a small, general-purpose transport: any two apps, any payload, and a stateless relay you can run yourself in minutes.'
				},
				{
					q: 'What if the other side is offline?',
					a: 'Send anyway. The sender keeps each message until the other side acknowledges it, and delivers it as soon as both are connected at the same time. The relay stores nothing, so an app that quits before then should keep its own copy of what it has not seen delivered.'
				},
				{
					q: 'What happens when the relay restarts?',
					a: 'Nothing you need to handle. Rooms are not stored on the relay: peers reconnect, authenticate again and resume the stream without losing or duplicating a message.'
				},
				{
					q: 'Can more than two apps share a room?',
					a: 'A room is for exactly two. For more, use a group: up to 32 members (relays admit 10 by default), with commands, chat, voice and state transfer, through the same relays. The groups guide explains how.'
				},
				{
					q: 'Is it ready for production?',
					a: 'Not yet. The protocol is a v1 draft, extensively tested but not externally audited, and the wire format may still change. It is ready for prototypes and feedback.'
				}
			]
		},
		cta: {
			title: 'See it work in your browser',
			text: 'Open the demo on two devices, scan the QR code, send a message. The encryption runs on your devices; the relay only ever forwards ciphertext.',
			button: 'Open the live demo'
		}
	},
	docs: {
		toc: 'On this page',
		copy: 'Copy',
		copied: 'Copied'
	},
	voice: {
		eyebrow: 'Live demo',
		title: 'Group voice chat',
		lead: 'Talk with up to ten people at once, end-to-end encrypted. Create a room, send the link to a friend or open it on your phone, and speak.',
		steps: [
			'Create a room, and share the link.',
			'Everyone who opens it enters a name and joins with their microphone.',
			'Speak. Use headphones if you are in the same room, or the microphones hear each other.'
		],
		note: 'The real thing: the Rust core in WebAssembly, Opus through WebCodecs, an adaptive jitter buffer per speaker, all through the public relay <code>{relay}</code>, which forwards only ciphertext. The room key is in the link after <code>#</code>, which browsers never send to a server.',
		noscript: 'The demo needs JavaScript and WebAssembly.',
		loading: 'Loading…',
		start: {
			title: 'Start a voice room',
			text: 'You get a link to share. Anyone with it can join, up to ten at a time, so share it privately.',
			create: 'Create a room'
		},
		join: {
			title: 'Join the voice room',
			name: 'Your name',
			namePlaceholder: 'e.g. Alice',
			button: 'Join with microphone',
			starting: 'Starting the microphone…',
			hint: 'Your browser will ask for the microphone. Nothing is recorded or stored.'
		},
		problems: {
			mic: 'The microphone is not available (permission denied, or none found). You can still listen and chat.',
			codecs:
				'This browser cannot encode Opus audio (WebCodecs). Text chat works; for voice, use a recent Chrome, Edge or Firefox.',
			full: 'The room is full (ten people). The page keeps trying.',
			replaced: 'This room was opened with the same identity elsewhere.',
			link: 'That link is not a valid room link.',
			failed: 'The demo could not start in this browser.'
		},
		state: {
			offline: 'Connecting…',
			online: 'Connected · end-to-end encrypted',
			closed: 'Left'
		},
		room: {
			people: 'In the room',
			you: 'you',
			alone: 'Nobody else is here yet. Share the link.',
			leave: 'Leave',
			micOn: 'Mute your microphone: nobody hears you',
			micOff: 'Unmute your microphone',
			hearOn: 'Turn your headphones off: you hear nobody',
			hearOff: 'Hear everyone again',
			muteThem: 'Stop hearing {name} (only for you)',
			unmuteThem: 'Hear {name} again',
			theirMicOff: '{name} muted their microphone',
			theirHearingOff: '{name} turned their headphones off and hears nobody'
		},
		invite: {
			title: 'Invite',
			copy: 'Copy link',
			copied: 'Copied'
		},
		chat: {
			title: 'Chat',
			placeholder: 'Write a message',
			send: 'Send',
			empty: 'No messages yet.'
		},
		again: 'Start over'
	},
	demo: {
		eyebrow: 'Live demo',
		title: 'Pair two browsers',
		lead: 'Open this page on your phone and your laptop — or in two browser windows — connect them with a QR code, and chat end-to-end encrypted.',
		note: 'This is the real protocol: the Rust core compiled to WebAssembly, talking through the public relay <code>{relay}</code>. Each browser tab is its own peer, and reloading the tab keeps your room.',
		noscript: 'The demo needs JavaScript and WebAssembly.',
		voiceLink: 'More than two? Try the group voice chat demo →',
		steps: [
			'Create a room on one device.',
			'Scan the QR code with the other, or open the link there.',
			'Check that both show the same safety code, then chat. Try closing a tab, going offline or reloading: messages still arrive, once.'
		],
		loading: 'Loading…',
		start: {
			title: 'Start',
			create: 'Create a room',
			createHint: 'You will get a QR code for the other device.',
			or: 'or join with an invite',
			placeholder: 'Paste an invite or a demo link',
			join: 'Join',
			invalid: 'That is not a PeerConnect invite.'
		},
		invited: {
			title: 'You have been invited',
			text: 'Joining pairs this browser with the one that created the room. Only the first device to join can pair — if that was not you, the room will not respond.',
			join: 'Join the room',
			cancel: 'Cancel'
		},
		invite: {
			title: 'Invite the other device',
			text: 'Scan the code with the other device, or send it the link. Whoever opens it first becomes your peer, so share it privately.',
			copy: 'Copy link',
			copied: 'Copied'
		},
		state: {
			offline: 'Connecting',
			waiting: 'Waiting for the other device',
			connected: 'Connected · end-to-end encrypted',
			closed: 'Room ended'
		},
		role: {
			inviter: 'You created this room',
			joiner: 'You joined this room'
		},
		chat: {
			placeholder: 'Write a message',
			send: 'Send',
			empty:
				'No messages yet. Messages you send now are kept and delivered as soon as the other device connects.',
			you: 'You',
			peer: 'Peer',
			delivered: 'Delivered',
			queued: 'Sent, not yet delivered',
			lost: 'Not delivered: this tab was reloaded before the other device received it.'
		},
		details: {
			title: 'Room',
			safety: 'Safety code',
			safetyHint:
				'Both devices show the same code unless someone else used the invite first. Compare them.',
			safetyPending: 'Shown once paired',
			roomId: 'Room ID on the relay',
			paired: 'Paired: the invite no longer leads here',
			unpaired: 'Not paired yet'
		},
		actions: {
			leave: 'Disconnect',
			reconnect: 'Reconnect',
			end: 'End room',
			endConfirm: 'End this room for both devices?',
			again: 'Start over'
		},
		notices: {
			replaced: 'This room is open in another tab or device. This one will try again in a minute.',
			closedByPeer: 'The other device ended the room.',
			closed: 'You ended the room.',
			left: 'Disconnected. Your room is kept in this tab.'
		}
	},
	notFound: {
		title: 'Page not found',
		text: 'There is nothing at this address.',
		home: 'Back to the home page'
	},
	footer: {
		tagline: 'An open protocol for private, reliable channels between two apps.',
		license: 'Code dual-licensed MIT or Apache-2.0.',
		project: 'Project',
		resources: 'Resources',
		publicRelay: 'Public relay',
		groupSpec: 'Groups specification',
		status: 'Spec v1 · draft'
	}
};

export type Dict = typeof en;
