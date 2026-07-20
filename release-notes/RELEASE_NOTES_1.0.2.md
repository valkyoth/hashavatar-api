# hashavatar-website 1.0.2 Release Notes

Status: released

## Summary

`1.0.2` updated the API to `hashavatar` `1.0.2` and continued stable-series
hardening.

These notes are reconstructed from the tag and commit history.

## Changed

- Updated the renderer dependency to the published `hashavatar` `1.0.2` crate.
- Updated documentation and release metadata for `1.0.2`.
- Reviewed deployment security notes after the HSTS/proxy discussion and left
  proxy-managed HTTPS behavior documented.

## Security Notes

- Reviewed and fixed actionable items from the added pentest report.
- Removed the pentest report after remediation so it was not shipped as a
  repository artifact.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
