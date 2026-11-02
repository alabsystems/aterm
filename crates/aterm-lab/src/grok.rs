// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real Grok Build launcher and option validation against the lab stub, under the three guards
//! of `docs/DESIGN-harness-repos-2026-10-24.md` D27: the lab's key and the stub's base URLs, never
//! a sign-in; a scratch `HOME` and `GROK_HOME`; and the jail, which the launcher proves denies
//! `/usr/bin/open` before it execs Grok Build at all.

use std::path::Path;

use crate::LAB;
use crate::door::Door;
use crate::run::{Run, take_tag};
use crate::util::{shell_prepend_path, shell_quote};
use crate::{Fail, Res};

/// The key the lab hands Grok Build as `XAI_API_KEY`: no account's, and the stub knows it.
pub use crate::stub::xai::LAB_XAI_KEY;

/// Grok Build's home in the lab's scratch (`GROK_HOME`), relative to the scratch.
pub const HOME: &str = "home/.grok";

/// The exit status the launcher gives when the jail probe did not show `/usr/bin/open` denied.
pub const JAIL_UNPROVEN: i32 = 97;

/// What `/usr/bin/open` on a path that does not exist says inside the jail: the exec is denied
/// before `open` could look for the path.
pub const JAIL_DENIED: &str = "Operation not permitted";

fn lab(args: &[String]) -> Result<(Run, Vec<String>), Fail> {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    Ok((Run::load(&tag)?, a))
}

/// Checks that the lab has a Grok Build binary to run.
/// # Errors
/// None is there.
pub fn need_grok(run: &Run) -> Result<(), Fail> {
    if run.grok_bin.is_empty() || !crate::sys::executable(&run.grok_bin) {
        return Err(Fail::die(format!(
            "no Grok Build in lab '{}' (LAB_GROK=/path/to/a/scratch/store/grok at up)",
            run.tag
        )));
    }
    Ok(())
}

/// Rejects the verbs that sign in, update, serve, or reach a host outside the jail.
/// # Errors
/// What was refused.
pub fn safe_args(args: &[String]) -> Result<(), String> {
    for a in args {
        if matches!(
            a.as_str(),
            "login"
                | "logout"
                | "auth"
                | "update"
                | "upgrade"
                | "setup"
                | "mcp"
                | "plugin"
                | "plugins"
                | "worker"
                | "cursor-worker"
                | "leader"
                | "host-server"
                | "ssh"
                | "clone"
                | "wrap"
                | "workspace"
                | "dashboard"
                | "share"
                | "export"
                | "--device-auth"
                | "--leader-socket"
        ) {
            return Err(format!(
                "grok: '{a}' is refused in the lab (a sign-in, an update, a server or a road to xAI outside the jail's reach)"
            ));
        }
    }
    Ok(())
}

/// Rejects `-e` overrides for environment variables owned by the lab launcher.
/// # Errors
/// The variable is the lab's to set.
pub fn safe_env(kv: &str) -> Result<(), String> {
    let key = kv.split_once('=').map_or(kv, |(k, _)| k);
    let up = key.to_ascii_uppercase();
    let owned = up.ends_with("_API_KEY")
        || up.ends_with("_URL")
        || up.ends_with("_PROXY")
        || up.starts_with("XAI_")
        || up.starts_with("GROK_AUTH")
        || up.starts_with("XDG_")
        || matches!(
            up.as_str(),
            "HOME"
                | "PATH"
                | "BROWSER"
                | "GROK_HOME"
                | "GROK_DEPLOYMENT_KEY"
                | "GROK_DISABLE_AUTOUPDATER"
                | "GROK_LOGIN_DEVICE_FLOW"
                | "CODEX_HOME"
                | "CLAUDE_CONFIG_DIR"
        );
    if owned {
        return Err(format!(
            "grok: -e {key} is the lab's to set (the lab key, the stub, the black hole, the scratch HOME, no browser)"
        ));
    }
    Ok(())
}

