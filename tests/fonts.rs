//! Regression guard: the UI's typefaces come from this binary, and every
//! name involved matches on all four sides that have to agree.
//!
//! IBM Plex used to arrive over a `<link>` to fonts.googleapis.com. That
//! link is the one asset in the app that could fail without saying so: on a
//! bench with no route to Google — offline, or a corporate net that blocks
//! it — the page still rendered, just in Segoe UI and whatever generic
//! monospace the machine had. That is not only cosmetic. `app.js` sizes the
//! trace view's lane gutter at "~6.6 px per character at 11.5 px IBM Plex
//! Mono", a constant measured against a font that was not guaranteed to be
//! there, so the silent fallback clipped or over-padded lane labels on
//! exactly the machines least able to investigate it.
//!
//! The fonts are now `include_bytes!`d like every other asset and served
//! from `/fonts/`. That trades one silent failure for four brittle name
//! agreements: the `url()` in `style.css`, the file in `assets/fonts/`, the
//! match arm in `main.rs`, and the `<link rel="preload">` in `index.html`.
//! A typo in any one of them reproduces the original bug in a new way —
//! the page renders, in the fallback font, with no error anywhere. Hence
//! this file: it is a text guard over the assets and over `main.rs`'s
//! source, not a rendered check, in the same shape as `element_ids.rs`.

use std::collections::HashSet;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const STYLE_CSS: &str = include_str!("../assets/style.css");
const APP_JS: &str = include_str!("../assets/app.js");
const MAIN_RS: &str = include_str!("../src/main.rs");

/// Every `"/fonts/<name>"` mentioned in `src`, however it is quoted — the
/// stylesheet writes `url('/fonts/x.woff2')` and the page writes
/// `href="/fonts/x.woff2"`, and both should be held to the same list.
fn font_refs(src: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut rest = src;
    while let Some(at) = rest.find("/fonts/") {
        rest = &rest[at + "/fonts/".len()..];
        let end = rest
            .find(|c: char| c == '\'' || c == '"' || c == ')' || c.is_whitespace())
            .unwrap_or(rest.len());
        // `.woff2` and nothing else: prose in these files mentions the
        // `assets/fonts/` directory too, and a comment is not a reference.
        if end > 0 && rest[..end].ends_with(".woff2") {
            out.insert(rest[..end].to_string());
        }
        rest = &rest[end..];
    }
    out
}

#[test]
fn every_font_url_resolves_to_a_file_and_a_route() {
    let mut referenced = font_refs(STYLE_CSS);
    referenced.extend(font_refs(INDEX_HTML));
    assert!(
        !referenced.is_empty(),
        "no /fonts/ reference found at all — if the fonts moved back out to a \
         CDN, this whole guard is stale and should be deleted deliberately \
         rather than left passing vacuously"
    );

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    for name in &referenced {
        let path = dir.join(name);
        assert!(
            path.is_file(),
            "{name} is referenced but assets/fonts/{name} does not exist"
        );
        // A woff2 file and not, say, an HTML error page a proxy handed the
        // download: the four bytes every woff2 opens with.
        let head = std::fs::read(&path).expect("font file readable");
        assert_eq!(
            &head[..4],
            b"wOF2",
            "assets/fonts/{name} is not a woff2 file"
        );
        assert!(
            MAIN_RS.contains(&format!("\"{name}\"")),
            "{name} is referenced by the assets but main.rs's `font` handler \
             has no arm for it, so it would 404 and the page would silently \
             fall back"
        );
    }
}

#[test]
fn nothing_fetches_a_font_from_the_network() {
    for (what, src) in [
        ("index.html", INDEX_HTML),
        ("style.css", STYLE_CSS),
        ("app.js", APP_JS),
    ] {
        // Matched with the scheme separator, so the comments in these files
        // can still name the CDN they replaced and say why.
        for host in ["://fonts.googleapis.com", "://fonts.gstatic.com"] {
            assert!(
                !src.contains(host),
                "{what} reaches out to {host}; the fonts are served from \
                 /fonts/ precisely so an offline bench renders the same as a \
                 connected one"
            );
        }
    }
}

#[test]
fn every_embedded_font_is_actually_referenced() {
    // The other direction: a font that no longer has a `url()` pointing at
    // it is dead weight in the binary, and the two mono weights that are
    // easy to orphan (500, 600) are used by only a handful of rules.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts");
    let mut referenced = font_refs(STYLE_CSS);
    referenced.extend(font_refs(INDEX_HTML));
    for entry in std::fs::read_dir(&dir).expect("assets/fonts exists") {
        let name = entry.expect("readable dir entry").file_name();
        let name = name.to_string_lossy().to_string();
        if !name.ends_with(".woff2") {
            continue; // the OFL licence text lives here too
        }
        assert!(
            referenced.contains(&name),
            "assets/fonts/{name} is embedded but nothing references it"
        );
    }
}
