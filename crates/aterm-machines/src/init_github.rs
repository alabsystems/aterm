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
use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

mod checkout_inputs;
mod credentials;
mod dependencies;
mod inventory;
mod keychain;
mod native;
mod repos;
mod selection;
mod source;
pub use checkout_inputs::RepositoryCheckoutMetadata;
pub use credentials::{sync_vault_git_credentials, vault_git_credentials};
pub use inventory::{RepositoryCatalog, RepositoryCatalogVisibility};
pub use native::{InitialRepositoryInputs, RepositoryReadback, RepositoryRun};
pub(crate) use native::{automatic_inventory_counts, automatic_inventory_counts_captured};
pub use selection::{RepositoryCredentialWithdrawal, RepositorySelection};
pub use source::{
    RepositoryControlPaths, RepositoryInputMark, RepositoryInputPathCache, RepositoryRefreshCursor,
    repository_checkout_watch_paths, repository_input_control_watch_paths, repository_input_mark,
    repository_input_watch_paths,
};

/// The private vault entries that determine default repository acquisition. Other vault
/// entries must not invalidate a clone whose source and credential remain unchanged.
pub const VAULT_INPUT_ENTRIES: [&str; 2] = ["gh_access_token", inventory::ENTRY];

/// Whether a Git executable is installed ([`crate::items::github::installed_git`]): managed
/// Git is preferred, with developer-tools or system Git as fallbacks. This reads metadata
/// only; [`git_ready_for_inventory`] checks capabilities needed by selected repositories.
#[must_use]
pub fn git_installed(sys: &dyn System) -> bool {
    repos::git_installed(sys)
}

/// A test double's git before 2.44 answering [`git_ready_for_inventory`]'s capability question:
/// it refuses `--no-lazy-fetch`, and its read shows whether it `obeys` `GIT_NO_LAZY_FETCH`
/// ([`crate::checkout::lazy_fetch::before_the_option`]).
#[cfg(any(test, debug_assertions))]
pub use crate::checkout::lazy_fetch::before_the_option as git_before_no_lazy_fetch;

/// Whether the selected private inventory's Git requirements are met. Unlike the
/// metadata-only [`git_installed`], this may ask the selected executable whether
/// it supports safe local object reads when required dependencies need them
/// ([`crate::checkout::NoLazyFetch::of`]: `--no-lazy-fetch`, or `GIT_NO_LAZY_FETCH` it was
/// seen to obey, in a scratch folder of its own).
/// Call outside the materialization lock, with source admission checked again
/// before using the result. This reading grants no package or credential authority.
///
/// # Errors
/// The current account, default profile or private inventory cannot be admitted,
/// or the capability probe cannot run or be set up. Missing or insufficient Git returns false
/// when the selected inventory needs it.
pub fn git_ready_for_inventory(sys: &dyn System, ctx: &Ctx) -> Result<bool, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("repository Git requires exactly one local user".into());
    };
    if sys.euid() == 0 || sys.euid() != user.uid || sys.identity()?.user != *user {
        return Err("repository Git must be read as its current local user".into());
    }
    if !uses_vault_inventory(sys, ctx)? {
        return Err("repository Git no longer uses the private vault inventory".into());
    }
    let listed = inventory::Inventory::read(sys, user)?;
    if listed.repositories().is_empty() {
        return Ok(true);
    }
    if crate::items::github::installed_git_for(sys, Some(Path::new(&user.home))).is_none() {
        return Ok(false);
    }
    if listed
        .repositories()
        .iter()
        .all(|repo| repo.required_submodules.is_empty())
    {
        return Ok(true);
    }
    crate::checkout::NoLazyFetch::of(sys, user).map(|off| off.is_some())
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

/// The local clones of the vault's repository inventory, read offline for the git tuning
/// they converge to ([`crate::clone_tuning`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VaultClones {
    /// init.toml's `[github] enabled = false`: the person's git settings are not aterm's.
    Off,
    /// An explicit `[github]` profile names its own repositories; the vault inventory is left out.
    Profile,
    /// No private inventory from the Secrets vault is on this Mac.
    NoInventory,
    /// Each present clone's worktree top (`~/<name>`): a verified top-level worktree whose only
    /// origin is the inventory's repository, its required submodules ready ([`repos`]' partition).
    Present(Vec<PathBuf>),
}

/// [`VaultClones`] for this context's one user: no GitHub request, credential or write.
///
/// # Errors
/// The local account, init profile, private inventory, or home cannot be admitted.
pub(crate) fn vault_clones(sys: &dyn System, ctx: &Ctx) -> Result<VaultClones, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("repository inventory requires exactly one local user".into());
    };
    let table = profile(sys, user)?.and_then(|profile| profile.github);
    match asked(table, user)? {
        Asked::Off => return Ok(VaultClones::Off),
        Asked::Scoped(_) | Asked::Profile(_) => return Ok(VaultClones::Profile),
        Asked::Default => {}
    }
    match sys.stat(&inventory::file(&user.home)) {
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => {
            return Ok(VaultClones::NoInventory);
        }
        _ => {}
    }
    let listed = inventory::Inventory::read(sys, user)?.into_repositories();
    let standing = repos::partition(sys, user, &listed)?;
    Ok(VaultClones::Present(
        standing.present.into_iter().map(|(_, path)| path).collect(),
    ))
}

