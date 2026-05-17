# Security Policy

## Supported Versions

Security fixes are expected for the latest published `0.7.x` release.

## Reporting a Vulnerability

Please report security issues privately through GitHub Security Advisories for:

`https://github.com/valkyoth/hashavatar-api/security/advisories/new`

If GitHub advisories are unavailable, open a minimal public issue that asks for a private contact path without disclosing exploit details.

## Scope

Relevant security issues include:

- panics or resource exhaustion from untrusted avatar parameters
- unsafe SVG or output encoding behavior
- vulnerable dependency paths
- license or provenance concerns that affect safe redistribution

Please include reproduction steps, affected versions, and any known mitigations.

## Security Checks

CI runs formatting, release metadata validation, documentation link checks,
security invariant checks, clippy, tests, `cargo deny check`, `cargo audit`, a
local runtime smoke test, SBOM generation, and reproducible release build
checks. GitHub CodeQL default setup is enabled in repository security settings;
do not add a repo-level CodeQL workflow unless default setup is disabled first.

See `docs/SECURITY_CONTROLS.md` for the service-specific control map.
