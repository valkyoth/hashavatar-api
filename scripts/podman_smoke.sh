#!/usr/bin/env sh
set -eu

ROOT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/hashavatar-website-podman.XXXXXX")
IMAGE="${HASHAVATAR_WEBSITE_IMAGE:-hashavatar-website:local-wolfi}"
CONTAINER_NAME="hashavatar-website-smoke-$$"
EXPECTED_UID="${HASHAVATAR_WEBSITE_EXPECTED_UID:-10001}"
KEEP_LOGS="${HASHAVATAR_WEBSITE_PODMAN_KEEP_LOGS:-0}"

cleanup() {
    status=$?
    podman rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
    if [ "$KEEP_LOGS" = "1" ] || [ "$status" -ne 0 ]; then
        echo "podman smoke artifacts kept in $TMP_DIR" >&2
    else
        rm -rf "$TMP_DIR"
    fi
}
trap cleanup EXIT INT TERM

if [ -z "${CONTAINER_HOST:-}" ] && [ -n "${XDG_RUNTIME_DIR:-}" ] && [ -S "$XDG_RUNTIME_DIR/podman/podman.sock" ]; then
    CONTAINER_HOST="unix://$XDG_RUNTIME_DIR/podman/podman.sock"
    export CONTAINER_HOST
fi

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

echo "podman smoke: image=$IMAGE port=$port"
if [ -n "${CONTAINER_HOST:-}" ]; then
    echo "podman smoke: CONTAINER_HOST=$CONTAINER_HOST"
fi

podman build -t "$IMAGE" -f "$ROOT_DIR/Dockerfile" "$ROOT_DIR"

podman run -d \
    --name "$CONTAINER_NAME" \
    -e PORT=8080 \
    -p "127.0.0.1:$port:8080" \
    "$IMAGE" >/dev/null

for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
    status="$(curl -sS -o "$TMP_DIR/health.json" -w '%{http_code}' "http://127.0.0.1:$port/healthz" 2>/dev/null || true)"
    if [ "$status" = "200" ]; then
        break
    fi
    sleep 0.3
done

if [ "${status:-}" != "200" ]; then
    echo "podman smoke failed: healthz returned ${status:-no response}" >&2
    podman logs "$CONTAINER_NAME" >&2 || true
    exit 1
fi

grep -q '"status":"ok"' "$TMP_DIR/health.json"
if grep -Eq '"(service|s3_enabled|style_version)"' "$TMP_DIR/health.json"; then
    echo "podman smoke failed: healthz exposed deployment details" >&2
    cat "$TMP_DIR/health.json" >&2
    exit 1
fi

curl -sSf -D "$TMP_DIR/webp.headers" \
    "http://127.0.0.1:$port/v1/avatar?id=cat@hashavatar.app&algorithm=sha512&kind=cat&background=themed&accessory=glasses&color=gold&expression=happy&shape=circle&format=webp&size=256" \
    -o "$TMP_DIR/avatar.webp"
grep -q '^RIFF' "$TMP_DIR/avatar.webp"
grep -qi '^content-type: image/webp' "$TMP_DIR/webp.headers"
grep -qi '^x-content-type-options: nosniff' "$TMP_DIR/webp.headers"

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
        "http://127.0.0.1:$port/v1/avatar?id=robot@hashavatar.app&algorithm=sha512&kind=robot&background=white&format=png&size=128"
)"
if [ "$bad_format_status" != "400" ]; then
    echo "podman smoke failed: png format returned $bad_format_status, expected 400" >&2
    exit 1
fi
grep -q 'unsupported avatar format: expected webp' "$TMP_DIR/bad-format.txt"

bad_algorithm_status="$(
    curl -sS -o "$TMP_DIR/bad-algorithm.txt" -w '%{http_code}' \
        "http://127.0.0.1:$port/v1/avatar?id=robot@hashavatar.app&algorithm=blake3&kind=robot&background=white&format=webp&size=128"
)"
if [ "$bad_algorithm_status" != "400" ]; then
    echo "podman smoke failed: blake3 algorithm returned $bad_algorithm_status, expected 400" >&2
    exit 1
fi
grep -q 'unsupported hash algorithm: expected sha512' "$TMP_DIR/bad-algorithm.txt"

USER_LINE="$(podman run --rm --entrypoint /bin/sh "$IMAGE" -c id)"
case "$USER_LINE" in
    *"uid=$EXPECTED_UID"* )
        ;;
    * )
        echo "podman smoke failed: expected runtime uid=$EXPECTED_UID, got: $USER_LINE" >&2
        exit 1
        ;;
esac

echo "podman smoke: ok"
