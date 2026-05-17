use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use hashavatar::{
    AVATAR_STYLE_VERSION, AvatarBackground, AvatarKind, AvatarNamespace, AvatarOptions,
    AvatarOutputFormat, AvatarSpec, encode_avatar_for_namespace, render_avatar_for_namespace,
    render_avatar_svg_for_namespace,
};
use image::{GenericImage, ImageBuffer, Rgba, RgbaImage};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8080;
const TRUSTED_PROXIES_ENV: &str = "HASHAVATAR_TRUSTED_PROXIES";
const DEFAULT_ID: &str = "cat@hashavatar.app";
const SITE_NAME: &str = "hashavatar.app";
const SITE_URL: &str = "https://hashavatar.app";
const REPOSITORY_URL: &str = "https://github.com/valkyoth/hashavatar-api";
const CRATE_URL: &str = "https://crates.io/crates/hashavatar/";
const DEFAULT_NAMESPACE_TENANT: &str = "public";
const DEFAULT_NAMESPACE_STYLE: &str = "v2";
const AVATAR_TIMEOUT_MS: u64 = 3_000;
const STORAGE_TIMEOUT_MS: u64 = 5_000;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_BUCKETS: usize = 16_384;
const INTERNAL_ERROR_MESSAGE: &str = "An internal server error occurred.";
const MIN_SIZE: u32 = 64;
const MAX_SIZE: u32 = 1024;
const PRESET_PAGE_SIZE: usize = 12;

struct AppState {
    storage: Option<Arc<S3Storage>>,
    trusted_proxies: TrustedProxies,
    rate_limiter: RateLimiter,
    metrics: Metrics,
}

impl Clone for AppState {
    fn clone(&self) -> Self {
        Self {
            storage: self.storage.clone(),
            trusted_proxies: self.trusted_proxies.clone(),
            rate_limiter: self.rate_limiter.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    let host = std::env::var("PUBLIC_WEBSITE_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let port = std::env::var("PORT")
        .ok()
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let address: SocketAddr = format!("{host}:{port}").parse()?;

    let state = AppState {
        storage: S3Storage::from_env().await?.map(Arc::new),
        trusted_proxies: TrustedProxies::from_env()?,
        rate_limiter: RateLimiter::default(),
        metrics: Metrics::default(),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/help", get(help_page))
        .route("/docs", get(docs_page))
        .route("/docs/openapi.json", get(openapi_json))
        .route("/terms", get(terms_page))
        .route("/privacy", get(privacy_page))
        .route("/robots.txt", get(robots_txt))
        .route("/sitemap.xml", get(sitemap_xml))
        .route("/favicon.svg", get(favicon_svg))
        .route("/site.webmanifest", get(site_webmanifest))
        .route("/og.png", get(og_png))
        .route("/metrics", get(metrics_json))
        .route("/healthz", get(healthz))
        .route("/v1/avatar", get(query_avatar))
        .route("/v1/avatar/link", get(query_avatar_link))
        .route("/avatar/{kind}/{identity}/{format}", get(path_avatar))
        .with_state(state)
        .layer(middleware::from_fn(add_security_headers));

    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(service = SITE_NAME, %address, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

fn init_logging() {
    let _ = tracing_subscriber::fmt::try_init();
}

async fn add_security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; connect-src 'self'; form-action 'self'",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=()"),
    );
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );

    response
}

async fn index() -> Html<String> {
    Html(render_index_html())
}

async fn help_page() -> Html<String> {
    Html(render_help_html())
}

async fn docs_page() -> Html<String> {
    Html(render_docs_html())
}

async fn terms_page() -> Html<String> {
    Html(render_terms_html())
}

async fn privacy_page() -> Html<String> {
    Html(render_privacy_html())
}

async fn robots_txt() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!(
            "User-agent: *\nAllow: /\n\nSitemap: {}/sitemap.xml\n",
            SITE_URL
        ),
    )
}

async fn sitemap_xml() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>{site}/</loc></url>
  <url><loc>{site}/help</loc></url>
  <url><loc>{site}/terms</loc></url>
  <url><loc>{site}/privacy</loc></url>
</urlset>"#,
            site = SITE_URL
        ),
    )
}

async fn favicon_svg() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="16" fill="#f7f0e6"/><ellipse cx="32" cy="34" rx="18" ry="16" fill="#8d4dcb"/><polygon points="20,25 24,10 30,24" fill="#4c2d68"/><polygon points="44,25 40,10 34,24" fill="#4c2d68"/><ellipse cx="25" cy="31" rx="4" ry="5" fill="#fcf8ec"/><ellipse cx="39" cy="31" rx="4" ry="5" fill="#fcf8ec"/><ellipse cx="25" cy="31" rx="2" ry="3" fill="#18141c"/><ellipse cx="39" cy="31" rx="2" ry="3" fill="#18141c"/><rect x="22" y="40" width="20" height="5" rx="2" fill="#301218"/></svg>"##.to_string(),
    )
}

async fn site_webmanifest() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/manifest+json; charset=utf-8",
        )],
        Json(serde_json::json!({
            "name": SITE_NAME,
            "short_name": "hashavatar",
            "start_url": "/",
            "display": "standalone",
            "background_color": "#fbf6ee",
            "theme_color": "#d97a42",
            "icons": [{
                "src": "/favicon.svg",
                "sizes": "64x64",
                "type": "image/svg+xml",
                "purpose": "any"
            }]
        })),
    )
}

async fn metrics_json(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(state.metrics.snapshot(state.storage.is_some())),
    )
}

async fn openapi_json() -> impl IntoResponse {
    Json(serde_json::json!({
        "openapi": "3.1.0",
        "info": {
            "title": "hashavatar.app API",
            "version": AVATAR_STYLE_VERSION.to_string(),
            "description": "Public procedural avatar API"
        },
        "servers": [{ "url": SITE_URL }],
        "paths": {
            "/v1/avatar": {
                "get": {
                    "summary": "Generate an avatar",
                    "parameters": [
                        {"name":"id","in":"query","schema":{"type":"string"}},
                        {"name":"tenant","in":"query","schema":{"type":"string"}},
                        {"name":"style_version","in":"query","schema":{"type":"string"}},
                        {"name":"kind","in":"query","schema":{"type":"string","enum": AvatarKind::ALL.map(|kind| kind.as_str())}},
                        {"name":"background","in":"query","schema":{"type":"string","enum": AvatarBackground::ALL.map(|background| background.as_str())}},
                        {"name":"format","in":"query","schema":{"type":"string","enum":["webp","png","jpg","gif","svg"]}},
                        {"name":"size","in":"query","schema":{"type":"integer","minimum": MIN_SIZE, "maximum": MAX_SIZE}}
                    ],
                    "responses": {"200":{"description":"Avatar asset"}}
                }
            },
            "/v1/avatar/link": {
                "get": {
                    "summary": "Persist to object storage and return a signed link",
                    "responses": {"200":{"description":"Signed link metadata"}}
                }
            },
            "/avatar/{kind}/{identity}/{format}": {
                "get": {
                    "summary": "Path-style avatar URL",
                    "responses": {"200":{"description":"Avatar asset"}}
                }
            },
            "/metrics": {
                "get": {
                    "summary": "Service metrics",
                    "responses": {"200":{"description":"Metrics JSON"}}
                }
            }
        }
    }))
}

async fn og_png(Query(query): Query<OgQuery>) -> Response {
    let title_id = query.id.unwrap_or_else(|| DEFAULT_ID.to_string());
    let namespace = match AvatarNamespace::new(
        query.tenant.as_deref().unwrap_or(DEFAULT_NAMESPACE_TENANT),
        query
            .style_version
            .as_deref()
            .unwrap_or(DEFAULT_NAMESPACE_STYLE),
    ) {
        Ok(namespace) => namespace,
        Err(error) => return bad_request(&error.to_string()),
    };
    let spec = AvatarSpec::new(220, 220, 0).expect("Open Graph avatar spec should be valid");

    let mut canvas: RgbaImage = ImageBuffer::from_pixel(1200, 630, Rgba([251, 246, 238, 255]));
    draw_rect(&mut canvas, 0, 0, 1200, 630, Rgba([242, 236, 228, 255]));
    draw_circle(&mut canvas, 160, 140, 180, Rgba([255, 214, 170, 180]));
    draw_circle(&mut canvas, 1030, 500, 150, Rgba([217, 122, 66, 70]));

    let lead_kind = query
        .kind
        .as_deref()
        .and_then(|raw| AvatarKind::from_str(raw).ok())
        .unwrap_or(AvatarKind::Monster);
    for (idx, kind) in [lead_kind, AvatarKind::Robot, AvatarKind::Ghost]
        .into_iter()
        .enumerate()
    {
        let avatar = match render_avatar_for_namespace(
            spec,
            namespace,
            &title_id,
            AvatarOptions::new(
                kind,
                if idx == 1 {
                    AvatarBackground::White
                } else {
                    AvatarBackground::Themed
                },
            ),
        ) {
            Ok(avatar) => avatar,
            Err(error) => return bad_request(&error.to_string()),
        };
        overlay(&mut canvas, &avatar, 110 + idx as u32 * 260, 180);
    }

    let bytes = {
        use image::ImageEncoder;
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf)
            .write_image(
                canvas.as_raw(),
                canvas.width(),
                canvas.height(),
                image::ExtendedColorType::Rgba8,
            )
            .expect("png encode");
        buf
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
        .into_response()
}

async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "service": "hashavatar-api",
            "s3_enabled": state.storage.is_some(),
            "style_version": AVATAR_STYLE_VERSION,
        })),
    )
}

