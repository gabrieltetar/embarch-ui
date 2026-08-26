//! embarch-ui: one consolidated human-facing UI for the EmbArch suite.
//!
//! Milestone-1 status (embarch-doc/embarch-ui/milestone-1.md): §4.1–4.7
//! done — the app shell is live against the reviewed mockups, the
//! Dashboard/Topology tabs render real data from `embarch-core`, the
//! Enroll tab submits real enrollments, the Study Designer tab builds and
//! runs a `Study`, and the Debug tab live-tails Core's own log — entirely
//! through `embarch-core-client` (design.md §3 decision 5's amendment: no
//! in-process hardware access, and decision 7: never a direct logfile
//! read). VS Code extension/retirement land in §4.8 onward. See
//! embarch-doc/embarch-ui/design.md for the full architecture.

mod config;
mod logs;
mod snapshot;
mod study_designer;
mod trace;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Json};
use axum::routing::{get, post};
use axum::Router;
use embarch_core_client::CoreClient;
use futures_util::stream::Stream;
use serde::Deserialize;
use snapshot::Snapshot;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use study_designer::StudyDesigner;
use tokio::sync::{watch, Notify};

/// Binds loopback-only by default — same reasoning as `embarch-topology`'s
/// own local-UI precedent (embarch-doc/embarch-topology/design.md §3
/// decision 5) and `embarch-core/design.md` §3 decision 6's amendment: no
/// TLS, no reason to expose past localhost for a tool one engineer runs on
/// their own machine.
const BIND_ADDR: &str = "127.0.0.1";
const BIND_PORT: u16 = 4890;

/// `EMBARCH_UI_HOST`/`EMBARCH_UI_PORT` override the two constants above.
///
/// Added because the VS Code launcher extension already exposed `host`/`port`
/// settings and built the browser URL it opens from them, while this binary
/// hardcoded `127.0.0.1:4890` and the extension passed neither through — so
/// changing that setting opened a URL nothing was listening on. The
/// extension now forwards both as env vars (the same channel it already used
/// for `EMBARCH_UI_CONFIG`), rather than growing a CLI flag surface this
/// binary otherwise has none of.
///
/// An unparseable value falls back to the default rather than refusing to
/// start: a typo'd port shouldn't leave an engineer with no UI at all, and
/// the address actually bound is logged on every start either way.
fn bind_address() -> String {
    let host = std::env::var("EMBARCH_UI_HOST").unwrap_or_else(|_| BIND_ADDR.to_string());
    let port = std::env::var("EMBARCH_UI_PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(BIND_PORT);
    format!("{host}:{port}")
}

/// How often the background task re-polls embarch-core. Every connected
/// browser tab shares this one poll via the `watch` channel below — opening
/// a second tab doesn't double the load on Core.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

const INDEX_HTML: &str = include_str!("../assets/index.html");
const STYLE_CSS: &str = include_str!("../assets/style.css");
const APP_JS: &str = include_str!("../assets/app.js");

#[derive(Clone)]
pub(crate) struct AppState {
    snapshot_rx: watch::Receiver<Snapshot>,
    core: Arc<CoreClient>,
    /// Wakes `poll_loop` immediately instead of waiting out the rest of
    /// `POLL_INTERVAL` — used after a mutating call (enrollment) so the
    /// Dashboard/Topology tabs reflect it within roughly one round trip,
    /// not up to 5 seconds later.
    poke: Arc<Notify>,
    /// Always present, with or without a configured firmware repo
    /// (design.md §3 decision 14). It used to be `Option`, `None` whenever
    /// `[study_designer]` was absent from config, which made the whole tab
    /// unreachable — including the one route that could have fixed that.
    /// Whether a *project is open* is now the `StudyDesigner`'s own state,
    /// and a route that needs one still answers `404`; the difference is
    /// that `POST /api/study-designer/project` is a way out of it rather
    /// than another `404`.
    study_designer: StudyDesigner,
    /// New-lines-only batches from `logs::poll_loop` — the Debug tab's own
    /// SSE stream (`api/logs/events`). Deliberately separate from
    /// `snapshot_rx`: this channel only carries genuinely new log lines, so
    /// a fresh SSE subscriber must *not* replay the current value the way
    /// `events()`'s `Snapshot` stream does — backlog comes from
    /// `/api/logs/recent` instead.
    logs_rx: watch::Receiver<Vec<String>>,
    /// The same, for `embarch-api`'s own rolling logfile (design.md §3
    /// decision 13). A separate channel rather than one merged stream: the
    /// two sources rotate independently and a merged view would interleave
    /// them by arrival order, not by the timestamps on the lines — the Debug
    /// tab picks a source instead.
    api_logs_rx: watch::Receiver<Vec<String>>,
}

/// `main` spawns the whole tokio runtime on a dedicated big-stack thread
/// rather than using a plain `#[tokio::main]`, proactively — this suite has
/// already hit a real debug-build stack overflow deserializing a
/// GATT-sized `StudyResult` on a normal-sized stack twice
/// (`embarch-api/design.md` decision 36, `study-designer-ui`'s own fix in
/// `embarch-study-designer/milestone-11.md` §3.3b). `embarch-ui`'s own
/// Study Designer tab (§4.6) deserializes the identical oversized type on
/// every `get_study_status` poll — copying only the *first* half of
/// `embarch-api`'s fix (enlarging the thread that calls `block_on`) still
/// crashed live here on a real `discover` call, confirming the second half
/// is load-bearing too: `Builder::thread_stack_size` on the multi-thread
/// runtime itself, since a `new_multi_thread()` runtime's own worker
/// threads — not the thread that called `block_on` — are what actually
/// poll a spawned task (every axum request handler among them), each at
/// tokio's own default stack size unless told otherwise.
fn main() -> anyhow::Result<()> {
    std::thread::Builder::new()
        .stack_size(512 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(512 * 1024 * 1024)
                .build()
                .expect("failed to build tokio runtime")
                .block_on(async_main())
        })
        .expect("failed to spawn main thread with an explicit stack size")
        .join()
        .expect("main thread panicked")
}

