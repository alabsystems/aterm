// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-drive` — the AI-friendly "sugar" CLI over the core `await`/`send`/`key`/
//! `text` primitives. It teaches itself through `--help` and emits actionable
//! errors, so an AI agent builds correct intuition for the kernel without docs.
//! All real work flows through `aterm-ctl` (the std-only core client), so this is
//! a thin, honest wrapper — no protocol re-implementation.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::{CtlClient, DRIVE_HELP, RelayClient, SelfGovernor, Turn};

/// Resolve the `aterm-ctl` binary: `$ATERM_CTL`, then a sibling of this binary
/// (the cargo/install layout), then bare `aterm-ctl` on `PATH`.
fn resolve_ctl() -> PathBuf {
    if let Ok(p) = std::env::var("ATERM_CTL") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sib = dir.join("aterm-ctl");
        if sib.is_file() {
            return sib;
        }
    }
    PathBuf::from("aterm-ctl")
}

struct Opts {
    socket: Option<String>,
    /// A saved REMOTE connection name to `dial` via the local host (`--dial`).
    dial: Option<String>,
    idle_ms: u64,
    timeout_ms: u64,
    cmd: Vec<String>,
}

fn parse(argv: Vec<std::ffi::OsString>) -> Result<Opts, String> {
    let mut socket = None;
    let mut dial = None;
    let mut idle_ms = 600u64;
    let mut timeout_ms = 180_000u64;
    let mut cmd = Vec::new();
    let mut it = argv.into_iter().map(|a| a.to_string_lossy().into_owned());
    while let Some(a) = it.next() {
        match a.as_str() {
            "--socket" | "--sock" => {
                socket = Some(it.next().ok_or("--socket needs a PATH")?);
            }
            "--dial" | "--remote" => {
                dial = Some(it.next().ok_or("--dial needs a saved connection NAME")?);
            }
            "--idle" => {
                idle_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--idle needs a millisecond integer")?;
            }
            "--timeout" => {
                timeout_ms = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--timeout needs a millisecond integer")?;
            }
            "-h" | "--help" | "help" => {
                cmd = vec!["help".to_string()];
                break;
            }
            _ => {
                cmd.push(a);
                cmd.extend(it.by_ref());
                break;
            }
        }
    }
    Ok(Opts {
        socket,
        dial,
        idle_ms,
        timeout_ms,
        cmd,
    })
}

/// Resolve the LOCAL host's control socket + capability token for a `--dial` drive.
/// The socket comes from `--socket`/`$ATERM_CONTROL_SOCK`; the token from
/// `$ATERM_CONTROL_TOKEN`, else the sibling token file (`<socket>.sock` -> `.token`).
fn resolve_local_endpoint(opts: &Opts) -> Result<(String, String), String> {
    let sock = opts
        .socket
        .clone()
        .or_else(|| std::env::var("ATERM_CONTROL_SOCK").ok())
        .filter(|s| !s.is_empty() && s != "0" && s != "off")
        .ok_or(
            "--dial needs the LOCAL host socket: pass --socket <PATH> or set ATERM_CONTROL_SOCK",
        )?;
    let token = std::env::var("ATERM_CONTROL_TOKEN")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Convention: the token file sits beside the socket (`.sock` -> `.token`).
            let tok_path = sock.strip_suffix(".sock").map(|p| format!("{p}.token"))?;
            std::fs::read_to_string(&tok_path)
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .ok_or(
            "could not resolve the LOCAL control token: set ATERM_CONTROL_TOKEN, or ensure the \
             sibling <socket-with-.token> file is readable",
        )?;
    Ok((sock, token))
}

