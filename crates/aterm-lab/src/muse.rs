// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The REAL Muse Code (Meta's `muse`: the managed store binary or
//! `$LAB_MUSE`, by path) in the lab's tab, against the stub's Meta model API
//! ([`crate::stub::muse`]). The launch line names every variable it needs:
//!
//! * `HOME` is the scratch's `home/`, and the XDG directories are set under it
//!   (`XDG_CONFIG_HOME=<home>/.config`, …): Muse Code keeps its config, its
//!   workspace trust and its login in `<home>/.config/muse`, its sessions in
//!   `<home>/.local/share/muse`, and the tab's own `XDG_CONFIG_HOME` is the
//!   instance's; `TMPDIR` is the scratch's `tmp/` (its workspace probes and
//!   shell-sandbox directories), which the tab leaves unset;
//! * `META_API_KEY` is the lab's fixed fake key (`lab-not-a-key`, the stub
//!   logs it as `auth=key`): it beats any login, so no account is ever asked
//!   for; the other providers' keys and base URLs (`OPENAI_*`,
//!   `ANTHROPIC_*`, `OPENROUTER_*`) and the other agents' homes it reads
//!   (`CODEX_HOME`, `CLAUDE_CONFIG_DIR`) are unset;
//! * `--base-url http://127.0.0.1:<port>` sends the catalog and every turn to
//!   the stub, and `TBH_AUTH_BASE_URL`/`TBH_MINT_BASE_URL` its auth and mint
//!   calls; `TBH_DISABLE_TELEMETRY=1`, `TBH_DISABLE_FEATURE_CONFIG=1` and
//!   `TBH_DISABLE_DEFAULT_PLUGIN_INSTALL=1` keep the lanes that call home off;
//! * every proxy variable is the lab's black hole (egress is trapped, and the
//!   jail keeps every connect on loopback), `NO_PROXY` loopback.
//!
//! Two places none of the variables tried moves: the account's
//! `~/Library/Application Support/Muse` (its session-name authority, which
//! 1.4.10 opens whatever HOME and the XDG variables say) is on the jail's
//! protected list, and its local Session registry
//! stays in the shared `/private/tmp/tbh-<uid>-rt/muse` (measured: `TMPDIR`,
//! `TMP`, `TEMP`, `XDG_RUNTIME_DIR` and `HOME` pointed into the scratch left
//! it there), whose entries of this run `down` takes back
//! ([`crate::runtime_dirs`]).
//!
//! The harness keys are the lab's (`up`'s `LAB_HARNESS_KEYS`; the captures
//! turn it off). A login, a server outside the lab's process tree and the
//! other ways to reach Meta are refused ([`safe_args`]). Muse Code's retries
//! are its own (`provider_retry.max_attempts`, 10 by default, honouring
//! `retry-after`): `-- -c provider_retry.max_attempts=N` changes them.

use std::path::Path;

use crate::LAB;
use crate::door::Door;
use crate::run::{Run, take_tag};
use crate::stub::LAB_CODEX_KEY;
use crate::util::shell_quote;
use crate::{Fail, Res};

fn lab(args: &[String]) -> Result<(Run, Vec<String>), Fail> {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    Ok((Run::load(&tag)?, a))
}

/// The lab has a Muse Code to run.
///
/// # Errors
/// None is there.
pub fn need_muse(run: &Run) -> Result<(), Fail> {
    if run.muse_bin.is_empty() || !crate::sys::executable(&run.muse_bin) {
        return Err(Fail::die(format!(
            "no Muse Code in lab '{}' (aterm pkg install muse, or LAB_MUSE=/path/to/muse at up)",
            run.tag
        )));
    }
    Ok(())
}

/// Refuse what would start a login, store a credential, start a server
/// outside the lab's process tree or reach Meta by another road: these
/// words anywhere in `args` (a subcommand, an option's value), from the
/// subcommands `muse --help` lists (1.4.10): `login`, `logout`, `auth`,
/// `mcp` (an MCP server's OAuth), `serve`, `session-message`, `plugins`,
/// `sandbox` (its setup), `voice` (a transcription service), and the option
/// `--api-key-stdin`.
///
/// # Errors
/// What was refused.
pub fn safe_args(args: &[String]) -> Result<(), String> {
    for a in args {
        if matches!(
            a.as_str(),
            "login"
                | "logout"
                | "auth"
                | "mcp"
                | "serve"
                | "session-message"
                | "plugins"
                | "sandbox"
                | "voice"
                | "--api-key-stdin"
        ) {
            return Err(format!(
                "muse: '{a}' is refused in the lab (a login, a credential, a server or a road to Meta outside the jail's reach)"
            ));
        }
    }
    Ok(())
}

