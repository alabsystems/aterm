// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The REAL Gemini CLI and the REAL Antigravity CLI (`agy`) in the lab's
//! tab, each launched by path against the stub's Gemini API
//! ([`crate::stub::gemini`]): `GEMINI_API_KEY` is the lab's fake key
//! ([`LAB_GEMINI_KEY`]) and `GOOGLE_GEMINI_BASE_URL` the stub. Neither may
//! use a Google account or reach Google: each gets a scratch `HOME` of its
//! own (`gemini-home/`, `agy-home/`) seeded before every launch so that it
//! shows no sign-in, auth or onboarding screen and opens no browser, and the
//! jail keeps every connection on loopback (the owner's `~/.gemini` is on the
//! protected list).
//!
//! * Gemini CLI (Homebrew's `gemini-cli`, `$LAB_GEMINI`): `.gemini/settings.json`
//!   selects the `gemini-api-key` auth (without it the CLI infers its
//!   `gateway` auth from the base URL, refuses it, and opens its auth dialog),
//!   turns the folder trust off (`--untrusted`: on, with no trusted folder, so
//!   its trust dialog draws), the usage statistics (Clearcut), the telemetry,
//!   the update check and the IDE nudge off, and names a theme (no theme
//!   picker). No proxy: Gemini CLI routes even the loopback base URL through
//!   one, so the jail alone refuses what else it reaches for. `NO_BROWSER`
//!   is set and `BROWSER` is the open recorder; `TMPDIR` is the scratch's
//!   `tmp/`, where it writes its client error reports. It relaunches itself
//!   as a parent node and a child, and the parent ignores SIGTERM: `down`
//!   ends it with SIGKILL after its 4 s (`ignored TERM -> KILL`).
//! * agy (the managed store's, `$LAB_AGY`): `.gemini/antigravity-cli/settings.json`
//!   holds `"modelProvider": "gemini"` (without it agy signs in with a Google
//!   account in a browser: never launched without it) and the project in
//!   `trustedWorkspaces` (`--untrusted`: none, so its trust prompt draws);
//!   `cache/onboarding.json` marks the onboarding done (`--onboarding`: no
//!   onboarding and no trust seed, so its first-run screens draw; the
//!   provider stays). Its feature-flag and telemetry hosts
//!   (`antigravity-unleash.goog`, `play.googleapis.com`, which it asks for
//!   even on the API key) go to the black-hole proxy, which logs them; its
//!   auto-update is off as the managed launcher turns it off.
//!
//! A login (`login`, `logout`, `auth`) is refused in either's arguments, and
//! a Gemini CLI sandbox (profiles do not nest). The version of each is read
//! at `up` from its package (Gemini CLI's `package.json`, agy's atpkg build
//! record), else from its `--version` in the jail once its home is seeded,
//! never by running it unjailed or unseeded. The seeds are written and taken
//! out through no link ([`put`], [`scratch_remove`]): the agent writes the
//! directories they sit in.

use std::path::Path;

use crate::door::Door;
use crate::pyjson::{J, Style};
use crate::run::{Run, take_tag};
use crate::sys::{PutSrc, put, scratch_remove};
use crate::util::shell_quote;
use crate::{Fail, LAB, Res};

/// The fake key both CLIs send as `x-goog-api-key` (the stub logs `auth=gemini-key`).
pub use crate::stub::gemini::LAB_GEMINI_KEY;

/// The two agents of this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    /// Gemini CLI.
    Gemini,
    /// The Antigravity CLI.
    Agy,
}

impl Agent {
    /// The verb, the `env.sh` agent and the fixtures' prefix.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Agent::Gemini => "gemini",
            Agent::Agy => "agy",
        }
    }

    /// The binary the lab runs, as `up` recorded it (empty: none).
    #[must_use]
    pub fn bin(self, run: &Run) -> &str {
        match self {
            Agent::Gemini => &run.gemini_bin,
            Agent::Agy => &run.agy_bin,
        }
    }

    /// Its version, as `up` read it.
    #[must_use]
    pub fn ver(self, run: &Run) -> &str {
        match self {
            Agent::Gemini => &run.gemini_ver,
            Agent::Agy => &run.agy_ver,
        }
    }

    /// Its scratch `HOME`, under the lab's scratch.
    #[must_use]
    pub fn home(self) -> &'static str {
        match self {
            Agent::Gemini => "gemini-home",
            Agent::Agy => "agy-home",
        }
    }

    /// The rows that show its TUI is drawing (a trust, onboarding or idle screen).
    #[must_use]
    pub fn drawn(self) -> &'static str {
        match self {
            Agent::Gemini => "Type your message|trust the files|Gemini CLI|gemini-api-key",
            Agent::Agy => "Antigravity CLI|trust the contents|color scheme|Terms of Service",
        }
    }
}

