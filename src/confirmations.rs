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
        Err(e) => refusal(&format!("{e:#}")),
    }
}

/// A Core refusal as its own status with its own reason: `400`, `403`, `404`,
/// `409` and `422` pass through; any other failure is a `502` with the client's text.
fn refusal(text: &str) -> Response {
    for (marker, code) in [
        ("returned 400", StatusCode::BAD_REQUEST),
        ("returned 403", StatusCode::FORBIDDEN),
        ("returned 404", StatusCode::NOT_FOUND),
        ("returned 409", StatusCode::CONFLICT),
        ("returned 422", StatusCode::UNPROCESSABLE_ENTITY),
    ] {
        if text.contains(marker) {
            return (code, reason_of(text)).into_response();
        }
    }
    (StatusCode::BAD_GATEWAY, text.to_string()).into_response()
}

/// `GET /api/test-runs/{run_id}/card` — Core's go card for a prepared run, as Core
/// sent it: `{run_id, run_hash, lease_holder, prepared_utc_ms, card, nonce,
/// expires_utc_ms}`, every agent string still `{"agent_text": ...}` for the page to
/// draw as text (decision 111). Core's refusal comes back as its own status.
pub async fn api_run_card(State(state): State<crate::AppState>, axum::extract::Path(run_id): axum::extract::Path<String>) -> Response {
    match state.core.test_run_card(&run_id).await {
        Ok(card) => (StatusCode::OK, axum::Json(card)).into_response(),
        Err(e) => refusal(&format!("{e:#}")),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct GoBody {
    pub run_hash: String,
    pub nonce: String,
}

/// `POST /api/test-runs/{run_id}/go` — the person's go, `{run_hash, nonce}` from the
/// card the page drew, relayed through the one client with the person's token.
/// **Core's refusal comes back as its own status with its own reason** (`422`
/// `mismatched`, `expired` or `replayed`; `409` already started, an input
/// unconfirmed, or the lease; `403`; `404`), never a success. The route never
/// retries: the nonce is spent by the attempt, so the page reloads the card.
pub async fn api_run_go(
    State(state): State<crate::AppState>,
    axum::extract::Path(run_id): axum::extract::Path<String>,
    axum::extract::Json(body): axum::extract::Json<GoBody>,
) -> Response {
    match state.core.test_run_go(&run_id, &body.run_hash, &body.nonce).await {
        Ok(done) => (StatusCode::OK, axum::Json(done)).into_response(),
        Err(e) => refusal(&format!("{e:#}")),
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
        /// Every go the fake Core was given, `(run_id, body)`; a go is used once.
        goes: Vec<(String, Value)>,
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
            .route(
                "/test-runs/{run_id}/card",
                get(|S(s): S<Shared>, h: axum::http::HeaderMap, P(run_id): P<String>| async move {
                    note(&s, "run-card", &h);
                    axum::Json(json!({
                        "run_id": run_id, "run_hash": "rh", "lease_holder": null, "prepared_utc_ms": 1,
                        "card": { "unconfirmed": [{ "kind": "hook", "name": { "agent_text": MARKUP }, "hash": "h9" }] },
                        "nonce": "go-1", "expires_utc_ms": 2
                    }))
                }),
            )
            .route(
                "/test-runs/{run_id}/go",
                post(|S(s): S<Shared>, h: axum::http::HeaderMap, P(run_id): P<String>, J(v): J<Value>| async move {
                    note(&s, "go", &h);
                    let mut s = s.lock().unwrap();
                    let replayed = !s.goes.is_empty();
                    s.goes.push((run_id.clone(), v));
                    if replayed {
                        return (StatusCode::UNPROCESSABLE_ENTITY, "replayed: this go was already used").into_response();
                    }
                    axum::Json(json!({ "run_id": run_id, "run_hash": "rh", "state": "started", "started_utc_ms": 3, "lease_holder": "x" }))
                        .into_response()
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
            .route("/api/test-runs/{run_id}/card", get(api_run_card))
            .route("/api/test-runs/{run_id}/go", post(api_run_go))
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

    /// The go is given once with the person's token and the card's own run id, hash
    /// and nonce; the second is Core's refusal, with Core's status and words, and the
    /// route never sent it again by itself.
    #[tokio::test]
    async fn a_go_is_given_once_and_a_second_is_refused_with_cores_reason() {
        let stub = Shared::default();
        let ui = ui_over(&serve(stub.clone()).await).await;
        let card: Value = http().get(format!("{ui}/api/test-runs/r1/card")).send().await.unwrap().json().await.unwrap();
        assert_eq!(card["card"]["unconfirmed"][0]["name"]["agent_text"], MARKUP, "an agent string stays data");
        let body = json!({ "run_hash": card["run_hash"], "nonce": card["nonce"] });
        let first = http().post(format!("{ui}/api/test-runs/r1/go")).json(&body).send().await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        assert_eq!(first.json::<Value>().await.unwrap()["state"], "started");
        let second = http().post(format!("{ui}/api/test-runs/r1/go")).json(&body).send().await.unwrap();
        assert_eq!(second.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(second.text().await.unwrap(), "replayed: this go was already used");
        let s = stub.lock().unwrap();
        let sent = json!({ "run_id": "r1", "run_hash": "rh", "nonce": "go-1" });
        assert_eq!(s.goes, vec![("r1".to_string(), sent.clone()), ("r1".to_string(), sent)], "one attempt per click, no retry");
        assert!(s.seen.iter().all(|(_, a)| a == "Bearer person-x"), "{:?}", s.seen);
    }

    /// The go button is disabled while the card lists an unconfirmed input and names
    /// it; the page draws every agent string as text.
    #[test]
    fn the_page_disables_the_go_on_an_unconfirmed_input_and_names_it() {
        let js = include_str!("../assets/confirm.js");
        assert!(js.contains("unconfirmed") && js.contains("goBtn.disabled"));
        assert!(js.contains("agent_text"));
    }
}
