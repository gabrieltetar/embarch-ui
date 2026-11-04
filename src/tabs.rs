//! Tab folders: a tab the user's own config adds to the page, served at
//! runtime, with none of its code in this binary (decision 100).
//!
//! `[[tabs]]` in the config names an id and a folder. The folder holds
//! `tab.toml` and the tab's files. Three things come of that:
//!
//! - `/` draws each tab's nav item and panel into `index.html`, because
//!   `app.js` binds the nav items and reads the fragment once, at start-up.
//! - `/tab/<id>/<file>` serves the folder and nothing outside it.
//! - `/api/tab/<id>/<backend>/<rest>` forwards a call to a loopback service the
//!   manifest names, with its token added, and `/api/tab-events/<id>/<backend>/
//!   <path>` polls one of its paths and pushes the answer.
//!
//! **Trust.** A tab's script runs on this UI's own origin and can call
//! everything the page can. That is acceptable only because a tab comes from the
//! user's own config file: nothing a page or a request does can add one.
//!
//! **The folder is read per request**, so an edit to a tab shows on a reload.
//! Only the `[[tabs]]` list needs a restart. A tab that cannot be read is left
//! out and logged, and every other tab still works.

use crate::config::TabConfig;
use crate::desk::{pushed_view, same_origin, send};
use axum::body::Bytes;
use axum::extract::{FromRef, Path, State};
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::Router;
use serde::Deserialize;
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// The manifest's name inside a tab folder.
const MANIFEST: &str = "tab.toml";

/// The ids a tab may not take: the built-in tabs, and the fragments `app.js`
/// already sends to another tab, which would make the tab unreachable by link.
const RESERVED_IDS: [&str; 8] = ["desk", "topology", "study-designer", "live-study", "atlas", "debug", "enroll", "dashboard"];

// Where the drawn fragments go in `assets/index.html`. Each is a comment, so a
// page with no tab is the page as it always was.
const STYLES_MARK: &str = "<!-- tabs:styles -->";
const NAV_MARK: &str = "<!-- tabs:nav -->";
const PANELS_MARK: &str = "<!-- tabs:panels -->";
const SCRIPTS_MARK: &str = "<!-- tabs:scripts -->";

/// Long enough for a backend that does one `claude -p` call, as the desk does.
const CALL_TIMEOUT: Duration = Duration::from_secs(240);

/// The tabs this server was configured with. A tab whose manifest is broken
/// stays in the list, so fixing the file brings it back on the next reload.
#[derive(Default)]
pub struct Tabs {
    tabs: Vec<Tab>,
}

struct Tab {
    id: String,
    folder: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    label: String,
    icon: String,
    panel: String,
    #[serde(default)]
    scripts: Vec<String>,
    #[serde(default)]
    styles: Vec<String>,
    #[serde(default)]
    backends: Vec<Backend>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backend {
    name: String,
    url: String,
    token_file: PathBuf,
}

/// Lowercase letters, digits and dashes: safe in a route and a `data-tab`.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A file a manifest names goes into an HTML attribute and a URL as it is
/// written, so its characters are limited rather than escaped.
fn valid_file_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && name.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

/// `~/x` to the home folder's `x`. Anything else must already be absolute: a
/// relative path would depend on where the launcher happened to start this.
fn expand_home(path: &FsPath) -> Result<PathBuf, String> {
    let expanded = match path.strip_prefix("~") {
        Ok(rest) => {
            let home = std::env::var_os("HOME").ok_or("HOME is not set, so ~ cannot be expanded")?;
            PathBuf::from(home).join(rest)
        }
        Err(_) => path.to_path_buf(),
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Err(format!("{} is not absolute or under ~", path.display()))
    }
}

/// A backend URL is `http://`, a literal loopback address and a port, with
/// nothing else. A name such as `localhost` is refused because this server
/// would be sending the token wherever the name resolves.
fn check_backend_url(raw: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(raw).map_err(|e| format!("{raw}: {e}"))?;
    if url.scheme() != "http" {
        return Err(format!("{raw}: must be http://"));
    }
    if !url.username().is_empty() || url.password().is_some() || url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{raw}: must be only an address and a port"));
    }
    let host = url.host_str().unwrap_or("").trim_start_matches('[').trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(ip) if ip.is_loopback() => Ok(()),
        _ => Err(format!("{raw}: a backend must be a loopback address, such as 127.0.0.1")),
    }
}

