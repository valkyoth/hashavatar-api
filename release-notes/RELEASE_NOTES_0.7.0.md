# hashavatar-website 0.7.0 Release Notes

Status: released

## Summary

`0.7.0` updated the API and demo site for `hashavatar` `0.7.0`.

These notes are reconstructed from the tag and commit history.

## Added

- Added demo controls for selecting the hash algorithm supported by the
  renderer.
- Exposed the algorithm choice through generated preview and API links.

## Changed

- Updated the renderer dependency and release metadata to `0.7.0`.
- Verified the local renderer integration before switching back to the
  published crate.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
cargo test
```
