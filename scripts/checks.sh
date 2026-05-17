#!/usr/bin/env sh
set -eu

echo "checks: formatting"
cargo fmt --all --check

echo "checks: release metadata"
scripts/validate-release-metadata.sh

echo "checks: documentation links"
perl scripts/check-doc-links.pl

echo "checks: security invariants"
scripts/validate-security-invariants.sh

echo "checks: cargo check"
cargo check

echo "checks: clippy"
cargo clippy --all-targets -- -D warnings

echo "checks: tests"
cargo test

echo "checks: dependency policy"
cargo deny check

echo "checks: RustSec advisories"
cargo audit

echo "checks: ok"
