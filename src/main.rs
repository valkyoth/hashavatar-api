use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use axum::extract::{ConnectInfo, Extension, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use hashavatar::{
    AVATAR_STYLE_VERSION, AvatarAccessory, AvatarBackground, AvatarColor, AvatarExpression,
    AvatarIdentityOptions, AvatarKind, AvatarNamespace, AvatarOptions, AvatarOutputFormat,
    AvatarShape, AvatarSpec, AvatarStyleOptions, encode_avatar_style_with_identity_options,
    render_avatar_for_namespace,
};
use image::{GenericImage, ImageBuffer, Rgba, RgbaImage};
use ipnet::IpNet;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const TRUSTED_PROXIES_ENV: &str = "HASHAVATAR_TRUSTED_PROXIES";
const DEFAULT_ID: &str = "cat@hashavatar.app";
const SITE_NAME: &str = "hashavatar.app";
const SITE_URL: &str = "https://hashavatar.app";
const REPOSITORY_URL: &str = "https://github.com/valkyoth/hashavatar-api";
const CRATE_URL: &str = "https://crates.io/crates/hashavatar/";
const DEFAULT_NAMESPACE_TENANT: &str = "public";
const DEFAULT_NAMESPACE_STYLE: &str = "v2";
const DEFAULT_HASH_ALGORITHM: &str = "sha512";
const DEFAULT_ACCESSORY: AvatarAccessory = AvatarAccessory::None;
const DEFAULT_COLOR: AvatarColor = AvatarColor::Default;
const DEFAULT_EXPRESSION: AvatarExpression = AvatarExpression::Default;
const DEFAULT_SHAPE: AvatarShape = AvatarShape::Square;
const AVATAR_TIMEOUT_MS: u64 = 3_000;
const STORAGE_TIMEOUT_MS: u64 = 5_000;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
const MAX_RATE_LIMIT_BUCKETS: usize = 65_536;
const INTERNAL_ERROR_MESSAGE: &str = "An internal server error occurred.";
const MIN_SIZE: u32 = 64;
const MAX_SIZE: u32 = 1024;
const MAX_ID_BYTES: usize = 512;
const MAX_NAMESPACE_COMPONENT_BYTES: usize = 64;
const PRESET_PAGE_SIZE: usize = 12;
const INVALID_NAMESPACE_MESSAGE: &str = "invalid namespace: tenant and style_version must be 1-64 ASCII letters, digits, hyphens, or underscores";
const INVALID_HASH_ALGORITHM_MESSAGE: &str = "unsupported hash algorithm: expected sha512";
const INVALID_AVATAR_FORMAT_MESSAGE: &str = "unsupported avatar format: expected webp";
const INVALID_AVATAR_RENDER_MESSAGE: &str = "avatar generation failed";
const INDEX_SCRIPT_SHA256: &str = "'sha256-7gjoUnTfcILxVkX3DugGXgaAEhWr+Pn91S0M+2HGQTs='";
const INDEX_SCRIPT_SHA256_COMPAT: &str = "'sha256-ZswfTY7H35rbv8WC7NXBoiC7WNu86vSzCDChNWwZZDM='";

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
        .route(
            "/metrics",
            get(metrics_json).route_layer(middleware::from_fn(require_loopback_peer)),
        )
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

#[derive(Clone)]
struct CspNonce(String);

impl CspNonce {
    fn as_str(&self) -> &str {
        &self.0
    }
}

fn generate_csp_nonce() -> Result<CspNonce, getrandom::Error> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes)?;

    let mut nonce = String::with_capacity(32);
    for byte in bytes {
        nonce.push_str(&format!("{byte:02x}"));
    }
    Ok(CspNonce(nonce))
}

fn content_security_policy(nonce: &CspNonce) -> String {
    format!(
        "default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; img-src 'self' data:; style-src 'self' 'nonce-{nonce}'; script-src 'self' 'nonce-{nonce}' {script_hash} {script_hash_compat}; connect-src 'self'; form-action 'self'",
        nonce = nonce.as_str(),
        script_hash = INDEX_SCRIPT_SHA256,
        script_hash_compat = INDEX_SCRIPT_SHA256_COMPAT,
    )
}

fn static_content_security_policy() -> &'static str {
    "default-src 'self'; base-uri 'self'; object-src 'none'; frame-ancestors 'none'; img-src 'self' data:; style-src 'self'; script-src 'self'; connect-src 'self'; form-action 'self'"
}

async fn add_security_headers(mut request: Request, next: Next) -> Response {
    let csp_nonce = if route_uses_inline_html(request.uri().path()) {
        let nonce = match generate_csp_nonce() {
            Ok(nonce) => nonce,
            Err(error) => return secure_rng_failure(error),
        };
        request.extensions_mut().insert(nonce.clone());
        Some(nonce)
    } else {
        None
    };

    let mut response = next.run(request).await;
    let is_html_response = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.starts_with("text/html"));
    let csp = csp_nonce
        .as_ref()
        .map(content_security_policy)
        .unwrap_or_else(|| static_content_security_policy().to_string());
    apply_security_headers(response.headers_mut(), &csp, is_html_response);

    response
}

fn route_uses_inline_html(path: &str) -> bool {
    matches!(path, "/" | "/help" | "/docs" | "/terms" | "/privacy")
}

fn apply_security_headers(headers: &mut HeaderMap, csp: &str, is_html_response: bool) {
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_str(csp)
            .unwrap_or_else(|_| HeaderValue::from_static(static_content_security_policy())),
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
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    if is_html_response {
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store, max-age=0"),
        );
        headers.insert(
            header::HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        );
    }
    headers.insert(
        header::HeaderName::from_static("strict-transport-security"),
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
}

async fn require_loopback_peer(
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if !is_loopback_peer(peer_addr) {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

fn is_loopback_peer(peer_addr: SocketAddr) -> bool {
    normalize_ip(peer_addr.ip()).is_loopback()
}

fn secure_rng_failure(error: getrandom::Error) -> Response {
    tracing::error!(%error, "secure RNG failure; refusing to generate CSP nonce");
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, INTERNAL_ERROR_MESSAGE).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'none'; base-uri 'none'; frame-ancestors 'none'"),
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
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("cross-origin"),
    );
    response
}

async fn index(
    State(state): State<AppState>,
    Extension(csp_nonce): Extension<CspNonce>,
) -> Html<String> {
    Html(render_index_html(&csp_nonce, state.storage.is_some()))
}

async fn help_page(Extension(csp_nonce): Extension<CspNonce>) -> Html<String> {
    Html(render_help_html(&csp_nonce))
}

async fn docs_page(Extension(csp_nonce): Extension<CspNonce>) -> Html<String> {
    Html(render_docs_html(&csp_nonce))
}

async fn terms_page(Extension(csp_nonce): Extension<CspNonce>) -> Html<String> {
    Html(render_terms_html(&csp_nonce))
}

async fn privacy_page(Extension(csp_nonce): Extension<CspNonce>) -> Html<String> {
    Html(render_privacy_html(&csp_nonce))
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
    Json(openapi_document())
}

fn openapi_document() -> serde_json::Value {
    serde_json::json!({
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
                        {"name":"algorithm","in":"query","schema":{"type":"string","enum": ["sha512"]}},
                        {"name":"kind","in":"query","schema":{"type":"string","enum": AvatarKind::ALL.iter().map(|kind| kind.as_str()).collect::<Vec<_>>()}},
                        {"name":"background","in":"query","schema":{"type":"string","enum": AvatarBackground::ALL.iter().map(|background| background.as_str()).collect::<Vec<_>>()}},
                        {"name":"accessory","in":"query","schema":{"type":"string","enum": AvatarAccessory::ALL.iter().map(|accessory| accessory.as_str()).collect::<Vec<_>>()}},
                        {"name":"color","in":"query","schema":{"type":"string","enum": AvatarColor::ALL.iter().map(|color| color.as_str()).collect::<Vec<_>>()}},
                        {"name":"expression","in":"query","schema":{"type":"string","enum": AvatarExpression::ALL.iter().map(|expression| expression.as_str()).collect::<Vec<_>>()}},
                        {"name":"shape","in":"query","schema":{"type":"string","enum": AvatarShape::ALL.iter().map(|shape| shape.as_str()).collect::<Vec<_>>()}},
                        {"name":"format","in":"query","schema":{"type":"string","enum":["webp"]}},
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
        }
    })
}

