//! The Atlas tab's server half: read-only routes over the open project's
//! `embarch/atlas/` (the files `embarch-atlas` writes), and nothing else.
//!
//! **No Core call, no hardware, no write to the atlas.** Everything the map
//! draws — nodes, clusters, positions, statuses, citations — is in the
//! atlas's own `graph.json`, which `embarch-atlas join` (or `graph`) writes;
//! this file lists the atlases, serves that file as it is, and opens the
//! sources a citation points at: a document's card or section, a rendered
//! page, the raw PDF, a code excerpt at the atlas's commit.
//!
//! The one thing written is a render cache of PDF pages, under the atlas
//! directory's existing `cache/`, so a page cited twice is rendered once.
//! It is presentation, not atlas content: deleting it loses nothing.

use std::path::{Path as FsPath, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AppState;

/// Every page render is at this resolution. Served in the index so the
/// browser can place a citation's box (in PDF points) on the image without
/// restating the number.
const PAGE_DPI: u32 = 110;

const NO_PROJECT: &str = "no project is open — pick a firmware repo in Project, at the bottom of the sidebar";

fn refuse(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, msg.into()).into_response()
}

/// The open project and its `embarch/atlas/`; `None` with no project open.
fn atlas_dir(state: &AppState) -> Option<(PathBuf, PathBuf)> {
    let repo = state.study_designer.repo_path()?;
    let dir = repo.join("embarch").join("atlas");
    Some((repo, dir))
}

/// An atlas id is `<target>@<commit12>`; anything else never reaches the
/// filesystem.
fn valid_atlas_id(id: &str) -> bool {
    let Some((target, commit)) = id.split_once('@') else {
        return false;
    };
    !target.is_empty()
        && target.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        && (6..=40).contains(&commit.len())
        && commit.chars().all(|c| c.is_ascii_hexdigit())
}

/// A document id (`<slug>@<rev>`, `sch:<board>`) — no separators, no dots
/// in a row, so it cannot climb out of `docs/`.
fn valid_doc_id(doc: &str) -> bool {
    !doc.is_empty()
        && !doc.contains("..")
        && doc.chars().all(|c| c.is_ascii_alphanumeric() || "_.+@:-".contains(c))
}

fn git(repo: &FsPath, args: &[&str]) -> Option<std::process::Output> {
    std::process::Command::new("git").arg("-C").arg(repo).args(args).output().ok()
}

fn git_line(repo: &FsPath, args: &[&str]) -> Option<String> {
    let out = git(repo, args)?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[derive(Serialize)]
struct AtlasEntry {
    id: String,
    target: Option<String>,
    board: Option<String>,
    commit: Option<String>,
    created: Option<String>,
    /// `graph.json` exists: the map can draw this atlas. An atlas joined
    /// before the export existed lists, and says so, rather than vanishing.
    graph: bool,
    /// Its commit is the checkout's HEAD.
    head: bool,
    /// Commits from the atlas's commit to HEAD, when HEAD descends from it.
    behind: Option<u32>,
}

#[derive(Serialize)]
struct AtlasIndex {
    project: String,
    head: Option<String>,
    atlases: Vec<AtlasEntry>,
    /// The atlas the tab opens on: the one built at HEAD, else the newest
    /// one with a graph.
    default: Option<String>,
    page_dpi: u32,
}

/// `GET /api/atlas` — the open project's atlases, newest first.
pub async fn api_index(State(state): State<AppState>) -> Response {
    let Some((repo, dir)) = atlas_dir(&state) else {
        return refuse(StatusCode::NOT_FOUND, NO_PROJECT);
    };
    let head = git_line(&repo, &["rev-parse", "HEAD"]);
    let mut atlases = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("atlases")) {
        for entry in rd.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_atlas_id(&id) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path().join("atlas.json")) else {
                continue;
            };
            let stamp: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            let s = |k: &str| stamp.get(k).and_then(Value::as_str).map(str::to_string);
            let commit = s("commit");
            let at_head = matches!((&commit, &head), (Some(c), Some(h)) if c == h);
            let behind = match (&commit, &head) {
                (Some(c), Some(_)) if !at_head => git_line(&repo, &["rev-list", "--count", &format!("{c}..HEAD")])
                    .and_then(|n| n.parse().ok()),
                _ => None,
            };
            atlases.push(AtlasEntry {
                graph: entry.path().join("graph.json").is_file(),
                target: s("target"),
                board: s("board"),
                created: s("created"),
                head: at_head,
                behind,
                commit,
                id,
            });
        }
    }
    atlases.sort_by(|a, b| b.created.cmp(&a.created).then(a.id.cmp(&b.id)));
    let default = atlases
        .iter()
        .find(|a| a.head && a.graph)
        .or_else(|| atlases.iter().find(|a| a.graph))
        .map(|a| a.id.clone());
    Json(AtlasIndex {
        project: repo.to_string_lossy().into_owned(),
        head,
        atlases,
        default,
        page_dpi: PAGE_DPI,
    })
    .into_response()
}