/// The lab has this agent to run.
///
/// # Errors
/// None was found at `up`.
pub fn need(run: &Run, agent: Agent) -> Result<(), Fail> {
    let bin = agent.bin(run);
    if bin.is_empty() || !crate::sys::executable(bin) {
        let how = match agent {
            Agent::Gemini => "brew install gemini-cli, or LAB_GEMINI=/path/to/gemini at up",
            Agent::Agy => "aterm pkg install agy, or LAB_AGY=/path/to/agy at up",
        };
        return Err(Fail::die(format!(
            "no {} in lab '{}' ({how})",
            agent.name(),
            run.tag
        )));
    }
    Ok(())
}

const PRETTY: Style = Style {
    indent: Some(2),
    compact: false,
    ascii: true,
};

/// Gemini CLI's `.gemini/settings.json`: the API-key auth, no statistics,
/// telemetry, update check or IDE nudge, a theme, and the folder trust off
/// unless `untrusted` (then on, so the trust dialog draws for the project).
#[must_use]
pub fn gemini_settings(untrusted: bool) -> String {
    J::obj([
        (
            "security",
            J::obj([
                ("auth", J::obj([("selectedType", J::s("gemini-api-key"))])),
                ("folderTrust", J::obj([("enabled", J::Bool(untrusted))])),
            ]),
        ),
        (
            "privacy",
            J::obj([("usageStatisticsEnabled", J::Bool(false))]),
        ),
        ("telemetry", J::obj([("enabled", J::Bool(false))])),
        (
            "general",
            J::obj([
                ("enableAutoUpdate", J::Bool(false)),
                ("enableAutoUpdateNotification", J::Bool(false)),
            ]),
        ),
        ("ui", J::obj([("theme", J::s("Default"))])),
        (
            "ide",
            J::obj([("enabled", J::Bool(false)), ("hasSeenNudge", J::Bool(true))]),
        ),
    ])
    .dump(PRETTY)
}

/// agy's `.gemini/antigravity-cli/settings.json`: the Gemini API provider
/// (never left out: without it agy signs in with a Google account), and the
/// trusted workspaces (none: its trust prompt draws).
#[must_use]
pub fn agy_settings(trusted: &[String]) -> String {
    agy_settings_with(trusted, &[])
}

/// [`agy_settings`] with `extra` keys set after (`--setting KEY=VALUE`): one
/// of agy's own settings a measurement turns, e.g. `hideActiveItems`, the key
/// of `Show active subagents and background tasks below the input box`. The
/// provider is never one of them: a sign-in never runs here.
#[must_use]
pub fn agy_settings_with(trusted: &[String], extra: &[(String, J)]) -> String {
    let mut j = J::obj([("modelProvider", J::s("gemini"))]);
    if !trusted.is_empty() {
        j.set(
            "trustedWorkspaces",
            J::Arr(trusted.iter().map(J::s).collect()),
        );
    }
    for (key, value) in extra {
        if key != "modelProvider" {
            j.set(key, value.clone());
        }
    }
    j.dump(PRETTY)
}

/// One `--setting KEY=VALUE`: the value read as JSON (`true`, `3`, `"x"`),
/// else as a string; `None` for no `=`, an empty key, or the provider's key.
#[must_use]
pub fn setting_of(kv: &str) -> Option<(String, J)> {
    let (key, value) = kv.split_once('=')?;
    if key.is_empty() || key == "modelProvider" {
        return None;
    }
    let value = J::parse(value).unwrap_or_else(|_| J::s(value));
    Some((key.to_string(), value))
}

