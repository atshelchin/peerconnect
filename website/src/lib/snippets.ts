// Code shown on the site, with comments in the reader's language.

import type { Lang } from '#lib/i18n/index.ts';
import { RELAY_URL, REPO_URL } from '#lib/site.ts';

const REPO = REPO_URL ?? '<repository>';

const pick = (lang: Lang, en: string, zh: string) => (lang === 'zh' ? zh : en);

export function jsQuick(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `import { connect, Room } from "peerconnect";

// ${c('Device A: create a room and show its invite as a QR code.', '设备 A：创建房间，把邀请显示成二维码。')}
const room = Room.create("${RELAY_URL}");
showQrCode(room.invite);
const a = connect(room);
a.addEventListener("message", (e) => {
  console.log(new TextDecoder().decode(e.data));
});

// ${c('Device B: join with the scanned invite and say hello.', '设备 B：用扫到的邀请加入，打个招呼。')}
const b = connect(Room.join(scannedInvite));
b.send("hello"); // ${c('queued until A is reachable, delivered exactly once', '在 A 可达之前排队，恰好送达一次')}

// ${c('Persist conn.room.toString() to reconnect after a restart.', '保存 conn.room.toString()，重启后即可重新连接。')}`;
}

export function rustQuick(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `use peerconnect::{client, Event, Room};

let room = Room::join(&invite.parse()?);
let mut conn = client::connect(room);
conn.send(b"hello").await?; // ${c('queued until the inviter is reachable', '在邀请方可达之前排队')}

while let Some(event) = conn.next().await {
    if let Event::Message(data) = event {
        println!("{}", String::from_utf8_lossy(&data));
    }
}`;
}

export function jsFull(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `import { connect, Room } from "peerconnect";

// ${c('Peer A creates a room and shows the invite (e.g. as a QR code).', '端 A 创建房间并展示邀请（例如二维码）。')}
const room = Room.create("${RELAY_URL}");
showQrCode(room.invite);

const conn = connect(room);
conn.addEventListener("paired", () => save(conn.room.toString())); // ${c('a secret: store it safely', '这是秘密，请妥善保存')}
conn.addEventListener("state", () => console.log(conn.state));     // offline | waiting | connected | closed
conn.addEventListener("message", (e) => console.log(new TextDecoder().decode(e.data)));

// ${c('Peer B scans it and saves its room at once: it holds the identity A will pin.', '端 B 扫码后立即保存房间：其中包含 A 将要记住的身份。')}
const joined = Room.join(scannedInvite);
save(joined.toString());
const connB = connect(joined);
connB.addEventListener("paired", () => save(connB.room.toString()));
connB.send("hello"); // ${c('queued; delivered as soon as A is reachable', '先排队，A 一可达就送达')}
connB.addEventListener("delivered", (e) => console.log("A has everything below", e.detail));

// ${c('After a restart: connect(savedString) and carry on.', '重启之后：connect(savedString) 即可继续。')}
// ${c('To end the room: conn.close(), save conn.room, delete it once closed.', '结束房间：conn.close()，保存 conn.room，关闭后删除。')}`;
}

export function rustFull(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `use peerconnect::{client, Event, Room, State};

let room = Room::join(&invite.parse()?);
save(&room.to_string());               // ${c('before connecting: it holds the identity the inviter pins', '连接之前保存：其中包含邀请方将要记住的身份')}
let mut conn = client::connect(room);
conn.send(b"hello").await?;            // ${c('queued until the inviter is reachable', '在邀请方可达之前排队')}
while let Some(event) = conn.next().await {
    match event {
        Event::Message(data) => println!("{}", String::from_utf8_lossy(&data)),
        Event::Paired => save(&conn.room().to_string()),
        Event::State(State::Closed(_)) => break,
        _ => {}
    }
}`;
}

export function installJs(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `npm install peerconnect      # ${c('or: pnpm add, bun add, deno add npm:peerconnect', '或：pnpm add、bun add、deno add npm:peerconnect')}`;
}

export const rustDependency = `[dependencies]
peerconnect = { version = "0.1", features = ["client"] }`;

export function cloudflare(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `git clone ${REPO} && cd peerconnect
cargo install worker-build
cd crates/peerconnect-worker
npx wrangler deploy            # ${c('prints your wss:// URL', '会输出你的 wss:// 地址')}`;
}

export function docker(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `git clone ${REPO} && cd peerconnect
docker build -t peerconnect-relay .
docker run -p 8080:8080 peerconnect-relay      # ws://localhost:8080

# ${c('With automatic HTTPS (Caddy), on a host that DOMAIN points to:', '自动 HTTPS（Caddy），DOMAIN 需指向本机：')}
DOMAIN=relay.example.com docker compose -f deploy/docker-compose.yml up -d`;
}

