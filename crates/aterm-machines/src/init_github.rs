// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local GitHub bootstrap: git's HTTPS credential route (aterm's own helper, never the login
//! keychain) and repositories. By default the first launch's own repository stage ([`native`])
//! reads the private account and complete repository inventory from the vault's
//! `github-repositories.json`, beside its development token. init.toml's `[github]` table
//! scopes, turns off, or replaces the source with its own token source.
//! Beside it, in every mode but off, [`keychain`] replaces the keychain block an earlier `aterm
//! init` wrote, read from git's settings before any GitHub answer.

use crate::git_credential::{Git, Helper, Lookups};
use crate::items::{Ctx, Item};
use crate::model::{Probe, User};
use crate::port::{GithubError, Secret, System};
use serde::Deserialize;
use std::io;
use std::path::{Component, Path, PathBuf};

mod credentials;
mod dependencies;
mod inventory;
mod keychain;
mod native;
mod repos;
mod source;
pub use credentials::{sync_vault_git_credentials, vault_git_credentials};
pub use native::InitialRepositoryInputs;

/// The private vault entries that determine default repository acquisition. Other vault
/// entries must not invalidate a clone whose source and credential remain unchanged.
pub const VAULT_INPUT_ENTRIES: [&str; 2] = ["gh_access_token", "github-repositories.json"];

/// Whether Git is ready ([`crate::items::github::installed_git`]): managed Git is preferred,
/// with developer-tools or system Git as fallbacks. Repositories wait on
/// [`crate::init_observe::Wait::GitReadiness`] until one is available.
#[must_use]
pub fn git_installed(sys: &dyn System) -> bool {
    repos::git_installed(sys)
}

/// Whether repository setup uses the private vault inventory rather than an explicit profile.
/// This reads only the local init profile, without credentials or GitHub requests.
///
/// # Errors
/// The context or init profile cannot be admitted.
pub fn uses_vault_inventory(sys: &dyn System, ctx: &Ctx) -> Result<bool, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("repository inventory requires exactly one local user".into());
    };
    let table = profile(sys, user)?.and_then(|profile| profile.github);
    Ok(matches!(asked(table, user)?, Asked::Default))
}

/// Count matching local checkouts against the private vault's complete inventory. No
/// GitHub inventory, token, credential helper, or filesystem mutation is involved.
/// An explicit GitHub profile leaves this default inventory out of its setup.
///
/// # Errors
/// The local account, private inventory, or home cannot be admitted.
pub(crate) fn inventory_counts(
    sys: &dyn System,
    ctx: &Ctx,
) -> Result<Option<(usize, usize)>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("repository inventory requires exactly one local user".into());
    };
    let table = profile(sys, user)?.and_then(|profile| profile.github);
    if !matches!(asked(table, user)?, Asked::Default) {
        return Ok(None);
    }
    let listed = inventory::Inventory::read(sys, user)?.repositories()?;
    let standing = repos::partition(sys, user, &listed.repositories)?;
    Ok(Some((standing.present.len(), listed.repositories.len())))
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

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Github {
    pub account: String,
    pub owner: String,
    pub token_file: PathBuf,
    pub token_key: Option<String>,
}

impl Github {
    /// aterm's helper over this token source: a profile's own file, answering as its account.
    fn helper(&self, sys: &dyn System, user: &User) -> Option<Helper> {
        Helper::token_file(
            sys,
            user,
            &self.account,
            &self.token_file,
            self.token_key.as_deref(),
        )
    }
}

/// aterm's own GitHub helper for `user`, which every network git init and the converge pass run
/// carries
/// ([`crate::git_credential::pinned`]): over the token file init.toml's `[github]` names, else
/// the vault's; `None` while `user` has no launcher.
pub(crate) fn helper(sys: &dyn System, user: &User) -> Option<Helper> {
    match profile_source(sys, user) {
        Some(config) => config.helper(sys, user),
        None => Helper::vault(sys, user),
    }
}