#[derive(Clone, Copy)]
enum RateLimitRoute {
    Avatar,
    StorageLink,
}

impl RateLimitRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::Avatar => "avatar",
            Self::StorageLink => "storage-link",
        }
    }

    fn limit(self) -> u32 {
        match self {
            Self::Avatar => 240,
            Self::StorageLink => 30,
        }
    }
}

#[derive(Clone)]
struct RateLimiter {
    buckets: Arc<Mutex<lru::LruCache<String, RateBucket>>>,
}

#[derive(Clone, Copy)]
struct RateBucket {
    started_at: Instant,
    count: u32,
}

#[derive(Clone, Default)]
struct TrustedProxies {
    networks: Arc<Vec<IpNet>>,
}

impl TrustedProxies {
    fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        match std::env::var(TRUSTED_PROXIES_ENV) {
            Ok(raw) => Self::parse(&raw)
                .map_err(|message| format!("{TRUSTED_PROXIES_ENV}: {message}").into()),
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Err(error) => Err(Box::new(error)),
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        let mut networks = Vec::new();
        for value in raw.split([',', ' ', '\n', '\t']) {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }

            let network = value
                .parse::<IpNet>()
                .or_else(|_| value.parse::<IpAddr>().map(IpNet::from))
                .map_err(|_| format!("invalid trusted proxy address or CIDR: {value}"))?;
            networks.push(network);
        }

        Ok(Self {
            networks: Arc::new(networks),
        })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        self.networks.iter().any(|network| network.contains(&ip))
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::with_capacity(MAX_RATE_LIMIT_BUCKETS)
    }
}

impl RateLimiter {
    fn with_capacity(capacity: usize) -> Self {
        let capacity = NonZeroUsize::new(capacity.max(1)).expect("capacity is non-zero");
        Self {
            buckets: Arc::new(Mutex::new(lru::LruCache::new(capacity))),
        }
    }

    fn check(&self, key: String, limit: u32) -> bool {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().expect("rate limiter poisoned");
        let bucket = buckets.get_or_insert_mut(key, || RateBucket {
            started_at: now,
            count: 0,
        });
        if now.duration_since(bucket.started_at) >= RATE_LIMIT_WINDOW {
            bucket.started_at = now;
            bucket.count = 0;
        }
        if bucket.count >= limit {
            return false;
        }
        bucket.count += 1;
        true
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.lock().expect("rate limiter poisoned").len()
    }
}

#[derive(Default, Clone)]
struct Metrics {
    requests_total: Arc<AtomicU64>,
    avatar_rendered_total: Arc<AtomicU64>,
    avatar_link_total: Arc<AtomicU64>,
    limited_total: Arc<AtomicU64>,
    storage_write_total: Arc<AtomicU64>,
    storage_hit_total: Arc<AtomicU64>,
    storage_redirect_total: Arc<AtomicU64>,
    generation_millis_total: Arc<AtomicU64>,
    format_webp_total: Arc<AtomicU64>,
    format_png_total: Arc<AtomicU64>,
    format_jpeg_total: Arc<AtomicU64>,
    format_gif_total: Arc<AtomicU64>,
    format_svg_total: Arc<AtomicU64>,
}

#[derive(Serialize)]
struct MetricsSnapshot {
    requests_total: u64,
    avatar_rendered_total: u64,
    avatar_link_total: u64,
    limited_total: u64,
    storage_write_total: u64,
    storage_hit_total: u64,
    storage_redirect_total: u64,
    generation_millis_total: u64,
    formats: serde_json::Value,
    s3_enabled: bool,
}

impl Metrics {
    fn increment_format(&self, format: &str) {
        match format {
            "webp" => {
                self.format_webp_total.fetch_add(1, Ordering::Relaxed);
            }
            "png" => {
                self.format_png_total.fetch_add(1, Ordering::Relaxed);
            }
            "jpg" => {
                self.format_jpeg_total.fetch_add(1, Ordering::Relaxed);
            }
            "gif" => {
                self.format_gif_total.fetch_add(1, Ordering::Relaxed);
            }
            "svg" => {
                self.format_svg_total.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    fn observe_generation(&self, duration: Duration) {
        self.generation_millis_total
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
    }

    fn snapshot(&self, s3_enabled: bool) -> MetricsSnapshot {
        MetricsSnapshot {
            requests_total: self.requests_total.load(Ordering::Relaxed),
            avatar_rendered_total: self.avatar_rendered_total.load(Ordering::Relaxed),
            avatar_link_total: self.avatar_link_total.load(Ordering::Relaxed),
            limited_total: self.limited_total.load(Ordering::Relaxed),
            storage_write_total: self.storage_write_total.load(Ordering::Relaxed),
            storage_hit_total: self.storage_hit_total.load(Ordering::Relaxed),
            storage_redirect_total: self.storage_redirect_total.load(Ordering::Relaxed),
            generation_millis_total: self.generation_millis_total.load(Ordering::Relaxed),
            formats: serde_json::json!({
                "webp": self.format_webp_total.load(Ordering::Relaxed),
                "png": self.format_png_total.load(Ordering::Relaxed),
                "jpg": self.format_jpeg_total.load(Ordering::Relaxed),
                "gif": self.format_gif_total.load(Ordering::Relaxed),
                "svg": self.format_svg_total.load(Ordering::Relaxed),
            }),
            s3_enabled,
        }
    }
}

async fn enforce_limits(
    state: &AppState,
    headers: &HeaderMap,
    peer_ip: IpAddr,
    route: RateLimitRoute,
    request: &AvatarRequest,
) -> Result<(), Response> {
    let ip = client_ip(headers, peer_ip, &state.trusted_proxies);
    let key = format!(
        "{}:{}:{}:{}",
        route.as_str(),
        ip,
        request.namespace_tenant,
        request.kind.as_str()
    );
    let allowed = state.rate_limiter.check(key, route.limit());
    if allowed {
        Ok(())
    } else {
        state.metrics.limited_total.fetch_add(1, Ordering::Relaxed);
        Err((
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded, please retry shortly".to_string(),
        )
            .into_response())
    }
}

fn client_ip(headers: &HeaderMap, peer_ip: IpAddr, trusted_proxies: &TrustedProxies) -> String {
    if !trusted_proxies.contains(peer_ip) {
        return peer_ip.to_string();
    }

    for header_name in ["cf-connecting-ip", "x-forwarded-for", "x-real-ip"] {
        if let Some(value) = headers
            .get(header_name)
            .and_then(|value| value.to_str().ok())
            && let Some(first) = value.split(',').next()
        {
            let trimmed = first.trim();
            if let Ok(ip) = trimmed.parse::<IpAddr>() {
                return ip.to_string();
            }
        }
    }
    peer_ip.to_string()
}

async fn query_avatar(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<AvatarQuery>,
) -> Response {
    let request = match AvatarRequest::from_query(query) {
        Ok(request) => request,
        Err(message) => return bad_request(&message),
    };

    if let Err(response) = enforce_limits(
        &state,
        &headers,
        peer_addr.ip(),
        RateLimitRoute::Avatar,
        &request,
    )
    .await
    {
        return response;
    }
    serve_avatar(state, request).await
}

async fn query_avatar_link(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<AvatarQuery>,
) -> Response {
    let request = match AvatarRequest::from_query(query) {
        Ok(request) => request,
        Err(message) => return bad_request(&message),
    };

    if let Err(response) = enforce_limits(
        &state,
        &headers,
        peer_addr.ip(),
        RateLimitRoute::StorageLink,
        &request,
    )
    .await
    {
        return response;
    }
    serve_avatar_link(state, request).await
}

async fn path_avatar(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(path): Path<PathAvatar>,
) -> Response {
    let kind = match AvatarKind::from_str(&path.kind) {
        Ok(kind) => kind,
        Err(_) => return bad_request("unsupported avatar kind"),
    };
    let format = match AvatarRequestFormat::from_str(&path.format) {
        Ok(format) => format,
        Err(_) => return bad_request("unsupported avatar format"),
    };

    let request = AvatarRequest {
        identity: path.identity,
        namespace_tenant: DEFAULT_NAMESPACE_TENANT.to_string(),
        namespace_style: DEFAULT_NAMESPACE_STYLE.to_string(),
        kind,
        background: AvatarBackground::Themed,
        format,
        size: 256,
        persist: false,
        redirect: false,
    };

    if let Err(response) = enforce_limits(
        &state,
        &headers,
        peer_addr.ip(),
        RateLimitRoute::Avatar,
        &request,
    )
    .await
    {
        return response;
    }
    serve_avatar(state, request).await
}

async fn serve_avatar(state: AppState, request: AvatarRequest) -> Response {
    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let asset = match tokio::time::timeout(Duration::from_millis(AVATAR_TIMEOUT_MS), async {
        build_avatar_asset(&request)
    })
    .await
    {
        Ok(Ok(asset)) => asset,
        Ok(Err(message)) => return bad_request(&message),
        Err(_) => return request_timeout("avatar generation timed out"),
    };

    state
        .metrics
        .avatar_rendered_total
        .fetch_add(1, Ordering::Relaxed);
    state.metrics.observe_generation(started.elapsed());

    let format_name = request.format.as_str();
    state.metrics.increment_format(format_name);

    let etag = etag_for(&asset.cache_key);
    let mut headers = cache_headers(&etag);
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(asset.content_type),
    );

    if request.persist {
        let storage = match state.storage.as_ref() {
            Some(storage) => storage,
            None => return bad_request("S3 storage is not configured on this server"),
        };

        match tokio::time::timeout(
            Duration::from_millis(STORAGE_TIMEOUT_MS),
            storage.store_and_sign(&asset, &state.metrics),
        )
        .await
        {
            Ok(Ok(signed)) => {
                headers.insert(
                    HeaderName::storage_key(),
                    HeaderValue::from_str(&signed.object_key)
                        .unwrap_or_else(|_| HeaderValue::from_static("unavailable")),
                );
                headers.insert(
                    HeaderName::signed_url(),
                    HeaderValue::from_str(&signed.signed_url)
                        .unwrap_or_else(|_| HeaderValue::from_static("unavailable")),
                );

                if request.redirect {
                    state
                        .metrics
                        .storage_redirect_total
                        .fetch_add(1, Ordering::Relaxed);
                    return Redirect::temporary(&signed.signed_url).into_response();
                }
            }
            Ok(Err(error)) => return internal_error(error),
            Err(_) => return request_timeout("object storage timed out"),
        }
    }

    (StatusCode::OK, headers, asset.body).into_response()
}

async fn serve_avatar_link(state: AppState, request: AvatarRequest) -> Response {
    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    let storage = match state.storage.as_ref() {
        Some(storage) => storage,
        None => return bad_request("S3 storage is not configured on this server"),
    };

    let started = Instant::now();
    let asset = match tokio::time::timeout(Duration::from_millis(AVATAR_TIMEOUT_MS), async {
        build_avatar_asset(&request)
    })
    .await
    {
        Ok(Ok(asset)) => asset,
        Ok(Err(message)) => return bad_request(&message),
        Err(_) => return request_timeout("avatar generation timed out"),
    };
    state.metrics.observe_generation(started.elapsed());
    state
        .metrics
        .avatar_link_total
        .fetch_add(1, Ordering::Relaxed);

    match tokio::time::timeout(
        Duration::from_millis(STORAGE_TIMEOUT_MS),
        storage.store_and_sign(&asset, &state.metrics),
    )
    .await
    {
        Ok(Ok(signed)) => (
            StatusCode::OK,
            Json(AvatarLinkResponse {
                object_key: signed.object_key,
                signed_url: signed.signed_url,
                expires_in_seconds: storage.presign_ttl.as_secs(),
                content_type: asset.content_type.to_string(),
                cache_key: asset.cache_key,
            }),
        )
            .into_response(),
        Ok(Err(error)) => internal_error(error),
        Err(_) => request_timeout("object storage timed out"),
    }
}

fn build_avatar_asset(request: &AvatarRequest) -> Result<AvatarAsset, String> {
    let identity = request.identity.trim();
    if identity.is_empty() {
        return Err("missing identity".to_string());
    }
    if !(MIN_SIZE..=MAX_SIZE).contains(&request.size) {
        return Err("size must be between 64 and 1024".to_string());
    }

    let spec = AvatarSpec::new(request.size, request.size, 0).map_err(|error| error.to_string())?;
    let options = AvatarOptions::new(request.kind, request.background);
    let namespace = AvatarNamespace::new(&request.namespace_tenant, &request.namespace_style)
        .map_err(|error| error.to_string())?;
    let cache_key = format!(
        "{}:{}:{}:{}:{}:{}:{}",
        request.namespace_tenant,
        request.namespace_style,
        identity,
        request.kind,
        request.background,
        request.format,
        request.size
    );

    let (body, content_type) = match request.format {
        AvatarRequestFormat::Webp => (
            encode_avatar_for_namespace(
                spec,
                namespace,
                identity,
                AvatarOutputFormat::WebP,
                options,
            )
            .map_err(|error| format!("avatar generation failed: {error}"))?,
            "image/webp",
        ),
        AvatarRequestFormat::Png => (
            encode_avatar_for_namespace(
                spec,
                namespace,
                identity,
                AvatarOutputFormat::Png,
                options,
            )
            .map_err(|error| format!("avatar generation failed: {error}"))?,
            "image/png",
        ),
        AvatarRequestFormat::Jpeg => (
            encode_avatar_for_namespace(
                spec,
                namespace,
                identity,
                AvatarOutputFormat::Jpeg,
                options,
            )
            .map_err(|error| format!("avatar generation failed: {error}"))?,
            "image/jpeg",
        ),
        AvatarRequestFormat::Gif => (
            encode_avatar_for_namespace(
                spec,
                namespace,
                identity,
                AvatarOutputFormat::Gif,
                options,
            )
            .map_err(|error| format!("avatar generation failed: {error}"))?,
            "image/gif",
        ),
        AvatarRequestFormat::Svg => (
            render_avatar_svg_for_namespace(spec, namespace, identity, options)
                .map_err(|error| format!("avatar generation failed: {error}"))?
                .into_bytes(),
            "image/svg+xml",
        ),
    };

    Ok(AvatarAsset {
        body,
        content_type,
        cache_key,
        object_key: object_key_for(request, identity),
    })
}

fn cache_headers(etag: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400, s-maxage=31536000, immutable"),
    );
    headers.insert(
        HeaderName::cdn_cache_control(),
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        HeaderName::cloudflare_cache_control(),
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).unwrap_or_else(|_| HeaderValue::from_static("\"avatar\"")),
    );
    headers
}