async fn async_main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();

    let config = config::load()?;
    let core = Arc::new(embarch_core_client::CoreClient::new(&config.core)?);

    let (tx, rx) = watch::channel(Snapshot::pending());
    let poke = Arc::new(Notify::new());
    tokio::spawn(poll_loop(core.clone(), tx, poke.clone()));

    // Seeded from config when it names a repo — the zero-click default for a
    // single-repo bench that milestone-1.md §4.6 chose, kept exactly as it
    // was. What is new is that `None` is now an *openable* state rather than
    // a dead tab (design.md §3 decision 14).
    let study_designer = StudyDesigner::new(config.study_designer, core.clone());

    let (logs_tx, logs_rx) = watch::channel(Vec::new());
    tokio::spawn(logs::poll_loop(core.clone(), logs_tx));

    let (api_logs_tx, api_logs_rx) = watch::channel(Vec::new());
    tokio::spawn(logs::api_poll_loop(api_logs_tx));

    let state = AppState { snapshot_rx: rx, core, poke, study_designer, logs_rx, api_logs_rx };

    let app = Router::new()
        .route("/", get(index))
        .route("/style.css", get(style_css))
        .route("/app.js", get(app_js))
        .route("/events", get(events))
        .route("/api/snapshot", get(api_snapshot))
        .route("/api/enroll", post(api_enroll))
        .route("/api/signals", post(api_declare_signal))
        .route("/api/signals/{name}", axum::routing::delete(api_remove_signal))
        .route("/api/trace/{study_id}", get(api_trace_taps))
        .route("/api/trace/{study_id}/{name}", get(api_trace_view))
        .route(
            "/api/study-designer/project",
            get(study_designer::api_project).post(study_designer::api_open_project),
        )
        // Deliberately *not* `/studies/new`: `study_slug` would accept
        // "new" as a perfectly good slug, so that path would be ambiguous
        // with a real study named "new" on the sibling `{slug}` route.
        .route("/api/study-designer/new-study", post(study_designer::api_new_study))
        .route("/api/study-designer/actions", get(study_designer::api_actions))
        .route("/api/study-designer/bench-state", get(study_designer::api_bench_state))
        .route("/api/study-designer/version-check", get(study_designer::api_version_check))
        .route("/api/study-designer/registry", get(study_designer::api_registry).post(study_designer::api_register_action))
        .route("/api/study-designer/discover", post(study_designer::api_discover))
        .route("/api/study-designer/run", post(study_designer::api_run))
        .route("/api/study-designer/events", get(study_designer::api_run_events))
        .route(
            "/api/study-designer/studies",
            get(study_designer::api_studies_list).post(study_designer::api_studies_save),
        )
        .route(
            "/api/study-designer/studies/{slug}",
            get(study_designer::api_studies_load).delete(study_designer::api_studies_delete),
        )
        .route("/api/study-designer/gatt/{study_id}", get(study_designer::api_gatt_data))
        .route("/api/logs/recent", get(api_logs_recent))
        .route("/api/logs/events", get(api_logs_events))
        .route("/api/api-logs/recent", get(api_api_logs_recent))
        .route("/api/api-logs/events", get(api_api_logs_events))
        .with_state(state);

    let addr = bind_address();
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("embarch-ui listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Runs for the lifetime of the process, publishing a fresh `Snapshot`
/// every `POLL_INTERVAL` (or immediately, on a `poke`) — the one place
/// embarch-ui ever talks to Core for Dashboard/Topology data. `tx.send`
/// failing just means every receiver (every open browser tab's SSE
/// connection, plus `/api/snapshot`'s own borrow) has dropped; nothing to
/// do but keep polling in case a new tab opens, so the error is
/// deliberately ignored rather than ending the loop.
async fn poll_loop(core: Arc<CoreClient>, tx: watch::Sender<Snapshot>, poke: Arc<Notify>) {
    loop {
        let snapshot = snapshot::poll(&core).await;
        let _ = tx.send(snapshot);
        tokio::select! {
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
            _ = poke.notified() => {}
        }
    }
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn style_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE_CSS)
}

