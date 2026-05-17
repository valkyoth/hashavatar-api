#!/usr/bin/env sh
set -eu

first_target="${HASHAVATAR_API_REPRO_TARGET_A:-target/reproducible-a}"
second_target="${HASHAVATAR_API_REPRO_TARGET_B:-target/reproducible-b}"

if command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
    SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
else
    SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-0}"
fi
export SOURCE_DATE_EPOCH

CARGO_TARGET_DIR="$first_target" cargo build --release --locked
CARGO_TARGET_DIR="$second_target" cargo build --release --locked

first_binary="$first_target/release/hashavatar-api"
second_binary="$second_target/release/hashavatar-api"

if ! cmp -s "$first_binary" "$second_binary"; then
    echo "release binary is not reproducible across two clean target directories" >&2
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$first_binary" "$second_binary" >&2
    fi
    exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$first_binary"
else
    cksum "$first_binary"
fi

echo "reproducible build check: ok"
