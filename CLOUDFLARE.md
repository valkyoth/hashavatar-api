# Cloudflare Setup

The goal is simple: cache avatar responses at the edge so your origin only gets hit on cold misses.

## 1. Put Cloudflare in front of the service

Deploy this app somewhere public:

- Fly.io
- Render
- Railway
- VPS with Docker/Nginx

Point a hostname like `avatars.yourdomain.com` at that origin through Cloudflare.

## 2. Create a Cache Rule

In Cloudflare Dashboard:

- `Rules`
- `Cache Rules`
- `Create rule`

Recommended expression:

```text
(http.host eq "avatars.yourdomain.com" and starts_with(http.request.uri.path, "/v1/avatar")) or
(http.host eq "avatars.yourdomain.com" and starts_with(http.request.uri.path, "/avatar/"))
```

Recommended actions:

- `Eligible for cache`: On
- `Cache key`: include query string for `/v1/avatar`
- `Edge TTL`: respect origin or set to 1 year
- `Browser TTL`: respect origin

For the path API, query strings are not required unless you add optional parameters later.

## 3. Keep the landing page dynamic

Do not apply the long avatar cache rule to `/`. Only cache the avatar endpoints.

## 4. Rate limiting

Add a Cloudflare Rate Limiting rule for avatar endpoints.

Recommended starting point:

- path starts with `/v1/avatar` or `/avatar/`
- 120 requests per minute per IP
- managed challenge or temporary block

Tune based on real traffic.

## 5. Origin protection

Recommended:

- only allow traffic from Cloudflare IPs at the firewall or reverse proxy
- enable gzip/brotli at Cloudflare
- use HTTPS only

## 6. Optional custom cache key normalization

If you want stricter cache deduplication:

- normalize query ordering at the application layer
- prefer canonical URLs in clients
- include every rendering parameter in the cache key: `id`, `tenant`,
  `style_version`, `algorithm`, `kind`, `background`, `accessory`, `color`,
  `expression`, `shape`, `format`, and `size`
- prefer the path API when you want the cleanest cache key shape

## 7. Why this is safe to cache hard

The avatar response is deterministic for the full request tuple:

- identity
- tenant and style version
- algorithm
- kind
- background
- accessory
- color
- expression
- shape
- format
- size

That means long-lived immutable edge caching is appropriate.
