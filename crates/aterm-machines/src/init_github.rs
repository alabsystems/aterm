// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Optional local GitHub bootstrap for keychain credentials, git HTTPS helper, and repositories.

use crate::git_credential::{GITHUB, Git, Lookups};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{Secret, System};
use serde::Deserialize;
use std::io;
use std::path::{Component, Path, PathBuf};

mod native;
mod repos;

/// Reconciles repositories after the selected secrets stage has synchronized.
///
/// # Errors
/// A source, identity, inventory, destination or native operation is unsafe or incomplete.
pub fn run_initial_repositories(
    sys: &dyn System,
    ctx: &Ctx,
    mode: crate::model::Mode,
) -> Result<Vec<crate::model::ItemReport>, String> {
    native::run(sys, ctx, mode)
}

const INPUT_MAX: usize = 64 * 1024;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    github: Option<Github>,
    /// The develop profile's checkouts ([`crate::init_develop`]), which the repository item
    /// leaves to the develop items.
    #[serde(default)]
    develop: Option<crate::init_develop::Table>,
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
    let Some(mut profile) = profile(sys, user)? else {
        return Ok(Vec::new());
    };
    let recorded = crate::init_develop::recorded(user, profile.develop.as_ref())?;
    let Some(config) = profile.github.take() else {
        return Ok(Vec::new());
    };
    Ok(vec![
        Box::new(Auth {
            user: user.clone(),
            config: config.clone(),
        }),
        repos::item(user, &config, recorded),
    ])
}

fn profile(sys: &dyn System, user: &User) -> Result<Option<Config>, String> {
    let path = Path::new(&user.home).join(".config/aterm/init.toml");
    let bytes = match sys.read_owned_file(&path, user.uid, INPUT_MAX, false) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot safely read ~/.config/aterm/init.toml (requires an owned regular file, no links or group/other writes, at most 64 KiB)".into()),
    };
    decode(&bytes, user).map(Some)
}

/// Returns aterm workspace checkouts in the home directory owned by the configured account.
///
/// # Errors
/// The profile cannot be admitted, or the listing or a worktree cannot be read.
pub(crate) fn aterm_checkouts(sys: &dyn System, ctx: &Ctx) -> Result<Vec<PathBuf>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init --develop requires exactly one local user".into());
    };
    let Some(config) = profile(sys, user)?.and_then(|mut profile| profile.github.take()) else {
        return Ok(Vec::new());
    };
    let Some(token) = auth_ready(sys, user, &config)? else {
        return Ok(Vec::new());
    };
    repos::aterm_checkouts(sys, user, &config, &token)
}

fn decode(bytes: &[u8], user: &User) -> Result<Config, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "init.toml must be UTF-8; no configuration contents were printed")?;
    let mut profile: Config = aterm_toml::from_str(text)
        .map_err(|_| "invalid init.toml; expected [github] account, owner, token_file and optional token_key, and [develop] checkouts (credential values do not belong in this file)")?;
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

pub(super) fn auth_id(user: &User) -> String {
    format!("init.github.auth@{}", user.name)
}

/// Performs one GitHub GET request as `user` with `token`.
pub(super) fn get(
    sys: &dyn System,
    user: &User,
    token: &Secret,
    endpoint: &str,
) -> Result<crate::github_pages::Page, String> {
    let reply = sys.github_api(user, token, endpoint)?;
    crate::github_pages::page(reply.status, reply.headers, &reply.body)
}

/// The account `token` signs in as (`GET /user`).
pub(super) fn identity(sys: &dyn System, user: &User, token: &Secret) -> Result<String, String> {
    let (value, _) = get(sys, user, token, "/user")?;
    value["login"]
        .as_str()
        .filter(|account| login(account))
        .map(str::to_owned)
        .ok_or_else(|| "GitHub returned an invalid authenticated account".into())
}

fn stored(sys: &dyn System, user: &User, account: &str) -> Result<Option<Secret>, String> {
    let Some(token) = Git::keychain().fill(sys, Some(user), GITHUB, account)? else {
        return Ok(None);
    };
    Ok(match identity(sys, user, &token) {
        Ok(signed_in) if signed_in.eq_ignore_ascii_case(account) => Some(token),
        _ => None,
    })
}

