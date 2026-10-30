//! The Desk tab's one route: `/api/desk/{*rest}`, forwarded to embarch-desk
//! on its loopback port with the desk token attached.
//!
//! The browser never talks to the desk directly. That keeps the desk's
//! token out of the page and keeps the Desk tab on this server's origin like
//! every other tab. The token rides on everything forwarded, so the proxy
//! also refuses any request that did not come from this server's own page
//! (`same_origin`): otherwise every other site the browser has open could
//! make the desk write to Linear or Harvest as the user, or queue a prompt for
//! a chat in auto mode.

use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::stream::Stream;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

const DEFAULT_PORT: u16 = 4891;

/// Where embarch-desk keeps its token: the same resolution the desk uses
/// (`EMBARCH_DESK_CONFIG_DIR`, else `$XDG_CONFIG_HOME/embarch-desk`, else
/// `~/.config/embarch-desk`). Both run as the same user on the same side.
fn token_path() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("EMBARCH_DESK_CONFIG_DIR") {
        return Some(PathBuf::from(d).join("desk-token"));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("embarch-desk").join("desk-token"))
}

fn desk_url(rest: &str) -> String {
    let port = std::env::var("EMBARCH_DESK_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT);
    format!("http://127.0.0.1:{port}/{}", rest.trim_start_matches('/'))
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    // Long enough for a tile's polish, which is one `claude -p` call.
    C.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_secs(240)).build().expect("reqwest client"))
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

/// Whether a request came from this server's own page. Browsers send
/// `Sec-Fetch-Site` to a loopback origin, and only `same-origin` passes. A
/// browser that does not send it still sends `Origin` on any write, and that
/// must name the host the request was addressed to. A request with neither is
/// let through only as a read (`GET`, `HEAD`): no browser writes without
/// `Origin`, and another origin cannot read the answer.
///
/// What this closes: a page elsewhere posting plain text, which needs no CORS
/// preflight, to `/api/desk/mailbox` or `/api/desk/tiles/{id}/approve`.
fn same_origin(method: &Method, headers: &HeaderMap) -> bool {
    if let Some(site) = headers.get("sec-fetch-site") {
        return site.as_bytes() == b"same-origin";
    }
    let Some(origin) = headers.get(header::ORIGIN) else {
        return *method == Method::GET || *method == Method::HEAD;
    };
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok());
    match (origin.to_str().ok().and_then(|o| o.strip_prefix("http://")), host) {
        (Some(o), Some(h)) => o.eq_ignore_ascii_case(h),
        _ => false,
    }
}

pub async fn proxy(method: Method, headers: HeaderMap, Path(rest): Path<String>, body: Bytes) -> Response {
    if !same_origin(&method, &headers) {
        return json_error(StatusCode::FORBIDDEN, "the desk proxy answers only this UI's own page");
    }
    let Some(token) = desk_token() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "embarch-desk is not set up on this machine — install it and run `embarch-desk service install`",
        );
    };
    forward(desk_url(&rest), &token, method, headers.get(header::CONTENT_TYPE).cloned(), body).await
}

/// One request on to the desk, with the token, the body and the body's own
/// `Content-Type`. The type is passed through, never made JSON here: the
/// desk's JSON routes then refuse a body the browser sent as something else.
async fn forward(url: String, token: &str, method: Method, content_type: Option<HeaderValue>, body: Bytes) -> Response {
    let mut req = client().request(method, url).bearer_auth(token);
    if let Some(ct) = content_type {
        req = req.header(header::CONTENT_TYPE, ct);
    }
    if !body.is_empty() {
        req = req.body(body);
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let bytes = resp.bytes().await.unwrap_or_default();
            (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
        }
        Err(e) if e.is_connect() => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "embarch-desk is not running — `embarch-desk service install` starts it, `embarch-desk service status` says why it stopped",
        ),
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &format!("embarch-desk: {e}")),
    }
}

/// How often the server asks the desk for a fresh view while a Desk tab
/// is listening. The desk refreshes its own sources on slower clocks.
const VIEW_EVERY: Duration = Duration::from_secs(10);

fn desk_token() -> Option<String> {
    token_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| t.trim().to_string()).filter(|t| !t.is_empty())
}

async fn fetch_view() -> Result<String, String> {
    let token = desk_token().ok_or("embarch-desk is not set up on this machine — install it and run `embarch-desk service install`")?;
    let resp = client().get(desk_url("view")).bearer_auth(token).send().await.map_err(|e| {
        if e.is_connect() {
            "embarch-desk is not running — `embarch-desk service install` starts it".to_string()
        } else {
            format!("embarch-desk: {e}")
        }
    })?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if status.is_success() { Ok(body) } else { Err(format!("embarch-desk: HTTP {status}: {body}")) }
}

