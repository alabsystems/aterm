// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local GitHub bootstrap for keychain credentials, git HTTPS helper, and repositories: by
//! default the first launch's own repository stage ([`native`]), which init.toml's `[github]`
//! table scopes, turns off, or replaces with its own token source.

use crate::git_credential::{GITHUB, Git, Lookups};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{GithubError, Secret, System};
use serde::Deserialize;
use std::io;
use std::path::{Component, Path, PathBuf};

mod native;
mod repos;
pub use native::InitialRepositoryInputs;

/// Whether the git every clone runs ([`crate::items::github::installed_git`]: the Command Line
/// Tools', else Xcode's) is installed: the initial repositories wait on it
/// ([`crate::init_observe::Wait::CommandLineTools`]) until it is.
#[must_use]
pub fn git_installed(sys: &dyn System) -> bool {
    repos::git_installed(sys)
}

/// Reconcile using locally captured inputs, checking their materialization
/// identity after inventory and before effects. The check never leases network work.
/// # Errors
/// As [`run_initial_repositories`], or the captured version is no longer ready.
pub fn run_initial_repositories_captured(
    sys: &dyn System,
    ctx: &Ctx,
    mode: crate::model::Mode,
    inputs: InitialRepositoryInputs,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<Vec<crate::model::ItemReport>, String> {
    native::run_captured(sys, ctx, mode, inputs, Box::new(current))
}

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

/// The GitHub token the ALab vault's sync writes, under the home: the repository stage's token
/// source unless init.toml's `[github]` names another. It is the file agh adopts as its dev
/// profile when there is no agh.toml (agh design §6.10), so the two name one file.
pub(crate) const TOKEN_FILE: &str = agh::identity::vault::DEV_TOKEN_FILE;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    github: Option<Table>,
    /// The develop profile's checkouts ([`crate::init_develop`]), which the repository item
    /// leaves to the develop items.
    #[serde(default)]
    develop: Option<crate::init_develop::Table>,
}

/// init.toml's `[github]` table as written ([`Asked`] is what it asks).
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    /// `false` leaves the repositories to the person.
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    token_file: Option<PathBuf>,
    #[serde(default)]
    token_key: Option<String>,
}

const fn enabled() -> bool {
    true
}

/// What init.toml's `[github]` table asks of the repository stage.
enum Asked {
    /// No table: the native stage over the account's own repositories.
    Default,
    /// `enabled = false`: no repository stage.
    Off,
    /// `account` (and `owner`, else the account) without a token source: the native stage
    /// over that owner's repositories, signed in as that account.
    Scoped(Github),
    /// A `token_file` too: that source's credential item and its repository item, which also
    /// fast-forwards the clean worktrees behind GitHub.
    Profile(Github),
}

#[derive(Clone)]
pub(super) struct Github {
    pub account: String,
    pub owner: String,
    pub token_file: PathBuf,
    pub token_key: Option<String>,
}

