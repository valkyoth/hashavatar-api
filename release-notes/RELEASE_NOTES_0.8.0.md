# hashavatar-api 0.8.0 Release Notes

Status: released

## Summary

`0.8.0` updated the API for the `hashavatar` `0.8.0` renderer and included
pre-release hardening.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated the service and documentation for `hashavatar` `0.8.0`.
- Switched from local renderer testing back to the published crate before
  release.
- Refreshed dependencies before tagging.

## Security Notes

- Addressed reported issues in dependency versions, JSON-LD escaping, CSP
  handling, HTML attribute escaping, and ETag entropy.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
cargo test
```
