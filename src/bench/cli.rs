//! `embarch-ui bench make-live <slug> [--config <name>] [--bind <role>=<serial>]...`
//! — **Make live from the person's terminal**, so a Test needs no browser.
//!
//! This module parses, prints and nothing else. The apply is
//! [`super::live::make_live`], the very function `POST
//! /api/bench/benches/{slug}/live` calls, and a `--bind` is the very
//! enrolment `POST /api/enroll` makes (`crate::enroll`): the page and the
//! terminal cannot disagree about what a bench declares, what it proposes or
//! what is recorded live. The Core client is the one the server builds, the
//! person's (`config::core_client`), never the agent's.
//!
//! **A proposal is applied only when named.** The saved bench says which
//! probe it expects, which is not evidence about what is plugged in
//! (decision 90), so every proposal is printed and only a `--bind
//! <role>=<serial>` that names the proposal's role (a bench board: its
//! label) *and* the serial the proposal names confirms it. A bind that
//! names another serial, or nothing the bench proposed, applies nothing and
//! fails the command.

use std::fmt::Write as _;

use anyhow::{anyhow, bail, Context, Result};
use axum::http::StatusCode;

use super::files;
use super::live::{make_live, MakeLiveRequest};
use crate::{enroll, AppState, EnrollRequest};

pub const USAGE: &str = "usage: embarch-ui bench make-live <slug> [--config <name>] [--bind <role>=<serial>]...";

/// A parsed command line.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub slug: String,
    pub config: Option<String>,
    /// `(role, serial)`; a bench board's role is its label.
    pub binds: Vec<(String, String)>,
}

/// What the command printed, and whether everything in it went through.
pub struct Outcome {
    pub text: String,
    pub ok: bool,
}

pub fn parse(args: &[String]) -> Result<Args> {
    let mut it = args.iter().map(String::as_str);
    if (it.next(), it.next()) != (Some("bench"), Some("make-live")) {
        bail!("{USAGE}");
    }
    let mut slug = None;
    let mut config = None;
    let mut binds = Vec::new();
    while let Some(a) = it.next() {
        match a {
            "--config" => config = Some(it.next().ok_or_else(|| anyhow!("--config names a config\n{USAGE}"))?.to_string()),
            "--bind" => {
                let b = it.next().ok_or_else(|| anyhow!("--bind names <role>=<serial>\n{USAGE}"))?;
                match b.split_once('=') {
                    Some((role, serial)) if !role.is_empty() && !serial.is_empty() => binds.push((role.to_string(), serial.to_string())),
                    _ => bail!("--bind takes <role>=<serial>, got `{b}`\n{USAGE}"),
                }
            }
            flag if flag.starts_with("--") => bail!("unknown option `{flag}`\n{USAGE}"),
            s if slug.is_none() => slug = Some(s.to_string()),
            extra => bail!("unexpected argument `{extra}`\n{USAGE}"),
        }
    }
    Ok(Args { slug: slug.ok_or_else(|| anyhow!("{USAGE}"))?, config, binds })
}

/// Runs the subcommand in the person's terminal.
pub fn command(args: &[String]) -> Result<()> {
    let args = parse(args)?;
    let config = crate::config::load()?;
    let core = std::sync::Arc::new(crate::config::core_client(&config.core)?);
    let repo = match config.study_designer {
        Some(s) => s.firmware_repo_path,
        None => std::env::current_dir().context("no firmware repo is configured and the working directory is unreadable")?,
    };
    let outcome = tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async {
        let state = AppState::headless(core, repo.clone(), super::live::default_record_path()).await;
        run(&state, &repo, &args).await
    })?;
    print!("{}", outcome.text);
    if !outcome.ok {
        std::process::exit(1);
    }
    Ok(())
}