/// `GET /api/atlas/{id}/graph` — the atlas's `graph.json`, byte for byte.
pub async fn api_graph(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some((_repo, dir)) = atlas_dir(&state) else {
        return refuse(StatusCode::NOT_FOUND, NO_PROJECT);
    };
    if !valid_atlas_id(&id) {
        return refuse(StatusCode::BAD_REQUEST, "not an atlas id");
    }
    let path = dir.join("atlases").join(&id).join("graph.json");
    match tokio::fs::read(&path).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        Err(_) => refuse(
            StatusCode::NOT_FOUND,
            format!("{id} has no graph.json — run `embarch-atlas graph {id}`"),
        ),
    }
}

/// `GET /api/atlas/doc/{doc}/{*file}` — a translated document's card, outline
/// or tier-2 section, as text. Only those three shapes are served.
pub async fn api_doc_file(State(state): State<AppState>, Path((doc, file)): Path<(String, String)>) -> Response {
    let Some((_repo, dir)) = atlas_dir(&state) else {
        return refuse(StatusCode::NOT_FOUND, NO_PROJECT);
    };
    let file_ok = file == "card.md"
        || file == "toc.md"
        || file
            .strip_prefix("s/")
            .is_some_and(|f| f.ends_with(".md") && !f.contains('/') && !f.contains(".."));
    if !valid_doc_id(&doc) || !file_ok {
        return refuse(StatusCode::BAD_REQUEST, "not a document file");
    }
    match tokio::fs::read_to_string(dir.join("docs").join(&doc).join(&file)).await {
        Ok(text) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response(),
        Err(_) => refuse(StatusCode::NOT_FOUND, format!("{doc} has no {file}")),
    }
}

/// The graph's `docs` table, cached on the file's mtime: page and PDF
/// requests come in bursts against one atlas.
fn doc_pdf(dir: &FsPath, id: &str, doc: &str) -> Option<PathBuf> {
    static CACHE: Mutex<Option<(PathBuf, SystemTime, Value)>> = Mutex::new(None);
    let path = dir.join("atlases").join(id).join("graph.json");
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let mut cache = CACHE.lock().unwrap();
    let fresh = matches!(&*cache, Some((p, t, _)) if *p == path && *t == mtime);
    if !fresh {
        let text = std::fs::read_to_string(&path).ok()?;
        let graph: Value = serde_json::from_str(&text).ok()?;
        *cache = Some((path, mtime, graph.get("docs").cloned().unwrap_or(Value::Null)));
    }
    let docs = &cache.as_ref()?.2;
    docs.get(doc)?.get("pdf")?.as_str().map(PathBuf::from).filter(|p| p.is_file())
}

