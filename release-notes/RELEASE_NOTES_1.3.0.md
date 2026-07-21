# hashavatar-website 1.3.0 Release Notes

Status: release candidate

## Summary

`1.3.0` prepares the website for the stable `hashavatar` `1.3.0` renderer and
adopts its migration-oriented prepared-request workflow without changing the
website's public avatar API.

These notes describe the crates.io-backed release candidate before the website
is tagged.

## Changed

- Bumped `hashavatar-website` and the renderer dependency to `1.3.0`.
- Migrated normal avatar and Open Graph rendering to immutable prepared
  requests that own derived identities rather than raw identity input.
- Bound strict style validation, effective family capabilities, resource
  accounting, typed semantic cache keys, and WebP output to one prepared tuple.
- Added regression coverage for the prepared request's declared RGBA resource
  budget.
- Preserved existing pixels, URL parameters, response formats, and S3
  object-key layout.

## Verification

The complete release gate, all application tests, application smoke test, SBOM
generation, reproducibility check, and normal Wolfi container build pass
against the published renderer.
