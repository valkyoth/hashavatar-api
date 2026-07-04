# hashavatar-api 0.6.0 Release Notes

Status: released

## Summary

`0.6.0` moved the service to the published `hashavatar` `0.6.0` crate and
continued the security-first release process.

These notes are reconstructed from the tag and commit history.

## Added

- Added stronger self-testing and security gate coverage.
- Added documentation updates for the `0.6.0` API release.

## Changed

- Updated the renderer dependency to the published `hashavatar` `0.6.0`.
- Restored support for email-shaped avatar identities after accepting that risk
  for the public demo use case.

## Security Notes

- Fixed automated pentest findings around namespace validation, identity
  limits, S3 object-key entropy, mutex recovery, and ETag entropy.
- Kept detailed infrastructure errors out of client responses.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
cargo test
```
