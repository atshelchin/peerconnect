<script module lang="ts">
	export const toc = [
		{ id: 'concepts', label: '基本概念' },
		{ id: 'install', label: '安装' },
		{ id: 'tokens', label: 'Token' },
		{ id: 'javascript', label: 'JavaScript' },
		{ id: 'rust', label: 'Rust' },
		{ id: 'relay', label: '运行中继' },
		{ id: 'limits', label: '限制参数' },
		{ id: 'security', label: '安全清单' },
		{ id: 'limitations', label: '已知限制' }
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
		tokensJs,
		rustDependency,
		rustFull
	} from '#lib/snippets.ts';
</script>

<h1>快速上手</h1>
<p class="lead">
	PeerConnect
	为两个应用提供一条私密、可靠的通道，中间经过的中继无需被任何一方信任。本指南介绍需要了解的基本概念、JavaScript
	与 Rust 库的用法，以及如何运行你自己的中继。
</p>

<h2 id="concepts">基本概念</h2>
<dl>
	<dt>房间（Room）</dt>
	<dd>
		两个端之间的关系。它由一个随机的房间密钥和双方的身份密钥定义，而不是存储在中继上的任何东西。
	</dd>
	<dt>邀请方与加入方</dt>
	<dd>
		<strong>邀请方</strong>创建房间并发出<strong>邀请</strong>，<strong>加入方</strong
		>接受邀请。一个房间恰好只有这两个端。
	</dd>
	<dt>邀请（Invite）</dt>
	<dd>
		由中继地址、房间密钥和邀请方公钥组成的一个字符串，形如 <code>{RELAY_URL}#…</code
		>。可以显示为二维码或链接。在配对完成之前，谁先使用它，谁就成为加入方，所以请私下传递。
	</dd>
	<dt>配对</dt>
	<dd>
		首次连接时，邀请方会记住（pin）加入方的身份，双方一起迁移到由两者身份推导出的<strong
			>配对 ID</strong
		>。此后邀请就不再指向任何地方。
	</dd>
	<dt>房间记录</dt>
	<dd>
		一个端重新连接所需的全部信息，即字符串 <code>room.toString()</code
		>。它包含密钥，请像保存密码一样保存它。在创建或加入房间时、收到
		<code>paired</code> 事件后以及调用 <code>close()</code> 之后都要保存。
	</dd>
	<dt>连接（Connection）</dt>
	<dd>
		与房间的实时连接，会自动重连。它的状态是以下之一：<code>offline</code
		>（正在连接或等待重试）、<code>waiting</code>（已在中继上，对方不在）、<code>connected</code
		>（端到端会话已建立）和
		<code>closed</code>。
	</dd>
	<dt>送达保证</dt>
	<dd>
		只要两端都在运行，无论连接断开多少次，每条消息都恰好送达一次，并且保持顺序。对方不在时发送的消息由发送方保留，双方都连上后送达。
	</dd>
</dl>

<h2 id="install">安装</h2>
<p>JavaScript SDK 发布在 npm，Rust crate 发布在 crates.io；协议仍是草案，版本号为 0.1。</p>
<h3>JavaScript</h3>
<p>
	JavaScript SDK 是编译为 WebAssembly 的 Rust 核心，WebAssembly
	已内联进包中，因此无需配置打包工具即可在浏览器、Node.js 22+、Deno 和 Bun 中加载（gzip 后 46 KB）。
</p>
<CodeBlock code={installJs('zh')} lang="sh" title="shell" />
<h3>Rust</h3>
<p>
	默认情况下这个 crate 不做任何 I/O：其中的 <code>Peer</code> 是一个状态机，可以由任意 WebSocket
	库和事件循环驱动。启用
	<code>client</code> 特性后会附带一个现成的 Tokio 客户端。
</p>
<CodeBlock code={rustDependency} lang="toml" title="Cargo.toml" />
<p>如需从源码构建，请克隆<a href={REPO_URL} rel="external">代码仓库</a>，步骤见其 README。</p>

<h2 id="tokens">Token</h2>
<p>
	公共 relay <code>{RELAY_URL}</code> 的每个连接都需要 token：这样它知道是哪个应用在用、能阻止滥用；除此之外，它从
	token 里学不到任何东西。每一端各带自己的 token，token 不属于房间，也不在邀请里。获取方式有三种：
</p>
<ol>
	<li>
		<strong>公开试用</strong>：<code>trialToken</code> 无需账户即可拿到一个两小时有效的 token，用来试用。
	</li>
	<li>
		<strong>用户连接你的应用</strong>：在<a href="/zh/developers">开发者页面</a>（用 GitHub
		登录）登记应用，拿到 App ID 和 App Secret。用户从你的应用打开
		<code>peerconnect.dev/connect</code>，登录后带着 code 回来，你的应用用 code 换取他们的
		token（OAuth + PKCE）。
	</li>
	<li>
		<strong>由你的服务器签发</strong>：适合用户无需知道 PeerConnect 的应用，服务器用 App Secret
		为每个用户申请 token。
	</li>
