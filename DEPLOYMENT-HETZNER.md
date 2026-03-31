# Hetzner Deployment With Podman Compose

This setup is intended for a single Hetzner server using Podman and Caddy.

## What You Get

- the `hashavatar.app` Rust service running on an internal container network
- Caddy terminating HTTPS and serving the public domain
- automatic TLS through Caddy
- long-cache avatar responses ready for Cloudflare

## Files

- [`deploy/podman-compose.yml`](./deploy/podman-compose.yml)
- [`deploy/Caddyfile`](./deploy/Caddyfile)
- [`Dockerfile`](./Dockerfile)

## Directory Layout

Recommended on the server:

```text
/srv/hashavatar-app/
  public-website/
    deploy/
    Dockerfile
    Cargo.toml
    src/
```

## Prerequisites

- Podman
- podman-compose
- a DNS record for `hashavatar.app` pointing to your server
- optional but recommended: Cloudflare proxy enabled in front of the origin

## Start The Stack

From inside `public-website`:

```bash
cd deploy
podman-compose up -d --build
```

## Update The Service

After pulling new code:

```bash
cd deploy
podman-compose up -d --build
```

## Notes

- Caddy listens on `80` and `443`
- the Rust app stays internal on port `8080`
- if Cloudflare is in front, keep DNS proxied and apply the cache rules from [`CLOUDFLARE.md`](./CLOUDFLARE.md)
- the avatar endpoints are deterministic, so they are safe to cache hard at the edge
