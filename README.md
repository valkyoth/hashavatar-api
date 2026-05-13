# hashavatar.app

`hashavatar.app` is the public HTTP service and demo website built on top of the `hashavatar` crate.

It exposes deterministic avatar generation through stable URLs so the service can sit behind Cloudflare and serve cached results at the edge.

## What It Does

- serves a landing page at `/`
- exposes a health endpoint at `/healthz`
- exposes a query-based avatar API at `/v1/avatar`
- exposes a signed object-storage link endpoint at `/v1/avatar/link`
- exposes a path-based avatar API at `/avatar/{kind}/{identity}/{format}`
- exposes OpenAPI metadata at `/docs/openapi.json`

Supported formats:

- `webp`
- `png`
- `jpg`
- `gif`
- `svg`

Supported backgrounds:

- `themed`
- `white`
- `black`
- `dark`
- `light`
- `transparent`

Supported avatar families are provided by `hashavatar 0.5.0`, including `cat`, `dog`, `robot`, `fox`, `alien`, `monster`, `ghost`, `slime`, `bird`, `wizard`, `skull`, `paws`, `planet`, `rocket`, `mushroom`, `cactus`, `frog`, `panda`, `cupcake`, `pizza`, `icecream`, `octopus`, and `knight`.

## Example URLs

Query API:

```text
/v1/avatar?id=cat@hashavatar.app&kind=cat&background=themed&format=webp&size=256
```

Path API:

```text
/avatar/cat/cat@hashavatar.app/svg
/avatar/fox/fox@hashavatar.app/png
```

## Why This Works Well Behind Cloudflare

Avatar responses are deterministic for the full request tuple:

- identity
- tenant
- style version
- avatar kind
- background mode
- output format
- size

That makes aggressive edge caching appropriate.

The service emits:

- `Cache-Control: public, max-age=86400, s-maxage=31536000, immutable`
- `CDN-Cache-Control: public, max-age=31536000, immutable`
- `Cloudflare-CDN-Cache-Control: public, max-age=31536000, immutable`
- `ETag`

## Running Locally

Requires Rust 1.95 or newer.

```bash
cargo run
```

Default bind:

```text
0.0.0.0:8080
```

Environment variables:

- `PORT`
- `PUBLIC_WEBSITE_HOST`
- `HASHAVATAR_S3_BUCKET`
- `HASHAVATAR_S3_REGION`
- `HASHAVATAR_S3_ENDPOINT`
- `HASHAVATAR_S3_PATH_STYLE`
- `HASHAVATAR_S3_PREFIX`
- `HASHAVATAR_S3_PRESIGN_TTL_SECONDS`
- `HASHAVATAR_TRUSTED_PROXIES`

`HASHAVATAR_TRUSTED_PROXIES` accepts a comma or whitespace separated list of IP
addresses and CIDR ranges. Forwarded client IP headers are ignored unless the
direct peer address matches this allowlist.

## Security Checks

Recommended local checks:

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo audit
cargo deny check
```

## Running On Your Own Server

For self-hosting on Hetzner with Podman, see:

- [`DEPLOYMENT-HETZNER.md`](./DEPLOYMENT-HETZNER.md)
- [`deploy/podman-compose.yml`](./deploy/podman-compose.yml)
- [`deploy/fluxheim.toml`](./deploy/fluxheim.toml)

## Related Project

This service is powered by:

- [`hashavatar`](https://crates.io/crates/hashavatar)
- [`hashavatar` docs](https://docs.rs/hashavatar/latest/hashavatar/)
