#!/usr/bin/env sh
set -eu

fail() {
    echo "security invariants: $*" >&2
    exit 1
}

grep -q 'MAX_RATE_LIMIT_BUCKETS' src/main.rs \
    || fail "rate limiter capacity constant is missing"
grep -q 'struct RateLimiterState' src/main.rs \
    || fail "rate limiter bounded state is missing"
grep -q 'LruCache<String, RateBucket>' src/main.rs \
    || fail "rate limiter must use bounded O(1) LRU storage"
grep -q 'LruCache::new' src/main.rs \
    || fail "rate limiter LRU capacity initialization is missing"
if grep -q 'VecDeque<String>' src/main.rs || grep -q 'order.retain' src/main.rs; then
    fail "rate limiter must not use linear VecDeque touch operations"
fi
grep -q 'into_make_service_with_connect_info::<SocketAddr>' src/main.rs \
    || fail "server must expose peer socket addresses to handlers"
grep -q 'TRUSTED_PROXIES_ENV' src/main.rs \
    || fail "trusted proxy configuration is missing"
grep -q 'trusted_proxies.contains(peer_ip)' src/main.rs \
    || fail "forwarded IP headers must be gated by trusted proxy validation"
grep -q 'INTERNAL_ERROR_MESSAGE' src/main.rs \
    || fail "generic internal error message constant is missing"
grep -q 'fn add_security_headers' src/main.rs \
    || fail "security header middleware is missing"
grep -q 'fn secure_rng_failure' src/main.rs \
    || fail "CSP nonce RNG failure must fail closed"
if grep -q 'CSP_NONCE_FALLBACK_COUNTER' src/main.rs \
    || grep -q 'falling back to deterministic CSP nonce entropy' src/main.rs; then
    fail "CSP nonce generation must not use deterministic fallback entropy"
fi
grep -q 'MAX_ID_BYTES' src/main.rs \
    || fail "identity byte limit is missing"
grep -q 'MAX_NAMESPACE_COMPONENT_BYTES' src/main.rs \
    || fail "namespace component byte limit is missing"
grep -q 'fn is_valid_namespace_component' src/main.rs \
    || fail "path-safe namespace validation is missing"
grep -q 'fn rate_limit_ip_identity' src/main.rs \
    || fail "IPv6 rate-limit prefix aggregation is missing"
grep -q 'DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES)' src/main.rs \
    || fail "request body limit is missing"
grep -q 'async fn telemetry_gate' src/main.rs \
    || fail "pre-extraction telemetry rate limit is missing"
grep -q 'default-https-client' Cargo.toml \
    || fail "AWS HTTPS client feature is missing"
grep -q 'reqwest-rustls' Cargo.toml \
    || fail "OTLP HTTPS client feature is missing"
if ! awk '
    /^FROM / {
        seen = 1
        if ($2 !~ /@sha256:[0-9a-f]{64}$/) invalid = 1
    }
    END { exit !(seen && !invalid) }
' Dockerfile; then
    fail "container base images must be pinned by digest"
fi
if ! awk '
    /^[[:space:]]*uses:/ {
        split($2, action, "@")
        if (length(action[2]) != 40) exit 1
    }
' .github/workflows/*.yml; then
    fail "GitHub actions must be pinned to full commit SHAs"
fi

internal_error_body="$(
    awk '
        /^fn internal_error/ { in_fn = 1 }
        in_fn { print }
        in_fn && /^}/ { exit }
    ' src/main.rs
)"

printf '%s\n' "$internal_error_body" | grep -q 'tracing::error!' \
    || fail "internal_error must log detailed failures server-side"
printf '%s\n' "$internal_error_body" | grep -q 'INTERNAL_ERROR_MESSAGE' \
    || fail "internal_error must return a static generic client response"
if printf '%s\n' "$internal_error_body" | grep -q 'format!'; then
    fail "internal_error must not format internal details into the client response"
fi

grep -q 'rate_limiter_bounds_unique_attacker_keys' src/main.rs \
    || fail "rate limiter memory bound regression test is missing"
grep -q 'rate_limit_key_is_route_and_ip_scoped' src/main.rs \
    || fail "rate limiter route/IP scoping regression test is missing"
grep -q 'client_ip_ignores_forwarded_headers_from_untrusted_peers' src/main.rs \
    || fail "forwarded-header spoofing regression test is missing"
grep -q 'client_ip_uses_rightmost_untrusted_forwarded_ip' src/main.rs \
    || fail "forwarded-header chain regression test is missing"
grep -q 'internal_error_does_not_expose_details' src/main.rs \
    || fail "internal error disclosure regression test is missing"
grep -q 'build_avatar_asset_rejects_oversized_namespace' src/main.rs \
    || fail "hashavatar namespace validation regression test is missing"
grep -q 'object_key_uses_full_sha256_digest' src/main.rs \
    || fail "full object-key digest regression test is missing"
grep -q 'content_security_policy_uses_nonce_without_unsafe_inline' src/main.rs \
    || fail "CSP nonce regression test is missing"
grep -q 'render_json_ld_escapes_script_end_tags' src/main.rs \
    || fail "JSON-LD script breakout regression test is missing"
grep -q 'escape_html_attribute_handles_single_quotes' src/main.rs \
    || fail "attribute escaping regression test is missing"
grep -q 'etag_uses_full_sha256_digest' src/main.rs \
    || fail "full ETag digest regression test is missing"
grep -q 'metrics_generation_duration_saturates_at_u64_max' src/main.rs \
    || fail "metrics duration saturation regression test is missing"
grep -q 'ipv6_rate_limits_are_aggregated_to_prefix_64' src/main.rs \
    || fail "IPv6 rate-limit aggregation regression test is missing"
grep -q 'telemetry_rate_limit_runs_before_json_extraction' src/main.rs \
    || fail "pre-extraction telemetry rate-limit regression test is missing"
grep -q 'request_bodies_are_limited_to_four_kibibytes' src/main.rs \
    || fail "request body limit regression test is missing"
grep -q 's3_endpoint_requires_https_except_loopback' src/main.rs \
    || fail "S3 endpoint transport regression test is missing"

if [ -e .github/workflows/codeql.yml ] || [ -e .github/codeql/codeql-config.yml ]; then
    fail "CodeQL default setup is enabled in GitHub; remove repo-level advanced CodeQL configuration"
fi

echo "security invariants: ok"
