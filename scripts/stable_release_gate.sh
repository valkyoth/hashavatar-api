#!/usr/bin/env sh
set -eu

mode="${1:-check}"

case "$mode" in
    check | release)
        ;;
    *)
        echo "usage: scripts/stable_release_gate.sh [check|release]" >&2
        exit 2
        ;;
esac

echo "stable release gate: fast checks"
scripts/checks.sh

echo "stable release gate: local runtime smoke"
scripts/smoke_local.sh

echo "stable release gate: SBOM"
scripts/generate-sbom.sh

echo "stable release gate: reproducible release build"
scripts/reproducible_build_check.sh

if [ "${HASHAVATAR_API_GATE_PODMAN:-0}" = "1" ]; then
    echo "stable release gate: Podman smoke"
    scripts/podman_smoke.sh
else
    echo "stable release gate: skipping Podman smoke; set HASHAVATAR_API_GATE_PODMAN=1 to enable"
fi

echo "stable release gate ($mode): ok"
