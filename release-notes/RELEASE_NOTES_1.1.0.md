# hashavatar-api 1.1.0 Release Notes

Status: released

## Summary

`1.1.0` added privacy-preserving telemetry, localized website copy, and the
`hashavatar` `1.1.0` renderer update.

These notes are reconstructed from the tag and commit history.

## Added

- Added optional OpenTelemetry-style website events for aggregate, non-PII
  usage stats.
- Added environment-driven telemetry enable/disable and endpoint configuration.
- Added TOML-backed website translations under `config/i18n/keys`.
- Added a language menu that affects only website copy, not avatar generation
  identity or API parameters.
- Added broad language coverage, including RTL language support.

## Changed

- Updated the renderer dependency to the published `hashavatar` `1.1.0` crate.
- Made the language selector searchable and scrollable so the larger language
  list remains usable.
- Documented that translations are AI-assisted best effort and can be improved
  through GitHub contributions.
- Updated the GitHub checkout action to `v7`.

## Security Notes

- Hardened telemetry and rate limiting after review.
- Kept telemetry aggregate-focused and documented it in the privacy policy.

## Verification

```bash
scripts/checks.sh
scripts/smoke_local.sh
scripts/podman_smoke.sh
```
