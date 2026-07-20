# hashavatar-website 0.13.0 Release Notes

Status: released

## Summary

`0.13.0` updated the API demo for the `hashavatar` `0.13.0` renderer and its
new background options.

These notes are reconstructed from the tag and commit history.

## Added

- Added demo support for the new `hashavatar` background choices.
- Added and tested GitHub-published Wolfi container image workflow support.

## Changed

- Updated the renderer dependency and documentation to `0.13.0`.
- Updated the Fluxheim/Wolfi deployment path and verified the container smoke
  test locally.

## Security Notes

- Rate-limited the expensive OG image path.
- Reduced unnecessary CSP nonce generation for non-HTML responses.
- Documented the S3 signed-link header behavior for operators.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