/// `GET /api/desk-events`: the desk's view, pushed. The polling is here,
/// server-side, so the browser holds one `EventSource` and no interval
/// (decision 6) — the same rule every other live surface follows.
pub async fn events() -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = futures_util::stream::unfold(true, |first| async move {
        if !first {
            tokio::time::sleep(VIEW_EVERY).await;
        }
        let ev = match fetch_view().await {
            Ok(body) => Event::default().event("view").data(body),
            Err(msg) => Event::default().event("desk-error").data(msg),
        };
        Some((Ok(ev), false))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rest_of_the_path_lands_on_the_desks_own_route() {
        assert!(super::desk_url("tiles/w1@2026-10-22/approve").ends_with("/tiles/w1@2026-10-22/approve"));
        assert!(super::desk_url("/view").ends_with(":4891/view") || std::env::var("EMBARCH_DESK_PORT").is_ok());
    }

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn only_this_servers_own_page_gets_through_the_proxy() {
        let post = Method::POST;
        // The browser says where the request came from.
        assert!(same_origin(&post, &headers(&[("sec-fetch-site", "same-origin"), ("host", "127.0.0.1:4890")])));
        for site in ["cross-site", "same-site", "none"] {
            let h = headers(&[("sec-fetch-site", site), ("host", "127.0.0.1:4890"), ("origin", "http://127.0.0.1:4890")]);
            assert!(!same_origin(&post, &h), "{site} must be refused, whatever Origin says");
        }
        // A browser without Sec-Fetch-Site: Origin must name the host asked.
        assert!(same_origin(&post, &headers(&[("host", "127.0.0.1:4890"), ("origin", "http://127.0.0.1:4890")])));
        assert!(same_origin(&post, &headers(&[("host", "localhost:4890"), ("origin", "http://LOCALHOST:4890")])));
        assert!(!same_origin(&post, &headers(&[("host", "127.0.0.1:4890"), ("origin", "http://localhost:4890")])));
        assert!(!same_origin(&post, &headers(&[("host", "127.0.0.1:4890"), ("origin", "http://evil.example")])));
        assert!(!same_origin(&post, &headers(&[("host", "127.0.0.1:4890"), ("origin", "null")])));
        assert!(!same_origin(&post, &headers(&[("origin", "http://127.0.0.1:4890")])));
        // Neither header: a read passes, a write does not.
        assert!(same_origin(&Method::GET, &headers(&[("host", "127.0.0.1:4890")])));
        assert!(!same_origin(&post, &headers(&[("host", "127.0.0.1:4890")])));
        assert!(!same_origin(&Method::PUT, &HeaderMap::new()));
    }

    /// A stand-in desk that answers with the Content-Type and token it got.
    async fn echo_desk() -> String {
        async fn echo(headers: HeaderMap, body: Bytes) -> axum::Json<serde_json::Value> {
            let get = |k: header::HeaderName| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
            axum::Json(serde_json::json!({
                "content_type": get(header::CONTENT_TYPE),
                "auth": get(header::AUTHORIZATION),
                "body": String::from_utf8_lossy(&body),
            }))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, axum::Router::new().fallback(echo)).await.unwrap() });
        format!("http://{addr}/mailbox")
    }

    async fn seen(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn the_body_keeps_the_content_type_the_browser_sent() {
        let url = echo_desk().await;
        let body = Bytes::from_static(br#"{"kind":"focus"}"#);
        let plain = forward(url.clone(), "tok", Method::POST, Some(HeaderValue::from_static("text/plain;charset=UTF-8")), body.clone()).await;
        let got = seen(plain).await;
        assert_eq!(got["content_type"], "text/plain;charset=UTF-8", "a text/plain body must not reach the desk as JSON");
        assert_eq!((got["auth"].as_str(), got["body"].as_str()), (Some("Bearer tok"), Some(r#"{"kind":"focus"}"#)));
        let json = forward(url.clone(), "tok", Method::POST, Some(HeaderValue::from_static("application/json")), body.clone()).await;
        assert_eq!(seen(json).await["content_type"], "application/json");
        let none = forward(url, "tok", Method::POST, None, body).await;
        assert_eq!(seen(none).await["content_type"], serde_json::Value::Null);
    }
}