impl Manifest {
    fn check(&self) -> Result<(), String> {
        if self.label.trim().is_empty() {
            return Err("label is empty".into());
        }
        if !self.icon.ends_with(".svg") {
            return Err(format!("icon {} is not an .svg", self.icon));
        }
        let files = [&self.icon, &self.panel].into_iter().chain(&self.scripts).chain(&self.styles);
        if let Some(bad) = files.into_iter().find(|f| !valid_file_name(f)) {
            return Err(format!("{bad} is not a plain relative file name"));
        }
        let mut names = HashSet::new();
        for backend in &self.backends {
            if !valid_name(&backend.name) {
                return Err(format!("backend name {:?} must be lowercase letters, digits and dashes", backend.name));
            }
            if !names.insert(&backend.name) {
                return Err(format!("backend {} is named twice", backend.name));
            }
            check_backend_url(&backend.url)?;
            expand_home(&backend.token_file)?;
        }
        Ok(())
    }
}

/// Why a file inside a tab folder was not served.
#[derive(Debug, PartialEq)]
enum FileRefused {
    Outside,
    Missing,
}

/// `rel` under `folder`, or why not. The components are checked first, and the
/// result is compared after symlinks resolve, so a link inside the folder
/// pointing out of it is refused like a `..` is.
fn resolve_inside(folder: &FsPath, rel: &str) -> Result<PathBuf, FileRefused> {
    let rel = FsPath::new(rel);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(FileRefused::Outside);
    }
    let root = folder.canonicalize().map_err(|_| FileRefused::Missing)?;
    let full = root.join(rel).canonicalize().map_err(|_| FileRefused::Missing)?;
    if full.starts_with(&root) {
        Ok(full)
    } else {
        Err(FileRefused::Outside)
    }
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn content_type(path: &FsPath) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// One tab's share of the page.
#[derive(Default)]
struct Drawn {
    styles: String,
    nav: String,
    panels: String,
    scripts: String,
}

impl Tab {
    fn load(&self) -> Result<Manifest, String> {
        let path = resolve_inside(&self.folder, MANIFEST).map_err(|_| format!("no {MANIFEST} in {}", self.folder.display()))?;
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let manifest: Manifest = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        manifest.check().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(manifest)
    }

    fn read_text(&self, file: &str) -> Result<String, String> {
        let path = resolve_inside(&self.folder, file).map_err(|why| format!("{file}: {why:?} in {}", self.folder.display()))?;
        std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The panel and the icon are written in as they are, not escaped. They
    /// are the tab's own markup, from the same folder as its script. The icon
    /// goes inline so it follows the theme through `currentColor`, as the
    /// built-in icons do.
    fn draw(&self, into: &mut Drawn) -> Result<(), String> {
        let m = self.load()?;
        let panel = self.read_text(&m.panel)?;
        let icon = self.read_text(&m.icon)?;
        let id = &self.id;
        for style in &m.styles {
            into.styles.push_str(&format!("<link rel=\"stylesheet\" href=\"/tab/{id}/{style}\">\n"));
        }
        for script in &m.scripts {
            into.scripts.push_str(&format!("<script src=\"/tab/{id}/{script}\"></script>\n"));
        }
        into.nav.push_str(&format!(
            "<button class=\"nav-item\" data-tab=\"{id}\">\n{icon}\n<span class=\"nav-label\">{}</span>\n</button>\n",
            escape_html(&m.label)
        ));
        into.panels.push_str(&format!("<section class=\"tab-panel\" data-tab=\"{id}\">\n{panel}\n</section>\n"));
        Ok(())
    }
}

impl Tabs {
    /// Keeps the configured tabs whose id and folder can be used, and logs the
    /// rest. A manifest is read here only so a broken one shows in the log at
    /// start-up, before anyone opens the page.
    pub fn new(configured: Vec<TabConfig>) -> Self {
        let mut tabs: Vec<Tab> = Vec::new();
        for c in configured {
            let checked = check_id(&c.id, &tabs).and_then(|()| expand_home(&c.folder));
            match checked {
                Ok(folder) => {
                    let tab = Tab { id: c.id, folder };
                    if let Err(e) = tab.load() {
                        tracing::error!("tab {} is left out: {e}", tab.id);
                    }
                    tabs.push(tab);
                }
                Err(e) => tracing::error!("tab {} is left out: {e}", c.id),
            }
        }
        Tabs { tabs }
    }