/// The variables the launcher sets for the lab's own ends (the fake key, the
/// stub, the black hole): an `-e` may not replace them, nor name another
/// provider's key or base URL, which would carry a turn past the stub.
pub fn safe_env(kv: &str) -> Result<(), String> {
    let key = kv.split_once('=').map_or(kv, |(k, _)| k);
    let up = key.to_ascii_uppercase();
    let owned = up.ends_with("_API_KEY")
        || up.ends_with("_BASE_URL")
        || up.ends_with("_PROXY")
        || matches!(
            up.as_str(),
            "HOME" | "PATH" | "CODEX_HOME" | "CLAUDE_CONFIG_DIR"
        )
        || up.starts_with("XDG_");
    if owned {
        return Err(format!(
            "muse: -e {key} is the lab's to set (the fake key, the stub, the black hole, the scratch HOME)"
        ));
    }
    Ok(())
}

/// The launcher the tab types (`sh muse.sh`: `clear`, then `exec`), so the
/// tab's process IS Muse Code, launched by path ([module docs](self)).
#[must_use]
pub fn launcher(run: &Run, envs: &[String], args: &[String]) -> String {
    let w = run.w.to_string_lossy();
    let home = format!("{w}/home");
    let shim = run.run.join("shim");
    let px = &run.proxy;
    let stub = format!("http://127.0.0.1:{}", run.port);
    let q = |s: &str| shell_quote(s);
    let mut s = String::from("#!/bin/sh\nclear\n");
    s.push_str(
        "exec env -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY -u ANTHROPIC_BASE_URL \
         -u OPENROUTER_API_KEY -u OPENROUTER_BASE_URL -u CODEX_HOME -u CLAUDE_CONFIG_DIR -u MUSE_MODEL \\\n",
    );
    s.push_str(&format!(
        "  PATH=\"{}:$PATH\" HOME={} TMPDIR={} XDG_CONFIG_HOME={} XDG_DATA_HOME={} XDG_STATE_HOME={} XDG_CACHE_HOME={} \\\n",
        shim.display(),
        q(&home),
        q(&format!("{w}/tmp")),
        q(&format!("{home}/.config")),
        q(&format!("{home}/.local/share")),
        q(&format!("{home}/.local/state")),
        q(&format!("{home}/.cache"))
    ));
    s.push_str(&format!(
        "  HTTP_PROXY={px} HTTPS_PROXY={px} http_proxy={px} https_proxy={px} ALL_PROXY={px} all_proxy={px} \\\n"
    ));
    s.push_str("  NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \\\n");
    s.push_str(&format!(
        "  LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER={} \\\n",
        q(&format!("{w}/security.log")),
        q(&format!("{w}/open.log")),
        q(&shim.join("open").to_string_lossy())
    ));
    s.push_str(&format!(
        "  META_API_KEY={LAB_CODEX_KEY} TBH_AUTH_BASE_URL={stub} TBH_MINT_BASE_URL={stub} \\\n"
    ));
    s.push_str(
        "  TBH_DISABLE_TELEMETRY=1 TBH_DISABLE_FEATURE_CONFIG=1 TBH_DISABLE_DEFAULT_PLUGIN_INSTALL=1 \\\n",
    );
    for kv in envs {
        s.push_str(&format!("  {} \\\n", q(kv)));
    }
    s.push_str(&format!("  {} --base-url {stub}", q(&run.muse_bin)));
    for a in args {
        s.push(' ');
        s.push_str(&q(a));
    }
    s.push('\n');
    s
}

/// How `muse` launches.
#[derive(Debug, Default)]
pub struct Launch {
    /// `-e K=V`.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `-- <muse args>`.
    pub args: Vec<String>,
}

/// Parse `muse`'s options.
///
/// # Errors
/// An unknown option, `-e` with no value, or one that is the lab's to set ([`safe_env`]).
pub fn parse_launch(args: &[String]) -> Result<Launch, String> {
    let mut l = Launch::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" => {
                let kv = it.next().cloned().ok_or("muse: -e needs K=V")?;
                safe_env(&kv)?;
                l.envs.push(kv);
            }
            "--no-wait" => l.no_wait = true,
            "--" => l.args.extend(it.by_ref().cloned()),
            other => {
                return Err(format!(
                    "muse: unknown option '{other}' (use -e K=V, --no-wait, -- <muse args>)"
                ));
            }
        }
    }
    Ok(l)
}

/// Launch Muse Code in the lab's first tab; the `launched: …` line.
///
/// # Errors
/// No Muse Code, no witness, a refused argument, or the launch line could not be typed.
pub fn launch(run: &Run, l: &Launch) -> Result<String, Fail> {
    need_muse(run)?;
    crate::procs::need_witness(run)?;
    safe_args(&l.args)?;
    let mut run = run.clone();
    run.agent = "muse".into();
    run.save();
    let script = run.run.join("muse.sh");
    write_launcher(&script, &launcher(&run, &l.envs, &l.args))?;
    let door = Door::new(&run)?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    if door.ctl(&["send", &line])?.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    door.ctl(&["key", "enter"])?;
    Ok(format!(
        "launched: {} ({}) in lab '{}', META_API_KEY the lab's fake key, --base-url the stub  (launcher: {})",
        run.muse_bin,
        run.muse_ver,
        run.tag,
        script.display()
    ))
}

