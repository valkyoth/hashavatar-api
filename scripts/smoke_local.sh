#!/usr/bin/env sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/hashavatar-api-smoke.XXXXXX")
KEEP_LOGS=${HASHAVATAR_API_SMOKE_KEEP_LOGS:-0}
BUILD_RELEASE=${HASHAVATAR_API_SMOKE_RELEASE:-0}
HASHAVATAR_API_PID=

cleanup() {
    status=$?

    if [ -n "$HASHAVATAR_API_PID" ]; then
        kill "$HASHAVATAR_API_PID" 2>/dev/null || true
        sleep 0.2
        if kill -0 "$HASHAVATAR_API_PID" 2>/dev/null; then
            kill -9 "$HASHAVATAR_API_PID" 2>/dev/null || true
        fi
        wait "$HASHAVATAR_API_PID" 2>/dev/null || true
    fi

    if [ "$KEEP_LOGS" = "1" ] || [ "$status" -ne 0 ]; then
        echo "local smoke artifacts kept in $TMP_DIR" >&2
    else
        rm -rf "$TMP_DIR"
    fi
}
trap cleanup EXIT INT TERM

port=$(python3 - <<'PY'
import socket

sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
try:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
finally:
    sock.close()
PY
)

if [ "$BUILD_RELEASE" = "1" ]; then
    cargo build --quiet --release
    PORT="$port" PUBLIC_WEBSITE_HOST=127.0.0.1 "$ROOT_DIR/target/release/hashavatar-api" > "$TMP_DIR/server.log" 2>&1 &
else
    PORT="$port" PUBLIC_WEBSITE_HOST=127.0.0.1 cargo run --quiet > "$TMP_DIR/server.log" 2>&1 &
fi
HASHAVATAR_API_PID=$!

for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    status="$(curl -sS -o "$TMP_DIR/health.json" -w '%{http_code}' "http://127.0.0.1:$port/healthz" 2>/dev/null || true)"
    if [ "$status" = "200" ]; then
        break
    fi
    sleep 0.2
done

if [ "${status:-}" != "200" ]; then
    echo "local smoke failed: healthz returned ${status:-no response}" >&2
    exit 1
fi

grep -q '"status":"ok"' "$TMP_DIR/health.json"
grep -q '"service":"hashavatar-api"' "$TMP_DIR/health.json"

curl -sSf -D "$TMP_DIR/svg.headers" \
    "http://127.0.0.1:$port/v1/avatar?id=demo-cat&kind=cat&background=themed&format=svg&size=256" \
    -o "$TMP_DIR/avatar.svg"
grep -q '^<svg ' "$TMP_DIR/avatar.svg"
grep -qi '^content-type: image/svg+xml' "$TMP_DIR/svg.headers"
grep -qi '^x-content-type-options: nosniff' "$TMP_DIR/svg.headers"
grep -qi '^x-frame-options: DENY' "$TMP_DIR/svg.headers"
grep -qi '^referrer-policy: no-referrer' "$TMP_DIR/svg.headers"
grep -qi '^content-security-policy:' "$TMP_DIR/svg.headers"

curl -sSf -D "$TMP_DIR/png.headers" \
    "http://127.0.0.1:$port/v1/avatar?id=demo-robot&kind=robot&background=white&format=png&size=128" \
    -o "$TMP_DIR/avatar.png"
grep -qi '^content-type: image/png' "$TMP_DIR/png.headers"
test -s "$TMP_DIR/avatar.png"

bad_status="$(
    curl -sS -o "$TMP_DIR/bad-tenant.txt" -w '%{http_code}' \
        "http://127.0.0.1:$port/v1/avatar?id=demo-cat&tenant=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx&format=svg"
)"
if [ "$bad_status" != "400" ]; then
    echo "local smoke failed: oversized tenant returned $bad_status, expected 400" >&2
    exit 1
fi
grep -q 'tenant must be 1-64 ASCII' "$TMP_DIR/bad-tenant.txt"

email_status="$(
    curl -sS -o "$TMP_DIR/bad-email.txt" -w '%{http_code}' \
        "http://127.0.0.1:$port/v1/avatar?id=person@example.com&format=svg"
)"
if [ "$email_status" != "400" ]; then
    echo "local smoke failed: raw email identity returned $email_status, expected 400" >&2
    exit 1
fi
grep -q 'raw email addresses' "$TMP_DIR/bad-email.txt"

echo "local smoke: ok"
