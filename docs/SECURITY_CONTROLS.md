# Security Controls

This document maps the API service controls to the main risks covered by the
local test and release gates.

## Runtime Controls

| Risk | Control |
| --- | --- |
| Unbounded or expensive rate-limit state | The rate limiter uses bounded `LruCache` storage for O(1) LRU updates, 65,536 buckets, and regression tests for unique attacker keys. Avatar generation routes, including `/og.png`, pass through origin-side rate limits. |
| Forwarded-header spoofing | `X-Forwarded-For`, `X-Real-IP`, and `CF-Connecting-IP` are honored only when the direct peer is a configured trusted proxy; `X-Forwarded-For` chains are resolved from the rightmost untrusted address. |
| Verbose internal errors | Internal errors are logged with `tracing`; clients receive a generic static 500 body. |
| Browser-side content confusion | Responses receive CSP, `X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`, `Permissions-Policy`, `Cross-Origin-Resource-Policy`, and HTML-only `Cross-Origin-Opener-Policy` and `Strict-Transport-Security` headers. |
| Operational intelligence disclosure | `/metrics` is loopback-only and returns `404` to non-local peers; `/healthz` remains public for load balancers and uptime checks but only returns liveness status. |
| Object-storage metadata disclosure | Standard avatar responses do not expose presigned URLs or object keys in headers. `/v1/avatar/link` is the explicit JSON endpoint for signed-link metadata. |
| S3 prefix escaping | Tenant and style-version namespaces are limited to ASCII letters, digits, hyphens, and underscores before object keys are built. |
| Oversized avatar namespace or identity input | The service caps identities at 512 bytes and namespace components at 64 bytes before rendering. |
| Reflected error content | Client-facing `400` responses use static validation messages for parser and renderer errors instead of forwarding raw library error strings. |
| Cache identity collision | Cache keys and object keys include the active SHA-512 identity mode and output format. |
| PII in infrastructure logs | Email-shaped identities are accepted for compatibility; callers who want less personal data in URL logs should send opaque stable ids or one-way hashes. |
| Vulnerable or incompatible dependencies | `cargo audit` and `cargo deny check` run in the standard check script and CI. |

## Self-Testing

- `scripts/checks.sh` is the fast local gate for formatting, release metadata,
  documentation links, security invariants, clippy, tests, dependency policy,
  and RustSec advisories.
- `scripts/smoke_local.sh` starts the service locally, checks `/healthz`,
  renders WebP avatars, verifies security headers, rejects unsupported
  algorithms/formats, and checks oversized namespace rejection.
- `scripts/generate-sbom.sh` emits SPDX and CycloneDX SBOMs under
  `target/release-evidence`.
- `scripts/reproducible_build_check.sh` builds the release binary twice in
  separate target directories and compares the result.
- `scripts/stable_release_gate.sh` runs the fast gate, local smoke, SBOM, and
  reproducibility checks; optional Podman smoke can be enabled with
  `HASHAVATAR_API_GATE_PODMAN=1`.

## Boundaries

The API does not authenticate callers, encrypt responses, or provide abuse
protection beyond local rate limiting. Deployments should still use a trusted
reverse proxy, TLS, request logging controls, and infrastructure-level rate
limits appropriate for the environment.