/// Builds the `grok.sh` launcher script executed inside the lab tab: the jail probe first, then
/// Grok Build by path with the lab key, the stub's base URLs and the scratch homes.
#[must_use]
pub fn launcher(run: &Run, envs: &[String], args: &[String]) -> String {
    let w = run.w.to_string_lossy();
    let home = format!("{w}/home");
    let shim = run.run.join("shim");
    let px = shell_quote(&run.proxy);
    let api = shell_quote(&format!("http://127.0.0.1:{}/v1", run.port));
    let q = |s: &str| shell_quote(s);
    let mut s = String::from("#!/bin/sh\nclear\n");
    s.push_str(&format!(
        "probe=$(/usr/bin/open /nonexistent/aterm-lab-jail-probe 2>&1)\n\
         case $probe in *{}*) ;; *) printf 'aterm-lab: the jail probe did not deny /usr/bin/open: %s\\n' \"$probe\" >&2; exit {JAIL_UNPROVEN};; esac\n",
        q(JAIL_DENIED)
    ));
    s.push_str(
        "exec env -u OPENAI_API_KEY -u OPENAI_BASE_URL -u ANTHROPIC_API_KEY -u ANTHROPIC_BASE_URL \
         -u OPENROUTER_API_KEY -u OPENROUTER_BASE_URL -u CODEX_HOME -u CLAUDE_CONFIG_DIR \
         -u GROK_CODE_XAI_API_KEY -u GROK_DEPLOYMENT_KEY -u GROK_AUTH_PATH -u GROK_CONFIG -u GROK_CONFIG_PATH \\\n",
    );
    s.push_str(&format!(
        "  PATH={} HOME={} TMPDIR={} XDG_CONFIG_HOME={} XDG_DATA_HOME={} XDG_STATE_HOME={} XDG_CACHE_HOME={} \\\n",
        shell_prepend_path(&shim.to_string_lossy()),
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
        "  LAB_SECURITY_LOG={} LAB_OPEN_LOG={} BROWSER=/usr/bin/false \\\n",
        q(&format!("{w}/security.log")),
        q(&format!("{w}/open.log")),
    ));
    s.push_str(&format!(
        "  GROK_HOME={} XAI_API_KEY={LAB_XAI_KEY} GROK_XAI_API_BASE_URL={api} GROK_CLI_CHAT_PROXY_BASE_URL={api} \\\n",
        q(&format!("{w}/{HOME}"))
    ));
    s.push_str("  GROK_DISABLE_AUTOUPDATER=1 GROK_TELEMETRY_ENABLED=0 \\\n");
    for kv in envs {
        s.push_str(&format!("  {} \\\n", q(kv)));
    }
    s.push_str(&format!("  {}", q(&run.grok_bin)));
    for a in args {
        s.push(' ');
        s.push_str(&q(a));
    }
    s.push('\n');
    s
}

/// How `grok` launches.
#[derive(Debug, Default)]
pub struct Launch {
    /// `-e K=V`.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `-- <grok args>`.
    pub args: Vec<String>,
}

/// Parse `grok`'s options.
/// # Errors
/// An unknown option, `-e` with no value, or one that is the lab's to set ([`safe_env`]).
pub fn parse_launch(args: &[String]) -> Result<Launch, String> {
    let mut l = Launch::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" => {
                let kv = it.next().cloned().ok_or("grok: -e needs K=V")?;
                safe_env(&kv)?;
                l.envs.push(kv);
            }
            "--no-wait" => l.no_wait = true,
            "--" => l.args.extend(it.by_ref().cloned()),
            other => {
                return Err(format!(
                    "grok: unknown option '{other}' (use -e K=V, --no-wait, -- <grok args>)"
                ));
            }
        }
    }
    Ok(l)
}

/// Launch Grok Build in the lab's first tab; the `launched: …` line.
/// # Errors
/// No Grok Build, no witness, a refused argument, or the launch line could not be typed.
pub fn launch(run: &Run, l: &Launch) -> Result<String, Fail> {
    need_grok(run)?;
    crate::procs::need_witness(run)?;
    safe_args(&l.args)?;
    let mut run = run.clone();
    run.agent = "grok".into();
    run.save();
    let script = run.run.join("grok.sh");
    write_launcher(&script, &launcher(&run, &l.envs, &l.args))?;
    let door = Door::new(&run)?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    if door.ctl(&["send", &line])?.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    door.ctl(&["key", "enter"])?;
    Ok(format!(
        "launched: {} ({}) in lab '{}', XAI_API_KEY the lab's key, GROK_XAI_API_BASE_URL and \
         GROK_CLI_CHAT_PROXY_BASE_URL the stub  (launcher: {})",
        run.grok_bin,
        run.grok_ver,
        run.tag,
        script.display()
    ))
}

fn write_launcher(path: &Path, text: &str) -> Result<(), Fail> {
    crate::sys::put(path, 0o755, &crate::sys::PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))
}

