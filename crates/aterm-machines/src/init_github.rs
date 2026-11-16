// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Local GitHub bootstrap: git's HTTPS credential route (aterm's own helper, never the login
//! keychain) and repositories. By default the first launch's own repository stage ([`native`])
//! reads the private account and complete repository inventory from the vault's
//! `github-repositories.json`, beside its development token. init.toml's `[github]` table
//! scopes it to an account and owner, or turns it off; the token is always the vault's.
//! Beside it, in every mode but off, [`keychain`] replaces the keychain block an earlier `aterm
//! init` wrote, read from git's settings before any GitHub answer.

use crate::git_credential::Helper;
use crate::items::{Ctx, Item};
use crate::model::User;
use crate::port::{GithubError, Secret, System};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

mod checkout_inputs;
mod credentials;
mod dependencies;
mod inventory;
mod keychain;
mod native;
mod repos;
mod selection;
mod source;
pub use checkout_inputs::{PassWrites, RepositoryCheckoutMetadata, Wrote};
pub use credentials::{sync_vault_git_credentials, vault_git_credentials};
pub use inventory::{RepositoryCatalog, RepositoryCatalogChanges, RepositoryCatalogVisibility};
pub use native::{InitialRepositoryInputs, RepositoryReadback, RepositoryRun, RepositorySteps};
pub(crate) use native::{automatic_inventory_counts, automatic_inventory_counts_captured};
pub use selection::{RepositoryCredentialWithdrawal, RepositorySelection};
pub use source::{
    RepositoryControlPaths, RepositoryInputMark, RepositoryInputPathCache, RepositoryRefreshCursor,
    repository_checkout_watch_paths, repository_input_control_watch_paths, repository_input_mark,
    repository_input_watch_paths,
};

/// The private vault entries that determine default repository acquisition. Other vault
/// entries must not invalidate a clone whose source and credential remain unchanged.
pub const VAULT_INPUT_ENTRIES: [&str; 2] =
    [agh::identity::vault::Slot::Dev.entry(), inventory::ENTRY];

/// Whether a Git executable is installed ([`crate::items::github::installed_git`]): managed
/// Git is preferred, with developer-tools or system Git as fallbacks. This reads metadata
/// only; [`git_ready_for_inventory`] checks capabilities needed by selected repositories.
#[must_use]
pub fn git_installed(sys: &dyn System) -> bool {
    repos::git_installed(sys)
}

/// A test double's git before 2.45 answering [`git_ready_for_inventory`]'s capability question:
/// it refuses `--no-lazy-fetch`, and its read shows whether it `obeys` `GIT_NO_LAZY_FETCH`
/// ([`crate::checkout::lazy_fetch::before_the_option`]).
#[cfg(any(test, debug_assertions))]
pub use crate::checkout::lazy_fetch::before_the_option as git_before_no_lazy_fetch;

/// Whether `call`, a command as a test double records it ([`crate::port::Fake::calls`]),
/// downloads a repository: a new clone's one fetch of what GitHub advertised, or a required
/// dependency's `git clone`.
#[cfg(any(test, debug_assertions))]
#[must_use]
pub fn acquires_repository(call: &str) -> bool {
    repos::fetches_advertised(call) || call.contains(" clone ")
}

/// Whether the selected private inventory's Git requirements are met. Unlike the
/// metadata-only [`git_installed`], this may ask the selected executable whether
/// it supports safe local object reads when required dependencies need them
/// ([`crate::checkout::lazy_fetch::of`]: `--no-lazy-fetch`, or `GIT_NO_LAZY_FETCH` it was
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
    crate::checkout::lazy_fetch::of(sys, user).map(|off| off.is_some())
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
    Ok(matches!(asked(table)?, Asked::Default))
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
    match asked(table)? {
        Asked::Off => return Ok(VaultClones::Off),
        Asked::Scoped(_) => return Ok(VaultClones::Profile),
        Asked::Default => {}
    }
    if sys
        .vault_entry(Path::new(&user.home), inventory::ENTRY)
        .is_ok_and(|entry| entry.is_none())
    {
        return Ok(VaultClones::NoInventory);
    }
    let listed = inventory::Inventory::read(sys, user)?.into_repositories();
    let standing = repos::partition(sys, user, &listed)?;
    Ok(VaultClones::Present(
        standing.present.into_iter().map(|(_, path)| path).collect(),
    ))
}

