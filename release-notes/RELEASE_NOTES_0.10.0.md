# hashavatar-api 0.10.0 Release Notes

Status: released

## Summary

`0.10.0` expanded the demo site for the richer `hashavatar` `0.10.0` style
API.

These notes are reconstructed from the tag and commit history.

## Added

- Added demo controls for accessories, colors, expressions, and shapes.
- Added API/demo support for `AvatarStyleOptions`.

## Changed

- Updated the renderer dependency and release metadata to `0.10.0`.
- Adjusted the demo so avatar kinds only expose supported accessory choices.

## Security Notes

- Reviewed pentest findings around OG image rendering, proxy IP handling,
  privacy, panic handling, and network bind defaults during the release cycle.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
cargo test
```
