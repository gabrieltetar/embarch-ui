//! The bench editor's routes: `/api/bench/…`, JSON shaped as the TOML files.
//!
//! **No route here calls Core.** The vocabulary and the built-ins are served
//! from the binary, the repo's templates and benches are files in the open
//! repo, and the checks are pure. Making a bench live is the apply path's
//! (`/api/topology/profiles/{slug}/apply`), unchanged in phase 1.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;

use super::files::{self, Bench, LoadError};
use super::{builtin_clash, builtins, catalog, check, parts_dir, template_toml, vocabulary_json, PartTemplate};
use crate::topology::{now_utc_ms, profile_path, slugify};
use crate::AppState;

fn refuse(status: StatusCode, text: impl Into<String>) -> Response {
    (status, text.into()).into_response()
}

/// The open repo, or `409` with the sentence the Atlas and Study Designer
/// tabs refuse with.
///
/// **Every bench route refuses without a project, the vocabulary and the
/// built-ins included.** Answered with empty lists, a canvas would draw "no
/// saved benches" — a claim about a repo nobody opened, the unreadable-as-
/// empty failure spec.md rules out — and a parts bin of built-ins alone would
/// look like a repo that describes none of its boards.
fn repo(state: &AppState) -> Result<PathBuf, Box<Response>> {
    state
        .study_designer
        .repo_path()
        .ok_or_else(|| Box::new(refuse(StatusCode::CONFLICT, NO_PROJECT)))
}

const NO_PROJECT: &str = "no project is open — pick a firmware repo in Project, at the bottom of the sidebar";

/// `GET /api/bench/vocabulary` — kinds, which kinds join, power roles,
/// categories, looks and functions, served so the browser restates none.
pub async fn api_vocabulary(State(state): State<AppState>) -> Response {
    if let Err(resp) = repo(&state) {
        return *resp;
    }
    Json(vocabulary_json()).into_response()
}

/// `GET /api/bench/parts` — the built-ins, then the open repo's
/// `embarch/parts/*.toml`, with every file that could not be used in
/// `errors`.
pub async fn api_parts(State(state): State<AppState>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    Json(catalog(Some(&repo))).into_response()
}

/// `PUT /api/bench/parts/{id}` — writes a repo template to
/// `embarch/parts/<id>.toml` and answers the whole catalog again.
///
/// **A built-in's id is refused `409`**, naming the built-in (owner,
/// 2026-11-01). The id in the path must be the body's, and a slug: it is the
/// file name. A `source` key echoed back from `GET` is ignored.
pub async fn api_put_part(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(mut body): Json<serde_json::Value>,
) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    if let Some(obj) = body.as_object_mut() {
        obj.remove("source");
    }
    let template: PartTemplate = match serde_json::from_value(body) {
        Ok(t) => t,
        Err(e) => return refuse(StatusCode::BAD_REQUEST, format!("not a part template: {e}")),
    };
    if template.id != id {
        return refuse(
            StatusCode::BAD_REQUEST,
            format!("the path names `{id}` and the template's id is `{}`; they must be the same", template.id),
        );
    }
    if let Err(e) = template.check() {
        return refuse(StatusCode::BAD_REQUEST, e);
    }
    if let Some(builtin) = builtins().iter().find(|b| b.id == template.id) {
        return refuse(StatusCode::CONFLICT, builtin_clash(&template.id, &builtin.label));
    }
    let text = match template_toml(&template) {
        Ok(t) => t,
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    };
    let dir = parts_dir(&repo);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("failed to create {}: {e}", dir.display()));
    }
    let path = dir.join(format!("{id}.toml"));
    if let Err(e) = std::fs::write(&path, text) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("failed to write {}: {e}", path.display()));
    }
    Json(catalog(Some(&repo))).into_response()
}

/// `GET /api/bench/benches` — every bench in the repo, newest first, and
/// every file that would not load in `errors`.
pub async fn api_benches(State(state): State<AppState>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let (benches, errors) = files::list(&repo);
    Json(json!({ "benches": benches, "errors": errors })).into_response()
}

fn load_refusal(slug: &str, e: LoadError) -> Response {
    match e {
        LoadError::Missing => refuse(StatusCode::NOT_FOUND, format!("no saved bench `{slug}`")),
        LoadError::Unreadable(e) | LoadError::Unparseable(e) => refuse(StatusCode::BAD_REQUEST, e),
    }
}

