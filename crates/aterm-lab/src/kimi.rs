// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real Kimi Code (`kimi`) launcher against the lab stub's Chat Completions routes: a scratch
//! `HOME` and `KIMI_CODE_HOME` whose `config.toml` names one `openai`-type provider at the stub
//! with the lab's fake key, so no login, OAuth or Moonshot host is ever in play.

use std::path::Path;

use crate::LAB;
use crate::door::Door;
use crate::run::{Run, take_tag};
use crate::stub::LAB_CODEX_KEY;
use crate::stub::chat::MODEL;
use crate::util::{shell_prepend_path, shell_quote};
use crate::{Fail, Res};

/// The CLI verb and fixture prefix.
pub const NAME: &str = "kimi";

/// The scratch `KIMI_CODE_HOME`, under the lab's scratch `HOME` where the default home sits.
pub const HOME: &str = "home/.kimi-code";

/// What shows once Kimi Code draws: its welcome box, its trust prompt, its composer.
pub const DRAWN: &str = "Welcome to Kimi Code|Trust this folder|^ *│ > ";

fn lab(args: &[String]) -> Result<(Run, Vec<String>), Fail> {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    Ok((Run::load(&tag)?, a))
}

/// Checks that the lab has a Kimi Code binary to run.
/// # Errors
/// None is there.
pub fn need(run: &Run) -> Result<(), Fail> {
    if run.kimi_bin.is_empty() || !crate::sys::executable(&run.kimi_bin) {
        return Err(Fail::die(format!(
            "no Kimi Code in lab '{}' (aterm pkg install kimi, or LAB_KIMI=/path/to/kimi at up)",
            run.tag
        )));
    }
    Ok(())
}

/// The scratch `config.toml`: the lab provider at the stub's `/v1`, its fake key, and the one
/// model the stub's catalog lists as the default.
#[must_use]
pub fn config(port: &str) -> String {
    format!(
        "default_model = \"lab\"\n\n[providers.lab]\ntype = \"openai\"\nbase_url = \
         \"http://127.0.0.1:{port}/v1\"\napi_key = \"{LAB_CODEX_KEY}\"\n\n[models.lab]\nprovider = \
         \"lab\"\nmodel = \"{MODEL}\"\nmax_context_size = 100000\n"
    )
}

/// Rejects subcommands that sign in, serve, update, open a browser or reach a registry.
/// # Errors
/// What was refused.
pub fn safe_args(args: &[String]) -> Result<(), String> {
    match args.iter().find(|a| {
        matches!(
            a.as_str(),
            "login"
                | "logout"
                | "acp"
                | "upgrade"
                | "update"
                | "web"
                | "server"
                | "rc"
                | "vis"
                | "install-desktop"
                | "provider"
                | "doctor"
                | "--login"
        )
    }) {
        Some(a) => Err(format!(
            "{NAME}: '{a}' is refused in the lab (a sign-in, a server, an update or a road to Moonshot)"
        )),
        None => Ok(()),
    }
}

/// Rejects `-e` overrides for environment variables owned by the lab launcher.
/// # Errors
/// What was refused.
pub fn safe_env(kv: &str) -> Result<(), String> {
    let key = kv.split_once('=').map_or(kv, |(k, _)| k);
    let up = key.to_ascii_uppercase();
    let owned = up.ends_with("_API_KEY")
        || up.ends_with("_BASE_URL")
        || up.ends_with("_PROXY")
        || up.starts_with("KIMI_")
        || up.starts_with("MOONSHOT_")
        || matches!(up.as_str(), "HOME" | "PATH" | "BROWSER")
        || up.starts_with("XDG_");
    if owned {
        return Err(format!(
            "{NAME}: -e {key} is the lab's to set (the fake key, the stub, the black hole, the scratch homes)"
        ));
    }
    Ok(())
}

