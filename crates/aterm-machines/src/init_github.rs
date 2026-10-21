// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional local GitHub bootstrap. Configuration contains source locations only;
//! credentials travel through private stdin and never enter a report or command line.

use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Cmd, Secret, System};
use serde::Deserialize;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

mod repos;

const INPUT_MAX: usize = 64 * 1024;
const GIT: &str = crate::items::github::CLT_GIT;
const HOST: &str = "github.com";

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    github: Option<Github>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Github {
    pub account: String,
    pub owner: String,
    pub token_file: PathBuf,
    #[serde(default)]
    pub token_key: Option<String>,
}

/// Add GitHub authentication and repository reconciliation only when explicitly configured.
///
/// # Errors
/// The local profile cannot be admitted or decoded. Its contents are never quoted.
pub fn items(sys: &dyn System, ctx: &Ctx) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init GitHub requires exactly one local user".into());
    };
    let path = Path::new(&user.home).join(".config/aterm/init.toml");
    let bytes = match sys.read_owned_file(&path, user.uid, INPUT_MAX, false) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("cannot safely read ~/.config/aterm/init.toml (requires an owned regular file, no links or group/other writes, at most 64 KiB)".into()),
    };
    let mut profile = decode(&bytes, user)?;
    let Some(config) = profile.github.take() else {
        return Ok(Vec::new());
    };
    Ok(vec![
        Box::new(Auth {
            user: user.clone(),
            config: config.clone(),
        }),
        repos::item(user, &config),
    ])
}

fn decode(bytes: &[u8], user: &User) -> Result<Config, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "init.toml must be UTF-8; no configuration contents were printed")?;
    let mut profile: Config = aterm_toml::from_str(text)
        .map_err(|_| "invalid init.toml; expected [github] account, owner, token_file and optional token_key (credential values do not belong in this file)")?;
    if let Some(config) = &mut profile.github {
        if !login(&config.account) || !login(&config.owner) {
            return Err("GitHub account and owner must be valid login names".into());
        }
        if config.token_key.as_deref().is_some_and(|k| !env_key(k)) {
            return Err("GitHub token_key must be an environment assignment name".into());
        }
        let path = config
            .token_file
            .to_str()
            .ok_or("GitHub token_file must be UTF-8")?;
        config.token_file = if let Some(relative) = path.strip_prefix("~/") {
            Path::new(&user.home).join(relative)
        } else {
            config.token_file.clone()
        };
        if !config.token_file.is_absolute()
            || config
                .token_file
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(
                "GitHub token_file must be an absolute or ~/ path without dot components".into(),
            );
        }
        // This file is never an input to init, even through an accidentally broad profile.
        if config
            .token_file
            .file_name()
            .is_some_and(|n| n == "release-credentials.toml")
        {
            return Err("GitHub token_file cannot name aterm release credentials".into());
        }
    }
    Ok(profile)
}

fn login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn env_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

pub(super) fn gh(sys: &dyn System) -> Result<PathBuf, String> {
    crate::controller::github::GH_PATHS
        .iter()
        .map(Path::new)
        .find(|p| sys.stat(p).is_ok())
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "GitHub bootstrap needs gh ({} or {}), which aterm does not install",
                crate::controller::github::GH_PATHS[0],
                crate::controller::github::GH_PATHS[1]
            )
        })
}

pub(super) fn auth_id(user: &User) -> String {
    format!("init.github.auth@{}", user.name)
}

fn gh_cmd(path: &Path, user: &User) -> Cmd {
    Cmd::new(path)
        .as_user(user)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_HOST", HOST)
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
}

fn active(sys: &dyn System, user: &User, path: &Path, account: &str) -> Result<bool, String> {
    let cmd = gh_cmd(path, user).args(["api", "--hostname", HOST, "user", "--jq", ".login"]);
    let ran = sys.run_complete_stdout(&cmd)
        .map_err(|_| "cannot completely verify the active gh account (network, timeout or output capture failure)")?;
    if !ran.success() {
        return Ok(false);
    }
    let value = std::str::from_utf8(&ran.stdout)
        .map_err(|_| "gh returned an invalid account record")?
        .trim();
    if !login(value) {
        return Err("gh returned an invalid account record".into());
    }
    Ok(value.eq_ignore_ascii_case(account))
}

