use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3::config::Builder as S3ConfigBuilder;
use aws_sdk_s3::presigning::PresigningConfig;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use hashavatar::{
    AvatarBackground, AvatarKind, AvatarOptions, AvatarOutputFormat, AvatarSpec,
    encode_avatar_for_id, render_avatar_svg_for_id,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_ID: &str = "demo@example.com";
const SITE_NAME: &str = "hashavatar.app";
const SITE_URL: &str = "https://hashavatar.app";
const REPOSITORY_URL: &str = "https://repoheim.eu/valkyoth/hashavatar-api";
const CRATE_URL: &str = "https://crates.io/crates/hashavatar/";
const MIN_SIZE: u32 = 64;
const MAX_SIZE: u32 = 1024;

#[derive(Clone)]
struct AppState {
    storage: Option<Arc<S3Storage>>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = std::env::var("PUBLIC_WEBSITE_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let port = std::env::var("PORT")
        .ok()
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let address: SocketAddr = format!("{host}:{port}").parse()?;

    let state = AppState {
        storage: S3Storage::from_env().await?.map(Arc::new),
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/help", get(help_page))
        .route("/terms", get(terms_page))
        .route("/privacy", get(privacy_page))
        .route("/robots.txt", get(robots_txt))
        .route("/sitemap.xml", get(sitemap_xml))
        .route("/healthz", get(healthz))
        .route("/v1/avatar", get(query_avatar))
        .route("/v1/avatar/link", get(query_avatar_link))
        .route("/avatar/{kind}/{identity}/{format}", get(path_avatar))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("{SITE_NAME} listening on http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<String> {
    Html(render_index_html())
}

async fn help_page() -> Html<String> {
    Html(render_help_html())
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

async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "ok",
            "service": "hashavatar-api",
            "s3_enabled": state.storage.is_some()
        })),
    )
}

async fn query_avatar(
    State(state): State<AppState>,
    Query(query): Query<AvatarQuery>,
) -> Response {
    let request = match AvatarRequest::from_query(query) {
        Ok(request) => request,
        Err(message) => return bad_request(&message),
    };

    serve_avatar(state, request).await
}

async fn query_avatar_link(
    State(state): State<AppState>,
    Query(query): Query<AvatarQuery>,
) -> Response {
    let request = match AvatarRequest::from_query(query) {
        Ok(request) => request,
        Err(message) => return bad_request(&message),
    };

    serve_avatar_link(state, request).await
}

async fn path_avatar(
    State(state): State<AppState>,
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
        kind,
        background: AvatarBackground::Themed,
        format,
        size: 256,
        persist: false,
        redirect: false,
    };

    serve_avatar(state, request).await
}

async fn serve_avatar(state: AppState, request: AvatarRequest) -> Response {
    let asset = match build_avatar_asset(&request) {
        Ok(asset) => asset,
        Err(message) => return bad_request(&message),
    };

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

        match storage.store_and_sign(&asset).await {
            Ok(signed) => {
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
                    return Redirect::temporary(&signed.signed_url).into_response();
                }
            }
            Err(error) => return internal_error(error),
        }
    }

    (StatusCode::OK, headers, asset.body).into_response()
}

