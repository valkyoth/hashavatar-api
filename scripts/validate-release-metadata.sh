#!/usr/bin/env sh
set -eu

cargo_version="$(
    sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | sed -n '1p'
)"
cargo_rust_version="$(
    sed -n 's/^rust-version = "\([^"]*\)"/\1/p' Cargo.toml | sed -n '1p'
)"
toolchain_version="$(
    sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml | sed -n '1p'
)"
docker_rust_image="$(
    sed -n 's/^FROM rust:\([^ ]*\) AS build$/\1/p' Dockerfile | cut -d@ -f1 | sed -n '1p'
)"
lock_version="$(
    awk '
        $0 == "name = \"hashavatar-website\"" { in_package = 1; next }
        in_package && /^version = / {
            gsub(/version = "|"/, "", $0);
            print $0;
            exit
        }
    ' Cargo.lock
)"

if [ -z "$cargo_version" ]; then
    echo "release metadata: Cargo.toml package version is missing" >&2
    exit 1
fi

if [ -z "$cargo_rust_version" ]; then
    echo "release metadata: Cargo.toml rust-version is missing" >&2
    exit 1
fi

if [ "$toolchain_version" != "$cargo_rust_version.0" ]; then
    echo "release metadata: rust-toolchain.toml channel $toolchain_version does not match Cargo.toml rust-version $cargo_rust_version" >&2
    exit 1
fi

if [ "$docker_rust_image" != "$cargo_rust_version" ]; then
    echo "release metadata: Dockerfile Rust image $docker_rust_image does not match Cargo.toml rust-version $cargo_rust_version" >&2
    exit 1
fi

if [ "$lock_version" != "$cargo_version" ]; then
    echo "release metadata: Cargo.lock version $lock_version does not match Cargo.toml version $cargo_version" >&2
    exit 1
fi

if ! grep -q '^license = "EUPL-1.2"$' Cargo.toml; then
    echo "release metadata: Cargo.toml must declare license = \"EUPL-1.2\"" >&2
    exit 1
fi

for manifest in Cargo.toml Cargo.toml.split-template; do
    if ! grep -q '^name = "hashavatar-website"$' "$manifest"; then
        echo "release metadata: $manifest must name the private hashavatar-website package" >&2
        exit 1
    fi
    if ! grep -q '^publish = false$' "$manifest"; then
        echo "release metadata: $manifest must declare publish = false" >&2
        exit 1
    fi
    if ! grep -q '^repository = "https://github.com/valkyoth/hashavatar-website"$' "$manifest"; then
        echo "release metadata: $manifest has the wrong repository URL" >&2
        exit 1
    fi
done

if ! grep -q 'EUROPEAN UNION PUBLIC LICENCE v. 1.2' LICENSE; then
    echo "release metadata: LICENSE does not look like EUPL 1.2" >&2
    exit 1
fi

echo "release metadata: ok"