</ol>
<CodeBlock code={tokensJs('zh')} lang="js" title="tokens.js" />
<p>
	<code>token</code> 传一个函数时，relay 一旦拒绝 token（关闭码
	4403），连接会重新调用它，然后继续，消息不丢。自己部署的 relay 不需要
	token，除非你给它配置了密钥（<a href="#binary"><code>PEERCONNECT_TOKEN_SECRET</code></a>）。
</p>

<h2 id="javascript">JavaScript</h2>
<p>一端创建房间并展示邀请，另一端用它加入。双方都要保存房间记录，以便之后在任何地方重新连接。</p>
<CodeBlock code={jsFull('zh')} lang="js" title="app.js" />
<h3>连接事件</h3>
<div class="table">
	<table>
		<thead><tr><th>事件</th><th>触发时机</th></tr></thead>
		<tbody>
			<tr
				><td><code>state</code></td><td
					><code>conn.state</code>（以及 <code>conn.reason</code>）发生变化。</td
				></tr
			>
			<tr
				><td><code>message</code></td><td
					>收到一条消息：已认证、已解密、按顺序。<code>e.data</code> 是
					<code>Uint8Array</code>。</td
				></tr
			>
			<tr
				><td><code>delivered</code></td><td
					>对方已收到所有 id 小于 <code>e.detail</code> 的消息。</td
				></tr
			>
			<tr
				><td><code>paired</code></td><td
					>配对完成，房间已迁移到配对 ID。请再次保存 <code>conn.room.toString()</code>。</td
				></tr
			>
			<tr
				><td><code>replaced</code></td><td
					>另一个连接占用了本端的席位——通常是同一个房间在别处被打开。本连接会在一分钟后重试；调用 <code
						>leave()</code
					> 可以把席位让给另一个连接。</td
				></tr
			>
		</tbody>
	</table>