fn readiness(
    sys: &dyn System,
    user: &User,
    config: &Github,
) -> Result<(Lookups, Option<Secret>), String> {
    let git = Git::keychain();
    if !git.installed(sys) {
        return Err(
            "GitHub bootstrap needs Apple's Command Line Tools git (aterm init --only base.clt \
             installs them)"
                .into(),
        );
    }
    let helper = git.configured(sys, user, &config.account)?;
    Ok((helper, stored(sys, user, &config.account)?))
}

/// Returns the keychain token for the configured account when git's helper is also ready.
pub(super) fn auth_ready(
    sys: &dyn System,
    user: &User,
    config: &Github,
) -> Result<Option<Secret>, String> {
    let (helper, token) = readiness(sys, user, config)?;
    Ok(token.filter(|_| helper.ready()))
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
            "GitHub credential for {} in the login keychain and HTTPS git helper",
            self.config.account
        )
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        match readiness(sys, &self.user, &self.config) {
            Ok((Lookups::Ready, Some(_))) => Probe::Ok { now: self.title() },
            Ok((Lookups::Not(why), Some(_))) => Probe::Change {
                from: format!("GitHub HTTPS helper needs setup: {why}"),
                to: self.title(),
            },
            Ok((_, None)) => match read_token(sys, &self.user, &self.config) {
                Ok(_) => Probe::Change {
                    from: "GitHub credential or HTTPS helper needs setup".into(),
                    to: self.title(),
                },
                Err(why) => Probe::Blocked { why },
            },
            Err(why) => Probe::Unusable { why },
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if sys.euid() != self.user.uid {
            return Err("GitHub bootstrap must run as its local user".into());
        }
        let git = Git::keychain();
        let (helper, held) = readiness(sys, &self.user, &self.config)?;
        if held.is_none() {
            crate::port::no_core_dumps();
            let token = read_token(sys, &self.user, &self.config)?;
            let account = identity(sys, &self.user, &token).map_err(|_| {
                "GitHub did not verify the supplied token (output withheld); nothing was stored"
            })?;
            if !account.eq_ignore_ascii_case(&self.config.account) {
                return Err(
                    "the supplied token belongs to a different GitHub account; nothing was stored"
                        .into(),
                );
            }
            git.store(sys, Some(&self.user), GITHUB, &self.config.account, &token)?;
        }
        if !helper.ready() {
            git.configure(sys, &self.user, &self.config.account)?;
        }
        if let Lookups::Not(why) = git.configured(sys, &self.user, &self.config.account)? {
            return Err(format!(
                "git's GitHub HTTPS settings did not read back after setup: {why}"
            ));
        }
        if stored(sys, &self.user, &self.config.account)?.is_none() {
            return Err(format!(
                "the login keychain's GitHub credential did not read back as one GitHub signs \
                 in as {} (output withheld)",
                self.config.account
            ));
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
    use crate::git_credential::script;
    use crate::items::defaults::tests::{ctx, user};
    use crate::model::{Mode, Outcome};
    use crate::port::{Fake, GithubResponse, Ran, Stat};

    const GIT: &str = crate::items::github::CLT_GIT;
    const SOURCE: &str = "/Users//alice/.secrets/github";
    const VALUE: &str = "fixture_secret_injected_only";
    const STORED: &str = "fixture_stored_keychain_only";

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
        sys.stat_of(GIT, stat(0o755, 0));
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
    fn signs_in(sys: &Fake, u: &User, login: &str) {
        sys.on_github_api(
            u,
            "/user",
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: format!("{{\"login\":\"{login}\"}}").into_bytes(),
            }),
        );
    }
    fn fill(u: &User) -> String {
        script::credential(Some(u), "fill")
    }
    fn approve(u: &User) -> String {
        script::credential(Some(u), "approve")
    }
    fn clean(sys: &Fake) {
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .all(|c| !c.contains(VALUE) && !c.contains(STORED) && !c.contains("/gh")),
            "{calls:?}"
        );
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
        let u = auth.user.clone();
        script::unconfigured(&sys, &u);
        script::holds_nothing(&sys, Some(&u));
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Plan,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Planned);
        let calls = sys.calls();
        assert_eq!(calls.first(), Some(&script::helpers(&u)), "{calls:?}");
        assert_eq!(calls.last(), Some(&fill(&u)), "{calls:?}");
        assert!(
            calls
                .iter()
                .all(|c| !c.contains("approve") && !c.contains("--global") && !c.contains(" agh ")),
            "{calls:?}"
        );
        assert!(!aterm_json::to_string(&rows).unwrap().contains(VALUE));
        clean(&sys);
    }

    #[test]
    fn configured_plan_waits_for_auth_before_inventory_and_failed_auth_blocks_clones() {
        for mode in [Mode::Plan, Mode::Apply] {
            let (sys, auth, context) = fixture();
            let u = auth.user.clone();
            let profile =
                b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
            let path = "/Users//alice/.config/aterm/init.toml";
            sys.file(path, profile)
                .stat_of(path, stat(0o644, profile.len()));
            script::unconfigured(&sys, &u);
            script::holds_nothing(&sys, Some(&u));
            sys.on_github_api(&u, "/user", Err("refused (output withheld)".into()));
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
                || c.contains("approve")
                || c.contains("--global")));
            clean(&sys);
        }
    }

    #[test]
    fn ready_credential_does_not_need_or_read_the_token_source() {
        let (sys, mut auth, context) = fixture();
        auth.config.token_file = "/not-present".into();
        let u = auth.user.clone();
        script::configured(&sys, &u, "octocat");
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        signs_in(&sys, &u, "Octocat");
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Apply,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        assert!(sys.calls().iter().all(|c| !c.contains("approve")));
        clean(&sys);
    }

    #[test]
    fn a_stored_credential_can_repair_the_helper_without_the_token_source() {
        let (sys, mut auth, context) = fixture();
        auth.config.token_file = "/not-present".into();
        let u = auth.user.clone();
        script::unconfigured(&sys, &u);
        script::unconfigured(&sys, &u);
        script::configured(&sys, &u, "octocat");
        let writes = script::configure(&sys, &u, "octocat");
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        signs_in(&sys, &u, "octocat");
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Apply,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Changed, "{rows:?}");
        let calls = sys.calls();
        assert!(writes.iter().all(|w| calls.contains(w)), "{calls:?}");
        assert!(!calls.contains(&approve(&u)), "nothing new is stored");
        clean(&sys);
    }

    #[test]
    fn token_identity_is_checked_before_anything_is_stored_and_all_failure_output_is_withheld() {
        for answer in [
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: b"{\"login\":\"someone-else\"}".to_vec(),
            }),
            Err(format!("refused {VALUE}")),
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: VALUE.as_bytes().to_vec(),
            }),
        ] {
            let (sys, auth, context) = fixture();
            let u = auth.user.clone();
            script::unconfigured(&sys, &u);
            script::holds_nothing(&sys, Some(&u));
            sys.on_github_api(&u, "/user", answer);
            let error = auth.apply(&sys, &context).unwrap_err();
            assert!(!error.contains(VALUE), "{error}");
            assert!(
                !sys.calls()
                    .iter()
                    .any(|c| c.contains("approve") || c.contains("--global")),
                "{:?}",
                sys.calls()
            );
            clean(&sys);
        }
    }

    #[test]
    fn a_successful_apply_stores_through_stdin_then_reads_the_credential_and_helper_back() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        script::unconfigured(&sys, &u);
        script::configured(&sys, &u, "octocat");
        let writes = script::configure(&sys, &u, "octocat");
        script::holds_nothing(&sys, Some(&u));
        script::holds(&sys, Some(&u), GITHUB, "octocat", VALUE);
        signs_in(&sys, &u, "octocat");
        let approve = script::approves(&sys, Some(&u));
        auth.apply(&sys, &context).unwrap();
        let calls = sys.calls();
        let at = |key: &str| calls.iter().position(|c| c == key).unwrap();
        let last = |key: &str| calls.iter().rposition(|c| c == key).unwrap();
        assert!(at("alice$ agh GET /user") < at(&approve), "{calls:?}");
        assert!(at(&approve) < at(&writes[0]), "{calls:?}");
        assert!(at(&writes[3]) < last(&script::helpers(&u)), "{calls:?}");
        assert!(last(&script::helpers(&u)) < last(&fill(&u)), "{calls:?}");
        assert!(last(&fill(&u)) < last("alice$ agh GET /user"), "{calls:?}");
        clean(&sys);
    }

    #[test]
    fn the_read_back_after_setup_names_what_did_not_read_back() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        let mut foreign = script::configured_settings(&u, "octocat");
        foreign.push_str(&script::printed(&[(
            "/Users//alice/.gitconfig",
            "credential.https://GitHub.com.helper",
            "!other-credential-helper",
        )]));
        script::unconfigured(&sys, &u);
        sys.on(&script::helpers(&u), Ran::ok(&foreign));
        let writes = script::configure(&sys, &u, "octocat");
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        signs_in(&sys, &u, "octocat");
        let error = auth.apply(&sys, &context).unwrap_err();
        assert!(
            error.contains(
                "credential.https://GitHub.com.helper in /Users//alice/.gitconfig also runs for \
                 github.com"
            ),
            "{error}"
        );
        assert!(!error.contains("other-credential-helper"), "{error}");
        assert!(writes.iter().all(|w| sys.calls().contains(w)));
        clean(&sys);

        let (sys, auth, context) = fixture();
        script::unconfigured(&sys, &u);
        script::configured(&sys, &u, "octocat");
        script::configure(&sys, &u, "octocat");
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        signs_in(&sys, &u, "octocat");
        sys.on_github_api(&u, "/user", Err("connection reset".into()));
        let error = auth.apply(&sys, &context).unwrap_err();
        assert!(
            error.starts_with("the login keychain's GitHub credential did not read back"),
            "{error}"
        );
        assert!(
            !error.contains("helper") && !error.contains("connection reset"),
            "{error}"
        );
        clean(&sys);
    }

    #[test]
    fn a_store_the_keychain_does_not_read_back_fails_before_the_helper_is_configured() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        script::unconfigured(&sys, &u);
        script::configure(&sys, &u, "octocat");
        script::holds_nothing(&sys, Some(&u));
        signs_in(&sys, &u, "octocat");
        script::approves(&sys, Some(&u));
        let error = auth.apply(&sys, &context).unwrap_err();
        assert!(error.contains("did not hand back"), "{error}");
        assert!(!sys.calls().iter().any(|c| c.contains("--global")));
        clean(&sys);
    }

    #[test]
    fn unsafe_token_file_and_unproven_helper_are_never_ready() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        script::unconfigured(&sys, &u);
        script::holds_nothing(&sys, Some(&u));
        sys.stat_of(SOURCE, stat(0o644, VALUE.len()));
        assert!(matches!(auth.probe(&sys, &context), Probe::Blocked { .. }));
        let (sys, auth, context) = fixture();
        script::configured(&sys, &u, "octocat");
        sys.refuse_complete_stdout(&script::helpers(&u), "rejected partial tail");
        assert!(matches!(auth.probe(&sys, &context), Probe::Unusable { .. }));
        let (sys, auth, context) = fixture();
        script::configured(&sys, &u, "octocat");
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        signs_in(&sys, &u, "someone-else");
        assert!(matches!(auth.probe(&sys, &context), Probe::Change { .. }));
        assert!(sys.calls().iter().all(|c| !c.contains("approve")));
    }

    #[test]
    fn without_clt_git_the_bootstrap_names_the_item_that_installs_it() {
        let (sys, auth, context) = fixture();
        let sys = {
            let mut bare = Fake::root();
            bare.euid = sys.euid;
            bare
        };
        let Probe::Unusable { why } = auth.probe(&sys, &context) else {
            panic!("no git is not ready");
        };
        assert!(why.contains("aterm init --only base.clt"), "{why}");
        assert!(
            !why.contains("xcode-select") && !why.contains("brew"),
            "{why}"
        );
    }
}
