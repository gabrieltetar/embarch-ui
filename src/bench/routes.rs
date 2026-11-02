//! The bench editor's routes: `/api/bench/…`, JSON shaped as the TOML files.
//!
//! **No route here calls Core.** The vocabulary and the built-ins are served
//! from the binary, the repo's templates and benches are files in the open
//! repo, and the checks are pure. Making a bench live is the apply path's
//! (`/api/topology/profiles/{slug}/apply`), unchanged in phase 1.
//!
//! **Creating and updating are two routes.** `POST` creates and refuses `409`
//! when anything is already at the derived path; `PUT` updates and names the
//! `version` its `GET` returned, refused `409` when the file changed since or
//! does not parse — so two windows cannot silently overwrite each other, and
//! a new bench cannot land on an old one.

use std::path::PathBuf;

use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::json;

use super::files::{self, Bench, LoadError};
use super::{
    builtin_clash, builtins, catalog, check, hold_writes, parse_template, parts_dir, template_toml, version_of,
    vocabulary_json, write_in_repo, PartTemplate, WriteError, WriteMode,
};
use crate::topology::now_utc_ms;
use crate::AppState;

fn refuse(status: StatusCode, text: impl Into<String>) -> Response {
    (status, text.into()).into_response()
}

/// The header naming which of a save's `409`s this is, so the browser keys
/// on a word rather than on the sentence: `version-changed` (the file changed
/// since it was read; reading it again is the way out), `unparseable` (it no
/// longer loads, and is fixed by hand), `rename` (the name no longer derives
/// the slug). The body stays the plain-text sentence every refusal is.
pub const REFUSAL_HEADER: &str = "x-embarch-refusal";

fn refuse_as(status: StatusCode, code: &'static str, text: impl Into<String>) -> Response {
    let mut resp = refuse(status, text);
    resp.headers_mut().insert(REFUSAL_HEADER, axum::http::HeaderValue::from_static(code));
    resp
}

const NO_PROJECT: &str = "no project is open — pick a firmware repo in Project, at the bottom of the sidebar";

/// The open repo, extracted **ahead of any body**, so a request with no
/// project open is refused `409` with the Atlas and Study Designer tabs'
/// sentence whatever its body holds — never a `400`/`415`/`422` about JSON.
///
/// **Every bench route refuses without a project, the vocabulary and the
/// built-ins included.** Answered with empty lists, a canvas would draw "no
/// saved benches" — a claim about a repo nobody opened, the unreadable-as-
/// empty failure spec.md rules out.
pub struct OpenRepo(pub PathBuf);

impl FromRequestParts<AppState> for OpenRepo {
    type Rejection = Response;

    async fn from_request_parts(_parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        state
            .study_designer
            .repo_path()
            .map(OpenRepo)
            .ok_or_else(|| refuse(StatusCode::CONFLICT, NO_PROJECT))
    }
}

fn write_refusal(what: &str, e: WriteError) -> Response {
    match e {
        WriteError::Exists => refuse(
            StatusCode::CONFLICT,
            format!("{what} already exists — open it and save that, or pick another name"),
        ),
        WriteError::Other(e) => refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
}

/// `GET /api/bench/vocabulary` — kinds, which kinds join, sides, power roles,
/// categories, looks and functions, and which kinds take a power role and
/// which functions take an interface or line facts: served so the browser
/// restates none, the part editor included.
pub async fn api_vocabulary(OpenRepo(_): OpenRepo) -> Response {
    Json(vocabulary_json()).into_response()
}

/// `GET /api/bench/parts` — the built-ins, then the open repo's
/// `embarch/parts/*.toml` (each with its file's `version`), with every file
/// that could not be used in `errors`.
pub async fn api_parts(OpenRepo(repo): OpenRepo) -> Response {
    Json(catalog(Some(&repo))).into_response()
}

/// A template from a request body: `source` and `version` (echoed back from
/// `GET`) are not template fields, and the rest must be.
fn template_from(mut body: serde_json::Value) -> Result<(PartTemplate, Option<String>), Box<Response>> {
    let mut version = None;
    if let Some(obj) = body.as_object_mut() {
        obj.remove("source");
        version = obj.remove("version").and_then(|v| v.as_str().map(str::to_string));
    }
    let template: PartTemplate = serde_json::from_value(body)
        .map_err(|e| Box::new(refuse(StatusCode::BAD_REQUEST, format!("not a part template: {e}"))))?;
    template.check().map_err(|e| Box::new(refuse(StatusCode::BAD_REQUEST, e)))?;
    if let Some(builtin) = builtins().iter().find(|b| b.id == template.id) {
        return Err(Box::new(refuse(StatusCode::CONFLICT, builtin_clash(&template.id, &builtin.label))));
    }
    Ok((template, version))
}

fn write_template(repo: &std::path::Path, template: &PartTemplate, mode: WriteMode) -> Result<(), Box<Response>> {
    let text = template_toml(template).map_err(|e| Box::new(refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))))?;
    write_in_repo(repo, &["embarch", "parts"], &format!("{}.toml", template.id), text.as_bytes(), mode)
        .map(|_| ())
        .map_err(|e| Box::new(write_refusal(&format!("embarch/parts/{}.toml", template.id), e)))
}

