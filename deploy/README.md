# Deploying With Fluxheim

This folder contains a small Podman Compose deployment for `hashavatar.app`.
It runs two containers on one private network:

- `hashavatar`: builds this repository with the Wolfi runtime image from
  `../Dockerfile` and listens internally on port `8080`.
- `fluxheim`: runs `ghcr.io/valkyoth/fluxheim:v1.6.30-wolfi`, publishes ports
  `80` and `443`, terminates TLS, redirects HTTP to HTTPS, redirects
  `www.hashavatar.app` to `hashavatar.app`, and proxies traffic to
  `hashavatar:8080`.

The app uses the direct peer IP for rate limiting by default. The compose file
sets `HASHAVATAR_TRUSTED_PROXIES=10.89.42.0/24` and pins the private network to
that subnet so the app only honors `X-Forwarded-For` style headers from the
Fluxheim network. Do not expose the app container port directly to the internet.
Keep this trusted-proxy range aligned with the private proxy network; widening it
can let untrusted peers inflate rate-limit key cardinality.
Do not add a Fluxheim route for `/metrics`: the application also checks for a
loopback peer, but a same-host reverse proxy connects from loopback and would
make metrics public if explicitly forwarded.

The app container is hardened for the expected runtime shape: read-only root
filesystem, no new privileges, all Linux capabilities dropped, and a small
`/tmp` tmpfs for temporary runtime files.

## Optional OpenTelemetry

OpenTelemetry metrics are disabled by default. Enable them only when you have an
OTLP collector or observability backend ready:

```bash
HASHAVATAR_OTLP=enabled \
HASHAVATAR_OTLP_ENDPOINT=http://otel-collector:4318/v1/metrics \
podman compose -f deploy/podman-compose.yml up -d --build
```

The app records aggregate request, page-view, visible-time, click, and avatar
generation metrics. Labels are bounded to routes, sections, click categories,
and avatar style choices; identities, tenant names, URLs, referrers, user
agents, and IP addresses are not sent as metric attributes by the application.

## Files

- `podman-compose.yml`: starts the app and Fluxheim gateway.
- `fluxheim.toml`: Fluxheim listener, TLS, redirect, and proxy config.
- `tls/`: create this locally for the certificate files. It is intentionally
  not committed.

## TLS Files

Place your certificate and key here:

```text
deploy/tls/hashavatar.app/fullchain.pem
deploy/tls/hashavatar.app/privkey.pem
```

The Fluxheim container runs as UID `65532`, so the mounted files must be
readable by that user:

```bash
sudo chown -R 65532:65532 deploy/tls
sudo chmod 0644 deploy/tls/hashavatar.app/fullchain.pem
sudo chmod 0600 deploy/tls/hashavatar.app/privkey.pem
```

Cloudflare Origin CA, Let's Encrypt, or another operator-managed certificate
can be used. This example does not request certificates automatically.

## Start

From the repository root:

```bash
podman compose -f deploy/podman-compose.yml up -d --build
```

Or from this directory:

```bash
podman compose -f podman-compose.yml up -d --build
```

## Check

```bash
podman compose -f deploy/podman-compose.yml ps
podman logs hashavatar-fluxheim
podman logs hashavatar-app
curl -k -H 'Host: hashavatar.app' https://127.0.0.1/healthz
```

## Update

Pull the new repository version, then rebuild the app container and restart the
gateway stack:

```bash
podman compose -f deploy/podman-compose.yml up -d --build
```