/// Whether `row` is the keychain item's ([`keychain`]), which no repository step depends on.
pub(crate) fn keychain_row(row: &crate::model::ItemReport) -> bool {
    row.id.starts_with("init.github.keychain@")
}

/// The token source init.toml's `[github]` names for `user`; `None` is the vault's.
fn profile_source(sys: &dyn System, user: &User) -> Option<Github> {
    let table = profile(sys, user)
        .ok()
        .flatten()
        .and_then(|read| read.github);
    match asked(table, user) {
        Ok(Asked::Profile(config)) => Some(config),
        _ => None,
    }
}

/// Whether the token aterm's helper ([`helper`]) would hand back is on this machine, read from
/// the file as the helper reads it: no GitHub call, and the value goes no further than the
/// scrubbed buffer this drops. A helper with none answers `quit`, which ends the person's own
/// `git push`.
pub(super) fn token_here(sys: &dyn System, user: &User) -> bool {
    let source = profile_source(sys, user).unwrap_or_else(|| Github {
        account: String::new(),
        owner: String::new(),
        token_file: Path::new(&user.home).join(TOKEN_FILE),
        token_key: None,
    });
    read_token(sys, user, &source).is_ok()
}

/// Whether `user`'s git settings are aterm's to touch: `false` for `[github] enabled = false`.
///
/// # Errors
/// init.toml cannot be read safely, or its `[github]` table cannot be admitted.
pub(super) fn settings_managed(sys: &dyn System, user: &User) -> Result<bool, String> {
    let table = profile(sys, user)?.and_then(|read| read.github);
    Ok(!matches!(asked(table, user)?, Asked::Off))
}

/// The repository stage `aterm init` runs: the first launch's own ([`native`]) unless init.toml's
/// `[github]` turns it off or names its own token source, whose authentication and repository
/// items then run. Either waits for the item `secrets` names, when one does: the secrets vault
/// this run joins, or this Mac waits to be let into, writes the token file it reads
/// ([`crate::init_secrets::Secrets::waited_on`]). The keychain item ([`keychain`]) comes first
/// and waits for nothing: it asks GitHub nothing.
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
        Asked::Default => {
            return Ok(vec![
                keychain::item(user),
                native::item(user, secrets.map(str::to_owned), true),
            ]);
        }
        Asked::Scoped(_) => {
            return Ok(vec![
                keychain::item(user),
                native::item(user, secrets.map(str::to_owned), false),
            ]);
        }
        Asked::Profile(config) => config,
    };
    let recorded = crate::init_develop::recorded(user, profile.develop.as_ref())?;
    Ok(vec![
        keychain::item(user),
        Box::new(Auth {
            user: user.clone(),
            config: config.clone(),
            after: secrets.map(str::to_owned),
        }),
        repos::item(user, &config, recorded),
    ])
}

/// What `sync.clone` reads of the GitHub step an `aterm init` owes ([`crate::init::owed_clone`]):
/// whether the token it reads is on this Mac, and the step's mark.
pub(crate) struct TokenRead {
    /// The token is here: the vault's ([`TOKEN_FILE`]), or the `token_file` init.toml's
    /// `[github]` names.
    pub(crate) landed: bool,
    /// Why a token file that is there cannot be used (not a regular file of the user's, a
    /// link, readable by others, or too long): what is wrong with it, in plain words.
    pub(crate) unusable: Option<String>,
    /// A digest of init.toml, private repository metadata, the token and whether git is installed, which moves when any of
    /// them changes: a clone only a person could finish waits until it does. The token itself is
    /// not kept.
    pub(crate) mark: String,
}

