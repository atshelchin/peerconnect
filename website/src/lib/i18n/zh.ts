import type { Dict } from './en';

export const zh: Dict = {
	meta: {
		siteName: 'PeerConnect',
		home: {
			title: 'PeerConnect — 两个应用之间的端到端加密通道',
			description:
				'扫码配对两个应用，经由无状态的 WebSocket 中继进行端到端加密通信，中继既读不到也伪造不了任何消息。提供 Rust 与 JavaScript 实现，可自行部署。'
		},
		docs: {
			title: '快速上手 — PeerConnect',
			description:
				'用 JavaScript 或 Rust 接入 PeerConnect，并在 Cloudflare Workers、Docker 或单个二进制文件上运行你自己的中继。'
		},
		demo: {
			title: '在线演示：配对两个浏览器 — PeerConnect',
			description:
				'用二维码配对两个浏览器，通过公共 PeerConnect 中继进行端到端加密聊天。演示运行的是编译为 WebAssembly 的真实协议实现。'
		},
		spec: {
			title: '协议规范 v1 — PeerConnect',
			description:
				'PeerConnect v1 协议规范：房间与邀请、无状态中继准入、Noise IKpsk1 握手、可靠消息流与安全考量。'
		},
		notFound: {
			title: '页面不存在 — PeerConnect',
			description: '你访问的页面不存在。'
		}
	},
	nav: {
		home: '首页',
		docs: '文档',
		demo: '演示',
		spec: '规范',
		github: 'GitHub',
		skip: '跳到正文',
		language: '语言',
		menu: '菜单'
	},
	banner: {
		text: '本页面提供中文版。',
		action: '切换到中文',
		dismiss: '关闭'
	},
	home: {
		hero: {
			eyebrow: '开放协议 · v1 草案',
			title: '两个应用，一个房间。<br />中继什么也读不到。',
			lead: 'PeerConnect 把两个应用连在一起——手机和电脑、网页和钱包、分处不同网络的两个服务——中间经过的 WebSocket 中继既读不到、也伪造或重放不了它们之间的任何消息，而且什么都不存储。',
			primary: '试用在线演示',
			secondary: '快速上手',
			facts: [
				['6', '种帧类型'],
				['1', '次握手'],
				['0', '字节存储在中继上']
			]
		},
		diagram: {
			label: '一条消息的旅程',
			peerA: '端 A',
			peerB: '端 B',
			relay: '中继',
			plaintext: '你好',
			sees: '只能看到',
			caption: '中继只在房间的两个席位之间转发密文，从不持有任何密钥。'
		},
		features: {
			eyebrow: '为什么选择 PeerConnect',
			title: '一个小而专注的协议',
			intro:
				'在恰好两个端之间建立私密、可靠的通道。没有账号，没有消息存储，也无需信任中间的服务器。',
			items: [
				{
					title: '不必信任中继',
					text: '每条消息都经过端到端加密与认证，密钥来自 Noise <code>IKpsk1</code> 握手。中继只能看到大小和时间，看不到内容、密钥或身份。'
				},
				{
					title: '天生无状态',
					text: '没有数据库，没有账号。房间只在有人连接时才存在于中继上；中继可以随时重启，甚至换成另一台，两端重新连上即可。'
				},
				{
					title: '可靠、有序、恰好一次',
					text: '消息带编号、有确认，断线后自动重传。对方不在线时照样发送，它一回来就能收到。发送方本身就是队列。'
				},
				{
					title: '邀请会自动失效',
					text: '首次连接后，两端会迁移到一个由双方身份推导出的房间 ID。二维码或链接即使泄露，也找不到任何人。'
				},
				{
					title: '随处可运行',
					text: '同一个不含 I/O 的 Rust 核心驱动 Linux 中继、Cloudflare Workers 中继、Rust 客户端，并编译为 WebAssembly 供浏览器、Node.js、Deno 和 Bun 使用。'
				},
				{
					title: '有规范，有测试',
					text: '简短的书面规范；由独立实现复现的测试向量；会切断、重放和篡改流量的确定性模拟与混沌测试。'
				}
			]
		},
		how: {
			eyebrow: '工作原理',
			title: '从一张二维码到一条长期通道',
			intro: '四个步骤，中继不接触任何秘密。',
			steps: [
				{
					title: '创建房间',
					text: '邀请方随机生成 32 字节的房间密钥，并由它推导出房间在中继上的 ID（一个 Ed25519 公钥）以及握手用的预共享密钥。',
					detail: 'Room.create(relay)'
				},
				{
					title: '交出邀请',
					text: '邀请由中继地址、房间密钥和邀请方的公钥组成。可以显示成二维码、发一个链接或直接粘贴——任何私密渠道都行。',
					detail: 'wss://relay…#secret‖key'
				},
				{
					title: '配对',
					text: '加入方向中继证明自己持有房间密钥（但不泄露它），随后进行 Noise 握手。邀请方记住它的身份，双方一起迁移到只有彼此才能算出的配对 ID。',
					detail: '邀请 ID → 配对 ID'
				},
				{
					title: '长久通信',
					text: '消息双向流动，以 ChaCha20-Poly1305 加密。网络会断，中继会重启，电脑会休眠：两端会自动重连、重新握手，并从中断处继续。',
					detail: '重连 · 续传 · 恰好一次'
				}
			]
		},
		code: {
			eyebrow: '开发体验',
			title: '几行代码，一条私密通道',
			intro: '浏览器与服务器使用同一个协议、同一个核心。连接会自动重连并续传。',
			note: '<code>npm install peerconnect</code> · <code>cargo add peerconnect</code>，详见<a href="{docs}#install">安装</a>。'
		},
		relay: {
			eyebrow: '自行部署',
			title: '运行你自己的中继',
			intro: '无需配置，无需备份。中继只在两个席位之间转发帧，转发完就忘。',
			cards: [
				{
					title: 'Cloudflare Workers',
					text: '每个房间一个 Durable Object，并使用 WebSocket Hibernation：空闲房间零成本，运行在 Cloudflare 的全球网络上。'
				},
				{
					title: 'Docker',
					text: '空镜像里只有一个静态二进制文件。放在任意 TLS 反向代理之后，或使用自带的 Caddy 配置自动启用 HTTPS。'
				},
				{
					title: '单个二进制文件',
					text: '一条命令：<code>cargo install peerconnect-relay</code>。四个环境变量即可配置，内置按客户端地址的连接数限制。'
				}
			],
			public:
				'只是想试试？可以使用公共中继 <code>{relay}</code>，在线演示用的就是它。仅供测试，不保证可用性。',
			guide: '部署指南'
		},
		security: {
			eyebrow: '安全',
			title: '中继能看到什么，看不到什么',
			seesTitle: '中继能知道',
			sees: ['房间 ID 及其使用时间', '网络地址与连接时间', '帧的大小与时序'],
			notTitle: '中继永远不知道',
			not: ['消息内容', '身份密钥或房间密钥', '哪些房间属于同一个人（只要每个房间使用独立的身份）'],
			status:
				'<strong>当前状态：</strong>v1 规范仍是征求意见的草案，代码尚未经过外部审计。欢迎基于它开发、测试、尝试攻破它——但暂时不要用它保护高价值资产。',
			limitations: '已知限制'
		},
		uses: {
			eyebrow: '应用场景',
			title: '为“两个”而生',
			items: [
				{
					title: '钱包 ↔ 网页应用',
					text: '电脑上的网站发起请求，在手机上确认；中继看不到你签了什么。'
				},
				{
					title: '手机 ↔ 电脑',
					text: '手机应用与桌面应用配对一次，之后只要双方在线，就能同步、遥控或分享。'
				},
				{
					title: '设备到设备',
					text: '在自己的设备之间传文件、剪贴板或设置，不需要账号，云端也不留副本。'
				},
				{
					title: '你 ↔ 你的服务',
					text: '用手机访问 NAT 之后的服务或智能体，无需开放端口，也无需信任任何中间商。'
				}
			]
		},
		faq: {
			eyebrow: 'FAQ',
			title: '常见问题',
			items: [
				{
					q: '中继能读取或篡改我的消息吗？',
					a: '不能。消息使用只有两端持有的密钥加密并认证。中继可以丢弃、延迟消息或切断连接——两端会察觉并恢复——但它无法在不被发现的情况下读取、伪造、重放或重排消息。'
				},
				{
					q: '它和 WebRTC 有什么不同？',
					a: 'WebRTC 让两端直连，面向实时音视频，为此需要信令服务器以及 STUN/TURN。PeerConnect 始终经由中继、使用普通的 WebSocket，因此凡是能上网的地方都能用；它还提供了 WebRTC 留给你自己解决的部分：配对、持久身份，以及跨重连的可靠送达。'
				},
				{
					q: '它和 WalletConnect 有什么不同？',
					a: 'WalletConnect 是围绕加密钱包和其自有网络构建的生态。PeerConnect 是一个小巧的通用传输层：任意两个应用、任意数据，以及一个几分钟就能自己部署的无状态中继。'
				},
				{
					q: '如果对方不在线怎么办？',
					a: '照样发送。发送方会保留每条消息，直到对方确认收到；只要双方同时在线，消息就会送达。中继不存储任何东西，所以如果应用在送达前就退出，应自行保存尚未确认送达的消息。'
				},
				{
					q: '中继重启了会怎样？',
					a: '你什么都不用处理。房间并不存储在中继上：两端会重新连接、重新认证并继续消息流，不丢失也不重复任何一条消息。'
				},
				{
					q: '可以用于生产环境吗？',
					a: '还不行。协议目前是 v1 草案，经过了大量测试但尚未经过外部审计，线上格式也可能变化。它已经适合用来做原型和提供反馈。'
				}
			]
		},
		cta: {
			title: '在浏览器里亲眼看看',
			text: '在两台设备上打开演示，扫码，发一条消息。加密在你的设备上完成，中继始终只转发密文。',
			button: '打开在线演示'
		}
	},
	docs: {
		toc: '本页目录',
		copy: '复制',
		copied: '已复制'
	},
	demo: {
		eyebrow: '在线演示',
		title: '配对两个浏览器',
		lead: '在手机和电脑上（或两个浏览器窗口中）打开本页，用二维码把它们连起来，然后进行端到端加密聊天。',
		note: '这里运行的是真实协议：Rust 核心编译成 WebAssembly，通过公共中继 <code>{relay}</code> 通信。每个浏览器标签页都是一个独立的端，刷新标签页不会丢失房间。',
		noscript: '演示需要 JavaScript 和 WebAssembly。',
		steps: [
			'在一台设备上创建房间。',
			'用另一台设备扫描二维码，或在那里打开链接。',
			'确认两边显示的安全码相同，然后开始聊天。试试关掉标签页、断网或刷新：消息依然会送达，而且只送达一次。'
		],
		loading: '加载中…',
		start: {
			title: '开始',
			create: '创建房间',
			createHint: '你会得到一个给另一台设备扫描的二维码。',
			or: '或使用邀请加入',
			placeholder: '粘贴邀请或演示链接',
			join: '加入',
			invalid: '这不是有效的 PeerConnect 邀请。'
		},
		invited: {
			title: '你收到了一个邀请',
			text: '加入后，这个浏览器将与创建房间的浏览器配对。只有第一个加入的设备能配对成功——如果别人抢先了，房间将不会有回应。',
			join: '加入房间',
			cancel: '取消'
		},
		invite: {
			title: '邀请另一台设备',
			text: '用另一台设备扫描二维码，或把链接发给它。第一个打开的人就会成为你的对端，所以请私下分享。',
			copy: '复制链接',
			copied: '已复制'
		},
		state: {
			offline: '连接中',
			waiting: '等待另一台设备',
			connected: '已连接 · 端到端加密',
			closed: '房间已结束'
		},
		role: {
			inviter: '你创建了这个房间',
			joiner: '你加入了这个房间'
		},
		chat: {
			placeholder: '输入消息',
			send: '发送',
			empty: '还没有消息。现在发送的消息会被保留，另一台设备一连上就会送达。',
			you: '你',
			peer: '对端',
			delivered: '已送达',
			queued: '已发送，尚未送达',
			lost: '未送达：对方收到之前，此标签页被刷新了。'
		},
		details: {
			title: '房间',
			safety: '安全码',
			safetyHint: '如果没有人抢先使用邀请，两台设备会显示相同的安全码。请对比一下。',
			safetyPending: '配对后显示',
			roomId: '中继上的房间 ID',
			paired: '已配对：邀请不再指向这里',
			unpaired: '尚未配对'
		},
		actions: {
			leave: '断开',
			reconnect: '重新连接',
			end: '结束房间',
			endConfirm: '要为两台设备结束这个房间吗？',
			again: '重新开始'
		},
		notices: {
			replaced: '这个房间已在另一个标签页或设备上打开。此处将在一分钟后重试。',
			closedByPeer: '对方结束了房间。',
			closed: '你结束了房间。',
			left: '已断开。房间仍保存在此标签页中。'
		}
	},
	notFound: {
		title: '页面不存在',
		text: '这个地址上什么也没有。',
		home: '返回首页'
	},
	footer: {
		tagline: '一个在两个应用之间建立私密、可靠通道的开放协议。',
		license: '代码采用 MIT 或 Apache-2.0 双许可。',
		project: '项目',
		resources: '资源',
		publicRelay: '公共中继',
		status: '规范 v1 · 草案'
	}
};
