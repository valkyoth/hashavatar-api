# hashavatar-website 0.12.0 Release Notes

Status: released

## Summary

`0.12.0` updated the API and demo for the stricter `hashavatar` `0.12.0`
renderer.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated the renderer dependency and release metadata to `0.12.0`.
- Simplified the public demo around the supported SHA-512 and WebP path.
- Fixed preview refresh behavior when identities change.
- Hardened handling of invalid and non-ASCII identity input.

## Security Notes

- Moved CPU-heavy avatar rendering off Tokio executor threads.
- Restricted or documented operational endpoints.
- Tightened error handling, security headers, health responses, and metrics
  behavior after pentest review.
- Removed direct high-throughput S3 write bypasses from the regular avatar path.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
cargo test
```