/// [`TokenRead`] for this Mac's one user; `None` where `[github]` turns the step off.
///
/// # Errors
/// The run does not name exactly one user, or init.toml cannot be admitted.
pub(crate) fn token_read(sys: &dyn System, ctx: &Ctx) -> Result<Option<TokenRead>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init GitHub requires exactly one local user".into());
    };
    let config = Path::new(&user.home).join(".config/aterm/init.toml");
    let config = sys
        .read_owned_file(&config, user.uid, INPUT_MAX, false)
        .unwrap_or_default();
    let table = profile(sys, user)?.and_then(|profile| profile.github);
    let path = match asked(table, user)? {
        Asked::Off => return Ok(None),
        Asked::Default => Path::new(&user.home).join(TOKEN_FILE),
        Asked::Scoped(config) | Asked::Profile(config) => config.token_file,
    };
    let read = sys.read_owned_file(&path, user.uid, INPUT_MAX, true);
    let unusable = match &read {
        Err(why) if why.kind() != io::ErrorKind::NotFound => Some(format!(
            "the GitHub token at {} is on this Mac but cannot be used ({why}): it must be a \
             regular file of yours, not a link, that only you can read, of at most {} KiB",
            path.display(),
            INPUT_MAX / 1024
        )),
        _ => None,
    };
    let token = read.ok().map(zeroize::Zeroizing::new);
    let inventory = sys
        .read_owned_file(
            &Path::new(&user.home).join(inventory::FILE),
            user.uid,
            inventory::MAX_BYTES,
            true,
        )
        .unwrap_or_default();
    // Whether git is there is part of the mark: a step that stopped without it runs again once
    // the Command Line Tools bring it.
    let git = [u8::from(crate::items::github::installed_git(sys).is_some())];
    let mut digest = aterm_digest::Sha256::new();
    for part in [
        config.as_slice(),
        token.as_ref().map_or(&[][..], |token| token.as_slice()),
        inventory.as_slice(),
        &git[..],
    ] {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    Ok(Some(TokenRead {
        landed: token.is_some(),
        unusable,
        mark: aterm_codec::hex::encode_lower(&digest.finalize()),
    }))
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
    if table
        .token_key
        .as_deref()
        .is_some_and(|key| !aterm_secrets::token_line::assignable(key))
    {
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

/// Why the GitHub bootstrap cannot start on a Mac without git.
pub(crate) const NEEDS_GIT: &str = "GitHub bootstrap needs git, from Apple's Command Line Tools \
                                    or Xcode (aterm init --only base.clt installs the tools)";

/// The token file's token, when aterm's route hands it back ([`Git::lookups`]) and GitHub signs
/// it in as the configured account. A GitHub that cannot vouch for that token is no token here:
/// the callers only read, and the Auth row says why.
pub(super) fn auth_ready(
    sys: &dyn System,
    user: &User,
    config: &Github,
) -> Result<Option<Secret>, String> {
    let Ok(token) = read_token(sys, user, config) else {
        return Ok(None);
    };
    let git = Git::for_user(sys, user);
    if !git.installed(sys) {
        return Err(NEEDS_GIT.into());
    }
    let helper = config.helper(sys, user);
    if !git.lookups(sys, user, helper.as_ref(), &token)?.ready() {
        return Ok(None);
    }
    Ok(signs_in_as(sys, user, &token)
        .ok()
        .flatten()
        .filter(|account| account.eq_ignore_ascii_case(&config.account))
        .map(|_| token))
}

/// git's GitHub HTTPS route for a profile's own token file: aterm's helper hands that token back,
/// never the login keychain, and GitHub signs it in as the configured account. The item only
/// reads: it stores the token nowhere and writes nothing.
struct Auth {
    user: User,
    config: Github,
    /// The secrets item it waits for ([`items`]).
    after: Option<String>,
}

impl Auth {
    /// The token, and what aterm's route hands back for it; the probe's answer where it cannot
    /// read them.
    fn read(&self, sys: &dyn System) -> Result<(Secret, Lookups), Probe> {
        let git = Git::for_user(sys, &self.user);
        if !git.installed(sys) {
            return Err(Probe::Unusable {
                why: NEEDS_GIT.into(),
            });
        }
        let token = read_token(sys, &self.user, &self.config)
            .map_err(|unread| Probe::Blocked { why: unread.why })?;
        let helper = self.config.helper(sys, &self.user);
        let lookups = git
            .lookups(sys, &self.user, helper.as_ref(), &token)
            .map_err(|why| Probe::Unusable { why })?;
        Ok((token, lookups))
    }
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
            "GitHub HTTPS credential for {} through aterm's own helper, never the login keychain",
            self.config.account
        )
    }
    fn after(&self) -> Vec<String> {
        self.after.iter().cloned().collect()
    }
    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let (token, lookups) = match self.read(sys) {
            Ok(read) => read,
            Err(probe) => return probe,
        };
        if let Lookups::Not(why) = lookups {
            return Probe::Blocked {
                why: format!(
                    "aterm's GitHub route does not hand back the token in {}: {why}; aterm never \
                     keeps it in the login keychain",
                    self.config.token_file.display()
                ),
            };
        }
        match signs_in_as(sys, &self.user, &token) {
            Ok(Some(account)) if account.eq_ignore_ascii_case(&self.config.account) => {
                Probe::Ok { now: self.title() }
            }
            Ok(_) => Probe::Blocked {
                why: format!(
                    "aterm's helper hands back the token in {}, but GitHub refuses it or signs it \
                     in as another account than {}",
                    self.config.token_file.display(),
                    self.config.account
                ),
            },
            Err(_) => Probe::Unusable {
                why: "aterm's helper hands back the GitHub token file's token, but GitHub did \
                      not say which account it signs in as (output withheld)"
                    .into(),
            },
        }
    }
    /// The item only reads: its probe plans no change.
    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Ok(())
    }
}

