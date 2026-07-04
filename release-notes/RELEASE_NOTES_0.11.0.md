# hashavatar-api 0.11.0 Release Notes

Status: released

## Summary

`0.11.0` updated the API service to the `hashavatar` `0.11.0` renderer.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated the renderer dependency and package metadata to `0.11.0`.
- Re-tested the local renderer integration before moving to the published crate.
- Updated documentation for the `0.11.0` release.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
cargo test
```
