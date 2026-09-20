//! embarch-ui: one consolidated human-facing UI for the EmbArch suite.
//!
//! The app shell is live against the reviewed mockups, the Dashboard/
//! Topology tabs render real data from `embarch-core` — and Topology
//! submits real enrolments, dropped onto the diagram itself since the
//! Enroll tab folded into it (decision 43) — the Study Designer tab builds and runs a
//! `Study`, and the Debug tab live-tails Core's own log — entirely through
//! `embarch-core-client` (decision 5's amendment: no in-process hardware
//! access, and decision 7: never a direct logfile read). See
//! `embarch-doc/embarch-ui/spec.md` and `embarch-doc/embarch-ui/decisions/`
//! for the full architecture.

mod config;
mod firmware_build;
mod live_study;
mod logs;
mod snapshot;
mod studies_api;
mod study_designer;
mod time_chart;
mod topology;
mod trace;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use embarch_core_client::CoreClient;
use futures_util::stream::Stream;
use serde::Deserialize;
use snapshot::Snapshot;
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use study_designer::StudyDesigner;
use tokio::sync::{watch, Notify};

/// Binds loopback-only by default — same reasoning as `embarch-topology`'s
/// own local-UI precedent (`embarch-topology` decision 5) and `embarch-core`
/// decision 6's amendment: no TLS, no reason to expose past localhost for a
/// tool one engineer runs on their own machine.
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
/// The EmbArch mark, served as the browser tab icon. 64 px: a tab renders it
/// at 16 CSS px, which is 32 physical on a 2x display, and 64 covers 4x. The
/// GIMP master and the full-size export live beside it in `assets/brand/`.
const FAVICON_PNG: &[u8] = include_bytes!("../assets/brand/favicon-64.png");
/// The same mark traced to paths, which a tab renders crisply at any DPI. The
/// PNG stays as the second `<link>`: an SVG icon is the one asset type a
/// browser is allowed to decline, and a blank tab is a worse default.
const FAVICON_SVG: &str = include_str!("../assets/brand/embarch-mark.svg");

/// IBM Plex, baked in rather than fetched from fonts.googleapis.com
/// (`assets/style.css`'s `@font-face` block says why at length): a bench
/// machine is regularly offline, and the CDN link these replace failed
/// *silently* — the app fell back to Segoe UI and the trace view's
/// hardcoded 6.6 px-per-character lane gutter stopped matching the font it
/// was measured against. Latin subsets, ~91 KB for all four, which is the
/// whole cost of never depending on Google to render the UI. Sans is one
/// variable file covering 400-700; Mono ships the two weights the
/// stylesheet actually pairs it with. SIL OFL
/// 1.1, licence text beside them in `assets/fonts/`.
const FONT_SANS_VAR: &[u8] = include_bytes!("../assets/fonts/ibm-plex-sans-var-latin.woff2");
const FONT_MONO_400: &[u8] = include_bytes!("../assets/fonts/ibm-plex-mono-400-latin.woff2");
const FONT_MONO_600: &[u8] = include_bytes!("../assets/fonts/ibm-plex-mono-600-latin.woff2");

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
    /// (decision 14). It used to be `Option`, `None` whenever
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
    /// The same, for `embarch-api`'s own rolling logfile (decision 13). A
    /// separate channel rather than one merged stream: the
    /// two sources rotate independently and a merged view would interleave
    /// them by arrival order, not by the timestamps on the lines — the Debug
    /// tab picks a source instead.
    api_logs_rx: watch::Receiver<Vec<String>>,
    /// The most recently decoded trace, kept whole and server-side.
    ///
    /// **This is what makes the windowed route affordable** (decision 18).
    /// Binning a window is arithmetic over spans that are
    /// already decoded; re-fetching a 13 MB CSV from Core and re-decoding it
    /// per pan would move the cost the decision removes rather than remove it.
    /// One entry, because the tab shows one trace at a time and a second
    /// entry would only ever hold the one before it — and `GET
    /// /api/trace/{study}/{tap}` re-decodes unconditionally, so **loading the
    /// view is the refresh**, rather than there being a staleness rule nobody
    /// can see.
    trace_cache: Arc<tokio::sync::Mutex<Option<CachedTrace>>>,
    /// The Time chart's own one-entry cache — **a new one rather than a wider
    /// `trace_cache` or `table_cache`**. Each of those holds one entry because
    /// each serves one card asking many questions about one file; widening
    /// either to fit this chart would change behaviour for those cards.
    time_chart_cache: Arc<tokio::sync::Mutex<Option<time_chart::CachedTimeChart>>>,
    /// The same shape, for the Data cards' rendered CSVs — see
    /// `studies_api::table_for`. One entry, because the tab shows one tap's
    /// table at a time, and paging it is many requests against one file.
    table_cache: Arc<tokio::sync::Mutex<Option<studies_api::CachedTable>>>,
    /// Every study this process is watching live, and the one place a new
    /// watch is started (`live_study`). **One subscription to embarch-core
    /// per study, never one per browser** — which is what lets a tab opened
    /// or reloaded mid-run replay the whole run so far.
    live: Arc<live_study::LiveStudies>,
    /// Per-project build locks, so two runs that both want to build the
    /// same project queue rather than stomp one output directory. The same
    /// `BuildLocks` `embarch-api` holds, from the same crate — and held per
    /// process, not per request, which is the whole point of a lock.
    build_locks: Arc<embarch_firmware_build::build::BuildLocks>,
    /// Build-and-flash runs this process has started, and what each has
    /// said so far — what `GET /api/build/events` streams.
    build_runs: Arc<firmware_build::BuildRuns>,
    /// Where `embarch-api`'s project config is, when this UI's own config
    /// names it. `None` falls through to `EMBARCH_API_CONFIG`.
    build_config_path: Option<std::path::PathBuf>,
}

