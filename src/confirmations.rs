//! The Confirm tab's two routes: what waits for the engineer, and the engineer's
//! confirmation (`embarch-ui` decision 110).
//!
//! A confirmation is the person's act, made where the agent cannot reach, on
//! exactly what Core recorded. Both routes go through the UI's one Core client,
//! which sends the **person's** token (decision 103), because Core refuses the
//! agent's token on the card and on the confirmation. The list joins each pending
//! item with its card and nonce; the page draws every string in them as text.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

/// `GET /api/confirmations` — `{items: [{hash, kind, name, card, confirmation,
/// nonce, expires_utc_ms}]}`, each pending definition with Core's own card for it.
/// An item whose card could not be read is `{hash, kind, name, error}` with Core's
/// reason, so one refusal does not hide the rest. A list that cannot be read is a
/// `502` with the client's text.
pub async fn api_confirmations(State(state): State<crate::AppState>) -> Response {
    let pending = match state.core.pending_confirmations().await {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    let mut items = Vec::new();
    for p in pending["pending"].as_array().cloned().unwrap_or_default() {
        let hash = p["hash"].as_str().unwrap_or_default().to_string();
        let mut item = json!({ "hash": hash, "kind": p["kind"], "name": p["name"] });
        match state.core.confirmation_card(&hash).await {
            Ok(card) => {
                item["card"] = card["card"].clone();
                item["confirmation"] = card["confirmation"].clone();
                item["nonce"] = card["nonce"].clone();
                item["expires_utc_ms"] = card["expires_utc_ms"].clone();
            }
            Err(e) => item["error"] = Value::String(reason_of(&format!("{e:#}"))),
        }
        items.push(item);
    }
    (StatusCode::OK, axum::Json(json!({ "items": items }))).into_response()
}

#[derive(Debug, serde::Deserialize)]
pub struct ConfirmBody {
    pub hash: String,
    pub nonce: String,
}

/// `POST /api/confirmations` — one confirmation, `{hash, nonce}`, relayed with
/// the surface `ui`. **Core's refusal comes back as its own status with its own
/// reason** (`403` a nonce unknown, spent, expired or for another card; `409` it
/// needs none; `404`; `400`), never a success and never a generic failure. The
/// nonce is spent by the attempt, so the page reloads the card after a refusal.
pub async fn api_confirm(State(state): State<crate::AppState>, axum::extract::Json(body): axum::extract::Json<ConfirmBody>) -> Response {
    match state.core.confirm(&body.hash, &body.nonce, Some("ui")).await {
        Ok(done) => (StatusCode::CREATED, axum::Json(done)).into_response(),
        Err(e) => {
            let text = format!("{e:#}");
            for (marker, code) in [
                ("returned 400", StatusCode::BAD_REQUEST),
                ("returned 403", StatusCode::FORBIDDEN),
                ("returned 404", StatusCode::NOT_FOUND),
                ("returned 409", StatusCode::CONFLICT),
            ] {
                if text.contains(marker) {
                    return (code, reason_of(&text)).into_response();
                }
            }
            (StatusCode::BAD_GATEWAY, text).into_response()
        }
    }
}

/// Core's own words, without the client's `embarch-core returned {status}: ` prefix.
fn reason_of(text: &str) -> String {
    match text.split_once(": ") {
        Some((head, rest)) if head.contains("returned") => rest.trim().to_string(),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Json as J, Path as P, State as S};
    use axum::routing::{get, post};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Stub {
        /// `(path, Authorization)` of every request.
        seen: Vec<(String, String)>,
        confirmed: Vec<Value>,
        refuse: Option<(StatusCode, &'static str)>,
    }
    type Shared = Arc<Mutex<Stub>>;

    const MARKUP: &str = "<img src=x onerror=alert(1)>";

    async fn serve(stub: Shared) -> String {
        fn note(s: &Shared, path: &str, h: &axum::http::HeaderMap) {
            let auth = h.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
            s.lock().unwrap().seen.push((path.to_string(), auth));
        }
        let app = axum::Router::new()
            .route(
                "/confirmations/pending",
                get(|S(s): S<Shared>, h: axum::http::HeaderMap| async move {
                    note(&s, "pending", &h);
                    axum::Json(json!({ "pending": [{ "hash": "h1", "kind": "test", "name": MARKUP }] }))
                }),
            )
            .route(
                "/confirmations/{hash}/card",
                get(|S(s): S<Shared>, h: axum::http::HeaderMap, P(hash): P<String>| async move {
                    note(&s, "card", &h);
                    axum::Json(json!({
                        "card": { "hash": hash, "kind": "test", "definition": { "title": MARKUP }, "diff": [] },
                        "confirmation": "unconfirmed", "nonce": "n-1", "expires_utc_ms": 1
                    }))
                }),
            )
            .route(
                "/confirmations",
                post(|S(s): S<Shared>, h: axum::http::HeaderMap, J(v): J<Value>| async move {
                    note(&s, "confirm", &h);
                    let mut s = s.lock().unwrap();
                    if let Some((code, why)) = s.refuse {
                        return (code, why).into_response();
                    }
                    s.confirmed.push(v);
                    (StatusCode::CREATED, axum::Json(json!({ "entry": {}, "hash": "e" }))).into_response()
                }),
            )
            .fallback(|| async { StatusCode::NOT_FOUND })
            .with_state(stub);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    async fn ui_over(core_url: &str) -> String {
        let cfg: embarch_core_client::CoreConfig =
            toml::from_str(&format!("base_url = \"{core_url}\"\nperson_token = \"person-x\"\n")).unwrap();
        let core = Arc::new(crate::config::core_client(&cfg).unwrap());
        let state = crate::AppState::headless(core, std::env::temp_dir(), None).await;
        let app = axum::Router::new()
            .route("/api/confirmations", get(api_confirmations).post(api_confirm))
            .with_state(state);
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn http() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn one_input_is_confirmed_with_the_person_token_the_nonce_and_surface_ui() {
        let stub = Shared::default();
        let ui = ui_over(&serve(stub.clone()).await).await;
        let list: Value = http().get(format!("{ui}/api/confirmations")).send().await.unwrap().json().await.unwrap();
        let item = &list["items"][0];
        assert_eq!(item["hash"], "h1");
        let nonce = item["nonce"].as_str().unwrap();
        let resp = http().post(format!("{ui}/api/confirmations")).json(&json!({ "hash": "h1", "nonce": nonce })).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let s = stub.lock().unwrap();
        assert_eq!(s.confirmed, vec![json!({ "hash": "h1", "nonce": "n-1", "surface": "ui" })]);
        assert!(s.seen.len() >= 3 && s.seen.iter().all(|(_, a)| a == "Bearer person-x"), "{:?}", s.seen);
    }

    /// The list carries the agent's string untouched, for the page to draw as text.
    #[tokio::test]
    async fn a_test_name_holding_markup_is_served_as_data_and_drawn_as_text() {
        let ui = ui_over(&serve(Shared::default()).await).await;
        let list: Value = http().get(format!("{ui}/api/confirmations")).send().await.unwrap().json().await.unwrap();
        assert_eq!(list["items"][0]["name"], MARKUP);
        assert_eq!(list["items"][0]["card"]["definition"]["title"], MARKUP);
        // The page draws with textContent and never assigns markup.
        let js = include_str!("../assets/confirm.js");
        assert!(js.contains("textContent"));
        for banned in ["innerHTML", "outerHTML", "insertAdjacentHTML", "document.write"] {
            assert!(!js.contains(banned), "confirm.js uses {banned}");
        }
    }

    #[tokio::test]
    async fn a_refused_confirmation_comes_back_with_cores_reason() {
        let stub = Shared::default();
        stub.lock().unwrap().refuse = Some((StatusCode::FORBIDDEN, "nonce expired"));
        let ui = ui_over(&serve(stub).await).await;
        let resp = http().post(format!("{ui}/api/confirmations")).json(&json!({ "hash": "h1", "nonce": "old" })).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(resp.text().await.unwrap(), "nonce expired");
    }
}