/// The repository stage `aterm init` runs: the first launch's own ([`native`]) unless init.toml's
/// `[github]` turns it off or names its own token source, whose authentication and repository
/// items then run. Either waits for the item `secrets` names, when one does: the secrets vault
/// this run joins, or this Mac waits to be let into, writes the token file it reads
/// ([`crate::init_secrets::Secrets::waited_on`]).
///
/// # Errors
/// The local profile cannot be admitted or decoded. Its contents are never quoted.
pub fn items(
    sys: &dyn System,
    ctx: &Ctx,
    secrets: Option<&str>,
) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init GitHub requires exactly one local user".into());
    };
    let profile = profile(sys, user)?.unwrap_or_default();
    let config = match asked(profile.github, user)? {
        Asked::Off => return Ok(Vec::new()),
        Asked::Default => return Ok(vec![native::item(user, secrets.map(str::to_owned), true)]),
        Asked::Scoped(_) => {
            return Ok(vec![native::item(user, secrets.map(str::to_owned), false)]);
        }
        Asked::Profile(config) => config,
    };
    let recorded = crate::init_develop::recorded(user, profile.develop.as_ref())?;
    Ok(vec![
        Box::new(Auth {
            user: user.clone(),
            config: config.clone(),
            after: secrets.map(str::to_owned),
            revert: std::sync::Mutex::new(None),
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
    decode(&bytes).map(Some)
}

/// Returns the home's checkouts of the configured owner's repositories that are aterm workspaces.
///
/// # Errors
/// The profile cannot be admitted, or the listing or a worktree cannot be read.
pub(crate) fn aterm_checkouts(sys: &dyn System, ctx: &Ctx) -> Result<Vec<PathBuf>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init --develop requires exactly one local user".into());
    };
    let table = profile(sys, user)?.and_then(|profile| profile.github);
    let Asked::Profile(config) = asked(table, user)? else {
        return Ok(Vec::new());
    };
    let Some(token) = auth_ready(sys, user, &config)? else {
        return Ok(Vec::new());
    };
    repos::aterm_checkouts(sys, user, &config, &token)
}

fn decode(bytes: &[u8]) -> Result<Config, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "init.toml must be UTF-8; no configuration contents were printed")?;
    aterm_toml::from_str(text)
        .map_err(|_| "invalid init.toml; expected [github] enabled, account, owner, token_file and token_key, and [develop] checkouts (credential values do not belong in this file)".into())
}

/// What `table` asks ([`Asked`]), its names and paths checked.
fn asked(table: Option<Table>, user: &User) -> Result<Asked, String> {
    let Some(table) = table else {
        return Ok(Asked::Default);
    };
    if !table.enabled {
        return Ok(Asked::Off);
    }
    let Some(account) = table.account else {
        return Err(
            "GitHub owner, token_file and token_key need an account in init.toml's [github]".into(),
        );
    };
    let owner = table.owner.unwrap_or_else(|| account.clone());
    if !login(&account) || !login(&owner) {
        return Err("GitHub account and owner must be valid login names".into());
    }
    if table.token_key.as_deref().is_some_and(|k| !env_key(k)) {
        return Err("GitHub token_key must be an environment assignment name".into());
    }
    let Some(token_file) = table.token_file else {
        if table.token_key.is_some() {
            return Err("GitHub token_key reads the token_file init.toml's [github] names".into());
        }
        return Ok(Asked::Scoped(Github {
            account,
            owner,
            token_file: Path::new(&user.home).join(TOKEN_FILE),
            token_key: None,
        }));
    };
    let path = token_file
        .to_str()
        .ok_or("GitHub token_file must be UTF-8")?;
    let token_file = if let Some(relative) = path.strip_prefix("~/") {
        Path::new(&user.home).join(relative)
    } else {
        token_file.clone()
    };
    if !token_file.is_absolute()
        || token_file
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(
            "GitHub token_file must be an absolute or ~/ path without dot components".into(),
        );
    }
    if token_file
        .file_name()
        .is_some_and(|n| n == "release-credentials.toml")
    {
        return Err("GitHub token_file cannot name aterm release credentials".into());
    }
    Ok(Asked::Profile(Github {
        account,
        owner,
        token_file,
        token_key: table.token_key,
    }))
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
) -> Result<crate::github_pages::Page, GithubError> {
    let reply = sys.github_api(user, token, endpoint)?;
    if reply.status == 401 {
        return Err(GithubError {
            failure: crate::port::GithubFailure::Unauthenticated,
            why: "GitHub rejected the credential or repository request (output withheld)".into(),
        });
    }
    crate::github_pages::page(reply.status, reply.headers, &reply.body).map_err(GithubError::other)
}

/// The account `token` signs in as (`GET /user`).
pub(super) fn identity(
    sys: &dyn System,
    user: &User,
    token: &Secret,
) -> Result<String, GithubError> {
    signed_in(get(sys, user, token, "/user")?)
}