/// One decoded capture, keyed by the study and tap it was decoded from.
struct CachedTrace {
    study_id: String,
    tap: String,
    view: Arc<trace::TraceView>,
}

/// `main` spawns the whole tokio runtime on a dedicated big-stack thread
/// rather than using a plain `#[tokio::main]`, proactively — this suite has
/// already hit a real debug-build stack overflow deserializing a
/// GATT-sized `StudyResult` on a normal-sized stack twice
/// (`embarch-api` decision 36, and `study-designer-ui`'s own
/// earlier fix for the identical crash). `embarch-ui`'s own Study Designer
/// tab deserializes the identical oversized type on
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
    // single-repo bench, kept exactly as it was. What is new is that `None`
    // is now an *openable* state rather than a dead tab (decision 14).
    let study_designer = StudyDesigner::new(config.study_designer, core.clone());

    let (logs_tx, logs_rx) = watch::channel(Vec::new());
    tokio::spawn(logs::poll_loop(core.clone(), logs_tx));

    let (api_logs_tx, api_logs_rx) = watch::channel(Vec::new());
    tokio::spawn(logs::api_poll_loop(api_logs_tx));

    let live = live_study::LiveStudies::new(core.clone());

    let build_config_path = config.build.as_ref().map(|b| b.config_path.clone());

    let state = AppState {
        snapshot_rx: rx,
        core,
        poke,
        study_designer,
        logs_rx,
        api_logs_rx,
        trace_cache: Arc::new(tokio::sync::Mutex::new(None)),
        table_cache: Arc::new(tokio::sync::Mutex::new(None)),
        time_chart_cache: Arc::new(tokio::sync::Mutex::new(None)),
        live,
        build_locks: Arc::new(embarch_firmware_build::build::BuildLocks::new()),
        build_runs: firmware_build::BuildRuns::new(),
        build_config_path,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/style.css", get(style_css))
        .route("/app.js", get(app_js))
        .route("/favicon.png", get(favicon_png))
        .route("/favicon.svg", get(favicon_svg))
        .route("/fonts/{file}", get(font))
        .route("/events", get(events))
        .route("/api/snapshot", get(api_snapshot))
        .route("/api/enroll", post(api_enroll))
        .route("/api/signals", post(api_declare_signal))
        .route("/api/signals/{name}", axum::routing::delete(api_remove_signal))
        // ---- Topology: the board catalog, saved benches, validation ------
        //
        // Three owners, three routes (decision 44): the catalog and the
        // profiles are project files this binary reads and writes, while
        // every enrolment, signal and link write still goes to Core over
        // HTTP+Bearer like everything else hardware-adjacent (decision 5).
        .route("/api/enrolled/{role}", axum::routing::delete(topology::api_unenroll))
        .route(
            "/api/topology/boards",
            get(topology::api_boards).post(topology::api_save_board),
        )
        .route(
            "/api/topology/boards/{name}",
            axum::routing::delete(topology::api_delete_board),
        )
        .route("/api/topology/link", post(topology::api_link))
        .route("/api/topology/pickers", get(topology::api_pickers))
        .route(
            "/api/topology/roles/{role}/board",
            post(topology::api_set_role_board),
        )
        .route("/api/topology/boards/rescan", post(topology::api_rescan_boards))
        .route("/api/topology/validate", post(topology::api_validate))
        .route(
            "/api/topology/profiles",
            get(topology::api_profiles).post(topology::api_save_profile),
        )
        .route(
            "/api/topology/profiles/{slug}",
            axum::routing::delete(topology::api_delete_profile),
        )
        .route(
            "/api/topology/profiles/{slug}/apply",
            post(topology::api_apply_profile),
        )
        // ---- Live Study -------------------------------------------------
        .route("/api/live/events", get(live_study::api_live_events))
        .route("/api/live/run", post(live_study::api_live_run))
        .route("/api/studies", get(studies_api::api_studies))
        .route("/api/studies/{study_id}", get(studies_api::api_study))
        .route(
            "/api/studies/{study_id}/stream/{name}/rows",
            get(studies_api::api_stream_rows),
        )
        .route(
            "/api/studies/{study_id}/stream/{name}/series",
            get(studies_api::api_stream_series),
        )
        .route(
            "/api/studies/{study_id}/stream/{name}/text",
            get(studies_api::api_stream_text),
        )
        .route(
            "/api/studies/{study_id}/stream/{name}/head",
            get(studies_api::api_stream_head),
        )
        .route(
            "/api/studies/{study_id}/stream/{name}/download",
            get(studies_api::api_stream_download),
        )
        // ---- the Time chart ---------------------------------------------
        //
        // One study, every stream it produced, one axis. `/marks` is the
        // windowed route the chart actually draws from; `/mark/{id}` opens one
        // of them. `mark` comes before nothing ambiguous — a study id cannot
        // contain a slash — so ordering here is plain.
        .route("/api/time-chart/{study_id}", get(time_chart::api_time_chart))
        .route("/api/time-chart/{study_id}/marks", get(time_chart::api_time_chart_marks))
        .route("/api/time-chart/{study_id}/mark/{id}", get(time_chart::api_time_chart_mark))
        .route("/api/trace/{study_id}", get(api_trace_taps))
        .route("/api/trace/{study_id}/{name}", get(api_trace_view))
        .route("/api/trace/{study_id}/{name}/bins", get(api_trace_bins))
        .route(
            "/api/study-designer/project",
            get(study_designer::api_project).post(study_designer::api_open_project),
        )
        // Deliberately *not* `/studies/new`: `study_slug` would accept
        // "new" as a perfectly good slug, so that path would be ambiguous
        // with a real study named "new" on the sibling `{slug}` route.
        .route("/api/study-designer/new-study", post(study_designer::api_new_study))
        .route(
            "/api/study-designer/static-analysis",
            post(study_designer::api_static_analysis),
        )
        .route("/api/study-designer/actions", get(study_designer::api_actions))
        .route("/api/study-designer/bench-state", get(study_designer::api_bench_state))
        .route("/api/study-designer/version-check", get(study_designer::api_version_check))
        .route("/api/study-designer/registry", get(study_designer::api_registry).post(study_designer::api_register_action))
        .route(
            "/api/study-designer/registry/{name}",
            axum::routing::delete(study_designer::api_registry_delete),
        )
        .route(
            "/api/study-designer/structs",
            get(study_designer::api_structs).post(study_designer::api_struct_save),
        )
        .route(
            "/api/study-designer/structs/{name}",
            axum::routing::delete(study_designer::api_struct_delete),
        )
        .route("/api/study-designer/discover", post(study_designer::api_discover))
        .route("/api/study-designer/run", post(study_designer::api_run))
        .route("/api/study-designer/preflight", post(study_designer::api_preflight))
        .route(
            "/api/study-designer/studies",
            get(study_designer::api_studies_list).post(study_designer::api_studies_save),
        )
        .route(
            "/api/study-designer/studies/{slug}",
            get(study_designer::api_studies_load).delete(study_designer::api_studies_delete),
        )
        // Both sit under `{slug}`, so a study named "summary" or "run" is
        // unambiguous: the literal segment follows the capture rather than
        // competing with it.
        .route(
            "/api/study-designer/studies/{slug}/summary",
            get(study_designer::api_study_summary),
        )
        .route("/api/study-designer/studies/{slug}/run", post(study_designer::api_study_run))
        .route("/api/study-designer/gatt/{study_id}", get(study_designer::api_gatt_data))
        // `.eap` protocol files. `/protocols/check` comes BEFORE
        // `/protocols/{stem}` for the same reason `new-study` is not
        // `/studies/new`: `check` is a perfectly good file stem, so a repo
        // with a `check.eap` would otherwise make one of these two
        // unreachable. axum matches literals before captures, so this is
        // unambiguous either way — the ordering states the intent.
        .route("/api/study-designer/protocols", get(study_designer::api_protocols))
        .route("/api/study-designer/protocols/check", post(study_designer::api_protocol_check))
        .route(
            "/api/study-designer/protocols/{stem}",
            get(study_designer::api_protocol_read)
                .put(study_designer::api_protocol_write)
                .delete(study_designer::api_protocol_delete),
        )
        .route("/api/logs/recent", get(api_logs_recent))
        .route("/api/logs/events", get(api_logs_events))
        .route("/api/build/survey", get(firmware_build::api_build_survey))
        .route("/api/build/events", get(firmware_build::api_build_events))
        .route("/api/build/logs", get(firmware_build::api_build_logs))
        .route("/api/build/logs/{id}", get(firmware_build::api_build_log))
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

/// **The three assets that change on every deploy are served
/// revalidating, with an `ETag`** — and this is a defect that cost a whole
/// rework's worth of confusion before it was found (2026-09-20).
///
/// `index.html`, `style.css` and `app.js` are `include_str!`-embedded, so a
/// new binary *is* a new page. They were served with no `Cache-Control`, no
/// `ETag` and no `Last-Modified` at all, which does not mean "do not
/// cache": with no validators and no directives a browser applies heuristic
/// caching and reuses what it has. So a redeploy changed nothing on screen,
/// a reload changed nothing, and the page kept rendering a build from
/// before the work — while `curl` against the same port served the new one.
/// The fonts and the favicons already carried headers; these three, the
/// only ones that move, did not.
///
/// `no-cache` means *revalidate*, not *do not store*: with the `ETag` below
/// a reload against an unchanged binary is a `304` and a few bytes, and
/// against a new one it is the new asset. **The tag is derived from the
/// bytes**, not from a version string, so it cannot go stale by someone
/// forgetting to bump it — the failure this whole entry is about.
fn asset_etag(bytes: &[u8]) -> String {
    // FNV-1a, 64-bit: no dependency, and strong enough for "are these the
    // same bytes I served last time" — the question an ETag asks.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("\"{hash:016x}\"")
}

fn revalidating_asset(
    headers: &header::HeaderMap,
    content_type: &'static str,
    body: &'static [u8],
) -> Response {
    static TAGS: OnceLock<StdMutex<HashMap<usize, String>>> = OnceLock::new();
    let tags = TAGS.get_or_init(|| StdMutex::new(HashMap::new()));
    let etag = {
        let mut tags = tags.lock().unwrap();
        tags.entry(body.as_ptr() as usize)
            .or_insert_with(|| asset_etag(body))
            .clone()
    };

    let unchanged = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|sent| sent.split(',').any(|tag| tag.trim() == etag));
    if unchanged {
        return (
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, "no-cache".to_string())],
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        body,
    )
        .into_response()
}

