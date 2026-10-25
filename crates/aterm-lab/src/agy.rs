// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Isolated lab harness for launching the real Antigravity CLI (`agy`) against the loopback Gemini API stub.

use std::path::Path;

use crate::door::Door;
use crate::pyjson::{J, Style};
use crate::run::{Run, take_tag};
use crate::sys::{PutSrc, put, scratch_remove};
use crate::util::shell_quote;
use crate::{Fail, LAB, Res};

/// Fake API key agy sends as `x-goog-api-key`.
pub use crate::stub::gemini::LAB_GEMINI_KEY;

/// The CLI verb and fixture prefix.
pub const NAME: &str = "agy";

/// Scratch `HOME` subdirectory name under the lab root.
pub const HOME: &str = "agy-home";

/// Screen regex matching agy's initial TUI frame.
pub const DRAWN: &str = "Antigravity CLI|trust the contents|color scheme|Terms of Service";

/// Verify that `run` has an agy configured.
///
/// # Errors
/// Returns a [`Fail`] when no binary was found at `up`.
pub fn need(run: &Run) -> Result<(), Fail> {
    let bin = &run.agy_bin;
    if bin.is_empty() || !crate::sys::executable(bin) {
        return Err(Fail::die(format!(
            "no {NAME} in lab '{}' (aterm pkg install agy, or LAB_AGY=/path/to/agy at up)",
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

/// Reject CLI arguments that would initiate a sign-in.
///
/// # Errors
/// Returns a refusal message naming the forbidden argument.
pub fn safe_args(args: &[String]) -> Result<(), String> {
    match args
        .iter()
        .find(|a| matches!(a.as_str(), "login" | "logout" | "auth"))
    {
        Some(a) => Err(format!(
            "{NAME}: '{a}' is refused in the lab (a Google sign-in never runs here)"
        )),
        None => Ok(()),
    }
}

/// Parsed launch options for `agy`.
#[derive(Debug, Default)]
pub struct Launch {
    /// `-e K=V`.
    pub envs: Vec<String>,
    /// `--no-wait`.
    pub no_wait: bool,
    /// `--untrusted`: no trust seed, so the folder-trust dialog draws.
    pub untrusted: bool,
    /// `--onboarding`: no onboarding seed either, so the first-run screens draw.
    pub onboarding: bool,
    /// `--setting KEY=VALUE`: a key of its `settings.json` set beside the seed's
    /// ([`agy_settings_with`]).
    pub settings: Vec<(String, J)>,
    /// `-- <args>`.
    pub args: Vec<String>,
}

/// Parse CLI launch flags.
///
/// # Errors
/// Returns an error on unknown options or missing flag values.
pub fn parse_launch(args: &[String]) -> Result<Launch, String> {
    let mut l = Launch::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-e" => l.envs.push(
                it.next()
                    .cloned()
                    .ok_or_else(|| format!("{NAME}: -e needs K=V"))?,
            ),
            "--no-wait" => l.no_wait = true,
            "--untrusted" => l.untrusted = true,
            "--onboarding" => {
                l.onboarding = true;
                l.untrusted = true;
            }
            "--setting" => l
                .settings
                .push(it.next().and_then(|kv| setting_of(kv)).ok_or_else(|| {
                    format!("{NAME}: --setting needs KEY=VALUE (never the provider)")
                })?),
            "--" => l.args.extend(it.by_ref().cloned()),
            other => {
                return Err(format!(
                    "{NAME}: unknown option '{other}' (use -e K=V, --no-wait, --untrusted, --onboarding, --setting KEY=VALUE, -- <{NAME} args>)"
                ));
            }
        }
    }
    Ok(l)
}

/// Build the shell command line that `exec`s agy in its isolated scratch environment.
#[must_use]
pub fn launcher(run: &Run, envs: &[String], args: &[String]) -> String {
    let w = run.w.to_string_lossy();
    let shim = run.run.join("shim");
    let px = &run.proxy;
    let mut s = String::from("#!/bin/sh\nclear\n");
    s.push_str(
        "exec env -u GOOGLE_API_KEY -u GOOGLE_GENAI_USE_VERTEXAI -u GOOGLE_CLOUD_PROJECT -u GOOGLE_APPLICATION_CREDENTIALS \\\n",
    );
    s.push_str(&format!(
        "  PATH=\"{}:$PATH\" HOME={} TMPDIR={} \\\n",
        shim.display(),
        shell_quote(&format!("{w}/{HOME}")),
        shell_quote(&format!("{w}/tmp"))
    ));
    s.push_str(&format!(
        "  HTTP_PROXY={px} HTTPS_PROXY={px} http_proxy={px} https_proxy={px} ALL_PROXY={px} all_proxy={px} \\\n"
    ));
    s.push_str("  NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost \\\n");
    s.push_str(&format!(
        "  GEMINI_API_KEY={LAB_GEMINI_KEY} GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:{} \\\n",
        run.port
    ));
    s.push_str(&format!(
        "  AGY_CLI_DISABLE_AUTO_UPDATE=true BROWSER={} LAB_OPEN_LOG={} LAB_SECURITY_LOG={} \\\n",
        shell_quote(&shim.join("open").to_string_lossy()),
        shell_quote(&format!("{w}/open.log")),
        shell_quote(&format!("{w}/security.log"))
    ));
    for kv in envs {
        s.push_str(&format!("  {} \\\n", shell_quote(kv)));
    }
    s.push_str(&format!("  {}", shell_quote(&run.agy_bin)));
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

/// Write or remove agy's scratch-home config files prior to launch.
///
/// # Errors
/// Returns a [`Fail`] on symlink or I/O errors.
pub fn seed(run: &Run, l: &Launch) -> Result<(), Fail> {
    let dir = run.w.join(HOME).join(".gemini/antigravity-cli");
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
    Ok(())
}

/// Seed the scratch home and launch agy in the lab's first tab.
///
/// # Errors
/// No agy, no witness, a refused argument, or the launch line could not be typed.
pub fn launch(run: &Run, l: &Launch) -> Result<String, Fail> {
    need(run)?;
    crate::procs::need_witness(run)?;
    safe_args(&l.args)?;
    let mut run = run.clone();
    run.agent = NAME.into();
    run.save();
    seed(&run, l)?;
    let script = run.run.join(format!("{NAME}.sh"));
    put_text(&script, 0o755, &launcher(&run, &l.envs, &l.args))?;
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
        run.agy_bin,
        run.agy_ver,
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

/// `agy [tag] [-e K=V]... [--no-wait] [--untrusted] [--onboarding] [--setting KEY=VALUE]... [-- args]`.
pub fn cmd_agy(args: &[String]) -> Res {
    let mut a = args.to_vec();
    let tag = take_tag(&mut a)?;
    let run = Run::load(&tag)?;
    let l = parse_launch(&a)?;
    println!("{}", launch(&run, &l)?);
    let run = Run::load(&tag)?;
    if !l.no_wait && Door::new(&run)?.await_match(DRAWN, 60).unwrap_or(false) {
        println!("the TUI is drawing ({LAB} text {tag})");
    }
    Ok(0)
}

#[cfg(test)]
#[path = "agy_launch_tests.rs"]
mod launch_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_sign_in_never_runs_in_the_lab() {
        for bad in ["login", "logout", "auth"] {
            assert!(safe_args(&v(&["-p", "x", bad])).is_err(), "{bad}");
        }
        assert!(safe_args(&v(&["-p", "hi"])).is_ok());
        assert!(safe_args(&v(&["-s"])).is_ok());
    }

    /// `--setting KEY=VALUE` sets one of agy's own keys beside the seed's, the
    /// value read as JSON where it is JSON.
    #[test]
    fn an_agy_setting_is_seeded_beside_the_provider() {
        let l = parse_launch(&v(&["--setting", "hideActiveItems=true"])).unwrap();
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
            assert!(parse_launch(&v(&["--setting", bad])).is_err(), "{bad}");
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
    }

    #[test]
    fn options_parse() {
        let l = parse_launch(&v(&["-e", "K=v 1", "--onboarding", "--", "-p", "hi"])).unwrap();
        assert!(l.onboarding && l.untrusted && !l.no_wait);
        assert_eq!((l.envs, l.args), (v(&["K=v 1"]), v(&["-p", "hi"])));
        let g = parse_launch(&v(&["--untrusted", "--no-wait"])).unwrap();
        assert!(g.untrusted && g.no_wait && !g.onboarding);
        assert!(parse_launch(&v(&["-e"])).is_err());
        assert!(
            parse_launch(&v(&["--sandbox"])).is_err(),
            "no option of agy's"
        );
    }

    /// The seeds keep every sign-in away: agy's provider is the Gemini API
    /// whether trusted or not; only the trust seed moves.
    #[test]
    fn the_seeds_keep_the_auth_and_move_only_the_trust() {
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
    fn the_launcher_execs_agy_by_path_with_the_fake_key_and_the_stub() {
        let run = Run {
            tag: "t".into(),
            run: "/s/runs/t".into(),
            w: "/s/runs/t/scratch".into(),
            port: "5".into(),
            proxy: "http://127.0.0.1:6".into(),
            agy_bin: "/a b/agy".into(),
            ..Run::default()
        };
        let a = launcher(&run, &v(&["K=v 1"]), &v(&["-m", "x y"]));
        assert!(
            a.starts_with("#!/bin/sh\nclear\nexec env -u GOOGLE_API_KEY"),
            "{a}"
        );
        assert!(
            a.contains(
                "GEMINI_API_KEY=lab-gemini-not-a-key GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:5 \\\n"
            ),
            "{a}"
        );
        assert!(a.contains("HOME=/s/runs/t/scratch/agy-home"), "{a}");
        assert!(
            a.contains("HTTPS_PROXY=http://127.0.0.1:6")
                && a.contains("NO_PROXY=127.0.0.1,localhost"),
            "{a}"
        );
        assert!(a.contains("AGY_CLI_DISABLE_AUTO_UPDATE=true"), "{a}");
        assert!(a.ends_with("  'K=v 1' \\\n  '/a b/agy' -m 'x y'\n"), "{a}");
    }
}
