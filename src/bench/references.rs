//! Who names a config: the saved host studies and tests of the open repo
//! (decision 120). **Pure file reads**, no Core: `embarch/studies/*.json` and
//! everything under `embarch/tests/suites/<suite>/`.
//!
//! A host study's prompt names the config it wants by exact name
//! (`Prompt.bench_config`, `embarch-study-designer` decision 88); a test names
//! one in `bench.config`, beside the bench it needs. A prompt carries no bench,
//! so a prompt naming `A` holds every bench's `A`; a test holds only the
//! config of the bench it names (by the bench's name or slug).
//!
//! **A file that does not parse is never skipped**: it is listed as
//! unreadable, and an unreadable file refuses the edit exactly as a reference
//! does, because it might be the one naming the config (as the Study
//! Designer's own reference scan treats it).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// One saved study or test that names the config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reference {
    /// `study` or `test`.
    pub kind: &'static str,
    /// The file, relative to the repo.
    pub file: String,
    /// The study's name, or the test's `test_id`.
    pub name: String,
    /// What names it: the prompts (by step name), and `bench.config` for a test.
    pub by: Vec<String>,
}

/// The answer for one config name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Scan {
    pub named_by: Vec<Reference>,
    /// Files that could not be read or parsed, each with why.
    pub unreadable: Vec<String>,
}

impl Scan {
    pub fn blocks(&self) -> bool {
        !self.named_by.is_empty() || !self.unreadable.is_empty()
    }

    /// The refusal's sentence, one line per study, test and unreadable file.
    pub fn sentence(&self, config: &str) -> String {
        let mut out = format!(
            "config {config} is named by saved studies or tests, so it cannot be renamed or deleted until they stop naming it:"
        );
        for r in &self.named_by {
            out.push_str(&format!("\n- {} {} ({}): {}", r.kind, r.name, r.file, r.by.join("; ")));
        }
        for u in &self.unreadable {
            out.push_str(&format!("\n- unreadable, so it might name it: {u}"));
        }
        out
    }
}

fn rel(repo: &Path, p: &Path) -> String {
    p.strip_prefix(repo).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

/// Every `.json` file at or below `dir`, sorted; a missing directory is none.
fn json_files(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>, unreadable: &mut Vec<String>, repo: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            unreadable.push(format!("{}: {e}", rel(repo, dir)));
            return;
        }
    };
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in entries {
        match entry {
            Ok(e) => paths.push(e.path()),
            Err(e) => unreadable.push(format!("{}: an entry could not be read ({e})", rel(repo, dir))),
        }
    }
    paths.sort();
    for p in paths {
        if p.is_dir() {
            if recursive {
                json_files(&p, true, out, unreadable, repo);
            }
        } else if p.extension().and_then(|e| e.to_str()) == Some("json") {
            out.push(p);
        }
    }
}

/// The steps in `value` whose prompt names `config`, by step name (the prompt's
/// text when the step has none). Walks every array and object, so a `Repeat`'s
/// inner steps and the teardown are found.
fn prompts_naming(value: &Value, config: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            let named = map
                .get("action")
                .and_then(|a| a.get("Prompt"))
                .and_then(|p| p.get("bench_config"))
                .and_then(Value::as_str)
                .is_some_and(|c| c == config);
            if named {
                let step = map.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty());
                let text = map.get("action").and_then(|a| a.get("Prompt")).and_then(|p| p.get("text")).and_then(Value::as_str);
                let label = format!("prompt in step {}", step.or(text).map_or_else(|| "(unnamed)".to_string(), |s| format!("`{s}`")));
                if !out.contains(&label) {
                    out.push(label);
                }
            }
            for v in map.values() {
                prompts_naming(v, config, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| prompts_naming(v, config, out)),
        _ => {}
    }
}

/// Whether a test file's `bench` is one of `benches` (a bench's name or slug)
/// and its `config` is `config`.
fn test_names(value: &Value, config: &str, benches: &[&str]) -> bool {
    let Some(b) = value.get("bench") else { return false };
    b.get("config").and_then(Value::as_str) == Some(config)
        && b.get("name").and_then(Value::as_str).is_some_and(|n| benches.contains(&n))
}