/// Reconcile using locally captured inputs, checking their materialization
/// identity after inventory and before effects. The check never leases network work.
/// # Errors
/// As [`run_initial_repositories`], or the captured version is no longer ready.
pub fn run_initial_repositories_captured(
    sys: &dyn System,
    ctx: &Ctx,
    mode: crate::model::Mode,
    work: aterm_secrets::converge::RepositoryWork,
    inputs: InitialRepositoryInputs,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<RepositoryRun, String> {
    native::run_captured(sys, ctx, mode, work, inputs, Box::new(current))
}

/// Reconciles repositories after the selected secrets stage has synchronized.
///
/// # Errors
/// A source, identity, inventory, destination or native operation is unsafe or incomplete.
pub fn run_initial_repositories(
    sys: &dyn System,
    ctx: &Ctx,
    mode: crate::model::Mode,
) -> Result<RepositoryRun, String> {
    native::run(sys, ctx, mode)
}

/// Retire only a captured local repository credential intent. No helper, Git or keychain
/// stage is admitted. The caller must freshly recheck its Stop and repository holds.
/// # Errors
/// The captured choice, account, inventory, caller authority or request changed.
pub fn run_repository_credential_withdrawal(
    sys: &dyn System,
    ctx: &Ctx,
    mode: crate::model::Mode,
    withdrawal: RepositoryCredentialWithdrawal,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<Vec<crate::model::ItemReport>, String> {
    native::run_withdrawal(sys, ctx, mode, withdrawal, Box::new(current))
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
    items_for_work(
        sys,
        ctx,
        secrets,
        aterm_secrets::converge::RepositoryWork::Refresh,
    )
}

/// Build the same default or explicit stage for an owned acquisition/refresh job.
pub(crate) fn items_for_work(
    sys: &dyn System,
    ctx: &Ctx,
    secrets: Option<&str>,
    work: aterm_secrets::converge::RepositoryWork,
) -> Result<Vec<Box<dyn Item>>, String> {
    items_for_execution(sys, ctx, secrets, work, None)
}

pub(crate) fn items_for_execution(
    sys: &dyn System,
    ctx: &Ctx,
    secrets: Option<&str>,
    work: aterm_secrets::converge::RepositoryWork,
    execution: Option<crate::sync::repository_refresh::Execution>,
) -> Result<Vec<Box<dyn Item>>, String> {
    items_for_execution_with_withdrawal_guard(sys, ctx, secrets, work, execution, None)
}

pub(crate) fn items_for_execution_with_withdrawal_guard(
    sys: &dyn System,
    ctx: &Ctx,
    secrets: Option<&str>,
    work: aterm_secrets::converge::RepositoryWork,
    execution: Option<crate::sync::repository_refresh::Execution>,
    withdrawal_current: Option<Box<dyn Fn() -> Result<bool, String> + Send + Sync>>,
) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init GitHub requires exactly one local user".into());
    };
    let withdrawal = RepositoryCredentialWithdrawal::capture(sys, ctx);
    let withdrawal_unread = withdrawal.is_err();
    if let Ok(Some(withdrawal)) = withdrawal {
        return Ok(vec![native::withdrawal_item(
            user,
            withdrawal,
            withdrawal_current,
        )]);
    }
    let profile = profile(sys, user)?.unwrap_or_default();
    let config = match asked(profile.github, user)? {
        Asked::Off => {
            // A failed cleanup observation is a Native blocker, not a failure to build
            // every other setup group and not permission to retire an unknown intent.
            return Ok(if withdrawal_unread {
                vec![native::item(
                    user,
                    secrets.map(str::to_owned),
                    false,
                    work,
                    execution,
                    withdrawal_current,
                )]
            } else {
                Vec::new()
            });
        }
        Asked::Default => {
            return Ok(vec![
                keychain::item(user),
                native::item(
                    user,
                    secrets.map(str::to_owned),
                    true,
                    work,
                    execution,
                    withdrawal_current,
                ),
            ]);
        }
        Asked::Scoped(_) => {
            return Ok(vec![
                keychain::item(user),
                native::item(
                    user,
                    secrets.map(str::to_owned),
                    false,
                    work,
                    execution,
                    withdrawal_current,
                ),
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
            "the GitHub token at {} is on this {} but cannot be used ({why}): it must be a \
             regular file of yours, not a link, that only you can read, of at most {} KiB",
            path.display(),
            sys.machine(),
            INPUT_MAX / 1024
        )),
        _ => None,
    };
    let token = read.ok().map(zeroize::Zeroizing::new);
    let inventory = sys
        .read_owned_file(
            &inventory::file(&user.home),
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

/// Semantic checkout ownership for default refresh, separate from credential authority.
pub(super) fn develop_checkouts(
    sys: &dyn System,
    user: &User,
) -> Result<BTreeSet<PathBuf>, String> {
    let profile = profile(sys, user)?.unwrap_or_default();
    Ok(
        crate::init_develop::recorded(user, profile.develop.as_ref())?
            .into_iter()
            .collect(),
    )
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
                        sys.machine()
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
#[path = "init_github_scripted_tests.rs"]
pub(crate) mod scripted;

#[cfg(test)]
#[path = "init_github_tests.rs"]
mod tests;