async fn app_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], APP_JS)
}

/// A plain snapshot read — handy for curl/debugging and as the one-off
/// fetch a page reload can use, though the shell's own JS relies on
/// `/events` (below) rather than fetching this on a timer (design.md §3
/// decision 6: push, not client-side interval polling).
async fn api_snapshot(State(state): State<AppState>) -> Json<Snapshot> {
    Json(state.snapshot_rx.borrow().clone())
}

/// Suite-wide SSE convergence (embarch-ui/design.md §3 decision 6): one
/// `/events` stream every tab subscribes to. Sends the current snapshot
/// immediately on connect, then again every time `poll_loop` publishes a
/// new one — a client never has to poll to find out something changed.
async fn events(State(state): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.snapshot_rx.clone();
    let stream = futures_util::stream::unfold((rx, true), |(mut rx, first)| async move {
        if !first && rx.changed().await.is_err() {
            // The sender (poll_loop) is gone — process is shutting down.
            return None;
        }
        let snapshot = rx.borrow().clone();
        let payload = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
        let event = Event::default().event("snapshot").data(payload);
        Some((Ok(event), (rx, false)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Debug, Deserialize)]
struct EnrollRequest {
    role: String,
    chip: String,
    #[serde(default)]
    probe_serial: Option<String>,
}

/// Submits to `embarch-core`'s existing `POST /probes/enroll` over
/// HTTP+Bearer via `embarch-core-client` — never a direct in-process call
/// to `embarch_topology::hardware::enroll`, which would reintroduce the
/// exact `hw_lock`-bypass bug `embarch-topology/design.md` decision 14
/// already fixed once (embarch-ui/design.md §3 decision 5).
///
/// Unlike Core's own `GET /enroll` page — a static page with no server of
/// its own, so it has no choice but to ask a human to paste in a bearer
/// token by hand — this handler already holds a live `CoreClient`
/// server-side. The browser talking to embarch-ui never sees Core's token
/// at all, a real UX improvement over the page this tab replaces, not just
/// a straight port of it.
async fn api_enroll(State(state): State<AppState>, Json(req): Json<EnrollRequest>) -> impl IntoResponse {
    match state
        .core
        .enroll_probe(&req.role, &req.chip, req.probe_serial.as_deref())
        .await
    {
        Ok(resp) => {
            // Wake the poll loop so Dashboard/Topology (and this tab's own
            // enrolled-boards list) reflect the new enrollment on the very
            // next SSE push, not up to POLL_INTERVAL later.
            state.poke.notify_one();
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ---- signal routes (design.md §3 decision 10) -------------------------------

/// Declares (or re-declares) where a named DUT signal goes, through Core's
/// `POST /signals`.
///
/// A proxy rather than a browser-to-Core call, for the same two reasons
/// `/api/enroll` is one: this handler already holds a live `CoreClient`, so
/// the browser never sees Core's bearer token, and Core owns the write.
///
/// `declare_signal` is idempotent by name, so this is both "add a route" and
/// "move a route" — and the move is the whole reason `SignalLink` records a
/// declared route rather than the wiring that happens to be in place: a saved
/// `Study` names the signal and never the carrier, so nothing it authored has
/// to be re-authored the day a cable moves.
async fn api_declare_signal(
    State(state): State<AppState>,
    Json(link): Json<embarch_core_client::SignalLink>,
) -> impl IntoResponse {
    match state.core.declare_signal(&link).await {
        Ok(()) => {
            state.poke.notify_one();
            (StatusCode::OK, Json(serde_json::json!({ "declared": link.name }))).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// Un-declares a signal, through Core's `DELETE /signals/{name}`.
///
/// Exists because decision 10's own consequence demands it: this tab is the
/// only human surface for signal routes, and without a removal the one place
/// that can state a wire could never retract one.
///
/// A name nothing was declared under answers `404` rather than a silent
/// success — a row this tab thought existed and did not is worth learning
/// about.
async fn api_remove_signal(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.core.remove_signal(&name).await {
        Ok(true) => {
            state.poke.notify_one();
            (StatusCode::OK, Json(serde_json::json!({ "removed": name }))).into_response()
        }
        Ok(false) => (
            StatusCode::NOT_FOUND,
            format!("no signal is declared under the name '{name}'"),
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

// ---- the Trace view (design.md §3 decision 10's second half) ----------------

/// Which of a study's taps are outpost traces, and what Core has to say about
/// each — read from Core's `GET /study/{id}/streams`.
///
/// **This is the call that makes an unnamed trace visible as one.** Nothing
/// else on Core's HTTP surface carries the reason: `GET /study/{id}` returns a
/// `StreamRef` with no room for it, and the stream route serves the rendered
/// CSV either way. A Trace view that skipped this would be structurally
/// incapable of telling a named trace from a refused one, which is the exact
/// confusion decision 10 exists to prevent.
async fn api_trace_taps(
    State(state): State<AppState>,
    axum::extract::Path(study_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.core.study_streams(&study_id).await {
        Ok(Some(index)) => {
            let taps: Vec<serde_json::Value> = index
                .streams
                .iter()
                .map(|e| {
                    serde_json::json!({
                        "name": e.name,
                        // Serialized rather than matched on: which encodings
                        // this view can draw is this tab's business, and the
                        // vocabulary is the shared crate's.
                        "encoding": e.encoding,
                        "is_outpost_trace": matches!(
                            e.encoding,
                            embarch_study_designer::StreamEncoding::OutpostTrace
                        ),
                        "rendered": e.rendered,
                        "note": e.note,
                        // Two facts, not one: a trace can be named and untimed
                        // or timed and unnamed, and the tab draws each
                        // differently (`embarch-outpost/design.md` §3
                        // decisions 9, 16).
                        "named": e.is_named(),
                        "timed": e.is_timed(),
                    })
                })
                .collect();
            (StatusCode::OK, Json(serde_json::json!({ "study_id": study_id, "taps": taps })))
                .into_response()
        }
        // No `streams/` at all: a study that predates it, or one that never
        // got far enough to write it. An expected state, said plainly.
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("study '{study_id}' recorded no streams"),
        )
            .into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// One tap's recorded timeline, decoded server-side into lanes and gaps.
///
/// Two calls to Core, and both are needed: the stream index says whether the
/// trace is named and why not, and the stream route hands back the rendered
/// CSV. Decoding happens in `trace.rs` through `embarch-study-designer`'s own
/// `outpost` module rather than in the browser, so no trace knowledge — column
/// order, record kinds, `IRQ_UNKNOWN` — lives in `app.js`.
async fn api_trace_view(
    State(state): State<AppState>,
    axum::extract::Path((study_id, name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    let index = match state.core.study_streams(&study_id).await {
        Ok(Some(index)) => index,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("study '{study_id}' recorded no streams"))
                .into_response()
        }
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    let Some(entry) = index.streams.iter().find(|e| e.name == name) else {
        let declared: Vec<&str> = index.streams.iter().map(|e| e.name.as_str()).collect();
        return (
            StatusCode::NOT_FOUND,
            format!("study '{study_id}' declared no tap named '{name}' — it declared: {}",
                if declared.is_empty() { "(none)".to_string() } else { declared.join(", ") }),
        )
            .into_response();
    };
    if !entry.rendered {
        // Without a rendering there is nothing decoded to draw, and drawing
        // the raw bytes as if they were a timeline is the one thing this view
        // must not do. Core's own note usually says why.
        return (
            StatusCode::CONFLICT,
            format!(
                "tap '{name}' has no decoded rendering, so there is no timeline to draw. {}",
                entry.note.clone().unwrap_or_else(|| "Core recorded no reason.".to_string())
            ),
        )
            .into_response();
    }

    let bytes = match state.core.get_study_stream(&study_id, &name, false).await {
        Ok(bytes) => bytes,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    };
    let csv = String::from_utf8_lossy(&bytes);

    match trace::parse(
        &study_id,
        &name,
        &csv,
        entry.is_named(),
        entry.is_timed(),
        entry.note.clone(),
    ) {
        Ok(view) => (StatusCode::OK, Json(view)).into_response(),
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct LogsRecentQuery {
    tail: Option<usize>,
}

/// One-shot backlog fetch for the Debug tab's first paint — a thin proxy
/// over `embarch-core-client`'s own `GET /logs/recent`, never a direct
/// filesystem read (design.md §3 decision 7). Ongoing live lines come from
/// `/api/logs/events` (SSE) instead, not a repeated call to this endpoint.
async fn api_logs_recent(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<LogsRecentQuery>,
) -> impl IntoResponse {
    let tail = q.tail.unwrap_or(200);
    match state.core.logs_recent(tail).await {
        Ok(lines) => (StatusCode::OK, Json(serde_json::json!({ "lines": lines }))).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response(),
    }
}

/// `api_logs_recent`'s counterpart for `embarch-api`'s own rolling logfile
/// (design.md §3 decision 13) — a direct file read rather than a proxy,
/// because `embarch-api` is not a service to proxy to. See `logs.rs`'s
/// module comment for why that does not reopen decision 7's argument.
///
/// A machine where `embarch-api` has never run returns `{"lines": []}` and
/// a `200`, not an error: nothing logged is a real answer.
async fn api_api_logs_recent(
    axum::extract::Query(q): axum::extract::Query<LogsRecentQuery>,
) -> impl IntoResponse {
    let tail = q.tail.unwrap_or(200);
    match tokio::task::spawn_blocking(move || embarch_core_client::api_log::read_recent(tail)).await {
        Ok(Ok(lines)) => (StatusCode::OK, Json(serde_json::json!({ "lines": lines }))).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response(),
    }
}

/// `api_logs_events`'s counterpart, over `logs::api_poll_loop`'s channel.
async fn api_api_logs_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    sse_lines(state.api_logs_rx.clone())
}

/// Live tail: one SSE event per non-empty batch `logs::poll_loop` publishes
/// — unlike `events()` above, a freshly-connected subscriber does **not**
/// get sent the channel's current value first (`/api/logs/recent` is the
/// backlog path; replaying the last batch here would either duplicate it
/// or, worse, silently skip whatever arrived between that batch and this
/// connection).
async fn api_logs_events(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    sse_lines(state.logs_rx.clone())
}

/// Shared by both log streams: relay each new batch a `watch` channel
/// publishes as one `lines` SSE event.
///
/// **`mark_unchanged` is load-bearing, and its absence was a real defect**
/// (found 2026-08-25 while adding the second stream, present since the
/// first shipped). A `watch::Receiver` cloned from one whose `changed()` was
/// never awaited inherits that receiver's version — which is still the
/// channel's initial version — so the very first `changed()` on the clone
/// returns immediately with whatever batch was published most recently.
/// Every browser opening the Debug tab therefore got the last batch
/// replayed on top of its own `/recent` backlog fetch, as duplicate lines.
/// Marking the current value seen at subscribe time is what the comment on
/// `api_logs_events` always claimed the code did.
fn sse_lines(rx: watch::Receiver<Vec<String>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = rx;
    rx.mark_unchanged();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        if rx.changed().await.is_err() {
            return None;
        }
        let lines = rx.borrow().clone();
        let payload = serde_json::to_string(&lines).unwrap_or_else(|_| "[]".to_string());
        let event = Event::default().event("lines").data(payload);
        Some((Ok::<_, Infallible>(event), rx))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
