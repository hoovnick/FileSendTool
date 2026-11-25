//! The HTTP side: a landing page per share and a Range-capable byte stream.
//!
//! Bound to loopback only. The sole public way in is the Cloudflare tunnel,
//! so nothing here is reachable once the tunnel is down.

use axum::{
    Router,
    body::Body,
    extract::{Path as AxPath, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use std::{
    net::SocketAddr,
    sync::{Arc, RwLock},
};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use crate::share::{Store, human_left, human_size};

// ---------------------------------------------------------------- activity log

#[derive(Debug, Clone)]
pub struct LogLine {
    pub when: chrono::DateTime<chrono::Local>,
    pub text: String,
}

pub type Log = Arc<RwLock<Vec<LogLine>>>;

pub fn new_log() -> Log {
    Arc::new(RwLock::new(Vec::new()))
}

const LOG_CAP: usize = 500;

pub fn note(log: &Log, text: impl Into<String>) {
    let mut g = log.write().unwrap();
    g.push(LogLine {
        when: chrono::Local::now(),
        text: text.into(),
    });
    let n = g.len();
    if n > LOG_CAP {
        g.drain(0..n - LOG_CAP);
    }
}

// ------------------------------------------------------------------ app state

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub log: Log,
}

/// Start the server on an OS-assigned loopback port. Returns that port.
pub fn spawn(store: Store, log: Log) -> std::io::Result<u16> {
    // Bind synchronously so the caller has the port before we hand off the socket.
    let std_listener = std::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))?;
    let port = std_listener.local_addr()?.port();
    std_listener.set_nonblocking(true)?;

    std::thread::Builder::new()
        .name("fst-http".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(async move {
                let listener =
                    tokio::net::TcpListener::from_std(std_listener).expect("adopt std listener");
                let app = router(AppState { store, log });
                if let Err(e) = axum::serve(listener, app).await {
                    eprintln!("fst: server stopped: {e}");
                }
            });
        })?;

    Ok(port)
}

fn router(state: AppState) -> Router {
    Router::new()
        // axum's `get` already answers HEAD, so adding `.head()` here would panic
        // on an overlapping method route.
        .route("/d/{token}", get(landing))
        .route("/f/{token}/{name}", get(bytes))
        .fallback(gone)
        .with_state(state)
}

// -------------------------------------------------------------------- handlers

async fn landing(State(st): State<AppState>, AxPath(token): AxPath<String>) -> Response {
    let (name, size, href, foot) = {
        let g = st.store.read().unwrap();
        let Some(share) = g.get(&token).filter(|s| s.is_live()) else {
            return gone_response();
        };

        let left = share
            .time_left()
            .map(|d| format!("Link expires in {}", human_left(d)))
            .unwrap_or_else(|| "Link does not expire".to_string());

        let limit = match share.max_downloads {
            Some(m) => {
                let n = m.saturating_sub(share.downloads);
                format!(" · {n} download{} left", if n == 1 { "" } else { "s" })
            }
            None => String::new(),
        };

        let href = format!(
            "/f/{}/{}",
            token,
            utf8_percent_encode(&share.name, SEGMENT)
        );
        (
            share.name.clone(),
            human_size(share.size),
            href,
            format!("{left}{limit}"),
        )
    };

    let page = LANDING
        .replace("__STYLE__", STYLE)
        .replace("__ARROW__", ARROW)
        .replace("__NAME__", &esc(&name))
        .replace("__SIZE__", &esc(&size))
        .replace("__HREF__", &esc(&href))
        .replace("__FOOT__", &esc(&foot));

    (
        StatusCode::OK,
        [
            ("x-robots-tag", "noindex, nofollow"),
            ("cache-control", "no-store"),
        ],
        Html(page),
    )
        .into_response()
}

async fn bytes(
    State(st): State<AppState>,
    AxPath((token, _name)): AxPath<(String, String)>,
    req: Request,
) -> Response {
    let is_head = req.method() == Method::HEAD;
    let ranged = req.headers().contains_key(header::RANGE);
    let who = client_ip(&req);
    let agent = short_agent(&req);

    // Resolve the token to a path. The URL never contributes a path component,
    // so traversal is not something that has to be filtered out.
    let (path, name, size) = {
        let g = st.store.read().unwrap();
        match g.get(&token).filter(|s| s.is_live()) {
            Some(s) => (s.path.clone(), s.name.clone(), s.size),
            None => return gone_response(),
        }
    };

    // Count only full, non-HEAD transfers: a resume or a preflight is not a download.
    if !is_head && !ranged {
        {
            let mut g = st.store.write().unwrap();
            match g.get_mut(&token) {
                // Re-checked under the write lock, so two racing requests cannot
                // both slip past the last allowed download.
                Some(s) if s.is_live() => s.downloads += 1,
                _ => return gone_response(),
            }
        }
        note(
            &st.log,
            format!("{name} · {} · started by {who} ({agent})", human_size(size)),
        );
    } else if !is_head {
        note(&st.log, format!("{name} · resumed by {who} ({agent})"));
    }

    // tower-http does the Range / 206 / Content-Range work and streams with
    // backpressure, so memory stays flat no matter how large the file is.
    // `ServeFile`'s error type is `Infallible`, so this cannot fail.
    let served = ServeFile::new(&path)
        .oneshot(req)
        .await
        .expect("ServeFile is infallible");

    let mut resp = served.map(Body::new);
    if resp.status() == StatusCode::NOT_FOUND {
        // Moved or deleted since it was shared.
        note(&st.log, format!("{name} · FILE MISSING on disk"));
        return gone_response();
    }

    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&content_disposition(&name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert("x-robots-tag", HeaderValue::from_static("noindex, nofollow"));
    resp
}

/// One response for unknown, expired, exhausted and revoked alike, so a probe
/// cannot learn whether a token was ever real.
///
/// Synchronous on purpose: callers hit this while holding a lock guard on the
/// share table, and awaiting there would make the handler futures non-`Send`.
fn gone_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        [
            ("x-robots-tag", "noindex, nofollow"),
            ("cache-control", "no-store"),
        ],
        Html(GONE.replace("__STYLE__", STYLE)),
    )
        .into_response()
}