async fn serve_avatar_link(state: AppState, request: AvatarRequest) -> Response {
    let storage = match state.storage.as_ref() {
        Some(storage) => storage,
        None => return bad_request("S3 storage is not configured on this server"),
    };

    let asset = match build_avatar_asset(&request) {
        Ok(asset) => asset,
        Err(message) => return bad_request(&message),
    };

    match storage.store_and_sign(&asset).await {
        Ok(signed) => (
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
        Err(error) => internal_error(error),
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

    let spec = AvatarSpec::new(request.size, request.size, 0);
    let options = AvatarOptions::new(request.kind, request.background);
    let cache_key = format!(
        "{}:{}:{}:{}:{}",
        identity, request.kind, request.background, request.format, request.size
    );

    let (body, content_type) = match request.format {
        AvatarRequestFormat::Webp => (
            encode_avatar_for_id(spec, identity, AvatarOutputFormat::WebP, options)
                .map_err(|error| format!("avatar generation failed: {error}"))?,
            "image/webp",
        ),
        AvatarRequestFormat::Png => (
            encode_avatar_for_id(spec, identity, AvatarOutputFormat::Png, options)
                .map_err(|error| format!("avatar generation failed: {error}"))?,
            "image/png",
        ),
        AvatarRequestFormat::Svg => (
            render_avatar_svg_for_id(spec, identity, options).into_bytes(),
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
            "{}:{}:{}:{}:{}",
            identity, request.kind, request.background, request.format, request.size
        )
        .as_bytes(),
    );
    let mut encoded = String::with_capacity(20);
    for byte in &digest[..10] {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!(
        "{}/{}/{}/{}.{}",
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
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("avatar generation failed: {error}"),
    )
        .into_response()
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

fn render_meta_tags(title: &str, description: &str, path: &str) -> String {
    let canonical = if path == "/" {
        format!("{SITE_URL}/")
    } else {
        format!("{SITE_URL}{path}")
    };
    let preview_image = format!(
        "{site}/v1/avatar?id=hashavatar.app&kind=monster&background=themed&format=png&size=512",
        site = SITE_URL
    );
    let full_title = format!("{title} · {SITE_NAME}");

    format!(
        r#"<title>{title}</title>
  <meta name="description" content="{description}" />
  <meta name="robots" content="index,follow,max-image-preview:large,max-snippet:-1,max-video-preview:-1" />
  <link rel="canonical" href="{canonical}" />
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
  <meta name="twitter:image" content="{image}" />"#,
        title = escape_html_attribute(&full_title),
        description = escape_html_attribute(description),
        canonical = escape_html_attribute(&canonical),
        image = escape_html_attribute(&preview_image),
        site_name = escape_html_attribute(SITE_NAME),
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
    let description = "Deterministic procedural avatars for emails, usernames, and internal ids. Generate cat, dog, robot, fox, alien, and monster avatars as WebP, PNG, or SVG.";
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
      grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
      gap: 14px;
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
      .field-grid, .example-grid {{ grid-template-columns: 1fr; }}
    }}
  </style>
</head>
<body>
  <main>
    <div class="site-nav">
      <a class="brand" href="/">hashavatar.app</a>
      <div class="nav-links">
        <a href="/help">Help</a>
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

        <div class="generator">
          <div class="field-grid full">
            <div>
              <label for="identity">Identity</label>
              <input id="identity" type="text" value="{id}" placeholder="you@example.com" spellcheck="false" autocomplete="off" />
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="kind">Avatar Type</label>
              <select id="kind">
                <option value="cat">Cat</option>
                <option value="dog">Dog</option>
                <option value="robot">Robot</option>
                <option value="fox">Fox</option>
                <option value="alien" selected>Alien</option>
                <option value="monster">Monster</option>
              </select>
            </div>
            <div>
              <label for="background">Background</label>
              <select id="background">
                <option value="themed" selected>Themed</option>
                <option value="white">White</option>
              </select>
            </div>
          </div>

          <div class="field-grid">
            <div>
              <label for="format">Format</label>
              <select id="format">
                <option value="webp" selected>WebP</option>
                <option value="png">PNG</option>
                <option value="svg">SVG</option>
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
            <a id="download-button" class="button-link" href="/v1/avatar?id={id}&kind=alien&background=themed&format=webp&size=256" download="hashavatar.webp">Download</a>
            <a id="open-button" class="button-link secondary" href="/v1/avatar?id={id}&kind=alien&background=themed&format=webp&size=256" target="_blank" rel="noreferrer">Open Raw</a>
          </div>

          <div class="url-panel">
            <div class="url-label">Direct URL</div>
            <div id="avatar-url" class="url-text"></div>
          </div>

          <div class="url-panel">
            <div class="url-label">Signed Storage Link</div>
            <div id="signed-url" class="url-text">Enable S3 configuration on the server to use signed links.</div>
          </div>
        </div>
      </div>

      <div class="preview">
        <div class="panel">
          <img id="avatar-preview" src="/v1/avatar?id={id}&kind=alien&background=themed&format=webp&size=256" alt="Generated avatar preview" />
        </div>
        <div class="preview-meta">
          <div><strong>API:</strong> <span id="api-mode">/v1/avatar</span></div>
          <div><strong>Storage:</strong> optional S3 persistence with presigned links via <code>/v1/avatar/link</code></div>
          <div><strong>Cache:</strong> Cloudflare-friendly long cache headers</div>
          <div><strong>Tip:</strong> Every URL is deterministic, so you can embed it directly in your app.</div>
        </div>

        <div class="examples" style="width:100%;">
          <div class="example-grid">
            <button class="example-card" data-id="alice@example.com" data-kind="cat" data-background="themed" data-format="webp" data-size="256">
              <img src="/v1/avatar?id=alice@example.com&kind=cat&background=themed&format=webp&size=160" alt="Cat preset" />
              <div class="example-title">Cat preset</div>
            </button>
            <button class="example-card" data-id="barkley@hashavatar.app" data-kind="dog" data-background="white" data-format="webp" data-size="256">
              <img src="/v1/avatar?id=barkley@hashavatar.app&kind=dog&background=white&format=webp&size=160" alt="Dog preset" />
              <div class="example-title">Dog preset</div>
            </button>
            <button class="example-card" data-id="buildbot-42" data-kind="robot" data-background="white" data-format="png" data-size="256">
              <img src="/v1/avatar?id=buildbot-42&kind=robot&background=white&format=webp&size=160" alt="Robot preset" />
              <div class="example-title">Robot preset</div>
            </button>
            <button class="example-card" data-id="ember-forest" data-kind="fox" data-background="themed" data-format="webp" data-size="256">
              <img src="/v1/avatar?id=ember-forest&kind=fox&background=themed&format=webp&size=160" alt="Fox preset" />
              <div class="example-title">Fox preset</div>
            </button>
            <button class="example-card" data-id="space-user" data-kind="alien" data-background="themed" data-format="svg" data-size="320">
              <img src="/v1/avatar?id=space-user&kind=alien&background=themed&format=webp&size=160" alt="Alien preset" />
              <div class="example-title">Alien preset</div>
            </button>
            <button class="example-card" data-id="cryptid-lab" data-kind="monster" data-background="themed" data-format="webp" data-size="512">
              <img src="/v1/avatar?id=cryptid-lab&kind=monster&background=themed&format=webp&size=160" alt="Monster preset" />
              <div class="example-title">Monster preset</div>
            </button>
          </div>
        </div>
      </div>
    </section>
    {footer}
  </main>
  <script>
    const identityEl = document.getElementById("identity");
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

    function currentIdentity() {{
      return identityEl.value.trim() || "{id}";
    }}

    function currentUrl() {{
      const query = new URLSearchParams({{
        id: currentIdentity(),
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
      downloadButton.setAttribute("download", `hashavatar-${{kindEl.value}}.${{formatEl.value}}`);
      openButton.href = url;
      updateSignedUrl();
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

    [identityEl, kindEl, backgroundEl, formatEl, sizeEl].forEach((el) => {{
      el.addEventListener("input", refresh);
      el.addEventListener("change", refresh);
    }});

    document.querySelectorAll(".example-card").forEach((card) => {{
      card.addEventListener("click", () => {{
        identityEl.value = card.dataset.id;
        kindEl.value = card.dataset.kind;
        backgroundEl.value = card.dataset.background;
        formatEl.value = card.dataset.format;
        sizeEl.value = card.dataset.size;
        refresh();
      }});
    }});

    refresh();
  </script>
</body>
</html>"#,
        id = DEFAULT_ID,
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
    <pre><code>https://{site}/v1/avatar?id=alice@example.com&amp;kind=robot&amp;background=white&amp;format=webp&amp;size=256</code></pre>
  </section>
  <section class="card">
    <h2>Path Style URL</h2>
    <p>Use the path form if you prefer cleaner embed URLs.</p>
    <pre><code>https://{site}/avatar/fox/alice@example.com/svg</code></pre>
  </section>
  <section class="card">
    <h2>HTML Example</h2>
    <pre><code>&lt;img
  src="https://{site}/v1/avatar?id=alice@example.com&amp;kind=monster&amp;background=themed&amp;format=webp&amp;size=256"
  alt="Alice avatar"
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
    <li><code>kind</code>: <code>cat</code>, <code>dog</code>, <code>robot</code>, <code>fox</code>, <code>alien</code>, or <code>monster</code></li>
    <li><code>background</code>: <code>themed</code> or <code>white</code></li>
    <li><code>format</code>: <code>webp</code>, <code>png</code>, or <code>svg</code></li>
    <li><code>size</code>: from <code>64</code> up to <code>1024</code></li>
  </ul>
</section>
<section class="card">
  <h2>Signed Storage Links</h2>
  <p>If this deployment has object storage configured, request a presigned storage link from <code>/v1/avatar/link</code>. That endpoint stores the generated object and returns JSON with the signed URL and object key.</p>
  <pre><code>GET https://{site}/v1/avatar/link?id=alice@example.com&amp;kind=robot&amp;background=white&amp;format=webp&amp;size=256</code></pre>
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
  <p>You can inspect the implementation in the public <a class="inline-link" href="https://repoheim.eu/valkyoth/hashavatar-api" target="_blank" rel="noreferrer">API repository</a> and the reusable avatar renderer in the <a class="inline-link" href="https://crates.io/crates/hashavatar/" target="_blank" rel="noreferrer">Rust crate</a>.</p>
</section>
"#,
    )
}

#[derive(Debug, Deserialize)]
struct AvatarQuery {
    id: Option<String>,
    kind: Option<String>,
    background: Option<String>,
    format: Option<String>,
    size: Option<u32>,
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
    Svg,
}

impl AvatarRequestFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Webp => "webp",
            Self::Png => "png",
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
            "svg" => Ok(Self::Svg),
            _ => Err("unsupported avatar format"),
        }
    }
}

#[derive(Debug)]
struct AvatarRequest {
    identity: String,
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

        let region = std::env::var("HASHAVATAR_S3_REGION").unwrap_or_else(|_| "us-east-1".to_string());
        let endpoint = std::env::var("HASHAVATAR_S3_ENDPOINT").ok();
        let force_path_style = std::env::var("HASHAVATAR_S3_PATH_STYLE")
            .ok()
            .map(|raw| matches!(raw.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false);
        let prefix = std::env::var("HASHAVATAR_S3_PREFIX").unwrap_or_else(|_| "avatars".to_string());
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
    ) -> Result<SignedStorageObject, Box<dyn std::error::Error>> {
        let key = format!("{}/{}", self.prefix.trim_matches('/'), asset.object_key);
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(asset.body.clone()))
            .content_type(asset.content_type)
            .cache_control("public, max-age=31536000, immutable")
            .send()
            .await?;

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