/// [`identity`], `None` when GitHub refuses `token` itself (401: expired or revoked), an
/// answer as definite as another account's.
fn signs_in_as(sys: &dyn System, user: &User, token: &Secret) -> Result<Option<String>, String> {
    let reply = sys.github_api(user, token, "/user")?;
    if reply.status == 401 {
        return Ok(None);
    }
    signed_in(crate::github_pages::page(
        reply.status,
        reply.headers,
        &reply.body,
    )?)
    .map(Some)
    .map_err(String::from)
}

/// The login a `GET /user` page names.
fn signed_in((value, _): crate::github_pages::Page) -> Result<String, GithubError> {
    value["login"]
        .as_str()
        .filter(|account| login(account))
        .map(str::to_owned)
        .ok_or_else(|| GithubError::other("GitHub returned an invalid authenticated account"))
}

fn stored(sys: &dyn System, user: &User, account: &str) -> Result<Option<Secret>, String> {
    let Some(token) = Git::keychain(sys).fill(sys, Some(user), GITHUB, account)? else {
        return Ok(None);
    };
    Ok(match identity(sys, user, &token) {
        Ok(signed_in) if signed_in.eq_ignore_ascii_case(account) => Some(token),
        _ => None,
    })
}

/// Why the GitHub bootstrap cannot start on a Mac without git.
pub(crate) const NEEDS_GIT: &str = "GitHub bootstrap needs git, from Apple's Command Line Tools \
                                    or Xcode (aterm init --only base.clt installs the tools)";

type Readiness = (Lookups, Option<Secret>, Option<(PathBuf, String)>);

fn readiness(sys: &dyn System, user: &User, config: &Github) -> Result<Readiness, String> {
    let git = Git::keychain(sys);
    if !git.installed(sys) {
        return Err(NEEDS_GIT.into());
    }
    let owners = [config.owner.as_str()];
    let (helper, shadowed) = git.configured_or_shadowed(sys, user, &config.account, &owners)?;
    Ok((helper, stored(sys, user, &config.account)?, shadowed))
}

/// Returns the keychain token for the configured account when git's helper is also ready, else
/// the token file's when git's own lookups already hand it back ([`answered`]). A GitHub that
/// cannot vouch for that token is no token here: the callers only read, and the Auth row says why.
pub(super) fn auth_ready(
    sys: &dyn System,
    user: &User,
    config: &Github,
) -> Result<Option<Secret>, String> {
    let (helper, token, _) = readiness(sys, user, config)?;
    Ok(match token.filter(|_| helper.ready()) {
        Some(token) => Some(token),
        None => answered(sys, user, config).ok().flatten(),
    })
}

/// The token file's token, when git's own lookups already hand it back for every repository
/// alike ([`native::lookups_answer_for`]) and GitHub signs it in as the configured account: the
/// keychain is then neither written nor made git's only helper. `None` when they hand back
/// another token or none, GitHub refuses it or signs it in as another account, or the token file
/// is unusable; the keychain path then goes on.
/// # Errors
/// They hand it back but GitHub did not say whom it signs in as: rewriting git's settings on
/// that guess could remove the vault's helper for good.
fn answered(sys: &dyn System, user: &User, config: &Github) -> Result<Option<Secret>, String> {
    let Ok(token) = read_token(sys, user, config) else {
        return Ok(None);
    };
    let owners = [config.owner.as_str()];
    if !native::lookups_answer_for(sys, user, &config.account, &owners, &token) {
        return Ok(None);
    }
    let account = signs_in_as(sys, user, &token).map_err(|_| {
        "git's own helpers hand back the GitHub token file's token, but GitHub did not say which \
         account it signs in as (output withheld); nothing was changed"
            .to_owned()
    })?;
    Ok(account
        .filter(|account| account.eq_ignore_ascii_case(&config.account))
        .map(|_| token))
}