async fn og_png(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<OgQuery>,
) -> Response {
    if let Err(response) =
        enforce_limits(&state, &headers, peer_addr.ip(), RateLimitRoute::OgImage).await
    {
        return response;
    }

    let title_id = query
        .id
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| DEFAULT_ID.to_string());
    if let Err(message) = validate_identity(&title_id) {
        return bad_request(&message);
    }

    let tenant = query
        .tenant
        .as_deref()
        .unwrap_or(DEFAULT_NAMESPACE_TENANT)
        .to_string();
    let style_version = query
        .style_version
        .as_deref()
        .unwrap_or(DEFAULT_NAMESPACE_STYLE)
        .to_string();
    if validate_namespace_component("tenant", &tenant).is_err()
        || validate_namespace_component("style_version", &style_version).is_err()
    {
        return bad_request(INVALID_NAMESPACE_MESSAGE);
    }

    let lead_kind = query
        .kind
        .as_deref()
        .and_then(|raw| AvatarKind::from_str(raw).ok())
        .unwrap_or(AvatarKind::Monster);

    let render = tokio::task::spawn_blocking(move || {
        build_og_png_bytes(&title_id, &tenant, &style_version, lead_kind)
    });
    let bytes =
        match tokio::time::timeout(Duration::from_millis(AVATAR_TIMEOUT_MS * 3), render).await {
            Ok(Ok(Ok(bytes))) => bytes,
            Ok(Ok(Err(error))) => return error.into_response(),
            Ok(Err(error)) => return internal_error(error),
            Err(_) => return request_timeout("Open Graph image generation timed out"),
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

enum OgPngError {
    BadRequest(&'static str),
    Internal(String),
}

impl OgPngError {
    fn into_response(self) -> Response {
        match self {
            Self::BadRequest(message) => bad_request(message),
            Self::Internal(message) => internal_error(message),
        }
    }
}

fn build_og_png_bytes(
    title_id: &str,
    tenant: &str,
    style_version: &str,
    lead_kind: AvatarKind,
) -> Result<Vec<u8>, OgPngError> {
    let namespace = AvatarNamespace::new(tenant, style_version)
        .map_err(|_| OgPngError::BadRequest(INVALID_NAMESPACE_MESSAGE))?;
    let spec = AvatarSpec::new(220, 220, 0)
        .map_err(|_| OgPngError::BadRequest(INVALID_AVATAR_RENDER_MESSAGE))?;

    let mut canvas: RgbaImage = ImageBuffer::from_pixel(1200, 630, Rgba([251, 246, 238, 255]));
    draw_rect(&mut canvas, 0, 0, 1200, 630, Rgba([242, 236, 228, 255]));
    draw_circle(&mut canvas, 160, 140, 180, Rgba([255, 214, 170, 180]));
    draw_circle(&mut canvas, 1030, 500, 150, Rgba([217, 122, 66, 70]));

    for (idx, kind) in [lead_kind, AvatarKind::Robot, AvatarKind::Ghost]
        .into_iter()
        .enumerate()
    {
        let avatar = render_avatar_for_namespace(
            spec,
            namespace,
            title_id,
            AvatarOptions::new(
                kind,
                if idx == 1 {
                    AvatarBackground::White
                } else {
                    AvatarBackground::Themed
                },
            ),
        )
        .map_err(|_| OgPngError::BadRequest(INVALID_AVATAR_RENDER_MESSAGE))?;
        overlay(&mut canvas, &avatar, 110 + idx as u32 * 260, 180)
            .map_err(|error| OgPngError::Internal(error.to_string()))?;
    }

    use image::ImageEncoder;
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buf)
        .write_image(
            canvas.as_raw(),
            canvas.width(),
            canvas.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| OgPngError::Internal(error.to_string()))?;
    Ok(buf)
}

async fn healthz() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
        })),
    )
}

#[derive(Clone, Copy)]
enum RateLimitRoute {
    Avatar,
    StorageLink,
    OgImage,
}

impl RateLimitRoute {
    fn as_str(self) -> &'static str {
        match self {
            Self::Avatar => "avatar",
            Self::StorageLink => "storage-link",
            Self::OgImage => "og-image",
        }
    }

    fn limit(self) -> u32 {
        match self {
            Self::Avatar => 240,
            Self::StorageLink => 30,
            Self::OgImage => 60,
        }
    }
}

#[derive(Clone)]
struct RateLimiter {
    buckets: Arc<Mutex<RateLimiterState>>,
}

#[derive(Clone, Copy)]
struct RateBucket {
    started_at: Instant,
    count: u32,
}

struct RateLimiterState {
    buckets: LruCache<String, RateBucket>,
}

impl RateLimiterState {
    fn new(capacity: usize) -> Self {
        let capacity =
            NonZeroUsize::new(capacity.max(1)).expect("rate limiter capacity is nonzero");
        Self {
            buckets: LruCache::new(capacity),
        }
    }

    fn bucket_for(&mut self, key: String, now: Instant) -> &mut RateBucket {
        if self.buckets.get(&key).is_none() {
            self.buckets.push(
                key.clone(),
                RateBucket {
                    started_at: now,
                    count: 0,
                },
            );
        }

        self.buckets
            .get_mut(&key)
            .expect("rate limiter bucket is present after insertion")
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets.len()
    }
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
        let ip = normalize_ip(ip);
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
        Self {
            buckets: Arc::new(Mutex::new(RateLimiterState::new(capacity))),
        }
    }

    async fn check(&self, key: String, limit: u32) -> Result<(), u64> {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().await;
        let bucket = buckets.bucket_for(key, now);
        if now.duration_since(bucket.started_at) >= RATE_LIMIT_WINDOW {
            bucket.started_at = now;
            bucket.count = 0;
        }
        if bucket.count >= limit {
            let elapsed = now.duration_since(bucket.started_at);
            let remaining = RATE_LIMIT_WINDOW.saturating_sub(elapsed).as_secs().max(1);
            return Err(remaining);
        }
        bucket.count += 1;
        Ok(())
    }

    #[cfg(test)]
    async fn len(&self) -> usize {
        self.buckets.lock().await.len()
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
        let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        self.generation_millis_total
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(millis))
            })
            .ok();
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
) -> Result<(), Response> {
    let ip = client_ip(headers, peer_ip, &state.trusted_proxies);
    let key = rate_limit_key(route, &ip);
    match state.rate_limiter.check(key, route.limit()).await {
        Ok(()) => Ok(()),
        Err(retry_after_secs) => {
            state.metrics.limited_total.fetch_add(1, Ordering::Relaxed);
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                "rate limit exceeded, please retry shortly".to_string(),
            )
                .into_response();
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&retry_after_secs.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("60")),
            );
            Err(response)
        }
    }
}

fn rate_limit_key(route: RateLimitRoute, ip: &str) -> String {
    format!("{}:{ip}", route.as_str())
}

fn client_ip(headers: &HeaderMap, peer_ip: IpAddr, trusted_proxies: &TrustedProxies) -> String {
    let peer_ip = normalize_ip(peer_ip);
    if !trusted_proxies.contains(peer_ip) {
        return peer_ip.to_string();
    }

    if let Some(ip) = single_ip_header(headers, "cf-connecting-ip") {
        return ip.to_string();
    }

    if let Some(ip) = single_ip_header(headers, "x-real-ip") {
        return ip.to_string();
    }

    if let Some(value) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
    {
        for candidate in value.split(',').rev() {
            if let Ok(ip) = candidate.trim().parse::<IpAddr>().map(normalize_ip)
                && !trusted_proxies.contains(ip)
            {
                return ip.to_string();
            }
        }
    }
    peer_ip.to_string()
}

fn single_ip_header(headers: &HeaderMap, header_name: &'static str) -> Option<IpAddr> {
    headers
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
        .map(normalize_ip)
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ipv6)),
        IpAddr::V4(_) => ip,
    }
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

    let route = if request.persist {
        RateLimitRoute::StorageLink
    } else {
        RateLimitRoute::Avatar
    };
    if let Err(response) = enforce_limits(&state, &headers, peer_addr.ip(), route).await {
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
        Err(_) => return bad_request(INVALID_AVATAR_FORMAT_MESSAGE),
    };

    let request = AvatarRequest {
        identity: path.identity,
        namespace_tenant: DEFAULT_NAMESPACE_TENANT.to_string(),
        namespace_style: DEFAULT_NAMESPACE_STYLE.to_string(),
        kind,
        background: AvatarBackground::Themed,
        accessory: DEFAULT_ACCESSORY,
        color: DEFAULT_COLOR,
        expression: DEFAULT_EXPRESSION,
        shape: DEFAULT_SHAPE,
        format,
        size: 256,
        persist: false,
        redirect: false,
    };
    if let Err(message) = request.validate() {
        return bad_request(&message);
    }

    if let Err(response) =
        enforce_limits(&state, &headers, peer_addr.ip(), RateLimitRoute::Avatar).await
    {
        return response;
    }
    serve_avatar(state, request).await
}

async fn serve_avatar(state: AppState, request: AvatarRequest) -> Response {
    state.metrics.requests_total.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let asset = match generate_avatar_asset(request.clone()).await {
        Ok(asset) => asset,
        Err(response) => return response,
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
    let asset = match generate_avatar_asset(request).await {
        Ok(asset) => asset,
        Err(response) => return response,
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
                cache_key: sha256_hex(&asset.cache_key),
            }),
        )
            .into_response(),
        Ok(Err(error)) => internal_error(error),
        Err(_) => request_timeout("object storage timed out"),
    }
}

async fn generate_avatar_asset(request: AvatarRequest) -> Result<AvatarAsset, Response> {
    let render = tokio::task::spawn_blocking(move || build_avatar_asset(&request));
    match tokio::time::timeout(Duration::from_millis(AVATAR_TIMEOUT_MS), render).await {
        Ok(Ok(Ok(asset))) => Ok(asset),
        Ok(Ok(Err(message))) => Err(bad_request(&message)),
        Ok(Err(error)) => Err(internal_error(error)),
        Err(_) => Err(request_timeout("avatar generation timed out")),
    }
}

