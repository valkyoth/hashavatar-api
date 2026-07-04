# hashavatar-api 1.0.0 Release Notes

Status: released

## Summary

`1.0.0` was the first stable release of the hashavatar API and demo service.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated the service to the stable `hashavatar` `1.0.0` renderer.
- Updated package metadata, README content, deployment docs, and examples for
  the stable release.
- Refreshed dependencies and release tooling before tagging.

## Security Notes

- Reviewed and remediated pre-1.0 pentest findings before the stable tag.
- Kept expensive rendering paths bounded and moved CPU work away from the async
  executor where needed.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
