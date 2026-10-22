//! The Desk tab's one route: `/api/desk/{*rest}`, forwarded to embarch-desk
//! on its loopback port with the desk token attached.
//!
//! The browser never talks to the desk directly. That keeps the desk's
//! token out of the page — and so keeps every other site the browser has open
//! from making the desk post to Slack or Harvest as the user — and keeps the
//! Desk tab on this server's origin like every other tab.

use axum::body::Bytes;
use axum::extract::Path;
use axum::http::{header, Method, StatusCode};
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

pub async fn proxy(method: Method, Path(rest): Path<String>, body: Bytes) -> Response {
    let Some(token) = desk_token() else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "embarch-desk is not set up on this machine — install it and run `embarch-desk service install`",
        );
    };
    let mut req = client().request(method, desk_url(&rest)).bearer_auth(token);
    if !body.is_empty() {
        req = req.header(header::CONTENT_TYPE, "application/json").body(body);
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
    #[test]
    fn the_rest_of_the_path_lands_on_the_desks_own_route() {
        assert!(super::desk_url("tiles/w1@2026-10-22/approve").ends_with("/tiles/w1@2026-10-22/approve"));
        assert!(super::desk_url("/view").ends_with(":4891/view") || std::env::var("EMBARCH_DESK_PORT").is_ok());
    }
}