/// The whole drive CLI as a callable: `argv[1..]` in, exit code out. Served
/// in-process by the ONE `aterm` binary (`aterm drive …` / argv0 alias) and
/// by the thin standalone bin.
pub fn main_entry(argv: Vec<std::ffi::OsString>) -> ExitCode {
    let opts = match parse(argv) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("aterm-drive: {e}\n\nRun `aterm-drive --help` for usage.");
            return ExitCode::FAILURE;
        }
    };
    match run(&opts) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("aterm-drive: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(opts: &Opts) -> Result<String, String> {
    let verb = opts.cmd.first().map(String::as_str).unwrap_or("help");
    if verb == "help" {
        return Ok(format!("{DRIVE_HELP}\n"));
    }

    // `--dial <name>`: drive a REMOTE aterm over the local host's `dial` relay. A
    // persistent `RelayClient` speaks the SAME verbs as the local path, so the Turn
    // is byte-identical — predicates run on the authoritative remote host. Supports
    // the `prompt` drive loop (the remote use case); other verbs stay local.
    if let Some(name) = &opts.dial {
        if verb != "prompt" {
            return Err(format!(
                "--dial supports the `prompt` command (the drive loop); got `{verb}`. \
                 Run local read/await/shot without --dial."
            ));
        }
        let text = opts.cmd[1..].join(" ");
        if text.is_empty() {
            return Err(
                "prompt needs text, e.g. `aterm-drive --dial work prompt 'say hi'`".to_string(),
            );
        }
        let (sock, token) = resolve_local_endpoint(opts)?;
        let mut client = RelayClient::dial_via_local(&sock, &token, name).map_err(|e| {
            format!(
                "could not dial remote connection '{name}' via the local host ({e}).\n  \
                 • Is '{name}' a connection saved on THIS host? (the local aterm resolves it)\n  \
                 • Is the LOCAL socket/token right? (--socket / ATERM_CONTROL_SOCK / ATERM_CONTROL_TOKEN)"
            )
        })?;
        let mut gov = SelfGovernor::disabled(64, 8, 5_000_000);
        gov.enable_self_write();
        let turn = Turn {
            idle: Duration::from_millis(opts.idle_ms),
            timeout: Duration::from_millis(opts.timeout_ms),
            ..Turn::default()
        };
        return turn
            .run(&mut client, &mut gov, text.as_bytes())
            .map_err(|e| e.to_string());
    }

    let ctl = resolve_ctl();
    let mut client = CtlClient::new(ctl.clone(), opts.socket.clone());

    // A friendly preflight: if we cannot even read the screen, explain why before
    // attempting to drive — this is the error an AI hits most and learns from.
    if let Err(e) = client.run(&["cursor"]) {
        return Err(format!(
            "cannot reach a target aterm over the control socket ({e}).\n  \
             • Is a host aterm running? Launch one headless:\n      \
             ATERM_HEADLESS=1 aterm-gui &\n  \
             • Point at its socket (it prints 'control socket listening at <PATH>'):\n      \
             export ATERM_CONTROL_SOCK=<PATH>   (or pass --socket <PATH>)\n  \
             • aterm-ctl resolved to: {}",
            ctl.display()
        ));
    }

    match verb {
        "prompt" => {
            let text = opts.cmd[1..].join(" ");
            if text.is_empty() {
                return Err("prompt needs text, e.g. `aterm-drive prompt 'say hi'`".to_string());
            }
            // A permissive governor: this drives ANOTHER session (cross-session),
            // so self-write is enabled with ample headroom. (Self-driving a single
            // terminal is the case where the floor matters — see the lib docs.)
            let mut gov = SelfGovernor::disabled(64, 8, 5_000_000);
            gov.enable_self_write();
            let turn = Turn {
                idle: Duration::from_millis(opts.idle_ms),
                timeout: Duration::from_millis(opts.timeout_ms),
                ..Turn::default()
            };
            turn.run(&mut client, &mut gov, text.as_bytes())
                .map_err(|e| e.to_string())
        }
        "read" => client.run(&["text"]),
        "shot" => {
            let path = opts.cmd.get(1).cloned();
            let mut args = vec!["image"];
            if let Some(p) = &path {
                args.push(p);
            }
            client.run(&args)
        }
        "await" => {
            if opts.cmd.len() < 2 {
                return Err(
                    "await needs a condition: idle <ms> | match <regex> | seq | block\n  \
                     e.g. `aterm-drive await match 'BUILD SUCCESSFUL'`"
                        .to_string(),
                );
            }
            // Pass the condition straight through to the core verb, but supply the
            // tool's --timeout so a bare `await idle 500` still has a sane bound.
            let mut args: Vec<String> = opts.cmd[1..].to_vec();
            if !args.iter().any(|a| a == "timeout") {
                args.push("timeout".to_string());
                args.push(opts.timeout_ms.to_string());
            }
            let mut a: Vec<&str> = vec!["await"];
            a.extend(args.iter().map(String::as_str));
            client.run(&a)
        }
        other => Err(format!(
            "unknown command '{other}'. Valid: prompt | read | await | shot | help.\n  \
             Run `aterm-drive --help` for the full guide."
        )),
    }
}
