#!/bin/sh
# The self-serve flow end to end, all local (docs/self-serve.md): a fake
# GitHub, the relay Worker in workerd with tokens required, and the built
# site bound to it. Needs worker-build and the JS SDK built
# (js/scripts/build.sh). SITE_HOLD=1 keeps everything up for a browser.
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RELAY_DIR="$ROOT/crates/peerconnect-worker"
SITE="$ROOT/website"
RUN=$(mktemp -d)
free_port() { node -e 'const s=require("net").createServer().listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})'; }
RELAY_PORT=${RELAY_PORT:-$(free_port)}
SITE_PORT=${SITE_PORT:-$(free_port)}
GITHUB_PORT=${GITHUB_PORT:-$(free_port)}
TOKEN_SECRET=$(openssl rand -hex 32)
SERVICE=$(openssl rand -hex 32)
GH_ID=test-client-id
GH_SECRET=$(openssl rand -hex 20)
printf 'PEERCONNECT_TOKEN_SECRET=%s\nPEERCONNECT_SERVICE_SECRET=%s\nREQUIRE_TOKEN=1\n' "$TOKEN_SECRET" "$SERVICE" > "$RUN/relay.vars"
printf 'SERVICE_SECRET=%s\nGITHUB_CLIENT_ID=%s\nGITHUB_CLIENT_SECRET=%s\nSESSION_KEY=%s\nAPP_SECRET_KEY=%s\n' \
  "$SERVICE" "$GH_ID" "$GH_SECRET" "$(openssl rand -hex 32)" "$(openssl rand -hex 32)" > "$RUN/site.vars"

PIDS=""
cleanup() { for p in $PIDS; do kill -- -"$p" 2>/dev/null || kill "$p" 2>/dev/null; done; rm -rf "$RUN"; }
trap cleanup EXIT
set -m
node "$SITE/tests/fake-github.mjs" "$GITHUB_PORT" "$GH_ID" "$GH_SECRET" > "$RUN/github.log" 2>&1 & PIDS="$PIDS $!"

cd "$RELAY_DIR"
npx --yes wrangler@4 d1 migrations apply RELAY_DB --local --persist-to "$RUN/relay" > /dev/null
npx --yes wrangler@4 dev --env-file "$RUN/relay.vars" --persist-to "$RUN/relay" \
  --ip 127.0.0.1 --port "$RELAY_PORT" > "$RUN/relay.log" 2>&1 & PIDS="$PIDS $!"

cd "$SITE"
npx vite build > "$RUN/build.log" 2>&1 || { cat "$RUN/build.log" >&2; exit 1; }
npx wrangler d1 migrations apply SITE_DB --local -c wrangler.e2e.jsonc --persist-to "$RUN/site" > /dev/null
npx wrangler dev -c wrangler.e2e.jsonc --env-file "$RUN/site.vars" --persist-to "$RUN/site" \
  --var "GITHUB_OAUTH_BASE:http://127.0.0.1:$GITHUB_PORT" --var "GITHUB_API_BASE:http://127.0.0.1:$GITHUB_PORT" \
  --var "RELAY_PUBLIC_URL:ws://127.0.0.1:$RELAY_PORT" \
  --ip 127.0.0.1 --port "$SITE_PORT" > "$RUN/site.log" 2>&1 & PIDS="$PIDS $!"
set +m

# Our own servers only: another program may hold a port we were given.
wait_for() {
  TRIES=0
  until curl --noproxy '*' -s "http://127.0.0.1:$1/" | grep -q "$2"; do
    TRIES=$((TRIES + 1))
    [ "$TRIES" -lt 240 ] || { echo "port $1 did not come up" >&2; tail -30 "$RUN/relay.log" "$RUN/site.log" >&2; exit 1; }
    sleep 1
  done
}
wait_for "$RELAY_PORT" "peerconnect relay"
wait_for "$SITE_PORT" "PeerConnect"

if [ -n "$SITE_HOLD" ]; then
  echo "site http://127.0.0.1:$SITE_PORT  github http://127.0.0.1:$GITHUB_PORT  relay ws://127.0.0.1:$RELAY_PORT"
  while :; do sleep 3600; done
fi

# KEEP_LOG=<file>: the relay's log is kept there.
trap '[ -n "$KEEP_LOG" ] && cp "$RUN/relay.log" "$KEEP_LOG"; cleanup' EXIT
SITE_URL=http://127.0.0.1:$SITE_PORT RELAY_URL=ws://127.0.0.1:$RELAY_PORT GITHUB_URL=http://127.0.0.1:$GITHUB_PORT \
  node --test --test-concurrency=1 --test-timeout=120000 "$SITE/tests/self-serve.test.mjs" || {
  tail -40 "$RUN/site.log" "$RUN/relay.log" >&2
  exit 1
}