fn etag_for(cache_key: &str) -> String {
    let digest = Sha256::digest(cache_key.as_bytes());
    let mut encoded = String::with_capacity(18);
    encoded.push('"');
    for byte in &digest[..8] {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded.push('"');
    encoded
}

fn object_key_for(request: &AvatarRequest, identity: &str) -> String {
    let digest = Sha256::digest(
        format!(
            "{}:{}:{}:{}:{}:{}:{}",
            request.namespace_tenant,
            request.namespace_style,
            identity,
            request.kind,
            request.background,
            request.format,
            request.size
        )
        .as_bytes(),
    );
    let mut encoded = String::with_capacity(20);
    for byte in &digest[..10] {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!(
        "{}/{}/{}/{}/{}/{}.{}",
        request.namespace_tenant,
        request.namespace_style,
        request.kind.as_str(),
        request.background.as_str(),
        request.size,
        encoded,
        request.format.as_str()
    )
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, message.to_string()).into_response()
}

fn internal_error(error: impl std::fmt::Display) -> Response {
    tracing::error!(error = %error, "avatar generation failed");
    (StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_ERROR_MESSAGE).into_response()
}

fn request_timeout(message: &str) -> Response {
    (StatusCode::REQUEST_TIMEOUT, message.to_string()).into_response()
}

fn draw_rect(image: &mut RgbaImage, x: u32, y: u32, width: u32, height: u32, color: Rgba<u8>) {
    for yy in y..(y + height).min(image.height()) {
        for xx in x..(x + width).min(image.width()) {
            image.put_pixel(xx, yy, color);
        }
    }
}

fn draw_circle(image: &mut RgbaImage, cx: i32, cy: i32, radius: i32, color: Rgba<u8>) {
    for y in -radius..=radius {
        for x in -radius..=radius {
            if x * x + y * y <= radius * radius {
                let px = cx + x;
                let py = cy + y;
                if px >= 0 && py >= 0 && (px as u32) < image.width() && (py as u32) < image.height()
                {
                    image.put_pixel(px as u32, py as u32, color);
                }
            }
        }
    }
}

fn overlay(canvas: &mut RgbaImage, image: &RgbaImage, x: u32, y: u32) {
    let _ = canvas.copy_from(image, x, y);
}

fn shared_page_styles() -> &'static str {
    r#"
    :root {
      --bg: #fbf6ee;
      --panel: rgba(255,255,255,0.86);
      --ink: #1f2933;
      --muted: #52606d;
      --line: rgba(31, 41, 51, 0.08);
      --accent: #d97a42;
      --accent-strong: #b85a25;
      --surface: rgba(255,255,255,0.74);
      font-family: "IBM Plex Sans", "Segoe UI", sans-serif;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      min-height: 100vh;
      background:
        radial-gradient(circle at top left, rgba(255, 214, 170, 0.95), transparent 26%),
        radial-gradient(circle at bottom right, rgba(217, 122, 66, 0.18), transparent 30%),
        linear-gradient(135deg, #fbf6ee, #f2ece4);
      color: var(--ink);
      padding: 32px 20px;
    }
    main {
      width: min(1180px, 100%);
      margin: 0 auto;
      background: var(--panel);
      border: 1px solid var(--line);
      border-radius: 28px;
      box-shadow: 0 24px 70px rgba(75, 48, 25, 0.14);
      overflow: hidden;
    }
    .site-nav {
      display: flex;
      justify-content: space-between;
      align-items: center;
      gap: 16px;
      padding: 20px 28px;
      border-bottom: 1px solid var(--line);
      background: rgba(255,255,255,0.5);
    }
    .brand {
      font-weight: 800;
      letter-spacing: -0.03em;
      color: var(--ink);
      text-decoration: none;
    }
    .nav-links, .footer-links {
      display: flex;
      flex-wrap: wrap;
      gap: 12px;
    }
    .nav-links a, .footer-links a, .inline-link {
      color: var(--accent-strong);
      text-decoration: none;
      font-weight: 700;
    }
    .nav-links a:hover, .footer-links a:hover, .inline-link:hover {
      text-decoration: underline;
    }
    .page {
      padding: 36px;
      display: grid;
      gap: 18px;
    }
    .eyebrow {
      text-transform: uppercase;
      color: var(--accent);
      font-weight: 700;
      font-size: 0.8rem;
      letter-spacing: 0.13em;
    }
    h1 {
      font-size: clamp(2.2rem, 6vw, 4.4rem);
      line-height: 0.95;
      margin: 8px 0 8px;
      letter-spacing: -0.05em;
      max-width: 12ch;
    }
    h2 {
      margin: 12px 0 8px;
      font-size: 1.2rem;
    }
    p, li {
      color: var(--muted);
      line-height: 1.7;
      font-size: 1rem;
    }
    ul {
      margin: 0;
      padding-left: 20px;
    }
    .lead {
      max-width: 70ch;
      margin: 0;
    }
    .content-grid {
      display: grid;
      gap: 18px;
      grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    }
    .card {
      padding: 20px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 22px;
      display: grid;
      gap: 10px;
    }
    pre {
      margin: 0;
      padding: 14px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 18px;
      overflow: auto;
      font-size: 0.94rem;
    }
    code {
      font-family: "IBM Plex Mono", monospace;
    }
    .site-footer {
      padding: 24px 28px 28px;
      border-top: 1px solid var(--line);
      display: grid;
      gap: 10px;
      background: rgba(255,255,255,0.52);
    }
    .footer-copy {
      color: var(--muted);
      font-size: 0.95rem;
    }
    @media (max-width: 860px) {
      .site-nav {
        align-items: start;
        flex-direction: column;
      }
      .page {
        padding: 24px;
      }
    }
    "#
}

fn render_footer_html() -> String {
    format!(
        r#"<footer class="site-footer">
  <div class="footer-links">
    <a href="/help">Help</a>
    <a href="/docs">Docs</a>
    <a href="/terms">Terms</a>
    <a href="/privacy">Privacy</a>
    <a href="{repo}" target="_blank" rel="noreferrer">Repository</a>
    <a href="{crate_url}" target="_blank" rel="noreferrer">Rust Crate</a>
  </div>
  <div class="footer-copy">
    hashavatar.app is a deterministic avatar API and demo service built on the open-source <code>hashavatar</code> Rust crate.
  </div>
</footer>"#,
        repo = REPOSITORY_URL,
        crate_url = CRATE_URL,
    )
}

fn escape_html_attribute(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn selected_attr(selected: bool) -> &'static str {
    if selected { " selected" } else { "" }
}

fn avatar_kind_label(kind: AvatarKind) -> &'static str {
    match kind {
        AvatarKind::Cat => "Cat",
        AvatarKind::Dog => "Dog",
        AvatarKind::Robot => "Robot",
        AvatarKind::Fox => "Fox",
        AvatarKind::Alien => "Alien",
        AvatarKind::Monster => "Monster",
        AvatarKind::Ghost => "Ghost",
        AvatarKind::Slime => "Slime",
        AvatarKind::Bird => "Bird",
        AvatarKind::Wizard => "Wizard",
        AvatarKind::Skull => "Skull",
        AvatarKind::Paws => "Paws",
        AvatarKind::Planet => "Planet",
        AvatarKind::Rocket => "Rocket",
        AvatarKind::Mushroom => "Mushroom",
        AvatarKind::Cactus => "Cactus",
        AvatarKind::Frog => "Frog",
        AvatarKind::Panda => "Panda",
        AvatarKind::Cupcake => "Cupcake",
        AvatarKind::Pizza => "Pizza",
        AvatarKind::Icecream => "Ice Cream",
        AvatarKind::Octopus => "Octopus",
        AvatarKind::Knight => "Knight",
    }
}

