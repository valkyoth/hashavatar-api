# hashavatar-api 1.1.2 Release Notes

Status: draft

## Summary

`1.1.2` is the next stable patch release for the API, renderer dependency,
Rust toolchain, dependency graph, and CI tooling.

These notes are based on the current working tree and should be rechecked
against the final tag before publishing.

## Changed

- Bumped `hashavatar-api` to `1.1.2`.
- Updated the renderer dependency to `hashavatar` `1.1.2`.
- Updated the project toolchain and MSRV to Rust `1.97.0`.
- Updated `lru` to `0.18.1` and refreshed all compatible transitive crates.
- Updated the AWS SDK dependencies to their latest compatible releases.
- Updated `taiki-e/install-action` to `v2.83.0` and verified the remaining
  GitHub workflow actions are current.
- Updated the Docker builder and project documentation for Rust `1.97.0` and
  release `1.1.2`.

## Verification

```bash
cargo outdated --workspace --root-deps-only
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