    /// The page with every readable tab drawn in. `None` when there is none to
    /// draw, so the caller serves the built-in page untouched.
    pub fn render_index(&self, page: &str) -> Option<String> {
        let mut drawn = Drawn::default();
        for tab in &self.tabs {
            let mut one = Drawn::default();
            match tab.draw(&mut one) {
                Ok(()) => {
                    drawn.styles.push_str(&one.styles);
                    drawn.nav.push_str(&one.nav);
                    drawn.panels.push_str(&one.panels);
                    drawn.scripts.push_str(&one.scripts);
                }
                Err(e) => tracing::error!("tab {} is left out: {e}", tab.id),
            }
        }
        if drawn.nav.is_empty() {
            return None;
        }
        // The panels go last, so nothing in a panel can be taken for a mark.
        Some(
            page.replacen(STYLES_MARK, &drawn.styles, 1)
                .replacen(NAV_MARK, &drawn.nav, 1)
                .replacen(SCRIPTS_MARK, &drawn.scripts, 1)
                .replacen(PANELS_MARK, &drawn.panels, 1),
        )
    }

    fn find(&self, id: &str) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    /// The backend a call names, or the status and reason to answer with.
    fn backend(&self, id: &str, name: &str) -> Result<Backend, (StatusCode, String)> {
        let Some(tab) = self.find(id) else {
            return Err((StatusCode::NOT_FOUND, format!("no tab named {id}")));
        };
        let manifest = tab.load().map_err(|e| {
            tracing::error!("tab {id} is left out: {e}");
            (StatusCode::SERVICE_UNAVAILABLE, format!("tab {id}: {e}"))
        })?;
        manifest
            .backends
            .into_iter()
            .find(|b| b.name == name)
            .ok_or_else(|| (StatusCode::NOT_FOUND, format!("tab {id} has no backend named {name}")))
    }
}

fn check_id(id: &str, taken: &[Tab]) -> Result<(), String> {
    if !valid_name(id) {
        return Err("an id must be lowercase letters, digits and dashes".into());
    }
    if RESERVED_IDS.contains(&id) {
        return Err("that id belongs to a built-in tab".into());
    }
    if taken.iter().any(|t| t.id == id) {
        return Err("that id is named twice".into());
    }
    Ok(())
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(serde_json::json!({ "error": msg }))).into_response()
}

fn not_this_page() -> Response {
    json_error(StatusCode::FORBIDDEN, "tab routes answer only this UI's own page")
}

/// No redirects, so a backend cannot send a call and its token off loopback,
/// and no system proxy, so the call never leaves the machine on its way there.
fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .expect("reqwest client")
    })
}

impl Backend {
    fn token(&self) -> Result<String, String> {
        let path = expand_home(&self.token_file)?;
        let token = std::fs::read_to_string(&path).map_err(|e| format!("backend {}: token file {}: {e}", self.name, path.display()))?;
        let token = token.trim();
        if token.is_empty() {
            return Err(format!("backend {}: token file {} is empty", self.name, path.display()));
        }
        Ok(token.to_string())
    }

    /// The rest of the request's path as the browser sent it, not decoded, so a
    /// `%2F` or `%3F` inside a segment reaches the backend still encoded. Both
    /// routes have the same shape: `/<api>/<route>/<id>/<backend>/<rest>`.
    fn target(&self, uri: &Uri) -> String {
        let rest = uri.path().splitn(6, '/').nth(5).unwrap_or("");
        let base = self.url.trim_end_matches('/');
        match uri.query() {
            Some(query) => format!("{base}/{rest}?{query}"),
            None => format!("{base}/{rest}"),
        }
    }

    fn describe_failure(&self, e: &reqwest::Error) -> String {
        if e.is_connect() {
            format!("backend {} is not running at {}", self.name, self.url)
        } else {
            format!("backend {}: {e}", self.name)
        }
    }
}

