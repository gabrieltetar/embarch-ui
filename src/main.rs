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

mod atlas;
mod bench;
mod config;
mod desk;
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
/// The Atlas tab's map, a file of its own: `app.js` is already the shell plus
/// five tabs, and the map is the one view that is mostly drawing code.
const ATLAS_JS: &str = include_str!("../assets/atlas.js");
const DESK_JS: &str = include_str!("../assets/desk.js");
/// The Topology tab's bench canvas, a file of its own like the Atlas map.
const BENCH_JS: &str = include_str!("../assets/bench.js");
/// The bench editor's part editor: a dialog of its own over the canvas, in a
/// file of its own so the canvas and the editor change apart.
const BENCH_PARTS_JS: &str = include_str!("../assets/bench-parts.js");
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
    /// Where this UI records which bench is live in each repo
    /// (`bench::live`): beside its per-user state, never in a firmware repo.
    /// `None` when there is no per-user data directory, and the live routes
    /// say so.
    bench_live_path: Option<std::path::PathBuf>,
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
    // Follows studies this process did not start, so their prompts reach the
    // modal (`embarch-ui` decision 69).
    tokio::spawn(live.clone().watch_running());

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
        bench_live_path: bench::live::default_record_path(),
    };

    let app = app(state);

    let addr = bind_address();
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("embarch-ui listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Every route this server answers, and what every answer carries.
fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/style.css", get(style_css))
        .route("/app.js", get(app_js))
        .route("/atlas.js", get(atlas_js))
        .route("/desk.js", get(desk_js))
        .route("/bench.js", get(bench_js))
        .route("/bench-parts.js", get(bench_parts_js))
        .route("/api/desk-events", get(desk::events))
        .route("/api/desk/{*rest}", axum::routing::any(desk::proxy))
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
        // ---- The bench editor: templates, benches, checks ---------------
        //
        // Files and pure functions only (`bench`): the vocabulary and the
        // built-in parts come from the binary, the repo's parts and benches
        // from `embarch/parts/` and `embarch/topologies/`, and no route here
        // calls Core. A bench goes live through the apply route above.
        .route("/api/bench/vocabulary", get(bench::routes::api_vocabulary))
        .route("/api/bench/parts", get(bench::routes::api_parts).post(bench::routes::api_post_part))
        .route("/api/bench/parts/{id}", axum::routing::put(bench::routes::api_put_part))
        .route("/api/bench/benches", get(bench::routes::api_benches).post(bench::routes::api_post_bench))
        .route(
            "/api/bench/benches/{slug}",
            get(bench::routes::api_bench).put(bench::routes::api_put_bench),
        )
        .route("/api/bench/benches/{slug}/live", post(bench::live::api_make_live))
        .route("/api/bench/live", get(bench::live::api_live).delete(bench::live::api_clear_live))
        .route("/api/bench/live/retract", post(bench::live::api_retract))
        .route("/api/bench/check", post(bench::routes::api_check))
        .route("/api/bench/candidates", post(bench::routes::api_candidates))
        // ---- Live Study -------------------------------------------------
        .route("/api/live/events", get(live_study::api_live_events))
        .route("/api/live/run", post(live_study::api_live_run))
        .route("/api/live/prompt", post(live_study::api_live_prompt))
        .route("/api/prompts/events", get(live_study::api_prompt_events))
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
        .route("/api/atlas", get(atlas::api_index))
        .route("/api/atlas/code", get(atlas::api_code))
        .route("/api/atlas/doc/{doc}/{*file}", get(atlas::api_doc_file))
        .route("/api/atlas/{id}/graph", get(atlas::api_graph))
        .route("/api/atlas/{id}/page/{doc}/{page}", get(atlas::api_page))
        .route("/api/atlas/{id}/pdf/{doc}", get(atlas::api_pdf))
        .route("/api/logs/recent", get(api_logs_recent))
        .route("/api/logs/events", get(api_logs_events))
        .route("/api/build/survey", get(firmware_build::api_build_survey))
        .route("/api/build/events", get(firmware_build::api_build_events))
        .route("/api/build/logs", get(firmware_build::api_build_logs))
        .route("/api/build/logs/{id}", get(firmware_build::api_build_log))
        .route("/api/api-logs/recent", get(api_api_logs_recent))
        .route("/api/api-logs/events", get(api_api_logs_events))
        .with_state(state)
        .layer(axum::middleware::map_response(deny_framing))
}