fn build_avatar_asset(request: &AvatarRequest) -> Result<AvatarAsset, String> {
    let identity = request.identity.trim();
    validate_identity(identity)?;
    validate_namespace_component("tenant", &request.namespace_tenant)?;
    validate_namespace_component("style_version", &request.namespace_style)?;

    if !(MIN_SIZE..=MAX_SIZE).contains(&request.size) {
        return Err("size must be between 64 and 1024".to_string());
    }

    let spec = AvatarSpec::new(request.size, request.size, 0)
        .map_err(|_| INVALID_AVATAR_RENDER_MESSAGE.to_string())?;
    let style = request.style_options();
    let namespace = AvatarNamespace::new(&request.namespace_tenant, &request.namespace_style)
        .map_err(|_| INVALID_NAMESPACE_MESSAGE.to_string())?;
    let identity_options = AvatarIdentityOptions::new(namespace);
    let accessory = request.effective_accessory();
    let expression = request.effective_expression();
    let cache_key = format!(
        "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
        request.namespace_tenant,
        request.namespace_style,
        DEFAULT_HASH_ALGORITHM,
        identity,
        request.kind,
        request.background,
        accessory,
        request.color,
        expression,
        request.shape,
        request.format,
        request.size
    );

    let body = encode_avatar_style_with_identity_options(
        spec,
        identity_options,
        identity,
        AvatarOutputFormat::WebP,
        style,
    )
    .map_err(|_| INVALID_AVATAR_RENDER_MESSAGE.to_string())?;

    Ok(AvatarAsset {
        body,
        content_type: "image/webp",
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
    format!("\"{}\"", sha256_hex(cache_key))
}

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

fn object_key_for(request: &AvatarRequest, identity: &str) -> String {
    let accessory = request.effective_accessory();
    let expression = request.effective_expression();
    let digest = Sha256::digest(
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            request.namespace_tenant,
            request.namespace_style,
            DEFAULT_HASH_ALGORITHM,
            identity,
            request.kind,
            request.background,
            accessory,
            request.color,
            expression,
            request.shape,
            request.format,
            request.size
        )
        .as_bytes(),
    );
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!(
        "{}/{}/{}/{}/{}/{}/{}/{}/{}/{}/{}.{}",
        request.namespace_tenant,
        request.namespace_style,
        DEFAULT_HASH_ALGORITHM,
        request.kind.as_str(),
        request.background.as_str(),
        accessory.as_str(),
        request.color.as_str(),
        expression.as_str(),
        request.shape.as_str(),
        request.size,
        encoded,
        request.format.as_str()
    )
}

fn validate_identity(identity: &str) -> Result<(), String> {
    if identity.is_empty() {
        return Err("missing identity".to_string());
    }
    if identity.len() > MAX_ID_BYTES {
        return Err(format!(
            "identity must be at most {MAX_ID_BYTES} bytes; send a stable internal id or one-way hash"
        ));
    }
    Ok(())
}

fn validate_namespace_component(name: &str, value: &str) -> Result<(), String> {
    if !is_valid_namespace_component(value) {
        return Err(format!(
            "{name} must be 1-{MAX_NAMESPACE_COMPONENT_BYTES} ASCII letters, digits, hyphens, or underscores"
        ));
    }
    Ok(())
}

fn validate_hash_algorithm(value: Option<&str>) -> Result<(), String> {
    match value.map(str::trim) {
        Some(raw) if !raw.is_empty() && !raw.eq_ignore_ascii_case(DEFAULT_HASH_ALGORITHM) => {
            Err(INVALID_HASH_ALGORITHM_MESSAGE.to_string())
        }
        _ => Ok(()),
    }
}

fn is_valid_namespace_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_NAMESPACE_COMPONENT_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
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
    if radius < 0 {
        return;
    }
    for y in -radius..=radius {
        for x in -radius..=radius {
            if is_inside_circle(x, y, radius) {
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

fn is_inside_circle(x: i32, y: i32, radius: i32) -> bool {
    if radius < 0 {
        return false;
    }
    let x_squared = i64::from(x) * i64::from(x);
    let y_squared = i64::from(y) * i64::from(y);
    let radius_squared = i64::from(radius) * i64::from(radius);
    x_squared + y_squared <= radius_squared
}

fn overlay(
    canvas: &mut RgbaImage,
    image: &RgbaImage,
    x: u32,
    y: u32,
) -> Result<(), image::ImageError> {
    canvas.copy_from(image, x, y)
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
      letter-spacing: 0;
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
      letter-spacing: 0;
    }
    h1 {
      font-size: clamp(2.2rem, 6vw, 4.4rem);
      line-height: 0.95;
      margin: 8px 0 8px;
      letter-spacing: 0;
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
        .replace('\'', "&#39;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn selected_attr(selected: bool) -> &'static str {
    if selected { " selected" } else { "" }
}

fn disabled_attr(disabled: bool) -> &'static str {
    if disabled { " disabled" } else { "" }
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
        AvatarKind::Bear => "Bear",
        AvatarKind::Penguin => "Penguin",
        AvatarKind::Dragon => "Dragon",
        AvatarKind::Ninja => "Ninja",
        AvatarKind::Astronaut => "Astronaut",
        AvatarKind::Diamond => "Diamond",
        AvatarKind::CoffeeCup => "Coffee Cup",
        AvatarKind::Shield => "Shield",
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
        AvatarBackground::PolkaDot => "Polka Dot",
        AvatarBackground::Striped => "Striped",
        AvatarBackground::Checkerboard => "Checkerboard",
        AvatarBackground::Grid => "Grid",
        AvatarBackground::Sunrise => "Sunrise",
        AvatarBackground::Ocean => "Ocean",
        AvatarBackground::Starry => "Starry",
    }
}

fn accessory_label(accessory: AvatarAccessory) -> &'static str {
    match accessory {
        AvatarAccessory::None => "None",
        AvatarAccessory::Glasses => "Glasses",
        AvatarAccessory::Hat => "Hat",
        AvatarAccessory::Headphones => "Headphones",
        AvatarAccessory::Crown => "Crown",
        AvatarAccessory::Bowtie => "Bowtie",
        AvatarAccessory::Eyepatch => "Eyepatch",
        AvatarAccessory::Scarf => "Scarf",
        AvatarAccessory::Halo => "Halo",
        AvatarAccessory::Horns => "Horns",
    }
}

fn color_label(color: AvatarColor) -> &'static str {
    match color {
        AvatarColor::Default => "Default",
        AvatarColor::NeonMint => "Neon Mint",
        AvatarColor::PastelPink => "Pastel Pink",
        AvatarColor::Crimson => "Crimson",
        AvatarColor::Gold => "Gold",
        AvatarColor::DeepSeaBlue => "Deep Sea Blue",
    }
}

fn expression_label(expression: AvatarExpression) -> &'static str {
    match expression {
        AvatarExpression::Default => "Default",
        AvatarExpression::Happy => "Happy",
        AvatarExpression::Grumpy => "Grumpy",
        AvatarExpression::Surprised => "Surprised",
        AvatarExpression::Sleepy => "Sleepy",
        AvatarExpression::Winking => "Winking",
        AvatarExpression::Cool => "Cool",
        AvatarExpression::Crying => "Crying",
    }
}

fn shape_label(shape: AvatarShape) -> &'static str {
    match shape {
        AvatarShape::Square => "Square",
        AvatarShape::Circle => "Circle",
        AvatarShape::Squircle => "Squircle",
        AvatarShape::Hexagon => "Hexagon",
        AvatarShape::Octagon => "Octagon",
    }
}