/// Whether `xdg_file`'s helper hands back the token file's token and GitHub signs it in as
/// `config.account`.
fn shadowed_vault_answers(
    sys: &dyn System,
    user: &User,
    config: &Github,
    xdg_file: &Path,
) -> Result<bool, String> {
    let Ok(token) = read_token(sys, user, config) else {
        return Ok(false);
    };
    if !Git::keychain(sys)
        .fill_with_global_file(sys, user, xdg_file, GITHUB)
        .is_ok_and(|ans| ans.is_some_and(|got| got.bytes() == token.bytes()))
    {
        return Ok(false);
    }
    let account = signs_in_as(sys, user, &token).map_err(|_| {
        "git's XDG helper hands back the GitHub token file's token, but GitHub did not say which \
         account it signs in as (output withheld); nothing was changed"
            .to_owned()
    })?;
    Ok(account.is_some_and(|account| account.eq_ignore_ascii_case(&config.account)))
}

struct Auth {
    user: User,
    config: Github,
    /// The secrets item it waits for ([`items`]).
    after: Option<String>,
    revert: std::sync::Mutex<Option<String>>,
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
    fn after(&self) -> Vec<String> {
        self.after.iter().cloned().collect()
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        *self
            .revert
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        let (helper, held, shadowed) = match readiness(sys, &self.user, &self.config) {
            Ok(ready) => ready,
            Err(why) => return Probe::Unusable { why },
        };
        if let Some((xdg_file, revert)) = shadowed {
            match shadowed_vault_answers(sys, &self.user, &self.config, &xdg_file) {
                Ok(true) => {
                    *self
                        .revert
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(revert);
                    return Probe::Change {
                        from: "git's global file overrides the XDG GitHub credential helper".into(),
                        to: format!(
                            "GitHub credential for {}: use git's XDG credential helper",
                            self.config.account
                        ),
                    };
                }
                Err(why) => return Probe::Unusable { why },
                Ok(false) => {}
            }
        }
        if matches!((&helper, &held), (Lookups::Ready, Some(_))) {
            return Probe::Ok { now: self.title() };
        }
        match answered(sys, &self.user, &self.config) {
            Ok(Some(_)) => Probe::Ok {
                now: format!(
                    "GitHub credential for {}: git's own helpers already hand back the token \
                     file's token, left as they are",
                    self.config.account
                ),
            },
            Err(why) => Probe::Unusable { why },
            Ok(None) => match (helper, held) {
                (Lookups::Not(why), Some(_)) => Probe::Change {
                    from: format!("GitHub HTTPS helper needs setup: {why}"),
                    to: self.title(),
                },
                (_, None) => match read_token(sys, &self.user, &self.config) {
                    Ok(_) => Probe::Change {
                        from: "GitHub credential or HTTPS helper needs setup".into(),
                        to: self.title(),
                    },
                    Err(unread) => Probe::Blocked { why: unread.why },
                },
                (Lookups::Ready, Some(_)) => Probe::Ok { now: self.title() },
            },
        }
    }
    fn apply(&self, sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        if sys.euid() != self.user.uid {
            return Err("GitHub bootstrap must run as its local user".into());
        }
        let already_answered = answered(sys, &self.user, &self.config)?.is_some();
        let git = Git::keychain(sys);
        if !git.installed(sys) {
            return Err(NEEDS_GIT.into());
        }
        let owners = [self.config.owner.as_str()];
        let (helper, shadowed) =
            git.configured_or_shadowed(sys, &self.user, &self.config.account, &owners)?;
        if let Some((xdg_file, _)) = shadowed
            && shadowed_vault_answers(sys, &self.user, &self.config, &xdg_file)?
        {
            git.unconfigure(sys, &self.user)?;
            if answered(sys, &self.user, &self.config)?.is_none() {
                return Err(
                    "git's XDG GitHub credential helper did not answer after removing the global override"
                        .into(),
                );
            }
            return Ok(());
        }
        if already_answered {
            return Ok(());
        }
        let held = stored(sys, &self.user, &self.config.account)?;
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
    fn revert(&self) -> Option<String> {
        self.revert
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|settings| {
                format!(
                    "{settings} (puts back git's GitHub settings; the login keychain's github.com \
                     credential stays)"
                )
            })
    }
}

