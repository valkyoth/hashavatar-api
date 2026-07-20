# hashavatar-website 0.9.0 Release Notes

Status: released

## Summary

`0.9.0` prepared the API and browser demo for the `hashavatar` `0.9.0`
renderer.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated the renderer dependency and package metadata to `0.9.0`.
- Fixed the browser CSP nonce path for inline structured data.
- Fixed demo/API parameter compatibility so preview requests no longer returned
  repeated 400 responses for supported options.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
cargo test
```