fn background_label(background: AvatarBackground) -> &'static str {
    match background {
        AvatarBackground::Themed => "Themed",
        AvatarBackground::White => "White",
        AvatarBackground::Black => "Black",
        AvatarBackground::Dark => "Dark",
        AvatarBackground::Light => "Light",
        AvatarBackground::Transparent => "Transparent",
    }
}

fn kind_options_html(selected: AvatarKind) -> String {
    AvatarKind::ALL
        .into_iter()
        .map(|kind| {
            format!(
                r#"<option value="{value}" data-identity="{value}@hashavatar.app"{selected}>{label}</option>"#,
                value = kind.as_str(),
                label = avatar_kind_label(kind),
                selected = selected_attr(kind == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn background_options_html(selected: AvatarBackground) -> String {
    AvatarBackground::ALL
        .into_iter()
        .map(|background| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = background.as_str(),
                label = background_label(background),
                selected = selected_attr(background == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_options_html(selected: AvatarRequestFormat) -> String {
    [
        (AvatarRequestFormat::Webp, "WebP"),
        (AvatarRequestFormat::Png, "PNG"),
        (AvatarRequestFormat::Jpeg, "JPEG/JPG"),
        (AvatarRequestFormat::Gif, "GIF"),
        (AvatarRequestFormat::Svg, "SVG"),
    ]
    .into_iter()
    .map(|(format, label)| {
        format!(
            r#"<option value="{value}"{selected}>{label}</option>"#,
            value = format.as_str(),
            selected = selected_attr(format == selected),
            label = label,
        )
    })
    .collect::<Vec<_>>()
    .join("\n")
}

#[derive(Serialize)]
struct PresetExample {
    label: &'static str,
    id: &'static str,
    kind: &'static str,
    background: &'static str,
    format: &'static str,
    size: &'static str,
}

fn preset_examples() -> Vec<PresetExample> {
    AvatarKind::ALL
        .into_iter()
        .map(|kind| PresetExample {
            label: avatar_kind_label(kind),
            id: match kind {
                AvatarKind::Icecream => "icecream@hashavatar.app",
                _ => kind.as_str(),
            },
            kind: kind.as_str(),
            background: match kind {
                AvatarKind::Dog
                | AvatarKind::Robot
                | AvatarKind::Slime
                | AvatarKind::Wizard
                | AvatarKind::Paws => "white",
                AvatarKind::Panda | AvatarKind::Knight => "light",
                AvatarKind::Ghost | AvatarKind::Skull => "dark",
                _ => "themed",
            },
            format: "webp",
            size: "256",
        })
        .map(|mut preset| {
            preset.id = match preset.kind {
                "cat" => "cat@hashavatar.app",
                "dog" => "dog@hashavatar.app",
                "robot" => "robot@hashavatar.app",
                "fox" => "fox@hashavatar.app",
                "alien" => "alien@hashavatar.app",
                "monster" => "monster@hashavatar.app",
                "ghost" => "ghost@hashavatar.app",
                "slime" => "slime@hashavatar.app",
                "bird" => "bird@hashavatar.app",
                "wizard" => "wizard@hashavatar.app",
                "skull" => "skull@hashavatar.app",
                "paws" => "paws@hashavatar.app",
                "planet" => "planet@hashavatar.app",
                "rocket" => "rocket@hashavatar.app",
                "mushroom" => "mushroom@hashavatar.app",
                "cactus" => "cactus@hashavatar.app",
                "frog" => "frog@hashavatar.app",
                "panda" => "panda@hashavatar.app",
                "cupcake" => "cupcake@hashavatar.app",
                "pizza" => "pizza@hashavatar.app",
                "icecream" => "icecream@hashavatar.app",
                "octopus" => "octopus@hashavatar.app",
                "knight" => "knight@hashavatar.app",
                _ => DEFAULT_ID,
            };
            preset
        })
        .collect()
}

fn preset_examples_json() -> String {
    serde_json::to_string(&preset_examples()).expect("preset examples should serialize")
}

fn render_meta_tags(title: &str, description: &str, path: &str) -> String {
    let canonical = if path == "/" {
        format!("{SITE_URL}/")
    } else {
        format!("{SITE_URL}{path}")
    };
    let preview_image = format!(
        "{site}/og.png?id=hashavatar.app&kind=monster",
        site = SITE_URL
    );
    let full_title = format!("{title} · {SITE_NAME}");

    format!(
        r#"<title>{title}</title>
  <meta name="description" content="{description}" />
  <meta name="robots" content="index,follow,max-image-preview:large,max-snippet:-1,max-video-preview:-1" />
  <link rel="canonical" href="{canonical}" />
  <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
  <link rel="manifest" href="/site.webmanifest" />
  <meta property="og:type" content="website" />
  <meta property="og:site_name" content="{site_name}" />
  <meta property="og:title" content="{title}" />
  <meta property="og:description" content="{description}" />
  <meta property="og:url" content="{canonical}" />
  <meta property="og:image" content="{image}" />
  <meta property="og:image:alt" content="Procedural avatar preview from hashavatar.app" />
  <meta name="twitter:card" content="summary_large_image" />
  <meta name="twitter:title" content="{title}" />
  <meta name="twitter:description" content="{description}" />
  <meta name="twitter:image" content="{image}" />
  {json_ld}"#,
        title = escape_html_attribute(&full_title),
        description = escape_html_attribute(description),
        canonical = escape_html_attribute(&canonical),
        image = escape_html_attribute(&preview_image),
        site_name = escape_html_attribute(SITE_NAME),
        json_ld = render_json_ld(&full_title, description, &canonical),
    )
}

fn render_json_ld(title: &str, description: &str, canonical: &str) -> String {
    let title = serde_json::to_string(title).unwrap_or_else(|_| "\"hashavatar.app\"".to_string());
    let description = serde_json::to_string(description)
        .unwrap_or_else(|_| "\"Deterministic avatar API\"".to_string());
    let canonical = serde_json::to_string(canonical).unwrap_or_else(|_| format!("\"{SITE_URL}/\""));
    let site_url = serde_json::to_string(SITE_URL).unwrap_or_else(|_| format!("\"{SITE_URL}\""));
    let search_target = serde_json::to_string(&format!("{SITE_URL}/?id={{search_term_string}}"))
        .unwrap_or_else(|_| format!("\"{SITE_URL}/?id={{search_term_string}}\""));

    format!(
        r#"<script type="application/ld+json">{{
  "@context": "https://schema.org",
  "@type": "WebSite",
  "name": {title},
  "url": {site_url},
  "description": {description},
  "potentialAction": {{
    "@type": "SearchAction",
    "target": {search_target},
    "query-input": "required name=search_term_string"
  }}
}}</script>
<script type="application/ld+json">{{
  "@context": "https://schema.org",
  "@type": "WebPage",
  "name": {title},
  "url": {canonical},
  "description": {description},
  "isPartOf": {{
    "@type": "WebSite",
    "name": "hashavatar.app",
    "url": {site_url}
  }}
}}</script>"#,
        title = title,
        description = description,
        canonical = canonical,
        site_url = site_url,
        search_target = search_target,
    )
}

fn render_page_html(
    page_title: &str,
    description: &str,
    path: &str,
    eyebrow: &str,
    lead: &str,
    body: &str,
) -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  {meta_tags}
  <style>{styles}</style>
</head>
<body>
  <main>
    <div class="site-nav">
      <a class="brand" href="/">{site_name}</a>
      <div class="nav-links">
        <a href="/help">Help</a>
        <a href="/docs">Docs</a>
        <a href="/terms">Terms</a>
        <a href="/privacy">Privacy</a>
        <a href="{repo}" target="_blank" rel="noreferrer">Repository</a>
        <a href="{crate_url}" target="_blank" rel="noreferrer">Rust Crate</a>
      </div>
    </div>
    <section class="page">
      <div class="eyebrow">{eyebrow}</div>
      <h1>{page_title}</h1>
      <p class="lead">{lead}</p>
      {body}
    </section>
    {footer}
  </main>
</body>
</html>"#,
        meta_tags = render_meta_tags(page_title, description, path),
        styles = shared_page_styles(),
        site_name = SITE_NAME,
        eyebrow = eyebrow,
        page_title = page_title,
        lead = lead,
        body = body,
        footer = render_footer_html(),
        repo = REPOSITORY_URL,
        crate_url = CRATE_URL,
    )
}

fn render_index_html() -> String {
    let description = "Deterministic procedural avatars for emails, usernames, and internal ids. Generate 23 avatar families as WebP, PNG, JPEG, GIF, or SVG.";
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  {meta_tags}
  <style>
    {styles}
    .hero {{
      display: grid;
      grid-template-columns: 1.1fr 0.9fr;
    }}
    .copy, .preview {{ padding: 36px; }}
    .copy {{ border-right: 1px solid var(--line); }}
    h1 {{
      font-size: clamp(2.2rem, 6vw, 4.4rem);
      line-height: 0.95;
      margin: 12px 0 16px;
      letter-spacing: -0.05em;
      max-width: 10ch;
    }}
    p {{
      color: var(--muted);
      line-height: 1.65;
      margin: 0 0 16px;
      max-width: 60ch;
    }}
    .eyebrow {{
      text-transform: uppercase;
      color: var(--accent);
      font-weight: 700;
      font-size: 0.8rem;
      letter-spacing: 0.13em;
    }}
    .generator {{
      margin-top: 26px;
      display: grid;
      gap: 16px;
    }}
    .field-grid {{
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 14px;
    }}
    .field-grid.full {{
      grid-template-columns: 1fr;
    }}
    label {{
      display: block;
      margin-bottom: 8px;
      font-size: 0.92rem;
      font-weight: 700;
      color: var(--ink);
    }}
    input, select {{
      width: 100%;
      border: 1px solid rgba(82, 96, 109, 0.18);
      background: rgba(255,255,255,0.95);
      color: var(--ink);
      border-radius: 16px;
      padding: 14px 16px;
      font: inherit;
      outline: none;
      transition: border-color 160ms ease, box-shadow 160ms ease, transform 160ms ease;
    }}
    input:focus, select:focus {{
      border-color: rgba(217, 122, 66, 0.65);
      box-shadow: 0 0 0 5px rgba(217, 122, 66, 0.12);
      transform: translateY(-1px);
    }}
    .actions {{
      display: flex;
      flex-wrap: wrap;
      gap: 12px;
    }}
    button, .button-link {{
      border: 0;
      border-radius: 16px;
      padding: 14px 18px;
      background: linear-gradient(180deg, #dd8750, #c96831);
      color: white;
      font: inherit;
      font-weight: 700;
      cursor: pointer;
      text-decoration: none;
      display: inline-flex;
      align-items: center;
      justify-content: center;
      min-height: 52px;
      box-shadow: 0 14px 28px rgba(201, 104, 49, 0.22);
    }}
    .button-link.secondary, button.secondary {{
      background: white;
      color: var(--ink);
      border: 1px solid var(--line);
      box-shadow: none;
    }}
    .url-panel {{
      padding: 16px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 18px;
      display: grid;
      gap: 8px;
    }}
    .url-label {{
      font-size: 0.84rem;
      text-transform: uppercase;
      letter-spacing: 0.12em;
      color: var(--accent-strong);
      font-weight: 700;
    }}
    .url-text {{
      overflow-wrap: anywhere;
      font-family: "IBM Plex Mono", monospace;
      font-size: 0.94rem;
      color: var(--ink);
    }}
    .preview {{
      display: grid;
      align-content: start;
      gap: 18px;
      background:
        radial-gradient(circle at center, rgba(255,255,255,0.74), rgba(255,255,255,0) 62%),
        linear-gradient(180deg, rgba(255,255,255,0.5), rgba(255,255,255,0.15));
    }}
    .panel {{
      width: min(320px, 100%);
      aspect-ratio: 1;
      border-radius: 28px;
      background: linear-gradient(180deg, rgba(255,255,255,0.95), rgba(255,255,255,0.74));
      box-shadow: inset 0 1px 0 rgba(255,255,255,0.8), 0 18px 40px rgba(82,96,109,0.12);
      display: grid;
      place-items: center;
      padding: 12px;
    }}
    img {{
      width: 100%;
      height: auto;
      display: block;
    }}
    .preview-meta {{
      width: 100%;
      padding: 16px;
      border-radius: 18px;
      border: 1px solid var(--line);
      background: var(--surface);
      color: var(--muted);
      display: grid;
      gap: 6px;
    }}
    .examples {{
      display: grid;
      gap: 14px;
      margin-top: 24px;
    }}
    .example-grid {{
      display: grid;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      gap: 14px;
    }}
    .example-header {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
    }}
    .example-page {{
      color: var(--muted);
      font-size: 0.9rem;
      font-weight: 700;
    }}
    .example-card {{
      border: 1px solid var(--line);
      border-radius: 20px;
      background: rgba(255,255,255,0.74);
      padding: 14px;
      display: grid;
      gap: 10px;
      cursor: pointer;
      transition: transform 160ms ease, box-shadow 160ms ease;
    }}
    .example-card:hover {{
      transform: translateY(-2px);
      box-shadow: 0 16px 30px rgba(82,96,109,0.1);
    }}
    .example-card img {{
      border-radius: 16px;
      border: 1px solid var(--line);
      background: white;
    }}
    .example-title {{
      font-weight: 700;
      color: var(--ink);
    }}
    .example-controls {{
      display: flex;
      justify-content: center;
      gap: 10px;
      margin-top: 4px;
    }}
    .example-controls button {{
      min-height: 38px;
      padding: 8px 12px;
      border-radius: 999px;
      line-height: 1;
    }}
    pre {{
      margin: 0;
      padding: 14px;
      background: white;
      border: 1px solid var(--line);
      border-radius: 18px;
      overflow: auto;
      font-size: 0.94rem;
    }}
    code {{ font-family: "IBM Plex Mono", monospace; }}
    @media (max-width: 860px) {{
      .hero {{ grid-template-columns: 1fr; }}
      .copy {{ border-right: 0; border-bottom: 1px solid var(--line); }}
      .field-grid {{ grid-template-columns: 1fr; }}
      .example-grid {{ grid-template-columns: repeat(2, minmax(0, 1fr)); }}
    }}
    @media (max-width: 560px) {{
      .example-grid {{ grid-template-columns: 1fr; }}
    }}
  </style>
</head>
<body>
  <main>
    <div class="site-nav">
      <a class="brand" href="/">hashavatar.app</a>
      <div class="nav-links">
        <a href="/help">Help</a>
        <a href="/docs">Docs</a>
        <a href="/terms">Terms</a>
        <a href="/privacy">Privacy</a>
        <a href="{repo}" target="_blank" rel="noreferrer">Repository</a>
        <a href="{crate_url}" target="_blank" rel="noreferrer">Rust Crate</a>
      </div>
    </div>
    <section class="hero">
      <div class="copy">
        <div class="eyebrow">hashavatar.app</div>
        <h1>Generate A Public Avatar In Seconds</h1>
        <p>
          Turn any email, username, or stable identifier into a deterministic avatar URL.
          Choose the style, background, output format, and size, then copy the URL, download the result, or create a signed object-storage link.
        </p>
        <p>
          Privacy-conscious integration tip: avoid sending raw emails when you do not need to. Hash or namespace your internal ids client-side and use <code>tenant</code> plus <code>style_version</code> for separation.
        </p>

        <div class="generator">
          <div class="field-grid full">
            <div>
              <label for="identity">Identity</label>
              <input id="identity" type="text" value="{id}" placeholder="cat@hashavatar.app" spellcheck="false" autocomplete="off" />
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="tenant">Namespace Tenant</label>
              <input id="tenant" type="text" value="{tenant}" placeholder="public" spellcheck="false" autocomplete="off" />
            </div>
            <div>
              <label for="style-version">Style Version</label>
              <input id="style-version" type="text" value="{style_version}" placeholder="v2" spellcheck="false" autocomplete="off" />
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="kind">Avatar Type</label>
              <select id="kind">
                {kind_options}
              </select>
            </div>
            <div>
              <label for="background">Background</label>
              <select id="background">
                {background_options}
              </select>
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="format">Format</label>
              <select id="format">
                {format_options}
              </select>
            </div>
            <div>
              <label for="size">Size</label>
              <select id="size">
                <option value="128">128</option>
                <option value="256" selected>256</option>
                <option value="320">320</option>
                <option value="512">512</option>
                <option value="1024">1024</option>
              </select>
            </div>
          </div>

          <div class="actions">
            <button id="copy-button" type="button">Copy URL</button>
            <button id="copy-signed-button" type="button" class="secondary">Copy Signed Link</button>
            <a id="download-button" class="button-link" href="/v1/avatar?id={id}&kind=cat&background=themed&format=webp&size=256" download="hashavatar.webp">Download</a>
            <a id="open-button" class="button-link secondary" href="/v1/avatar?id={id}&kind=cat&background=themed&format=webp&size=256" target="_blank" rel="noreferrer">Open Raw</a>
          </div>

          <div class="url-panel">
            <div class="url-label">Direct URL</div>
            <div id="avatar-url" class="url-text"></div>
          </div>

          <div class="url-panel">
            <div class="url-label">Signed Storage Link</div>
            <div id="signed-url" class="url-text">Enable S3 configuration on the server to use signed links.</div>
          </div>

          <div class="url-panel">
            <div class="url-label">Machine-Readable API</div>
            <div class="url-text"><a class="inline-link" href="/docs/openapi.json">/docs/openapi.json</a> and <a class="inline-link" href="/metrics">/metrics</a></div>
          </div>
        </div>
      </div>

      <div class="preview">
        <div class="panel">
          <img id="avatar-preview" src="/v1/avatar?id={id}&kind=cat&background=themed&format=webp&size=256" alt="Generated avatar preview" />
        </div>
        <div class="preview-meta">
          <div><strong>API:</strong> <span id="api-mode">/v1/avatar</span></div>
          <div><strong>Storage:</strong> optional S3 persistence with presigned links via <code>/v1/avatar/link</code></div>
          <div><strong>Cache:</strong> Cloudflare-friendly long cache headers</div>
          <div><strong>Tip:</strong> Every URL is deterministic, so you can embed it directly in your app.</div>
        </div>

        <div class="examples" style="width:100%;">
          <div class="example-header">
            <div class="url-label">Preset Examples</div>
            <div id="example-page" class="example-page"></div>
          </div>
          <div class="example-grid" id="example-grid">
          </div>
          <div class="example-controls">
            <button id="preset-prev" type="button" class="secondary" aria-label="Previous preset page">&larr;</button>
            <button id="preset-next" type="button" class="secondary" aria-label="Next preset page">&rarr;</button>
          </div>
        </div>
      </div>
    </section>
    {footer}
  </main>
  <script>
    const identityEl = document.getElementById("identity");
    const tenantEl = document.getElementById("tenant");
    const styleVersionEl = document.getElementById("style-version");
    const kindEl = document.getElementById("kind");
    const backgroundEl = document.getElementById("background");
    const formatEl = document.getElementById("format");
    const sizeEl = document.getElementById("size");
    const previewEl = document.getElementById("avatar-preview");
    const urlEl = document.getElementById("avatar-url");
    const signedUrlEl = document.getElementById("signed-url");
    const copyButton = document.getElementById("copy-button");
    const copySignedButton = document.getElementById("copy-signed-button");
    const downloadButton = document.getElementById("download-button");
    const openButton = document.getElementById("open-button");
    const exampleGrid = document.getElementById("example-grid");
    const examplePage = document.getElementById("example-page");
    const presetPrev = document.getElementById("preset-prev");
    const presetNext = document.getElementById("preset-next");
    const presetExamples = {preset_examples};
    const presetPageSize = {preset_page_size};
    const presetIdentities = new Map(
      Array.from(kindEl.options).map((option) => [option.value, option.dataset.identity])
    );
    let presetPage = 0;

    function currentIdentity() {{
      return identityEl.value.trim() || "{id}";
    }}

    function selectedPresetIdentity() {{
      return presetIdentities.get(kindEl.value) || "{id}";
    }}

    function isPresetIdentity(value) {{
      for (const identity of presetIdentities.values()) {{
        if (value === identity) {{
          return true;
        }}
      }}
      return false;
    }}

    function currentUrl() {{
      const query = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        kind: kindEl.value,
        background: backgroundEl.value,
        format: formatEl.value,
        size: sizeEl.value,
      }});
      return `/v1/avatar?${{query.toString()}}`;
    }}

    function currentSignedUrlEndpoint() {{
      const query = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        kind: kindEl.value,
        background: backgroundEl.value,
        format: formatEl.value,
        size: sizeEl.value,
      }});
      return `/v1/avatar/link?${{query.toString()}}`;
    }}

    async function updateSignedUrl() {{
      try {{
        const response = await fetch(currentSignedUrlEndpoint(), {{ headers: {{ "accept": "application/json" }} }});
        if (!response.ok) {{
          signedUrlEl.textContent = "Signed storage links are unavailable until S3 is configured on the server.";
          return;
        }}
        const payload = await response.json();
        signedUrlEl.textContent = payload.signed_url;
      }} catch (_) {{
        signedUrlEl.textContent = "Signed storage links are unavailable until S3 is configured on the server.";
      }}
    }}

    function refresh() {{
      const url = currentUrl();
      const previewQuery = new URLSearchParams({{
        id: currentIdentity(),
        kind: kindEl.value,
        background: backgroundEl.value,
        format: formatEl.value === "svg" ? "svg" : "webp",
        size: sizeEl.value,
        ts: String(Date.now()),
      }});

      previewEl.src = `/v1/avatar?${{previewQuery.toString()}}`;
      urlEl.textContent = `${{window.location.origin}}${{url}}`;
      downloadButton.href = url;
      const extension = formatEl.value === "jpg" ? "jpg" : formatEl.value;
      downloadButton.setAttribute("download", `hashavatar-${{kindEl.value}}.${{extension}}`);
      openButton.href = url;
      updateSignedUrl();
    }}

    function setFromPreset(preset) {{
      identityEl.value = preset.id;
      tenantEl.value = "{tenant}";
      styleVersionEl.value = "{style_version}";
      kindEl.value = preset.kind;
      backgroundEl.value = backgroundEl.value || preset.background;
      formatEl.value = preset.format;
      sizeEl.value = preset.size;
      refresh();
    }}

    function renderPresetPage() {{
      const pageCount = Math.ceil(presetExamples.length / presetPageSize);
      presetPage = (presetPage + pageCount) % pageCount;
      const start = presetPage * presetPageSize;
      const pageItems = presetExamples.slice(start, start + presetPageSize);
      const exampleBackground = backgroundEl.value || "themed";
      exampleGrid.replaceChildren();
      for (const preset of pageItems) {{
        const button = document.createElement("button");
        button.type = "button";
        button.className = "example-card";
        button.addEventListener("click", () => setFromPreset(preset));

        const query = new URLSearchParams({{
          id: preset.id,
          kind: preset.kind,
          background: exampleBackground,
          format: "webp",
          size: "160",
        }});
        const image = document.createElement("img");
        image.src = `/v1/avatar?${{query.toString()}}`;
        image.alt = `${{preset.label}} preset`;

        const title = document.createElement("div");
        title.className = "example-title";
        title.textContent = `${{preset.label}} preset`;

        button.append(image, title);
        exampleGrid.append(button);
      }}
      examplePage.textContent = `${{presetPage + 1}} / ${{pageCount}}`;
      presetPrev.disabled = pageCount <= 1;
      presetNext.disabled = pageCount <= 1;
    }}

    async function copyText(text, button, idleText, successText) {{
      try {{
        await navigator.clipboard.writeText(text);
        button.textContent = successText;
      }} catch (_) {{
        button.textContent = "Copy failed";
      }}
      window.setTimeout(() => button.textContent = idleText, 1200);
    }}

    copyButton.addEventListener("click", () => copyText(`${{window.location.origin}}${{currentUrl()}}`, copyButton, "Copy URL", "Copied"));
    copySignedButton.addEventListener("click", () => copyText(signedUrlEl.textContent, copySignedButton, "Copy Signed Link", "Copied"));

    [identityEl, tenantEl, styleVersionEl, kindEl, backgroundEl, formatEl, sizeEl].forEach((el) => {{
      el.addEventListener("input", refresh);
      el.addEventListener("change", refresh);
    }});

    backgroundEl.addEventListener("change", renderPresetPage);

    kindEl.addEventListener("change", () => {{
      const current = identityEl.value.trim();
      if (current === "" || isPresetIdentity(current)) {{
        identityEl.value = selectedPresetIdentity();
      }}
      refresh();
    }});

    presetPrev.addEventListener("click", () => {{
      presetPage -= 1;
      renderPresetPage();
    }});

    presetNext.addEventListener("click", () => {{
      presetPage += 1;
      renderPresetPage();
    }});

    renderPresetPage();
    refresh();
  </script>