fn kind_options_html(selected: AvatarKind) -> String {
    AvatarKind::ALL
        .iter()
        .copied()
        .map(|kind| {
            format!(
                r#"<option value="{value}" data-identity="{value}@hashavatar.app" data-supports-layers="{supports_layers}"{selected}>{label}</option>"#,
                value = kind.as_str(),
                label = avatar_kind_label(kind),
                supports_layers = kind.supports_face_layers(),
                selected = selected_attr(kind == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn background_options_html(selected: AvatarBackground) -> String {
    AvatarBackground::ALL
        .iter()
        .copied()
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

fn accessory_options_html(selected: AvatarAccessory) -> String {
    AvatarAccessory::ALL
        .iter()
        .copied()
        .map(|accessory| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = accessory.as_str(),
                label = accessory_label(accessory),
                selected = selected_attr(accessory == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn color_options_html(selected: AvatarColor) -> String {
    AvatarColor::ALL
        .iter()
        .copied()
        .map(|color| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = color.as_str(),
                label = color_label(color),
                selected = selected_attr(color == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn expression_options_html(selected: AvatarExpression) -> String {
    AvatarExpression::ALL
        .iter()
        .copied()
        .map(|expression| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = expression.as_str(),
                label = expression_label(expression),
                selected = selected_attr(expression == selected),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn shape_options_html(selected: AvatarShape) -> String {
    AvatarShape::ALL
        .iter()
        .copied()
        .map(|shape| {
            format!(
                r#"<option value="{value}"{selected}>{label}</option>"#,
                value = shape.as_str(),
                label = shape_label(shape),
                selected = selected_attr(shape == selected),
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
        .iter()
        .copied()
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
                | AvatarKind::Paws
                | AvatarKind::Penguin
                | AvatarKind::Astronaut
                | AvatarKind::CoffeeCup => "white",
                AvatarKind::Panda | AvatarKind::Knight | AvatarKind::Bear => "light",
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
                "bear" => "bear@hashavatar.app",
                "penguin" => "penguin@hashavatar.app",
                "dragon" => "dragon@hashavatar.app",
                "ninja" => "ninja@hashavatar.app",
                "astronaut" => "astronaut@hashavatar.app",
                "diamond" => "diamond@hashavatar.app",
                "coffee-cup" => "coffee-cup@hashavatar.app",
                "shield" => "shield@hashavatar.app",
                _ => DEFAULT_ID,
            };
            preset
        })
        .collect()
}

fn preset_examples_json() -> String {
    serde_json::to_string(&preset_examples()).expect("preset examples should serialize")
}

fn render_meta_tags(title: &str, description: &str, path: &str, csp_nonce: &CspNonce) -> String {
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
        json_ld = render_json_ld(&full_title, description, &canonical, csp_nonce),
    )
}

fn json_script_string(value: &str, fallback: &str) -> String {
    serde_json::to_string(value)
        .or_else(|_| serde_json::to_string(fallback))
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace("</", "<\\/")
        .replace("<!--", "<\\u0021--")
}

fn render_json_ld(title: &str, description: &str, canonical: &str, csp_nonce: &CspNonce) -> String {
    let title = json_script_string(title, "hashavatar.app");
    let description = json_script_string(description, "Deterministic avatar API");
    let canonical = json_script_string(canonical, &format!("{SITE_URL}/"));
    let site_url = json_script_string(SITE_URL, SITE_URL);
    let search_target = json_script_string(
        &format!("{SITE_URL}/?id={{search_term_string}}"),
        &format!("{SITE_URL}/?id={{search_term_string}}"),
    );
    let nonce = escape_html_attribute(csp_nonce.as_str());

    format!(
        r#"<script nonce="{nonce}" type="application/ld+json">{{
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
<script nonce="{nonce}" type="application/ld+json">{{
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
        nonce = nonce,
    )
}

fn render_page_html(
    page_title: &str,
    description: &str,
    path: &str,
    eyebrow: &str,
    lead: &str,
    body: &str,
    csp_nonce: &CspNonce,
) -> String {
    let nonce = escape_html_attribute(csp_nonce.as_str());
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  {meta_tags}
  <style nonce="{nonce}">{styles}</style>
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
        meta_tags = render_meta_tags(page_title, description, path, csp_nonce),
        styles = shared_page_styles(),
        nonce = nonce,
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

fn render_index_html(csp_nonce: &CspNonce, storage_links_enabled: bool) -> String {
    let description = "Deterministic procedural avatars for opaque user ids, stable usernames, and one-way hashes. Generate 31 avatar families as WebP images.";
    let nonce = escape_html_attribute(csp_nonce.as_str());
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  {meta_tags}
  <style nonce="{nonce}">
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
      letter-spacing: 0;
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
      letter-spacing: 0;
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
      letter-spacing: 0;
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
      width: 100%;
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
          Turn any opaque user id, stable username, or one-way hash into a deterministic avatar URL.
          Choose the style, background, and size, then copy the WebP URL, download the result, or create a signed object-storage link.
        </p>
        <p>
          Privacy-conscious integration tip: email-shaped identifiers are accepted for convenience, but a stable internal id or one-way hash is better when you want less personal data in URL logs.
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
              <label for="accessory">Accessory</label>
              <select id="accessory">
                {accessory_options}
              </select>
            </div>
            <div>
              <label for="color">Accent Color</label>
              <select id="color">
                {color_options}
              </select>
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="expression">Expression</label>
              <select id="expression">
                {expression_options}
              </select>
            </div>
            <div>
              <label for="shape">Shape</label>
              <select id="shape">
                {shape_options}
              </select>
            </div>
          </div>

          <div class="field-grid full">
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
            <button id="copy-signed-button" type="button" class="secondary"{signed_disabled}>Copy Signed Link</button>
            <a id="download-button" class="button-link" href="/v1/avatar?id={id}&algorithm=sha512&kind=cat&background=themed&format=webp&size=256" download="hashavatar.webp">Download</a>
            <a id="open-button" class="button-link secondary" href="/v1/avatar?id={id}&algorithm=sha512&kind=cat&background=themed&format=webp&size=256" target="_blank" rel="noreferrer">Open Raw</a>
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
            <div class="url-text"><a class="inline-link" href="/docs/openapi.json">/docs/openapi.json</a></div>
          </div>
        </div>
      </div>

      <div class="preview">
        <div class="panel">
          <img id="avatar-preview" src="/v1/avatar?id={id}&algorithm=sha512&kind=cat&background=themed&format=webp&size=256" alt="Generated avatar preview" />
        </div>
        <div class="preview-meta">
          <div><strong>API:</strong> <span id="api-mode">/v1/avatar</span></div>
          <div><strong>Storage:</strong> optional S3 persistence with presigned links via <code>/v1/avatar/link</code></div>
          <div><strong>Cache:</strong> Cloudflare-friendly long cache headers</div>
          <div><strong>Tip:</strong> Every URL is deterministic, so you can embed it directly in your app.</div>
        </div>

        <div class="examples">
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
  <script nonce="{nonce}">
    const identityEl = document.getElementById("identity");
    const tenantEl = document.getElementById("tenant");
    const styleVersionEl = document.getElementById("style-version");
    const kindEl = document.getElementById("kind");
    const backgroundEl = document.getElementById("background");
    const accessoryEl = document.getElementById("accessory");
    const colorEl = document.getElementById("color");
    const expressionEl = document.getElementById("expression");
    const shapeEl = document.getElementById("shape");
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
    const storageLinksEnabled = {storage_links_enabled};
    const presetIdentities = new Map(
      Array.from(kindEl.options).map((option) => [option.value, option.dataset.identity])
    );
    const styleLayerSupport = new Map(
      Array.from(kindEl.options).map((option) => [option.value, option.dataset.supportsLayers === "true"])
    );
    let presetPage = 0;
    let refreshTimer = 0;
    let presetRenderTimer = 0;

    function currentIdentity() {{
      return identityEl.value.trim() || "{id}";
    }}

    function selectedPresetIdentity() {{
      return presetIdentities.get(kindEl.value) || "{id}";
    }}

    function supportsStyleLayers(kind) {{
      return styleLayerSupport.get(kind) !== false;
    }}

    function styleParamsForKind(kind) {{
      if (!supportsStyleLayers(kind)) {{
        return {{
          accessory: "none",
          color: colorEl.value,
          expression: "default",
          shape: shapeEl.value,
        }};
      }}
      return {{
        accessory: accessoryEl.value,
        color: colorEl.value,
        expression: expressionEl.value,
        shape: shapeEl.value,
      }};
    }}

    function syncStyleLayerAvailability() {{
      const supportsLayers = supportsStyleLayers(kindEl.value);
      accessoryEl.disabled = !supportsLayers;
      expressionEl.disabled = !supportsLayers;
      if (!supportsLayers) {{
        accessoryEl.value = "none";
        expressionEl.value = "default";
      }}
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
      const styleParams = styleParamsForKind(kindEl.value);
      const query = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        algorithm: "sha512",
        kind: kindEl.value,
        background: backgroundEl.value,
        accessory: styleParams.accessory,
        color: styleParams.color,
        expression: styleParams.expression,
        shape: styleParams.shape,
        format: "webp",
        size: sizeEl.value,
      }});
      return `/v1/avatar?${{query.toString()}}`;
    }}

    function currentSignedUrlEndpoint() {{
      const styleParams = styleParamsForKind(kindEl.value);
      const query = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        algorithm: "sha512",
        kind: kindEl.value,
        background: backgroundEl.value,
        accessory: styleParams.accessory,
        color: styleParams.color,
        expression: styleParams.expression,
        shape: styleParams.shape,
        format: "webp",
        size: sizeEl.value,
      }});
      return `/v1/avatar/link?${{query.toString()}}`;
    }}

    async function updateSignedUrl() {{
      if (!storageLinksEnabled) {{
        signedUrlEl.textContent = "Signed storage links are unavailable until S3 is configured on the server.";
        copySignedButton.disabled = true;
        return;
      }}

      try {{
        const response = await fetch(currentSignedUrlEndpoint(), {{ headers: {{ "accept": "application/json" }} }});
        if (!response.ok) {{
          signedUrlEl.textContent = "Signed storage links are unavailable until S3 is configured on the server.";
          return;
        }}
        const payload = await response.json();
        signedUrlEl.textContent = payload.signed_url;
        copySignedButton.disabled = false;
      }} catch (_) {{
        signedUrlEl.textContent = "Signed storage links are unavailable until S3 is configured on the server.";
        copySignedButton.disabled = true;
      }}
    }}

    function refresh() {{
      syncStyleLayerAvailability();
      const url = currentUrl();
      const styleParams = styleParamsForKind(kindEl.value);
      const previewQuery = new URLSearchParams({{
        id: currentIdentity(),
        tenant: tenantEl.value.trim() || "{tenant}",
        style_version: styleVersionEl.value.trim() || "{style_version}",
        algorithm: "sha512",
        kind: kindEl.value,
        background: backgroundEl.value,
        accessory: styleParams.accessory,
        color: styleParams.color,
        expression: styleParams.expression,
        shape: styleParams.shape,
        format: "webp",
        size: sizeEl.value,
        ts: String(Date.now()),
      }});

      previewEl.src = `/v1/avatar?${{previewQuery.toString()}}`;
      urlEl.textContent = `${{window.location.origin}}${{url}}`;
      downloadButton.href = url;
      downloadButton.setAttribute("download", `hashavatar-${{kindEl.value}}.webp`);
      openButton.href = url;
      updateSignedUrl();
    }}

    function scheduleRefresh() {{
      window.clearTimeout(refreshTimer);
      refreshTimer = window.setTimeout(refresh, 180);
    }}

    function scheduleFullRefresh() {{
      window.clearTimeout(refreshTimer);
      window.clearTimeout(presetRenderTimer);
      refreshTimer = window.setTimeout(refresh, 180);
      presetRenderTimer = window.setTimeout(renderPresetPage, 180);
    }}

    function refreshNowWithPresets() {{
      window.clearTimeout(refreshTimer);
      window.clearTimeout(presetRenderTimer);
      renderPresetPage();
      refresh();
    }}

    function setFromPreset(preset) {{
      identityEl.value = preset.id;
      tenantEl.value = "{tenant}";
      styleVersionEl.value = "{style_version}";
      kindEl.value = preset.kind;
      backgroundEl.value = backgroundEl.value || preset.background;
      sizeEl.value = preset.size;
      refreshNowWithPresets();
    }}

    function renderPresetPage() {{
      const pageCount = Math.ceil(presetExamples.length / presetPageSize);
      presetPage = (presetPage + pageCount) % pageCount;
      const start = presetPage * presetPageSize;
      const pageItems = presetExamples.slice(start, start + presetPageSize);
      const exampleBackground = backgroundEl.value || "themed";
      exampleGrid.replaceChildren();
      for (const preset of pageItems) {{
        const styleParams = styleParamsForKind(preset.kind);
        const button = document.createElement("button");
        button.type = "button";
        button.className = "example-card";
        button.addEventListener("click", () => setFromPreset(preset));

        const query = new URLSearchParams({{
          id: currentIdentity(),
          tenant: tenantEl.value.trim() || "{tenant}",
          style_version: styleVersionEl.value.trim() || "{style_version}",
          algorithm: "sha512",
          kind: preset.kind,
          background: exampleBackground,
          accessory: styleParams.accessory,
          color: styleParams.color,
          expression: styleParams.expression,
          shape: styleParams.shape,
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

    [identityEl, tenantEl, styleVersionEl].forEach((el) => {{
      el.addEventListener("input", scheduleFullRefresh);
      el.addEventListener("change", refreshNowWithPresets);
    }});

    sizeEl.addEventListener("input", scheduleRefresh);
    sizeEl.addEventListener("change", refresh);

    [backgroundEl, accessoryEl, colorEl, expressionEl, shapeEl].forEach((el) => {{
      el.addEventListener("input", scheduleRefresh);
      el.addEventListener("change", refreshNowWithPresets);
    }});

    kindEl.addEventListener("change", () => {{
      const current = identityEl.value.trim();
      if (current === "" || isPresetIdentity(current)) {{
        identityEl.value = selectedPresetIdentity();
      }}
      refreshNowWithPresets();
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
        accessory_options = accessory_options_html(DEFAULT_ACCESSORY),
        color_options = color_options_html(DEFAULT_COLOR),
        expression_options = expression_options_html(DEFAULT_EXPRESSION),
        shape_options = shape_options_html(DEFAULT_SHAPE),
        preset_examples = preset_examples_json(),
        preset_page_size = PRESET_PAGE_SIZE,
        storage_links_enabled = storage_links_enabled,
        signed_disabled = disabled_attr(!storage_links_enabled),
        meta_tags = render_meta_tags("Public Avatar API", description, "/", csp_nonce),
        styles = shared_page_styles(),
        nonce = nonce,
        footer = render_footer_html(),
        repo = REPOSITORY_URL,
        crate_url = CRATE_URL,
    )
}

fn render_help_html(csp_nonce: &CspNonce) -> String {
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
    <pre><code>https://{site}/v1/avatar?id=robot@hashavatar.app&amp;algorithm=sha512&amp;kind=robot&amp;background=white&amp;accessory=glasses&amp;color=gold&amp;expression=happy&amp;shape=circle&amp;format=webp&amp;size=256</code></pre>
  </section>
  <section class="card">
    <h2>Path Style URL</h2>
    <p>Use the path form if you prefer cleaner embed URLs.</p>
    <pre><code>https://{site}/avatar/fox/fox@hashavatar.app/webp</code></pre>
  </section>
  <section class="card">
    <h2>HTML Example</h2>
    <pre><code>&lt;img
  src="https://{site}/v1/avatar?id=monster@hashavatar.app&amp;algorithm=sha512&amp;kind=monster&amp;background=themed&amp;accessory=horns&amp;color=crimson&amp;expression=grumpy&amp;shape=hexagon&amp;format=webp&amp;size=256"
  alt="Generated monster avatar"
/&gt;</code></pre>
  </section>
  <section class="card">
    <h2>JavaScript Example</h2>
    <pre><code>const avatarUrl = new URL("https://{site}/v1/avatar");
avatarUrl.search = new URLSearchParams({{
  id: user.email,
  algorithm: "sha512",
  kind: "robot",
  background: "white",
  accessory: "glasses",
  color: "gold",
  expression: "happy",
  shape: "circle",
  format: "webp",
  size: "256",
}}).toString();</code></pre>
  </section>
</div>
<section class="card">
  <h2>Supported Parameters</h2>
  <ul>
    <li><code>id</code>: any stable identifier such as an email, username, internal user id, or one-way hash</li>
    <li><code>tenant</code>: optional namespace partition for multi-tenant apps</li>
    <li><code>style_version</code>: optional style namespace such as <code>v2</code></li>
    <li><code>algorithm</code>: identity hash mode; only <code>sha512</code> is supported</li>
    <li><code>kind</code>: any public hashavatar family, including <code>cat</code>, <code>dog</code>, <code>robot</code>, <code>planet</code>, <code>rocket</code>, <code>frog</code>, <code>panda</code>, <code>cupcake</code>, <code>pizza</code>, <code>octopus</code>, <code>knight</code>, <code>bear</code>, <code>penguin</code>, <code>dragon</code>, <code>ninja</code>, <code>astronaut</code>, <code>diamond</code>, <code>coffee-cup</code>, and <code>shield</code></li>
    <li><code>background</code>: <code>themed</code>, <code>white</code>, <code>black</code>, <code>dark</code>, <code>light</code>, <code>transparent</code>, <code>polka-dot</code>, <code>striped</code>, <code>checkerboard</code>, <code>grid</code>, <code>sunrise</code>, <code>ocean</code>, or <code>starry</code></li>
    <li><code>accessory</code>: <code>none</code>, <code>glasses</code>, <code>hat</code>, <code>headphones</code>, <code>crown</code>, <code>bowtie</code>, <code>eyepatch</code>, <code>scarf</code>, <code>halo</code>, or <code>horns</code></li>
    <li><code>color</code>: <code>default</code>, <code>neon-mint</code>, <code>pastel-pink</code>, <code>crimson</code>, <code>gold</code>, or <code>deep-sea-blue</code></li>
    <li><code>expression</code>: <code>default</code>, <code>happy</code>, <code>grumpy</code>, <code>surprised</code>, <code>sleepy</code>, <code>winking</code>, <code>cool</code>, or <code>crying</code></li>
    <li><code>shape</code>: <code>square</code>, <code>circle</code>, <code>squircle</code>, <code>hexagon</code>, or <code>octagon</code></li>
    <li><code>format</code>: output format; only <code>webp</code> is supported</li>
    <li><code>size</code>: from <code>64</code> up to <code>1024</code></li>
  </ul>
  <p>Accessory and expression layers apply to character-style families. Object-style families such as <code>planet</code>, <code>rocket</code>, <code>paws</code>, <code>mushroom</code>, <code>cactus</code>, <code>cupcake</code>, <code>pizza</code>, <code>icecream</code>, <code>diamond</code>, <code>coffee-cup</code>, and <code>shield</code> are normalized to <code>accessory=none</code> and <code>expression=default</code>.</p>
</section>
<section class="card">
  <h2>Signed Storage Links</h2>
  <p>If this deployment has object storage configured, request a presigned storage link from <code>/v1/avatar/link</code>. That endpoint stores the generated object and returns JSON with the signed URL, object key, and a hashed cache key. Standard avatar responses do not expose signed-link metadata in response headers.</p>
  <pre><code>GET https://{site}/v1/avatar/link?id=robot@hashavatar.app&amp;algorithm=sha512&amp;kind=robot&amp;background=white&amp;accessory=glasses&amp;color=gold&amp;expression=happy&amp;shape=circle&amp;format=webp&amp;size=256</code></pre>
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
        csp_nonce,
    )
}

fn render_docs_html(csp_nonce: &CspNonce) -> String {
    render_page_html(
        "Docs",
        "Reference documentation for the hashavatar.app public avatar API, storage link endpoint, and namespace-aware identity contract.",
        "/docs",
        "API Reference",
        "This is the product-facing reference for the public API. The same identity, tenant, style version, avatar family, style options, size, and WebP output are intended to remain stable within a major release.",
        &format!(
            r#"
<section class="card">
  <h2>Core Endpoints</h2>
  <ul>
    <li><code>GET /v1/avatar</code>: returns an avatar asset directly</li>
    <li><code>GET /v1/avatar/link</code>: stores the generated avatar in configured object storage and returns signed-link metadata</li>
    <li><code>GET /avatar/&lt;kind&gt;/&lt;identity&gt;/webp</code>: path-style public avatar URL</li>
    <li><code>GET /docs/openapi.json</code>: machine-readable API description</li>
  </ul>
</section>
<section class="card">
  <h2>Operational Endpoints</h2>
  <p><code>GET /healthz</code> is public for load balancers and uptime checks. <code>GET /metrics</code> is loopback-only and returns <code>404</code> to non-local peers.</p>
</section>
<div class="content-grid">
  <section class="card">
    <h2>Namespace Support</h2>
    <p>Use <code>tenant</code> and <code>style_version</code> to keep visual identity spaces separate between products or rollout phases.</p>
    <pre><code>GET https://{site}/v1/avatar?id=wizard@hashavatar.app&amp;tenant=acme&amp;style_version=v2&amp;algorithm=sha512&amp;kind=wizard&amp;background=white&amp;accessory=hat&amp;color=deep-sea-blue&amp;expression=cool&amp;shape=squircle&amp;format=webp&amp;size=256</code></pre>
  </section>
  <section class="card">
    <h2>Anonymous IDs</h2>
    <p>Send an internal stable id or a one-way application hash instead of raw personal data.</p>
  <pre><code>id = sha256(lowercase(email))</code></pre>
  </section>
  <section class="card">
    <h2>Rate Limits</h2>
    <p>The public service applies origin-side rate limits, with stricter limits on <code>/v1/avatar/link</code>, direct avatar requests with <code>persist=true</code>, and <code>/og.png</code> because object storage writes and Open Graph image rendering are more expensive than direct rendering.</p>
  </section>
  <section class="card">
    <h2>Timeouts</h2>
    <p>Avatar generation and storage operations are bounded by server-side timeouts so expensive requests cannot monopolize the origin indefinitely.</p>
  </section>
</div>
<section class="card">
  <h2>Errors</h2>
  <ul>
    <li><code>400</code>: invalid kind, unsupported algorithm or format, size, or missing identity</li>
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
        csp_nonce,
    )
}

fn render_terms_html(csp_nonce: &CspNonce) -> String {
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
        csp_nonce,
    )
}

fn render_privacy_html(csp_nonce: &CspNonce) -> String {
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
    <li>the opaque identifier you put in the request, such as an internal id, username, or one-way hash</li>
    <li>request parameters such as avatar type, style options, size, format, and background</li>
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
  <p>Email-shaped identifiers are accepted for compatibility, but URLs can appear in infrastructure logs. Send an internal stable id or a one-way application hash when you want to avoid putting personal data in the request URL.</p>
</section>
<section class="card">
  <h2>Repository And Crate</h2>
  <p>You can inspect the implementation in the public <a class="inline-link" href="https://github.com/valkyoth/hashavatar-api" target="_blank" rel="noreferrer">API repository</a> and the reusable avatar renderer in the <a class="inline-link" href="https://crates.io/crates/hashavatar/" target="_blank" rel="noreferrer">Rust crate</a>.</p>
</section>
"#,
        csp_nonce,
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
    accessory: Option<String>,
    color: Option<String>,
    expression: Option<String>,
    shape: Option<String>,
    format: Option<String>,
    algorithm: Option<String>,
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
}

impl AvatarRequestFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
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
            _ => Err(INVALID_AVATAR_FORMAT_MESSAGE),
        }
    }
}

#[derive(Clone)]
struct AvatarRequest {
    identity: String,
    namespace_tenant: String,
    namespace_style: String,
    kind: AvatarKind,
    background: AvatarBackground,
    accessory: AvatarAccessory,
    color: AvatarColor,
    expression: AvatarExpression,
    shape: AvatarShape,
    format: AvatarRequestFormat,
    size: u32,
    persist: bool,
    redirect: bool,
}

impl std::fmt::Debug for AvatarRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AvatarRequest")
            .field("identity", &"[redacted]")
            .field("namespace_tenant", &self.namespace_tenant)
            .field("namespace_style", &self.namespace_style)
            .field("kind", &self.kind)
            .field("background", &self.background)
            .field("accessory", &self.accessory)
            .field("color", &self.color)
            .field("expression", &self.expression)
            .field("shape", &self.shape)
            .field("format", &self.format)
            .field("size", &self.size)
            .field("persist", &self.persist)
            .field("redirect", &self.redirect)
            .finish()
    }
}

impl AvatarRequest {
    fn from_query(query: AvatarQuery) -> Result<Self, String> {
        validate_hash_algorithm(query.algorithm.as_deref())?;
        let format = match query.format.as_deref().map(str::trim) {
            Some(raw) if !raw.is_empty() => AvatarRequestFormat::from_str(raw)
                .map_err(|_| INVALID_AVATAR_FORMAT_MESSAGE.to_string())?,
            _ => AvatarRequestFormat::Webp,
        };

        let request = Self {
            identity: query
                .id
                .map(|value| value.trim().to_string())
                .unwrap_or_else(|| DEFAULT_ID.to_string()),
            namespace_tenant: query
                .tenant
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_NAMESPACE_TENANT.to_string()),
            namespace_style: query
                .style_version
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
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
            accessory: query
                .accessory
                .as_deref()
                .and_then(|raw| AvatarAccessory::from_str(raw).ok())
                .unwrap_or(DEFAULT_ACCESSORY),
            color: query
                .color
                .as_deref()
                .and_then(|raw| AvatarColor::from_str(raw).ok())
                .unwrap_or(DEFAULT_COLOR),
            expression: query
                .expression
                .as_deref()
                .and_then(|raw| AvatarExpression::from_str(raw).ok())
                .unwrap_or(DEFAULT_EXPRESSION),
            shape: query
                .shape
                .as_deref()
                .and_then(|raw| AvatarShape::from_str(raw).ok())
                .unwrap_or(DEFAULT_SHAPE),
            format,
            size: query.size.unwrap_or(256),
            persist: query.persist.unwrap_or(false),
            redirect: query.redirect.unwrap_or(false),
        };
        request.validate()?;
        Ok(request)
    }

    fn validate(&self) -> Result<(), String> {
        validate_identity(self.identity.trim())?;
        validate_namespace_component("tenant", &self.namespace_tenant)?;
        validate_namespace_component("style_version", &self.namespace_style)?;
        Ok(())
    }

    fn effective_accessory(&self) -> AvatarAccessory {
        if self.kind.supports_face_layers() {
            self.accessory
        } else {
            DEFAULT_ACCESSORY
        }
    }

    fn effective_expression(&self) -> AvatarExpression {
        if self.kind.supports_face_layers() {
            self.expression
        } else {
            DEFAULT_EXPRESSION
        }
    }

    fn style_options(&self) -> AvatarStyleOptions {
        AvatarStyleOptions::new(
            self.kind,
            self.background,
            self.effective_accessory(),
            self.color,
            self.effective_expression(),
            self.shape,
        )
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
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn response_text(response: Response) -> String {
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("response body");
        std::str::from_utf8(&body).expect("utf8 body").to_string()
    }

    fn test_avatar_request(format: AvatarRequestFormat) -> AvatarRequest {
        AvatarRequest {
            identity: DEFAULT_ID.to_string(),
            namespace_tenant: DEFAULT_NAMESPACE_TENANT.to_string(),
            namespace_style: DEFAULT_NAMESPACE_STYLE.to_string(),
            kind: AvatarKind::Cat,
            background: AvatarBackground::Themed,
            accessory: DEFAULT_ACCESSORY,
            color: DEFAULT_COLOR,
            expression: DEFAULT_EXPRESSION,
            shape: DEFAULT_SHAPE,
            format,
            size: 256,
            persist: false,
            redirect: false,
        }
    }

    #[tokio::test]
    async fn rate_limiter_enforces_per_key_limit() {
        let limiter = RateLimiter::with_capacity(8);
        let key = "avatar:127.0.0.1:public:cat".to_string();

        assert!(limiter.check(key.clone(), 2).await.is_ok());
        assert!(limiter.check(key.clone(), 2).await.is_ok());
        let retry_after = limiter
            .check(key, 2)
            .await
            .expect_err("third request should be rate limited");
        assert!((1..=60).contains(&retry_after));
    }

    #[tokio::test]
    async fn rate_limiter_evicts_oldest_bucket_at_capacity() {
        let limiter = RateLimiter::with_capacity(2);

        assert!(limiter.check("first".to_string(), 1).await.is_ok());
        assert!(limiter.check("second".to_string(), 1).await.is_ok());
        assert_eq!(limiter.len().await, 2);

        assert!(limiter.check("third".to_string(), 1).await.is_ok());
        assert_eq!(limiter.len().await, 2);

        assert!(limiter.check("first".to_string(), 1).await.is_ok());
        assert_eq!(limiter.len().await, 2);
    }

    #[tokio::test]
    async fn rate_limiter_bounds_unique_attacker_keys() {
        let limiter = RateLimiter::with_capacity(32);

        for idx in 0..1_000 {
            assert!(
                limiter
                    .check(format!("avatar:spoofed-{idx}:tenant-{idx}:cat"), 1)
                    .await
                    .is_ok()
            );
        }

        assert_eq!(limiter.len().await, 32);
    }

    #[test]
    fn rate_limiter_capacity_is_churn_resistant() {
        let capacity = MAX_RATE_LIMIT_BUCKETS;
        assert!(capacity >= 65_536);
    }

    #[test]
    fn rate_limit_key_is_route_and_ip_scoped() {
        assert_eq!(
            rate_limit_key(RateLimitRoute::Avatar, "203.0.113.10"),
            "avatar:203.0.113.10"
        );
        assert_eq!(
            rate_limit_key(RateLimitRoute::StorageLink, "203.0.113.10"),
            "storage-link:203.0.113.10"
        );
        assert_eq!(
            rate_limit_key(RateLimitRoute::OgImage, "203.0.113.10"),
            "og-image:203.0.113.10"
        );
    }

    #[tokio::test]
    async fn rate_limit_response_includes_retry_after() {
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(8),
            metrics: Metrics::default(),
        };
        let headers = HeaderMap::new();
        let peer_ip = IpAddr::from([203, 0, 113, 10]);

        for _ in 0..RateLimitRoute::StorageLink.limit() {
            assert!(
                enforce_limits(&state, &headers, peer_ip, RateLimitRoute::StorageLink)
                    .await
                    .is_ok()
            );
        }

        let response = enforce_limits(&state, &headers, peer_ip, RateLimitRoute::StorageLink)
            .await
            .expect_err("request should be rate limited");

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .expect("retry-after should be a second count");
        assert!((1..=60).contains(&retry_after));
        assert_eq!(state.metrics.limited_total.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn rate_limiter_uses_non_poisoning_async_mutex() {
        let limiter = RateLimiter::with_capacity(2);
        let buckets = limiter.buckets.clone();

        let task = tokio::spawn(async move {
            let _guard = buckets.lock().await;
            panic!("poison rate limiter lock");
        });
        assert!(task.await.expect_err("task should panic").is_panic());

        assert!(limiter.check("after-poison".to_string(), 1).await.is_ok());
    }

    #[test]
    fn metrics_endpoint_is_loopback_only() {
        assert!(is_loopback_peer(
            "127.0.0.1:8080".parse().expect("ipv4 loopback")
        ));
        assert!(is_loopback_peer(
            "[::1]:8080".parse().expect("ipv6 loopback")
        ));
        assert!(is_loopback_peer(
            "[::ffff:127.0.0.1]:8080"
                .parse()
                .expect("mapped ipv4 loopback")
        ));
        assert!(!is_loopback_peer(
            "198.51.100.10:8080".parse().expect("remote peer")
        ));
    }

    #[tokio::test]
    async fn healthz_only_exposes_liveness() {
        let response = healthz().await.into_response();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_text(response).await;
        let payload: serde_json::Value =
            serde_json::from_str(&body).expect("healthz should return json");

        assert_eq!(payload, serde_json::json!({"status": "ok"}));
        assert!(payload.get("service").is_none());
        assert!(payload.get("s3_enabled").is_none());
        assert!(payload.get("style_version").is_none());
    }

    #[test]
    fn metrics_generation_duration_saturates_at_u64_max() {
        let metrics = Metrics::default();

        metrics
            .generation_millis_total
            .store(u64::MAX - 10, Ordering::Relaxed);
        metrics.observe_generation(Duration::from_millis(25));

        assert_eq!(
            metrics.generation_millis_total.load(Ordering::Relaxed),
            u64::MAX
        );

        metrics.observe_generation(Duration::from_secs(u64::MAX));

        assert_eq!(
            metrics.generation_millis_total.load(Ordering::Relaxed),
            u64::MAX
        );
    }

    #[test]
    fn content_security_policy_uses_nonce_without_unsafe_inline() {
        let nonce = CspNonce("testnonce".to_string());
        let policy = content_security_policy(&nonce);

        assert!(policy.contains("style-src 'self' 'nonce-testnonce'"));
        assert!(policy.contains("script-src 'self' 'nonce-testnonce'"));
        assert!(!policy.contains("unsafe-inline"));
    }

    #[test]
    fn non_html_routes_use_static_csp_without_nonce() {
        assert!(route_uses_inline_html("/"));
        assert!(route_uses_inline_html("/docs"));
        assert!(!route_uses_inline_html("/v1/avatar"));
        assert!(!route_uses_inline_html("/og.png"));
        assert!(!static_content_security_policy().contains("nonce-"));
        assert!(static_content_security_policy().contains("script-src 'self'"));
    }

    #[test]
    fn security_headers_include_modern_isolation_policy() {
        let mut html_response = Html("ok").into_response();
        apply_security_headers(
            html_response.headers_mut(),
            &content_security_policy(&CspNonce("testnonce".to_string())),
            true,
        );

        assert_eq!(
            html_response
                .headers()
                .get("cross-origin-resource-policy")
                .and_then(|value| value.to_str().ok()),
            Some("cross-origin")
        );
        assert_eq!(
            html_response
                .headers()
                .get("cross-origin-opener-policy")
                .and_then(|value| value.to_str().ok()),
            Some("same-origin")
        );
        assert_eq!(
            html_response
                .headers()
                .get("strict-transport-security")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000; includeSubDomains")
        );

        let mut image_headers = cache_headers("\"etag\"");
        apply_security_headers(
            &mut image_headers,
            &content_security_policy(&CspNonce("testnonce".to_string())),
            false,
        );

        assert_eq!(
            image_headers
                .get("cross-origin-resource-policy")
                .and_then(|value| value.to_str().ok()),
            Some("cross-origin")
        );
        assert!(!image_headers.contains_key("cross-origin-opener-policy"));
        assert_eq!(
            image_headers
                .get("strict-transport-security")
                .and_then(|value| value.to_str().ok()),
            Some("max-age=31536000; includeSubDomains")
        );
    }

    #[test]
    fn standard_avatar_response_does_not_emit_signed_storage_headers() {
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
        let serve_avatar_source = source
            .split("async fn serve_avatar(")
            .nth(1)
            .and_then(|after_name| after_name.split("async fn serve_avatar_link(").next())
            .expect("serve_avatar source should be present");
        let serve_avatar_link_source = source
            .split("async fn serve_avatar_link(")
            .nth(1)
            .and_then(|after_name| after_name.split("async fn generate_avatar_asset(").next())
            .expect("serve_avatar_link source should be present");

        assert!(!serve_avatar_source.contains("HeaderName::storage_key()"));
        assert!(!serve_avatar_source.contains("HeaderName::signed_url()"));
        assert!(source.contains("async fn serve_avatar_link("));
        assert!(serve_avatar_link_source.contains("object_key: signed.object_key"));
        assert!(serve_avatar_link_source.contains("signed_url: signed.signed_url"));
        assert!(serve_avatar_link_source.contains("cache_key: sha256_hex(&asset.cache_key)"));
        assert!(!serve_avatar_link_source.contains("cache_key: asset.cache_key"));
    }

    #[test]
    fn rendered_index_applies_csp_nonce_to_inline_blocks() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, false);

        assert!(html.contains(r#"<style nonce="testnonce">"#));
        assert!(html.contains(r#"<script nonce="testnonce">"#));
        assert!(html.contains(r#"<script nonce="testnonce" type="application/ld+json">"#));
    }

    #[test]
    fn rendered_index_disables_signed_link_fetches_without_storage() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, false);

        assert!(html.contains("const storageLinksEnabled = false;"));
        assert!(
            html.contains(r#"id="copy-signed-button" type="button" class="secondary" disabled"#)
        );
    }

    #[test]
    fn rendered_index_enables_signed_link_fetches_with_storage() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, true);

        assert!(html.contains("const storageLinksEnabled = true;"));
        assert!(
            !html.contains(r#"id="copy-signed-button" type="button" class="secondary" disabled"#)
        );
    }

    #[test]
    fn rendered_index_exposes_avatar_style_controls() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_index_html(&nonce, false);

        assert!(html.contains(r#"<select id="accessory">"#));
        assert!(html.contains(r#"<select id="color">"#));
        assert!(html.contains(r#"<select id="expression">"#));
        assert!(html.contains(r#"<select id="shape">"#));
        assert!(html.contains(
            r#"value="cat" data-identity="cat@hashavatar.app" data-supports-layers="true""#
        ));
        assert!(html.contains(
            r#"value="planet" data-identity="planet@hashavatar.app" data-supports-layers="false""#
        ));
        assert!(html.contains(
            r#"value="bear" data-identity="bear@hashavatar.app" data-supports-layers="true""#
        ));
        assert!(html.contains(
            r#"value="coffee-cup" data-identity="coffee-cup@hashavatar.app" data-supports-layers="false""#
        ));
        for background in [
            "polka-dot",
            "striped",
            "checkerboard",
            "grid",
            "sunrise",
            "ocean",
            "starry",
        ] {
            assert!(
                html.contains(&format!(r#"value="{background}""#)),
                "missing background option {background}"
            );
        }
        assert!(html.contains("syncStyleLayerAvailability();"));
        assert!(html.contains("accessoryEl.disabled = !supportsLayers;"));
        assert!(html.contains("accessory: accessoryEl.value"));
        assert!(html.contains("color: colorEl.value"));
        assert!(html.contains("expression: expressionEl.value"));
        assert!(html.contains("shape: shapeEl.value"));
        assert!(!html.contains("algorithm-options"));
        assert!(!html.contains(r#"id="format""#));
        assert!(html.contains(r#"algorithm: "sha512""#));
        assert!(html.contains(r#"format: "webp""#));
        assert!(!html.contains("id: preset.id"));
        assert!(html.contains(r#"el.addEventListener("input", scheduleFullRefresh);"#));
        assert!(html.contains("refreshNowWithPresets();"));
        assert!(!html.contains(r#"el.addEventListener("input", renderPresetPage);"#));
    }

    #[test]
    fn public_docs_do_not_advertise_metrics_as_public_api() {
        let nonce = CspNonce("testnonce".to_string());
        let index_html = render_index_html(&nonce, false);
        let docs_html = render_docs_html(&nonce);
        let openapi = openapi_document();

        assert!(!index_html.contains(r#"href="/metrics""#));
        assert!(docs_html.contains("loopback-only"));
        assert!(docs_html.contains("returns <code>404</code> to non-local peers"));
        assert!(openapi["paths"].get("/metrics").is_none());
    }

    #[test]
    fn og_png_handler_applies_avatar_rate_limits() {
        let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/main.rs"));
        let handler = source
            .split("async fn og_png(")
            .nth(1)
            .and_then(|after_name| after_name.split("enum OgPngError").next())
            .expect("og_png handler should be present");

        assert!(handler.contains("enforce_limits("));
        assert!(handler.contains("RateLimitRoute::OgImage"));
        assert!(handler.contains("validate_identity(&title_id)"));
        assert!(handler.contains("tokio::task::spawn_blocking"));
        assert!(handler.contains("build_og_png_bytes("));
        assert!(handler.contains("tokio::time::timeout"));
        assert!(!handler.contains("ImageBuffer::from_pixel"));
    }

    #[test]
    fn render_json_ld_escapes_script_end_tags() {
        let nonce = CspNonce("testnonce".to_string());
        let html = render_json_ld(
            "</script><script>alert(1)</script>",
            "description",
            "https://hashavatar.app/",
            &nonce,
        );

        assert!(html.contains(r#"<\/script><script>alert(1)<\/script>"#));
        assert!(!html.contains("</script><script>alert(1)</script>"));
    }

    #[test]
    fn escape_html_attribute_handles_single_quotes() {
        assert_eq!(
            escape_html_attribute(r#"'"><tag>&"#),
            "&#39;&quot;&gt;&lt;tag&gt;&amp;"
        );
    }

    #[test]
    fn etag_uses_full_sha256_digest() {
        let etag = etag_for("example-cache-key");
        let raw = etag.trim_matches('"');

        assert_eq!(etag.len(), 66);
        assert_eq!(raw.len(), 64);
        assert!(raw.bytes().all(|byte| byte.is_ascii_hexdigit()));
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
    fn ipv4_mapped_addresses_are_canonicalized_for_rate_limits() {
        let mapped_peer = "::ffff:198.51.100.10"
            .parse::<IpAddr>()
            .expect("mapped peer");
        let mapped_proxy = "::ffff:10.89.42.10"
            .parse::<IpAddr>()
            .expect("mapped proxy");
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("::ffff:203.0.113.99, ::ffff:10.89.42.10"),
        );
        let trusted_proxies = TrustedProxies::parse("10.89.42.0/24").expect("trusted proxy CIDR");

        assert_eq!(normalize_ip(mapped_peer).to_string(), "198.51.100.10");
        assert!(trusted_proxies.contains(mapped_proxy));
        assert_eq!(
            client_ip(&headers, mapped_proxy, &trusted_proxies),
            "203.0.113.99"
        );
        assert_eq!(
            client_ip(&HeaderMap::new(), mapped_peer, &TrustedProxies::default()),
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
    fn client_ip_uses_rightmost_untrusted_forwarded_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("192.0.2.123, 203.0.113.99, 10.89.42.10"),
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

    #[test]
    fn invalid_algorithm_error_does_not_reflect_input() {
        let reflected = "sha512<script>alert(1)</script>";
        let error = AvatarRequest::from_query(AvatarQuery {
            algorithm: Some(reflected.to_string()),
            id: Some(DEFAULT_ID.to_string()),
            kind: None,
            background: None,
            accessory: None,
            color: None,
            expression: None,
            shape: None,
            format: None,
            size: None,
            tenant: None,
            style_version: None,
            persist: None,
            redirect: None,
        })
        .expect_err("invalid algorithm should be rejected");

        assert_eq!(error, INVALID_HASH_ALGORITHM_MESSAGE);
        assert!(!error.contains(reflected));
        assert!(!error.contains("<script>"));
    }

    #[tokio::test]
    async fn og_namespace_error_does_not_reflect_input() {
        let reflected = "public<script>alert(1)</script>";
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(8),
            metrics: Metrics::default(),
        };
        let response = og_png(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().expect("peer address")),
            HeaderMap::new(),
            Query(OgQuery {
                id: Some(DEFAULT_ID.to_string()),
                tenant: Some(reflected.to_string()),
                style_version: Some(DEFAULT_NAMESPACE_STYLE.to_string()),
                kind: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_text(response).await;

        assert_eq!(body, INVALID_NAMESPACE_MESSAGE);
        assert!(!body.contains(reflected));
        assert!(!body.contains("<script>"));
    }

    #[tokio::test]
    async fn og_png_rejects_oversized_identity_before_rendering() {
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(8),
            metrics: Metrics::default(),
        };
        let response = og_png(
            State(state),
            ConnectInfo("127.0.0.1:8080".parse().expect("peer address")),
            HeaderMap::new(),
            Query(OgQuery {
                id: Some("x".repeat(MAX_ID_BYTES + 1)),
                tenant: None,
                style_version: None,
                kind: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response_text(response).await;
        assert!(body.contains("identity must be at most"));
    }

    #[tokio::test]
    async fn persisted_avatar_requests_use_storage_rate_limit() {
        let state = AppState {
            storage: None,
            trusted_proxies: TrustedProxies::default(),
            rate_limiter: RateLimiter::with_capacity(64),
            metrics: Metrics::default(),
        };
        let headers = HeaderMap::new();
        let peer_addr: SocketAddr = "127.0.0.1:8080".parse().expect("peer address");

        for _ in 0..RateLimitRoute::StorageLink.limit() {
            assert!(
                enforce_limits(
                    &state,
                    &headers,
                    peer_addr.ip(),
                    RateLimitRoute::StorageLink
                )
                .await
                .is_ok()
            );
        }

        let response = query_avatar(
            State(state),
            ConnectInfo(peer_addr),
            headers,
            Query(AvatarQuery {
                algorithm: None,
                id: Some(DEFAULT_ID.to_string()),
                kind: None,
                background: None,
                accessory: None,
                color: None,
                expression: None,
                shape: None,
                format: None,
                size: None,
                tenant: None,
                style_version: None,
                persist: Some(true),
                redirect: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(response.headers().contains_key(header::RETRY_AFTER));
    }

    #[test]
    fn avatar_request_debug_redacts_identity() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.identity = "user@example.com".to_string();

        let debug = format!("{request:?}");

        assert!(debug.contains("identity: \"[redacted]\""));
        assert!(!debug.contains("user@example.com"));
    }

    #[test]
    fn draw_circle_uses_wide_arithmetic_for_large_radius() {
        assert!(is_inside_circle(46_341, 0, 46_341));
        assert!(is_inside_circle(46_341, 46_341, 65_537));
        assert!(!is_inside_circle(46_341, 46_341, 46_341));
        assert!(!is_inside_circle(0, 0, -1));
    }

    #[test]
    fn overlay_reports_out_of_bounds_composition() {
        let mut canvas = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0]));
        let avatar = RgbaImage::from_pixel(8, 8, Rgba([255, 255, 255, 255]));

        assert!(overlay(&mut canvas, &avatar, 4, 4).is_ok());
        assert!(overlay(&mut canvas, &avatar, 12, 12).is_err());
    }

    #[tokio::test]
    async fn internal_error_does_not_expose_details() {
        let response = internal_error("s3 bucket hashavatar-private in eu-north-1 denied");

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = response_text(response).await;

        assert_eq!(body, INTERNAL_ERROR_MESSAGE);
        assert!(!body.contains("hashavatar-private"));
        assert!(!body.contains("eu-north-1"));
    }

    #[test]
    fn build_avatar_asset_renders_webp_with_hashavatar_1_0_1() {
        let request = test_avatar_request(AvatarRequestFormat::Webp);
        let asset = build_avatar_asset(&request).expect("webp avatar should render");

        assert_eq!(asset.content_type, "image/webp");
        assert!(asset.body.starts_with(b"RIFF"));
    }

    #[test]
    fn avatar_request_rejects_non_sha512_algorithm() {
        let error = AvatarRequest::from_query(AvatarQuery {
            algorithm: Some("blake3".to_string()),
            id: Some(DEFAULT_ID.to_string()),
            kind: None,
            background: None,
            accessory: None,
            color: None,
            expression: None,
            shape: None,
            format: None,
            size: None,
            tenant: None,
            style_version: None,
            persist: None,
            redirect: None,
        })
        .expect_err("non-sha512 algorithm should be rejected");

        assert_eq!(error, INVALID_HASH_ALGORITHM_MESSAGE);
    }

    #[test]
    fn avatar_request_rejects_non_webp_format() {
        let error = AvatarRequest::from_query(AvatarQuery {
            algorithm: None,
            id: Some(DEFAULT_ID.to_string()),
            kind: None,
            background: None,
            accessory: None,
            color: None,
            expression: None,
            shape: None,
            format: Some("svg".to_string()),
            size: None,
            tenant: None,
            style_version: None,
            persist: None,
            redirect: None,
        })
        .expect_err("non-webp format should be rejected");

        assert_eq!(error, INVALID_AVATAR_FORMAT_MESSAGE);
    }

    #[test]
    fn build_avatar_asset_supports_explicit_style_layers() {
        let base = test_avatar_request(AvatarRequestFormat::Webp);
        let base_asset = build_avatar_asset(&base).expect("base avatar should render");

        let mut request = base;
        request.accessory = AvatarAccessory::Glasses;
        request.color = AvatarColor::Gold;
        request.expression = AvatarExpression::Happy;
        request.shape = AvatarShape::Circle;

        let styled_asset = build_avatar_asset(&request).expect("styled avatar should render");

        assert_eq!(styled_asset.content_type, "image/webp");
        assert_ne!(base_asset.cache_key, styled_asset.cache_key);
        assert_ne!(base_asset.object_key, styled_asset.object_key);
        assert!(
            styled_asset
                .object_key
                .contains("/glasses/gold/happy/circle/")
        );
    }

    #[test]
    fn build_avatar_asset_normalizes_unsupported_accessory_layers() {
        let mut unsupported = test_avatar_request(AvatarRequestFormat::Webp);
        unsupported.kind = AvatarKind::CoffeeCup;
        unsupported.accessory = AvatarAccessory::Glasses;
        unsupported.color = AvatarColor::Gold;
        unsupported.expression = AvatarExpression::Happy;
        unsupported.shape = AvatarShape::Circle;

        let mut normalized = unsupported.clone();
        normalized.accessory = DEFAULT_ACCESSORY;
        normalized.expression = DEFAULT_EXPRESSION;

        let unsupported_asset =
            build_avatar_asset(&unsupported).expect("unsupported style avatar should render");
        let normalized_asset =
            build_avatar_asset(&normalized).expect("normalized style avatar should render");

        assert_eq!(unsupported_asset.cache_key, normalized_asset.cache_key);
        assert_eq!(unsupported_asset.object_key, normalized_asset.object_key);
        assert!(
            unsupported_asset
                .object_key
                .contains("/coffee-cup/themed/none/gold/default/circle/")
        );
    }

    #[test]
    fn build_avatar_asset_rejects_oversized_namespace() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.namespace_tenant = "x".repeat(MAX_NAMESPACE_COMPONENT_BYTES + 1);

        let error = match build_avatar_asset(&request) {
            Ok(_) => panic!("oversized tenant should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("tenant must be 1-64 ASCII"));
    }

    #[test]
    fn build_avatar_asset_rejects_path_like_namespace() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.namespace_tenant = "../admin".to_string();

        let error = match build_avatar_asset(&request) {
            Ok(_) => panic!("path-like tenant should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("tenant must be 1-64 ASCII"));
    }

    #[test]
    fn build_avatar_asset_rejects_oversized_identity() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.identity = "x".repeat(MAX_ID_BYTES + 1);

        let error = match build_avatar_asset(&request) {
            Ok(_) => panic!("oversized identity should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("identity must be at most 512 bytes"));
    }

    #[test]
    fn build_avatar_asset_allows_email_identity() {
        let mut request = test_avatar_request(AvatarRequestFormat::Webp);
        request.identity = "person@example.com".to_string();

        let asset = build_avatar_asset(&request).expect("email-shaped identity should render");

        assert_eq!(asset.content_type, "image/webp");
    }

    #[test]
    fn build_avatar_asset_allows_reported_identity_inputs() {
        for identity in [
            "dsdssLOLhield@hashavatar.appdsdssdasas",
            "asjkjhsajkashjL\u{00d6}OLALALALAL",
        ] {
            let mut request = test_avatar_request(AvatarRequestFormat::Webp);
            request.identity = identity.to_string();

            let asset = build_avatar_asset(&request).expect("reported identity should render");

            assert_eq!(asset.content_type, "image/webp");
        }
    }

    #[test]
    fn object_key_uses_full_sha256_digest() {
        let request = test_avatar_request(AvatarRequestFormat::Webp);
        let asset = build_avatar_asset(&request).expect("avatar should render");
        let filename = asset
            .object_key
            .rsplit('/')
            .next()
            .expect("object key filename");
        let digest = filename
            .strip_suffix(".webp")
            .expect("webp object key suffix");

        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