export function binary(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `cargo install peerconnect-relay
peerconnect-relay              # ${c('listens on 0.0.0.0:8080', '监听 0.0.0.0:8080')}`;
}

// Groups. The relay in these is a placeholder: groups need a relay that serves
// `/g1/`, such as one of your own.
const GROUP_RELAY = 'wss://relay.example.com';

export function jsGroup(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `import { Group, Identity, joinGroup } from "peerconnect";

// ${c('Whoever forms the group (a player, or your game server) creates it once.', '组建群组的一方（某位玩家，或你的游戏服务器）只需创建一次。')}
const group = Group.create("${GROUP_RELAY}");
const invite = group.toString(); // ${c('the whole credential: hand it to members only', '这就是全部凭证：只交给成员')}

// ${c('Every member: a lasting identity, then join.', '每个成员：一个长期身份，然后加入。')}
const me = savedSeed ? Identity.fromSeed(savedSeed) : Identity.generate();
save(me.seed);                   // ${c('so that the others recognise me next time', '下次其他成员才能认出我')}
const conn = joinGroup(invite, { identity: me, profile: "alice" });

conn.addEventListener("joined", (e) => console.log("joined", e.detail.member));
conn.addEventListener("left", (e) => console.log("left", e.detail.member));
conn.addEventListener("message", (e) => {
  const { from, channel, data, direct } = e.detail; // ${c('our own messages too', '也包括我们自己发的')}
  if (channel === "command") applyCommand(from, data);
  else showChat(from, new TextDecoder().decode(data));
});

conn.send("command", JSON.stringify({ op: "place", block: 7 })); // ${c('to everyone, in order, resent', '发给所有人，有序，会重传')}
conn.send("chat", "on my way");
conn.sendTo(hostId, "command", JSON.stringify({ op: "open", door: 3 })); // ${c('to one member, not resent', '只发给一个成员，不重传')}`;
}

export function jsRealtime(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `// ${c('Positions: 10–20 times a second, the full current value each time.', '位置：每秒 10–20 次，每次都发送完整的当前值。')}
setInterval(() => conn.sendDatagram(encodePose(tick++, player)), 66);
conn.addEventListener("datagram", (e) => {
  avatars.update(e.detail.from, decodePose(e.detail.data)); // ${c('keep the newest, interpolate', '保留最新的，插值显示')}
});

// ${c('Your own audio codec: frames numbered by seq, dropped rather than delayed.', '自带音频编解码器：帧按 seq 编号，宁可丢弃也不延迟。')}
conn.sendVoice(seq++, opusPacket); // ${c('false if it was dropped', '被丢弃时返回 false')}
conn.addEventListener("voice", (e) => jitterBuffer.push(e.detail.from, e.detail.seq, e.detail.data));`;
}

export function jsSync(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `// ${c('Every member: answer with a snapshot taken right here, in the handler.', '每个成员：就在事件处理函数里拍下快照并回复。')}
conn.addEventListener("syncrequest", (e) => {
  const snapshot = saveWorld(); // ${c('exactly the commands applied so far', '恰好包含目前已应用的全部指令')}
  compress(snapshot).then((data) => conn.provideSync(e.detail.from, e.detail.transfer, data));
  // ${c('or: conn.declineSync(e.detail.from, e.detail.transfer)', '或者：conn.declineSync(e.detail.from, e.detail.transfer)')}
});

// ${c('The newcomer: ask a member once it has joined.', '新成员：等某个成员 joined 之后向它请求。')}
conn.addEventListener("joined", (e) => {
  if (!synced && e.detail.member === hostId) conn.requestSync(hostId);
});
conn.addEventListener("snapshot", async (e) => {
  loadWorld(await decompress(e.detail.data)); // ${c('replace the state with it', '用它替换当前状态')}
  synced = true; // ${c('the commands that follow are exactly those it lacks', '随后到达的指令恰好是快照里缺少的')}
});
conn.addEventListener("syncfailed", () => askAnotherMember());`;
}