/// What a probe that found the block an earlier `aterm init` wrote says it reads.
pub(super) const KEYCHAIN_BLOCK: &str = "git's global settings hold a GitHub credential block of the shape an earlier aterm init \
     wrote, which reads the login keychain (or, off a Mac, a plain token file)";

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
        .map_err(|error| {
            let absent = error.kind() == io::ErrorKind::NotFound;
            TokenUnread {
                absent,
                why: if absent {
                    format!(
                        "the GitHub token file ({}) is not on this {} yet",
                        config.token_file.display(),
                        if sys.macos() { "Mac" } else { "machine" }
                    )
                } else {
                    "GitHub token source must be an owned private regular file (0600), without \
                     links, at most 64 KiB"
                        .into()
                },
            }
        })?;
    let result = token(&bytes, config.token_key.as_deref());
    aterm_uds::secret::scrub(&mut bytes);
    result.map_err(|why| TokenUnread { absent: false, why })
}

fn token(bytes: &[u8], key: Option<&str>) -> Result<Secret, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "GitHub token source must be UTF-8")?;
    let value = if let Some(key) = key {
        aterm_secrets::token_line::assigned(text, key).map_err(|unassigned| match unassigned {
            aterm_secrets::token_line::Unassigned::Repeated => {
                "GitHub token_key appears more than once; no file content was executed"
            }
            aterm_secrets::token_line::Unassigned::Missing => {
                "GitHub token_key was not found; no file content was executed"
            }
        })?
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
    use crate::port::{Fake, GithubResponse, Stat};

    const GIT: &str = crate::items::github::CLT_GIT;
    const SOURCE: &str = "/Users//alice/.secrets/github";
    const VALUE: &str = "fixture_secret_injected_only";
    const OTHER: &str = "fixture_other_token_only";

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
        script::launcher(&sys, &u);
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
            },
            ctx(&[u]),
        )
    }
    /// aterm's route over `auth`'s token file hands back `token`.
    fn holds(sys: &Fake, auth: &Auth, token: &str) {
        let helper = auth.config.helper(sys, &auth.user).unwrap();
        script::hands_back(sys, &auth.user, &helper, token);
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
    /// No call carries a token, none is gh's, and none asks the login keychain or writes git's
    /// settings other than by replacing the keychain block.
    fn clean(sys: &Fake) {
        let calls = sys.calls();
        assert!(
            calls.iter().all(|c| !c.contains(VALUE)
                && !c.contains(OTHER)
                && !c.contains("/gh")
                && !c.contains("approve")
                && !c.contains("osxkeychain")
                && !c.contains("--replace-all")
                && !c.contains("--global")),
            "{calls:?}"
        );
    }

    /// Nothing replaced in git's settings either.
    fn unwritten(sys: &Fake) {
        clean(sys);
        assert!(
            sys.calls()
                .iter()
                .all(|c| !c.contains("--unset-all") && !c.contains("--add")),
            "{:?}",
            sys.calls()
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
        let native: &[&str] = &["init.github.keychain@alice", "init.github.native@alice"];
        assert_eq!(ids(&sys), native);
        let u = &context.users[0];
        for (text, want) in [
            (&b"[github]\nenabled = false\n"[..], &[][..]),
            (b"[develop]\ncheckouts = []\n", native),
            (b"[github]\naccount = 'alice'\n", native),
            (
                b"[github]\naccount='octocat'\nowner='octocat'\ntoken_file='~/.secrets/github'\n",
                &[
                    "init.github.keychain@alice",
                    "init.github.auth@alice",
                    "init.github.repositories@alice",
                ][..],
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

    /// A GitHub that cannot vouch for the token stops the repositories, in a plan and an apply:
    /// no inventory runs, no clone starts, and the keychain block still gives way, since that
    /// waits for neither the token nor GitHub.
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
            script::keychain_block(&sys, &u, "octocat");
            script::keychain_block(&sys, &u, "octocat");
            script::alike(&sys, &u);
            holds(&sys, &auth, VALUE);
            let helper = auth.config.helper(&sys, &u).unwrap();
            script::replaces_keychain_block_with(&sys, &u, &helper);
            sys.on_github_api(&u, "/user", Err("refused (output withheld)".into()));
            let rows = engine::run(
                &items(&sys, &context, None).unwrap(),
                mode,
                &sys,
                &context,
                &mut NoJournal,
            );
            assert_eq!(rows.len(), 3);
            assert_eq!(
                rows[0].outcome,
                if mode == Mode::Plan {
                    Outcome::Planned
                } else {
                    Outcome::Changed
                },
                "{rows:?}"
            );
            assert_eq!(rows[1].outcome, Outcome::Unknown, "{rows:?}");
            assert_eq!(rows[2].outcome, Outcome::Blocked, "{rows:?}");
            assert!(!aterm_json::to_string(&rows).unwrap().contains(VALUE));
            assert!(
                !sys.calls()
                    .iter()
                    .any(|c| c.contains("/repos") || c.contains(" clone "))
            );
            clean(&sys);
        }
    }

    /// The token file is the credential's one source: without it the row waits for it, and no
    /// git or helper runs.
    #[test]
    fn a_missing_token_file_blocks_and_runs_nothing() {
        let (sys, mut auth, context) = fixture();
        auth.config.token_file = "/not-present".into();
        let Probe::Blocked { why } = auth.probe(&sys, &context) else {
            panic!("no token file is not ready");
        };
        assert!(why.contains("/not-present"), "{why}");
        assert!(
            sys.calls().iter().all(|c| !c.contains(GIT)),
            "{:?}",
            sys.calls()
        );
    }

    /// GitHub vouches for the token git hands back: another account's, a refusal and an answer
    /// that is no account are never ready, and their output is withheld.
    #[test]
    fn the_token_identity_is_checked_by_github_and_all_failure_output_is_withheld() {
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
            holds(&sys, &auth, VALUE);
            sys.on_github_api(&u, "/user", answer);
            let why = match auth.probe(&sys, &context) {
                Probe::Blocked { why } | Probe::Unusable { why } => why,
                other => panic!("{other:?}"),
            };
            assert!(!why.contains(VALUE), "{why}");
            assert!(auth_ready(&sys, &u, &auth.config).unwrap().is_none());
            unwritten(&sys);
        }
    }

    /// The row reads and writes nothing of git's settings: aterm's helper is asked once, for the
    /// token file's token, and then GitHub. With no launcher there is no route: the row says so,
    /// and neither is asked.
    #[test]
    fn the_auth_row_asks_atermss_helper_then_github_and_writes_nothing() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        holds(&sys, &auth, VALUE);
        signs_in(&sys, &u, "octocat");
        assert!(matches!(auth.probe(&sys, &context), Probe::Ok { .. }));
        let calls = sys.calls();
        let token_file = "--token-file /Users//alice/.secrets/github";
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(
            calls[0].contains(token_file) && calls[0].ends_with("credential fill"),
            "{calls:?}"
        );
        assert_eq!(calls[1], "alice$ agh GET /user");
        unwritten(&sys);

        let mut bare = Fake::root();
        bare.euid = 501;
        bare.stat_of(GIT, stat(0o755, 0));
        bare.file(SOURCE, VALUE.as_bytes())
            .stat_of(SOURCE, stat(0o600, VALUE.len()));
        let Probe::Blocked { why } = auth.probe(&bare, &context) else {
            panic!("no launcher is no route");
        };
        assert!(why.contains(".local/bin/aterm"), "{why}");
        assert!(bare.calls().is_empty(), "{:?}", bare.calls());
    }

    #[test]
    fn unsafe_token_file_and_unproven_lookups_are_never_ready() {
        let (sys, auth, context) = fixture();
        sys.stat_of(SOURCE, stat(0o644, VALUE.len()));
        assert!(matches!(auth.probe(&sys, &context), Probe::Blocked { .. }));
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        holds(&sys, &auth, VALUE);
        signs_in(&sys, &u, "someone-else");
        assert!(matches!(auth.probe(&sys, &context), Probe::Blocked { .. }));
        unwritten(&sys);
    }

    #[test]
    fn git_lookups_that_already_hand_back_the_token_file_are_left_as_they_are() {
        let (sys, auth, context) = fixture();
        let u = auth.user.clone();
        holds(&sys, &auth, VALUE);
        signs_in(&sys, &u, "Octocat");
        let ready = auth_ready(&sys, &u, &auth.config).unwrap();
        assert!(ready.is_some_and(|token| token.bytes() == VALUE.as_bytes()));
        let rows = engine::run(
            &[Box::new(auth)],
            Mode::Apply,
            &sys,
            &context,
            &mut NoJournal,
        );
        assert_eq!(rows[0].outcome, Outcome::Ok, "{rows:?}");
        unwritten(&sys);
    }

    /// Lookups that hand back another token, or one GitHub signs in as another account, refuses
    /// or cannot vouch for, are not ready, and nothing is written: aterm never stores a token for
    /// git, in the keychain or anywhere else.
    #[test]
    fn git_lookups_no_one_vouches_for_are_not_ready_and_nothing_is_written() {
        let refused = || {
            Ok(GithubResponse {
                status: 401,
                headers: Vec::new(),
                body: Vec::new(),
            })
        };
        let cases: [(&str, Result<GithubResponse, String>, bool); 4] = [
            (
                OTHER,
                Ok(GithubResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: b"{\"login\":\"octocat\"}".to_vec(),
                }),
                false,
            ),
            (
                VALUE,
                Ok(GithubResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: b"{\"login\":\"someone-else\"}".to_vec(),
                }),
                false,
            ),
            (VALUE, refused(), false),
            (VALUE, Err("timed out (output withheld)".into()), true),
        ];
        for (held, answer, unusable) in cases {
            let (sys, auth, context) = fixture();
            let u = auth.user.clone();
            holds(&sys, &auth, held);
            sys.on_github_api(&u, "/user", answer);
            assert!(auth_ready(&sys, &u, &auth.config).unwrap().is_none());
            let probed = auth.probe(&sys, &context);
            if unusable {
                assert!(matches!(probed, Probe::Unusable { .. }), "{probed:?}");
            } else {
                assert!(matches!(probed, Probe::Blocked { .. }), "{probed:?}");
            }
            unwritten(&sys);
        }
        let (sys, auth, context) = fixture();
        let u = context.users[0].clone();
        holds(&sys, &auth, VALUE);
        sys.on_github_api(&u, "/user", Err("timed out (output withheld)".into()));
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
