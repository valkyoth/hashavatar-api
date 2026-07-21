# hashavatar-website 1.2.0 Release Notes

Status: released

## Summary

`1.2.0` prepares the website for the stable `hashavatar` `1.2.0` renderer and
validates the new migration contracts against the complete application.

These notes describe the signed `v1.2.0` release tag.

## Changed

- Bumped `hashavatar-website` and the renderer dependency to `1.2.0`.
- Adopted the renderer's authoritative family capability metadata for website
  controls, telemetry normalization, and style handling.
- Enabled strict style validation after unsupported family layers have been
  canonicalized to their neutral values.
- Replaced the website's manually assembled cache identity with the renderer's
  typed semantic WebP asset key.
- Preserved the existing public URL parameters and S3 object-key layout.
- Updated Tokio to `1.53.1` and `taiki-e/install-action` to `v2.84.0`.

## Verification

The complete release gate, all application tests, application smoke test, SBOM
generation, reproducibility check, and normal Wolfi container build pass
against the published renderer.