fn write_launcher(path: &Path, text: &str) -> Result<(), Fail> {
    crate::sys::put(path, 0o755, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))
}

/// What shows once Muse Code draws: its banner, its trust box, its caret.
pub const DRAWN: &str = "Muse Code|Do you trust this workspace|^❯";

/// `muse [tag] [-e K=V]... [--no-wait] [-- muse args]`.
pub fn cmd_muse(args: &[String]) -> Res {
    let (run, a) = lab(args)?;
    let l = parse_launch(&a)?;
    println!("{}", launch(&run, &l)?);
    let run = Run::load(&run.tag)?;
    if !l.no_wait && Door::new(&run)?.await_match(DRAWN, 60).unwrap_or(false) {
        println!("the TUI is drawing ({LAB} text {})", run.tag);
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_login_a_credential_or_a_server_never_runs_in_the_lab() {
        for bad in [
            "login",
            "logout",
            "auth",
            "mcp",
            "serve",
            "session-message",
            "plugins",
            "sandbox",
            "voice",
            "--api-key-stdin",
        ] {
            assert!(safe_args(&v(&["--trust-workspace", bad])).is_err(), "{bad}");
        }
        // NEGATIVE CONTROL: the options the captures pass, and a prompt that merely holds a word
        assert!(
            safe_args(&v(&[
                "--trust-workspace",
                "-c",
                "provider_retry.max_attempts=3",
                "please login later"
            ]))
            .is_ok()
        );
    }

    #[test]
    fn options_parse_and_the_launcher_execs_muse_by_path_at_the_stub() {
        let l = parse_launch(&v(&[
            "-e",
            "K=v 1",
            "--no-wait",
            "--",
            "--trust-workspace",
            "-c",
            "x=1",
        ]))
        .unwrap();
        assert!(l.no_wait && l.envs == ["K=v 1"]);
        assert_eq!(l.args, ["--trust-workspace", "-c", "x=1"]);
        assert!(parse_launch(&v(&["--bogus"])).is_err());
        assert!(parse_launch(&v(&["-e"])).is_err());
        let run = Run {
            tag: "t".into(),
            run: "/s/runs/t".into(),
            w: "/s/runs/t/scratch".into(),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            muse_bin: "/a b/muse".into(),
            ..Run::default()
        };
        let s = launcher(&run, &l.envs, &l.args);
        assert!(
            s.starts_with("#!/bin/sh\nclear\nexec env -u OPENAI_API_KEY "),
            "{s}"
        );
        for want in [
            " -u CODEX_HOME -u CLAUDE_CONFIG_DIR ",
            "  PATH=\"/s/runs/t/shim:$PATH\" HOME=/s/runs/t/scratch/home TMPDIR=/s/runs/t/scratch/tmp XDG_CONFIG_HOME=/s/runs/t/scratch/home/.config ",
            "  HTTP_PROXY=http://127.0.0.1:6 HTTPS_PROXY=http://127.0.0.1:6 ",
            "  META_API_KEY=lab-not-a-key TBH_AUTH_BASE_URL=http://127.0.0.1:5 TBH_MINT_BASE_URL=http://127.0.0.1:5 \\\n",
            "TBH_DISABLE_TELEMETRY=1 TBH_DISABLE_FEATURE_CONFIG=1",
            "  'K=v 1' \\\n",
        ] {
            assert!(s.contains(want), "{want} in\n{s}");
        }
        assert!(
            s.ends_with("  '/a b/muse' --base-url http://127.0.0.1:5 --trust-workspace -c x=1\n"),
            "{s}"
        );
    }

    /// An `-e` never replaces the fake key, the stub or the black hole.
    /// NEGATIVE CONTROL: an ordinary variable passes.
    #[test]
    fn an_extra_variable_never_replaces_the_lab_s_own() {
        for bad in [
            "META_API_KEY=real",
            "OPENAI_API_KEY=x",
            "TBH_AUTH_BASE_URL=https://example.com",
            "https_proxy=",
            "HOME=/Users//o",
            "XDG_CONFIG_HOME=/Users//o/.config",
        ] {
            assert!(parse_launch(&v(&["-e", bad])).is_err(), "{bad}");
        }
        assert!(parse_launch(&v(&["-e", "MUSE_DISABLE_APPROVAL_JUDGE=1"])).is_ok());
        assert!(safe_env("TBH_DISABLE_TELEMETRY=0").is_ok());
    }
}