/// What shows once Grok Build draws, or once the launcher refused it.
pub const DRAWN: &str = "ctrl\\+q|Grok Build|trust|jail probe";

/// `grok [tag] [-e K=V]... [--no-wait] [-- grok args]`.
pub fn cmd_grok(args: &[String]) -> Res {
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
    fn a_sign_in_an_update_or_a_server_never_runs_in_the_lab() {
        for bad in [
            "login",
            "logout",
            "update",
            "mcp",
            "leader",
            "worker",
            "host-server",
            "--device-auth",
            "--leader-socket",
        ] {
            assert!(safe_args(&v(&["--model", "m", bad])).is_err(), "{bad}");
        }
        assert!(safe_args(&v(&["-r", "--model", "m", "please login later"])).is_ok());
    }

    #[test]
    fn options_parse_and_the_launcher_proves_the_jail_then_execs_grok_at_the_stub() {
        let l = parse_launch(&v(&["-e", "K=v 1", "--no-wait", "--", "-r", "x"])).unwrap();
        assert!(l.no_wait && l.envs == ["K=v 1"]);
        assert_eq!(l.args, ["-r", "x"]);
        assert!(parse_launch(&v(&["--bogus"])).is_err());
        assert!(parse_launch(&v(&["-e"])).is_err());
        let run = Run {
            tag: "t".into(),
            run: "/s/runs/t".into(),
            w: "/s/runs/t/scratch".into(),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            grok_bin: "/a b/grok".into(),
            ..Run::default()
        };
        let s = launcher(&run, &l.envs, &l.args);
        let probe = s.find("probe=$(/usr/bin/open ").expect("the probe");
        let exec = s.find("exec env -u OPENAI_API_KEY ").expect("the exec");
        assert!(probe < exec, "the probe runs before Grok Build\n{s}");
        assert!(
            s.contains(&format!("case $probe in *'{JAIL_DENIED}'*) ;; *) printf ")),
            "{s}"
        );
        assert!(s.contains(&format!("exit {JAIL_UNPROVEN};; esac\n")), "{s}");
        for want in [
            " -u GROK_CODE_XAI_API_KEY -u GROK_DEPLOYMENT_KEY -u GROK_AUTH_PATH ",
            "  PATH=\"/s/runs/t/shim:$PATH\" HOME=/s/runs/t/scratch/home TMPDIR=/s/runs/t/scratch/tmp XDG_CONFIG_HOME=/s/runs/t/scratch/home/.config ",
            "  HTTP_PROXY=http://127.0.0.1:6 HTTPS_PROXY=http://127.0.0.1:6 ",
            " BROWSER=/usr/bin/false \\\n",
            "  GROK_HOME=/s/runs/t/scratch/home/.grok XAI_API_KEY=xai-lab-not-a-key \
             GROK_XAI_API_BASE_URL=http://127.0.0.1:5/v1 GROK_CLI_CHAT_PROXY_BASE_URL=http://127.0.0.1:5/v1 \\\n",
            "  GROK_DISABLE_AUTOUPDATER=1 ",
            "  'K=v 1' \\\n",
        ] {
            assert!(s.contains(want), "{want} in\n{s}");
        }
        assert!(s.ends_with("  '/a b/grok' -r x\n"), "{s}");
    }

    #[test]
    fn an_extra_variable_never_replaces_the_lab_s_own() {
        for bad in [
            "XAI_API_KEY=real",
            "GROK_CODE_XAI_API_KEY=x",
            "GROK_XAI_API_BASE_URL=https://api.x.ai/v1",
            "GROK_CLI_CHAT_PROXY_BASE_URL=https://cli-chat-proxy.grok.com/v1",
            "GROK_MODELS_LIST_URL=https://api.x.ai/v1/models",
            "GROK_HOME=/Users//o/.grok",
            "GROK_AUTH_PATH=/Users//o/.grok/auth.json",
            "GROK_DEPLOYMENT_KEY=k",
            "GROK_DISABLE_AUTOUPDATER=0",
            "BROWSER=open",
            "https_proxy=",
            "HOME=/Users//o",
            "XDG_CONFIG_HOME=/Users//o/.config",
        ] {
            assert!(parse_launch(&v(&["-e", bad])).is_err(), "{bad}");
        }
        assert!(parse_launch(&v(&["-e", "GROK_FOLDER_TRUST=0"])).is_ok());
    }
}