fn helper(sys: &dyn System, user: &User, path: &Path) -> Result<bool, String> {
    if sys.stat(Path::new(GIT)).is_err() {
        return Err(
            "GitHub bootstrap needs Apple's Command Line Tools (xcode-select --install)".into(),
        );
    }
    let cmd = Cmd::new(GIT).as_user(user).args([
        "config",
        "--global",
        "--get-all",
        "credential.https://github.com.helper",
    ]);
    let ran = sys
        .run_complete_stdout(&cmd)
        .map_err(|_| "cannot completely inspect GitHub's git credential helper")?;
    if ran.status == Some(1) && ran.stdout.is_empty() {
        return Ok(false);
    }
    if !ran.success() {
        return Err("cannot inspect GitHub's git credential helper".into());
    }
    let text = std::str::from_utf8(&ran.stdout)
        .map_err(|_| "invalid GitHub git credential helper record")?;
    let lines: Vec<_> = text.lines().collect();
    let expected = format!("!{} auth git-credential", path.display());
    let quoted = format!("!\"{}\" auth git-credential", path.display());
    // The empty entry resets inherited helpers. A later helper would change the
    // credential selection, so only the generated terminal pair is accepted.
    Ok(lines.len() >= 2
        && lines[lines.len() - 2].is_empty()
        && matches!(lines.last(), Some(value) if *value == expected || *value == quoted))
}

pub(super) fn auth_ready(sys: &dyn System, user: &User, config: &Github) -> Result<bool, String> {
    let (account, helper) = readiness(sys, user, config)?;
    Ok(account && helper)
}

fn readiness(sys: &dyn System, user: &User, config: &Github) -> Result<(bool, bool), String> {
    let path = gh(sys)?;
    let helper = helper(sys, user, &path)?;
    Ok((active(sys, user, &path, &config.account)?, helper))
}

struct Auth {
    user: User,
    config: Github,
}

impl Item for Auth {
    fn id(&self) -> String {
        auth_id(&self.user)
    }
    fn family(&self) -> &'static str {
        "github"
    }
    fn title(&self) -> String {
        format!(
            "GitHub authentication as {} and HTTPS git helper",
            self.config.account
        )
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match readiness(sys, &self.user, &self.config) {
            Ok((true, true)) => Probe::Ok { now: self.title() },
            Ok((true, false)) => Probe::Change {
                from: "GitHub HTTPS helper needs setup".into(),
                to: self.title(),
            },
            Ok((false, _)) => match read_token(sys, &self.user, &self.config) {
                Ok(_) => Probe::Change {
                    from: "GitHub account or HTTPS helper needs setup".into(),
                    to: self.title(),
                },
                Err(why) => Probe::Blocked { why },
            },
            Err(why) => Probe::Unusable { why },
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        let path = gh(sys)?;
        if sys.euid() != self.user.uid {
            return Err("GitHub bootstrap must run as its local user".into());
        }
        // Check tools before changing authentication. No inherited token/environment
        // can override this login: System runs with a cleared fixed environment.
        helper(sys, &self.user, &path)?;
        if !active(sys, &self.user, &path, &self.config.account)? {
            crate::port::no_core_dumps();
            let token = read_token(sys, &self.user, &self.config)?;
            preflight(sys, &self.user, &self.config, &token)?;
            let cmd = gh_cmd(&path, &self.user).args([
                "auth",
                "login",
                "--hostname",
                HOST,
                "--git-protocol",
                "https",
                "--with-token",
            ]);
            let ran = sys.run_complete_stdout_input(&cmd, token.bytes()).map_err(
                |_| "gh authentication failed (private input or complete output was not verified)",
            )?;
            if !ran.success() {
                return Err("gh authentication failed; check token scopes and system credential storage (output withheld)".into());
            }
        }
        if !active(sys, &self.user, &path, &self.config.account)? {
            return Err("gh did not verify the configured GitHub account after login".into());
        }
        if !helper(sys, &self.user, &path)? {
            let cmd = gh_cmd(&path, &self.user).args(["auth", "setup-git", "--hostname", HOST]);
            let ran = sys
                .run_complete_stdout(&cmd)
                .map_err(|_| "gh could not configure the GitHub HTTPS helper (output withheld)")?;
            if !ran.success() {
                return Err(
                    "gh could not configure the GitHub HTTPS helper (output withheld)".into(),
                );
            }
        }
        if !auth_ready(sys, &self.user, &self.config)? {
            return Err("GitHub authentication/helper read-back failed".into());
        }
        Ok(())
    }
}