/// Who in `repo` names `config` of the bench called one of `benches` (its name
/// and slug).
pub fn scan(repo: &Path, config: &str, benches: &[&str]) -> Scan {
    let mut scan = Scan::default();
    let mut studies = Vec::new();
    json_files(&repo.join("embarch").join("studies"), false, &mut studies, &mut scan.unreadable, repo);
    let mut tests = Vec::new();
    let suites = repo.join("embarch").join("tests").join("suites");
    json_files(&suites, true, &mut tests, &mut scan.unreadable, repo);

    for path in studies.iter().chain(tests.iter()) {
        let file = rel(repo, path);
        let in_suites = path.starts_with(&suites);
        let is_manifest = in_suites && path.file_name().and_then(|n| n.to_str()) == Some("suite.json");
        let value = match std::fs::read_to_string(path) {
            Err(e) => {
                scan.unreadable.push(format!("{file}: {e}"));
                continue;
            }
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Err(e) => {
                    scan.unreadable.push(format!("{file}: not valid JSON ({e})"));
                    continue;
                }
                Ok(v) => v,
            },
        };
        if is_manifest {
            // A manifest names tests by file; one that is not there is
            // unreadable, for it may be the test that names the config.
            let dir = path.parent().unwrap_or(&suites);
            for t in value.get("tests").and_then(Value::as_array).into_iter().flatten() {
                if let Some(f) = t.get("file").and_then(Value::as_str) {
                    if !dir.join(f).is_file() {
                        scan.unreadable.push(format!("{file}: the test file `{f}` it lists is missing"));
                    }
                }
            }
            continue;
        }
        let mut by = Vec::new();
        let is_test = in_suites && value.get("test_id").is_some();
        if is_test && test_names(&value, config, benches) {
            by.push(format!("bench.config is {config}"));
        }
        prompts_naming(&value, config, &mut by);
        if by.is_empty() {
            continue;
        }
        let name = if is_test {
            value.get("test_id").and_then(Value::as_str)
        } else {
            value.get("name").and_then(Value::as_str)
        }
        .map(str::to_string)
        .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string());
        scan.named_by.push(Reference { kind: if is_test { "test" } else { "study" }, file, name, by });
    }
    scan.unreadable.sort();
    scan.unreadable.dedup();
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(tag: &str) -> PathBuf {
        let r = std::env::temp_dir().join(format!("ebm-refs-{tag}-{}-{}", std::process::id(), crate::topology::now_utc_ms()));
        std::fs::create_dir_all(&r).unwrap();
        r
    }

    fn put(repo: &Path, path: &str, text: &str) {
        let p = repo.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    const PROMPT_A: &str =
        r#"{"name":"s","steps":[{"name":"swap","action":{"Prompt":{"text":"t","bench_config":"A"}},"timeout_ms":1}]}"#;

    #[test]
    fn a_test_and_a_study_naming_the_config_are_named_and_others_are_not() {
        let r = repo("named");
        put(&r, "embarch/studies/uses-a.json", PROMPT_A);
        put(&r, "embarch/studies/uses-b.json", &PROMPT_A.replace("\"A\"", "\"B\""));
        put(&r, "embarch/studies/none.json", r#"{"name":"n","steps":[]}"#);
        put(&r, "embarch/tests/suites/s/suite.json", r#"{"tests":[{"test_id":"t1","file":"t1/test.json"}]}"#);
        put(&r, "embarch/tests/suites/s/t1/test.json", r#"{"test_id":"t1","bench":{"name":"bench-one","config":"A"}}"#);
        put(&r, "embarch/tests/suites/s/t1/study.json", &PROMPT_A.replace("\"s\"", "\"inner\""));
        let scan = scan(&r, "A", &["bench-one"]);
        let names: Vec<_> = scan.named_by.iter().map(|x| (x.kind, x.name.as_str())).collect();
        assert_eq!(names, vec![("study", "s"), ("study", "inner"), ("test", "t1")]);
        assert!(scan.unreadable.is_empty());
        assert!(!super::scan(&r, "Z", &["bench-one"]).blocks());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_test_of_another_bench_does_not_name_this_benchs_config() {
        let r = repo("other");
        put(&r, "embarch/tests/suites/s/suite.json", r#"{"tests":[{"test_id":"t1","file":"t1.json"}]}"#);
        put(&r, "embarch/tests/suites/s/t1.json", r#"{"test_id":"t1","bench":{"name":"elsewhere","config":"A"}}"#);
        assert!(!scan(&r, "A", &["bench-one"]).blocks());
        assert!(scan(&r, "A", &["elsewhere"]).blocks());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_file_that_does_not_parse_or_is_missing_is_listed_and_blocks() {
        let r = repo("bad");
        put(&r, "embarch/studies/broken.json", "{ nope");
        put(&r, "embarch/tests/suites/s/suite.json", r#"{"tests":[{"test_id":"t1","file":"gone.json"}]}"#);
        let scan = scan(&r, "A", &["b"]);
        assert!(scan.named_by.is_empty());
        assert_eq!(scan.unreadable.len(), 2, "{:?}", scan.unreadable);
        assert!(scan.blocks());
        assert!(scan.sentence("A").contains("broken.json"));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_repo_with_no_studies_or_tests_names_nothing() {
        let r = repo("empty");
        assert!(!scan(&r, "A", &["b"]).blocks());
        let _ = std::fs::remove_dir_all(&r);
    }
}