/// Router fallback wrapper for [`gone_response`].
async fn gone() -> Response {
    gone_response()
}

// --------------------------------------------------------------------- helpers

/// Percent-encoding set for a single path segment: deliberately aggressive.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'/')
    .add(b'\\')
    .add(b'+')
    .add(b'&')
    .add(b'=')
    .add(b';')
    .add(b':')
    .add(b'@')
    .add(b'$')
    .add(b',')
    .add(b'[')
    .add(b']')
    .add(b'^')
    .add(b'|')
    .add(b'\'');

/// `attachment` with both a plain and a UTF-8 filename, so non-ASCII names survive.
fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{}",
        utf8_percent_encode(name, SEGMENT)
    )
}

fn client_ip(req: &Request) -> String {
    for h in ["cf-connecting-ip", "x-forwarded-for"] {
        if let Some(v) = req.headers().get(h).and_then(|v| v.to_str().ok()) {
            if let Some(first) = v.split(',').next() {
                let first = first.trim();
                if !first.is_empty() {
                    return first.to_string();
                }
            }
        }
    }
    "unknown".into()
}

/// Enough of the User-Agent to recognise a person, not enough to fill the log.
fn short_agent(req: &Request) -> String {
    let ua = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ua.is_empty() {
        return "no UA".into();
    }
    let platform = if ua.contains("iPhone") || ua.contains("iPad") {
        "iOS"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Mac OS X") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        "other"
    };
    let browser = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("Chrome/") {
        "Chrome"
    } else if ua.contains("Firefox/") {
        "Firefox"
    } else if ua.contains("Safari/") {
        "Safari"
    } else if ua.contains("curl") {
        "curl"
    } else {
        "app"
    };
    format!("{platform}/{browser}")
}

/// A filename is text we did not choose, going into HTML. Escape it.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ----------------------------------------------------------------------- pages

const STYLE: &str = r#"
:root{color-scheme:dark}
*{box-sizing:border-box}
body{margin:0;min-height:100vh;display:grid;place-items:center;padding:24px;
  background:#0d1117;color:#e6edf3;
  font:16px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,Helvetica,Arial,sans-serif}
.card{width:100%;max-width:420px;text-align:center}
.icon{width:56px;height:56px;border-radius:14px;background:#2f81f7;margin:0 auto 20px;
  display:grid;place-items:center}
.icon svg{width:30px;height:30px;fill:#fff;display:block}
h1{font-size:19px;font-weight:600;margin:0 0 6px;overflow-wrap:anywhere}
.size{color:#8b949e;font-size:15px;margin:0 0 28px}
a.dl{display:block;background:#2f81f7;color:#fff;text-decoration:none;
  padding:15px 20px;border-radius:10px;font-size:17px;font-weight:600}
a.dl:active{background:#1f6feb}
.foot{color:#6e7681;font-size:13px;margin:22px 0 0}
.gone p{color:#8b949e;margin:0}
"#;

const ARROW: &str = r#"<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 2 2 12h6v10h8V12h6z"/></svg>"#;

const LANDING: &str = r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow">
<title>__NAME__</title>
<meta property="og:title" content="__NAME__">
<meta property="og:description" content="__SIZE__ · ready to download">
<meta property="og:type" content="website">
<style>__STYLE__</style>
</head><body>
<div class="card">
  <div class="icon">__ARROW__</div>
  <h1>__NAME__</h1>
  <p class="size">__SIZE__</p>
  <a class="dl" href="__HREF__" download>Download</a>
  <p class="foot">__FOOT__</p>
</div>
</body></html>"#;

const GONE: &str = r#"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex,nofollow">
<title>Link unavailable</title>
<style>__STYLE__</style>
</head><body>
<div class="card gone">
  <h1>This link isn&#39;t available</h1>
  <p>It may have expired, been turned off, or never existed.</p>
</div>
</body></html>"#;