/// `GET /tab/<id>/<file>`. The same check as a forwarded call: without it,
/// another site could load a private tab's script with a `<script>` tag and
/// read what it defines.
async fn file(State(tabs): State<Arc<Tabs>>, method: Method, headers: HeaderMap, Path((id, rel)): Path<(String, String)>) -> Response {
    if !same_origin(&method, &headers) {
        return not_this_page();
    }
    let Some(tab) = tabs.find(&id) else {
        return json_error(StatusCode::NOT_FOUND, &format!("no tab named {id}"));
    };
    let path = match resolve_inside(&tab.folder, &rel) {
        Ok(path) => path,
        Err(FileRefused::Outside) => return json_error(StatusCode::FORBIDDEN, "that path is outside the tab's folder"),
        Err(FileRefused::Missing) => return json_error(StatusCode::NOT_FOUND, &format!("tab {id} has no {rel}")),
    };
    match tokio::fs::read(&path).await {
        Ok(bytes) => crate::revalidating_bytes(&headers, content_type(&path), bytes),
        Err(_) => json_error(StatusCode::NOT_FOUND, &format!("tab {id} has no {rel}")),
    }
}

/// `/api/tab/<id>/<backend>/<rest>`: one call, with the backend's token. The
/// token stays out of the page. The answer's `Content-Type` comes back with it,
/// and no other header either way crosses, so no cookie reaches a backend.
async fn proxy(
    State(tabs): State<Arc<Tabs>>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    Path((id, name, _rest)): Path<(String, String, String)>,
    body: Bytes,
) -> Response {
    if !same_origin(&method, &headers) {
        return not_this_page();
    }
    let backend = match tabs.backend(&id, &name) {
        Ok(backend) => backend,
        Err((status, why)) => return json_error(status, &why),
    };
    let token = match backend.token() {
        Ok(token) => token,
        Err(e) => return json_error(StatusCode::SERVICE_UNAVAILABLE, &e),
    };
    let sent = send(client(), backend.target(&uri), &token, method, headers.get(header::CONTENT_TYPE).cloned(), body).await;
    match sent {
        Ok(resp) => {
            let status = resp.status();
            let kind = resp.headers().get(header::CONTENT_TYPE).cloned();
            match resp.bytes().await {
                Ok(bytes) => match kind {
                    Some(kind) => (status, [(header::CONTENT_TYPE, kind)], bytes).into_response(),
                    None => (status, bytes).into_response(),
                },
                Err(e) => json_error(StatusCode::BAD_GATEWAY, &backend.describe_failure(&e)),
            }
        }
        Err(e) if e.is_connect() => json_error(StatusCode::SERVICE_UNAVAILABLE, &backend.describe_failure(&e)),
        Err(e) => json_error(StatusCode::BAD_GATEWAY, &backend.describe_failure(&e)),
    }
}

async fn fetch_view(backend: Backend, target: String) -> Result<String, String> {
    let token = backend.token()?;
    let resp = client().get(target).bearer_auth(token).send().await.map_err(|e| backend.describe_failure(&e))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| backend.describe_failure(&e))?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!("backend {}: HTTP {status}: {body}", backend.name))
    }
}

/// `GET /api/tab-events/<id>/<backend>/<path>`: that path of the backend,
/// polled here and pushed, so the page holds one `EventSource` and no interval,
/// exactly as the Desk tab does through `desk::events`.
async fn events(
    State(tabs): State<Arc<Tabs>>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    Path((id, name, _path)): Path<(String, String, String)>,
) -> Response {
    if !same_origin(&method, &headers) {
        return not_this_page();
    }
    let backend = match tabs.backend(&id, &name) {
        Ok(backend) => backend,
        Err((status, why)) => return json_error(status, &why),
    };
    let target = backend.target(&uri);
    pushed_view(move || fetch_view(backend.clone(), target.clone())).into_response()
}