</body>
</html>"#,
        id = DEFAULT_ID,
        tenant = DEFAULT_NAMESPACE_TENANT,
        style_version = DEFAULT_NAMESPACE_STYLE,
        kind_options = kind_options_html(AvatarKind::Cat),
        background_options = background_options_html(AvatarBackground::Themed),
        format_options = format_options_html(AvatarRequestFormat::Webp),
        preset_examples = preset_examples_json(),
        preset_page_size = PRESET_PAGE_SIZE,
        meta_tags = render_meta_tags("Public Avatar API", description, "/"),
        styles = shared_page_styles(),
        footer = render_footer_html(),
        repo = REPOSITORY_URL,
        crate_url = CRATE_URL,
    )
}

fn render_help_html() -> String {
    render_page_html(
        "Help",
        "Integration guide for using the hashavatar.app avatar API in web apps, frontends, and backends.",
        "/help",
        "Integration Guide",
        "Use hashavatar.app directly from the browser, your frontend, or your backend. Every avatar URL is deterministic, so the same identifier and options always produce the same result.",
        &format!(
            r#"
<div class="content-grid">
  <section class="card">
    <h2>Basic URL</h2>
    <p>Use the query endpoint when you want a simple public image URL.</p>
    <pre><code>https://{site}/v1/avatar?id=robot@hashavatar.app&amp;kind=robot&amp;background=white&amp;format=webp&amp;size=256</code></pre>
  </section>
  <section class="card">
    <h2>Path Style URL</h2>
    <p>Use the path form if you prefer cleaner embed URLs.</p>
    <pre><code>https://{site}/avatar/fox/fox@hashavatar.app/svg</code></pre>
  </section>
  <section class="card">
    <h2>HTML Example</h2>
    <pre><code>&lt;img
  src="https://{site}/v1/avatar?id=monster@hashavatar.app&amp;kind=monster&amp;background=themed&amp;format=webp&amp;size=256"
  alt="Generated monster avatar"
/&gt;</code></pre>
  </section>
  <section class="card">
    <h2>JavaScript Example</h2>
    <pre><code>const avatarUrl = new URL("https://{site}/v1/avatar");
avatarUrl.search = new URLSearchParams({{
  id: user.email,
  kind: "robot",
  background: "white",
  format: "webp",
  size: "256",
}}).toString();</code></pre>
  </section>
</div>
<section class="card">
  <h2>Supported Parameters</h2>
  <ul>
    <li><code>id</code>: any stable identifier such as an email, username, or internal user id</li>
    <li><code>tenant</code>: optional namespace partition for multi-tenant apps</li>
    <li><code>style_version</code>: optional style namespace such as <code>v2</code></li>
    <li><code>kind</code>: any public hashavatar family, including <code>cat</code>, <code>dog</code>, <code>robot</code>, <code>planet</code>, <code>rocket</code>, <code>frog</code>, <code>panda</code>, <code>cupcake</code>, <code>pizza</code>, <code>octopus</code>, and <code>knight</code></li>
    <li><code>background</code>: <code>themed</code>, <code>white</code>, <code>black</code>, <code>dark</code>, <code>light</code>, or <code>transparent</code></li>
    <li><code>format</code>: <code>webp</code>, <code>png</code>, <code>jpg</code>, <code>gif</code>, or <code>svg</code></li>
    <li><code>size</code>: from <code>64</code> up to <code>1024</code></li>
  </ul>
</section>
<section class="card">
  <h2>Signed Storage Links</h2>
  <p>If this deployment has object storage configured, request a presigned storage link from <code>/v1/avatar/link</code>. That endpoint stores the generated object and returns JSON with the signed URL and object key.</p>
  <pre><code>GET https://{site}/v1/avatar/link?id=robot@hashavatar.app&amp;kind=robot&amp;background=white&amp;format=webp&amp;size=256</code></pre>
</section>
<section class="card">
  <h2>Open Source</h2>
  <p>The public site source lives in <a class="inline-link" href="{repo}" target="_blank" rel="noreferrer">the repository</a> and the reusable rendering crate is published on <a class="inline-link" href="{crate_url}" target="_blank" rel="noreferrer">crates.io</a>.</p>
</section>
"#,
            site = SITE_NAME,
            repo = REPOSITORY_URL,
            crate_url = CRATE_URL,
        ),
    )
}