/// Builds the `kimi.sh` launcher script executed inside the lab tab.
#[must_use]
pub fn launcher(run: &Run, envs: &[String], args: &[String]) -> String {
    let w = run.w.to_string_lossy();
    let home = format!("{w}/home");
    let shim = run.run.join("shim");
    let px = shell_quote(&run.proxy);
    let q = |s: &str| shell_quote(s);
    let mut s = String::from("#!/bin/sh\nclear\n");
    s.push_str(
        "exec env -u KIMI_API_KEY -u KIMI_BASE_URL -u KIMI_MODEL_NAME -u KIMI_MODEL_API_KEY \
         -u KIMI_CODE_CUSTOM_HEADERS -u KIMI_CODE_BASE_URL -u KIMI_CODE_OAUTH_HOST -u MOONSHOT_API_KEY \
         -u MOONSHOT_BASE_URL -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY \\\n",
    );
    s.push_str(&format!(
        "  PATH={} HOME={} TMPDIR={} KIMI_CODE_HOME={} \\\n",
        shell_prepend_path(&shim.to_string_lossy()),
        q(&home),
        q(&format!("{w}/tmp")),
        q(&format!("{w}/{HOME}"))
    ));
    s.push_str(&format!(
        "  XDG_CONFIG_HOME={} XDG_DATA_HOME={} XDG_STATE_HOME={} XDG_CACHE_HOME={} \\\n",
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
    s.push_str("  KIMI_CODE_NO_AUTO_UPDATE=1 KIMI_DISABLE_TELEMETRY=1 \\\n");
    for kv in envs {
        s.push_str(&format!("  {} \\\n", q(kv)));
    }
    s.push_str(&format!("  {}", q(&run.kimi_bin)));
    for a in args {
        s.push(' ');
        s.push_str(&q(a));
    }
    s.push('\n');
    s
}

/// How `kimi` launches.
#[derive(Debug, Default)]
pub struct Launch {
    /// `-e K=V`.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `-- <kimi args>`.
    pub args: Vec<String>,
}

/// Parse `kimi`'s options.
/// # Errors
/// An unknown option, `-e` with no value, or one that is the lab's to set ([`safe_env`]).
pub fn parse_launch(args: &[String]) -> Result<Launch, String> {
    let mut l = Launch::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" => {
                let kv = it
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{NAME}: -e needs K=V"))?;
                safe_env(&kv)?;
                l.envs.push(kv);
            }
            "--no-wait" => l.no_wait = true,
            "--" => l.args.extend(it.by_ref().cloned()),
            other => {
                return Err(format!(
                    "{NAME}: unknown option '{other}' (use -e K=V, --no-wait, -- <{NAME} args>)"
                ));
            }
        }
    }
    Ok(l)
}

fn put(path: &Path, mode: u32, text: &str) -> Result<(), Fail> {
    crate::sys::put(path, mode, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))
}

/// Launch Kimi Code in the lab's first tab; the `launched: …` line.
/// # Errors
/// No Kimi Code, no witness, a refused argument, or the launch line could not be typed.
pub fn launch(run: &Run, l: &Launch) -> Result<String, Fail> {
    need(run)?;
    crate::procs::need_witness(run)?;
    safe_args(&l.args)?;
    let mut run = run.clone();
    run.agent = NAME.into();
    run.save();
    let khome = run.w.join(HOME);
    std::fs::create_dir_all(&khome)
        .map_err(|e| Fail::die(format!("cannot make {}: {e}", khome.display())))?;
    put(&khome.join("config.toml"), 0o600, &config(&run.port))?;
    let script = run.run.join("kimi.sh");
    put(&script, 0o755, &launcher(&run, &l.envs, &l.args))?;
    let door = Door::new(&run)?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    if door.ctl(&["send", &line])?.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    door.ctl(&["key", "enter"])?;
    Ok(format!(
        "launched: {} ({}) in lab '{}', its provider the stub's /v1 with the lab's fake key  (launcher: {})",
        run.kimi_bin,
        run.kimi_ver,
        run.tag,
        script.display()
    ))
}

/// `kimi [tag] [-e K=V]... [--no-wait] [-- kimi args]`.
pub fn cmd_kimi(args: &[String]) -> Res {
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
#[path = "kimi_tests.rs"]
mod tests;
