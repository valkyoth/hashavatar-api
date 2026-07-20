# hashavatar-website 0.5.0 Release Notes

Status: released

## Summary

`0.5.0` focused on security hardening, CI security coverage, and dependency
updates.

These notes are reconstructed from the tag and commit history.

## Added

- Added security self-test gates.
- Adopted GitHub CodeQL default setup.
- Added funding metadata updates for the repository.

## Changed

- Updated the `lru` dependency through the Dependabot update path.
- Prepared package and documentation metadata for `0.5.0`.

## Security Notes

- Bounded in-memory rate limiter state.
- Validated forwarded client IP handling.
- Replaced verbose internal error responses with generic client-facing errors.

## Verification

```bash
scripts/checks.sh
cargo test
```
