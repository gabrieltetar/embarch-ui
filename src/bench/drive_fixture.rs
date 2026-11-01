//! The browser drive's bench fixtures, held to the real types.
//!
//! `tests/browser/drive_bench.py` serves the bench routes from two files:
//! `tests/fixtures/bench/catalog.json` — the vocabulary and the built-in
//! catalog exactly as this build serves them — and `fixture.json`, the repo
//! templates and benches a repo could hold. A fixture that drifted from the
//! contract would test a page against JSON no server sends, so both are
//! checked here.

use super::files::{self, Bench};
use super::{catalog, parse_template, parts_dir, vocabulary_json, PartTemplate};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bench").join(name)
}

fn read(name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap_or_default()).unwrap_or(Value::Null)
}

/// `catalog.json` is the vocabulary and `catalog(None)` as served, so the
/// drive draws the real built-ins; a template change fails here until the
/// file is rewritten with `EMBARCH_REGEN_FIXTURES=1 cargo test`.
#[test]
fn the_drive_serves_the_real_vocabulary_and_builtin_catalog() {
    let now = serde_json::json!({ "vocabulary": vocabulary_json(), "catalog": catalog(None) });
    if std::env::var_os("EMBARCH_REGEN_FIXTURES").is_some() {
        std::fs::write(fixture("catalog.json"), serde_json::to_string_pretty(&now).unwrap() + "\n").unwrap();
        return;
    }
    assert!(
        read("catalog.json") == now,
        "tests/fixtures/bench/catalog.json is not what this build serves; rewrite it with EMBARCH_REGEN_FIXTURES=1 cargo test"
    );
}

/// Every repo template in `fixture.json` is one a repo could hold, every
/// bench one the routes could serve — and the one template a newer server
/// might serve is outside this build's vocabulary on purpose.
#[test]
fn the_drive_fixture_is_the_contracts_shape() {
    let fx = read("fixture.json");
    let template = |v: &Value| -> Result<PartTemplate, String> {
        let mut v = v.clone();
        let obj = v.as_object_mut().ok_or("not an object")?;
        assert!(obj.remove("source").is_some_and(|s| s == "repo"), "{obj:?}");
        assert!(obj.remove("version").is_some_and(|s| s.is_string()), "{obj:?}");
        let t: PartTemplate = serde_json::from_value(v).map_err(|e| e.to_string())?;
        t.check()?;
        Ok(t)
    };
    for t in fx["repo_parts"].as_array().unwrap() {
        template(t).unwrap_or_else(|e| panic!("repo part {}: {e}", t["id"]));
    }
    for t in fx["newer_parts"].as_array().unwrap() {
        assert!(template(t).is_err(), "{} is meant to be outside this build's vocabulary", t["id"]);
    }
    let rows = fx["benches"]["benches"].as_array().unwrap();
    assert!(!rows.is_empty());
    for row in rows {
        let slug = row["slug"].as_str().unwrap();
        let bench: Bench = serde_json::from_value(fx[format!("bench/{slug}")].clone())
            .unwrap_or_else(|e| panic!("bench {slug}: {e}"));
        assert_eq!(bench.slug, slug);
        assert_eq!(bench.legacy, row["legacy"].as_bool().unwrap());
        assert!(bench.version.is_some(), "a served bench carries its version");
    }
}

/// The page names the template or bench an unreadable file was: `id` on a
/// template's error, `slug` on a bench's, neither on a directory's.
#[test]
fn an_unreadable_template_names_its_id_and_an_unreadable_bench_its_slug() {
    let dir = std::env::temp_dir().join(format!(
        "ebm-errids-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(parts_dir(&dir)).unwrap();
    std::fs::write(parts_dir(&dir).join("bad.toml"), "id = \"bad\"\nlabel = 3\n").unwrap();
    assert!(parse_template("id = \"bad\"\nlabel = 3\n").is_err());
    let cat = catalog(Some(&dir));
    assert_eq!(cat.errors.len(), 1);
    let served = serde_json::to_value(&cat.errors[0]).unwrap();
    assert_eq!(served["id"], "bad");
    assert!(served.get("slug").is_none(), "{served}");
    let topologies = dir.join("embarch").join("topologies");
    std::fs::create_dir_all(&topologies).unwrap();
    std::fs::write(topologies.join("broken.toml"), "name = [\n").unwrap();
    std::fs::write(topologies.join("Not A Slug.toml"), "name = \"x\"\n").unwrap();
    let (_, errors) = files::list(&dir);
    let by_file: BTreeMap<_, _> = errors.iter().map(|e| (e.file.as_str(), serde_json::to_value(e).unwrap())).collect();
    assert_eq!(by_file["embarch/topologies/broken.toml"]["slug"], "broken");
    assert!(by_file["embarch/topologies/broken.toml"].get("id").is_none());
    assert!(by_file["embarch/topologies/Not A Slug.toml"].get("slug").is_none(), "no route addresses it");
    let _ = std::fs::remove_dir_all(&dir);
}
