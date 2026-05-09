# Hetzner Deployment With Podman Compose And Fluxheim

This setup is intended for a single Hetzner server using Podman and Fluxheim.

## What You Get

- the `hashavatar.app` Rust service running on an internal container network
- Fluxheim running as the public HTTP/HTTPS gateway
- Wolfi-based runtime images for both Fluxheim and the Rust service
- long-cache avatar responses ready for Cloudflare

## Files

- [`deploy/podman-compose.yml`](./deploy/podman-compose.yml)
- [`deploy/fluxheim.toml`](./deploy/fluxheim.toml)
- [`Dockerfile`](./Dockerfile)

## Directory Layout

Recommended on the server:

```text
/srv/hashavatar-app/
  public-website/
    deploy/
      fluxheim.toml
      tls/
        hashavatar.app/
          fullchain.pem
          privkey.pem
    Dockerfile
    Cargo.toml
    src/
```

## Prerequisites

- Podman
- podman-compose
- a DNS record for `hashavatar.app` pointing to your server
- a TLS certificate and private key mounted at `deploy/tls/hashavatar.app/fullchain.pem` and `deploy/tls/hashavatar.app/privkey.pem`
- optional but recommended: Cloudflare proxy enabled in front of the origin

Cloudflare Origin CA, Let's Encrypt, or another operator-managed certificate can
be used. Fluxheim validates and serves the mounted certificate; this deployment
example does not run a separate ACME client.

The Fluxheim container runs as UID `65532`, so the mounted certificate and key
must be readable by that container user. A common setup is:

```bash
sudo chown -R 65532:65532 deploy/tls
sudo chmod 0644 deploy/tls/hashavatar.app/fullchain.pem
sudo chmod 0600 deploy/tls/hashavatar.app/privkey.pem
```

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

- Fluxheim listens publicly on `80` and `443`
- the Rust app stays internal on port `8080`
- if Cloudflare is in front, keep DNS proxied and apply the cache rules from [`CLOUDFLARE.md`](./CLOUDFLARE.md)
- the avatar endpoints are deterministic, so they are safe to cache hard at the edge
