# hashavatar.app

`hashavatar.app` is the public HTTP service built on top of the `hashavatar` crate.

It exposes deterministic avatar generation through stable URLs so the service can sit safely behind Cloudflare and serve cached results at the edge.

## What It Does

- serves a landing page at `/`
- exposes a health endpoint at `/healthz`
- exposes a query-based avatar API at `/v1/avatar`
- exposes a path-based avatar API at `/avatar/{kind}/{identity}/{format}`

Supported formats:

- `webp`
- `png`
- `svg`

Supported avatar families:

- `cat`
- `dog`
- `robot`
- `fox`
- `alien`

## Example URLs

Query API:

```text
/v1/avatar?id=alice@example.com&kind=robot&background=white&format=webp&size=256
```

Path API:

```text
/avatar/cat/alice@example.com/svg
/avatar/fox/alice@example.com/png
```

## Why This Works Well Behind Cloudflare

Avatar responses are deterministic for the full request tuple:

- identity
- avatar kind
- background mode
- output format
- size

That makes aggressive edge caching appropriate.

The service already emits:

- `Cache-Control: public, max-age=86400, s-maxage=31536000, immutable`
- `CDN-Cache-Control: public, max-age=31536000, immutable`
- `Cloudflare-CDN-Cache-Control: public, max-age=31536000, immutable`
- `ETag`

This gives browsers a shorter cache while allowing Cloudflare to keep hot avatar objects cached for a long time.

## Running Locally

From inside `public-website`:

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

## Running On Your Own Server

For self-hosting on Hetzner with Podman, see:

- [`DEPLOYMENT-HETZNER.md`](./DEPLOYMENT-HETZNER.md)
- [`deploy/podman-compose.yml`](./deploy/podman-compose.yml)
- [`deploy/Caddyfile`](./deploy/Caddyfile)

## Deployment Shape

Typical setup:

1. deploy the service to a public origin
2. put Cloudflare in front of it
3. cache only `/v1/avatar` and `/avatar/`
4. keep `/` and other operational endpoints separate from aggressive asset caching
5. rate-limit the avatar endpoints at Cloudflare

Deployment helper files included here:

- [`Dockerfile`](./Dockerfile)
- [`CLOUDFLARE.md`](./CLOUDFLARE.md)

## Operational Guidance

Recommended production practices:

- keep avatar sizes bounded
- enable HTTPS only
- restrict origin access to Cloudflare where possible
- normalize or document canonical URL usage
- monitor cache hit ratio and origin error rate

## Related Project

This service is powered by the parent crate:

- [`hashavatar`](../README.md)