/// **No other site may frame this UI** (desk review r2, finding 3). The Desk
/// tab's proxy answers only this UI's own page (`desk::same_origin`), and a
/// page framed by another site *is* this UI's own page: every request the
/// frame makes is same-origin. What is left is click-jacking: the owner,
/// lured into clicking on cue, presses an invisible Send on an agent's draft,
/// Discard, Open chat or Start work. So every response says it may not be
/// framed: `frame-ancestors` for browsers that read CSP, `X-Frame-Options`
/// for any that do not. `'none'` rather than `'self'`: no page of this UI
/// frames another (the Atlas tab opens a PDF in a new window), and the
/// launcher opens the UI in the system browser, not in a VS Code webview.
async fn deny_framing(mut resp: Response) -> Response {
    let headers = resp.headers_mut();
    headers.append(header::CONTENT_SECURITY_POLICY, header::HeaderValue::from_static("frame-ancestors 'none'"));
    headers.insert(header::X_FRAME_OPTIONS, header::HeaderValue::from_static("DENY"));
    resp
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

async fn desk_js(headers: header::HeaderMap) -> Response {
    revalidating_asset(&headers, "text/javascript; charset=utf-8", DESK_JS.as_bytes())
}

async fn atlas_js(headers: header::HeaderMap) -> Response {
    revalidating_asset(&headers, "text/javascript; charset=utf-8", ATLAS_JS.as_bytes())
}

async fn bench_js(headers: header::HeaderMap) -> Response {
    revalidating_asset(&headers, "text/javascript; charset=utf-8", BENCH_JS.as_bytes())
}

async fn bench_parts_js(headers: header::HeaderMap) -> Response {
    revalidating_asset(&headers, "text/javascript; charset=utf-8", BENCH_PARTS_JS.as_bytes())
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
    /// What the probe attaches as. **Empty is looked up, not refused**
    /// (decision 48): it is a read-out of the board type's chip in the
    /// dialog, and a board type whose catalog row states none still has one
    /// — the SoC its scanned targets declare, resolved through Core.
    #[serde(default)]
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
async fn api_enroll(State(state): State<AppState>, Json(req): Json<EnrollRequest>) -> Response {
    // The same lookup and the same refusal `POST .../board` has (decision
    // 48) — an enrolment attaches, so it needs a chip, and the paths that
    // produce one must not disagree about where it comes from or what an
    // unanswerable one says. No Core call is made on a refusal.
    let chip = match crate::topology::resolve_chip_or_refuse(&state, &req.role, req.name.as_deref().unwrap_or(""), &req.chip).await {
        Ok(chip) => chip,
        Err(refusal) => return refusal,
    };
    match state
        .core
        .enroll_probe(&req.role, &chip, req.probe_serial.as_deref(), req.name.as_deref())
        .await
    {
        Ok(resp) => {
            // Wake the poll loop so Dashboard/Topology (and this tab's own
            // enrolled-boards list) reflect the new enrollment on the very
            // next SSE push, not up to POLL_INTERVAL later.
            state.poke.notify_one();
            // The mirror's own sentence for what this enrol moved, sent along
            // so the page says what the CLI and the MCP tool say instead of
            // composing a third wording (decision 61). Absent when nothing moved.
            let note = resp.moved_roles_note();
            let mut body = serde_json::to_value(&resp).unwrap_or_default();
            if let (false, Some(obj)) = (note.is_empty(), body.as_object_mut()) {
                obj.insert("moved_roles_note".into(), note.into());
            }
            (StatusCode::OK, Json(body)).into_response()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The real router over a Core nobody answers on, served on a free port.
    async fn serve_app() -> String {
        serve_app_over("http://127.0.0.1:1").await
    }

    async fn serve_app_over(core_url: &str) -> String {
        serve_app_in(core_url, None).await
    }

    /// The real router with `repo` open as the project.
    async fn serve_app_with_repo(repo: &std::path::Path) -> String {
        serve_app_in("http://127.0.0.1:1", Some(repo.to_path_buf())).await
    }

    async fn serve_app_in(core_url: &str, repo: Option<std::path::PathBuf>) -> String {
        let live = std::env::temp_dir().join(format!(
            "ebm-live-record-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        serve_app_live(core_url, repo, Some(live)).await
    }

    /// The real router with `repo` open and the live-bench record at `live`,
    /// so a test can restart the server over the same record.
    async fn serve_app_live(core_url: &str, repo: Option<std::path::PathBuf>, bench_live_path: Option<std::path::PathBuf>) -> String {
        let core_config: embarch_core_client::CoreConfig =
            toml::from_str(&format!("base_url = \"{core_url}\"\ntoken = \"x\"\n")).unwrap();
        let core = Arc::new(CoreClient::new(&core_config).unwrap());
        let (_snapshot_tx, snapshot_rx) = watch::channel(Snapshot::pending());
        let (_logs_tx, logs_rx) = watch::channel(Vec::new());
        let (_api_logs_tx, api_logs_rx) = watch::channel(Vec::new());
        let state = AppState {
            snapshot_rx,
            core: core.clone(),
            poke: Arc::new(Notify::new()),
            study_designer: StudyDesigner::new(
                repo.map(|firmware_repo_path| config::StudyDesignerConfig { firmware_repo_path }),
                core.clone(),
            ),
            logs_rx,
            api_logs_rx,
            trace_cache: Arc::new(tokio::sync::Mutex::new(None)),
            table_cache: Arc::new(tokio::sync::Mutex::new(None)),
            time_chart_cache: Arc::new(tokio::sync::Mutex::new(None)),
            live: live_study::LiveStudies::new(core),
            build_locks: Arc::new(embarch_firmware_build::build::BuildLocks::new()),
            build_runs: firmware_build::BuildRuns::new(),
            build_config_path: None,
            bench_live_path,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app(state)).await.unwrap() });
        format!("http://{addr}")
    }

    /// Decision 48: an enrolment that names a board whose chip cannot be
    /// answered is refused here, with the role route's guidance, and never
    /// reaches Core. The stub Core counts connections.
    #[tokio::test]
    async fn enroll_refuses_a_board_with_no_chip_without_calling_core() {
        let core = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let core_url = format!("http://{}", core.local_addr().unwrap());
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        tokio::spawn(async move {
            loop {
                if core.accept().await.is_ok() {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });
        let base = serve_app_over(&core_url).await;
        let http = reqwest::Client::new();
        for body in [
            serde_json::json!({ "role": "dut", "chip": "", "probe_serial": "ABC", "name": "plank" }),
            serde_json::json!({ "role": "dut", "chip": "", "probe_serial": "ABC" }),
        ] {
            let resp = http.post(format!("{base}/api/enroll")).json(&body).send().await.unwrap();
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "{body}");
            let text = resp.text().await.unwrap();
            assert!(text.contains("no chip"), "{text}");
        }
        let named = http
            .post(format!("{base}/api/enroll"))
            .json(&serde_json::json!({ "role": "dut", "chip": "", "name": "plank" }))
            .send()
            .await
            .unwrap();
        assert!(named.text().await.unwrap().contains("Board types"), "names where to set it");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "Core was called");
    }

    fn says_never_frame_me(resp: &reqwest::Response, what: &str) {
        let get = |k: header::HeaderName| resp.headers().get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
        assert_eq!(get(header::CONTENT_SECURITY_POLICY).as_deref(), Some("frame-ancestors 'none'"), "{what}: {}", resp.status());
        assert_eq!(get(header::X_FRAME_OPTIONS).as_deref(), Some("DENY"), "{what}: {}", resp.status());
    }

    /// The part editor is its own script: served beside `bench.js`, loaded
    /// by the page after it (the editor calls the canvas's preview), and
    /// revalidated like every other asset.
    #[tokio::test]
    async fn the_part_editor_script_is_served_and_loaded_after_the_canvas() {
        let base = serve_app().await;
        let http = reqwest::Client::new();
        let resp = http.get(format!("{base}/bench-parts.js")).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), "text/javascript; charset=utf-8");
        let etag = resp.headers().get(header::ETAG).unwrap().clone();
        assert_eq!(resp.text().await.unwrap(), BENCH_PARTS_JS);
        let again = http.get(format!("{base}/bench-parts.js")).header(header::IF_NONE_MATCH, etag).send().await.unwrap();
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        let canvas = INDEX_HTML.find("<script src=\"/bench.js\">").unwrap();
        let editor = INDEX_HTML.find("<script src=\"/bench-parts.js\">").unwrap();
        assert!(editor > canvas, "the editor loads after the canvas it previews with");
    }

    /// Desk review r2, finding 3: another site framing the page would make
    /// every request the frame sends a same-origin one.
    #[tokio::test]
    async fn no_answer_from_this_server_may_be_framed_by_another_site() {
        let base = serve_app().await;
        let http = reqwest::Client::new();
        let page = http.get(format!("{base}/")).send().await.unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        says_never_frame_me(&page, "the page");
        let etag = page.headers().get(header::ETAG).unwrap().clone();
        let again = http.get(format!("{base}/")).header(header::IF_NONE_MATCH, etag).send().await.unwrap();
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED);
        says_never_frame_me(&again, "the page, unchanged");
        for (what, path) in [
            ("a script", "/desk.js"),
            ("the icon", "/favicon.svg"),
            ("an API read", "/api/snapshot"),
            ("a handler's 404", "/fonts/none.woff2"),
            ("a route nobody serves", "/no/such/route"),
        ] {
            says_never_frame_me(&http.get(format!("{base}{path}")).send().await.unwrap(), what);
        }
        // Refused before it could reach any desk.
        let refused = http
            .post(format!("{base}/api/desk/mailbox"))
            .header("sec-fetch-site", "cross-site")
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        says_never_frame_me(&refused, "the proxy's refusal");
    }

    /// Decision 64: a first-page `/rows` re-fetches (loading the view is the
    /// refresh), so a table cached mid-run is not what the post-run re-read
    /// gets; later pages and `/series` reuse that load and cost no fetch.
    #[tokio::test]
    async fn a_first_page_rows_read_refetches_and_later_reads_reuse_it() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let csv = Arc::new(std::sync::Mutex::new(String::new()));
        let hits = Arc::new(AtomicUsize::new(0));
        let (c, h) = (csv.clone(), hits.clone());
        let stub = axum::Router::new().fallback(move || {
            let (c, h) = (c.clone(), h.clone());
            async move {
                h.fetch_add(1, Ordering::SeqCst);
                let body = c.lock().unwrap().clone();
                ([(header::CONTENT_TYPE, "text/csv")], body)
            }
        });
        let core = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let core_url = format!("http://{}", core.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(core, stub).await.unwrap() });
        let base = serve_app_over(&core_url).await;
        let http = reqwest::Client::new();
        let rows = format!("{base}/api/studies/s1/stream/t/rows");
        let series = format!("{base}/api/studies/s1/stream/t/series?width=10&column=value");
        let mk = |n: usize| {
            let mut s = String::from("rx_utc_ms,value\n");
            for i in 0..n {
                s += &format!("{},{}\n", 1000 + i, i);
            }
            s
        };

        *csv.lock().unwrap() = mk(3);
        let r: serde_json::Value = http.get(&rows).send().await.unwrap().json().await.unwrap();
        assert_eq!(r["total"], 3);
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        // The run wrote more; the next first-page read is the load.
        *csv.lock().unwrap() = mk(250);
        let r: serde_json::Value = http.get(&rows).send().await.unwrap().json().await.unwrap();
        assert_eq!(r["total"], 250);
        assert_eq!(hits.load(Ordering::SeqCst), 2);

        // Paging and binning reuse it: no further fetch, and they agree.
        for from in [100, 200] {
            let r: serde_json::Value =
                http.get(format!("{rows}?from={from}&limit=100")).send().await.unwrap().json().await.unwrap();
            assert_eq!(r["total"], 250);
        }
        let s: serde_json::Value = http.get(&series).send().await.unwrap().json().await.unwrap();
        assert!(s.to_string().contains("249"), "series sees the longer table: {s}");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }

    /// Decision 69: the modal's answer reaches Core as `answered_by: "ui"`
    /// with the prompt id it names, and a `409` comes back as a `409` with
    /// Core's own words rather than a success or a generic failure.
    #[tokio::test]
    async fn the_prompt_answer_route_relays_as_ui_and_passes_a_409_on() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let log = seen.clone();
        let stub = axum::Router::new().route(
            "/study/{id}/prompt",
            post(move |axum::extract::Json(body): axum::extract::Json<serde_json::Value>| {
                let log = log.clone();
                async move {
                    log.lock().unwrap().push(body.clone());
                    if body["prompt_id"] == 9 {
                        return (StatusCode::CONFLICT, "prompt 9 is not the one waiting").into_response();
                    }
                    axum::Json(serde_json::json!({
                        "study_id": "s1", "prompt_id": body["prompt_id"], "answer": body["answer"],
                        "answered_by": body["answered_by"], "status": "running",
                    }))
                    .into_response()
                }
            }),
        );
        let core = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let core_url = format!("http://{}", core.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(core, stub).await.unwrap() });
        let base = serve_app_over(&core_url).await;
        let http = reqwest::Client::new();
        let route = format!("{base}/api/live/prompt");

        let ok = http
            .post(&route)
            .json(&serde_json::json!({ "study_id": "s1", "prompt_id": 4, "answer": "confirmed" }))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let sent = seen.lock().unwrap()[0].clone();
        assert_eq!(sent["answered_by"], "ui");
        assert_eq!(sent["prompt_id"], 4);
        assert_eq!(sent["answer"], "confirmed");

        let conflict = http
            .post(&route)
            .json(&serde_json::json!({ "study_id": "s1", "prompt_id": 9, "answer": "cancelled" }))
            .send()
            .await
            .unwrap();
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert!(conflict.text().await.unwrap().contains("not the one waiting"));

        // An answer that is neither word never reaches Core.
        let before = seen.lock().unwrap().len();
        let bad = http
            .post(&route)
            .json(&serde_json::json!({ "study_id": "s1", "prompt_id": 4, "answer": "maybe" }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert_eq!(seen.lock().unwrap().len(), before);
    }

    fn bench_repo(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ebm-routes-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The bench editor's routes on a repo with nothing in it: the built-ins
    /// and the vocabulary are the binary's, and there are no benches.
    #[tokio::test]
    async fn the_bench_routes_answer_an_empty_repo() {
        let repo = bench_repo("empty");
        let base = serve_app_with_repo(&repo).await;
        let http = reqwest::Client::new();
        let get = |path: &str| http.get(format!("{base}{path}")).send();
        let vocab: serde_json::Value = get("/api/bench/vocabulary").await.unwrap().json().await.unwrap();
        assert_eq!(vocab["hub_kind"], "usb-host");
        let parts: serde_json::Value = get("/api/bench/parts").await.unwrap().json().await.unwrap();
        assert_eq!(parts["parts"].as_array().unwrap().len(), 12);
        assert_eq!(parts["errors"], serde_json::json!([]));
        let benches: serde_json::Value = get("/api/bench/benches").await.unwrap().json().await.unwrap();
        assert_eq!(benches, serde_json::json!({ "benches": [], "errors": [] }));
        let missing = get("/api/bench/benches/nope").await.unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A file `POST /api/topology/profiles` wrote loads as parts, is not
    /// rewritten by being read, saves in the new shape, and still lists and
    /// refuses an old-style overwrite through the topology routes.
    #[tokio::test]
    async fn a_legacy_bench_loads_as_parts_and_saves_in_the_new_shape() {
        let repo = bench_repo("legacy");
        let dir = repo.join("embarch").join("topologies");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = "name = \"BLE bench\"\nsaved_at_utc_ms = 1755000000000\n\n[dut]\nboard = \"wearable-rev6\"\nchip = \"nRF54L15\"\nprobe_serial = \"001057729826\"\n\n[dev-bench]\nboard = \"nrf54l15dk/nrf54l15/cpuapp\"\nchip = \"nRF54L15\"\nprobe_serial = \"001050288460\"\nlink_port_serial = \"D607104\"\nlink_port_interface = 2\n";
        std::fs::write(dir.join("ble-bench.toml"), legacy).unwrap();
        std::fs::write(dir.join("broken.toml"), "name = \n").unwrap();
        let base = serve_app_with_repo(&repo).await;
        let http = reqwest::Client::new();

        let list: serde_json::Value = http.get(format!("{base}/api/bench/benches")).send().await.unwrap().json().await.unwrap();
        assert_eq!(list["benches"], serde_json::json!([{ "slug": "ble-bench", "name": "BLE bench", "legacy": true }]));
        assert_eq!(list["errors"][0]["file"], "embarch/topologies/broken.toml");

        let bench: serde_json::Value = http.get(format!("{base}/api/bench/benches/ble-bench")).send().await.unwrap().json().await.unwrap();
        assert_eq!(bench["legacy"], true);
        assert_eq!(bench["parts"].as_array().unwrap().len(), 3);
        assert_eq!(bench["cables"][1], serde_json::json!({ "from": "dut.usb", "to": "host" }));
        assert_eq!(std::fs::read_to_string(dir.join("ble-bench.toml")).unwrap(), legacy, "reading never rewrites");

        let check: serde_json::Value = http
            .post(format!("{base}/api/bench/check"))
            .json(&serde_json::json!({ "bench": bench, "config": null }))
            .send().await.unwrap().json().await.unwrap();
        assert_eq!(check, serde_json::json!({ "findings": [] }));
        let cands: serde_json::Value = http
            .post(format!("{base}/api/bench/candidates"))
            .json(&serde_json::json!({ "bench": bench, "config": null, "from": "host" }))
            .send().await.unwrap().json().await.unwrap();
        assert_eq!(cands["verdicts"]["dut.usb"]["fit"], "refused");
        assert_eq!(cands["verdicts"]["dev-bench.radio"], serde_json::json!({ "fit": "hidden" }));
        let bad = http
            .post(format!("{base}/api/bench/check"))
            .json(&serde_json::json!({ "bench": bench, "config": "nope" }))
            .send().await.unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        let saved = http.put(format!("{base}/api/bench/benches/ble-bench")).json(&bench).send().await.unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        let saved: serde_json::Value = saved.json().await.unwrap();
        assert_eq!(saved["legacy"], false);
        assert_eq!(saved["parts"], bench["parts"]);
        assert_ne!(saved["version"], bench["version"], "a save moves the version");
        let text = std::fs::read_to_string(dir.join("ble-bench.toml")).unwrap();
        assert!(text.contains("[[parts]]") && !text.contains("[dut]"), "{text}");

        // Fix round 3, M1(b): a USB key that is no connector of the part is
        // refused before anything is written; the saved file is untouched.
        let before = std::fs::read(dir.join("ble-bench.toml")).unwrap();
        let mut hostile = saved.clone();
        hostile["parts"][1]["usb"]["a\nb\"'''"] = serde_json::json!({ "probe": "X" });
        let refused = http.put(format!("{base}/api/bench/benches/ble-bench")).json(&hostile).send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert!(refused.text().await.unwrap().contains("is not a connector of `nrf54l15dk`"));
        assert_eq!(std::fs::read(dir.join("ble-bench.toml")).unwrap(), before);

        // A second window holding the old version cannot overwrite the save.
        let stale = http.put(format!("{base}/api/bench/benches/ble-bench")).json(&bench).send().await.unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        assert!(stale.text().await.unwrap().contains("changed since it was read"));
        let mut unversioned = saved.clone();
        unversioned["version"] = serde_json::Value::Null;
        let refused = http.put(format!("{base}/api/bench/benches/ble-bench")).json(&unversioned).send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        let not_slug = http.get(format!("{base}/api/bench/benches/BLE-Bench")).send().await.unwrap();
        assert_eq!(not_slug.status(), StatusCode::BAD_REQUEST, "a path is never slugified into another bench");

        // The topology routes read the same file, roles derived from the parts,
        // and neither the old save nor the old delete touches it.
        let profiles: serde_json::Value = http.get(format!("{base}/api/topology/profiles")).send().await.unwrap().json().await.unwrap();
        let row = profiles["profiles"].as_array().unwrap().iter().find(|p| p["slug"] == "ble-bench").unwrap().clone();
        assert_eq!(row["dev_bench"]["link_port_interface"], 2);
        assert_eq!(row["dut"]["probe_serial"], "001057729826");
        let overwrite = http.post(format!("{base}/api/topology/profiles")).json(&serde_json::json!({ "name": "BLE bench" })).send().await.unwrap();
        assert_eq!(overwrite.status(), StatusCode::CONFLICT);
        let delete = http.delete(format!("{base}/api/topology/profiles/ble-bench")).send().await.unwrap();
        assert_eq!(delete.status(), StatusCode::CONFLICT);
        // A new-shape file with one typo is not overwritten either.
        std::fs::write(dir.join("typo.toml"), "name = \"Typo\"\n[[parts]\nid = \"a\"\n").unwrap();
        let typo = http.post(format!("{base}/api/topology/profiles")).json(&serde_json::json!({ "name": "Typo" })).send().await.unwrap();
        assert_eq!(typo.status(), StatusCode::CONFLICT);
        assert!(typo.text().await.unwrap().contains("does not parse"));
        assert!(std::fs::read_to_string(dir.join("typo.toml")).unwrap().contains("[[parts]\n"));

        // Renaming is refused; a new bench is created with POST, once.
        let mut renamed = saved.clone();
        renamed["name"] = serde_json::json!("Other bench");
        let refused = http.put(format!("{base}/api/bench/benches/ble-bench")).json(&renamed).send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        let created = http.post(format!("{base}/api/bench/benches")).json(&renamed).send().await.unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let created: serde_json::Value = created.json().await.unwrap();
        assert_eq!(created["slug"], "other-bench");
        assert!(created["version"].is_string());
        let again = http.post(format!("{base}/api/bench/benches")).json(&renamed).send().await.unwrap();
        assert_eq!(again.status(), StatusCode::CONFLICT);
        let mut onto_broken = renamed.clone();
        onto_broken["name"] = serde_json::json!("Broken");
        let onto = http.post(format!("{base}/api/bench/benches")).json(&onto_broken).send().await.unwrap();
        assert_eq!(onto.status(), StatusCode::CONFLICT, "a file that does not parse is still a file");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Fix round 1, H1: a bench the editor saved records no chip for a
    /// supported dev-bench board, so apply looks it up and the proposal
    /// carries it; an enrol for the dev-bench role with no chip finds the same
    /// one rather than refusing. A recorded chip is only the fallback, and a
    /// role with no probe says so.
    #[tokio::test]
    async fn apply_proposes_the_chip_it_looked_up_and_says_when_a_role_has_no_probe() {
        let repo = bench_repo("apply");
        let dir = repo.join("embarch").join("topologies");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pd.toml"),
            "name = \"pd\"\n\n[[parts]]\nid = \"host\"\ntemplate = \"this-machine\"\n\n[[parts]]\nid = \"dev-bench\"\ntemplate = \"nrf54l15dk\"\nrole = \"dev-bench\"\nboard = \"nrf54l15dk/nrf54l15/cpuapp\"\nusb.usb = { probe = \"001050288460\", vcom = 2 }\n\n[[parts]]\nid = \"dut\"\ntemplate = \"generic-board\"\nrole = \"dut\"\nboard = \"plank\"\nchip = { board = \"plank\", chip = \"STM32G0B1VE\" }\n",
        )
        .unwrap();
        let base = serve_app_with_repo(&repo).await;
        let http = reqwest::Client::new();
        let report: serde_json::Value = http.post(format!("{base}/api/topology/profiles/pd/apply")).send().await.unwrap().json().await.unwrap();
        let proposals = report["proposals"].as_array().unwrap();
        assert_eq!(proposals.len(), 1, "{report}");
        assert_eq!(proposals[0]["role"], "dev-bench");
        assert_eq!(proposals[0]["chip"], "nRF54L15", "the looked-up chip, not the empty recorded one");
        assert_eq!(proposals[0]["link_port_interface"], 2);
        let applied = report["applied"].as_array().unwrap();
        assert!(applied.iter().any(|a| a["detail"] == "dut: no probe on its part — not proposed"), "{report}");
        // The DUT's board is in no catalog; the recorded chip is the fallback, so the
        // board write reached Core (unreachable here) rather than being refused.
        let dut_board = applied.iter().find(|a| a["what"] == "dut board").unwrap();
        assert!(!dut_board["detail"].as_str().unwrap().contains("has no chip"), "{report}");

        let enrol = http
            .post(format!("{base}/api/enroll"))
            .json(&serde_json::json!({ "role": "dev-bench", "chip": "", "name": "nrf54l15dk/nrf54l15/cpuapp", "probe_serial": "001050288460" }))
            .send()
            .await
            .unwrap();
        assert_eq!(enrol.status(), StatusCode::BAD_GATEWAY, "the chip was found; only the unreachable Core failed");
        let refused = http
            .post(format!("{base}/api/enroll"))
            .json(&serde_json::json!({ "role": "dut", "chip": "", "name": "plank", "probe_serial": "X" }))
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
        assert!(refused.text().await.unwrap().contains("it is not a row in embarch/boards.toml"), "the refusal says why");

        // Fix round 2: a chip recorded for another board is not used, and an
        // enrolment with no chip — or whose SWD leads reach two probes — is not
        // proposed; each says why.
        std::fs::create_dir_all(repo.join("embarch/parts")).unwrap();
        std::fs::write(
            repo.join("embarch/parts/swd-board.toml"),
            "id = \"swd-board\"\nlabel = \"SWD board\"\ncategory = \"board\"\nlook = \"board\"\n[[connectors]]\nid = \"swd\"\nlabel = \"SWD\"\nkind = \"swd\"\nside = \"left\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("pd2.toml"),
            "name = \"pd2\"\n\n[[parts]]\nid = \"host\"\ntemplate = \"this-machine\"\n\n[[parts]]\nid = \"p1\"\ntemplate = \"debug-probe\"\nusb.usb = { probe = \"J1\" }\n\n[[parts]]\nid = \"p2\"\ntemplate = \"debug-probe\"\nusb.usb = { probe = \"J2\" }\n\n[[parts]]\nid = \"bench\"\ntemplate = \"swd-board\"\nrole = \"dev-bench\"\nboard = \"nrf54l15dk/nrf54l15/cpuapp\"\n\n[[parts]]\nid = \"dut\"\ntemplate = \"generic-board\"\nrole = \"dut\"\nboard = \"plank\"\nchip = { board = \"wearable-rev6\", chip = \"nRF54L15\" }\nusb.usb = { probe = \"X1\" }\n\n[[configs]]\nname = \"A\"\ncables = [[\"p1.swd\", \"bench.swd\"]]\n\n[[configs]]\nname = \"B\"\ncables = [[\"p2.swd\", \"bench.swd\"]]\n",
        )
        .unwrap();
        let report: serde_json::Value = http.post(format!("{base}/api/topology/profiles/pd2/apply")).send().await.unwrap().json().await.unwrap();
        assert_eq!(report["proposals"], serde_json::json!([]), "{report}");
        let detail = |what: &str| report["applied"].as_array().unwrap().iter().find(|a| a["what"] == what).map(|a| a["detail"].as_str().unwrap().to_string()).unwrap_or_default();
        assert_eq!(detail("dev-bench probe"), "dev-bench: its SWD leads reach different probes (J1 in config “A”; J2 in config “B”) — not proposed");
        assert!(detail("dut board").contains("(recorded chip was for wearable-rev6, not used)"), "{report}");
        assert!(detail("dut probe").starts_with("dut: not proposed — 'plank' has no chip"), "{report}");

        // Fix round 3: the first bench proposed a dev-bench enrolment, so its
        // link waits for it; a cleared board never keeps the old board's chip,
        // and a dev-bench link with no enrolment proposed says what to do.
        let first: serde_json::Value = http.post(format!("{base}/api/topology/profiles/pd/apply")).send().await.unwrap().json().await.unwrap();
        let link = first["applied"].as_array().unwrap().iter().find(|a| a["what"] == "dev-bench link").unwrap().clone();
        assert!(link["detail"].as_str().unwrap().starts_with("deferred:"), "{first}");
        std::fs::write(
            dir.join("pd3.toml"),
            "name = \"pd3\"\n\n[[parts]]\nid = \"host\"\ntemplate = \"this-machine\"\n\n[[parts]]\nid = \"bench\"\ntemplate = \"nrf54l15dk\"\nrole = \"dev-bench\"\nboard = \"nrf54l15dk/nrf54l15/cpuapp\"\nusb.usb = { vcom = { serial = \"D607104\", interface = 2 } }\n\n[[parts]]\nid = \"dut\"\ntemplate = \"generic-board\"\nrole = \"dut\"\nchip = { board = \"wearable-rev6\", chip = \"nRF54L15\" }\nusb.usb = { probe = \"X1\" }\n",
        )
        .unwrap();
        let third: serde_json::Value = http.post(format!("{base}/api/topology/profiles/pd3/apply")).send().await.unwrap().json().await.unwrap();
        assert_eq!(third["proposals"], serde_json::json!([]), "{third}");
        let detail = |what: &str| third["applied"].as_array().unwrap().iter().find(|a| a["what"] == what).map(|a| a["detail"].as_str().unwrap().to_string()).unwrap_or_default();
        assert_eq!(
            detail("dut probe"),
            "dut: not proposed — no board type to look a chip up for, and the recorded chip was for wearable-rev6, not used"
        );
        assert!(detail("dev-bench link").starts_with("not declared:") && detail("dev-bench link").contains("proposes no dev-bench enrolment"), "{third}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Fix round 2: the old Delete never follows a linked
    /// `embarch/topologies` out of the repo.
    #[tokio::test]
    async fn the_old_delete_never_removes_a_file_outside_the_repo() {
        let base_dir = bench_repo("delete-link");
        let repo = base_dir.join("repo");
        let outside = base_dir.join("outside");
        std::fs::create_dir_all(repo.join("embarch")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("x.toml"), "name = \"x\"\n").unwrap();
        std::os::unix::fs::symlink(&outside, repo.join("embarch/topologies")).unwrap();
        let base = serve_app_with_repo(&repo).await;
        let resp = reqwest::Client::new().delete(format!("{base}/api/topology/profiles/x")).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(resp.text().await.unwrap().contains("outside the repo"));
        assert!(outside.join("x.toml").exists());
        let _ = std::fs::remove_dir_all(&base_dir);
    }

    /// Parts: `POST` creates once, `PUT` names the version it read, and a
    /// built-in's id is refused naming the built-in.
    #[tokio::test]
    async fn a_repo_part_is_created_once_updated_by_version_and_a_builtin_id_is_refused() {
        let repo = bench_repo("parts");
        let base = serve_app_with_repo(&repo).await;
        let http = reqwest::Client::new();
        let part = serde_json::json!({
            "id": "pd-charger", "label": "PD charger", "subtitle": null, "category": "board", "look": "power-board",
            "source": "repo", "hub": false, "width": 190, "min_height": null, "rows_top": null,
            "connectors": [{ "id": "usb-c-1", "label": "USB-C 1", "kind": "usb-c", "side": "left", "power": "source", "max_cables": 1, "functions": [] }],
            "passthrough": []
        });
        let created = http.post(format!("{base}/api/bench/parts")).json(&part).send().await.unwrap();
        assert_eq!(created.status(), StatusCode::CREATED);
        let body: serde_json::Value = created.json().await.unwrap();
        let served = body["parts"].as_array().unwrap().iter().find(|p| p["id"] == "pd-charger").unwrap().clone();
        assert_eq!(served["source"], "repo");
        assert!(served["version"].is_string());
        assert_eq!(http.post(format!("{base}/api/bench/parts")).json(&part).send().await.unwrap().status(), StatusCode::CONFLICT);

        let mut edited = served.clone();
        edited["label"] = serde_json::json!("PD charger rev B");
        let put = http.put(format!("{base}/api/bench/parts/pd-charger")).json(&edited).send().await.unwrap();
        assert_eq!(put.status(), StatusCode::OK);
        let stale = http.put(format!("{base}/api/bench/parts/pd-charger")).json(&edited).send().await.unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT, "the version moved");
        let unversioned = http.put(format!("{base}/api/bench/parts/pd-charger")).json(&part).send().await.unwrap();
        assert_eq!(unversioned.status(), StatusCode::BAD_REQUEST);
        let missing = http.put(format!("{base}/api/bench/parts/nothing")).json(&{ let mut p = edited.clone(); p["id"] = "nothing".into(); p }).send().await.unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert!(std::fs::read_to_string(repo.join("embarch/parts/pd-charger.toml")).unwrap().contains("rev B"));

        let mut clash = part.clone();
        clash["id"] = serde_json::json!("cy4500");
        let refused = http.post(format!("{base}/api/bench/parts")).json(&clash).send().await.unwrap();
        assert_eq!(refused.status(), StatusCode::CONFLICT);
        assert!(refused.text().await.unwrap().contains("built-in part `cy4500` (CY4500)"));
        let mismatch = http.put(format!("{base}/api/bench/parts/other")).json(&edited).send().await.unwrap();
        assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Without a project every bench route refuses `409` naming the picker,
    /// the vocabulary and the built-ins included, **whatever the body** — the
    /// repo is extracted before it: an empty list would read as "this repo has
    /// no benches", and a 422 about JSON would hide the real cause.
    #[tokio::test]
    async fn without_a_repo_every_bench_route_refuses_naming_the_picker() {
        let base = serve_app().await;
        let http = reqwest::Client::new();
        let bench = serde_json::json!({ "name": "B", "parts": [] });
        let calls = [
            http.get(format!("{base}/api/bench/vocabulary")),
            http.get(format!("{base}/api/bench/parts")),
            http.post(format!("{base}/api/bench/parts")).json(&serde_json::json!({})),
            http.put(format!("{base}/api/bench/parts/x")).json(&serde_json::json!({})),
            http.get(format!("{base}/api/bench/benches")),
            http.post(format!("{base}/api/bench/benches")).json(&bench),
            http.get(format!("{base}/api/bench/benches/b")),
            http.put(format!("{base}/api/bench/benches/b")).json(&bench),
            http.post(format!("{base}/api/bench/check")).json(&serde_json::json!({ "bench": bench })),
            http.post(format!("{base}/api/bench/candidates")).json(&serde_json::json!({ "bench": bench, "from": "x" })),
            http.post(format!("{base}/api/bench/check")).body("not json"),
            http.put(format!("{base}/api/bench/benches/b")).header("content-type", "application/json").body("{"),
            http.get(format!("{base}/api/bench/live")),
            http.delete(format!("{base}/api/bench/live")),
            http.post(format!("{base}/api/bench/benches/b/live")).json(&serde_json::json!({ "config": null, "version": "v" })),
            http.post(format!("{base}/api/bench/live/retract")).json(&serde_json::json!({ "kind": "signal", "id": "x" })),
            http.post(format!("{base}/api/bench/benches/b/live")).body("not json"),
        ];
        for call in calls {
            let resp = call.send().await.unwrap();
            let what = resp.url().to_string();
            assert_eq!(resp.status(), StatusCode::CONFLICT, "{what}");
            assert_eq!(resp.headers().get(bench::routes::REFUSAL_HEADER).unwrap(), "no-project", "{what}");
            assert_eq!(
                resp.text().await.unwrap(),
                "no project is open — pick a firmware repo in Project, at the bottom of the sidebar",
                "{what}"
            );
        }
    }

    // ---- making a bench live, against a stub Core --------------------------

    /// Core's stores, as a stub holds them: what each GET answers, and every
    /// write it was sent, in order.
    #[derive(Default)]
    struct StubCore {
        signals: Vec<serde_json::Value>,
        instruments: Vec<serde_json::Value>,
        enrolled: Vec<serde_json::Value>,
        bootload: Option<serde_json::Value>,
        writes: Vec<String>,
        /// When set, `PUT /instruments` is refused with this text, as Core
        /// refuses a write under a lease the UI does not hold.
        refuse_instruments: Option<String>,
    }

    type Stub = Arc<std::sync::Mutex<StubCore>>;

    /// A stub Core serving the routes making a bench live reads and writes.
    async fn serve_stub_core(stub: Stub) -> String {
        use axum::extract::{Json as J, Path as P, State as S};
        use axum::routing::{delete, get, put};
        fn named(list: &mut Vec<serde_json::Value>, v: serde_json::Value, key: &str) {
            list.retain(|x| x[key] != v[key]);
            list.push(v);
        }
        let app = axum::Router::new()
            .route(
                "/signals",
                get(|S(s): S<Stub>| async move { axum::Json(serde_json::Value::Array(s.lock().unwrap().signals.clone())) })
                    .post(|S(s): S<Stub>, J(v): J<serde_json::Value>| async move {
                        let mut s = s.lock().unwrap();
                        s.writes.push(format!("POST /signals {}", v["name"].as_str().unwrap_or_default()));
                        named(&mut s.signals, v, "name");
                        StatusCode::NO_CONTENT
                    }),
            )
            .route(
                "/signals/{name}",
                delete(|S(s): S<Stub>, P(name): P<String>| async move {
                    let mut s = s.lock().unwrap();
                    s.writes.push(format!("DELETE /signals/{name}"));
                    let before = s.signals.len();
                    s.signals.retain(|x| x["name"] != name.as_str());
                    if s.signals.len() < before { StatusCode::NO_CONTENT.into_response() } else { (StatusCode::NOT_FOUND, "no such signal").into_response() }
                }),
            )
            .route(
                "/instruments",
                get(|S(s): S<Stub>| async move { axum::Json(serde_json::Value::Array(s.lock().unwrap().instruments.clone())) })
                    .put(|S(s): S<Stub>, J(v): J<serde_json::Value>| async move {
                        let mut s = s.lock().unwrap();
                        s.writes.push(format!("PUT /instruments {v}"));
                        if let Some(text) = s.refuse_instruments.clone() {
                            return (StatusCode::CONFLICT, text).into_response();
                        }
                        named(&mut s.instruments, v.clone(), "name");
                        axum::Json(v).into_response()
                    }),
            )
            .route(
                "/instruments/{name}",
                delete(|S(s): S<Stub>, P(name): P<String>| async move {
                    let mut s = s.lock().unwrap();
                    s.writes.push(format!("DELETE /instruments/{name}"));
                    let before = s.instruments.len();
                    s.instruments.retain(|x| x["name"] != name.as_str());
                    if s.instruments.len() < before { StatusCode::NO_CONTENT.into_response() } else { (StatusCode::NOT_FOUND, "no such instrument").into_response() }
                }),
            )
            .route(
                "/bootload/ports",
                get(|S(s): S<Stub>| async move {
                    match s.lock().unwrap().bootload.clone() {
                        Some(v) => axum::Json(v).into_response(),
                        None => (StatusCode::NOT_FOUND, "no bootload ports declared").into_response(),
                    }
                })
                .put(|S(s): S<Stub>, J(mut v): J<serde_json::Value>| async move {
                    let mut s = s.lock().unwrap();
                    s.writes.push(format!("PUT /bootload/ports {v}"));
                    v["role"] = "dut".into();
                    s.bootload = Some(v.clone());
                    axum::Json(v).into_response()
                })
                .delete(|S(s): S<Stub>| async move {
                    let mut s = s.lock().unwrap();
                    s.writes.push("DELETE /bootload/ports".to_string());
                    match s.bootload.take() {
                        Some(_) => StatusCode::NO_CONTENT.into_response(),
                        None => (StatusCode::NOT_FOUND, "no bootload ports declared").into_response(),
                    }
                }),
            )
            .route("/probes/enrolled", get(|S(s): S<Stub>| async move { axum::Json(serde_json::Value::Array(s.lock().unwrap().enrolled.clone())) }))
            .route(
                "/probes/enrolled/{role}/board",
                put(|S(s): S<Stub>, P(role): P<String>, J(v): J<serde_json::Value>| async move {
                    let mut s = s.lock().unwrap();
                    s.writes.push(format!("PUT /probes/enrolled/{role}/board {v}"));
                    let (board, chip) = (v["board"].clone(), v["chip"].clone());
                    let row = match s.enrolled.iter_mut().find(|r| r["role"] == role.as_str()) {
                        Some(r) => {
                            r["name"] = board;
                            r["chip"] = chip;
                            r.clone()
                        }
                        None => {
                            let r = serde_json::json!({ "role": role, "name": board, "chip": chip });
                            s.enrolled.push(r.clone());
                            r
                        }
                    };
                    axum::Json(row)
                }),
            )
            .route(
                "/probes/enrolled/{role}",
                delete(|S(s): S<Stub>, P(role): P<String>| async move {
                    let mut s = s.lock().unwrap();
                    s.writes.push(format!("DELETE /probes/enrolled/{role}"));
                    let before = s.enrolled.len();
                    s.enrolled.retain(|r| r["role"] != role.as_str());
                    if s.enrolled.len() < before { StatusCode::NO_CONTENT.into_response() } else { (StatusCode::NOT_FOUND, "no such role").into_response() }
                }),
            )
            .route(
                "/probes/by-serial/{serial}",
                delete(|S(s): S<Stub>, P(serial): P<String>| async move {
                    let mut s = s.lock().unwrap();
                    s.writes.push(format!("DELETE /probes/by-serial/{serial}"));
                    let before = s.enrolled.len();
                    s.enrolled.retain(|r| r["probe_serial"] != serial.as_str());
                    if s.enrolled.len() < before { StatusCode::NO_CONTENT.into_response() } else { (StatusCode::NOT_FOUND, "no such probe").into_response() }
                }),
            )
            .route(
                "/dev-bench/link",
                post(|S(s): S<Stub>, J(v): J<serde_json::Value>| async move {
                    s.lock().unwrap().writes.push(format!("POST /dev-bench/link {v}"));
                    StatusCode::NO_CONTENT
                }),
            )
            .with_state(stub);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    /// A Core already holding things the PD bench below does not name: a
    /// stale signal, an old instrument, a bench board on another probe, and
    /// the DUT role on a probe the bench does not bind.
    fn stub_with_leftovers() -> Stub {
        Arc::new(std::sync::Mutex::new(StubCore {
            signals: vec![serde_json::json!({ "name": "stale-sig", "origin_role": "dut", "direction": "bidirectional", "route": { "kind": "direct", "port_serial": "S0" } })],
            instruments: vec![serde_json::json!({ "name": "old-load", "port": { "vid": 1, "pid": 2 }, "baud": 9600 })],
            enrolled: vec![
                serde_json::json!({ "role": "", "name": "old-board", "chip": "STM32G0B1VE", "probe_serial": "OLD1" }),
                serde_json::json!({ "role": "dut", "name": "x", "chip": "nRF54L15", "probe_serial": "Z9" }),
            ],
            ..StubCore::default()
        }))
    }

    /// The PD bench: a dev-bench DK, a DUT with a bootload pair, the partner
    /// Nucleo as a bench board, an SCPI load, a console signal.
    fn pd_live_repo(tag: &str) -> std::path::PathBuf {
        let repo = bench_repo(tag);
        std::fs::create_dir_all(repo.join("embarch/parts")).unwrap();
        std::fs::create_dir_all(repo.join("embarch/topologies")).unwrap();
        std::fs::write(
            repo.join("embarch/parts/pd-dut.toml"),
            "id = \"pd-dut\"\nlabel = \"PD DUT\"\ncategory = \"board\"\nlook = \"power-board\"\n\n[[connectors]]\nid = \"usb\"\nlabel = \"USB\"\nkind = \"usb-host\"\nside = \"left\"\nfunctions = [{ function = \"probe\" }, { function = \"bootload-app\" }, { function = \"bootload-bootloader\" }]\n",
        )
        .unwrap();
        std::fs::write(
            repo.join("embarch/topologies/pd.toml"),
            r#"name = "pd"
saved_at_utc_ms = 1

[[parts]]
id = "host"
template = "this-machine"

[[parts]]
id = "bench"
template = "nrf54l15dk"
role = "dev-bench"
board = "nrf54l15dk/nrf54l15/cpuapp"
usb = { usb = { probe = "B1", vcom = { serial = "B1V", interface = 2 } } }

[[parts]]
id = "dut"
template = "pd-dut"
role = "dut"
board = "plank"
chip = { board = "plank", chip = "STM32G0B1VE" }
usb = { usb = { probe = "D1", bootload-app = { vid = 12259, pid = 256, serial = "APP" }, bootload-bootloader = { vid = 12259, pid = 512 } } }

[[parts]]
id = "partner"
template = "nucleo-g0b1re"
label = "pd-bench"
chip = { board = "", chip = "STM32G0B1VE" }
usb = { stlink = { probe = "N1" } }

[[parts]]
id = "load"
template = "scpi-load"
label = "eload"
usb = { usb = { scpi = { vid = 6790, pid = 29987 } } }

[[cables]]
from = "bench.usb"
to = "host"

[[cables]]
from = "dut.usb"
to = "host"

[[cables]]
from = "partner.stlink"
to = "host"

[[cables]]
from = "load.usb"
to = "host"

[[configs]]
name = "A"
cables = []

[[signals]]
name = "console"
origin_role = "dut"
direction = "bidirectional"

[signals.route]
kind = "direct"
port_serial = "C1"
"#,
        )
        .unwrap();
        repo
    }

    async fn version_of_bench(http: &reqwest::Client, base: &str, slug: &str) -> String {
        let bench: serde_json::Value = http.get(format!("{base}/api/bench/benches/{slug}")).send().await.unwrap().json().await.unwrap();
        bench["version"].as_str().unwrap().to_string()
    }

    async fn make_live(http: &reqwest::Client, base: &str, version: &str) -> reqwest::Response {
        http.post(format!("{base}/api/bench/benches/pd/live"))
            .json(&serde_json::json!({ "config": "A", "version": version }))
            .send()
            .await
            .unwrap()
    }

    /// Phase 4: every kind of declaration applies through Core, each with its
    /// own row; enrolments come back as proposals (a role's named by its board
    /// type, a bench board's by its label); what Core holds that the bench
    /// does not name comes back as leftovers and **nothing is retracted**.
    #[tokio::test]
    async fn making_a_bench_live_applies_each_kind_and_lists_leftovers_without_retracting() {
        let stub = stub_with_leftovers();
        let core = serve_stub_core(stub.clone()).await;
        let repo = pd_live_repo("live-apply");
        let base = serve_app_live(&core, Some(repo.clone()), Some(repo.join("state/live-benches.json"))).await;
        let http = reqwest::Client::new();
        let version = version_of_bench(&http, &base, "pd").await;
        let resp = make_live(&http, &base, &version).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let report: serde_json::Value = resp.json().await.unwrap();

        let rows = report["applied"].as_array().unwrap();
        let find = |what: &str| rows.iter().find(|r| r["what"].as_str().unwrap().starts_with(what)).unwrap_or_else(|| panic!("no {what} row in {report}")).clone();
        assert_eq!(find("signal console"), serde_json::json!({ "what": "signal console", "route": "POST /signals", "ok": true, "text": "declared" }));
        assert_eq!(find("dev-bench board")["what"], "dev-bench board nrf54l15dk/nrf54l15/cpuapp (nRF54L15)");
        assert_eq!(find("dev-bench board")["route"], "PUT /probes/enrolled/dev-bench/board");
        assert_eq!(find("dut board")["what"], "dut board plank (STM32G0B1VE)", "the recorded chip, its board still the part's");
        assert_eq!(find("instrument eload")["text"], "declared on 1a86:7523 at 9600 baud");
        assert!(find("bootload ports")["ok"].as_bool().unwrap(), "{report}");
        let link = find("dev-bench link");
        assert_eq!(link["ok"], false);
        assert!(link["text"].as_str().unwrap().contains("confirm its enrolment below, then make this bench live again"), "{link}");
        assert!(rows.iter().all(|r| r["ok"] == true || r["what"] == "dev-bench link"), "{report}");

        let proposals = report["proposals"].as_array().unwrap();
        let by_part = |part: &str| proposals.iter().find(|p| p["part"] == part).unwrap_or_else(|| panic!("no proposal for {part}: {report}")).clone();
        let dut = by_part("dut");
        assert_eq!((dut["kind"].as_str(), dut["role"].as_str(), dut["name"].as_str(), dut["chip"].as_str(), dut["probe_serial"].as_str()),
            (Some("enroll"), Some("dut"), Some("plank"), Some("STM32G0B1VE"), Some("D1")));
        assert!(dut["text"].as_str().unwrap().contains("displaces x on probe Z9"), "{dut}");
        assert_eq!(by_part("bench")["chip"], "nRF54L15");
        let partner = by_part("partner");
        assert_eq!((partner["role"].clone(), partner["name"].as_str(), partner["probe_serial"].as_str()), (serde_json::Value::Null, Some("pd-bench"), Some("N1")));
        assert_eq!(report["not_proposed"], serde_json::json!([]));

        let left: Vec<(String, String)> = report["leftovers"].as_array().unwrap().iter()
            .map(|l| (l["kind"].as_str().unwrap().to_string(), l["id"].as_str().unwrap().to_string())).collect();
        assert_eq!(left, [
            ("signal".to_string(), "stale-sig".to_string()),
            ("instrument".to_string(), "old-load".to_string()),
            ("bench-board".to_string(), "OLD1".to_string()),
            ("role".to_string(), "dut".to_string()),
        ]);
        assert_eq!(report["live"]["slug"], "pd");
        assert_eq!(report["live"]["config"], "A");
        assert_eq!(report["live"]["missing"], false);

        let writes = stub.lock().unwrap().writes.clone();
        assert!(writes.iter().all(|w| !w.starts_with("DELETE")), "making a bench live retracts nothing: {writes:?}");
        let inst = writes.iter().find(|w| w.starts_with("PUT /instruments")).unwrap();
        for fact in ["\"baud\":9600", "\"write_terminator\":\"\\n\"", "\"read_terminator\":\"\\r\\n\"", "\"min_interval_ms\":200", "\"ack_ok\":\"Rexecu success\"", "\"vid\":6790"] {
            assert!(inst.contains(fact), "{fact} missing from {inst}");
        }
        let boot = writes.iter().find(|w| w.starts_with("PUT /bootload/ports")).unwrap();
        assert!(boot.contains("\"serial\":\"APP\"") && boot.contains("\"pid\":512"), "{boot}");

        let live: serde_json::Value = http.get(format!("{base}/api/bench/live")).send().await.unwrap().json().await.unwrap();
        assert_eq!(live["live"]["slug"], "pd");
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// A Core refusal — here a lease the UI does not hold — is one row with
    /// Core's words; the rest still apply.
    #[tokio::test]
    async fn a_core_refusal_is_one_row_and_the_rest_still_apply() {
        let stub = stub_with_leftovers();
        stub.lock().unwrap().refuse_instruments = Some("the bench lease is held by agent-7".to_string());
        let core = serve_stub_core(stub.clone()).await;
        let repo = pd_live_repo("live-refusal");
        let base = serve_app_live(&core, Some(repo.clone()), Some(repo.join("state/live-benches.json"))).await;
        let http = reqwest::Client::new();
        let version = version_of_bench(&http, &base, "pd").await;
        let report: serde_json::Value = make_live(&http, &base, &version).await.json().await.unwrap();
        let rows = report["applied"].as_array().unwrap();
        let refused: Vec<_> = rows.iter().filter(|r| r["ok"] == false && r["what"] != "dev-bench link").collect();
        assert_eq!(refused.len(), 1, "{report}");
        assert_eq!(refused[0]["what"], "instrument eload");
        assert!(refused[0]["text"].as_str().unwrap().contains("the bench lease is held by agent-7"), "{}", refused[0]);
        assert!(rows.iter().any(|r| r["what"] == "signal console" && r["ok"] == true));
        assert!(rows.iter().any(|r| r["what"] == "bootload ports" && r["ok"] == true));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Retract refuses what the leftovers do not list, removes what they do
    /// through the matching Core delete, and answers the list recomputed.
    /// Making the bench live again re-applies and keeps its `since`; a file
    /// changed since it was read is `version-changed`.
    #[tokio::test]
    async fn retract_takes_only_a_listed_leftover_and_a_re_apply_keeps_the_record() {
        let stub = stub_with_leftovers();
        let core = serve_stub_core(stub.clone()).await;
        let repo = pd_live_repo("live-retract");
        let base = serve_app_live(&core, Some(repo.clone()), Some(repo.join("state/live-benches.json"))).await;
        let http = reqwest::Client::new();
        let not_live = http.post(format!("{base}/api/bench/live/retract")).json(&serde_json::json!({ "kind": "signal", "id": "stale-sig" })).send().await.unwrap();
        assert_eq!((not_live.status(), not_live.headers().get(bench::routes::REFUSAL_HEADER).unwrap().to_str().unwrap()), (StatusCode::CONFLICT, "not-live"));

        let version = version_of_bench(&http, &base, "pd").await;
        let first: serde_json::Value = make_live(&http, &base, &version).await.json().await.unwrap();
        let retract = |kind: &str, id: &str| http.post(format!("{base}/api/bench/live/retract")).json(&serde_json::json!({ "kind": kind, "id": id })).send();

        let unlisted = retract("signal", "console").await.unwrap();
        assert_eq!(unlisted.status(), StatusCode::CONFLICT);
        assert_eq!(unlisted.headers().get(bench::routes::REFUSAL_HEADER).unwrap(), "not-offered");
        assert!(stub.lock().unwrap().writes.iter().all(|w| !w.starts_with("DELETE")), "an unlisted id reaches no Core delete");

        for (kind, id, delete) in [
            ("signal", "stale-sig", "DELETE /signals/stale-sig"),
            ("instrument", "old-load", "DELETE /instruments/old-load"),
            ("bench-board", "OLD1", "DELETE /probes/by-serial/OLD1"),
            ("role", "dut", "DELETE /probes/enrolled/dut"),
        ] {
            let resp = retract(kind, id).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{kind} {id}");
            let body: serde_json::Value = resp.json().await.unwrap();
            assert_eq!(body["ok"], true, "{body}");
            assert_eq!(body["text"], format!("retracted the {kind} `{id}`"));
            assert!(body["leftovers"].as_array().unwrap().iter().all(|l| l["id"] != id), "{body}");
            assert_eq!(stub.lock().unwrap().writes.last().unwrap(), delete);
        }
        let again = retract("signal", "stale-sig").await.unwrap();
        assert_eq!(again.status(), StatusCode::CONFLICT, "gone from the list, so no longer offered");

        // A re-apply (the write-through after a Save) answers the same shape and keeps `since`.
        let second: serde_json::Value = make_live(&http, &base, &version).await.json().await.unwrap();
        assert_eq!(second["live"]["since_utc_ms"], first["live"]["since_utc_ms"]);
        assert_eq!(second["leftovers"], serde_json::json!([]));
        for key in ["applied", "proposals", "not_proposed", "leftovers"] {
            assert!(second[key].is_array(), "{key}");
        }

        std::fs::write(repo.join("embarch/topologies/pd.toml"), std::fs::read_to_string(repo.join("embarch/topologies/pd.toml")).unwrap() + "\n").unwrap();
        let stale = make_live(&http, &base, &version).await;
        assert_eq!((stale.status(), stale.headers().get(bench::routes::REFUSAL_HEADER).unwrap().to_str().unwrap()), (StatusCode::CONFLICT, "version-changed"));
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// The record is this UI's, on this machine: it survives a restart over
    /// the same state, reads `missing` once the bench file is gone, and
    /// clearing it touches nothing in Core.
    #[tokio::test]
    async fn the_live_record_survives_a_restart_and_says_when_its_file_is_gone() {
        let stub = stub_with_leftovers();
        let core = serve_stub_core(stub.clone()).await;
        let repo = pd_live_repo("live-restart");
        let record = repo.join("state/live-benches.json");
        let http = reqwest::Client::new();
        let first = serve_app_live(&core, Some(repo.clone()), Some(record.clone())).await;
        assert_eq!(http.get(format!("{first}/api/bench/live")).send().await.unwrap().json::<serde_json::Value>().await.unwrap(), serde_json::json!({ "live": null }));
        let version = version_of_bench(&http, &first, "pd").await;
        assert_eq!(make_live(&http, &first, &version).await.status(), StatusCode::OK);

        let restarted = serve_app_live(&core, Some(repo.clone()), Some(record.clone())).await;
        let live: serde_json::Value = http.get(format!("{restarted}/api/bench/live")).send().await.unwrap().json().await.unwrap();
        assert_eq!((live["live"]["slug"].as_str(), live["live"]["config"].as_str(), live["live"]["missing"].as_bool()), (Some("pd"), Some("A"), Some(false)));
        assert!(!std::fs::read_to_string(&record).unwrap().is_empty());
        assert!(!repo.join("embarch").read_dir().unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("live")), "never in the firmware repo");

        std::fs::remove_file(repo.join("embarch/topologies/pd.toml")).unwrap();
        let live: serde_json::Value = http.get(format!("{restarted}/api/bench/live")).send().await.unwrap().json().await.unwrap();
        assert_eq!((live["live"]["slug"].as_str(), live["live"]["missing"].as_bool()), (Some("pd"), Some(true)), "points at nothing, and says so");

        let writes = stub.lock().unwrap().writes.len();
        let cleared = http.delete(format!("{restarted}/api/bench/live")).send().await.unwrap();
        assert_eq!(cleared.status(), StatusCode::NO_CONTENT);
        assert_eq!(stub.lock().unwrap().writes.len(), writes, "clearing the record touches nothing in Core");
        assert_eq!(http.get(format!("{restarted}/api/bench/live")).send().await.unwrap().json::<serde_json::Value>().await.unwrap(), serde_json::json!({ "live": null }));
        let _ = std::fs::remove_dir_all(&repo);
    }
}
