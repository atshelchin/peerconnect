<script module lang="ts">
	export const toc = [
		{ id: 'overview', label: '群组能做什么' },
		{ id: 'guarantees', label: '保证' },
		{ id: 'classes', label: '流量类别' },
		{ id: 'identities', label: '身份与角色' },
		{ id: 'state-transfer', label: '状态同步' },
		{ id: 'consistency', label: '一致性' },
		{ id: 'javascript', label: 'JavaScript' },
		{ id: 'rust', label: 'Rust' },
		{ id: 'engines', label: '游戏引擎（C）' },
		{ id: 'relays', label: '中继' },
		{ id: 'latency', label: '延迟' },
		{ id: 'limits', label: '限制参数' },
		{ id: 'limitations', label: '已知限制' }
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

<h1>群组</h1>
<p class="lead">
	房间只连接两个端，群组则最多可以容纳 4
	名成员：一支游戏小队、一个共享世界里的玩家、一个小团队。成员之间互发指令、聊天、语音和状态，全程端到端加密，经过的仍是那套不受信任、不存状态的中继。
</p>

<p class="try"><a href="/zh/demo/voice">体验一下：在浏览器里进行多人语音视频聊天 →</a></p>

<h2 id="overview">群组能做什么</h2>
<ul>
	<li>
		<strong>游戏小队与组队。</strong
		>玩家的操作走指令，边玩边用语音交流，大厅里文字聊天——三者互不拖累。
	</li>
	<li>
		<strong>共享世界。</strong
		>后来加入的玩家先从已在场的成员那里拿到整个世界的快照，再收到此后发生的每一处改动。
	</li>
	<li>
		<strong>团队与协作工具。</strong
		>每个人看到的每位作者的改动都保持该作者的原始顺序；谁能改什么，由应用按成员 ID 决定。
	</li>
</ul>
<p>
	群组有一套独立的协议——<a href="/spec/groups">PeerConnect 群组协议</a>——使用单独的中继端点（<code
		>/g1/&lt;id&gt;</code
	>）。参考实现的中继同时支持房间和群组。
</p>
<dl>
	<dt>群组（Group）</dt>
	<dd>
		所有持有同一个 32 字节<strong>群组密钥</strong
		>的人。它不是中继上的某个对象，只在有成员连接时才存在于中继上。
	</dd>
	<dt>邀请（Invite）</dt>
	<dd>
		中继地址加上群组密钥组成的一个字符串，形如 <code>wss://relay…#…</code
		>。它就是全部凭证：没有配对步骤，也没有别的东西需要保存。拿到它的人就能加入群组、读到群里的所有内容，并以自己的身份发言，所以只把它交给成员，并通过私密渠道传递。组建小队的游戏服务器通常会自己创建群组，再把邀请分发给每位玩家。
	</dd>
	<dt>成员（Member）</dt>
	<dd>群组的参与者，以其身份公钥相互识别，这个公钥就是它的<strong>成员 ID</strong>。</dd>
	<dt>席位（Slot）</dt>
	<dd>
		连接在群组中期间，中继分配给它的编号（0–3）。中继会告诉每个成员当前有谁在场。一个群组有 4
		个席位，所以最多 4 名成员。
	</dd>
</dl>

<h2 id="guarantees">保证</h2>
<ul>
	<li>
		<strong>中继无法读取、伪造或重放。</strong
		>每一帧都用每个连接新生成的发送方密钥加密，该密钥由发送方的身份签名担保；成员之间必须回显一个新鲜的随机挑战才会互相接纳，因此中继无法重放之前的会话。中继能看到群组
		ID、席位、帧的大小、类别和时序，但看不到内容和身份。
	</li>
	<li>
		<strong>指令和聊天是可靠消息流。</strong
		>消息带编号，由发送方保留，重连后自动重传。只要收发双方都在运行，每条消息都会按发送顺序、恰好一次地送达每个在场的成员（包括发送者自己）。离线期间发出的消息会在重连后发出。
	</li>
	<li>
		<strong>语音和数据报是实时的。</strong
		>无论卡在哪里，它们都宁可被丢弃也不会被延迟：发送方套接字积压时由发送方丢弃，接收方跟不上时由中继丢弃。迟到的声音比丢失的声音更糟。
	</li>
	<li>
		<strong>紧急的流量优先。</strong
		>无论在发送方、中继还是加密层，较不紧急的类别都不会挡住更紧急的类别。
	</li>
	<li>
		<strong>新成员精确追上进度。</strong
		>状态同步先送来一份快照，再送来快照之后缺少的那些指令，一条不多，一条不重。
	</li>
	<li>
		<strong>中继上什么都不存。</strong
		>除了当前打开的连接，中继不保留任何状态；世界只存在于各成员的应用里。
	</li>
</ul>

<h2 id="classes">流量类别</h2>
<p>
	帧的类型决定了它属于哪个类别，这也是中继对它唯一了解的信息。每个类别在发送方有自己的队列，在中继里也为每个接收方单独排队，在加密层还有各自的计数器。按紧急程度从高到低：
</p>
<div class="table">
	<table>
		<thead><tr><th>类别</th><th>送达方式</th><th>承载内容</th></tr></thead>
		<tbody>
			<tr
				><td><strong>指令</strong></td><td>可靠、有序、会重传</td><td
					>改变共享状态的操作：放一个方块、开一扇门、一次编辑。密钥交换和控制消息也走这里。</td
				></tr
			>
			<tr
				><td><strong>语音</strong></td><td>实时：宁可丢弃也不延迟</td><td
					>语音帧，以及位置、姿态这类只关心最新值的数据报。</td
				></tr
			>
			<tr
				><td><strong>聊天</strong></td><td>可靠、有序、会重传</td><td
					>文字聊天，以及其他可以让指令和语音先走的内容。</td
				></tr
			>
			<tr
				><td><strong>视频</strong></td><td>实时：宁可丢弃也不延迟</td><td
					>视频帧，切成每片都装得进一个数据报的分片，再完整拼回；丢失的分片重传一次，彻底丢失的帧会触发关键帧请求。排在聊天之后，以免把聊天挤掉。</td
				></tr
			>
			<tr><td><strong>批量</strong></td><td>可靠，端到端流量控制</td><td>状态同步用的快照。</td></tr
			>
		</tbody>
	</table>
</div>
<p>
	因此，刷屏的聊天或一份 50 MB
	的快照都不会耽误任何一条指令，语音也不会排在聊天后面，视频也不会挤掉聊天。浏览器里，<code
		>peerconnect/video</code
	>
	的 <code>startVideo</code> 通过 WebCodecs 发送摄像头画面，并把每位成员画进一个 canvas。指令流和聊天流中的每条消息最多
	16,352 字节。
</p>

<h2 id="identities">身份与角色</h2>
<p>
	每个成员都有一个<strong>身份</strong>：由你的应用生成并保管的 Ed25519
	密钥对，比如每个玩家账号一个。请持久保存它的种子（seed），其他成员在重启之后正是靠它认出这名成员。同一个身份可以加入多个群组。中继看不到身份本身，它能看到的加入令牌在不同群组之间也无法关联。
</p>
<p>
	当 <code>joined</code>
	事件报告某个成员时，说明该成员的密钥已为这个连接签了名，而归属于它的每一帧都是用这个连接的密钥加密的。任何成员都无法冒充别人。成员的<strong
		>资料（profile）</strong
	>——昵称、头像等，最多 1,024 字节——会随之一起发送，但那只是成员的自我声明；成员 ID 才是经过证明的。
</p>
<p>
	<strong>角色由应用负责。</strong>任何持有邀请的人都能加入并发送任何内容。请维护成员 ID
	列表——所有者、主机、管理员——并用它检查每条指令的发送者。要把某人踢出去，就把群组迁移到新的密钥。
</p>
<p>
	一个身份最多占用一个席位。同一身份再次连接时（比如网络切换后，或在另一台设备上），新连接会接管该席位，旧连接则收到
	<code>replaced</code>。
</p>

<h2 id="state-transfer">状态同步</h2>
<p>
	后来加入的成员，或者错过太多消息的成员，可以向已在场的成员请求一份<strong>快照</strong
	>：内容是你的应用自己的字节，比如存档的世界、一份文档或一局游戏的状态。
</p>
<ol>
	<li>
		新成员调用 <code>requestSync(member)</code>。从这一刻起，它的 SDK
		会先扣住收到的实时指令，暂不交给应用。
	</li>
	<li>
		被请求成员的应用收到 <code>syncrequest</code>，并且<strong>就在这个事件处理函数里</strong
		>拍下快照，不要等到处理下一个事件。SDK 会在这一刻记下<strong>边界（frontier）</strong
		>：应用已经应用了每条消息流中的哪些指令，精确到每一条。快照本身可以稍后再用
		<code>provideSync</code> 发出，比如先在 worker 线程里压缩一下。
	</li>
	<li>
		快照以批量类别传输，排在其他所有流量之后；发送方按它给链路带来的延迟控制节奏（默认最多 50
		ms），所以加载期间语音依然流畅。
	</li>
	<li>
		新成员的应用收到 <code>snapshot</code
		>，用它替换自己的状态。随后到达的指令恰好是快照里缺少的那些，包括提供方拍快照时自己都还没收到的指令；快照里已有的则会被丢弃。
	</li>
</ol>
<p>
	结果是：快照加上其后送达的指令，恰好等于提供方已应用的全部内容再加上此后的一切。如果提供方在传输途中离开，或者
	15 秒内毫无进展，新成员会收到 <code>syncfailed</code
	>，以及此前扣住的指令，然后可以去问另一个成员。聊天消息不会被扣住，因为它不属于状态。
</p>

<h2 id="consistency">一致性</h2>
<p>
	每个成员的消息流在所有地方都保持该成员的发送顺序，但两个成员的消息流在不同接收方那里可能交错得不一样。如果
	Alice 和 Bob
	同时移动同一个方块，不同成员对它最终停在哪里可能会有分歧。中继本可以强制一个统一的顺序，但它不可信：它完全可以给不同成员看不同的顺序。所以，如果你的指令之间不可交换，就需要在上面加一条规则。下面两种模式能覆盖大多数应用。
</p>
<h3>每个对象一个所有者</h3>
<p>
	给每个对象指定一个所有者（一个成员
	ID），只有来自该对象所有者的指令才会被应用。这样并发的指令总是作用于不同的对象，彼此可交换：无论消息流怎样交错，所有成员最终的状态都一样。它不需要主机，有人离线也不受影响，适合玩家大多只改自己东西的建造类游戏，以及按章节划分所有权的协作工具。转让对象本身也是一条由当前所有者发出的指令。
</p>
<h3>主机权威</h3>
<p>
	选一个成员当<strong>主机</strong>。其他成员用
	<code>sendTo(host, "command", …)</code>
	向它发送请求；主机逐一检查、应用，再把结果作为自己的指令广播出去。包括主机在内，所有人只应用主机的消息流：一条流，一个顺序，没有冲突需要调和。从主机做状态同步，就能原样复制它的状态。
</p>
<p>
	这正是大多数游戏用服务器做的事，只不过这里主机就是服务器。当模拟不具确定性时（物理引擎、真实时钟），这是正确的选择，因为那时只能有一个成员来运行模拟。主机可以固定——比如世界的创建者，它不在时世界只读——也可以按确定的规则选出：在场且有资格当主机的成员中，成员
	ID 最小的那个；每个成员根据 <code>joined</code> 和
	<code>left</code>
	事件都会算出同一个答案。主机更换时，每个成员都向新主机请求一次快照，这样所有人都会与新主机完全对齐。
</p>
<h3>移动的东西</h3>
<p>
	位置、姿态和动画状态每秒变化多次，而且只有最新值有意义。请把它们作为<strong>数据报</strong
	>发送：每个移动物体每秒 10–20
	次，每次携带完整的当前值以及序号或时间戳。接收方保留最新的一份并做插值；丢一个数据报毫无损失，因为下一个会替代它。不要把它们当作指令发送：卡顿之后，可靠消息流会把每一个过时的位置按顺序补发一遍，还会挡在真正重要的指令前面。
</p>
<p>
	锁步（lockstep）要求所有成员逐帧按约定的顺序应用每个人的指令，它需要逐位确定的模拟，并且会被最慢的成员拖住。除了格斗和即时战略游戏，请优先选择上面两种模式。
</p>

<h2 id="javascript">JavaScript</h2>
<p>
	群组和房间在同一个包里，支持浏览器、Node.js 22+、Deno 和 Bun。浏览器语音是单独的入口
	<code>peerconnect/voice</code>。群组连接会自动重连，直到你调用 <code>leave()</code>。
</p>
<CodeBlock code={installJs('zh')} lang="sh" title="shell" />
<h3>加入与发送</h3>
<CodeBlock code={jsGroup('zh')} lang="js" title="squad.js" />
<h3>位置与原始音频</h3>
<CodeBlock code={jsRealtime('zh')} lang="js" title="realtime.js" />
<h3>让新成员追上进度</h3>
<CodeBlock code={jsSync('zh')} lang="js" title="sync.js" />
<h3>浏览器语音</h3>
<p>
	<code>startVoice</code> 会采集麦克风（使用浏览器自带的回声消除、降噪和自动增益），通过 WebCodecs
	编码为
	Opus，只在你说话时发送；每个其他成员的声音都经过各自的自适应抖动缓冲播放，卡顿之后它会丢帧追赶，而不是延后播放。每一帧的格式是
	<code>hold[1] ‖ Opus 包</code>（48 kHz 单声道，默认 20 ms
	一帧），与原生客户端相同，所以浏览器和游戏客户端能互相听见。
</p>
<CodeBlock code={jsVoice('zh')} lang="js" title="voice.js" />
<div class="table">
	<table>
		<thead><tr><th>选项</th><th>默认值</th><th>含义</th></tr></thead>
		<tbody>
			<tr
				><td><code>bitrate</code></td><td><code>24000</code></td><td>Opus 码率（比特每秒）。</td
				></tr
			>
			<tr
				><td><code>frameMs</code></td><td><code>20</code></td><td
					>10、20、40 或 60 毫秒。帧越长，包越少，延迟也越大。</td
				></tr
			>
			<tr><td><code>vad</code></td><td><code>true</code></td><td>只在说话时发送。</td></tr>
			<tr
				><td><code>threshold</code></td><td><code>-50</code></td><td
					>麦克风电平高于此值（dBFS）即视为说话。</td
				></tr
			>
			<tr
				><td><code>minDelay</code>、<code>maxDelay</code></td><td
					><code>30</code>、<code>250</code></td
				><td>抖动缓冲的上下限，单位毫秒。</td></tr
			>
		</tbody>
	</table>
</div>
<p>
	它需要支持 Opus 的 WebCodecs 音频，不支持时会直接 reject（已在 Chromium
	中测试）。戴耳机可以彻底避免回声。
</p>
<h3>事件</h3>
<div class="table">
	<table>
		<thead><tr><th>事件</th><th>触发时机</th></tr></thead>
		<tbody>
			<tr
				><td><code>state</code></td><td
					><code>conn.state</code> 变化：<code>offline</code>、<code>online</code> 或
					<code>closed</code>。</td
				></tr
			>
			<tr
				><td><code>joined</code></td><td
					>有成员被接纳，其身份已经过证明：<code>member</code>、<code>profile</code>。</td
				></tr
			>
			<tr><td><code>left</code></td><td>有成员离开。</td></tr>
			<tr
				><td><code>message</code></td><td
					>一条指令或聊天消息，按其所在消息流的顺序到达，也包括我们自己发的；只发给我们的消息带有
					<code>direct</code>。</td
				></tr
			>
			<tr
				><td><code>voice</code>、<code>datagram</code></td><td
					>来自 <code>from</code> 的实时数据；语音带有 <code>seq</code>。</td
				></tr
			>
			<tr
				><td><code>missed</code></td><td
					>某成员的消息流丢失了 <code>count</code> 条消息：发送方已经不再保留它们。</td
				></tr
			>
			<tr
				><td><code>syncrequest</code></td><td
					>有成员请求快照。请在处理函数里拍下快照，然后调用 <code>provideSync</code> 或
					<code>declineSync</code>。</td
				></tr
			>
			<tr><td><code>snapshot</code></td><td>请求的快照到了：用它替换应用状态。</td></tr>
			<tr
				><td><code>syncfailed</code></td><td
					>同步失败；之前扣住的指令随后送达。请换一个成员再试。</td
				></tr
			>
			<tr
				><td><code>replaced</code></td><td
					>我们的身份在别处加入并占用了我们的席位。30–60 秒后会重试；调用 <code>leave()</code>
					可停止。</td
				></tr
			>
			<tr><td><code>full</code></td><td>群组在这个中继上已满，稍后会重试。</td></tr>
		</tbody>
	</table>
</div>
<h3>方法</h3>
<div class="table">
	<table>
		<thead><tr><th>调用</th><th>作用</th></tr></thead>
		<tbody>
			<tr
				><td><code>Group.create(relay)</code></td><td
					>创建群组。<code>group.toString()</code> 就是它的邀请。</td
				></tr
			>
			<tr><td><code>Group.parse(invite)</code></td><td>解析邀请。</td></tr>
			<tr
				><td><code>Identity.generate()</code>、<code>Identity.fromSeed(seed)</code></td><td
					>成员身份；请持久保存 <code>seed</code>。<code>id</code> 即成员 ID。</td
				></tr
			>
			<tr
				><td><code>joinGroup(group, options)</code></td><td
					>以 <code>identity</code> 和 <code>profile</code> 加入，直到离开前一直留在群组中。</td
				></tr
			>
			<tr
				><td><code>conn.send(channel, data)</code></td><td
					>在 <code>"command"</code> 或 <code>"chat"</code> 上广播，返回消息 id。</td
				></tr
			>
			<tr
				><td><code>conn.sendTo(member, channel, data)</code></td><td
					>只发给一个成员，与该频道的消息保持顺序；连接断开时不会重传。</td
				></tr
			>
			<tr
				><td><code>conn.sendVoice(seq, data)</code>、<code>conn.sendDatagram(data, to?)</code></td
				><td>实时数据；被丢弃时返回 <code>false</code>。</td></tr
			>
			<tr><td><code>conn.requestSync(member)</code></td><td>向某个成员请求快照。</td></tr>
			<tr
				><td
					><code>conn.provideSync(to, transfer, snapshot)</code>、<code
						>conn.declineSync(to, transfer)</code
					></td
				><td>回应 <code>syncrequest</code>。</td></tr
			>
			<tr
				><td><code>conn.members</code>、<code>conn.id</code>、<code>conn.rtt</code></td><td
					>已接纳的成员（成员 ID → 资料）、我们的成员 ID、最近一次到中继的往返时间。</td
				></tr
			>
			<tr><td><code>conn.leave()</code></td><td>离开群组。</td></tr>
		</tbody>
	</table>
</div>

<h2 id="rust">Rust</h2>
<p>
	<code>client</code> 特性提供了 <code>client::group</code>，一个基于 Tokio 的驱动。连接可以解引用为
	<code>GroupSender</code>，<code>conn.sender()</code> 会给你一份克隆，用于在其他任务中发送。
</p>
<CodeBlock code={rustDependency} lang="toml" title="Cargo.toml" />
<CodeBlock code={rustGroup('zh')} lang="rust" title="main.rs" />
<h3>使用你自己的事件循环</h3>
<p>
	不启用 <code>client</code> 特性时，这个 crate 不做任何 I/O。<code>group::Member</code>
	就是以状态机形式呈现的完整成员：你把一个 WebSocket 上发生的事情告诉它，再执行它要求的操作。自带主循环的引擎每帧轮询一次即可——比如在
	GDExtension 里配合 Godot 的 <code>WebSocketPeer</code>——同一个状态机在浏览器里则通过 JavaScript
	SDK 运行。时间单位是毫秒，任何单调时钟都可以。
</p>
<CodeBlock code={rustMember('zh')} lang="rust" title="engine.rs" />
<p>
	可以用终端群聊示例体验：
	<code>cargo run -p peerconnect --features client --example group -- new &lt;relay&gt; alice</code
	>，然后在另一个终端运行 <code>… -- join '&lt;invite&gt;' bob</code>。
</p>

<h2 id="engines">游戏引擎（C）</h2>
<p>
	<code>peerconnect-ffi</code> 把同一个状态机封装成 C 库，供 Unity、Unreal、Godot（通过 GDExtension 或
	GDNative）以及任何支持 C FFI 的语言使用。它不含 I/O，也不开线程：WebSocket 归你的引擎管，你每帧把发生的事情告诉成员对象，再执行它要求的操作。
</p>
<CodeBlock code={cBuild('zh')} lang="sh" title="shell" />
<CodeBlock code={cMember('zh')} lang="c" title="game.c" />
<p>
	套接字关闭时，用关闭码（或 -1）调用 <code>pc_member_handle_close</code
	>；成员对象会自行重连，再次给出
	<code>PC_CONNECT</code>。<code>pc_output</code>
	中的指针在对该成员的下一次调用之前有效，需要保留的内容请自行复制。只要使用 SDK 的格式
	<code>hold[1] ‖ Opus 包</code>，语音帧就能与浏览器互通。
</p>

<h2 id="relays">中继</h2>
<p>
	Linux 中继、它的 Docker 镜像以及 Cloudflare Worker 都同时支持群组和房间，无需额外配置（见<a
		href="/zh/docs#relay">运行中继</a
	>）。它们每个群组允许 4 名成员，这是协议的上限；在 Linux 中继上，可以用
	<code>PEERCONNECT_MAX_GROUP_MEMBERS</code> 把它调低。
</p>
<p>
	Linux
	中继为每个接收方的每个类别分别排队。对于已经落后的接收方，它会丢弃发往它的语音；如果某个接收方跟不上指令或聊天，它会暂缓读取发送方的数据，但最多
	5 秒，之后关闭卡住的接收方；它还把每个连接的发送速率限制在 2 MiB/s，突发最多 4 MiB。Cloudflare
	Worker
	看不到自己缓冲了多少数据，只能把每一帧立即转发：它既不能丢弃迟到的语音，也不能让指令优先，而且每个语音帧都是一条计费的
	Durable Object 消息。大规模使用语音时，请用 Linux 中继。
</p>
<p>
	Linux 中继还在同号的 UDP 端口上通过 QUIC
	提供群组：每个类别各走一条流，语音走数据报，所以丢一个包只会挡住它所在的类别，语音也从不等待重传。邀请里的中继
	URL 决定用什么：
</p>
<ul>
	<li><code>wss://relay.example.com</code>：WebSocket，任何中继都支持。</li>
	<li>
		<code>https://relay.example.com</code>：浏览器用 WebTransport，Rust 用原生 QUIC，走 UDP
		443，不通时回退到 <code>wss://</code>。中继需要浏览器信任的证书（<code
			>PEERCONNECT_TLS_CERT</code
		>
		和 <code>PEERCONNECT_TLS_KEY</code>）。
	</li>
	<li>
		<code>quic://203.0.113.9:8080</code>：不需要域名也不需要证书。原生客户端用原生
		QUIC，不校验中继的证书；浏览器用
		WebTransport，固定这类中继提供的“每周证书”的哈希，这张证书任何人都能算出来（<a
			href="/spec/groups#sec-3-7">§3.7</a
		>）；两者在 UDP 不通时都回退到 <code>ws://</code>。
	</li>
</ul>
<p>
	QUIC 有 1.5 秒的先手；UDP 不通的地方，成员自动改用
	WebSocket，同一个群组里两种可以混用。在每一跳丢包 3% 的情况下，语音 p99 在 WebSocket 上是 138
	ms，在 QUIC 上是 53 ms（<a href="/spec/groups#sec-3-6">规范</a>）。
</p>

<h2 id="latency">延迟与负载</h2>
<p>
	在一台 Apple M2 Pro 上用游戏负载测试测得：每个玩家都是一个真实的、端到端加密的 WebSocket
	客户端，每秒发送 20 次位置、2 条指令，每 10 秒一条聊天；每组 3
	人同时说话；测试中途每组再加入一名新成员，加载 2 MB 快照。延迟从发送方调用 <code>send</code> 算到接收方收到事件。
</p>
<div class="table">
	<table>
		<thead
			><tr
				><th>负载（Linux 中继）</th><th>指令 p50 / p99</th><th>语音 p50 / p99</th><th
					>加载快照期间语音 p99</th
				><th>中继</th></tr
			></thead
		>
		<tbody>
			<tr
				><td>10 组，100 名玩家</td><td>0.95 / 2.6 ms</td><td>0.80 / 2.4 ms</td><td>6.0 ms</td><td
					>0.4 核，6 MB</td
				></tr
			>
			<tr
				><td>50 组，500 名玩家</td><td>0.92 / 3.5 ms</td><td>0.77 / 3.3 ms</td><td>2.7 ms</td><td
					>1.7 核，19 MB</td
				></tr
			>
			<tr
				><td>100 组，1,000 名玩家</td><td>1.6 / 17 ms</td><td>1.4 / 15 ms</td><td>19 ms</td><td
					>4.1 核，35 MB</td
				></tr
			>
		</tbody>
	</table>
</div>
<p>所有可靠消息都恰好送达一次。100 组时整台机器已满负荷：负载生成器自己就占用了五到六个核。</p>
<p>
	模拟真实网络：每个玩家各自处在一条带深缓冲的接入链路之后，3 名玩家，外加一名正在加载 2 MB
	快照的新成员。
</p>
<div class="table">
	<table>
		<thead
			><tr
				><th>每个玩家的链路</th><th>语音 p50</th><th>语音 p99</th><th>加载快照期间语音 p99</th><th
					>快照耗时</th
				></tr
			></thead
		>
		<tbody>
			<tr><td>20 Mbit/s，单程 20 ms</td><td>43 ms</td><td>47 ms</td><td>59 ms</td><td>2.1 s</td></tr
			>
			<tr
				><td>2 Mbit/s，单程 40 ms</td><td>84 ms</td><td>89 ms</td><td>144 ms</td><td>10.4 s</td></tr
			>
			<tr
				><td>1 Mbit/s，单程 60 ms</td><td>125 ms</td><td>131 ms</td><td>197 ms</td><td>21 s</td></tr
			>
		</tbody>
	</table>
</div>
<p>
	链路本身的延迟是下限，中继和 SDK 只增加约 1
	毫秒。状态传输会按自己给链路带来的延迟控制节奏：若改用固定窗口，2 Mbit/s 链路上加载快照期间的语音
	p99 是 920 ms。
</p>
<div class="table">
	<table>
		<thead><tr><th>浏览器语音（Chromium，20 ms Opus 帧）</th><th>结果</th></tr></thead>
		<tbody>
			<tr><td>从采集结束到开始播放</td><td>p50 18–29 ms</td></tr>
			<tr
				><td>链路每个方向加入 0–60 ms 抖动</td><td
					>p50 110–140 ms；抖动缓冲自适应到约 120 ms；无丢失</td
				></tr
			>
			<tr><td>1 秒卡顿之后</td><td>2 秒内恢复到 p50 50–70 ms（丢帧追赶，而不是延后播放）</td></tr>
		</tbody>
	</table>
</div>
<p>
	从嘴到耳的总延迟还要加上正在采集的那 20 ms 一帧、音频硬件自身的延迟（约 10–30 ms），以及网络。
</p>

<h2 id="limits">限制参数</h2>
<div class="table">
	<table>
		<thead><tr><th>项目</th><th>上限</th></tr></thead>
		<tbody>
			<tr><td>每个群组的成员数</td><td>4（4 个席位）；超出的连接由中继以关闭码 4429 拒绝</td></tr>
			<tr><td>单条指令或聊天消息</td><td>16,352 字节</td></tr>
			<tr><td>成员资料</td><td>1,024 字节</td></tr>
			<tr><td>为重传保留的消息</td><td>每个频道最近 8,192 条或 8 MiB</td></tr>
			<tr><td>快照</td><td>在途 16 KiB – 1 MiB，按带来的延迟控制节奏；默认最大接受 64 MiB</td></tr>
			<tr><td>保活</td><td>每 10 秒一次 ping；25 秒收不到任何数据的连接会被重建</td></tr>
			<tr
				><td>消息流中的缺口</td><td>3 秒后再次请求；15 秒后放弃，并以 <code>missed</code> 报告</td
				></tr
			>
			<tr><td>停滞的状态同步</td><td>15 秒后失败</td></tr>
		</tbody>
	</table>
</div>

<h2 id="limitations">已知限制</h2>
<ul>
	<li>
		<strong>走 WebSocket 时，语音跑在 TCP 上。</strong>在 Cloudflare 中继上，以及 UDP
		不通的地方，一个丢失的包会挡住后面的数据，直到它被重传。抖动缓冲和丢弃迟到帧能把影响控制在一次卡顿之内，但在丢包的链路上仍然明显。走
		QUIC（Linux 中继）时不会这样（见<a href="#relays">中继</a>）。
	</li>
	<li>
		<strong>成员之间没有统一顺序。</strong
		>中继不会对不同成员的指令进行排序。如果应用的指令不可交换，需要在上面加一条规则（见<a
			href="#consistency">一致性</a
		>）。
	</li>
	<li>
		<strong>任何成员都能读取和发送一切。</strong>角色需要应用按成员 ID
		自行执行。要排除某人，只能把群组迁移到新的密钥。
	</li>
	<li>
		<strong>相对群组密钥没有前向保密。</strong
		>发送方密钥每个连接都是新的，但它们是在由群组密钥派生的密钥保护下传递的：如果有人录下流量，之后又拿到了群组密钥，就能解密这些流量。在意这一点时，请轮换密钥。
	</li>
	<li>
		<strong>消息流只存在于内存中。</strong
		>发送方重启后会开启一条新的消息流，旧消息不复存在；接收方重启后则从头开始——这时请请求一份快照。
	</li>
	<li>
		<strong>Cloudflare 中继</strong
		>会立即转发每一帧：不会丢弃迟到的语音，不会让指令优先，而且每个语音帧都是一条计费的 Durable
		Object 消息。
	</li>
	<li>
		<strong>浏览器语音</strong>需要支持 Opus 的 WebCodecs 音频，目前在 Chromium
		中测试。回声消除由浏览器负责。
	</li>
	<li>
		<strong>尚未经过独立审计。</strong
		>设计经历了多轮对抗性评审，测试向量也由一个仅依据规范编写的独立实现复现，但这些都不能代替外部的密码学审计。
	</li>
</ul>
<p class="next">
	完整的协议见<a href="/spec/groups">群组协议规范</a>（英文）。两个端之间的房间，请看<a
		href="/zh/docs">快速上手</a
	>。
</p>