/// Reconcile using locally captured inputs, checking their materialization
/// identity after inventory and before effects. The check never leases network work.
/// `steps` names the items the run binds: acquisition, the refresh of what was here before it,
/// or both, acquisition first.
/// # Errors
/// As [`run_initial_repositories`], or the captured version is no longer ready.
pub fn run_initial_repositories_captured(
    sys: &dyn System,
    ctx: &Ctx,
    mode: crate::model::Mode,
    work: aterm_secrets::converge::RepositoryWork,
    steps: RepositorySteps,
    inputs: InitialRepositoryInputs,
    current: impl Fn() -> Result<bool, String> + Send + Sync + 'static,
) -> Result<RepositoryRun, String> {
    native::run_captured(sys, ctx, mode, work, steps, inputs, Box::new(current))
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

/// The vault entry of the GitHub token (the registry's `gh:dev`): the repository stage's one
/// token source, read in place from this Mac's store ([`System::vault_entry`]). It is the
/// entry agh adopts as its dev profile when there is no agh.toml (agh design §6.10).
pub(crate) const TOKEN_ENTRY: &str = agh::identity::vault::Slot::Dev.entry();

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
    /// `account` (and `owner`, else the account): the native stage over that owner's
    /// repositories, signed in as that account.
    Scoped(Github),
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Github {
    pub account: String,
    pub owner: String,
}

/// aterm's own GitHub helper for `user`, which every network git init and the converge pass run
/// carries ([`crate::git_credential::pinned`]): over the vault's token; `None` while `user` has
/// no launcher.
pub(crate) fn helper(sys: &dyn System, user: &User) -> Option<Helper> {
    Helper::vault(sys, user)
}

/// The stage's id, as `aterm init --only` spells it. Each of its items' ids starts with it and
/// a dot ([`names_item`]): `init.github.native@<user>` (acquisition),
/// `init.github.refresh@<user>`, `init.github.keychain@<user>`, and the clone tuning beside
/// them ([`crate::clone_tuning::ID`]).
pub const ID: &str = "init.github";

/// Whether `id` names an item of this stage: [`ID`], a dot, then the item's own name. The
/// stage's own id, which its failure row carries when its items cannot be built, names none.
#[must_use]
pub fn names_item(id: &str) -> bool {
    id.strip_prefix(ID)
        .and_then(|item| item.strip_prefix('.'))
        .is_some_and(|item| !item.is_empty())
}

/// Whether `row` is the keychain item's ([`keychain`]), which no repository step depends on.
pub(crate) fn keychain_row(row: &crate::model::ItemReport) -> bool {
    row.id
        .strip_prefix(ID)
        .is_some_and(|item| item.starts_with(".keychain@"))
}

/// Whether `row` is the refresh of the checkouts already here (`init.github.refresh@<user>`), a
/// row of its own after acquisition: no reading of presence or standing counts it.
#[must_use]
pub fn refresh_row(row: &crate::model::ItemReport) -> bool {
    row.id
        .strip_prefix(ID)
        .is_some_and(|item| item.starts_with(".refresh@"))
}

/// Whether the token aterm's helper ([`helper`]) would hand back is on this machine, read in
/// place from the store as the helper reads it: no GitHub call, and the value goes no further
/// than the wiped buffer this drops. A helper with none answers `quit`, which ends the person's own
/// `git push`.
pub(super) fn token_here(sys: &dyn System, user: &User) -> bool {
    read_token(sys, user).is_ok()
}

/// Whether `user`'s git settings are aterm's to touch: `false` for `[github] enabled = false`.
///
/// # Errors
/// init.toml cannot be read safely, or its `[github]` table cannot be admitted.
pub(super) fn settings_managed(sys: &dyn System, user: &User) -> Result<bool, String> {
    let table = profile(sys, user)?.and_then(|read| read.github);
    Ok(!matches!(asked(table)?, Asked::Off))
}

/// The repository stage `aterm init` runs: the first launch's own ([`native`]) unless init.toml's
/// `[github]` turns it off, its acquisition and then the refresh of what was here before it. It
/// waits for the item `secrets` names, when one does: the secrets vault this run joins, or this
/// Mac waits to be let into, delivers the token entry it reads
/// ([`crate::init_secrets::Secrets::waited_on`]). The keychain item ([`keychain`]) comes first
/// and waits for nothing: it asks GitHub nothing. Run as an apply (`mode`), the stage decides
/// the repository lease before it reads anything, and stands aside while another pass holds it;
/// a plan never takes the lease. `only` is the run's `--only` words (none: every row): where
/// they name the acquisition's row but not the refresh's, the refresh is not built, and the
/// acquisition lets its lease go at its own row's end rather than leave it to a refresh the run
/// would never start ([`RepositorySteps::named_by`]).
///
/// # Errors
/// The local profile cannot be admitted or decoded. Its contents are never quoted.
pub fn items(
    sys: &dyn System,
    ctx: &Ctx,
    secrets: Option<&str>,
    mode: crate::model::Mode,
    only: &[String],
) -> Result<Vec<Box<dyn Item>>, String> {
    let steps = match ctx.users.as_slice() {
        [user] => RepositorySteps::named_by(only, &user.name),
        // `stage_items` refuses a run that does not name exactly one user.
        _ => RepositorySteps::Both,
    };
    stage_items(
        sys,
        ctx,
        secrets,
        mode,
        aterm_secrets::converge::RepositoryWork::Refresh,
        steps,
        None,
        None,
    )
}

/// The same default or explicit stage for an owned job: the items `steps` names, run in `mode`,
/// within the enclosing pass's `execution` where one is given.
#[expect(
    clippy::too_many_arguments,
    reason = "the stage's selection, its run's mode and the enclosing pass's lease"
)]
pub(crate) fn stage_items(
    sys: &dyn System,
    ctx: &Ctx,
    secrets: Option<&str>,
    mode: crate::model::Mode,
    work: aterm_secrets::converge::RepositoryWork,
    steps: RepositorySteps,
    execution: Option<crate::sync::repository_refresh::Execution>,
    withdrawal_current: Option<Box<dyn Fn() -> Result<bool, String> + Send + Sync>>,
) -> Result<Vec<Box<dyn Item>>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("aterm init GitHub requires exactly one local user".into());
    };
    let withdrawal = RepositoryCredentialWithdrawal::capture(sys, ctx);
    let withdrawal_unread = withdrawal.is_err();
    if let Ok(Some(withdrawal)) = withdrawal {
        // A withdrawal retires the credential's checkouts: there is nothing here to refresh.
        return Ok(if steps == RepositorySteps::Refresh {
            Vec::new()
        } else {
            vec![native::withdrawal_item(
                user,
                withdrawal,
                withdrawal_current,
            )]
        });
    }
    let profile = profile(sys, user)?.unwrap_or_default();
    match asked(profile.github)? {
        Asked::Off => {
            // A failed cleanup observation is a Native blocker, not a failure to build
            // every other setup group and not permission to retire an unknown intent.
            Ok(if withdrawal_unread && steps != RepositorySteps::Refresh {
                native::items(
                    user,
                    secrets.map(str::to_owned),
                    false,
                    mode,
                    work,
                    RepositorySteps::Acquisition,
                    execution,
                    withdrawal_current,
                )
            } else {
                Vec::new()
            })
        }
        asked => {
            let default = matches!(asked, Asked::Default);
            // An explicit `[github]` profile's checkouts are left as they are: no refresh.
            let steps = match steps {
                RepositorySteps::Refresh if !default => return Ok(Vec::new()),
                RepositorySteps::Both if !default => RepositorySteps::Acquisition,
                steps => steps,
            };
            let mut items = Vec::new();
            if steps != RepositorySteps::Refresh {
                items.push(keychain::item(user));
            }
            items.extend(native::items(
                user,
                secrets.map(str::to_owned),
                default,
                mode,
                work,
                steps,
                execution,
                withdrawal_current,
            ));
            Ok(items)
        }
    }
}