fn render_docs_html() -> String {
    render_page_html(
        "Docs",
        "Reference documentation for the hashavatar.app public avatar API, storage link endpoint, metrics, and namespace-aware identity contract.",
        "/docs",
        "API Reference",
        "This is the product-facing reference for the public API. The same identity, tenant, style version, kind, background, size, and format are intended to remain stable within a major release.",
        &format!(
            r#"
<section class="card">
  <h2>Core Endpoints</h2>
  <ul>
    <li><code>GET /v1/avatar</code>: returns an avatar asset directly</li>
    <li><code>GET /v1/avatar/link</code>: stores the generated avatar in configured object storage and returns signed-link metadata</li>
    <li><code>GET /avatar/&lt;kind&gt;/&lt;identity&gt;/&lt;format&gt;</code>: path-style public avatar URL</li>
    <li><code>GET /docs/openapi.json</code>: machine-readable API description</li>
    <li><code>GET /metrics</code>: basic runtime counters</li>
  </ul>
</section>
<div class="content-grid">
  <section class="card">
    <h2>Namespace Support</h2>
    <p>Use <code>tenant</code> and <code>style_version</code> to keep visual identity spaces separate between products or rollout phases.</p>
    <pre><code>GET https://{site}/v1/avatar?id=wizard@hashavatar.app&amp;tenant=acme&amp;style_version=v2&amp;kind=wizard&amp;background=white&amp;format=webp&amp;size=256</code></pre>
  </section>
  <section class="card">
    <h2>Anonymous IDs</h2>
    <p>Prefer sending an internal stable id or a one-way application hash instead of a raw email when privacy matters.</p>
    <pre><code>id = sha256(lowercase(email))</code></pre>
  </section>
  <section class="card">
    <h2>Rate Limits</h2>
    <p>The public service applies origin-side rate limits, with stricter limits on <code>/v1/avatar/link</code> because object storage writes are more expensive than direct rendering.</p>
  </section>
  <section class="card">
    <h2>Timeouts</h2>
    <p>Avatar generation and storage operations are bounded by server-side timeouts so expensive requests cannot monopolize the origin indefinitely.</p>
  </section>
</div>
<section class="card">
  <h2>Errors</h2>
  <ul>
    <li><code>400</code>: invalid kind, format, size, or missing identity</li>
    <li><code>408</code>: generation or storage timeout</li>
    <li><code>429</code>: rate limit exceeded</li>
    <li><code>500</code>: rendering or storage failure</li>
  </ul>
</section>
<section class="card">
  <h2>OpenAPI</h2>
  <p>For generated clients or tooling, use <a class="inline-link" href="/docs/openapi.json">/docs/openapi.json</a>.</p>
</section>
"#,
            site = SITE_NAME,
        ),
    )
}

