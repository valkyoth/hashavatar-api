# Security Policy

## Supported Versions

Security fixes are expected for the latest published `0.5.x` release.

## Reporting a Vulnerability

Please report security issues privately through GitHub Security Advisories for:

`https://github.com/valkyoth/hashavatar/security/advisories/new`

If GitHub advisories are unavailable, open a minimal public issue that asks for a private contact path without disclosing exploit details.

## Scope

Relevant security issues include:

- panics or resource exhaustion from untrusted avatar parameters
- unsafe SVG or output encoding behavior
- vulnerable dependency paths
- license or provenance concerns that affect safe redistribution

Please include reproduction steps, affected versions, and any known mitigations.

## Security Checks

CI runs formatting, clippy, tests, `cargo deny check`, and `cargo audit`.
GitHub CodeQL default setup should be enabled in repository security settings;
keep only one active CodeQL configuration to avoid duplicate code-scanning
uploads.
