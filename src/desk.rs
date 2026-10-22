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
use axum::response::{IntoResponse, Response};
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
    let token = token_path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| t.trim().to_string());
    let Some(token) = token.filter(|t| !t.is_empty()) else {
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

#[cfg(test)]
mod tests {
    #[test]
    fn the_rest_of_the_path_lands_on_the_desks_own_route() {
        assert!(super::desk_url("tiles/w1@2026-10-22/approve").ends_with("/tiles/w1@2026-10-22/approve"));
        assert!(super::desk_url("/view").ends_with(":4891/view") || std::env::var("EMBARCH_DESK_PORT").is_ok());
    }
}