async fn index(headers: header::HeaderMap) -> Response {
    revalidating_asset(&headers, "text/html; charset=utf-8", INDEX_HTML.as_bytes())
}

async fn style_css(headers: header::HeaderMap) -> Response {
    revalidating_asset(&headers, "text/css; charset=utf-8", STYLE_CSS.as_bytes())
}

async fn app_js(headers: header::HeaderMap) -> Response {
    revalidating_asset(&headers, "text/javascript; charset=utf-8", APP_JS.as_bytes())
}

async fn favicon_svg() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        FAVICON_SVG,
    )
}

/// The four `@font-face` sources. One route over a match rather than four
/// handlers: the set is closed, and an unknown name is a typo in the
/// stylesheet, which should 404 rather than resolve to something. The files
/// are immutable — a new subset would arrive under a new name — so they are
/// cached for a year, unlike `style.css`, which changes constantly and is
/// deliberately not cached at all.
async fn font(Path(file): Path<String>) -> Response {
    let body: &'static [u8] = match file.as_str() {
        "ibm-plex-sans-var-latin.woff2" => FONT_SANS_VAR,
        "ibm-plex-mono-400-latin.woff2" => FONT_MONO_400,
        "ibm-plex-mono-600-latin.woff2" => FONT_MONO_600,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response()
}