fn read_token(sys: &dyn System, user: &User, config: &Github) -> Result<Secret, String> {
    crate::port::no_core_dumps();
    let mut bytes = sys.read_owned_file(&config.token_file, user.uid, INPUT_MAX, true)
        .map_err(|_| "GitHub token source must be an owned private regular file (0600), without links, at most 64 KiB")?;
    let result = token(&bytes, config.token_key.as_deref());
    scrub(&mut bytes);
    result
}

fn token(bytes: &[u8], key: Option<&str>) -> Result<Secret, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "GitHub token source must be UTF-8")?;
    let value = if let Some(key) = key {
        let mut found = None;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            if name.trim() != key {
                continue;
            }
            if found.is_some() {
                return Err(
                    "GitHub token_key appears more than once; no file content was executed".into(),
                );
            }
            let value = value.trim();
            found = Some(
                if let Some(value) = value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
                    value
                } else if let Some(value) =
                    value.strip_prefix('"').and_then(|v| v.strip_suffix('"'))
                {
                    value
                } else {
                    value
                },
            );
        }
        found.ok_or("GitHub token_key was not found; no file content was executed")?
    } else {
        text.trim()
    };
    // GitHub token characters need no shell expansion or curl-config escaping.
    // Quoted env assignments are literal: substitutions/backslashes never execute.
    if value.is_empty()
        || value.len() > crate::port::SECRET_MAX
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err("GitHub token must be one nonempty literal token; shell expansion and multiline values are refused".into());
    }
    Secret::new(value.as_bytes()).map_err(|_| "GitHub token is too long".into())
}

fn preflight(sys: &dyn System, user: &User, config: &Github, token: &Secret) -> Result<(), String> {
    // curl accepts this header from a private config pipe, so identity is verified
    // before gh persists/switches any account. Neither argv nor environment has it.
    let prefix = b"header = \"Authorization: Bearer ";
    let mut input = Vec::with_capacity(prefix.len() + token.bytes().len() + 2);
    input.extend_from_slice(prefix);
    input.extend_from_slice(token.bytes());
    input.extend_from_slice(b"\"\n");
    let cmd = Cmd::new("/usr/bin/curl")
        .as_user(user)
        .args([
            "--disable",
            "--config",
            "-",
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--connect-timeout",
            "15",
            "--max-time",
            "55",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "User-Agent: aterm-init",
            "--url",
            "https://api.github.com/user",
        ])
        .budget(Duration::from_secs(60));
    let result = sys.run_complete_stdout_input(&cmd, &input);
    scrub(&mut input);
    let ran = result
        .map_err(|_| "cannot verify the supplied GitHub token before login (output withheld)")?;
    if !ran.success() {
        return Err("GitHub rejected the supplied token before login (output withheld)".into());
    }
    #[derive(Deserialize)]
    struct Identity {
        login: String,
    }
    let identity: Identity = aterm_json::from_slice(&ran.stdout)
        .map_err(|_| "invalid GitHub identity response; gh login was not changed")?;
    if !identity.login.eq_ignore_ascii_case(&config.account) {
        return Err(
            "the supplied token belongs to a different GitHub account; gh login was not changed"
                .into(),
        );
    }
    Ok(())
}

