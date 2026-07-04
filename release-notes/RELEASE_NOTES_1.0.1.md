# hashavatar-api 1.0.1 Release Notes

Status: released

## Summary

`1.0.1` was a stable-series hardening and maintenance release.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated Rust tooling and dependencies.
- Updated documentation and release metadata for `1.0.1`.

## Security Notes

- Added a process-wide cap on concurrent blocking avatar renders.
- Improved rate-limiter scalability and documented proxy/metrics deployment
  boundaries.
- Tightened S3 error handling and presigned URL TTL handling.
- Re-aligned audit and deny configuration expectations.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