fn render_terms_html() -> String {
    render_page_html(
        "Terms",
        "Best-effort service terms for the public hashavatar.app avatar API and demo website.",
        "/terms",
        "Service Terms",
        "This public service is provided on an informational and best-effort basis. Use it only if that risk profile works for your application.",
        r#"
<section class="card">
  <h2>No Warranty</h2>
  <p>This service and all generated outputs are provided as-is and as-available, without warranties of any kind, whether express or implied. We do not promise availability, correctness, fitness for a particular purpose, uninterrupted operation, or compatibility with your systems.</p>
</section>
<section class="card">
  <h2>No Liability</h2>
  <p>We are not responsible for downtime, outages, degraded performance, broken links, cache behavior, lost data, corrupted objects, third-party provider failures, or any direct or indirect damages arising from your use of the service.</p>
  <p>If you depend on these avatars in production, you should implement your own fallback behavior, caching strategy, and availability plan.</p>
</section>
<section class="card">
  <h2>Acceptable Use</h2>
  <p>Do not use the service to overload the infrastructure, bypass rate limits or cache controls, test abusive traffic patterns, or store illegal material through any persistence feature.</p>
</section>
<section class="card">
  <h2>Changes</h2>
  <p>We may change, limit, suspend, or discontinue the public service at any time and without notice. Public endpoints, output details, or operational limits may change as the service evolves.</p>
  <p>This page is operational guidance, not legal advice. If you need formal legal terms for a business deployment, you should publish a reviewed version specific to your jurisdiction and operator entity.</p>
</section>
"#,
    )
}

fn render_privacy_html() -> String {
    render_page_html(
        "Privacy",
        "Privacy notice for hashavatar.app covering request data, logs, and optional object storage behavior.",
        "/privacy",
        "Privacy Notice",
        "The service is intentionally simple, but a public avatar API still receives some request data in order to function. This page describes that practical baseline.",
        r#"
<section class="card">
  <h2>What The Service Receives</h2>
  <ul>
    <li>the identifier you put in the request, such as an email address or username</li>
    <li>request parameters such as avatar type, size, format, and background</li>
    <li>standard HTTP metadata handled by the server, reverse proxy, and CDN, such as IP address, user agent, referrer, and request timing</li>
  </ul>
</section>
<section class="card">
  <h2>What The App Itself Stores</h2>
  <p>The application does not require user accounts and does not set application cookies by default. In the basic request flow it generates the avatar on demand and returns it directly.</p>
  <p>If object storage support is enabled and a signed-link or persistence route is used, the generated avatar file and its object key may be stored in the configured S3-compatible bucket.</p>
</section>
<section class="card">
  <h2>Logging And Infrastructure</h2>
  <p>Depending on deployment, infrastructure components such as nginx, Caddy, Cloudflare, hosting providers, or S3-compatible storage may keep access logs and operational metadata. Those logs are part of running a public service and may contain the identifier you requested if it appears in the URL.</p>
</section>
<section class="card">
  <h2>What To Avoid Sending</h2>
  <p>If you do not want personal data to appear in logs or URLs, do not send raw personal data as the <code>id</code> value. A common pattern is to send an internal stable id or a one-way application hash instead of a plain email address.</p>
</section>
<section class="card">
  <h2>Repository And Crate</h2>
  <p>You can inspect the implementation in the public <a class="inline-link" href="https://github.com/valkyoth/hashavatar-api" target="_blank" rel="noreferrer">API repository</a> and the reusable avatar renderer in the <a class="inline-link" href="https://crates.io/crates/hashavatar/" target="_blank" rel="noreferrer">Rust crate</a>.</p>
</section>
"#,
    )
}