/// `POST /api/bench/parts` — creates a repo template at
/// `embarch/parts/<id>.toml` and answers the whole catalog, `201`.
///
/// **A built-in's id is refused `409`**, naming the built-in (owner,
/// 2026-11-01), and so is an id whose file already exists, parseable or not.
pub async fn api_post_part(OpenRepo(repo): OpenRepo, Json(body): Json<serde_json::Value>) -> Response {
    let (template, _) = match template_from(body) {
        Ok(t) => t,
        Err(resp) => return *resp,
    };
    let _held = hold_writes();
    if let Err(resp) = write_template(&repo, &template, WriteMode::Create) {
        return *resp;
    }
    (StatusCode::CREATED, Json(catalog(Some(&repo)))).into_response()
}

/// `PUT /api/bench/parts/{id}` — replaces a repo template, naming the
/// `version` `GET /api/bench/parts` served for it; answers the catalog.
///
/// `404` when there is no such file (create it with `POST`), `409` when the
/// file changed since that version or does not parse as a template, `400`
/// when the path and the body's id differ.
pub async fn api_put_part(OpenRepo(repo): OpenRepo, Path(id): Path<String>, Json(body): Json<serde_json::Value>) -> Response {
    let (template, version) = match template_from(body) {
        Ok(t) => t,
        Err(resp) => return *resp,
    };
    if template.id != id {
        return refuse(
            StatusCode::BAD_REQUEST,
            format!("the path names `{id}` and the template's id is `{}`; they must be the same", template.id),
        );
    }
    let Some(version) = version else {
        return refuse(StatusCode::BAD_REQUEST, "an update names the `version` it read; create a new part with POST /api/bench/parts");
    };
    let _held = hold_writes();
    let path = parts_dir(&repo).join(format!("{id}.toml"));
    let current = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return refuse(StatusCode::NOT_FOUND, format!("no part template embarch/parts/{id}.toml; create it with POST /api/bench/parts"))
        }
        Err(e) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, format!("could not read embarch/parts/{id}.toml: {e}")),
    };
    if version_of(&current) != version {
        return refuse(StatusCode::CONFLICT, format!("embarch/parts/{id}.toml changed since it was read; reload it and save again"));
    }
    if let Err(e) = std::str::from_utf8(&current).map_err(|e| e.to_string()).and_then(parse_template) {
        return refuse(
            StatusCode::CONFLICT,
            format!("embarch/parts/{id}.toml does not load as a template ({e}), so it is left alone — fix it by hand"),
        );
    }
    if let Err(resp) = write_template(&repo, &template, WriteMode::Replace) {
        return *resp;
    }
    Json(catalog(Some(&repo))).into_response()
}

/// `GET /api/bench/benches` — every bench in the repo, newest first, and
/// every file that would not load in `errors`.
pub async fn api_benches(OpenRepo(repo): OpenRepo) -> Response {
    let (benches, errors) = files::list(&repo);
    Json(json!({ "benches": benches, "errors": errors })).into_response()
}

fn load_refusal(slug: &str, e: LoadError) -> Response {
    match e {
        LoadError::Missing => refuse(StatusCode::NOT_FOUND, format!("no saved bench `{slug}`")),
        LoadError::Unreadable(e) | LoadError::Unparseable(e) => refuse(StatusCode::BAD_REQUEST, e),
    }
}

/// A path's slug, or the `400` for one that is not a slug: **a path is never
/// slugified into another bench's name** (fix round 1).
fn path_slug(slug: &str) -> Result<&str, Box<Response>> {
    if files::is_slug(slug) {
        Ok(slug)
    } else {
        Err(Box::new(refuse(
            StatusCode::BAD_REQUEST,
            format!("`{slug}` is not a bench slug: lowercase letters and digits, joined by single hyphens"),
        )))
    }
}

/// `GET /api/bench/benches/{slug}` — one bench with its `version`; a legacy
/// file comes back laid out as parts, with `legacy: true`, and is not
/// rewritten.
pub async fn api_bench(OpenRepo(repo): OpenRepo, Path(slug): Path<String>) -> Response {
    let slug = match path_slug(&slug) {
        Ok(s) => s,
        Err(resp) => return *resp,
    };
    match files::load(&repo, slug) {
        Ok(bench) => Json(bench).into_response(),
        Err(e) => load_refusal(slug, e),
    }
}