/// agy's `.gemini/antigravity-cli/cache/onboarding.json`: the onboarding done.
#[must_use]
pub fn agy_onboarded() -> String {
    J::obj([
        ("consumerOnboardingComplete", J::Bool(true)),
        ("enterpriseOnboardingComplete", J::Bool(false)),
        ("onboardingComplete", J::Bool(true)),
    ])
    .dump(PRETTY)
}

/// Refuse what would start a login (a Google sign-in) or a sandbox inside the
/// jail, anywhere in `args`.
///
/// # Errors
/// What was refused.
pub fn safe_args(agent: Agent, args: &[String]) -> Result<(), String> {
    for a in args {
        match (agent, a.as_str()) {
            (_, "login" | "logout" | "auth") => {
                return Err(format!(
                    "{}: '{a}' is refused in the lab (a Google sign-in never runs here)",
                    agent.name()
                ));
            }
            (Agent::Gemini, "-s" | "--sandbox") => {
                return Err(
                    "gemini: a sandbox is refused in the lab (sandbox profiles do not nest: the jail is the sandbox)"
                        .into(),
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// How `gemini` and `agy` launch.
#[derive(Debug, Default)]
pub struct Launch {
    /// `-e K=V`.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `--untrusted`: no trust seed, so the folder-trust dialog draws.
    pub untrusted: bool,
    /// `--onboarding` (agy): no onboarding seed either, so the first-run screens draw.
    pub onboarding: bool,
    /// `--setting KEY=VALUE` (agy): a key of its `settings.json` set beside the seed's
    /// ([`agy_settings_with`]).
    pub settings: Vec<(String, J)>,
    /// `-- <args>`.
    pub args: Vec<String>,
}

/// Parse `gemini`'s or `agy`'s options.
///
/// # Errors
/// An unknown option, or `-e` with no value.
pub fn parse_launch(agent: Agent, args: &[String]) -> Result<Launch, String> {
    let name = agent.name();
    let mut l = Launch::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (agent, a.as_str()) {
            (_, "-e") => l.envs.push(
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{name}: -e needs K=V"))?,
            ),
            (_, "--no-wait") => l.no_wait = true,
            (_, "--untrusted") => l.untrusted = true,
            (Agent::Agy, "--onboarding") => {
                l.onboarding = true;
                l.untrusted = true;
            }
            (Agent::Agy, "--setting") => {
                l.settings
                    .push(it.next().and_then(|kv| setting_of(kv)).ok_or_else(|| {
                        format!("{name}: --setting needs KEY=VALUE (never the provider)")
                    })?)
            }
            (_, "--") => l.args.extend(it.by_ref().cloned()),
            (_, other) => {
                return Err(format!(
                    "{name}: unknown option '{other}' (use -e K=V, --no-wait, --untrusted{}, -- <{name} args>)",
                    if agent == Agent::Agy {
                        ", --onboarding, --setting KEY=VALUE"
                    } else {
                        ""
                    }
                ));
            }
        }
    }
    Ok(l)
}

/// The launcher the tab types (`clear`, then `exec`, so the tab's process IS
/// the agent): the scratch `HOME`, the fake key, the stub's base URL, the open
/// recorder and the keychain tripwire's log, and for agy the black-hole proxy.
#[must_use]
pub fn launcher(run: &Run, agent: Agent, envs: &[String], args: &[String]) -> String {
    let w = run.w.to_string_lossy();
    let shim = run.run.join("shim");
    let px = &run.proxy;
    let mut s = String::from("#!/bin/sh\nclear\n");
    s.push_str(
        "exec env -u GOOGLE_API_KEY -u GOOGLE_GENAI_USE_VERTEXAI -u GOOGLE_CLOUD_PROJECT -u GOOGLE_APPLICATION_CREDENTIALS \\\n",
    );
    if agent == Agent::Gemini {
        // Gemini CLI routes even the loopback base URL through a proxy it is handed: none, the jail alone
        s.push_str(
            "  -u HTTP_PROXY -u HTTPS_PROXY -u http_proxy -u https_proxy -u ALL_PROXY -u all_proxy \\\n",
        );
    }
    s.push_str(&format!(
        "  PATH=\"{}:$PATH\" HOME={} TMPDIR={} \\\n",
        shim.display(),
        shell_quote(&format!("{w}/{}", agent.home())),
        shell_quote(&format!("{w}/tmp"))
    ));
    if agent == Agent::Agy {
        s.push_str(&format!(
            "  HTTP_PROXY={px} HTTPS_PROXY={px} http_proxy={px} https_proxy={px} ALL_PROXY={px} all_proxy={px} \\\n"
        ));
        s.push_str("  NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \\\n");
    }
    s.push_str(&format!(
        "  GEMINI_API_KEY={LAB_GEMINI_KEY} GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:{} \\\n",
        run.port
    ));
    s.push_str(&format!(
        "  {} BROWSER={} LAB_OPEN_LOG={} LAB_SECURITY_LOG={} \\\n",
        match agent {
            Agent::Gemini => "NO_BROWSER=true",
            Agent::Agy => "AGY_CLI_DISABLE_AUTO_UPDATE=true",
        },
        shell_quote(&shim.join("open").to_string_lossy()),
        shell_quote(&format!("{w}/open.log")),
        shell_quote(&format!("{w}/security.log"))
    ));
    for kv in envs {
        s.push_str(&format!("  {} \\\n", shell_quote(kv)));
    }
    s.push_str(&format!("  {}", shell_quote(agent.bin(run))));
    for a in args {
        s.push(' ');
        s.push_str(&shell_quote(a));
    }
    s.push('\n');
    s
}

fn put_text(path: &Path, mode: u32, text: &str) -> Result<(), Fail> {
    put(path, mode, &PutSrc::Bytes(text.as_bytes()))
        .map_err(|e| Fail::die(format!("cannot write {}: {e}", path.display())))
}

/// Remove a seed file of the scratch home through no link (the agent, jailed,
/// writes the directories it sits in).
fn unseed(path: &Path) -> Result<(), Fail> {
    scratch_remove(path).map_err(|e| Fail::die(format!("cannot remove {}: {e}", path.display())))
}

/// Seed the agent's scratch home for this launch (through no link): the
/// files that keep its sign-in, auth and onboarding screens away, and the
/// trust seed unless `untrusted`.
///
/// # Errors
/// A write was refused.
pub fn seed(run: &Run, agent: Agent, l: &Launch) -> Result<(), Fail> {
    let home = run.w.join(agent.home());
    match agent {
        Agent::Gemini => {
            put_text(
                &home.join(".gemini/settings.json"),
                0o600,
                &gemini_settings(l.untrusted),
            )?;
            if l.untrusted {
                unseed(&home.join(".gemini/trustedFolders.json"))?;
            }
        }
        Agent::Agy => {
            let dir = home.join(".gemini/antigravity-cli");
            let proj = std::fs::canonicalize(&run.proj)
                .map_or(run.proj.clone(), |p| p.to_string_lossy().into_owned());
            let mut trusted = vec![proj, run.proj.clone()];
            trusted.dedup();
            if l.untrusted {
                trusted.clear();
            }
            put_text(
                &dir.join("settings.json"),
                0o600,
                &agy_settings_with(&trusted, &l.settings),
            )?;
            if l.onboarding {
                unseed(&dir.join("cache/onboarding.json"))?;
            } else {
                put_text(&dir.join("cache/onboarding.json"), 0o600, &agy_onboarded())?;
            }
        }
    }
    Ok(())
}

/// Launch the agent in the lab's first tab; the `launched: …` line.
///
/// # Errors
/// No such agent, no witness, a refused argument, or the launch line could not be typed.
pub fn launch(run: &Run, agent: Agent, l: &Launch) -> Result<String, Fail> {
    need(run, agent)?;
    crate::procs::need_witness(run)?;
    safe_args(agent, &l.args)?;
    let mut run = run.clone();
    run.agent = agent.name().into();
    run.save();
    seed(&run, agent, l)?;
    let script = run.run.join(format!("{}.sh", agent.name()));
    put_text(&script, 0o755, &launcher(&run, agent, &l.envs, &l.args))?;
    let door = Door::new(&run)?;
    let line = format!("sh {}", shell_quote(&script.to_string_lossy()));
    if door.ctl(&["send", &line])?.code != 0 {
        return Err(Fail::die("could not type the launch line"));
    }
    let entered = door.ctl(&["key", "enter"])?;
    if entered.code != 0 {
        return Err(Fail::die(format!(
            "could not submit the launch line (exit {}): {}{}",
            entered.code, entered.stdout, entered.stderr
        )));
    }
    Ok(format!(
        "launched: {} ({}) in lab '{}'{}{}  (launcher: {})",
        agent.bin(&run),
        agent.ver(&run),
        run.tag,
        if l.untrusted { ", untrusted" } else { "" },
        if l.onboarding {
            ", onboarding not done"
        } else {
            ""
        },
        script.display()
    ))
}

/// `gemini|agy [tag] [-e K=V]... [--no-wait] [--untrusted] [--onboarding] [-- args]`.
fn cmd(agent: Agent, args: &[String]) -> Res {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    let run = Run::load(&tag)?;
    let l = parse_launch(agent, &a)?;
    println!("{}", launch(&run, agent, &l)?);
    let run = Run::load(&tag)?;
    if !l.no_wait
        && Door::new(&run)?
            .await_match(agent.drawn(), 60)
            .unwrap_or(false)
    {
        println!("the TUI is drawing ({LAB} text {tag})");
    }
    Ok(0)
}

/// `gemini [tag] [-e K=V]... [--no-wait] [--untrusted] [-- gemini args]`.
pub fn cmd_gemini(args: &[String]) -> Res {
    cmd(Agent::Gemini, args)
}

/// `agy [tag] [-e K=V]... [--no-wait] [--untrusted] [--onboarding] [-- agy args]`.
pub fn cmd_agy(args: &[String]) -> Res {
    cmd(Agent::Agy, args)
}

#[cfg(test)]
#[path = "gemini_launch_tests.rs"]
mod launch_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_sign_in_or_a_nested_sandbox_never_runs_in_the_lab() {
        for agent in [Agent::Gemini, Agent::Agy] {
            for bad in ["login", "logout", "auth"] {
                assert!(safe_args(agent, &v(&["-p", "x", bad])).is_err(), "{bad}");
            }
            assert!(safe_args(agent, &v(&["-p", "hi"])).is_ok());
        }
        assert!(safe_args(Agent::Gemini, &v(&["--sandbox"])).is_err());
        assert!(safe_args(Agent::Gemini, &v(&["-s"])).is_err());
        assert!(safe_args(Agent::Agy, &v(&["-s"])).is_ok());
    }

    /// `--setting KEY=VALUE` sets one of agy's own keys beside the seed's, the
    /// value read as JSON where it is JSON. NEGATIVE CONTROLS: the provider is
    /// never one (a sign-in never runs here), a word with no `=` is refused,
    /// and Gemini CLI takes no `--setting`.
    #[test]
    fn an_agy_setting_is_seeded_beside_the_provider() {
        let l = parse_launch(Agent::Agy, &v(&["--setting", "hideActiveItems=true"])).unwrap();
        assert_eq!(
            l.settings,
            vec![("hideActiveItems".to_string(), J::Bool(true))]
        );
        let seeded = J::parse(&agy_settings_with(&v(&["/p"]), &l.settings)).unwrap();
        assert_eq!(seeded.get("hideActiveItems"), Some(&J::Bool(true)));
        assert_eq!(
            seeded.get("modelProvider").and_then(J::as_str),
            Some("gemini")
        );
        assert_eq!(
            setting_of("theme=dark"),
            Some(("theme".into(), J::s("dark")))
        );
        for bad in ["modelProvider=google", "noequals", "=true"] {
            assert_eq!(setting_of(bad), None, "{bad}");
            assert!(
                parse_launch(Agent::Agy, &v(&["--setting", bad])).is_err(),
                "{bad}"
            );
        }
        let kept = J::parse(&agy_settings_with(
            &[],
            &[("modelProvider".to_string(), J::s("google"))],
        ))
        .unwrap();
        assert_eq!(
            kept.get("modelProvider").and_then(J::as_str),
            Some("gemini")
        );
        assert!(parse_launch(Agent::Gemini, &v(&["--setting", "a=1"])).is_err());
    }

    #[test]
    fn options_parse_and_onboarding_is_agys_alone() {
        let l = parse_launch(
            Agent::Agy,
            &v(&["-e", "K=v 1", "--onboarding", "--", "-p", "hi"]),
        )
        .unwrap();
        assert!(l.onboarding && l.untrusted && !l.no_wait);
        assert_eq!((l.envs, l.args), (v(&["K=v 1"]), v(&["-p", "hi"])));
        assert!(parse_launch(Agent::Gemini, &v(&["--onboarding"])).is_err());
        let g = parse_launch(Agent::Gemini, &v(&["--untrusted", "--no-wait"])).unwrap();
        assert!(g.untrusted && g.no_wait && !g.onboarding);
        assert!(parse_launch(Agent::Gemini, &v(&["-e"])).is_err());
    }

    /// The seeds keep every sign-in away: Gemini CLI's auth is the API key in
    /// both variants, agy's provider is the Gemini API in both; only the trust
    /// seed moves. NEGATIVE CONTROL: the trusted variant names the project and
    /// turns the folder trust off; the untrusted one does neither.
    #[test]
    fn the_seeds_keep_the_auth_and_move_only_the_trust() {
        for untrusted in [false, true] {
            let g = J::parse(&gemini_settings(untrusted)).unwrap();
            let sec = g.get("security").unwrap();
            assert_eq!(
                sec.get("auth")
                    .and_then(|a| a.get("selectedType"))
                    .and_then(J::as_str),
                Some("gemini-api-key")
            );
            assert_eq!(
                sec.get("folderTrust").and_then(|f| f.get("enabled")),
                Some(&J::Bool(untrusted))
            );
            assert_eq!(
                g.get("privacy")
                    .and_then(|p| p.get("usageStatisticsEnabled")),
                Some(&J::Bool(false))
            );
        }
        let trusted = J::parse(&agy_settings(&v(&["/private/tmp/at.X/proj"]))).unwrap();
        assert_eq!(
            trusted.get("modelProvider").and_then(J::as_str),
            Some("gemini")
        );
        assert_eq!(
            trusted
                .get("trustedWorkspaces")
                .and_then(J::as_arr)
                .map(<[J]>::len),
            Some(1)
        );
        let open = J::parse(&agy_settings(&[])).unwrap();
        assert_eq!(
            open.get("modelProvider").and_then(J::as_str),
            Some("gemini")
        );
        assert!(open.get("trustedWorkspaces").is_none());
        assert_eq!(
            J::parse(&agy_onboarded())
                .unwrap()
                .get("onboardingComplete"),
            Some(&J::Bool(true))
        );
    }

    #[test]
    fn the_launchers_exec_the_agent_by_path_with_the_fake_key_and_the_stub() {
        let run = Run {
            tag: "t".into(),
            run: "/s/runs/t".into(),
            w: "/s/runs/t/scratch".into(),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            gemini_bin: "/g h/gemini.js".into(),
            agy_bin: "/a b/agy".into(),
            ..Run::default()
        };
        let g = launcher(&run, Agent::Gemini, &v(&["K=v 1"]), &v(&["-m", "x y"]));
        assert!(
            g.starts_with("#!/bin/sh\nclear\nexec env -u GOOGLE_API_KEY"),
            "{g}"
        );
        assert!(g.contains("  -u HTTP_PROXY -u HTTPS_PROXY"), "{g}");
        assert!(
            g.contains("HOME=/s/runs/t/scratch/gemini-home TMPDIR=/s/runs/t/scratch/tmp"),
            "{g}"
        );
        assert!(
            g.contains(
                "GEMINI_API_KEY=lab-gemini-not-a-key GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:5 \\\n"
            ),
            "{g}"
        );
        assert!(
            g.contains("  NO_BROWSER=true BROWSER=/s/runs/t/shim/open "),
            "{g}"
        );
        assert!(
            !g.contains("HTTPS_PROXY=http"),
            "no proxy for Gemini CLI: {g}"
        );
        assert!(
            g.ends_with("  'K=v 1' \\\n  '/g h/gemini.js' -m 'x y'\n"),
            "{g}"
        );
        let a = launcher(&run, Agent::Agy, &[], &[]);
        assert!(a.contains("HOME=/s/runs/t/scratch/agy-home"), "{a}");
        assert!(
            a.contains("HTTPS_PROXY=http://127.0.0.1:6")
                && a.contains("NO_PROXY=127.0.0.1,localhost"),
            "{a}"
        );
        assert!(a.contains("AGY_CLI_DISABLE_AUTO_UPDATE=true"), "{a}");
        assert!(a.ends_with("  '/a b/agy'\n"), "{a}");
    }
}