/// `GET /api/bench/benches/{slug}` — one bench; a legacy file comes back laid
/// out as parts, with `legacy: true`, and is not rewritten.
pub async fn api_bench(State(state): State<AppState>, Path(slug): Path<String>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let slug = slugify(&slug);
    match files::load(&repo, &slug) {
        Ok(bench) => Json(bench).into_response(),
        Err(e) => load_refusal(&slug, e),
    }
}

/// `PUT /api/bench/benches/{slug}` — saves a bench in the new shape and
/// answers it as `GET` now would.
///
/// **The file name is derived from `name`, never taken from the path or the
/// body** (saved-benches.md). A path naming a bench that exists must be the
/// one the name derives: **renaming is refused `409`**, because the old slug
/// is what a desk names (decision 57) and a study will name its configs
/// through it. A path naming no file creates the bench at the derived slug,
/// unless a bench already lives there.
pub async fn api_put_bench(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Json(bench): Json<Bench>,
) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    if let Err(e) = files::check_shape(&bench) {
        return refuse(StatusCode::BAD_REQUEST, e);
    }
    let derived = slugify(&bench.name);
    let asked = slugify(&slug);
    if derived != asked {
        if profile_path(&repo, &asked).exists() {
            return refuse(
                StatusCode::CONFLICT,
                format!(
                    "`{}` names the bench `{derived}`, not `{asked}`: renaming a saved bench is refused, \
                     because a desk and a study name it by its slug. Keep a name that slugs to `{asked}`, \
                     or save a new bench under the new name.",
                    bench.name.trim()
                ),
            );
        }
        if profile_path(&repo, &derived).exists() {
            return refuse(
                StatusCode::CONFLICT,
                format!("a bench named `{derived}` already exists; pick another name, or save to `{derived}` itself"),
            );
        }
    }
    let file = files::file_from_bench(&bench, now_utc_ms());
    if let Err(e) = files::write(&repo, &derived, &file) {
        return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"));
    }
    match files::load(&repo, &derived) {
        Ok(saved) => Json(saved).into_response(),
        Err(e) => load_refusal(&derived, e),
    }
}

#[derive(Debug, Deserialize)]
pub struct CheckRequest {
    pub bench: Bench,
    /// The config whose cables are checked beside the bench-level ones;
    /// `None` checks the bench-level cables alone.
    #[serde(default)]
    pub config: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CandidatesRequest {
    pub bench: Bench,
    #[serde(default)]
    pub config: Option<String>,
    pub from: String,
}

/// The templates the open repo can use, by id.
fn templates(repo: &std::path::Path) -> check::Templates {
    catalog(Some(repo)).by_id()
}

fn config_of<'a>(bench: &'a Bench, name: Option<&str>) -> Result<Option<&'a files::Config>, Box<Response>> {
    match name {
        None => Ok(None),
        Some(name) => bench
            .config(name)
            .map(Some)
            .ok_or_else(|| Box::new(refuse(StatusCode::BAD_REQUEST, format!("this bench has no config named `{name}`")))),
    }
}

/// `POST /api/bench/check` — a bench and a config in, its findings out.
pub async fn api_check(State(state): State<AppState>, Json(req): Json<CheckRequest>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let config = match config_of(&req.bench, req.config.as_deref()) {
        Ok(c) => c,
        Err(resp) => return *resp,
    };
    let findings = check::check(&req.bench, &templates(&repo), config);
    Json(json!({ "findings": findings })).into_response()
}

/// `POST /api/bench/candidates` — one verdict per connector end on the bench
/// for a cable being laid from `from`, so the browser decides nothing.
pub async fn api_candidates(State(state): State<AppState>, Json(req): Json<CandidatesRequest>) -> Response {
    let repo = match repo(&state) {
        Ok(r) => r,
        Err(resp) => return *resp,
    };
    let config = match config_of(&req.bench, req.config.as_deref()) {
        Ok(c) => c,
        Err(resp) => return *resp,
    };
    match check::candidates(&req.bench, &templates(&repo), config, &req.from) {
        Ok(verdicts) => Json(json!({ "verdicts": verdicts })).into_response(),
        Err(e) => refuse(StatusCode::BAD_REQUEST, e),
    }
}
