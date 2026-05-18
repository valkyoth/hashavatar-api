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
if grep -Eq '"(service|s3_enabled|style_version)"' "$TMP_DIR/health.json"; then
    echo "local smoke failed: healthz exposed deployment details" >&2
    cat "$TMP_DIR/health.json" >&2
    exit 1
fi

curl -sSf -D "$TMP_DIR/webp.headers" \
    "http://127.0.0.1:$port/v1/avatar?id=cat@hashavatar.app&algorithm=sha512&kind=cat&background=themed&accessory=glasses&color=gold&expression=happy&shape=circle&format=webp&size=256" \
    -o "$TMP_DIR/avatar.webp"
grep -q '^RIFF' "$TMP_DIR/avatar.webp"
grep -qi '^content-type: image/webp' "$TMP_DIR/webp.headers"
grep -qi '^x-content-type-options: nosniff' "$TMP_DIR/webp.headers"
grep -qi '^x-frame-options: DENY' "$TMP_DIR/webp.headers"
grep -qi '^referrer-policy: no-referrer' "$TMP_DIR/webp.headers"
grep -qi '^cross-origin-resource-policy: cross-origin' "$TMP_DIR/webp.headers"
grep -qi '^content-security-policy:' "$TMP_DIR/webp.headers"

curl -sSf -D "$TMP_DIR/og.headers" \
    "http://127.0.0.1:$port/og.png?id=cat@hashavatar.app&kind=cat" \
    -o "$TMP_DIR/og.png"
test -s "$TMP_DIR/og.png"
grep -qi '^content-type: image/png' "$TMP_DIR/og.headers"
grep -qi '^content-security-policy:' "$TMP_DIR/og.headers"

curl -sSf \
    "http://127.0.0.1:$port/v1/avatar?id=planet@hashavatar.app&algorithm=sha512&kind=planet&background=themed&accessory=glasses&color=gold&expression=happy&shape=circle&format=webp&size=256" \
    -o "$TMP_DIR/unsupported-accessory.webp"
grep -q '^RIFF' "$TMP_DIR/unsupported-accessory.webp"

curl -sSf \
    "http://127.0.0.1:$port/v1/avatar?id=wizard@hashavatar.app&algorithm=sha512&kind=wizard&background=starry&color=deep-sea-blue&expression=cool&shape=squircle&format=webp&size=256" \
    -o "$TMP_DIR/starry-background.webp"
grep -q '^RIFF' "$TMP_DIR/starry-background.webp"

bad_format_status="$(
    curl -sS -o "$TMP_DIR/bad-format.txt" -w '%{http_code}' \
        "http://127.0.0.1:$port/v1/avatar?id=cat@hashavatar.app&format=svg"
)"
if [ "$bad_format_status" != "400" ]; then
    echo "local smoke failed: svg format returned $bad_format_status, expected 400" >&2
    exit 1
fi
grep -q 'unsupported avatar format: expected webp' "$TMP_DIR/bad-format.txt"

bad_algorithm_status="$(
    curl -sS -o "$TMP_DIR/bad-algorithm.txt" -w '%{http_code}' \
        "http://127.0.0.1:$port/v1/avatar?id=cat@hashavatar.app&algorithm=blake3&format=webp"
)"
if [ "$bad_algorithm_status" != "400" ]; then
    echo "local smoke failed: blake3 algorithm returned $bad_algorithm_status, expected 400" >&2
    exit 1
fi
grep -q 'unsupported hash algorithm: expected sha512' "$TMP_DIR/bad-algorithm.txt"

bad_status="$(
    curl -sS -o "$TMP_DIR/bad-tenant.txt" -w '%{http_code}' \
        "http://127.0.0.1:$port/v1/avatar?id=cat@hashavatar.app&tenant=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx&format=webp"
)"
if [ "$bad_status" != "400" ]; then
    echo "local smoke failed: oversized tenant returned $bad_status, expected 400" >&2
    exit 1
fi
grep -q 'tenant must be 1-64 ASCII' "$TMP_DIR/bad-tenant.txt"

echo "local smoke: ok"