async fn favicon_png() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        FAVICON_PNG,
    )
}

/// A plain snapshot read — handy for curl/debugging and as the one-off
/// fetch a page reload can use, though the shell's own JS relies on
/// `/events` (below) rather than fetching this on a timer (decision 6:
/// push, not client-side interval polling).
async fn api_snapshot(State(state): State<AppState>) -> Json<Snapshot> {
    Json(state.snapshot_rx.borrow().clone())
}

/// Suite-wide SSE convergence (decision 6): one
/// `/events` stream every tab subscribes to. Sends the current snapshot
/// immediately on connect, then again every time `poll_loop` publishes a
/// new one — a client never has to poll to find out something changed.
async fn events(State(state): State<AppState>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut rx = state.snapshot_rx.clone();
    // The same `mark_unchanged` `sse_lines` documents below, and missing here
    // for the same reason: a clone of a receiver whose `changed()` was never
    // awaited inherits that receiver's version, which is still the channel's
    // *initial* one — `borrow()` does not mark a value seen. So the first
    // `changed()` after the on-connect send returned immediately with the
    // snapshot just sent, and every tab opened got that one twice. Benign to
    // render, because a snapshot is idempotent, which is exactly why it went
    // unnoticed; it is still one wasted serialize-and-repaint per connection
    // and the loop below reads as though it cannot happen.
    rx.mark_unchanged();
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
    /// What the board being enrolled is called (decision 44) — picked from
    /// the project's catalog in the dialog, sent through to Core, and
    /// interpreted by neither this handler nor Core.
    #[serde(default)]
    name: Option<String>,
}

