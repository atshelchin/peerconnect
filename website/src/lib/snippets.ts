// Code shown on the site, with comments in the reader's language.

import type { Lang } from '#lib/i18n/index.ts';
import { RELAY_URL, REPO_URL, SITE_URL } from '#lib/site.ts';

const REPO = REPO_URL ?? '<repository>';

const pick = (lang: Lang, en: string, zh: string) => (lang === 'zh' ? zh : en);

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
peerconnect-relay              # ${c('listens on 0.0.0.0:8080', '监听 0.0.0.0:8080')}

# ${c('Optional: require tokens, and mint one', '可选：要求 token，并签发一个')}
export PEERCONNECT_TOKEN_SECRET=$(openssl rand -hex 32)
peerconnect-relay token my-app --ttl 30d`;
}

export function accessSh(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `# ${c('Try it: a trial token, no account (two hours)', '试用：无需账户的试用 token（两小时）')}
curl -X POST ${SITE_URL}/api/trial
# → {"token": "eyJ…", "expires_at": …, "relay_url": "${RELAY_URL}"}

# ${c('Your app’s server: a token per user of yours, with the app secret', '你的应用服务器：用 App Secret 为每个用户申请 token')}
curl ${SITE_URL}/api/apps/app_…/tokens \\
  -H "authorization: Bearer $PEERCONNECT_APP_SECRET" \\
  -H "content-type: application/json" \\
  -d '{"user": "user-42"}'

# ${c('Then each peer connects with its own token', '然后每一端用自己的 token 连接')}
#   ${RELAY_URL}/v1/<room id>?token=<token>`;
}

export function relaySketch(lang: Lang): string {
	const c = (en: string, zh: string) => pick(lang, en, zh);
	return `${c('A peer, in outline (SPEC §4–§6):', '一个端的大致流程（SPEC §4–§6）：')}

1. ${c('open', '打开')}  ${RELAY_URL}/v1/<room id>?token=<token>
2. ${c('relay → HELLO nonce[32]', 'relay → HELLO nonce[32]')}
3. ${c('peer  → JOIN role ‖ Ed25519 signature over the nonce', '端    → JOIN role ‖ 对 nonce 的 Ed25519 签名')}
4. ${c('relay → PRESENCE (is the other peer here?)', 'relay → PRESENCE（对方在不在）')}
5. ${c('both  ⇄ Noise_IKpsk1_25519_ChaChaPoly_SHA256 handshake', '双方  ⇄ Noise_IKpsk1_25519_ChaChaPoly_SHA256 握手')}
6. ${c('both  ⇄ numbered, acknowledged messages; resent after reconnecting', '双方  ⇄ 编号并确认的消息，重连后重发')}
7. ${c('text "ping" every 25 s', '每 25 秒发一次文本 "ping"')}

${c('Check every step against test-vectors/v1.json.', '每一步都用 test-vectors/v1.json 核对。')}`;
}

export function agentPrompt(lang: Lang): string {
	return pick(
		lang,
		`Read ${SITE_URL}/llms-full.txt and implement a PeerConnect peer in <your language>:
rooms and invites, joining the relay with a token, the Noise handshake and
pairing, and the reliable stream. Reproduce test-vectors/v1.json first. Use
${SITE_URL}/api/trial for tokens while testing. The repository's Rust and
JavaScript implementations are a reference, not a dependency.`,
		`阅读 ${SITE_URL}/llms-full.txt，用 <你的语言> 实现一个 PeerConnect 端：
房间与邀请、带 token 加入 relay、Noise 握手与配对、可靠消息流。
先复现 test-vectors/v1.json。测试时用 ${SITE_URL}/api/trial 获取 token。
仓库里的 Rust 和 JavaScript 实现只作参考，不作依赖。`
	);
}
