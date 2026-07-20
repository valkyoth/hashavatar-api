# hashavatar-website 1.0.3 Release Notes

Status: released

## Summary

`1.0.3` updated the stable API to the latest renderer and deployment stack at
the time.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated the service and documentation to `hashavatar-website` `1.0.3`.
- Updated the renderer dependency to the latest `hashavatar` crate available for
  the release.
- Updated direct crates and GitHub workflow tooling where possible.
- Updated the Podman Compose deployment to use Fluxheim `1.5.14`.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
