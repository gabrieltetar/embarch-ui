//! The UI as a systemd user service (decision 101), the way the desk runs
//! (`embarch-desk` decision 27): one instance by construction, and alive with
//! no VS Code window open. The launcher in `embarch-chat` reads and drives this
//! unit through `systemctl --user`; it never spawns the binary.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Command;

const UNIT: &str = "embarch-ui.service";

/// What the unit carries from the installing shell besides `PATH`: the
/// address and config this binary reads, and the distro name the Atlas tab's
/// editor links need, which a user service starts without. Nothing else, so a
/// session's `EMBARCH_DESK_PORT` never points the service at a dev desk.
const CARRIED: [&str; 4] = ["EMBARCH_UI_HOST", "EMBARCH_UI_PORT", "EMBARCH_UI_CONFIG", "WSL_DISTRO_NAME"];

const USAGE: &str = "usage: embarch-ui [service install|uninstall|status]";

/// `embarch-ui service install|uninstall|status`.
pub fn command(args: &[String]) -> Result<()> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["service", "install"] => install(),
        ["service", "uninstall"] => uninstall(),
        ["service", "status"] => status(),
        _ => bail!("{USAGE}"),
    }
}

fn unit_path() -> Result<PathBuf> {
    let config = match std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        Some(x) => PathBuf::from(x),
        None => PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(config.join("systemd").join("user").join(UNIT))
}

/// One word of a unit line, double-quoted so a space stays inside it: `\`
/// and `"` escaped, `%` doubled because systemd expands specifiers.
fn quoted(word: &str) -> String {
    format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\"").replace('%', "%%"))
}

/// The unit text, as the desk writes its own: under WSL, `PATH` holds
/// Windows folders like `/mnt/c/Program Files/…`, and systemd splits an
/// unquoted assignment at the first space, so every assignment and the
/// binary's path are quoted; `$` is doubled in ExecStart alone, the one of
/// the two that expands variables. `PATH` is carried because a build runs
/// `west` and the Atlas tab `git` and `pdftoppm`, and a user service starts
/// with systemd's bare default.
fn unit_text(exe: &str, path_env: &str, vars: &[(&str, String)]) -> String {
    let exe = quoted(&exe.replace('$', "$$"));
    let env: String = std::iter::once(format!("PATH={path_env}"))
        .chain(vars.iter().map(|(k, v)| format!("{k}={v}")))
        .map(|e| format!("Environment={}\n", quoted(&e)))
        .collect();
    format!(
        "[Unit]\n\
         Description=EmbArch UI: the suite's browser UI\n\
         After=network-online.target\n\n\
         [Service]\n\
         ExecStart={exe}\n\
         {env}\
         Restart=on-failure\n\
         RestartSec=5\n\n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

fn systemctl(args: &[&str]) -> Result<String> {
    let out = Command::new("systemctl").arg("--user").args(args).output().context("running systemctl --user")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() && args[0] != "status" {
        bail!("systemctl --user {}: {}", args.join(" "), text.trim());
    }
    Ok(text)
}

/// Which config the service will read, said at install: the variable the unit
/// carries, else the default file the binary finds itself (decision 102), else
/// none, which is the zero-config default and is worth saying aloud.
fn config_note(carried: bool, found: Option<PathBuf>) -> String {
    match (carried, found) {
        (true, Some(p)) => format!("config: {} (EMBARCH_UI_CONFIG, carried into the unit)", p.display()),
        (false, Some(p)) => format!("config: {} (the default path; EMBARCH_UI_CONFIG was not set)", p.display()),
        _ => "config: none; the UI runs on its defaults, with no tabs. Put a file at the default path or set EMBARCH_UI_CONFIG and reinstall".into(),
    }
}

/// Writes the unit for the binary running this, enables it and (re)starts
/// it: installing from a dev build's `target/debug` makes `cargo build`
/// plus `systemctl --user restart embarch-ui` the restart.
fn install() -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let path_env = std::env::var("PATH").unwrap_or_default();
    let vars: Vec<(&str, String)> =
        CARRIED.iter().filter_map(|&k| std::env::var(k).ok().filter(|v| !v.is_empty()).map(|v| (k, v))).collect();
    let p = unit_path()?;
    std::fs::create_dir_all(p.parent().expect("unit dir"))?;
    std::fs::write(&p, unit_text(&exe.display().to_string(), &path_env, &vars))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT])?;
    systemctl(&["restart", UNIT])?;
    println!("installed {} → {}; it starts with your user session", p.display(), exe.display());
    println!("{}", config_note(vars.iter().any(|(k, _)| *k == "EMBARCH_UI_CONFIG"), crate::config::resolve()));
    Ok(())
}

