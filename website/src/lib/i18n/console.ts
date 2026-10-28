// Strings of the signed-in pages (account, developers, connect). Kept apart
// from the site's dictionary: these pages are not prerendered or indexed.
// `zh` must have exactly the same shape as `en`.

const en = {
	title: {
		account: 'Your account',
		developers: 'Developers',
		connect: 'Connect',
		admin: 'Admin'
	},
	common: {
		signOut: 'Sign out',
		signedInAs: 'Signed in as',
		copy: 'Copy',
		copied: 'Copied',
		show: 'Show',
		hide: 'Hide',
		save: 'Save',
		delete: 'Delete',
		cancel: 'Cancel',
		created: 'Created',
		never: 'never',
		admin: 'Admin',
		developers: 'Developers',
		account: 'Account',
		today: 'Today',
		last30: 'Last 30 days',
		connections: 'connections',
		minutes: 'min connected',
		traffic: 'traffic'
	},
	account: {
		intro:
			'The apps you connected to PeerConnect, and tokens you made for your own use. Deleting one stops it at once.',
		clients: 'Your connections',
		of: 'of',
		none: 'Nothing connected yet. Apps built on PeerConnect send you here to connect them.',
		manual: 'Made by hand',
		confirmDelete: 'Delete {label}? Its token stops working at once.',
		deleted: '{label} is deleted.',
		build: 'Building an app?',
		buildLead: 'Register it on the developers page to get an app ID and an app secret.'
	},
	developers: {
		lead: 'Register an application to use the PeerConnect relay. Its users connect it with their GitHub account, or your server issues tokens for them with the app secret.',
		apps: 'Your applications',
		none: 'No applications yet.',
		create: 'Register an application',
		name: 'Name',
		namePlaceholder: 'My chat app',
		redirects: 'Redirect URIs',
		redirectsHint:
			'One per line, where peerconnect.dev sends people back: https://example.com/peerconnect, com.example.app:/peerconnect for mobile apps, http://localhost:5173/… while developing.',
		origins: 'Web origins',
		originsHint:
			'One per line: pages at these origins may exchange codes from the browser (https://example.com).',
		loopback: 'Desktop loopback path',
		loopbackHint:
			'Optional. Desktop apps come back to http://127.0.0.1:<any port><this path>, such as /peerconnect.',
		register: 'Register',
		limit: 'You can register {limit} applications.',
		invalid: 'Not saved:'
	},
	app: {
		back: 'All applications',
		credentials: 'Credentials',
		appId: 'App ID',
		appIdHint: 'Public: it goes in your app, in the link to peerconnect.dev/connect.',
		appSecret: 'App secret',
		appSecretHint:
			'Keep it on your server: it issues tokens for your users, and it exchanges codes for apps that do so on a server.',
		rotate: 'Rotate the secret',
		confirmRotate: 'Rotate the app secret? The current one stops working at once.',
		rotated: 'The app secret was rotated. The old one no longer works.',
		settings: 'Settings',
		saved: 'Saved.',
		users: 'Use',
		people: 'people connected it themselves',
		serverUsers: 'users your server issued tokens for',
		integrate: 'Integrate',
		pathA: 'Your users connect with GitHub',
		pathALead:
			'Send people to peerconnect.dev/connect; they sign in and come back with a code, which your app exchanges for their token. In a web app:',
		pathB: 'Your server issues the tokens',
		pathBLead:
			'Your users never see PeerConnect: your server asks for a token per user with the app secret, and hands it to your app.',
		testToken: 'A test token',
		testTokenLead:
			'A token on your own account, to try the relay from a script before the sign-in is built into your app.',
		makeToken: 'Make a test token',
		tokenFor: 'Token for {app}',
		danger: 'Delete the application',
		dangerLead:
			'Every token of its users stops at once, and its app ID can no longer be used. This cannot be undone.',
		confirmDelete: 'Delete {app}? Every token of its users stops working at once.',
		more: '{n} clients are left: delete again to go on.'
	},
	connect: {
		heading: 'Connect {app}',
		lead: 'Let <strong>{app}</strong> on <strong>{label}</strong> use PeerConnect as <strong>@{login}</strong>?',
		detail:
			'{app} gets a token for the PeerConnect relay. The relay only forwards what {app} encrypts: it cannot read your messages. You can disconnect it on your account page at any time.',
		as: 'Connect as',
		thisOne: 'This {app}: it keeps its connection',
		inUse: '{app} · in use there now; it will be disconnected',
		yourOwn: 'Made by hand',
		fresh: 'A new connection',
		approve: 'Connect',
		limit:
			'You have reached your limit of {limit} connections. Delete one on your account page, then try again.',
		done: 'Connected',
		returning: 'Going back to {app}… If nothing happens:',
		returnTo: 'Return to {app}'
	}
};

export type ConsoleDict = typeof en;

