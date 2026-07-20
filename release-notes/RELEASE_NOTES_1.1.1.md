# hashavatar-website 1.1.1 Release Notes

Status: draft

## Summary

`1.1.1` is the next stable patch release for the API, renderer dependency,
documentation, and build tooling.

These notes are based on the current working tree and should be rechecked
against the final tag before publishing.

## Changed

- Bumped `hashavatar-website` to `1.1.1`.
- Updated the renderer dependency to `hashavatar` `1.1.1`.
- Refreshed the lockfile with current compatible crate updates.
- Updated GitHub workflow tooling where newer action versions are available.
- Kept the project Rust toolchain pinned to Rust `1.96.0`.
- Updated the README for the `1.1.1` service and renderer versions.
- Expanded the website language note to state that translations are
  AI-assisted best effort and that native-speaker fixes are welcome.

## Verification

```bash
cargo outdated --workspace --root-deps-only
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