/// `POST /api/bench/benches` — creates a bench at the slug derived from its
/// `name` (saved-benches.md: derived, never taken), `201` with the bench as
/// `GET` now answers it. **Anything already at that path is `409`**,
/// parseable or not: a new bench never lands on an old one.
pub async fn api_post_bench(OpenRepo(repo): OpenRepo, Json(bench): Json<Bench>) -> Response {
    if let Err(e) = files::check_shape(&bench, &catalog(Some(&repo)).by_id()) {
        return refuse(StatusCode::BAD_REQUEST, e);
    }
    let slug = crate::topology::slugify(&bench.name);
    let _held = hold_writes();
    let file = files::file_from_bench(&bench, now_utc_ms());
    if let Err(e) = files::write(&repo, &slug, &file, WriteMode::Create) {
        return write_refusal(&files::bench_file(&slug), e);
    }
    match files::load(&repo, &slug) {
        Ok(saved) => (StatusCode::CREATED, Json(saved)).into_response(),
        Err(e) => load_refusal(&slug, e),
    }
}

/// `PUT /api/bench/benches/{slug}` — saves a bench in the new shape, naming
/// the `version` its `GET` returned, and answers it as `GET` now would.
///
/// `404` when there is no such bench (create it with `POST`); `409` when the
/// file changed since that version or does not parse (it is left alone), and
/// when `name` no longer derives this slug — **renaming is refused**, because
/// a desk names a bench by its slug (decision 57) and a study will name its
/// configs through it.
pub async fn api_put_bench(OpenRepo(repo): OpenRepo, Path(slug): Path<String>, Json(bench): Json<Bench>) -> Response {
    let slug = match path_slug(&slug) {
        Ok(s) => s.to_string(),
        Err(resp) => return *resp,
    };
    if let Err(e) = files::check_shape(&bench, &catalog(Some(&repo)).by_id()) {
        return refuse(StatusCode::BAD_REQUEST, e);
    }
    let Some(version) = bench.version.clone() else {
        return refuse(StatusCode::BAD_REQUEST, "an update names the `version` it read; create a new bench with POST /api/bench/benches");
    };
    let derived = crate::topology::slugify(&bench.name);
    if derived != slug {
        return refuse_as(
            StatusCode::CONFLICT,
            "rename",
            format!(
                "`{}` names the bench `{derived}`, not `{slug}`: renaming a saved bench is refused, because a \
                 desk and a study name it by its slug. Keep a name that slugs to `{slug}`, or create a new \
                 bench under the new name.",
                bench.name.trim()
            ),
        );
    }
    let _held = hold_writes();
    match files::read_file(&repo, &slug) {
        Ok((_, current)) if current == version => {}
        Ok(_) => {
            return refuse_as(
                StatusCode::CONFLICT,
                "version-changed",
                format!("{} changed since it was read; reload it and save again", files::bench_file(&slug)),
            )
        }
        Err(LoadError::Missing) => {
            return refuse(StatusCode::NOT_FOUND, format!("no saved bench `{slug}`; create it with POST /api/bench/benches"))
        }
        Err(LoadError::Unparseable(e)) => {
            return refuse_as(StatusCode::CONFLICT, "unparseable", format!("{e} — the file does not parse, so it is left alone; fix it by hand"))
        }
        Err(LoadError::Unreadable(e)) => return refuse(StatusCode::INTERNAL_SERVER_ERROR, e),
    }
    let file = files::file_from_bench(&bench, now_utc_ms());
    if let Err(e) = files::write(&repo, &slug, &file, WriteMode::Replace) {
        return write_refusal(&files::bench_file(&slug), e);
    }
    match files::load(&repo, &slug) {
        Ok(saved) => Json(saved).into_response(),
        Err(e) => load_refusal(&slug, e),
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
pub async fn api_check(OpenRepo(repo): OpenRepo, Json(req): Json<CheckRequest>) -> Response {
    if let Err(e) = files::check_size(&req.bench) {
        return refuse(StatusCode::BAD_REQUEST, e);
    }
    let config = match config_of(&req.bench, req.config.as_deref()) {
        Ok(c) => c,
        Err(resp) => return *resp,
    };
    let findings = check::check(&req.bench, &catalog(Some(&repo)).by_id(), config);
    Json(json!({ "findings": findings })).into_response()
}

/// `POST /api/bench/candidates` — one verdict per connector end on the bench
/// for a cable being laid from `from`, so the browser decides nothing; from a
/// stack plug, where its part may be mounted instead (each stack socket ok or
/// refused, the rest hidden).
pub async fn api_candidates(OpenRepo(repo): OpenRepo, Json(req): Json<CandidatesRequest>) -> Response {
    if let Err(e) = files::check_size(&req.bench) {
        return refuse(StatusCode::BAD_REQUEST, e);
    }
    let config = match config_of(&req.bench, req.config.as_deref()) {
        Ok(c) => c,
        Err(resp) => return *resp,
    };
    match check::candidates(&req.bench, &catalog(Some(&repo)).by_id(), config, &req.from) {
        Ok(verdicts) => Json(json!({ "verdicts": verdicts })).into_response(),
        Err(e) => refuse(StatusCode::BAD_REQUEST, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn each_409_on_a_bench_save_names_which_it_is() {
        let repo = std::env::temp_dir().join(format!("ebm-409-{}-{}", std::process::id(), now_utc_ms()));
        std::fs::create_dir_all(repo.join("embarch/topologies")).unwrap();
        let file = repo.join("embarch/topologies/b.toml");
        std::fs::write(&file, "name = \"b\"\nparts = []\n").unwrap();
        let read = files::load(&repo, "b").unwrap();
        let code = |resp: &Response| resp.headers().get(REFUSAL_HEADER).and_then(|v| v.to_str().ok()).map(str::to_string);
        // The file changed since it was read.
        std::fs::write(&file, "name = \"b\"\nsaved_at_utc_ms = 7\nparts = []\n").unwrap();
        let resp = api_put_bench(OpenRepo(repo.clone()), Path("b".into()), Json(read.clone())).await;
        assert_eq!((resp.status(), code(&resp).as_deref()), (StatusCode::CONFLICT, Some("version-changed")));
        // A name that no longer derives the slug.
        let mut renamed = files::load(&repo, "b").unwrap();
        renamed.name = "c".into();
        let resp = api_put_bench(OpenRepo(repo.clone()), Path("b".into()), Json(renamed)).await;
        assert_eq!((resp.status(), code(&resp).as_deref()), (StatusCode::CONFLICT, Some("rename")));
        // A file that no longer parses.
        let fresh = files::load(&repo, "b").unwrap();
        std::fs::write(&file, "name = [").unwrap();
        let mut stale = fresh;
        stale.version = Some(version_of(b"name = ["));
        let resp = api_put_bench(OpenRepo(repo.clone()), Path("b".into()), Json(stale)).await;
        assert_eq!((resp.status(), code(&resp).as_deref()), (StatusCode::CONFLICT, Some("unparseable")));
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_repo_write_never_follows_a_link_out_of_the_repo() {
        let base = std::env::temp_dir().join(format!("ebm-links-{}-{}", std::process::id(), now_utc_ms()));
        let repo = base.join("repo");
        let outside = base.join("outside");
        std::fs::create_dir_all(repo.join("embarch/parts")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("rc"), "precious").unwrap();
        // A committed link at the target is replaced, never written through.
        std::os::unix::fs::symlink(outside.join("rc"), repo.join("embarch/parts/x.toml")).unwrap();
        write_in_repo(&repo, &["embarch", "parts"], "x.toml", b"id = 1", WriteMode::Replace).unwrap();
        assert_eq!(std::fs::read_to_string(outside.join("rc")).unwrap(), "precious");
        assert_eq!(std::fs::read_to_string(repo.join("embarch/parts/x.toml")).unwrap(), "id = 1");
        // Create refuses anything at the path, a dangling link included.
        std::os::unix::fs::symlink(outside.join("gone"), repo.join("embarch/parts/y.toml")).unwrap();
        assert!(matches!(write_in_repo(&repo, &["embarch", "parts"], "y.toml", b"", WriteMode::Create), Err(WriteError::Exists)));
        assert!(!outside.join("gone").exists());
        // A linked directory resolving outside the repo is refused, and nothing is created there.
        std::os::unix::fs::symlink(&outside, repo.join("embarch/topologies")).unwrap();
        let e = write_in_repo(&repo, &["embarch", "topologies"], "b.toml", b"", WriteMode::Replace).unwrap_err();
        assert!(e.to_string().contains("outside the repo"), "{e}");
        assert!(!outside.join("b.toml").exists());
        std::os::unix::fs::symlink(&outside, repo.join("embarch/link")).unwrap();
        assert!(write_in_repo(&repo, &["embarch", "link", "deeper"], "c.toml", b"", WriteMode::Replace).is_err());
        assert!(!outside.join("deeper").exists(), "no directory was made outside");
        let leftovers: Vec<_> = std::fs::read_dir(repo.join("embarch/parts")).unwrap().flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }
}