const zh: ConsoleDict = {
	title: {
		account: '我的账户',
		developers: '开发者',
		connect: '连接',
		admin: '管理'
	},
	common: {
		signOut: '退出登录',
		signedInAs: '已登录为',
		copy: '复制',
		copied: '已复制',
		show: '显示',
		hide: '隐藏',
		save: '保存',
		delete: '删除',
		cancel: '取消',
		created: '创建于',
		never: '从未',
		admin: '管理',
		developers: '开发者',
		account: '账户',
		today: '今天',
		last30: '最近 30 天',
		connections: '次连接',
		minutes: '分钟在线',
		traffic: '流量'
	},
	account: {
		intro: '你连接到 PeerConnect 的应用，以及你给自己做的 token。删除后立即失效。',
		clients: '我的连接',
		of: '/',
		none: '还没有连接。基于 PeerConnect 的应用会把你带到这里来连接。',
		manual: '手动创建',
		confirmDelete: '删除「{label}」？它的 token 会立即失效。',
		deleted: '「{label}」已删除。',
		build: '在开发应用？',
		buildLead: '到开发者页面登记，拿到 App ID 和 App Secret。'
	},
	developers: {
		lead: '登记一个应用，即可使用 PeerConnect relay。你的用户可以用 GitHub 账户连接它，也可以由你的服务器用 App Secret 为他们签发 token。',
		apps: '我的应用',
		none: '还没有应用。',
		create: '登记应用',
		name: '名称',
		namePlaceholder: '我的聊天应用',
		redirects: '回调地址',
		redirectsHint:
			'每行一个，peerconnect.dev 把用户送回这里：https://example.com/peerconnect；移动应用用 com.example.app:/peerconnect；开发时可用 http://localhost:5173/…',
		origins: '网页来源',
		originsHint: '每行一个：这些来源的网页可以在浏览器里用 code 换 token（https://example.com）。',
		loopback: '桌面端回环路径',
		loopbackHint: '可选。桌面应用回到 http://127.0.0.1:<任意端口><此路径>，例如 /peerconnect。',
		register: '登记',
		limit: '每个账户最多登记 {limit} 个应用。',
		invalid: '未保存：'
	},
	app: {
		back: '全部应用',
		credentials: '凭证',
		appId: 'App ID',
		appIdHint: '公开的：写在你的应用里，放进跳转 peerconnect.dev/connect 的链接。',
		appSecret: 'App Secret',
		appSecretHint: '只放在你的服务器上：用它为用户签发 token，服务端换 code 时也用它。',
		rotate: '轮换 Secret',
		confirmRotate: '轮换 App Secret？当前的会立即失效。',
		rotated: 'App Secret 已轮换，旧的不再有效。',
		settings: '设置',
		saved: '已保存。',
		users: '用量',
		people: '人自己连接了它',
		serverUsers: '个用户由你的服务器签发 token',
		integrate: '接入',
		pathA: '用户用 GitHub 连接',
		pathALead:
			'把用户带到 peerconnect.dev/connect，他们登录后带着 code 回来，你的应用用 code 换取他们的 token。网页应用里：',
		pathB: '由你的服务器签发 token',
		pathBLead:
			'用户完全看不到 PeerConnect：你的服务器用 App Secret 为每个用户申请 token，再交给你的应用。',
		testToken: '测试 token',
		testTokenLead: '一个记在你自己账户下的 token，在应用接好登录之前，先用脚本试试 relay。',
		makeToken: '生成测试 token',
		tokenFor: '{app} 的 token',
		danger: '删除应用',
		dangerLead: '它所有用户的 token 立即失效，App ID 也不能再用。无法撤销。',
		confirmDelete: '删除「{app}」？它所有用户的 token 会立即失效。',
		more: '还剩 {n} 个客户端，请再点一次删除继续。'
	},
	connect: {
		heading: '连接 {app}',
		lead: '允许 <strong>{label}</strong> 上的 <strong>{app}</strong> 以 <strong>@{login}</strong> 的身份使用 PeerConnect？',
		detail:
			'{app} 会拿到一个 PeerConnect relay 的 token。relay 只转发 {app} 加密后的内容，读不到你的消息。你随时可以在账户页面断开它。',
		as: '连接为',
		thisOne: '这个 {app}：保持现有连接',
		inUse: '{app} · 正在那里使用，将被断开',
		yourOwn: '手动创建',
		fresh: '新建一个连接',
		approve: '连接',
		limit: '你已达到 {limit} 个连接的上限。请先在账户页面删除一个，再试一次。',
		done: '已连接',
		returning: '正在返回 {app}…… 如果没有反应：',
		returnTo: '返回 {app}'
	}
};

export const consoleText = { en, zh };

/** `text` with each `{name}` replaced. */
export function fill(text: string, values: Record<string, string | number>): string {
	return text.replace(/\{(\w+)\}/g, (m, k: string) => (k in values ? String(values[k]) : m));
}

const ESCAPES: Record<string, string> = {
	'&': '&amp;',
	'<': '&lt;',
	'>': '&gt;',
	'"': '&quot;',
	"'": '&#39;'
};

/** [`fill`] for a string rendered as HTML: the values are escaped, only the template's markup stays. */
export function fillHtml(text: string, values: Record<string, string | number>): string {
	const escaped = Object.fromEntries(
		Object.entries(values).map(([k, v]) => [k, String(v).replace(/[&<>"']/g, (c) => ESCAPES[c])])
	);
	return fill(text, escaped);
}
