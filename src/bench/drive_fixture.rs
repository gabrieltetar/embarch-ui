//! What the bench routes say about a file they could not read.

use super::files;
use super::{catalog, parse_template, parts_dir};
use std::collections::BTreeMap;

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