/// Why [`read_token`] has no token: the engine's line, and whether the source is not there
/// at all (the vault has not written it yet) rather than there and unusable.
pub(super) struct TokenUnread {
    /// Nothing is at the source's path.
    pub(super) absent: bool,
    /// The engine's line; it never quotes the file.
    pub(super) why: String,
}

impl From<TokenUnread> for String {
    fn from(unread: TokenUnread) -> Self {
        unread.why
    }
}

fn read_token(sys: &dyn System, user: &User, config: &Github) -> Result<Secret, TokenUnread> {
    crate::port::no_core_dumps();
    let mut bytes = sys
        .read_owned_file(&config.token_file, user.uid, INPUT_MAX, true)
        .map_err(|error| TokenUnread {
            absent: error.kind() == io::ErrorKind::NotFound,
            why: "GitHub token source must be an owned private regular file (0600), without \
                  links, at most 64 KiB"
                .into(),
        })?;
    let result = token(&bytes, config.token_key.as_deref());
    scrub(&mut bytes);
    result.map_err(|why| TokenUnread { absent: false, why })
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

/// What other modules' tests script of the GitHub stage.
#[cfg(test)]
pub(crate) mod scripted {
    /// `user`'s account on `sys` with one repository of hers, `name`, already a worktree in
    /// `~`: the stage init runs with no init.toml, converged.
    pub(crate) fn converged(sys: &crate::port::Fake, user: &crate::model::User, name: &str) {
        super::native::tests::converged(sys, user, name);
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
                after: None,
                revert: std::sync::Mutex::new(None),
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

    /// `text` read as init.toml and its `[github]` table checked.
    fn checked(text: &[u8], user: &User) -> Result<Asked, String> {
        decode(text).and_then(|profile| asked(profile.github, user))
    }

    /// With no init.toml, or one with no `[github]` table, init runs the first launch's own
    /// repository stage; `enabled = false` turns it off; `account` (and `owner`) alone scope that
    /// stage; a `token_file` brings the profile's own credential and repository items. A table
    /// that cannot be used is refused without echoing what it holds.
    #[test]
    fn the_native_stage_is_the_default_and_init_toml_scopes_replaces_or_turns_it_off() {
        let (sys, _auth, context) = fixture();
        let ids = |sys: &Fake| -> Vec<String> {
            items(sys, &context, None)
                .unwrap()
                .iter()
                .map(|item| item.id())
                .collect()
        };
        assert_eq!(ids(&sys), ["init.github.native@alice"]);
        let u = &context.users[0];
        let native: &[&str] = &["init.github.native@alice"];
        for (text, want) in [
            (&b"[github]\nenabled = false\n"[..], &[][..]),
            (b"[develop]\ncheckouts = []\n", native),
            (b"[github]\naccount = 'alice'\n", native),
            (
                b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n",
                &["init.github.auth@alice", "init.github.repositories@alice"][..],
            ),
        ] {
            let (sys, _auth, _context) = fixture();
            let path = "/Users//alice/.config/aterm/init.toml";
            sys.file(path, text).stat_of(path, stat(0o600, text.len()));
            assert_eq!(ids(&sys), want, "{}", String::from_utf8_lossy(text));
        }
        match checked(b"[github]\naccount='alice'\nowner='team'\n", u).unwrap() {
            Asked::Scoped(config) => {
                assert_eq!(
                    (config.account.as_str(), config.owner.as_str()),
                    ("alice", "team")
                );
                assert_eq!(
                    config.token_file,
                    Path::new("/Users//alice/.secrets/gh_access_token")
                );
            }
            _ => panic!("account and owner alone scope the native stage"),
        }
        match checked(
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n",
            u,
        )
        .unwrap()
        {
            Asked::Profile(config) => assert_eq!(config.token_file, Path::new(SOURCE)),
            _ => panic!("a token_file is the profile's own source"),
        }
        for text in [
            format!("[github]\naccount='octocat'\nowner='octocat'\ntoken='{VALUE}'\n"),
            format!("[github]\naccount='octocat'\nowner='octocat'\ntoken_file='{VALUE}\n"),
            "[github]\naccount='-bad'\nowner='octocat'\ntoken_file='~/.secrets/github'".into(),
            "[github]\naccount='octocat'\nowner='octocat'\ntoken_file='../source'".into(),
            "[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.aterm/release-credentials.toml'".into(),
            "[github]\nowner='octocat'\n".into(),
            format!("[github]\naccount='octocat'\ntoken_key='{VALUE}'\n"),
        ] {
            let error = checked(text.as_bytes(), u)
                .err()
                .expect("invalid profile refused");
            assert!(
                !error.contains(VALUE),
                "configuration values must not appear in errors"
            );
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
        assert!(calls.contains(&fill(&u)), "{calls:?}");
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
                &items(&sys, &context, None).unwrap(),
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
    fn git_lookups_that_already_hand_back_the_token_file_are_left_as_they_are() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        script::alike(&sys, &u);
        script::holds_nothing(&sys, Some(&u));
        script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
        signs_in(&sys, &u, "Octocat");
        let ready = auth_ready(&sys, &u, &auth.config).unwrap();
        assert!(ready.is_some_and(|token| token.bytes() == VALUE.as_bytes()));
        auth.apply(&sys, &context).unwrap();
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Apply,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .all(|c| !c.contains("approve") && !c.contains("--global")),
            "{calls:?}"
        );
        clean(&sys);
    }

    #[test]
    fn git_lookups_with_another_token_or_account_still_take_the_keychain_path() {
        for (held, login) in [(STORED, "octocat"), (VALUE, "someone-else")] {
            let (sys, auth, context) = fixture();
            let u = auth.user.clone();
            script::alike(&sys, &u);
            script::holds_nothing(&sys, Some(&u));
            script::lookups_hold(&sys, &u, GITHUB, "octocat", held);
            signs_in(&sys, &u, login);
            assert!(auth_ready(&sys, &u, &auth.config).unwrap().is_none());
            assert!(matches!(auth.probe(&sys, &context), Probe::Change { .. }));
            clean(&sys);
        }
    }

    #[test]
    fn git_lookups_github_cannot_vouch_for_change_nothing() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        script::alike(&sys, &u);
        script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        script::configure(&sys, &u, "octocat");
        sys.on_github_api(&u, "/user", Err("timed out (output withheld)".into()));
        signs_in(&sys, &u, "octocat");
        let error = auth.apply(&sys, &context).unwrap_err();
        assert!(error.contains("did not say which account"), "{error}");
        let calls = sys.calls();
        assert!(
            calls
                .iter()
                .all(|c| !c.contains("approve") && !c.contains("--global")),
            "a GitHub that answered for the keychain's token a moment later must not let the \
             vault's helper be replaced: {calls:?}"
        );
        let (sys, auth, context) = fixture();
        script::alike(&sys, &u);
        script::holds_nothing(&sys, Some(&u));
        script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
        sys.on_github_api(&u, "/user", Err("timed out (output withheld)".into()));
        assert!(matches!(auth.probe(&sys, &context), Probe::Unusable { .. }));
        assert!(auth_ready(&sys, &u, &auth.config).unwrap().is_none());
        let profile =
            b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n";
        let path = "/Users//alice/.config/aterm/init.toml";
        sys.file(path, profile)
            .stat_of(path, stat(0o644, profile.len()));
        assert_eq!(
            aterm_checkouts(&sys, &context).unwrap(),
            Vec::<PathBuf>::new(),
            "a develop pass lists no checkouts rather than failing whole"
        );
        clean(&sys);
    }

    #[test]
    fn a_token_github_refuses_takes_the_keychain_path_again() {
        let refused = || {
            Ok(GithubResponse {
                status: 401,
                headers: Vec::new(),
                body: Vec::new(),
            })
        };
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        script::alike(&sys, &u);
        script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        signs_in(&sys, &u, "octocat");
        sys.on_github_api(&u, "/user", refused());
        let Probe::Change { from, .. } = auth.probe(&sys, &context) else {
            panic!("a refused token is no answer, so the keychain's verified one repairs git");
        };
        assert!(from.contains("helper needs setup"), "{from}");

        let (sys, auth, context) = fixture();
        script::alike(&sys, &u);
        script::alike(&sys, &u);
        script::configured(&sys, &u, "octocat");
        script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
        script::holds(&sys, Some(&u), GITHUB, "octocat", STORED);
        let writes = script::configure(&sys, &u, "octocat");
        sys.on_github_api(&u, "/user", refused());
        signs_in(&sys, &u, "octocat");
        auth.apply(&sys, &context).unwrap();
        let calls = sys.calls();
        assert!(writes.iter().all(|w| calls.contains(w)), "{calls:?}");
        assert!(
            !calls.contains(&approve(&u)),
            "the keychain's token is kept"
        );

        let (sys, auth, context) = fixture();
        script::alike(&sys, &u);
        script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
        script::holds_nothing(&sys, Some(&u));
        sys.on_github_api(&u, "/user", refused());
        let error = auth.apply(&sys, &context).unwrap_err();
        assert!(
            error.contains("did not verify the supplied token"),
            "{error}"
        );
        assert!(!sys.calls().iter().any(|c| c.contains("--global")));
        clean(&sys);
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

    #[test]
    fn auth_converts_shadowed_keychain_gitconfig_even_when_its_token_is_current() {
        for cached in [STORED, VALUE] {
            let (sys, auth, context) = fixture();
            let u = auth.user.clone();
            script::shadowed(&sys, &u, "octocat");
            script::shadowed(&sys, &u, "octocat");
            if cached == VALUE {
                // The current token also passes the ordinary helper's uniformity read.
                script::shadowed(&sys, &u, "octocat");
            }
            script::alike(&sys, &u);
            script::alike(&sys, &u);
            script::holds(&sys, Some(&u), GITHUB, "octocat", cached);
            script::holds(&sys, Some(&u), GITHUB, "octocat", cached);
            script::lookups_hold(&sys, &u, GITHUB, "octocat", cached);
            script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
            script::lookups_hold(&sys, &u, GITHUB, "octocat", VALUE);
            script::lookups_hold_xdg(&sys, &u, GITHUB, "octocat", VALUE);
            script::lookups_hold_xdg(&sys, &u, GITHUB, "octocat", VALUE);
            let unsets = script::unconfigure(&sys, &u);
            signs_in(&sys, &u, "octocat");
            let probed = auth.probe(&sys, &context);
            assert!(matches!(probed, Probe::Change { .. }), "{probed:?}");
            assert!(auth.revert().as_deref().is_some_and(|r| {
                r.contains("--add credential.https://github.com.helper osxkeychain")
            }));
            auth.apply(&sys, &context).unwrap();
            let calls = sys.calls();
            assert!(unsets.iter().all(|k| calls.contains(k)), "{calls:?}");
            assert!(
                calls
                    .iter()
                    .all(|c| !c.contains("approve") && !c.contains("--replace-all")),
                "{calls:?}"
            );
            clean(&sys);
        }
    }
}