/// `GET /api/atlas/{id}/page/{doc}/{page}` — one PDF page as a PNG, rendered
/// with `pdftoppm` on first request and served from the cache after that.
pub async fn api_page(
    State(state): State<AppState>,
    Path((id, doc, page)): Path<(String, String, u32)>,
) -> Response {
    let Some((_repo, dir)) = atlas_dir(&state) else {
        return refuse(StatusCode::NOT_FOUND, NO_PROJECT);
    };
    if !valid_atlas_id(&id) || !valid_doc_id(&doc) || page == 0 {
        return refuse(StatusCode::BAD_REQUEST, "not a page");
    }
    let Some(pdf) = doc_pdf(&dir, &id, &doc) else {
        return refuse(StatusCode::NOT_FOUND, format!("{doc}: the source PDF is not on this machine"));
    };
    let cache = dir.join("cache").join("pages").join(doc.replace(':', "-"));
    let png = cache.join(format!("p{page:03}.png"));
    if !png.is_file() {
        if let Err(e) = tokio::fs::create_dir_all(&cache).await {
            return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("page cache: {e}"));
        }
        let prefix = cache.join(format!("p{page:03}"));
        let out = tokio::process::Command::new("pdftoppm")
            .args(["-r", &PAGE_DPI.to_string(), "-png", "-singlefile"])
            .args(["-f", &page.to_string(), "-l", &page.to_string()])
            .arg(&pdf)
            .arg(&prefix)
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() && png.is_file() => {}
            Ok(o) => {
                return refuse(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("pdftoppm: {}", String::from_utf8_lossy(&o.stderr).trim()),
                )
            }
            Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("pdftoppm: {e}")),
        }
    }
    match tokio::fs::read(&png).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "private, max-age=3600")],
            bytes,
        )
            .into_response(),
        Err(e) => refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", png.display())),
    }
}

/// `GET /api/atlas/{id}/pdf/{doc}` — the raw PDF, for the browser's own
/// viewer (the page opens it at `#page=N`).
pub async fn api_pdf(State(state): State<AppState>, Path((id, doc)): Path<(String, String)>) -> Response {
    let Some((_repo, dir)) = atlas_dir(&state) else {
        return refuse(StatusCode::NOT_FOUND, NO_PROJECT);
    };
    if !valid_atlas_id(&id) || !valid_doc_id(&doc) {
        return refuse(StatusCode::BAD_REQUEST, "not a document");
    }
    let Some(pdf) = doc_pdf(&dir, &id, &doc) else {
        return refuse(StatusCode::NOT_FOUND, format!("{doc}: the source PDF is not on this machine"));
    };
    match tokio::fs::read(&pdf).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, "application/pdf")], bytes).into_response(),
        Err(e) => refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{}: {e}", pdf.display())),
    }
}

#[derive(Deserialize)]
pub struct CodeQuery {
    id: String,
    root: String,
    path: String,
    line: Option<u32>,
}

#[derive(Serialize)]
struct CodeExcerpt {
    path: String,
    line: Option<u32>,
    /// `[line number, text]`, a few lines either side of `line`.
    lines: Vec<(u32, String)>,
    /// `unchanged` / `changed` since the atlas's commit (the working tree
    /// against that commit), or `zephyr` for a file in the Zephyr tree,
    /// which the west manifest pins rather than this repo.
    state: &'static str,
    link: String,
}

