// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated lab harness for launching real Gemini CLI and Antigravity CLI (`agy`) against the loopback Gemini stub.

use std::path::Path;

use crate::door::Door;
use crate::pyjson::{J, Style};
use crate::run::{Run, take_tag};
use crate::sys::{PutSrc, put, scratch_remove};
use crate::util::shell_quote;
use crate::{Fail, LAB, Res};

/// Fake API key sent by both CLIs as `x-goog-api-key`.
pub use crate::stub::gemini::LAB_GEMINI_KEY;

/// Gemini-family CLI agent under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    /// Gemini CLI.
    Gemini,
    /// Antigravity CLI (`agy`).
    Agy,
}

impl Agent {
    /// Short CLI verb and fixture prefix (`"gemini"` or `"agy"`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Agent::Gemini => "gemini",
            Agent::Agy => "agy",
        }
    }

    /// Executable path recorded by `up` (empty if absent).
    #[must_use]
    pub fn bin(self, run: &Run) -> &str {
        match self {
            Agent::Gemini => &run.gemini_bin,
            Agent::Agy => &run.agy_bin,
        }
    }

    /// Version string recorded by `up`.
    #[must_use]
    pub fn ver(self, run: &Run) -> &str {
        match self {
            Agent::Gemini => &run.gemini_ver,
            Agent::Agy => &run.agy_ver,
        }
    }

    /// Scratch `HOME` subdirectory name under the lab root.
    #[must_use]
    pub fn home(self) -> &'static str {
        match self {
            Agent::Gemini => "gemini-home",
            Agent::Agy => "agy-home",
        }
    }

    /// Screen regex matching the agent's initial TUI frame.
    #[must_use]
    pub fn drawn(self) -> &'static str {
        match self {
            Agent::Gemini => "Type your message|trust the files|Gemini CLI|gemini-api-key",
            Agent::Agy => "Antigravity CLI|trust the contents|color scheme|Terms of Service",
        }
    }
}

/// Verify that `run` has a binary configured for `agent`.
///
/// # Errors
/// Returns a [`Fail`] when no binary was found at `up`.
pub fn need(run: &Run, agent: Agent) -> Result<(), Fail> {
    let bin = agent.bin(run);
    if bin.is_empty() || !crate::sys::executable(bin) {
        let how = match agent {
            Agent::Gemini => "LAB_GEMINI=/path/to/gemini at up names one; aterm installs none",
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

/// Build `.gemini/settings.json` with API-key auth and telemetry disabled.
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

/// Build `.gemini/antigravity-cli/settings.json` trusting `trusted` directories.
#[must_use]
pub fn agy_settings(trusted: &[String]) -> String {
    agy_settings_with(trusted, &[])
}

/// Build `agy` `settings.json` with `extra` key-value overrides.
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

/// Parse a `KEY=VALUE` override, interpreting `VALUE` as JSON when valid.
#[must_use]
pub fn setting_of(kv: &str) -> Option<(String, J)> {
    let (key, value) = kv.split_once('=')?;
    if key.is_empty() || key == "modelProvider" {
        return None;
    }
    let value = J::parse(value).unwrap_or_else(|_| J::s(value));
    Some((key.to_string(), value))
}

/// Build `.gemini/antigravity-cli/cache/onboarding.json` marking onboarding complete.
#[must_use]
pub fn agy_onboarded() -> String {
    J::obj([
        ("consumerOnboardingComplete", J::Bool(true)),
        ("enterpriseOnboardingComplete", J::Bool(false)),
        ("onboardingComplete", J::Bool(true)),
    ])
    .dump(PRETTY)
}

/// Reject CLI arguments that would initiate login or enable an inner sandbox.
///
/// # Errors
/// Returns a refusal message naming the forbidden flag.
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

/// Parsed launch options for `gemini` or `agy`.
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

/// Parse CLI launch flags for `agent`.
///
/// # Errors
/// Returns an error on unknown options or missing flag values.
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

/// Build the shell command line that `exec`s `agent` in its isolated scratch environment.
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

/// Write or remove scratch-home config files for `agent` prior to launch.
///
/// # Errors
/// Returns a [`Fail`] on symlink or I/O errors.
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

/// Seed the scratch home and launch `agent` in the lab's first tab.
///
/// # Errors
/// Returns a [`Fail`] if the binary or witness is missing, arguments are refused, or the TUI fails to draw.
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

/// Run the `gemini` lab subcommand.
pub fn cmd_gemini(args: &[String]) -> Res {
    cmd(Agent::Gemini, args)
}

/// Run the `agy` lab subcommand.
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
    /// value read as JSON where it is JSON.
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
    /// seed moves.
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