fn uninstall() -> Result<()> {
    let _ = systemctl(&["disable", "--now", UNIT]);
    let p = unit_path()?;
    if p.exists() {
        std::fs::remove_file(&p)?;
    }
    systemctl(&["daemon-reload"])?;
    println!("removed {}", p.display());
    Ok(())
}

fn status() -> Result<()> {
    print!("{}", systemctl(&["status", "--no-pager", UNIT])?);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_unit_runs_the_installing_binary_with_the_users_path() {
        let u = super::unit_text("/h/embarch-ui/target/debug/embarch-ui", "/h/.local/bin:/usr/bin", &[]);
        assert!(u.contains("\nExecStart=\"/h/embarch-ui/target/debug/embarch-ui\"\n"), "{u}");
        assert!(u.contains("\nEnvironment=\"PATH=/h/.local/bin:/usr/bin\"\n"), "{u}");
        assert!(u.contains("\nRestart=on-failure\n"), "{u}");
        assert!(u.contains("WantedBy=default.target"), "{u}");
    }

    #[test]
    fn a_path_with_spaces_stays_one_assignment() {
        let u = super::unit_text("/h/My Apps/$x/embarch-ui", "/h/bin:/mnt/c/Program Files/Git/cmd:/mnt/c/50%\\\"q\"", &[]);
        assert!(u.contains("\nExecStart=\"/h/My Apps/$$x/embarch-ui\"\n"), "{u}");
        assert!(u.contains("\nEnvironment=\"PATH=/h/bin:/mnt/c/Program Files/Git/cmd:/mnt/c/50%%\\\\\\\"q\\\"\"\n"), "{u}");
    }

    #[test]
    fn the_carried_variables_follow_path() {
        let vars = [("EMBARCH_UI_PORT", "4890".to_string()), ("WSL_DISTRO_NAME", "Ubuntu-24.04".to_string())];
        let u = super::unit_text("/h/bin/embarch-ui", "/usr/bin", &vars);
        assert!(u.contains("Environment=\"PATH=/usr/bin\"\nEnvironment=\"EMBARCH_UI_PORT=4890\"\nEnvironment=\"WSL_DISTRO_NAME=Ubuntu-24.04\"\n"), "{u}");
        assert!(!super::CARRIED.iter().any(|k| k.starts_with("EMBARCH_DESK") || k.starts_with("CLAUDE")));
    }

    #[test]
    fn install_says_which_config_the_unit_will_read_including_none() {
        let p = Some(std::path::PathBuf::from("/h/.config/embarch-ui/config.toml"));
        assert!(super::config_note(false, p.clone()).contains("default path"));
        assert!(super::config_note(true, p).contains("carried into the unit"));
        assert!(super::config_note(false, None).contains("config: none"));
    }

    #[test]
    fn only_the_three_service_commands_are_accepted() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for bad in [&["serve"][..], &["service"], &["service", "start"], &["service", "install", "extra"]] {
            let err = super::command(&args(bad)).unwrap_err().to_string();
            assert_eq!(err, super::USAGE, "{bad:?}");
        }
    }
}