/// `GET /api/atlas/code?id=&root=fw|zephyr&path=&line=` — the cited lines as
/// they were at the atlas's commit, whether the file has moved on since, and
/// a link that opens it in VS Code.
pub async fn api_code(State(state): State<AppState>, Query(q): Query<CodeQuery>) -> Response {
    let Some((repo, dir)) = atlas_dir(&state) else {
        return refuse(StatusCode::NOT_FOUND, NO_PROJECT);
    };
    if !valid_atlas_id(&q.id)
        || q.path.is_empty()
        || q.path.starts_with('/')
        || q.path.split('/').any(|seg| seg == ".." || seg.is_empty())
    {
        return refuse(StatusCode::BAD_REQUEST, "not a source path");
    }
    let stamp: Value = std::fs::read_to_string(dir.join("atlases").join(&q.id).join("atlas.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null);
    let commit = stamp.get("commit").and_then(Value::as_str).unwrap_or("");
    let (abs, text, state_name) = match q.root.as_str() {
        "fw" => {
            let abs = repo.join(&q.path);
            let at_commit = git(&repo, &["show", &format!("{commit}:{}", q.path)])
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
            let changed = git(&repo, &["diff", "--quiet", commit, "--", &q.path]).map(|o| o.status.code());
            let st = match changed {
                Some(Some(0)) => "unchanged",
                Some(Some(1)) => "changed",
                _ => "unknown",
            };
            (abs, at_commit, st)
        }
        "zephyr" => {
            let base = stamp
                .pointer("/build/zephyr_base")
                .and_then(Value::as_str)
                .map(PathBuf::from);
            let Some(base) = base else {
                return refuse(StatusCode::NOT_FOUND, "the atlas does not name its Zephyr tree");
            };
            let abs = base.join(&q.path);
            (abs.clone(), std::fs::read_to_string(&abs).ok(), "zephyr")
        }
        _ => return refuse(StatusCode::BAD_REQUEST, "root is fw or zephyr"),
    };
    let Some(text) = text else {
        return refuse(StatusCode::NOT_FOUND, format!("{} is not readable at {commit}", q.path));
    };
    let line = q.line.unwrap_or(1).max(1);
    let lo = line.saturating_sub(3).max(1);
    let lines = text
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u32 + 1, l.to_string()))
        .filter(|(n, _)| *n >= lo && *n <= line + 6)
        .collect();
    let abs_s = abs.to_string_lossy().into_owned();
    let link = match std::env::var("WSL_DISTRO_NAME") {
        Ok(distro) if !distro.is_empty() => format!("vscode://vscode-remote/wsl+{distro}{abs_s}:{line}"),
        _ => format!("vscode://file{abs_s}:{line}"),
    };
    Json(CodeExcerpt { path: q.path, line: q.line, lines, state: state_name, link }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The map's vocabulary — layer names, node kinds, status names, symbol
    /// names — comes from the atlas's `graph.json`, never from `atlas.js`
    /// (the same rule `app.js` keeps for the Study Designer's vocabularies).
    /// A copy here would go stale the day `embarch-atlas` renames a layer.
    #[test]
    fn atlas_js_restates_no_served_vocabulary() {
        const ATLAS_JS: &str = include_str!("../assets/atlas.js");
        for word in [
            "\"Middleware\"",
            "\"Drivers\"",
            "SoC peripherals",
            "Components + signals",
            "\"unverified\"",
            "\"mismatch\"",
            "\"Resistor\"",
            "\"Capacitor\"",
            "\"MCU pin\"",
            "\"Peripheral\"",
        ] {
            assert!(!ATLAS_JS.contains(word), "atlas.js restates served vocabulary: {word}");
        }
        assert!(ATLAS_JS.contains("VOC.status") && ATLAS_JS.contains("VOC.layers"));
    }

    #[test]
    fn atlas_ids_are_target_at_commit_and_nothing_else() {
        assert!(valid_atlas_id("demo@0123456789ab"));
        assert!(valid_atlas_id("demo-board_2@abcdef"));
        assert!(!valid_atlas_id("core"));
        assert!(!valid_atlas_id("../x@abcdef"));
        assert!(!valid_atlas_id("core@xyz123"));
        assert!(!valid_atlas_id("core@abc"));
    }

    #[test]
    fn doc_ids_cannot_leave_docs() {
        assert!(valid_doc_id("lsw100@04b"));
        assert!(valid_doc_id("sch:B"));
        assert!(valid_doc_id("mcu-rm@6"));
        assert!(!valid_doc_id("../hardware"));
        assert!(!valid_doc_id("a/b"));
        assert!(!valid_doc_id(""));
    }
}
