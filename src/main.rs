use std::net::SocketAddr;
use std::str::FromStr;

use hashavatar::{
    AvatarBackground, AvatarKind, AvatarOptions, AvatarOutputFormat, AvatarSpec,
    encode_avatar_for_id, render_avatar_svg_for_id,
};
use axum::extract::{Path, Query};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const DEFAULT_HOST: &str = "0.0.0.0";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_ID: &str = "demo@example.com";
const SITE_NAME: &str = "hashavatar.app";
const MIN_SIZE: u32 = 64;
const MAX_SIZE: u32 = 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let host = std::env::var("PUBLIC_WEBSITE_HOST").unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let port = std::env::var("PORT")
        .ok()
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let address: SocketAddr = format!("{host}:{port}").parse()?;

    let app = Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/v1/avatar", get(query_avatar))
        .route("/avatar/{kind}/{identity}/{format}", get(path_avatar));

    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("{SITE_NAME} listening on http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<String> {
    Html(render_index_html())
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json()))
}

async fn query_avatar(Query(query): Query<AvatarQuery>) -> Response {
    serve_avatar(
        query.id.as_deref().unwrap_or(DEFAULT_ID),
        query
            .kind
            .as_deref()
            .and_then(|raw| AvatarKind::from_str(raw).ok())
            .unwrap_or(AvatarKind::Cat),
        query
            .background
            .as_deref()
            .and_then(|raw| AvatarBackground::from_str(raw).ok())
            .unwrap_or(AvatarBackground::Themed),
        query
            .format
            .as_deref()
            .and_then(|raw| AvatarRequestFormat::from_str(raw).ok())
            .unwrap_or(AvatarRequestFormat::Webp),
        query.size.unwrap_or(256),
    )
}

async fn path_avatar(Path(path): Path<PathAvatar>) -> Response {
    let kind = match AvatarKind::from_str(&path.kind) {
        Ok(kind) => kind,
        Err(_) => return bad_request("unsupported avatar kind"),
    };
    let format = match AvatarRequestFormat::from_str(&path.format) {
        Ok(format) => format,
        Err(_) => return bad_request("unsupported avatar format"),
    };

    serve_avatar(&path.identity, kind, AvatarBackground::Themed, format, 256)
}

fn serve_avatar(
    identity: &str,
    kind: AvatarKind,
    background: AvatarBackground,
    format: AvatarRequestFormat,
    size: u32,
) -> Response {
    let normalized_id = identity.trim();
    if normalized_id.is_empty() {
        return bad_request("missing identity");
    }
    if !(MIN_SIZE..=MAX_SIZE).contains(&size) {
        return bad_request("size must be between 64 and 1024");
    }

    let spec = AvatarSpec::new(size, size, 0);
    let options = AvatarOptions::new(kind, background);
    let cache_key = format!("{normalized_id}:{kind}:{background}:{format}:{size}");
    let etag = etag_for(&cache_key);

    let mut headers = cache_headers(&etag);

    match format {
        AvatarRequestFormat::Webp => {
            match encode_avatar_for_id(spec, normalized_id, AvatarOutputFormat::WebP, options) {
                Ok(bytes) => {
                    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/webp"));
                    (StatusCode::OK, headers, bytes).into_response()
                }
                Err(error) => internal_error(error),
            }
        }
        AvatarRequestFormat::Png => {
            match encode_avatar_for_id(spec, normalized_id, AvatarOutputFormat::Png, options) {
                Ok(bytes) => {
                    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
                    (StatusCode::OK, headers, bytes).into_response()
                }
                Err(error) => internal_error(error),
            }
        }
        AvatarRequestFormat::Svg => {
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/svg+xml"),
            );
            let svg = render_avatar_svg_for_id(spec, normalized_id, options);
            (StatusCode::OK, headers, svg).into_response()
        }
    }
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
        HeaderName::cloudflare_cache_status_hint(),
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

fn render_index_html() -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>hashavatar.app</title>
  <style>
    :root {{
      --bg: #fbf6ee;
      --ink: #1f2933;
      --muted: #52606d;
      --line: rgba(31, 41, 51, 0.08);
      --accent: #d97a42;
      font-family: "IBM Plex Sans", "Segoe UI", sans-serif;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      min-height: 100vh;
      background:
        radial-gradient(circle at top left, rgba(255, 214, 170, 0.95), transparent 26%),
        radial-gradient(circle at bottom right, rgba(217, 122, 66, 0.18), transparent 30%),
        linear-gradient(135deg, #fbf6ee, #f2ece4);
      color: var(--ink);
      padding: 32px 20px;
    }}
    main {{
      width: min(1120px, 100%);
      margin: 0 auto;
      background: rgba(255,255,255,0.84);
      border: 1px solid var(--line);
      border-radius: 28px;
      box-shadow: 0 24px 70px rgba(75, 48, 25, 0.14);
      overflow: hidden;
    }}
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
    .preview {{
      display: grid;
      place-items: center;
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
    pre {{
      margin: 0;
      padding: 16px;
      background: #fff;
      border: 1px solid var(--line);
      border-radius: 18px;
      overflow: auto;
      font-size: 0.94rem;
    }}
    code {{ font-family: "IBM Plex Mono", monospace; }}
    .examples {{
      display: grid;
      gap: 14px;
    }}
    @media (max-width: 860px) {{
      .hero {{ grid-template-columns: 1fr; }}
      .copy {{ border-right: 0; border-bottom: 1px solid var(--line); }}
    }}
  </style>
</head>
<body>
  <main>
    <section class="hero">
      <div class="copy">
        <div class="eyebrow">hashavatar.app</div>
        <h1>Deterministic Avatars From A URL</h1>
        <p>
          hashavatar.app exposes the procedural avatar engine over HTTP with cache-friendly responses.
          Every avatar URL is deterministic, so Cloudflare can cache aggressively and shield the origin.
        </p>
        <div class="examples">
          <pre><code>/v1/avatar?id={id}&kind=robot&background=white&format=webp&size=256</code></pre>
          <pre><code>/avatar/cat/{id}/svg</code></pre>
          <pre><code>/avatar/fox/{id}/png</code></pre>
        </div>
      </div>
      <div class="preview">
        <div class="panel">
          <img src="/v1/avatar?id={id}&kind=alien&background=themed&format=webp&size=320" alt="Alien avatar preview" />
        </div>
        <p>Long-cache avatar responses with Cloudflare-compatible headers for hashavatar.app.</p>
      </div>
    </section>
  </main>
</body>
</html>"#,
        id = DEFAULT_ID
    )
}

#[derive(Debug, Deserialize)]
struct AvatarQuery {
    id: Option<String>,
    kind: Option<String>,
    background: Option<String>,
    format: Option<String>,
    size: Option<u32>,
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

struct HeaderName;

impl HeaderName {
    fn cdn_cache_control() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("cdn-cache-control")
    }

    fn cloudflare_cache_status_hint() -> axum::http::HeaderName {
        axum::http::HeaderName::from_static("cloudflare-cdn-cache-control")
    }
}

fn serde_json() -> serde_json::Value {
    serde_json::json!({
        "status": "ok",
        "service": "hashavatar-api"
    })
}
