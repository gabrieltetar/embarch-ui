//! Guard (ui/094): every browser drive script learns where geckodriver and the
//! UI are through `tests/browser/addr.py`, never from a hardcoded address.
//!
//! A script that ignored its port variables once opened a session on the
//! owner's own geckodriver. This is a text guard plus a no-browser run of the
//! helper itself; it starts nothing and touches no port.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn browser_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/browser")
}

fn drive_scripts() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(browser_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.starts_with("drive") && n.ends_with(".py")
        })
        .collect();
    v.sort();
    v
}

#[test]
fn every_drive_script_goes_through_addr() {
    let scripts = drive_scripts();
    assert!(scripts.len() >= 15, "found only {} drive scripts", scripts.len());
    for p in scripts {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let src = fs::read_to_string(&p).unwrap();
        assert!(src.contains("import addr"), "{name}: does not import addr");
        assert!(
            src.contains("addr.gecko_url()"),
            "{name}: geckodriver address does not come from addr.gecko_url()"
        );
        assert!(
            src.contains("addr.ui_url(") || src.contains("addr.ui_port("),
            "{name}: UI address does not come from addr.ui_url()/ui_port()"
        );
        for (i, line) in src.lines().enumerate() {
            let l = line.trim_start();
            if l.starts_with('#') {
                continue;
            }
            for bad in ["127.0.0.1:4444", "127.0.0.1:4899", "localhost:4444", "localhost:4899"] {
                assert!(!line.contains(bad), "{name}:{}: hardcoded {bad}", i + 1);
            }
            for legacy in ["\"WEBDRIVER\"", "\"GECKO\"", "\"UI\"", "\"EMBARCH_GECKO_PORT\"", "\"EMBARCH_UI_PORT\""] {
                // Setting EMBARCH_UI_PORT for a child process is fine; reading it is not.
                assert!(
                    !line.contains(&format!("environ.get({legacy}")),
                    "{name}:{}: reads {legacy} itself instead of through addr.py",
                    i + 1
                );
            }
        }
    }
}

fn run_addr(env: &[(&str, &str)]) -> (bool, String) {
    let mut c = Command::new("python3");
    c.current_dir(browser_dir())
        .env_remove("GECKO")
        .env_remove("UI")
        .env_remove("WEBDRIVER")
        .env_remove("EMBARCH_GECKO_PORT")
        .env_remove("EMBARCH_UI_PORT")
        .args([
            "-B",
            "-c",
            "import addr; print(addr.gecko_url(), addr.ui_url(), addr.ui_url(4897))",
        ]);
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().expect("python3 is needed for this test");
    let text = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    (o.status.success(), text.trim().to_string())
}

#[test]
fn addr_reads_the_two_variables() {
    let (ok, out) = run_addr(&[("EMBARCH_GECKO_PORT", "4601"), ("EMBARCH_UI_PORT", "4602")]);
    assert!(ok, "{out}");
    assert_eq!(out, "http://127.0.0.1:4601 http://127.0.0.1:4602 http://127.0.0.1:4602");
    let (ok, out) = run_addr(&[]);
    assert!(ok, "{out}");
    assert_eq!(out, "http://127.0.0.1:4444 http://127.0.0.1:4899 http://127.0.0.1:4897");
}

#[test]
fn addr_refuses_legacy_names_and_junk() {
    for k in ["GECKO", "UI", "WEBDRIVER"] {
        let (ok, out) = run_addr(&[(k, "http://127.0.0.1:1")]);
        assert!(!ok && out.contains("no longer read"), "{k}: {out}");
    }
    let (ok, out) = run_addr(&[("EMBARCH_UI_PORT", "http://x")]);
    assert!(!ok && out.contains("must be a port number"), "{out}");
}
