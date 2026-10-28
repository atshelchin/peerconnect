#!/usr/bin/env bash
# Runs tests/tokens.test.mjs against this Worker under `wrangler dev`, with
# tokens required and a fresh local database. Needs worker-build and the JS
# SDK built (js/scripts/build.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
# A free port: others on this machine may hold the usual ones, and a test
# that reached the wrong server would mean nothing.
port=${PORT:-$(node -e 'const s=require("net").createServer().listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')}
token_secret=$(openssl rand -hex 32)
service_secret=$(openssl rand -hex 32)
state=$(mktemp -d)
trap 'kill "$pid" 2>/dev/null || true; rm -rf "$state"' EXIT

npx --yes wrangler@4 d1 migrations apply RELAY_DB --local --persist-to "$state" > /dev/null
npx --yes wrangler@4 dev --port "$port" --ip 127.0.0.1 --persist-to "$state" \
  --var REQUIRE_TOKEN:1 --var "PEERCONNECT_TOKEN_SECRET:$token_secret" \
  --var "PEERCONNECT_SERVICE_SECRET:$service_secret" --var ALERT_REJECTIONS:2 > "$state/wrangler.log" 2>&1 &
pid=$!
for _ in $(seq 1 180); do
  [ "$(curl -sf --noproxy '*' "http://127.0.0.1:$port/")" = "peerconnect relay" ] && break
  sleep 2
done
RELAY="ws://127.0.0.1:$port" SERVICE_SECRET="$service_secret" node --test tests/tokens.test.mjs || {
  cat "$state/wrangler.log"
  exit 1
}
