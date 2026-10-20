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