fn scrub(bytes: &mut [u8]) {
    for byte in bytes {
        // SAFETY: the mutable slice owns each live byte. Volatile writes prevent
        // removal of the clearing when this temporary input buffer is dropped.
        unsafe { std::ptr::write_volatile(byte, 0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{self, NoJournal};
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, Ran, Stat};

    const GH: &str = "/opt/homebrew/bin/gh";
    const SOURCE: &str = "/Users//alice/.secrets/github";
    const VALUE: &str = "fixture_secret_injected_only";
    const ACTIVE: &str = "alice$ GH_PROMPT_DISABLED=1 GH_HOST=github.com GH_NO_UPDATE_NOTIFIER=1 GH_NO_EXTENSION_UPDATE_NOTIFIER=1 /opt/homebrew/bin/gh api --hostname github.com user --jq .login";
    const HELPER: &str = "alice$ /Library/Developer/CommandLineTools/usr/bin/git config --global --get-all credential.https://github.com.helper";
    const LOGIN: &str = "alice$ GH_PROMPT_DISABLED=1 GH_HOST=github.com GH_NO_UPDATE_NOTIFIER=1 GH_NO_EXTENSION_UPDATE_NOTIFIER=1 /opt/homebrew/bin/gh auth login --hostname github.com --git-protocol https --with-token";
    const SETUP: &str = "alice$ GH_PROMPT_DISABLED=1 GH_HOST=github.com GH_NO_UPDATE_NOTIFIER=1 GH_NO_EXTENSION_UPDATE_NOTIFIER=1 /opt/homebrew/bin/gh auth setup-git --hostname github.com";
    const CURL: &str = "alice$ /usr/bin/curl --disable --config - --silent --show-error --fail --proto =https --connect-timeout 15 --max-time 55 --header 'Accept: application/vnd.github+json' --header 'User-Agent: aterm-init' --url https://api.github.com/user";

    fn stat(mode: u32, len: usize) -> Stat {
        Stat {
            uid: 501,
            gid: 20,
            mode,
            dir: false,
            symlink: false,
            len: len as u64,
        }
    }
    fn fixture() -> (Fake, Auth, Ctx) {
        let mut sys = Fake::root();
        sys.euid = 501;
        sys.stat_of(GH, stat(0o755, 0)).stat_of(GIT, stat(0o755, 0));
        sys.file(SOURCE, VALUE.as_bytes())
            .stat_of(SOURCE, stat(0o600, VALUE.len()));
        let u = user("alice", 501);
        let config = Github {
            account: "octocat".into(),
            owner: "octocat".into(),
            token_file: SOURCE.into(),
            token_key: None,
        };
        (
            sys,
            Auth {
                user: u.clone(),
                config,
            },
            ctx(&[u]),
        )
    }
    fn good_helper() -> Ran {
        Ran::ok("\n!\"/opt/homebrew/bin/gh\" auth git-credential\n")
    }

    #[test]
    fn config_is_opt_in_and_never_echoes_invalid_or_inline_credentials() {
        let (sys, _auth, context) = fixture();
        assert!(items(&sys, &context).unwrap().is_empty());
        let u = &context.users[0];
        let parsed = decode(
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n",
            u,
        )
        .unwrap();
        assert_eq!(parsed.github.unwrap().token_file, Path::new(SOURCE));
        for text in [
            format!("[github]\naccount='octocat'\nowner='octocat'\ntoken='{VALUE}'\n"),
            format!("[github]\naccount='octocat'\nowner='octocat'\ntoken_file='{VALUE}\n"),
            "[github]\naccount='-bad'\nowner='octocat'\ntoken_file='~/.secrets/github'".into(),
            "[github]\naccount='octocat'\nowner='octocat'\ntoken_file='../source'".into(),
            "[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.aterm/release-credentials.toml'".into(),
        ] {
            let error = decode(text.as_bytes(), u).err().expect("invalid profile refused");
            assert!(!error.contains(VALUE), "configuration values must not appear in errors");
        }
    }

    #[test]
    fn tokens_are_raw_or_explicit_literal_env_assignments_without_evaluation() {
        assert_eq!(
            token(format!(" {VALUE}\n").as_bytes(), None)
                .unwrap()
                .bytes(),
            VALUE.as_bytes()
        );
        assert_eq!(
            token(
                format!("# note\nOTHER=unused\nexport GH_TOKEN = '{VALUE}'\n").as_bytes(),
                Some("GH_TOKEN")
            )
            .unwrap()
            .bytes(),
            VALUE.as_bytes()
        );
        for text in [
            "GH_TOKEN=$(touch /tmp/should-never-exist)",
            "GH_TOKEN=\"${OTHER}\"",
            "GH_TOKEN='first'\nGH_TOKEN='second'",
            "OTHER=token",
            "GH_TOKEN='line\nbreak'",
            "GH_TOKEN=token # comment",
        ] {
            assert!(token(text.as_bytes(), Some("GH_TOKEN")).is_err());
        }
        assert!(token(b"", None).is_err());
        assert!(token(b"first\nsecond", None).is_err());
    }

    #[test]
    fn plan_and_check_only_probe_and_never_send_or_persist_the_token() {
        let (sys, auth, context) = fixture();
        sys.on(HELPER, Ran::exit(1, ""))
            .on(ACTIVE, Ran::exit(1, "not logged in"));
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Plan,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Planned);
        assert_eq!(sys.calls(), vec![HELPER, ACTIVE]);
        assert!(!aterm_json::to_string(&rows).unwrap().contains(VALUE));
        assert!(sys.fed().is_empty());
    }

    #[test]
    fn configured_plan_waits_for_auth_before_inventory_and_failed_auth_blocks_clones() {
        for mode in [Mode::Plan, Mode::Apply] {
            let (sys, _auth, context) = fixture();
            let profile =
                b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
            let path = "/Users//alice/.config/aterm/init.toml";
            sys.file(path, profile)
                .stat_of(path, stat(0o644, profile.len()));
            sys.on(HELPER, Ran::exit(1, ""))
                .on(ACTIVE, Ran::exit(1, "not logged in"));
            sys.on(CURL, Ran::exit(22, VALUE));
            let rows = engine::run(
                &items(&sys, &context).unwrap(),
                mode,
                &sys,
                &context,
                &mut NoJournal,
            );
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows[0].outcome,
                if mode == Mode::Plan {
                    Outcome::Planned
                } else {
                    Outcome::Failed
                }
            );
            assert_eq!(
                rows[1].outcome,
                if mode == Mode::Plan {
                    Outcome::Planned
                } else {
                    Outcome::Blocked
                }
            );
            assert!(!aterm_json::to_string(&rows).unwrap().contains(VALUE));
            assert!(!sys.calls().iter().any(|c| c.contains("/repos")
                || c.contains(" clone ")
                || c.contains("setup-git")
                || c.contains("auth login")));
        }
    }

    #[test]
    fn ready_login_does_not_need_or_read_the_token_source() {
        let (sys, mut auth, context) = fixture();
        auth.config.token_file = "/not-present".into();
        sys.on(HELPER, good_helper())
            .on(ACTIVE, Ran::ok("octocat\n"));
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Apply,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok);
        assert_eq!(sys.calls(), vec![HELPER, ACTIVE]);
    }

    #[test]
    fn existing_correct_login_can_repair_helper_without_source_credentials() {
        let (sys, mut auth, context) = fixture();
        auth.config.token_file = "/not-present".into();
        sys.on(ACTIVE, Ran::ok("octocat\n"));
        sys.on(HELPER, Ran::exit(1, ""))
            .on(HELPER, Ran::exit(1, ""))
            .on(HELPER, Ran::exit(1, ""))
            .on(HELPER, good_helper());
        sys.on(SETUP, Ran::ok(""));
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Apply,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        assert!(sys.calls().contains(&SETUP.into()));
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("auth login") || c.contains("curl"))
        );
    }

    #[test]
    fn token_identity_is_checked_before_any_login_and_all_failure_output_is_withheld() {
        for response in [
            Ran::ok("{\"login\":\"someone-else\"}"),
            Ran::exit(22, VALUE),
            Ran::ok(VALUE),
        ] {
            let (sys, auth, context) = fixture();
            sys.on(HELPER, Ran::exit(1, ""))
                .on(ACTIVE, Ran::exit(1, "not logged in"));
            sys.on(CURL, response);
            let error = auth.apply(&sys, &context).unwrap_err();
            assert!(!error.contains(VALUE));
            assert!(
                !sys.calls()
                    .iter()
                    .any(|c| c.contains("auth login") || c.contains("setup-git"))
            );
            assert!(sys.calls().iter().all(|c| !c.contains(VALUE)));
        }
    }

    #[test]
    fn successful_apply_uses_private_input_then_verifies_login_and_helper() {
        let (sys, auth, context) = fixture();
        sys.on(HELPER, Ran::exit(1, ""))
            .on(HELPER, Ran::exit(1, ""))
            .on(HELPER, good_helper());
        sys.on(ACTIVE, Ran::exit(1, "not logged in"))
            .on(ACTIVE, Ran::ok("octocat\n"));
        sys.on(CURL, Ran::ok("{\"login\":\"octocat\"}"));
        sys.on(LOGIN, Ran::ok(""));
        sys.on(SETUP, Ran::ok(""));
        auth.apply(&sys, &context).unwrap();
        let calls = sys.calls();
        assert!(
            calls.iter().position(|c| c == CURL).unwrap()
                < calls.iter().position(|c| c == LOGIN).unwrap()
        );
        assert!(calls.iter().all(|c| !c.contains(VALUE)));
        assert!(
            sys.fed().is_empty(),
            "the input lane must not retain a secret copy"
        );
    }

    #[test]
    fn incomplete_identity_or_login_capture_stops_before_later_mutations() {
        for failing in [CURL, LOGIN] {
            let (sys, auth, context) = fixture();
            sys.on(HELPER, Ran::exit(1, ""))
                .on(ACTIVE, Ran::exit(1, "not logged in"));
            sys.on(CURL, Ran::ok("{\"login\":\"octocat\"}"));
            sys.on(LOGIN, Ran::ok(""));
            sys.refuse_complete_stdout(failing, VALUE);
            let error = auth.apply(&sys, &context).unwrap_err();
            assert!(!error.contains(VALUE));
            assert!(!sys.calls().iter().any(|c| c == SETUP));
            if failing == CURL {
                assert!(!sys.calls().iter().any(|c| c == LOGIN));
            }
        }
    }

    #[test]
    fn unsafe_token_file_and_unproven_helper_are_never_ready() {
        let (sys, auth, context) = fixture();
        sys.on(HELPER, Ran::exit(1, ""))
            .on(ACTIVE, Ran::exit(1, "not logged in"));
        sys.stat_of(SOURCE, stat(0o644, VALUE.len()));
        assert!(matches!(auth.probe(&sys, &context), Probe::Blocked { .. }));
        let (sys, auth, context) = fixture();
        sys.on(HELPER, good_helper())
            .on(ACTIVE, Ran::ok("octocat\n"));
        sys.refuse_complete_stdout(HELPER, "rejected partial tail");
        assert!(matches!(auth.probe(&sys, &context), Probe::Unusable { .. }));
        assert!(
            !sys.calls()
                .iter()
                .any(|c| c.contains("auth login") || c.contains("setup-git"))
        );
    }

    #[test]
    fn real_git_helper_read_distinguishes_global_profile_from_a_local_override() {
        crate::items::github::need_clt_git();
        let sys = crate::port::Live::new();
        let mut scratch = crate::port::Scratch::new(&sys, "github-helper-test").unwrap();
        let mut u = user("fixture", sys.euid());
        u.home = scratch.path().to_str().unwrap().into();
        let repo = scratch.path().join("repository");
        let command = Cmd::new(GIT)
            .as_user(&u)
            .args(["init", "--quiet", "--template=", "--"])
            .arg(&repo);
        assert!(sys.run_complete_stdout(&command).unwrap().success());
        let command = Cmd::new(GIT).as_user(&u).arg("-C").arg(&repo).args([
            "config",
            "credential.https://github.com.helper",
            "local-only",
        ]);
        assert!(sys.run_complete_stdout(&command).unwrap().success());
        let command = Cmd::new(GIT).as_user(&u).arg("-C").arg(&repo).args([
            "config",
            "--get-all",
            "credential.https://github.com.helper",
        ]);
        assert_eq!(
            sys.run_complete_stdout(&command).unwrap().out().trim(),
            "local-only"
        );
        assert!(!helper(&sys, &u, Path::new(GH)).unwrap());
        let global = b"[credential \"https://github.com\"]\n helper =\n helper = !\\\"/opt/homebrew/bin/gh\\\" auth git-credential\n";
        sys.write_file(&scratch.path().join(".gitconfig"), global, 0o600)
            .unwrap();
        assert!(helper(&sys, &u, Path::new(GH)).unwrap());
        let text = sys.run_complete_stdout(&command).unwrap().out();
        assert_eq!(text.lines().last(), Some("local-only"));
        scratch.close().unwrap();
    }
}