/// The make-live and its binds over `state`, with the report as text.
pub async fn run(state: &AppState, repo: &std::path::Path, args: &Args) -> Result<Outcome> {
    let version = match files::read_file(repo, &args.slug) {
        Ok((_, version)) => version,
        Err(files::LoadError::Missing) => bail!("no saved bench `{}` in {}", args.slug, repo.display()),
        Err(files::LoadError::Unparseable(e) | files::LoadError::Unreadable(e)) => bail!("{e}"),
    };
    let req = MakeLiveRequest { config: args.config.clone(), version: Some(version), write_through: Some(false) };
    let resp = make_live(state, repo, &args.slug, &req).await;
    let status = resp.status();
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    if status != StatusCode::OK {
        bail!("{} ({status})", String::from_utf8_lossy(&body));
    }
    let report: serde_json::Value = serde_json::from_slice(&body)?;
    let mut ok = true;
    let mut out = String::new();
    let live = &report["live"];
    let _ = writeln!(
        out,
        "{} is live{}",
        live["slug"].as_str().unwrap_or(&args.slug),
        live["config"].as_str().map(|c| format!(" · {c}")).unwrap_or_default()
    );

    let _ = writeln!(out, "\ndeclared:");
    for r in report["applied"].as_array().into_iter().flatten() {
        ok &= r["ok"] == true;
        let _ = writeln!(
            out,
            "  {}  {}  [{}]  {}",
            if r["ok"] == true { "ok  " } else { "FAIL" },
            r["what"].as_str().unwrap_or_default(),
            r["route"].as_str().unwrap_or_default(),
            r["text"].as_str().unwrap_or_default()
        );
    }

    let proposals = report["proposals"].as_array().cloned().unwrap_or_default();
    let _ = writeln!(out, "\nproposed probe bindings (applied only with --bind <role>=<serial>):");
    if proposals.is_empty() {
        let _ = writeln!(out, "  none");
    }
    let mut used = vec![false; args.binds.len()];
    for p in &proposals {
        let key = p["role"].as_str().map(str::to_string).unwrap_or_else(|| p["name"].as_str().unwrap_or_default().to_string());
        let serial = p["probe_serial"].as_str().unwrap_or_default();
        let _ = writeln!(out, "  {key}: {}", p["text"].as_str().unwrap_or_default());
        let Some(i) = args.binds.iter().position(|(role, _)| *role == key) else {
            let _ = writeln!(out, "      not applied — to apply: --bind {key}={serial}");
            continue;
        };
        used[i] = true;
        if args.binds[i].1 != serial {
            ok = false;
            let _ = writeln!(out, "      not applied — the bench names probe {serial}, and --bind names {}", args.binds[i].1);
            continue;
        }
        let enrol = EnrollRequest {
            role: p["role"].as_str().unwrap_or_default().to_string(),
            chip: p["chip"].as_str().unwrap_or_default().to_string(),
            probe_serial: Some(serial.to_string()),
            name: p["name"].as_str().map(str::to_string),
        };
        let resp = enroll(state, &enrol).await;
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
        if status == StatusCode::OK {
            let said: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let note = said["moved_roles_note"].as_str().map(|n| format!(" — {n}")).unwrap_or_default();
            let _ = writeln!(out, "      applied — enrolled on {}{note}", said["chip"].as_str().unwrap_or_default());
        } else {
            ok = false;
            let _ = writeln!(out, "      FAILED — {} ({status})", String::from_utf8_lossy(&body));
        }
    }
    for ((role, serial), used) in args.binds.iter().zip(used) {
        if !used {
            ok = false;
            let _ = writeln!(out, "\n--bind {role}={serial}: the bench proposes no binding for `{role}`; nothing applied for it");
        }
    }

    for n in report["not_proposed"].as_array().into_iter().flatten() {
        let _ = writeln!(out, "  not proposed: {}", n["text"].as_str().unwrap_or_default());
    }
    let leftovers = report["leftovers"].as_array().cloned().unwrap_or_default();
    if !leftovers.is_empty() {
        let _ = writeln!(out, "\nCore holds, and the bench does not name (nothing is retracted here; the Topology tab retracts):");
        for l in &leftovers {
            let _ = writeln!(out, "  {} {} — {}", l["kind"].as_str().unwrap_or_default(), l["id"].as_str().unwrap_or_default(), l["text"].as_str().unwrap_or_default());
        }
    }
    Ok(Outcome { text: out, ok })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_command_line_takes_a_slug_a_config_and_binds() {
        let got = parse(&a("bench make-live pd --config A --bind dut=D1 --bind pd-bench=N1")).unwrap();
        assert_eq!(got, Args { slug: "pd".into(), config: Some("A".into()), binds: vec![("dut".into(), "D1".into()), ("pd-bench".into(), "N1".into())] });
        assert_eq!(parse(&a("bench make-live pd")).unwrap(), Args { slug: "pd".into(), config: None, binds: vec![] });
        for bad in ["bench make-live", "bench make-live pd --bind dut", "bench make-live pd --bind =D1", "bench make-live pd --config", "bench make-live pd --nope", "bench make-live a b", "bench other"] {
            assert!(parse(&a(bad)).unwrap_err().to_string().contains("usage:"), "{bad}");
        }
    }
}