/// What `sync.clone` reads of the GitHub step an `aterm init` owes ([`crate::init::owed_clone`]):
/// whether the token it reads is on this Mac, and the step's mark.
pub(crate) struct TokenRead {
    /// This Mac's store holds the vault's token ([`TOKEN_ENTRY`]).
    pub(crate) landed: bool,
    /// Why the store's token cannot be read here (its local state is unreadable): what is
    /// wrong, in plain words.
    pub(crate) unusable: Option<String>,
    /// A digest of init.toml, private repository metadata, the token and whether git is
    /// installed, which moves when any of them changes: a clone only a person could finish
    /// waits until it does. The token itself is not kept.
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
    if matches!(asked(table)?, Asked::Off) {
        return Ok(None);
    }
    let read = sys.vault_entry(Path::new(&user.home), TOKEN_ENTRY);
    let unusable = read.as_ref().err().map(|why| {
        format!(
            "the GitHub token (vault entry {TOKEN_ENTRY}) cannot be read from this {}'s vault \
             copy ({why})",
            sys.machine()
        )
    });
    let token = read.ok().flatten();
    let inventory = sys
        .vault_entry(Path::new(&user.home), inventory::ENTRY)
        .ok()
        .flatten()
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

fn decode(bytes: &[u8]) -> Result<Config, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "init.toml must be UTF-8; no configuration contents were printed")?;
    aterm_toml::from_str(text)
        .map_err(|_| "invalid init.toml; expected [github] enabled, account and owner, and [develop] checkouts (the GitHub token is the vault's gh:dev)".into())
}