/// Submits to `embarch-core`'s existing `POST /probes/enroll` over
/// HTTP+Bearer via `embarch-core-client` — never a direct in-process call
/// to `embarch_topology::hardware::enroll`, which would reintroduce the
/// exact `hw_lock`-bypass bug `embarch-core` decision 25 already fixed once
/// (decision 5).
///
/// Unlike Core's own `GET /enroll` page — a static page with no server of
/// its own, so it has no choice but to ask a human to paste in a bearer
/// token by hand — this handler already holds a live `CoreClient`
/// server-side. The browser talking to embarch-ui never sees Core's token
/// at all, a real UX improvement over the page this route replaces, not
/// just a straight port of it.
///
/// Its caller is the Topology tab; the Enroll tab that used to own it was
/// folded into Topology (decision 43) without the route changing at all.
async fn api_enroll(State(state): State<AppState>, Json(req): Json<EnrollRequest>) -> impl IntoResponse {
    match state
        .core
        .enroll_probe(&req.role, &req.chip, req.probe_serial.as_deref(), req.name.as_deref())
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

// ---- signal routes (decision 10) --------------------------------------------

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

// ---- the Trace view (decision 10's second half) ------------------------------

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
                        // differently (`embarch-outpost` decision 18).
                        "named": e.is_named(),
                        "timed": e.is_timed(),
                        // A third, and the only one the firmware decided:
                        // whether the outpost kept itself out of the trace
                        // (`embarch-outpost` decision 19).
                        "self_excluded": e.self_excluded,
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
/// Several calls to Core, each for something the other cannot answer: the
/// stream index says whether the trace is named and why not, the stream
/// route hands back the rendered CSV, and the load route (`embarch-core`
/// decision 62) hands back the load repartition computed over that same CSV
/// — `embarch-ui` stopped recomputing that arithmetic itself in `tasks/ui/051`
/// (suite decision 4). Chart decoding still happens in `trace.rs` through
/// `embarch-study-designer`'s own `outpost` module rather than in the
/// browser, so no trace knowledge — column order, record kinds,
/// `IRQ_UNKNOWN` — lives in `app.js`.
async fn api_trace_view(
    State(state): State<AppState>,
    axum::extract::Path((study_id, name)): axum::extract::Path<(String, String)>,
) -> impl IntoResponse {
    match decode_trace(&state, &study_id, &name).await {
        Ok(view) => {
            // Loading the view *is* the refresh: this route always re-fetches
            // and re-decodes, and what it decoded is what `/bins` then answers
            // from until the next load.
            let body = (StatusCode::OK, Json(view.as_ref())).into_response();
            *state.trace_cache.lock().await =
                Some(CachedTrace { study_id, tap: name, view });
            body
        }
        Err(resp) => resp,
    }
}

/// One window of one tap's timeline, binned server-side — the route the tab
/// actually draws from (decision 18).
///
/// `from`/`to` are in the view's own [`trace::TraceView::unit`]s and default
/// to the whole capture; `width` is the number of bins, which is the plot's
/// width in pixels. At most `width` runs come back per lane, whatever the
/// dataset holds.
async fn api_trace_bins(
    State(state): State<AppState>,
    axum::extract::Path((study_id, name)): axum::extract::Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<TraceBinsQuery>,
) -> impl IntoResponse {
    let cached = {
        let guard = state.trace_cache.lock().await;
        guard
            .as_ref()
            .filter(|c| c.study_id == study_id && c.tap == name)
            .map(|c| c.view.clone())
    };
    // A miss is a deep link straight into a window, or a UI process that was
    // restarted under an open tab. Decoding it here rather than answering
    // `409` keeps that case working; it is the slow path by construction,
    // since the next request for the same tap hits the cache.
    let view = match cached {
        Some(view) => view,
        None => match decode_trace(&state, &study_id, &name).await {
            Ok(view) => {
                *state.trace_cache.lock().await = Some(CachedTrace {
                    study_id: study_id.clone(),
                    tap: name.clone(),
                    view: view.clone(),
                });
                view
            }
            Err(resp) => return resp,
        },
    };
    let from = q.from.unwrap_or(view.t_from);
    let to = q.to.unwrap_or(view.t_to);
    match trace::bin_window(&view, from, to, q.width.unwrap_or(1)) {
        Ok(bins) => (StatusCode::OK, Json(bins)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct TraceBinsQuery {
    from: Option<u64>,
    to: Option<u64>,
    width: Option<usize>,
}

/// Fetches one tap's rendered CSV from Core and decodes it into a
/// [`trace::TraceView`]. The error arm is the HTTP response to send.
pub(crate) async fn decode_trace(
    state: &AppState,
    study_id: &str,
    name: &str,
) -> Result<Arc<trace::TraceView>, axum::response::Response> {
    let index = match state.core.study_streams(study_id).await {
        Ok(Some(index)) => index,
        Ok(None) => {
            return Err((StatusCode::NOT_FOUND, format!("study '{study_id}' recorded no streams"))
                .into_response())
        }
        Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()),
    };
    let Some(entry) = index.streams.iter().find(|e| e.name == name) else {
        let declared: Vec<&str> = index.streams.iter().map(|e| e.name.as_str()).collect();
        return Err((
            StatusCode::NOT_FOUND,
            format!("study '{study_id}' declared no tap named '{name}' — it declared: {}",
                if declared.is_empty() { "(none)".to_string() } else { declared.join(", ") }),
        )
            .into_response());
    };
    if !entry.rendered {
        // Without a rendering there is nothing decoded to draw, and drawing
        // the raw bytes as if they were a timeline is the one thing this view
        // must not do. Core's own note usually says why.
        return Err((
            StatusCode::CONFLICT,
            format!(
                "tap '{name}' has no decoded rendering, so there is no timeline to draw. {}",
                entry.note.clone().unwrap_or_else(|| "Core recorded no reason.".to_string())
            ),
        )
            .into_response());
    }

    let bytes = match state.core.get_study_stream(study_id, name, false).await {
        Ok(bytes) => bytes,
        Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()),
    };
    let csv = String::from_utf8_lossy(&bytes);

    // A third call to Core: the load repartition itself (`embarch-core`
    // decision 62; suite decision 4). `embarch-ui` used to recompute this
    // from `csv` above (`tasks/ui/051` retired that second implementation),
    // so unlike `study_steps` below, a failure here is not swallowed into an
    // empty answer — the tab would otherwise render a populated chart beside
    // a load table quietly showing nothing. Same status as every other
    // proxied Core round trip in this function; the one refusal specific to
    // this route (a `422` when the rendered CSV's columns don't match this
    // build's) cannot occur silently anyway, because `trace::parse` below
    // checks the identical columns on the identical bytes and would refuse
    // the same way.
    let summary = match state.core.get_study_load(study_id, name).await {
        Ok(load) => load.summary,
        Err(e) => return Err((StatusCode::BAD_GATEWAY, format!("{e:#}")).into_response()),
    };

    // A fourth call to Core, and it is what puts the study's own steps above
    // the lanes. Its absence is not an error: a study that ran before Core
    // recorded per-step stamps, or one whose `events.json` has been swept,
    // still has a perfectly good timeline to draw — the row is what goes
    // missing, and the view says so itself rather than failing the request.
    let steps = step_stamps(&state.core, study_id).await;

    match trace::parse(
        study_id,
        name,
        &csv,
        entry.is_named(),
        entry.is_timed(),
        entry.self_excluded,
        entry.note.clone(),
        &steps,
        summary,
    ) {
        Ok(view) => Ok(Arc::new(view)),
        Err(e) => Err((StatusCode::UNPROCESSABLE_ENTITY, e).into_response()),
    }
}

/// embarch-core's own per-step arrival stamps, reduced to what placing a step
/// on a timeline needs.
///
/// Its absence is not an error: a study that ran before Core recorded per-step
/// stamps, or one whose `events.json` has been swept, still has a perfectly
/// good timeline to draw — the step row is what goes missing, and the view says
/// so itself rather than failing the request.
///
/// **One implementation, two callers.** The Trace card's step row and the Time
/// chart's step bands are the same bands through the same projection; a second
/// copy of this reduction is a second place `timed: false` could be read wrong.
pub(crate) async fn step_stamps(
    core: &CoreClient,
    study_id: &str,
) -> Vec<trace::StepStamp> {
    match core.study_steps(study_id).await {
        Ok(Some(steps)) if steps.timed => steps
            .steps
            .into_iter()
            .filter_map(|s| {
                Some(trace::StepStamp {
                    index: s.index,
                    name: s.step_name,
                    outcome: s.outcome,
                    reason: s.reason,
                    delay_before_ms: s.delay_before_ms.unwrap_or(0),
                    started_utc_ms: s.started_utc_ms?,
                    ended_utc_ms: s.ended_utc_ms?,
                })
            })
            .collect(),
        // `timed: false` is Core saying this study predates the stamps. Not
        // partially filled in and not guessed at — an untimed study hands back
        // no steps at all, and the caller renders its "no per-step arrival
        // stamps" sentence.
        Ok(_) => Vec::new(),
        Err(e) => {
            tracing::warn!("could not read study '{study_id}' steps for its step row: {e:#}");
            Vec::new()
        }
    }
}

#[derive(Debug, Deserialize)]
struct LogsRecentQuery {
    tail: Option<usize>,
}

/// One-shot backlog fetch for the Debug tab's first paint — a thin proxy
/// over `embarch-core-client`'s own `GET /logs/recent`, never a direct
/// filesystem read (decision 7). Ongoing live lines come from
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
/// (decision 13) — a direct file read rather than a proxy,
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