/// The routes for every tab folder. They are mounted whether or not a tab is
/// configured, and with none each answers `404`.
pub fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    Arc<Tabs>: FromRef<S>,
{
    Router::new()
        .route("/tab/{id}/{*file}", get(file))
        .route("/api/tab/{id}/{backend}/{*rest}", any(proxy))
        .route("/api/tab-events/{id}/{backend}/{*path}", get(events))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A folder under the system's temp folder, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!("ebm-tabs-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn write(&self, rel: &str, text: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The sample tab from the test fixture, with its one backend at `url`.
    fn sample_tab(url: &str) -> Scratch {
        let dir = Scratch::new();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-tab");
        for entry in std::fs::read_dir(&fixture).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.0.join(entry.file_name())).unwrap();
        }
        let token = dir.write("token-file", "s3cret\n");
        let manifest = std::fs::read_to_string(dir.0.join(MANIFEST)).unwrap();
        let manifest = manifest
            .replace("http://127.0.0.1:4999", url)
            .replace("/path/to/token", token.to_str().unwrap());
        std::fs::write(dir.0.join(MANIFEST), manifest).unwrap();
        dir
    }

    fn tabs_of(id: &str, folder: &FsPath) -> Tabs {
        Tabs::new(vec![TabConfig { id: id.to_string(), folder: folder.to_path_buf() }])
    }

    async fn serve(tabs: Tabs) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = routes::<Arc<Tabs>>().with_state(Arc::new(tabs));
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }

    /// A stand-in backend that answers with what it got and counts its calls.
    async fn echo_backend() -> (String, Arc<AtomicUsize>) {
        async fn echo(State(hits): State<Arc<AtomicUsize>>, method: Method, headers: HeaderMap, uri: Uri, body: Bytes) -> Response {
            hits.fetch_add(1, Ordering::SeqCst);
            let get = |k: header::HeaderName| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
            let seen = serde_json::json!({
                "method": method.as_str(),
                "uri": uri.to_string(),
                "auth": get(header::AUTHORIZATION),
                "cookie": get(header::COOKIE),
                "content_type": get(header::CONTENT_TYPE),
                "body": String::from_utf8_lossy(&body),
            });
            ([(header::CONTENT_TYPE, HeaderValue::from_static("application/x-echo"))], seen.to_string()).into_response()
        }
        let hits = Arc::new(AtomicUsize::new(0));
        let app = Router::new().fallback(echo).with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), hits)
    }

    /// Reqwest, without a system proxy, and without a browser's headers, so
    /// a request counts as a local read unless a test adds them.
    fn http() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none()).build().unwrap()
    }

    #[test]
    fn a_backend_must_be_a_literal_loopback_address() {
        for good in ["http://127.0.0.1:4892", "http://127.0.0.1:4892/", "http://127.9.9.9:1", "http://[::1]:4892"] {
            assert_eq!(check_backend_url(good), Ok(()), "{good}");
        }
        for bad in [
            "http://localhost:4892",
            "http://10.0.0.1:4892",
            "http://0.0.0.0:4892",
            "http://example.com",
            "https://127.0.0.1:4892",
            "http://127.0.0.1@example.com/",
            "http://user:pw@127.0.0.1:4892",
            "http://127.0.0.1:4892/sub",
            "http://127.0.0.1:4892/?x=1",
            "http://[::ffff:10.0.0.1]:4892",
            "not a url",
        ] {
            assert!(check_backend_url(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn a_manifest_file_name_cannot_leave_the_folder_or_the_attribute() {
        for good in ["tab.js", "sub/tab.js", "a-b_c.min.js"] {
            assert!(valid_file_name(good), "{good}");
        }
        for bad in ["", "../x", "a/../x", "/etc/passwd", "a//b", "./x", "x\"><script>", "a b"] {
            assert!(!valid_file_name(bad), "{bad}");
        }
    }

    #[test]
    fn an_id_cannot_take_a_built_in_tab_or_be_named_twice() {
        let dir = sample_tab("http://127.0.0.1:1");
        let config = |id: &str| TabConfig { id: id.to_string(), folder: dir.0.clone() };
        let tabs = Tabs::new(vec![config("desk"), config("enroll"), config("Bad Id"), config("sample"), config("sample")]);
        let ids: Vec<&str> = tabs.tabs.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["sample"]);
    }

    #[test]
    fn a_path_is_resolved_inside_the_folder_or_refused() {
        let outside = Scratch::new();
        outside.write("secret.txt", "secret");
        let dir = Scratch::new();
        dir.write("sub/ok.txt", "ok");
        std::os::unix::fs::symlink(outside.0.join("secret.txt"), dir.0.join("link.txt")).unwrap();
        std::os::unix::fs::symlink(&outside.0, dir.0.join("linkdir")).unwrap();

        assert!(resolve_inside(&dir.0, "sub/ok.txt").is_ok());
        assert_eq!(resolve_inside(&dir.0, "nope.txt"), Err(FileRefused::Missing));
        for outward in ["../x", "sub/../../x", "/etc/passwd", "link.txt", "linkdir/secret.txt"] {
            assert_eq!(resolve_inside(&dir.0, outward), Err(FileRefused::Outside), "{outward}");
        }
    }

    const PAGE: &str = "<head><!-- tabs:styles --></head><nav><b>Desk</b><!-- tabs:nav --><b>Debug</b></nav>\
                        <main><!-- tabs:panels --></main><!-- tabs:scripts -->";

    #[test]
    fn with_no_readable_tab_there_is_nothing_to_draw() {
        assert_eq!(Tabs::default().render_index(PAGE), None);
        let missing = Scratch::new();
        assert_eq!(tabs_of("gone", &missing.0.join("nowhere")).render_index(PAGE), None);
    }

    #[test]
    fn a_tab_is_drawn_into_the_page() {
        let dir = sample_tab("http://127.0.0.1:1");
        let page = tabs_of("sample", &dir.0).render_index(PAGE).unwrap();
        assert!(page.contains("<link rel=\"stylesheet\" href=\"/tab/sample/tab.css\">"), "{page}");
        assert!(page.contains("<script src=\"/tab/sample/tab.js\"></script>"), "{page}");
        assert!(page.contains("<section class=\"tab-panel\" data-tab=\"sample\">"), "{page}");
        assert!(page.contains("id=\"sample-result\""), "{page}");
        // Beside Desk, not at the foot of the list.
        let (desk, nav, debug) = (page.find(">Desk<").unwrap(), page.find("data-tab=\"sample\"").unwrap(), page.find(">Debug<").unwrap());
        assert!(desk < nav && nav < debug, "{page}");
        assert!(page.contains("<svg"), "the icon goes in inline: {page}");
        assert!(!page.contains("s3cret"), "the token stays out of the page");
        assert!(!page.contains("tabs:"), "no mark is left: {page}");
    }

    /// The marks are comments in the real page. One lost in an edit to
    /// `index.html` would drop that part of every tab without a word.
    #[test]
    fn the_real_page_has_each_mark_once_and_a_drawn_tab_leaves_none() {
        let real = include_str!("../assets/index.html");
        for mark in [STYLES_MARK, NAV_MARK, PANELS_MARK, SCRIPTS_MARK] {
            assert_eq!(real.matches(mark).count(), 1, "{mark}");
        }
        let dir = sample_tab("http://127.0.0.1:1");
        let page = tabs_of("sample", &dir.0).render_index(real).unwrap();
        assert!(!page.contains("<!-- tabs:"));
        // Straight after Desk, and its script after the built-in ones.
        let desk = page.find("<span class=\"nav-label\">Desk</span>").unwrap();
        let sample = page.find("<span class=\"nav-label\">Sample</span>").unwrap();
        let topology = page.find("<span class=\"nav-label\">Topology</span>").unwrap();
        assert!(desk < sample && sample < topology);
        assert!(page.find("/bench-parts.js").unwrap() < page.find("/tab/sample/tab.js").unwrap());
    }

    #[test]
    fn a_label_is_escaped() {
        let dir = sample_tab("http://127.0.0.1:1");
        let manifest = std::fs::read_to_string(dir.0.join(MANIFEST)).unwrap();
        std::fs::write(dir.0.join(MANIFEST), manifest.replace("label = \"Sample\"", "label = \"<b>A&B</b>\"")).unwrap();
        let page = tabs_of("sample", &dir.0).render_index(PAGE).unwrap();
        assert!(page.contains("&lt;b&gt;A&amp;B&lt;/b&gt;"), "{page}");
    }

    #[test]
    fn a_broken_tab_is_left_out_and_the_others_stay() {
        let good = sample_tab("http://127.0.0.1:1");
        let bad_backend = sample_tab("http://10.0.0.1:1");
        let typo = Scratch::new();
        typo.write(MANIFEST, "label = \"x\"\nicon = \"i.svg\"\npanel = \"p.html\"\nscript = [\"t.js\"]\n");
        let no_panel = sample_tab("http://127.0.0.1:1");
        std::fs::remove_file(no_panel.0.join("panel.html")).unwrap();
        let tabs = Tabs::new(vec![
            TabConfig { id: "bad-backend".into(), folder: bad_backend.0.clone() },
            TabConfig { id: "typo".into(), folder: typo.0.clone() },
            TabConfig { id: "no-panel".into(), folder: no_panel.0.clone() },
            TabConfig { id: "relative".into(), folder: PathBuf::from("some/where") },
            TabConfig { id: "good".into(), folder: good.0.clone() },
        ]);
        let page = tabs.render_index(PAGE).unwrap();
        assert!(page.contains("data-tab=\"good\""), "{page}");
        for gone in ["bad-backend", "typo", "no-panel", "relative"] {
            assert!(!page.contains(&format!("data-tab=\"{gone}\"")), "{gone}: {page}");
        }
    }

    #[test]
    fn an_edit_to_a_tab_shows_without_a_restart() {
        let dir = sample_tab("http://127.0.0.1:1");
        let tabs = tabs_of("sample", &dir.0);
        assert!(!tabs.render_index(PAGE).unwrap().contains("edited-panel"));
        dir.write("panel.html", "<p>edited-panel</p>");
        assert!(tabs.render_index(PAGE).unwrap().contains("edited-panel"));
    }

    #[tokio::test]
    async fn a_tabs_files_are_served_and_revalidate() {
        let dir = sample_tab("http://127.0.0.1:1");
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let http = http();
        let resp = http.get(format!("{ui}/tab/sample/tab.js")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/javascript; charset=utf-8");
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-cache");
        let tag = resp.headers()[header::ETAG].to_str().unwrap().to_string();
        let same = http.get(format!("{ui}/tab/sample/tab.js")).header(header::IF_NONE_MATCH, &tag).send().await.unwrap();
        assert_eq!(same.status(), 304);
        let svg = http.get(format!("{ui}/tab/sample/icon.svg")).send().await.unwrap();
        assert_eq!(svg.headers()[header::CONTENT_TYPE], "image/svg+xml");
        assert_eq!(http.get(format!("{ui}/tab/sample/nope.js")).send().await.unwrap().status(), 404);
        assert_eq!(http.get(format!("{ui}/tab/other/tab.js")).send().await.unwrap().status(), 404);
    }

    #[tokio::test]
    async fn a_path_outside_the_folder_is_refused_over_http() {
        let outer = Scratch::new();
        outer.write("secret.txt", "secret");
        let dir = outer.write("tab/x", "").parent().unwrap().to_path_buf();
        let sample = sample_tab("http://127.0.0.1:1");
        for entry in std::fs::read_dir(&sample.0).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), dir.join(entry.file_name())).unwrap();
        }
        std::os::unix::fs::symlink(outer.0.join("secret.txt"), dir.join("link.txt")).unwrap();
        let ui = serve(tabs_of("sample", &dir)).await;
        let http = http();
        // `..%2f` survives the client's own path clean-up, and the router
        // decodes it into a real `../`.
        for path in ["..%2fsecret.txt", "sub%2f..%2f..%2fsecret.txt", "%2fetc%2fpasswd", "link.txt"] {
            let resp = http.get(format!("{ui}/tab/sample/{path}")).send().await.unwrap();
            assert_eq!(resp.status(), 403, "{path}");
            assert!(!resp.text().await.unwrap().contains("secret\n"), "{path}");
        }
    }

    #[tokio::test]
    async fn a_call_reaches_the_backend_with_the_token_added() {
        let (backend, hits) = echo_backend().await;
        let dir = sample_tab(&backend);
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let resp = http()
            .post(format!("{ui}/api/tab/sample/echo/items/a%2Fb/go?x=1&y=two"))
            .header("sec-fetch-site", "same-origin")
            .header(header::CONTENT_TYPE, "text/plain")
            .header(header::AUTHORIZATION, "Bearer from-the-page")
            .header(header::COOKIE, "c=1")
            .body("hello")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        // The backend's own type comes back: this is not only JSON.
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/x-echo");
        let seen: serde_json::Value = serde_json::from_str(&resp.text().await.unwrap()).unwrap();
        assert_eq!(seen["method"], "POST");
        assert_eq!(seen["uri"], "/items/a%2Fb/go?x=1&y=two", "the path arrives as it was sent");
        assert_eq!(seen["auth"], "Bearer s3cret", "the backend's token replaces anything the page sent");
        assert_eq!(seen["cookie"], serde_json::Value::Null);
        assert_eq!(seen["content_type"], "text/plain");
        assert_eq!(seen["body"], "hello");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_cross_origin_request_is_refused_on_every_route() {
        let (backend, hits) = echo_backend().await;
        let dir = sample_tab(&backend);
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let http = http();
        let urls = ["tab/sample/tab.js", "api/tab/sample/echo/x", "api/tab-events/sample/echo/x"];
        for site in ["cross-site", "same-site", "none"] {
            for url in urls {
                let resp = http.get(format!("{ui}/{url}")).header("sec-fetch-site", site).send().await.unwrap();
                assert_eq!(resp.status(), 403, "{url} from {site}");
            }
        }
        // A browser that does not send Sec-Fetch-Site, writing from elsewhere.
        let resp = http.post(format!("{ui}/api/tab/sample/echo/x")).header("origin", "http://evil.example").body("x").send().await.unwrap();
        assert_eq!(resp.status(), 403);
        // A write naming no origin at all: no browser sends one, so it stops.
        assert_eq!(http.post(format!("{ui}/api/tab/sample/echo/x")).body("x").send().await.unwrap().status(), 403);
        assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the backend");
        // The page itself passes.
        let resp = http.get(format!("{ui}/api/tab/sample/echo/x")).header("sec-fetch-site", "same-origin").send().await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn a_non_loopback_backend_is_refused_and_says_why() {
        let dir = sample_tab("http://10.0.0.1:4892");
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let resp = http().get(format!("{ui}/api/tab/sample/echo/x")).send().await.unwrap();
        assert_eq!(resp.status(), 503);
        assert!(resp.text().await.unwrap().contains("loopback"));
    }

    #[tokio::test]
    async fn a_backend_cannot_send_a_call_off_with_a_redirect() {
        let (second, second_hits) = echo_backend().await;
        let target = format!("{second}/stolen");
        let first = Router::new().fallback(move || {
            let target = target.clone();
            async move { (StatusCode::FOUND, [(header::LOCATION, target)]).into_response() }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, first).await.unwrap() });
        let dir = sample_tab(&first_url);
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let resp = http().get(format!("{ui}/api/tab/sample/echo/x")).send().await.unwrap();
        assert_eq!(resp.status(), 302);
        assert!(resp.headers().get(header::LOCATION).is_none(), "the address does not reach the page");
        assert_eq!(second_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_missing_token_or_backend_stops_the_call_and_says_why() {
        let (backend, hits) = echo_backend().await;
        let dir = sample_tab(&backend);
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let http = http();
        let resp = http.get(format!("{ui}/api/tab/sample/nope/x")).send().await.unwrap();
        assert_eq!(resp.status(), 404);
        std::fs::remove_file(dir.0.join("token-file")).unwrap();
        let resp = http.get(format!("{ui}/api/tab/sample/echo/x")).send().await.unwrap();
        assert_eq!(resp.status(), 503);
        assert!(resp.text().await.unwrap().contains("token file"));
        assert_eq!(hits.load(Ordering::SeqCst), 0);
        // Nothing listening: the page is told so.
        let dead = sample_tab("http://127.0.0.1:1");
        let ui = serve(tabs_of("sample", &dead.0)).await;
        let resp = http.get(format!("{ui}/api/tab/sample/echo/x")).send().await.unwrap();
        assert_eq!(resp.status(), 503);
        assert!(resp.text().await.unwrap().contains("not running"));
    }

    /// The first event of a stream, as the browser would read it off the wire.
    async fn first_event(url: String) -> String {
        let mut resp = http().get(url).send().await.unwrap();
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
        let mut text = String::new();
        while !text.contains("\n\n") {
            let chunk = resp.chunk().await.unwrap().expect("the stream ended");
            text.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        text
    }

    #[tokio::test]
    async fn a_backends_path_is_polled_and_pushed() {
        let (backend, hits) = echo_backend().await;
        let dir = sample_tab(&backend);
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let got = first_event(format!("{ui}/api/tab-events/sample/echo/view?scope=all")).await;
        assert!(got.contains("event: view"), "{got}");
        assert!(got.contains("/view?scope=all"), "{got}");
        assert!(got.contains("Bearer s3cret"), "the poll carries the token: {got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_dead_backend_is_pushed_as_an_error() {
        let dir = sample_tab("http://127.0.0.1:1");
        let ui = serve(tabs_of("sample", &dir.0)).await;
        let got = first_event(format!("{ui}/api/tab-events/sample/echo/view")).await;
        assert!(got.contains("event: view-error"), "{got}");
        assert!(got.contains("not running"), "{got}");
    }
}