</div>
<h3>方法</h3>
<div class="table">
	<table>
		<thead><tr><th>调用</th><th>作用</th></tr></thead>
		<tbody>
			<tr
				><td><code>Room.create(relay)</code></td><td
					>创建房间，你是邀请方。<code>room.invite</code> 就是邀请。</td
				></tr
			>
			<tr
				><td><code>Room.join(invite)</code></td><td>接受邀请，你是加入方。请在连接之前保存记录。</td
				></tr
			>
			<tr><td><code>Room.parse(record)</code></td><td>恢复已保存的房间。</td></tr>
			<tr
				><td><code>connect(room, &#123; token &#125;)</code></td><td
					>连接房间，并保持连接，直到离开或关闭。</td
				></tr
			>
			<tr
				><td><code>conn.send(data)</code></td><td
					>把一条消息（字符串或字节，最大 16 MiB）加入队列并返回其 id。当已有 64 MiB
					等待确认，或房间正在关闭时会抛出异常。</td
				></tr
			>
			<tr
				><td><code>conn.leave()</code></td><td
					>断开连接，保留房间以便之后再连。尚未确认的消息会被丢弃。</td
				></tr
			>
			<tr
				><td><code>conn.close()</code></td><td
					>为双方结束房间。连接会持续到对方收到通知为止；请保存 <code>conn.room</code
					>，关闭后再删除。</td
				></tr
			>
		</tbody>
	</table>
</div>

<h2 id="rust">Rust</h2>
<p>
	Tokio 客户端与 JavaScript API 一一对应：<code>send</code> 加入队列并返回 id，<code>next()</code>
	产生事件，连接会自行重连并续传。
</p>
<CodeBlock code={rustFull('zh')} lang="rust" title="main.rs" />
<p>
	可以用聊天示例在终端里体验：
	<code>cargo run -p peerconnect --features client --example chat -- new {RELAY_URL} room-a</code
	>，然后在另一个终端运行
	<code>… -- join '&lt;invite&gt;' room-b</code>。
</p>

<h2 id="relay">运行中继</h2>
<p>
	中继在房间的两个席位之间转发帧，不存储任何东西。它会检查每个连接是否持有房间密钥，但自己并不知道这个密钥，因此不需要账号，也不需要任何配置。公共中继
	<code>{RELAY_URL}</code> 可供任何人测试使用；其他场景请运行你自己的中继。
</p>
<h3 id="cloudflare">Cloudflare Workers</h3>
<p>
	每个房间 ID 对应一个独立的 Durable Object，并使用 WebSocket Hibernation
	API：空闲房间零成本，出问题的房间也只会耗尽它自己。在 Cloudflare 控制台添加自定义域名后，即可通过 <code
		>wss://relay.你的域名</code
	> 访问。
</p>
<CodeBlock code={cloudflare('zh')} lang="sh" title="shell" />
<h3 id="docker">Docker</h3>
<p>
	镜像里只有一个静态二进制文件。可以放在任意终止 TLS 的反向代理之后，或使用自带的 Compose
	文件，它会加上 Caddy 自动启用 HTTPS。
</p>
<CodeBlock code={docker('zh')} lang="sh" title="shell" />
<h3 id="binary">二进制文件</h3>
<CodeBlock code={binary('zh')} lang="sh" title="shell" />
<div class="table">
	<table>
		<thead><tr><th>环境变量</th><th>默认值</th><th>含义</th></tr></thead>
		<tbody>
			<tr
				><td><code>PEERCONNECT_LISTEN</code></td><td><code>0.0.0.0:8080</code></td><td
					>监听地址。</td
				></tr
			>
			<tr
				><td><code>PEERCONNECT_MAX_CONNECTIONS</code></td><td><code>10000</code></td><td
					>最大并发 WebSocket 数。</td
				></tr
			>
			<tr
				><td><code>PEERCONNECT_MAX_PER_ADDRESS</code></td><td><code>256</code></td><td
					>每个客户端地址的最大并发 WebSocket 数（IPv6 按 /64 计）。</td
				></tr
			>
			<tr
				><td><code>PEERCONNECT_TRUST_PROXY</code></td><td>关闭</td><td
					>如果前面的反向代理会把客户端地址追加到 <code>X-Forwarded-For</code>，设为
					<code>1</code>。</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>所有中继都会响应 <code>GET /healthz</code>，用于健康检查。</p>

<h2 id="limits">限制参数</h2>
<div class="table">
	<table>
		<thead><tr><th>项目</th><th>上限</th></tr></thead>
		<tbody>
			<tr><td>每个房间的端数</td><td>2</td></tr>
			<tr><td>单条消息大小</td><td>16 MiB，以最大 64 KiB 的帧发送</td></tr>
			<tr><td>在途未确认数据</td><td>4 MiB（单条更大的消息可以超出）</td></tr>
			<tr
				><td>为对方保留的消息</td><td
					>64 MiB；超出后 <code>send</code> 会抛出异常（JavaScript）或等待（Rust）</td
				></tr
			>
			<tr><td>保活</td><td>每 25 秒一次 ping；60 秒收不到任何数据的连接会被重建</td></tr>
		</tbody>
	</table>
</div>

<h2 id="security">安全清单</h2>
<ul>
	<li>
		像保存密码一样保存房间记录——放进 Keychain、Android Keystore
		或同等的加密存储。拿到记录的人就能冒充这一端。
	</li>
	<li>加入方要在连接之前保存记录：其中包含邀请方将要记住的身份。</li>
	<li>通过私密渠道发送邀请。配对完成之前，邀请就是一张“持有即可用”的凭证。</li>
	<li>
		如果配对到错误的设备代价很高，可以让用户对比由双方身份密钥（<code>room.publicKey</code> 和
		<code>room.peerKey</code>）推导出的短码——在线演示里就有一个。
	</li>
	<li>房间关闭后删除它的记录。</li>
</ul>

<h2 id="limitations">已知限制</h2>
<ul>
	<li>
		<strong>队列保存在发送方内存中。</strong
		>消息能扛过断线和中继重启，但扛不过发送进程退出：如果需要跨重启送达，请在看到
		<code>delivered</code> 之前自行保留副本。接收方重启后，可能会再次收到它最后约 50 毫秒内收到的消息。
	</li>
	<li>
		<strong>结束房间需要双方再同时在线一次。</strong><code>close()</code> 会一直尝试，直到对方确认；如果对方再也不回来，它就永远不会知道。
	</li>
	<li>
		<strong>配对之前，邀请是持有即可用的凭证。</strong
		>谁先使用谁配对。这种抢占是可以察觉的——真正的加入方会一直连不上——但无法阻止。
	</li>
	<li>
		<strong>每个房间只用一个中继。</strong
		>中继宕机时，两端只能等待。双方可以约定换一个中继地址，但没有自动故障转移。
	</li>
	<li>
		<strong>Cloudflare 中继</strong>只能惰性地执行空闲超时，也无法施加背压；诚实的端受 4 MiB
		窗口约束。
	</li>
	<li>
		<strong>尚未经过独立审计。</strong
		>设计经历了多轮对抗性评审，测试向量也由独立实现复现，但这些都不能代替外部的密码学审计。
	</li>
</ul>
<p class="next">完整的协议及其威胁模型见<a href="/spec">协议规范</a>（英文）。</p>
