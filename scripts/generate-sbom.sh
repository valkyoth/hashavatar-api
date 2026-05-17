#!/usr/bin/env sh
set -eu

output_dir="${HASHAVATAR_API_SBOM_DIR:-target/release-evidence}"
mkdir -p "$output_dir"

if ! cargo sbom --version >/dev/null 2>&1; then
    echo "cargo-sbom is required; install with: cargo install --locked cargo-sbom --version 0.10.0" >&2
    exit 1
fi

spdx_output="$output_dir/hashavatar-api.spdx.json"
cyclonedx_output="$output_dir/hashavatar-api.cyclonedx.json"
manifest="$output_dir/sbom-MANIFEST.txt"

cargo sbom --output-format spdx_json_2_3 > "$spdx_output"
cargo sbom --output-format cyclone_dx_json_1_4 > "$cyclonedx_output"

test -s "$spdx_output"
test -s "$cyclonedx_output"
grep -q '"spdxVersion"[[:space:]]*:[[:space:]]*"SPDX-2.3"' "$spdx_output"
grep -q '"bomFormat"[[:space:]]*:[[:space:]]*"CycloneDX"' "$cyclonedx_output"

{
    echo "hashavatar-api SBOM evidence"
    echo
    echo "rustc:"
    rustc -Vv
    echo
    echo "cargo:"
    cargo -V
    echo
    echo "cargo-sbom:"
    cargo sbom --version
    echo
    echo "artifacts:"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$spdx_output" "$cyclonedx_output"
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$spdx_output" "$cyclonedx_output"
    else
        cksum "$spdx_output" "$cyclonedx_output"
    fi
} > "$manifest"

cat "$manifest"