/// What `table` asks ([`Asked`]), its names and paths checked.
fn asked(table: Option<Table>) -> Result<Asked, String> {
    let Some(table) = table else {
        return Ok(Asked::Default);
    };
    if !table.enabled {
        return Ok(Asked::Off);
    }
    let Some(account) = table.account else {
        return Err("GitHub owner needs an account in init.toml's [github]".into());
    };
    let owner = table.owner.unwrap_or_else(|| account.clone());
    if !login(&account) || !login(&owner) {
        return Err("GitHub account and owner must be valid login names".into());
    }
    Ok(Asked::Scoped(Github { account, owner }))
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

/// What a probe that found the block an earlier `aterm init` wrote says it reads.
pub(super) const KEYCHAIN_BLOCK: &str = "git's global settings hold a GitHub credential block of the shape an earlier aterm init \
     wrote, which reads the login keychain (or, off a Mac, a plain token file)";

/// Why [`read_token`] has no token: the engine's line, and whether the store holds none at all
/// (the vault has not delivered it yet) rather than one that cannot be used.
pub(super) struct TokenUnread {
    /// The store holds no such entry.
    pub(super) absent: bool,
    /// The engine's line; it never quotes the value.
    pub(super) why: String,
}

impl From<TokenUnread> for String {
    fn from(unread: TokenUnread) -> Self {
        unread.why
    }
}

/// The vault's GitHub token ([`TOKEN_ENTRY`]), the stage's one token source, read in place.
fn read_token(sys: &dyn System, user: &User) -> Result<Secret, TokenUnread> {
    crate::port::no_core_dumps();
    let bytes = sys
        .vault_entry(Path::new(&user.home), TOKEN_ENTRY)
        .map_err(|why| TokenUnread {
            absent: false,
            why: format!(
                "the GitHub token (vault entry {TOKEN_ENTRY}) cannot be read from this {}'s vault \
                 copy ({why})",
                sys.machine()
            ),
        })?
        .ok_or_else(|| TokenUnread {
            absent: true,
            why: format!(
                "the GitHub token (vault entry {TOKEN_ENTRY}) is not in this {}'s vault copy yet",
                sys.machine()
            ),
        })?;
    if bytes.len() > INPUT_MAX {
        return Err(TokenUnread {
            absent: false,
            why: "the GitHub token entry is longer than 64 KiB".into(),
        });
    }
    token(bytes.as_slice()).map_err(|why| TokenUnread { absent: false, why })
}

fn token(bytes: &[u8]) -> Result<Secret, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "GitHub token source must be UTF-8")?;
    let value = text.trim();
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