#[derive(Debug, Deserialize)]
struct OgQuery {
    id: Option<String>,
    tenant: Option<String>,
    style_version: Option<String>,
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AvatarQuery {
    id: Option<String>,
    kind: Option<String>,
    background: Option<String>,
    format: Option<String>,
    size: Option<u32>,
    tenant: Option<String>,
    style_version: Option<String>,
    persist: Option<bool>,
    redirect: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct PathAvatar {
    kind: String,
    identity: String,
    format: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AvatarRequestFormat {
    Webp,
    Png,
    Jpeg,
    Gif,
    Svg,
}

impl AvatarRequestFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Svg => "svg",
        }
    }
}

impl std::fmt::Display for AvatarRequestFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AvatarRequestFormat {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "webp" => Ok(Self::Webp),
            "png" => Ok(Self::Png),
            "jpg" | "jpeg" => Ok(Self::Jpeg),
            "gif" => Ok(Self::Gif),
            "svg" => Ok(Self::Svg),
            _ => Err("unsupported avatar format"),
        }
    }
}

#[derive(Debug)]
struct AvatarRequest {
    identity: String,
    namespace_tenant: String,
    namespace_style: String,
    kind: AvatarKind,
    background: AvatarBackground,
    format: AvatarRequestFormat,
    size: u32,
    persist: bool,
    redirect: bool,
}

impl AvatarRequest {
    fn from_query(query: AvatarQuery) -> Result<Self, String> {
        Ok(Self {
            identity: query.id.unwrap_or_else(|| DEFAULT_ID.to_string()),
            namespace_tenant: query
                .tenant
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_NAMESPACE_TENANT.to_string()),
            namespace_style: query
                .style_version
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_NAMESPACE_STYLE.to_string()),
            kind: query
                .kind
                .as_deref()
                .and_then(|raw| AvatarKind::from_str(raw).ok())
                .unwrap_or(AvatarKind::Cat),
            background: query
                .background
                .as_deref()
                .and_then(|raw| AvatarBackground::from_str(raw).ok())
                .unwrap_or(AvatarBackground::Themed),
            format: query
                .format
                .as_deref()
                .and_then(|raw| AvatarRequestFormat::from_str(raw).ok())
                .unwrap_or(AvatarRequestFormat::Webp),
            size: query.size.unwrap_or(256),
            persist: query.persist.unwrap_or(false),
            redirect: query.redirect.unwrap_or(false),
        })
    }
}

struct AvatarAsset {
    body: Vec<u8>,
    content_type: &'static str,
    cache_key: String,
    object_key: String,
}

#[derive(Serialize)]
struct AvatarLinkResponse {
    object_key: String,
    signed_url: String,
    expires_in_seconds: u64,
    content_type: String,
    cache_key: String,
}

struct SignedStorageObject {
    object_key: String,
    signed_url: String,
}

struct S3Storage {
    client: S3Client,
    bucket: String,
    prefix: String,
    presign_ttl: Duration,
}

impl S3Storage {
    async fn from_env() -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let bucket = match std::env::var("HASHAVATAR_S3_BUCKET") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(None),
        };

        let region =
            std::env::var("HASHAVATAR_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        let endpoint = std::env::var("HASHAVATAR_S3_ENDPOINT").ok();
        let force_path_style = std::env::var("HASHAVATAR_S3_PATH_STYLE")
            .ok()
            .map(|raw| matches!(raw.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);
        let prefix =
            std::env::var("HASHAVATAR_S3_PREFIX").unwrap_or_else(|_| "avatars".to_string());
        let ttl = std::env::var("HASHAVATAR_S3_PRESIGN_TTL_SECONDS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(900);

        let shared_config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region))
            .load()
            .await;

        let mut config_builder = S3ConfigBuilder::from(&shared_config);
        if let Some(endpoint) = endpoint {
            config_builder = config_builder.endpoint_url(endpoint);
        }
        if force_path_style {
            config_builder = config_builder.force_path_style(true);
        }

        Ok(Some(Self {
            client: S3Client::from_conf(config_builder.build()),
            bucket,
            prefix,
            presign_ttl: Duration::from_secs(ttl),
        }))
    }

    async fn store_and_sign(
        &self,
        asset: &AvatarAsset,
        metrics: &Metrics,
    ) -> Result<SignedStorageObject, Box<dyn std::error::Error>> {
        let key = format!("{}/{}", self.prefix.trim_matches('/'), asset.object_key);
        let exists = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .is_ok();

        if exists {
            metrics.storage_hit_total.fetch_add(1, Ordering::Relaxed);
        } else {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(ByteStream::from(asset.body.clone()))
                .content_type(asset.content_type)
                .cache_control("public, max-age=31536000, immutable")
                .send()
                .await?;
            metrics.storage_write_total.fetch_add(1, Ordering::Relaxed);
        }

        let presigned = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .presigned(PresigningConfig::expires_in(self.presign_ttl)?)
            .await?;

        Ok(SignedStorageObject {
            object_key: key,
            signed_url: presigned.uri().to_string(),
        })
    }
}

struct HeaderName;

impl HeaderName {
    fn cdn_cache_control() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("cdn-cache-control")
    }

    fn cloudflare_cache_control() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("cloudflare-cdn-cache-control")
    }

    fn storage_key() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("x-hashavatar-object-key")
    }

    fn signed_url() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("x-hashavatar-signed-url")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_avatar_request(format: AvatarRequestFormat) -> AvatarRequest {
        AvatarRequest {
            identity: DEFAULT_ID.to_string(),
            namespace_tenant: DEFAULT_NAMESPACE_TENANT.to_string(),
            namespace_style: DEFAULT_NAMESPACE_STYLE.to_string(),
            kind: AvatarKind::Cat,
            background: AvatarBackground::Themed,
            format,
            size: 256,
            persist: false,
            redirect: false,
        }
    }

    #[test]
    fn rate_limiter_enforces_per_key_limit() {
        let limiter = RateLimiter::with_capacity(8);
        let key = "avatar:127.0.0.1:public:cat".to_string();

        assert!(limiter.check(key.clone(), 2));
        assert!(limiter.check(key.clone(), 2));
        assert!(!limiter.check(key, 2));
    }

    #[test]
    fn rate_limiter_evicts_oldest_bucket_at_capacity() {
        let limiter = RateLimiter::with_capacity(2);

        assert!(limiter.check("first".to_string(), 1));
        assert!(limiter.check("second".to_string(), 1));
        assert_eq!(limiter.len(), 2);

        assert!(limiter.check("third".to_string(), 1));
        assert_eq!(limiter.len(), 2);

        assert!(limiter.check("first".to_string(), 1));
        assert_eq!(limiter.len(), 2);
    }

    #[test]
    fn rate_limiter_bounds_unique_attacker_keys() {
        let limiter = RateLimiter::with_capacity(32);

        for idx in 0..1_000 {
            assert!(limiter.check(format!("avatar:spoofed-{idx}:tenant-{idx}:cat"), 1));
        }

        assert_eq!(limiter.len(), 32);
    }

    #[test]
    fn client_ip_ignores_forwarded_headers_from_untrusted_peers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.99"));

        let peer_ip = IpAddr::from([198, 51, 100, 10]);
        let trusted_proxies = TrustedProxies::default();

        assert_eq!(
            client_ip(&headers, peer_ip, &trusted_proxies),
            "198.51.100.10"
        );
    }

    #[test]
    fn client_ip_honors_forwarded_headers_from_trusted_proxies() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.99, 10.89.42.10"),
        );

        let peer_ip = IpAddr::from([10, 89, 42, 10]);
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");

        assert_eq!(
            client_ip(&headers, peer_ip, &trusted_proxies),
            "203.0.113.99"
        );
    }

    #[test]
    fn client_ip_falls_back_to_peer_when_trusted_header_is_invalid() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("not an ip"));

        let peer_ip = IpAddr::from([10, 89, 42, 10]);
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");

        assert_eq!(
            client_ip(&headers, peer_ip, &trusted_proxies),
            "10.89.42.10"
        );
    }

    #[tokio::test]
    async fn internal_error_does_not_expose_details() {
        let response = internal_error("s3 bucket hashavatar-private in eu-north-1 denied");

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("internal error body");
        let body = std::str::from_utf8(&body).expect("utf8 body");

        assert_eq!(body, INTERNAL_ERROR_MESSAGE);
        assert!(!body.contains("hashavatar-private"));
        assert!(!body.contains("eu-north-1"));
    }

    #[test]
    fn build_avatar_asset_renders_svg_with_hashavatar_0_6() {
        let request = test_avatar_request(AvatarRequestFormat::Svg);
        let asset = build_avatar_asset(&request).expect("svg avatar should render");
        let body = std::str::from_utf8(&asset.body).expect("svg should be utf8");

        assert_eq!(asset.content_type, "image/svg+xml");
        assert!(body.starts_with("<svg "));
    }

    #[test]
    fn build_avatar_asset_rejects_oversized_namespace() {
        let mut request = test_avatar_request(AvatarRequestFormat::Svg);
        request.namespace_tenant = "x".repeat(129);

        let error = match build_avatar_asset(&request) {
            Ok(_) => panic!("oversized tenant should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("namespace tenant must be at most 128 bytes"));
    }
}
