# hashavatar-api 1.1.0-release Release Notes

Status: released

## Summary

`1.1.0-release` was the immutable replacement tag used for the `1.1.0`
container release after the original `v1.1.0` tag had already been published.

These notes are reconstructed from the tag and commit history.

## Fixed

- Included the locale configuration directory in the container build context so
  the localized website can start correctly from the published image.

## Notes

- The application version remained `1.1.0`.
- The extra tag was used because the original `v1.1.0` tag was immutable.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
