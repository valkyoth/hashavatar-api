#!/usr/bin/env sh
set -eu

fail() {
    echo "security invariants: $*" >&2
    exit 1
}

grep -q 'MAX_RATE_LIMIT_BUCKETS' src/main.rs \
    || fail "rate limiter capacity constant is missing"
grep -q 'LruCache' src/main.rs \
    || fail "rate limiter must use bounded LRU storage"
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
grep -q 'client_ip_ignores_forwarded_headers_from_untrusted_peers' src/main.rs \
    || fail "forwarded-header spoofing regression test is missing"
grep -q 'internal_error_does_not_expose_details' src/main.rs \
    || fail "internal error disclosure regression test is missing"
grep -q 'build_avatar_asset_rejects_oversized_namespace' src/main.rs \
    || fail "hashavatar namespace validation regression test is missing"

if [ -e .github/workflows/codeql.yml ] || [ -e .github/codeql/codeql-config.yml ]; then
    fail "CodeQL default setup is enabled in GitHub; remove repo-level advanced CodeQL configuration"
fi

echo "security invariants: ok"
