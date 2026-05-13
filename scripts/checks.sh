#!/usr/bin/env sh
set -eu

cargo fmt --all --check
scripts/validate-release-metadata.sh
perl scripts/check-doc-links.pl
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
cargo deny check
cargo audit