export function jsVoice(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `import { startVoice } from "peerconnect/voice";

joinButton.onclick = async () => {
  // ${c('From a user gesture. Rejects where WebCodecs has no Opus.', '必须由用户操作触发。浏览器的 WebCodecs 不支持 Opus 时会 reject。')}
  const voice = await startVoice(conn, { frameMs: 20 });
  voice.addEventListener("speaking", (e) => {
    highlight(e.detail.member, e.detail.speaking); // ${c('our own member ID too', '也包括我们自己')}
  });
  talkButton.onpointerdown = () => voice.setMuted(false); // ${c('push to talk', '按住说话')}
  talkButton.onpointerup = () => voice.setMuted(true);
  voice.setVolume(loudMember, 0.5);
  console.log(voice.stats()); // ${c('per speaker: delay, late, concealed, dropped', '每位说话者：延迟、迟到、补偿、丢弃')}
};`;
}

export function rustGroup(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `use peerconnect::client::group;
use peerconnect::group::{Channel, Config, Event, Identity};

let identity = Identity::from_seed(saved_seed); // ${c('or Identity::generate(), then save .seed()', '或 Identity::generate()，然后保存 .seed()')}
let config = Config { profile: b"alice".to_vec(), ..Config::default() };
let mut conn = group::join(invite.parse()?, identity, config)?;
conn.send(Channel::Command, b"place 7".to_vec()).await?;

while let Some(event) = conn.next().await {
    match event {
        Event::Joined { member, profile } => roster.add(member, profile),
        Event::Message { from, channel, data, .. } => apply(from, channel, &data),
        Event::Voice { from, seq, data } => jitter_buffer.push(from, seq, data),
        Event::SyncRequest { from, transfer } => conn.provide_sync(from, transfer, world.save()),
        Event::Snapshot { data, .. } => world = World::load(&data),
        _ => {}
    }
}`;
}

export function rustMember(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `use peerconnect::group::{Config, Member, Output};

let mut member = Member::new(invite.parse()?, identity, Config::default(), now_ms())?;

// ${c('Once per frame, from the engine’s own loop: no Tokio, no threads.', '每帧调用一次，跑在引擎自己的循环里：无需 Tokio，也无需线程。')}
fn process(&mut self) {
    let now = now_ms();
    self.member.handle_timeout(now);
    for packet in self.socket.received() {
        self.member.handle_binary(now, &packet);
    }
    while let Some(output) = self.member.poll_output() {
        match output {
            Output::Connect(url) => self.socket.open(&url), // ${c('then member.handle_open(now)', '随后调用 member.handle_open(now)')}
            Output::Send(frame, class) => self.socket.queue(frame, class), // ${c('most urgent class first', '最紧急的类别先发')}
            Output::Ping => self.socket.send_text("ping"),
            Output::Disconnect => self.socket.close(),
            Output::Event(event) => self.on_event(event),
        }
    }
}
// ${c('Socket closed: member.handle_close(now, Some(code)); it asks to connect again.', '连接关闭时：member.handle_close(now, Some(code))，它会再次要求连接。')}`;
}

export function cBuild(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `git clone ${REPO} && cd peerconnect
cargo build --release -p peerconnect-ffi
# ${c('target/release/libpeerconnect_ffi.so | .dylib | peerconnect_ffi.dll, and the static library', 'target/release/libpeerconnect_ffi.so | .dylib | peerconnect_ffi.dll，以及静态库')}
# ${c('header: crates/peerconnect-ffi/include/peerconnect.h', '头文件：crates/peerconnect-ffi/include/peerconnect.h')}`;
}

export function cMember(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `pc_member *m = pc_member_new(invite, seed, (const uint8_t *)"alice", 5, now_ms());

void every_frame(void) {
  pc_member_handle_timeout(m, now_ms());       /* ${c('harmless when nothing is due', '没有到期的事情时调用也无妨')} */
  for (/* ${c('each message the socket received', '套接字收到的每条消息')} */) pc_member_handle_binary(m, now_ms(), data, len);

  pc_output out;
  while (pc_member_poll(m, &out) != PC_NONE) {
    switch (out.kind) {
      case PC_CONNECT: open_websocket((const char *)out.data); break;   /* ${c('then pc_member_handle_open', '随后调用 pc_member_handle_open')} */
      case PC_SEND: send_binary(out.data, out.len, out.class_); break;  /* ${c('most urgent class first', '最紧急的类别先发')} */
      case PC_PING: send_text("ping"); break;
      case PC_DISCONNECT: close_websocket(); break;
      case PC_MESSAGE: on_message(out.member, out.channel, out.data, out.len); break;
      case PC_VOICE: jitter_buffer_push(out.member, out.seq, out.data, out.len); break;
      /* PC_JOINED, PC_LEFT, PC_SYNC_REQUEST, PC_SNAPSHOT, … */
    }
  }
}

pc_member_send(m, 0 /* command */, (const uint8_t *)"place 7", 7, NULL);`;
}
