// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private vault repository inventory and native cloning through [`crate::git_credential`].
//! An explicit init.toml profile can instead ask `agh` for that account's live inventory.

use super::{
    Asked, Github, INPUT_MAX, TokenUnread, asked, decode, get, identity, keychain, read_token,
    repos,
};
use crate::engine::Place;
use crate::git_credential::{Git, Lookups};
use crate::init_observe::{LastReading, Reading, Wait};
use crate::items::{Ctx, Item};
use crate::model::{ItemReport, Mode, Probe, User};
use crate::port::{GithubError, GithubFailure, Secret, System};
use crate::sync::repository_refresh::{Admission, Execution, RepositoryPass, Ticket};
use aterm_converge::{Class, Layer, Remedy, Requires, StoodAside};
use aterm_secrets::converge::RepositoryWork;
use aterm_types::macos_setup::FailureKind;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// The repositories the signed-in account owns, private ones included. Its forks and archived
/// repositories are left out of the default set ([`NativeRepositories::plan`]); an init.toml
/// `[github]` owner widens it.
const ACCESSIBLE: &str =
    "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc";

/// The step a person takes for the GitHub sign-in or settings a repository needs, as the
/// engine names it beside their remedy.
const GITHUB_STEP: &str = "github.sign-in";
/// Preserved checkout work is separate from the credential's sign-in remedy.
const REPOSITORIES_STEP: &str = "github.repositories";

/// What a local read of the private inventory found ([`automatic_inventory_counts`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InventoryCounts {
    /// Repositories present with their required dependencies.
    pub(crate) present: usize,
    /// Matching checkouts without a commit that no current empty-remote proof covers. A local
    /// read cannot tell an empty remote from one with commits: only a read of the remote does.
    pub(crate) unborn: usize,
    /// Repositories the inventory selects.
    pub(crate) total: usize,
}

impl InventoryCounts {
    /// Every selected repository has a matching checkout here, ready or without a commit.
    #[must_use]
    pub(crate) const fn all_here(&self) -> bool {
        self.present + self.unborn == self.total
    }
}

/// A nonempty automatic selection can publish a count with every repository here
/// ([`InventoryCounts::all_here`]) only after the captured default source's pinned credential
/// route also works. Empty selection checks no sign-in.
/// Missing work is left to the native
/// batch's ordinary route check, rather than invoking the helper twice before cloning.
/// # Errors
/// The account, source, checkouts or credential route cannot be read safely, the route
/// does not return the captured token, or the captured source is no longer ready.
pub(crate) fn automatic_inventory_counts(
    sys: &dyn System,
    ctx: &Ctx,
) -> Result<Option<InventoryCounts>, String> {
    if !super::uses_vault_inventory(sys, ctx)? {
        return Ok(None);
    }
    let captured = InitialRepositoryInputs::capture_ready(sys, ctx)?;
    automatic_inventory_counts_captured(sys, ctx, &captured, None)
}

/// Reuse an existing captured default source. A caller's materialization fence is
/// still its responsibility; this checks the capture's own source before returning.
pub(crate) fn automatic_inventory_counts_captured(
    sys: &dyn System,
    ctx: &Ctx,
    captured: &InitialRepositoryInputs,
    readback: Option<&RepositoryReadback>,
) -> Result<Option<InventoryCounts>, String> {
    captured.validate_default_context(sys, ctx)?;
    if captured.local.is_err()
        && let Some(selection) = super::RepositorySelection::capture(sys, ctx)?
        && selection.count() == 0
    {
        captured.validate_selection(sys, ctx)?;
        selection.ensure_current(sys)?;
        return Ok(Some(InventoryCounts {
            present: 0,
            unborn: 0,
            total: 0,
        }));
    }
    let local = captured.local.as_ref().map_err(|block| block.why.clone())?;
    let inventory = local
        .inventory
        .as_ref()
        .ok_or("the default repository source changed while its inputs were captured")?;
    let listed = inventory.repositories();
    let empty = readback
        .filter(|proof| {
            proof.user == captured.user && proof.source.as_ref() == captured.source.as_ref()
        })
        .map_or(&[][..], |proof| proof.empty.as_slice());
    let standing = repos::partition_with_empty(sys, &captured.user, listed, empty)?;
    let counts = InventoryCounts {
        present: standing.present.len(),
        unborn: standing.unborn.len(),
        total: listed.len(),
    };
    // capture_ready released the shared materialization lock before any helper runs.
    let lookups = if !listed.is_empty() && counts.all_here() {
        let helper = super::helper(sys, &captured.user);
        Some(Git::for_user(sys, &captured.user).lookups(
            sys,
            &captured.user,
            helper.as_ref(),
            &local.token,
        )?)
    } else {
        None
    };
    if !captured.is_current_now(sys)? {
        return Err(
            "repository inputs changed or are still syncing; waiting for a new ready snapshot"
                .into(),
        );
    }
    if let Some(Lookups::Not(why)) = lookups {
        return Err(format!(
            "matching worktrees are preserved, but aterm's GitHub route does not hand back the synced token: {why}"
        ));
    }
    Ok(Some(counts))
}

pub(super) fn run(sys: &dyn System, ctx: &Ctx, mode: Mode) -> Result<RepositoryRun, String> {
    run_with(sys, ctx, mode, RepositoryWork::Refresh, None)
}

pub(super) fn run_captured(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    work: RepositoryWork,
    inputs: InitialRepositoryInputs,
    current: Box<dyn Fn() -> Result<bool, String> + Send + Sync>,
) -> Result<RepositoryRun, String> {
    run_with(sys, ctx, mode, work, Some(Captured { inputs, current }))
}

/// `aterm init`'s repository item for `user` with no `[github]` token source in init.toml: the
/// same stage the first launch runs, after the secrets item `after` names when one does (the
/// vault writes the token it reads). With no `[github]` table at all (`unasked`), a Mac with no
/// token is no stage of this run.
pub(super) fn item(
    user: &User,
    after: Option<String>,
    unasked: bool,
    work: RepositoryWork,
    execution: Option<Execution>,
    withdrawal_current: Option<Box<dyn Fn() -> Result<bool, String> + Send + Sync>>,
) -> Box<dyn Item> {
    Box::new(NativeRepositories {
        after,
        unasked,
        work,
        execution,
        local_current: withdrawal_current,
        ..NativeRepositories::new(user.clone(), None)
    })
}

pub(super) fn withdrawal_item(
    user: &User,
    withdrawal: super::RepositoryCredentialWithdrawal,
    current: Option<Box<dyn Fn() -> Result<bool, String> + Send + Sync>>,
) -> Box<dyn Item> {
    Box::new(NativeRepositories {
        local_withdrawal: Some(withdrawal),
        local_current: current,
        ..NativeRepositories::new(user.clone(), None)
    })
}

pub(super) fn run_withdrawal(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    withdrawal: super::RepositoryCredentialWithdrawal,
    current: Box<dyn Fn() -> Result<bool, String> + Send + Sync>,
) -> Result<Vec<ItemReport>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("repository withdrawal requires exactly one local user".into());
    };
    if !withdrawal.belongs_to(user) {
        return Err("the repository withdrawal belongs to another account".into());
    }
    let item = NativeRepositories {
        local_withdrawal: Some(withdrawal),
        local_current: Some(current),
        ..NativeRepositories::new(user.clone(), None)
    };
    let bound = crate::engine::bind(
        [(
            &item as &dyn Item,
            Place {
                layer: Layer::Workstation,
                class: Class::Install,
                requires: Requires::Account,
                remedy: Remedy::Command("aterm init".into()),
            },
        )],
        sys,
        ctx,
    );
    if mode == Mode::Apply {
        let mut journal = sys.journal()?;
        aterm_converge::run(&bound, mode, journal.as_mut())
    } else {
        aterm_converge::run(&bound, mode, &mut aterm_converge::ReadOnly)
    }
    .map_err(|why| why.to_string())
}

fn run_with(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    work: RepositoryWork,
    captured: Option<Captured>,
) -> Result<RepositoryRun, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("initial repository setup requires exactly one local user".into());
    };
    // Candidate observation cannot replace the ordinary Native blocker's typed report.
    // Only a successful local withdrawal reading bypasses the paired repository stage.
    if let Ok(Some(withdrawal)) = super::RepositoryCredentialWithdrawal::capture(sys, ctx) {
        // Preserve the enclosing caller's independent holds even though an empty selection
        // cannot satisfy the ordinary paired token capture.
        let current: Box<dyn Fn() -> Result<bool, String> + Send + Sync> = match captured {
            Some(captured) => captured.current,
            None => Box::new(|| Ok(true)),
        };
        return run_withdrawal(sys, ctx, mode, withdrawal, current).map(|rows| RepositoryRun {
            rows,
            readback: None,
        });
    }
    if mode == Mode::Apply {
        // What a clone killed mid-way left (design §6.5): never a clone still running.
        repos::sweep_staging(sys, user);
    }
    // The keychain block goes first, whatever GitHub says below.
    let keychain = keychain::Keychain::new(user.clone());
    let item = NativeRepositories {
        work,
        observe_remote: mode == Mode::Apply || work == RepositoryWork::Refresh,
        ..NativeRepositories::new(user.clone(), captured)
    };
    let place = || Place {
        layer: Layer::Workstation,
        class: Class::Install,
        requires: Requires::Account,
        remedy: Remedy::Command("aterm init".into()),
    };
    let bound = crate::engine::bind(
        [
            (&keychain as &dyn Item, place()),
            (&item as &dyn Item, place()),
        ],
        sys,
        ctx,
    );
    let rows = if mode == Mode::Apply {
        let mut journal = sys.journal()?;
        aterm_converge::run(&bound, mode, journal.as_mut())
    } else {
        aterm_converge::run(&bound, mode, &mut aterm_converge::ReadOnly)
    }
    .map_err(|why| why.to_string())?;
    let readback = if mode == Mode::Apply {
        item.readback(sys)?
    } else {
        None
    };
    Ok(RepositoryRun { rows, readback })
}

/// Native rows and the current batch's optional proof of truly empty origins.
#[derive(Debug)]
pub struct RepositoryRun {
    pub rows: Vec<ItemReport>,
    pub readback: Option<RepositoryReadback>,
}

impl std::ops::Deref for RepositoryRun {
    type Target = [ItemReport];
    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl IntoIterator for RepositoryRun {
    type Item = ItemReport;
    type IntoIter = std::vec::IntoIter<ItemReport>;
    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_iter()
    }
}

/// Readback of complete empty remote advertisements. It lives only in the current
/// item/pass; every reuse still checks current inputs and actual local storage.
pub struct RepositoryReadback {
    user: User,
    source: Option<super::source::Stamp>,
    empty: Vec<repos::EmptyCheckout>,
}

impl std::fmt::Debug for RepositoryReadback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryReadback").finish_non_exhaustive()
    }
}

struct NativeRepositories {
    user: User,
    /// The caller's scheduling intent; it grants no authority over the captured source.
    work: RepositoryWork,
    /// An apply acquires missing work; a Refresh also admits read-only remote observation.
    /// A Plan/Repair readback alone cannot turn unseen remote state into missing work.
    observe_remote: bool,
    execution: Option<Execution>,
    refresh: Mutex<Option<ActiveRefresh>>,
    lease: Mutex<Option<Arc<RepositoryPass>>>,
    /// How many repositories the last probe found and whether it signed in, for
    /// System State.
    reading: LastReading,
    captured: Option<Captured>,
    /// Ordinary init captures only after its Secrets dependency has run. A successful
    /// generation stays fixed; an unready observation is retried rather than cached.
    prepared: OnceLock<InitialRepositoryInputs>,
    /// The secrets item it waits for in `aterm init` ([`item`]).
    after: Option<String>,
    /// `aterm init`'s default, which nobody configured: no token on this Mac is no stage here,
    /// not a person's step.
    unasked: bool,
    /// The apply reuses the probe's inventory and verdicts. Completion reuses only the
    /// inventory: every checkout and required dependency is read again after the batch.
    carried: Mutex<Option<Carried>>,
    /// What the last apply's clones made of their hook installers ([`repos::Cloned::hooks`]),
    /// which the read-back's row says.
    hooks: Mutex<Vec<String>>,
    selection: Mutex<Option<super::RepositorySelection>>,
    aside: Mutex<Option<StoodAside>>,
    acknowledge: Mutex<bool>,
    withdrawal: Mutex<Option<super::RepositoryCredentialWithdrawal>>,
    local_withdrawal: Option<super::RepositoryCredentialWithdrawal>,
    local_current: Option<Box<dyn Fn() -> Result<bool, String> + Send + Sync>>,
}

/// What one step read for the next ([`NativeRepositories::carried`]).
enum Carried {
    /// The probe's inventory and verdicts, for the apply.
    Probed(Plan, repos::Partition),
    /// The apply's inventory, whose current local readiness the read-back verifies.
    Applied(Plan),
}

/// One local GitHub profile/token reading or its typed blocker, without contacting
/// GitHub. Its secret is scrubbed when the final owner drops it; it is never part
/// of a command.
#[derive(Clone)]
pub struct InitialRepositoryInputs {
    user: User,
    local: Result<LocalInputs, Block>,
    source: Option<super::source::Stamp>,
    authority: InputAuthority,
    execution: Option<Execution>,
    readback: Option<Arc<RepositoryReadback>>,
}

struct ActiveRefresh {
    pass: Arc<RepositoryPass>,
    ticket: Ticket,
}

#[derive(Clone)]
enum InputAuthority {
    /// An external caller holds and rechecks its own ready source snapshot.
    Caller,
    /// This default source was deliberately local: a newly configured vault is a new source.
    Local(aterm_secrets::client::Paths),
    Vault(
        aterm_secrets::client::Paths,
        Box<aterm_secrets::client::MaterializationSnapshot>,
    ),
}

#[derive(Clone)]
struct LocalInputs {
    configured: Option<Github>,
    token: Arc<Secret>,
    inventory: Option<super::inventory::Inventory>,
}

struct Captured {
    inputs: InitialRepositoryInputs,
    current: Box<dyn Fn() -> Result<bool, String> + Send + Sync>,
}

impl InitialRepositoryInputs {
    /// The same opaque scheduling identity used by the local-input observer.
    /// It grants no permission over the captured files or a later Git effect.
    /// # Errors
    /// The account, default source or local scheduling inputs cannot be admitted.
    pub fn refresh_scope(
        &self,
        sys: &dyn System,
    ) -> Result<aterm_secrets::converge::RefreshScope, String> {
        if self.user.uid != sys.euid() || self.user.uid == 0 {
            return Err("repository refresh belongs to this login alone".into());
        }
        let local = self.local.as_ref().map_err(|block| block.why.clone())?;
        if local.configured.is_some() {
            return Err("explicit repository profiles use their own apply policy".into());
        }
        super::source::refresh_scope(
            sys,
            &self.user,
            self.source
                .as_ref()
                .ok_or("repository input source is not ready")?,
        )
    }
    /// Reuse the enclosing pass's one alignment lease. This supplies scheduling
    /// intent alone; all captured-input and effect admission remains unchanged.
    #[must_use]
    pub fn with_repository_execution(mut self, execution: Option<Execution>) -> Self {
        self.execution = execution;
        self
    }

    /// Borrow this pass's actual empty-origin observation for a fresh local readback.
    /// Matching source/account and local checkout checks still decide whether it applies.
    #[must_use]
    pub fn with_repository_readback(mut self, readback: Option<Arc<RepositoryReadback>>) -> Self {
        self.readback = readback;
        self
    }
    /// Read only local admitted profile/token files, for a caller holding its
    /// vault's ready materialization against writers. A blocked local reading is
    /// kept for the item's probe to report without reading those files again.
    /// # Errors
    /// The context does not name exactly one local user.
    pub fn capture(sys: &dyn System, ctx: &Ctx) -> Result<Self, String> {
        let [user] = ctx.users.as_slice() else {
            return Err("initial repository setup requires exactly one local user".into());
        };
        Ok(Self::local(sys, user))
    }

    /// Capture an ordinary account's inputs through its configured local vault when the
    /// default repository source is used. This runs lazily after the Secrets dependency;
    /// explicitly configured personal profiles continue to use their own local source.
    ///
    /// # Errors
    /// The context does not name exactly one local account. Unready inputs stay typed blockers.
    pub fn capture_ready(sys: &dyn System, ctx: &Ctx) -> Result<Self, String> {
        let [user] = ctx.users.as_slice() else {
            return Err("initial repository setup requires exactly one local user".into());
        };
        Ok(Self::ready(sys, user))
    }

    fn local(sys: &dyn System, user: &User) -> Self {
        let before = super::source::Stamp::read(sys, user);
        let local = Self::read(sys, user);
        let read = local.and_then(|local| {
            let before = before.map_err(Block::from)?;
            let after = super::source::Stamp::read(sys, user).map_err(Block::from)?;
            if before != after {
                return Err("repository inputs changed while they were read; waiting for a coherent version".to_owned().into());
            }
            Ok((local, after))
        });
        let (local, source) = match read {
            Ok((local, source)) => (Ok(local), Some(source)),
            Err(why) => (Err(why), None),
        };
        Self {
            user: user.clone(),
            local,
            source,
            authority: InputAuthority::Caller,
            execution: None,
            readback: None,
        }
    }

    fn ready(sys: &dyn System, user: &User) -> Self {
        let failed = |why: String| Self {
            user: user.clone(),
            local: Err(why.into()),
            source: None,
            authority: InputAuthority::Caller,
            execution: None,
            readback: None,
        };
        let table = match super::profile(sys, user) {
            Ok(profile) => profile.and_then(|profile| profile.github),
            Err(why) => {
                let mut inputs = failed(why.clone());
                inputs.local = Err(Block::of(Wait::Settings)(why));
                return inputs;
            }
        };
        let default = match super::asked(table, user) {
            Ok(Asked::Default) => true,
            Ok(_) => false,
            Err(why) => {
                let mut inputs = failed(why.clone());
                inputs.local = Err(Block::of(Wait::Settings)(why));
                return inputs;
            }
        };
        if !default {
            let inputs = Self::local(sys, user);
            if inputs
                .local
                .as_ref()
                .is_ok_and(|local| local.configured.is_none())
            {
                return failed("the repository source changed while it was admitted".into());
            }
            return inputs;
        }
        let paths = aterm_secrets::client::Paths::for_home(Path::new(&user.home));
        let configured = match aterm_secrets::client::config(&paths) {
            Ok(Some(configured)) => configured,
            Ok(None) => {
                let mut inputs = Self::local(sys, user);
                if inputs
                    .local
                    .as_ref()
                    .is_ok_and(|local| local.configured.is_some())
                {
                    return failed("the repository source changed while it was admitted".into());
                }
                inputs.authority = InputAuthority::Local(paths);
                return inputs;
            }
            Err(why) => return failed(why.to_string()),
        };
        match aterm_secrets::client::with_materialization_ready_for_entries(
            &paths,
            &configured,
            &super::VAULT_INPUT_ENTRIES,
            || Ok(Self::local(sys, user)),
        ) {
            Ok(Some(captured)) => {
                let mut inputs = captured.inputs;
                if inputs
                    .local
                    .as_ref()
                    .is_ok_and(|local| local.configured.is_some())
                {
                    return failed("the repository source changed while it was admitted".into());
                }
                inputs.authority = InputAuthority::Vault(paths, Box::new(captured.snapshot));
                inputs
            }
            Ok(None) => {
                failed("the configured secrets vault is not ready for repository inputs".into())
            }
            Err(why) => failed(why.to_string()),
        }
    }

    /// Whether the selected source and its captured inputs still hold. This observes only
    /// local files and ready-vault evidence, and grants no lease over later effects.
    ///
    /// # Errors
    /// The current local source or vault state cannot be safely read.
    pub fn is_current(&self, sys: &dyn System) -> Result<bool, String> {
        self.current_with(
            sys,
            aterm_secrets::client::MaterializationSnapshot::is_current,
        )
    }

    pub(crate) fn is_current_now(&self, sys: &dyn System) -> Result<bool, String> {
        self.current_with(
            sys,
            aterm_secrets::client::MaterializationSnapshot::is_current_now,
        )
    }

    /// A captured local failure is a blocker, not evidence that the vault changed.
    /// It can explain unfinished readback but cannot authorize acquisition or readiness.
    pub(crate) fn local_blocker(&self) -> Option<&str> {
        self.local.as_ref().err().map(|block| block.why.as_str())
    }

    /// An admitted default selection with no repository work. This supplies no
    /// credential-route or source-currency evidence; callers retain those fences.
    pub(crate) fn selects_no_repositories(&self) -> bool {
        self.local.as_ref().is_ok_and(|local| {
            local
                .inventory
                .as_ref()
                .is_some_and(|inventory| inventory.repositories().is_empty())
        })
    }

    pub(crate) fn validate_default_context(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
    ) -> Result<(), String> {
        if ctx.users.as_slice() != std::slice::from_ref(&self.user)
            || self.user.uid != sys.euid()
            || self.user.uid == 0
        {
            return Err("the captured repository account changed".into());
        }
        if !super::uses_vault_inventory(sys, ctx)?
            || self
                .local
                .as_ref()
                .is_ok_and(|local| local.configured.is_some())
        {
            return Err("the captured default repository source changed".into());
        }
        Ok(())
    }

    pub(crate) fn validate_selection(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.validate_default_context(sys, ctx)?;
        if self.local.is_ok() && !self.is_current_now(sys)? {
            return Err("captured repository inputs changed before selection readback".into());
        }
        Ok(())
    }

    fn current_with(
        &self,
        sys: &dyn System,
        materialization_current: fn(
            &aterm_secrets::client::MaterializationSnapshot,
            &aterm_secrets::client::Paths,
        ) -> aterm_secrets::Result<bool>,
    ) -> Result<bool, String> {
        if self.local.is_err() {
            return Ok(false);
        }
        match &self.authority {
            InputAuthority::Caller => {}
            InputAuthority::Local(paths) => {
                if aterm_secrets::client::config(paths)
                    .map_err(|why| why.to_string())?
                    .is_some()
                {
                    return Ok(false);
                }
            }
            InputAuthority::Vault(paths, snapshot) => {
                if !materialization_current(snapshot, paths).map_err(|why| why.to_string())? {
                    return Ok(false);
                }
            }
        }
        self.source
            .as_ref()
            .map_or(Ok(false), |source| source.is_current(sys, &self.user))
    }

    fn read(sys: &dyn System, user: &User) -> Result<LocalInputs, Block> {
        crate::port::no_core_dumps();
        if sys.euid() != user.uid {
            return Err(String::from("repository setup must run as its local user").into());
        }
        repos::admit_destination(sys, user)?;
        let configured = configuration(sys, user).map_err(Block::of(Wait::Settings))?;
        let source = configured.clone().unwrap_or_else(|| Github {
            account: String::new(),
            owner: String::new(),
            token_file: Path::new(&user.home).join(super::TOKEN_FILE),
            token_key: None,
        });
        let token = Arc::new(read_token(sys, user, &source).map_err(Block::token)?);
        let inventory = if configured.is_none() {
            Some(
                super::inventory::Inventory::read(sys, user)
                    .map_err(Block::of(Wait::RepositoryInventory))?,
            )
        } else {
            None
        };
        if inventory
            .as_ref()
            .is_none_or(|inventory| !inventory.repositories().is_empty())
            && !repos::git_installed(sys)
        {
            return Err(Block::of(Wait::GitReadiness)(repos::no_git(sys).into()));
        }
        Ok(LocalInputs {
            configured,
            token,
            inventory,
        })
    }
}

impl NativeRepositories {
    /// Capture this item's ready generation lazily, including a direct apply.
    /// Blocked observations remain retryable rather than filling the ready slot.
    fn prepared_inputs(&self, sys: &dyn System) -> InitialRepositoryInputs {
        self.prepared.get().cloned().unwrap_or_else(|| {
            let inputs = InitialRepositoryInputs::ready(sys, &self.user);
            if inputs.local.is_ok() {
                let _ = self.prepared.set(inputs.clone());
            }
            inputs
        })
    }

    fn new(user: User, captured: Option<Captured>) -> Self {
        Self {
            user,
            work: RepositoryWork::Refresh,
            observe_remote: true,
            execution: None,
            refresh: Mutex::new(None),
            lease: Mutex::new(None),
            reading: LastReading::default(),
            captured,
            prepared: OnceLock::new(),
            after: None,
            unasked: false,
            carried: Mutex::new(None),
            hooks: Mutex::new(Vec::new()),
            selection: Mutex::new(None),
            aside: Mutex::new(None),
            acknowledge: Mutex::new(false),
            withdrawal: Mutex::new(None),
            local_withdrawal: None,
            local_current: None,
        }
    }

    /// The inventory and verdicts the step before left for this one, taken.
    fn take(&self) -> Option<Carried> {
        self.carried
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn carry(&self, carried: Carried) {
        *self.carried.lock().unwrap_or_else(PoisonError::into_inner) = Some(carried);
    }

    /// Whether aterm's route hands back `plan`'s token ([`Git::lookups`]).
    fn lookups(&self, sys: &dyn System, plan: &Plan) -> Result<Lookups, String> {
        let helper = super::helper(sys, &self.user);
        Git::for_user(sys, &self.user).lookups(sys, &self.user, helper.as_ref(), &plan.token)
    }

    /// Read current local readiness against the apply's admitted inventory, or a new plan.
    fn read(&self, sys: &dyn System) -> Result<(Plan, repos::Partition), Block> {
        let plan = match self.take() {
            Some(Carried::Applied(plan) | Carried::Probed(plan, _)) => {
                self.ensure_current(sys).map_err(Block::from)?;
                plan
            }
            _ => self.plan(sys)?,
        };
        let standing =
            repos::partition_with_empty(sys, &self.user, &plan.repositories, &plan.empty)
                .map_err(Block::of(Wait::InTheWay))?;
        Ok((plan, standing))
    }

    fn readback(&self, sys: &dyn System) -> Result<Option<RepositoryReadback>, String> {
        let empty = {
            let carried = self.carried.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(Carried::Probed(plan, standing)) = carried.as_ref() else {
                return Ok(None);
            };
            if plan.empty.is_empty() || standing.present.len() != plan.repositories.len() {
                return Ok(None);
            }
            plan.empty.clone()
        };
        self.ensure_current(sys)?;
        let inputs = match &self.captured {
            Some(captured) => captured.inputs.clone(),
            None => self.prepared_inputs(sys),
        };
        let Some(source) = inputs.source else {
            return Ok(None);
        };
        Ok(Some(RepositoryReadback {
            user: self.user.clone(),
            source: Some(source),
            empty,
        }))
    }
}

impl Item for NativeRepositories {
    fn id(&self) -> String {
        format!("init.github.native@{}", self.user.name)
    }
    fn family(&self) -> &'static str {
        "github"
    }
    fn title(&self) -> String {
        "GitHub repositories from synced development credentials".into()
    }
    fn after(&self) -> Vec<String> {
        self.after.iter().cloned().collect()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        *self
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        *self
            .acknowledge
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = false;
        let withdrawal = self
            .withdrawal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        // What the last apply's clones made of their hook installers: whatever this read says,
        // the switch turned off since included, its row says that too.
        let hooks = std::mem::take(&mut *self.hooks.lock().unwrap_or_else(PoisonError::into_inner));
        let read = (|| -> Probe {
            self.reading.set(None);
            if let Some(withdrawal) = &self.local_withdrawal {
                return self.withdrawal_probe(sys, withdrawal.clone());
            }
            if let Some(withdrawal) = withdrawal {
                return self.withdrawal_probe(sys, withdrawal);
            }
            if matches!(super::settings_managed(sys, &self.user), Ok(false)) {
                match super::RepositoryCredentialWithdrawal::capture(sys, _ctx) {
                    Ok(Some(withdrawal)) => return self.withdrawal_probe(sys, withdrawal),
                    Err(why) => return Probe::Blocked { why },
                    Ok(None) => {}
                }
                return Probe::NotApplicable {
                    why: "repository setup is disabled in init.toml's [github] table".into(),
                };
            }
            let paired = self.read(sys);
            if paired.is_err()
                && self.captured.as_ref().map_or_else(
                    || self.prepared.get().is_none(),
                    |captured| captured.inputs.local.is_err(),
                )
            {
                match super::RepositorySelection::for_user(sys, &self.user) {
                    Ok(Some(selection)) => {
                        if let Err(why) = self.ensure_current(sys) {
                            return Probe::Blocked { why };
                        }
                        if selection.count() == 0 {
                            match super::RepositoryCredentialWithdrawal::from_empty(sys, selection)
                            {
                                Ok(Some(withdrawal)) => {
                                    return self.withdrawal_probe(sys, withdrawal);
                                }
                                Err(why) => return Probe::Blocked { why },
                                Ok(None) => {}
                            }
                            return Probe::NotApplicable {
                            why: "the private inventory selects no repositories; GitHub sign-in was not checked".into(),
                        };
                        }
                        match selection.needs_credential(sys) {
                            Ok(true) => {
                                let from = selection.waiting();
                                *self
                                    .selection
                                    .lock()
                                    .unwrap_or_else(PoisonError::into_inner) = Some(selection);
                                return Probe::Change {
                                from,
                                to: "record the development credential need in the existing vault protocol".into(),
                            };
                            }
                            Ok(false) => {}
                            Err(why) => return Probe::Blocked { why },
                        }
                    }
                    Ok(None) => {}
                    Err(block) => {
                        self.reading.set(block.wait.map(Reading::Waits));
                        return block.probe(self.unasked, sys.machine());
                    }
                }
            }
            let check = (|| {
                let (mut plan, mut standing) = paired?;
                if plan.empty_inventory {
                    self.ensure_current(sys).map_err(Block::from)?;
                    return Ok((plan, standing, None));
                }
                if let Some(why) = repos::named(&standing.git_waits) {
                    return Err(Block::of(Wait::GitReadiness)(why));
                }
                let lookups = self.lookups(sys, &plan)?;
                self.ensure_current(sys).map_err(Block::from)?;
                if lookups.ready() && self.observe_remote && !standing.unborn.is_empty() {
                    let recorded =
                        super::develop_checkouts(sys, &self.user).map_err(Block::from)?;
                    let mut preserved = Vec::new();
                    for candidate in &standing.unborn {
                        plan.empty.retain(|proof| !proof.is_for(&candidate.0));
                        match repos::observe_unborn(sys, &self.user, candidate, &recorded, &|| {
                            self.ensure_current(sys)
                        })
                        .map_err(|why| Block::of(Wait::Unreached)(why.to_string()))?
                        {
                            repos::UnbornObservation::Empty(empty) => plan.empty.push(*empty),
                            repos::UnbornObservation::Owed => {}
                            repos::UnbornObservation::Preserved(why) => {
                                preserved.push((candidate.1.clone(), why))
                            }
                        }
                    }
                    standing = repos::partition_with_empty(
                        sys,
                        &self.user,
                        &plan.repositories,
                        &plan.empty,
                    )
                    .map_err(Block::of(Wait::InTheWay))?;
                    standing
                        .unborn
                        .retain(|(_, path)| !preserved.iter().any(|(kept, _)| kept == path));
                    standing
                        .notes
                        .extend(preserved.into_iter().map(|(_, why)| why));
                    self.ensure_current(sys).map_err(Block::from)?;
                } else if lookups.ready() && !standing.unborn.is_empty() {
                    let recorded =
                        super::develop_checkouts(sys, &self.user).map_err(Block::from)?;
                    let mut preserved = Vec::new();
                    for (repo, path) in &standing.unborn {
                        let why = repos::local_unborn(
                            sys,
                            &self.user,
                            &(repo.clone(), path.clone()),
                            &recorded,
                        )
                        .map_err(Block::from)?;
                        if let Some(why) = why {
                            preserved.push((path.clone(), format!("~/{}: {why}", repo.name)));
                        }
                    }
                    standing
                        .unborn
                        .retain(|(_, path)| !preserved.iter().any(|(kept, _)| kept == path));
                    standing
                        .notes
                        .extend(preserved.into_iter().map(|(_, why)| why));
                    self.ensure_current(sys).map_err(Block::from)?;
                }
                // Signed in, with no repository left that fits: the row waits for room instead of
                // a pass that would clone nothing.
                let volume = sys.volume(Path::new(&self.user.home));
                let left: Vec<String> = standing
                    .missing
                    .iter()
                    .filter_map(|repo| repos::no_room(volume.as_ref(), repo))
                    .collect();
                if lookups.ready()
                    && !standing.missing.is_empty()
                    && standing.dependencies.is_empty()
                    && left.len() == standing.missing.len()
                {
                    let why = repos::short_of_room(volume.as_ref(), &left, CHECK_AGAIN)
                        .unwrap_or_default();
                    return Err(Block::of(Wait::DiskSpace)(why));
                }
                Ok::<_, Block>((plan, standing, Some(lookups)))
            })();
            let (plan, standing, lookups) = match check {
                Ok(read) => read,
                Err(block) => {
                    // Why it cannot go on, where this probe decided it: System State's note.
                    self.reading.set(block.wait.map(Reading::Waits));
                    return block.probe(self.unasked, sys.machine());
                }
            };
            let Some(lookups) = lookups else {
                match super::RepositoryCredentialWithdrawal::capture(sys, _ctx) {
                    Ok(Some(withdrawal)) => return self.withdrawal_probe(sys, withdrawal),
                    Err(why) => return Probe::Blocked { why },
                    Ok(None) => {}
                }
                return Probe::NotApplicable {
                    why: "the private inventory selects no repositories; GitHub sign-in was not checked"
                        .into(),
                };
            };
            let ready = lookups.ready();
            let count = plan.repositories.len();
            let present = standing.present.len();
            let missing = count.saturating_sub(present);
            self.reading.set(Some(Reading::Repositories {
                present,
                count,
                signed_in: ready,
            }));
            let mut notes = plan.notes.clone();
            notes.extend(standing.notes.iter().cloned());
            let probe = if let Lookups::Not(why) = &lookups {
                Probe::Blocked {
                    why: format!(
                        "{missing} of {count} repositories missing, and aterm's GitHub route does \
                     not hand back the synced token: {why}; aterm never keeps the token in the \
                     login keychain"
                    ),
                }
            } else if standing.missing.is_empty()
                && standing.dependencies.is_empty()
                && standing.unborn.is_empty()
                && !standing.notes.is_empty()
            {
                Probe::Person {
                    step: REPOSITORIES_STEP.into(),
                    how: format!(
                        "{present} of {count} repositories ready; preserved work or dependency metadata blocks completion: {}",
                        repos::named(&standing.notes).unwrap_or_default()
                    ),
                }
            } else if missing == 0 {
                let mut now = format!(
                    "{count} matching worktrees present; aterm's GitHub route hands back the synced \
                 token"
                );
                if let Some(left) = repos::named(&notes) {
                    now.push_str(&format!("; left as they are: {left}"));
                }
                Probe::Ok { now }
            } else {
                Probe::Change {
                from: format!("{missing} of {count} repositories missing or needing required dependencies"),
                to: "clone missing repositories and their required dependencies through aterm's GitHub route; preserve existing work"
                    .into(),
            }
            };
            let probe = if matches!(probe, Probe::Ok { .. }) {
                match super::RepositorySelection::acknowledgment_due(sys, &self.user) {
                    Ok(true) => {
                        *self
                            .acknowledge
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) =
                            self.work == RepositoryWork::Repair;
                        Probe::Change {
                            from: "the delivered development credential has no consumer acknowledgment yet".into(),
                            to: "confirm the paired credential route under this pass's repository intent".into(),
                        }
                    }
                    Ok(false) => probe,
                    Err(why) => Probe::Blocked { why },
                }
            } else {
                probe
            };
            self.carry(Carried::Probed(plan, standing));
            probe
        })();
        let read = repos::with_hooks(read, &hooks);
        if let Err(why) = self.finish_refresh(sys, matches!(read, Probe::Ok { .. })) {
            return Probe::Blocked { why };
        }
        read
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.apply_guarded(sys, ctx, &|| Ok(()))
    }

    fn refresh_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        if self.work != RepositoryWork::Refresh {
            return None;
        }
        if let Err(why) = current().and_then(|()| self.ensure_current(sys)) {
            return Some(Err(why));
        }
        match configuration(sys, &self.user) {
            Ok(None) => Some(self.apply_guarded(sys, ctx, current)),
            Ok(Some(_)) => None,
            Err(why) => Some(Err(why)),
        }
    }

    fn refresh_changes(&self, sys: &dyn System, _ctx: &Ctx) -> bool {
        self.local_withdrawal.is_none()
            && self.work == RepositoryWork::Refresh
            && matches!(configuration(sys, &self.user), Ok(None))
    }

    fn apply_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        authorized()?;
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let withdrawal = self
            .withdrawal
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(withdrawal) = withdrawal {
            return withdrawal.retire(sys, &|| {
                authorized()?;
                self.withdrawal_current()
            });
        }
        if self.local_withdrawal.is_some() {
            return Err("the local credential withdrawal was not admitted by its probe".into());
        }
        self.ensure_current(sys)?;
        let selection = self
            .selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let selection = match selection {
            Some(selection) => Some(selection),
            None if self.captured.as_ref().map_or_else(
                || self.prepared_inputs(sys).local.is_err(),
                |captured| captured.inputs.local.is_err(),
            ) =>
            {
                super::RepositorySelection::for_user(sys, &self.user).map_err(|block| block.why)?
            }
            None => None,
        };
        if let Some(selection) = selection {
            selection.ensure_current(sys)?;
            authorized()?;
            if selection.count() == 0 {
                return Ok(());
            }
            if selection.needs_credential(sys)? {
                let current = || {
                    authorized()?;
                    self.ensure_current(sys)
                };
                let _ = selection.request_credential(sys, &current)?;
                *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(StoodAside::Waiting(selection.waiting()));
                return Ok(());
            }
        }
        if std::mem::take(
            &mut *self
                .acknowledge
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        ) {
            let Some(Carried::Probed(plan, _)) = self.take() else {
                return Err("credential acknowledgment has no paired consumer reading".into());
            };
            let current = || {
                authorized()?;
                self.ensure_current(sys)
            };
            current()?;
            if let Lookups::Not(why) = self.lookups(sys, &plan)? {
                return Err(why);
            }
            current()?;
            self.acknowledge_credential(sys, &plan.token, &current)?;
            current()?;
            self.carry(Carried::Applied(plan));
            return Ok(());
        }
        let work = self.begin_refresh(sys)?;
        let result = self.apply_batch(sys, ctx, authorized, work);
        if result.is_err() {
            self.finish_refresh(sys, false)?;
        }
        result
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }
}

impl NativeRepositories {
    fn withdrawal_current(&self) -> Result<(), String> {
        if let Some(current) = &self.local_current
            && !current()?
        {
            return Err(
                "repository credential withdrawal is held by the current setup choices".into(),
            );
        }
        if let Some(captured) = &self.captured
            && !(captured.current)()?
        {
            return Err("the captured repository source no longer admits local withdrawal".into());
        }
        Ok(())
    }

    fn withdrawal_probe(
        &self,
        sys: &dyn System,
        withdrawal: super::RepositoryCredentialWithdrawal,
    ) -> Probe {
        let pending = self
            .withdrawal_current()
            .and_then(|()| withdrawal.pending(sys));
        match pending {
            Ok(true) => {
                *self
                    .withdrawal
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(withdrawal);
                Probe::Change {
                    from: "the default repository credential request is no longer needed".into(),
                    to: "retire the request without checking GitHub sign-in".into(),
                }
            }
            Ok(false) => Probe::Ok {
                now: "no repository credential request remains to retire; GitHub sign-in was not checked"
                    .into(),
            },
            Err(why) => Probe::Blocked { why },
        }
    }

    fn acknowledge_credential(
        &self,
        sys: &dyn System,
        token: &Secret,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        current()?;
        if !super::RepositorySelection::consumer_ack_due(sys, &self.user)? {
            return Ok(());
        }
        if let Some(selection) =
            super::RepositorySelection::for_user(sys, &self.user).map_err(|block| block.why)?
        {
            selection.credential_satisfied(sys, token, current)?;
        }
        Ok(())
    }

    fn apply_batch(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
        work: RepositoryWork,
    ) -> Result<(), String> {
        authorized()?;
        self.ensure_current(sys)?;
        if !super::settings_managed(sys, &self.user)? {
            return Ok(());
        }
        let (mut plan, mut standing) = match self.take() {
            Some(Carried::Probed(plan, standing)) => (plan, standing),
            _ => {
                let plan = self.plan(sys).map_err(|block| {
                    // Why it stopped, where this step decided it: the failed row keeps it for
                    // System State's page (`init_observe::failure_note`), the engine's line for
                    // the log.
                    if let Some(wait) = block.wait {
                        self.reading.set(Some(Reading::Waits(wait)));
                    }
                    block.why
                })?;
                let standing = repos::partition(sys, &self.user, &plan.repositories)?;
                (plan, standing)
            }
        };
        let recorded = super::develop_checkouts(sys, &self.user)?;
        let current = || {
            authorized()?;
            if !super::settings_managed(sys, &self.user)? {
                return Err("repository setup was disabled in init.toml while cloning".into());
            }
            self.ensure_current(sys)?;
            if recorded != super::develop_checkouts(sys, &self.user)? {
                return Err(
                    "develop checkout selection changed while repositories were being refreshed"
                        .into(),
                );
            }
            Ok(())
        };
        current()?;
        if plan.empty_inventory {
            self.carry(Carried::Applied(plan));
            return Ok(());
        }
        if let Some(why) = repos::named(&standing.git_waits) {
            self.reading.set(Some(Reading::Waits(Wait::GitReadiness)));
            return Err(why);
        }
        if let Lookups::Not(why) = self.lookups(sys, &plan)? {
            return Err(format!(
                "aterm's GitHub route does not hand back the synced token: {why}; no repository \
                 was cloned"
            ));
        }
        // Show locally verified checkouts alongside acquisitions. These observations
        // describe this selected inventory; they authorize no repository effect.
        for (repo, path) in &standing.present {
            let mut progress = crate::repository_progress::Progress::new(&repo.name, path.clone());
            progress.placed = true;
            progress.usable = true;
            progress.enter(crate::repository_progress::Phase::Ready);
            sys.repository_progress(&progress);
        }
        let mut failed = Vec::new();
        let mut network_failed = false;
        // A fresh Refresh probe has already observed its empty origins; repair uses only
        // this item's actual readback. A newly constructed item has neither observation.
        if !standing.unborn.is_empty() {
            standing =
                repos::partition_with_empty(sys, &self.user, &plan.repositories, &plan.empty)?;
            if !standing.unborn.is_empty() {
                self.retain_repository_lease(sys)?;
            }
            for candidate in &standing.unborn {
                plan.empty.retain(|proof| !proof.is_for(&candidate.0));
                match repos::acquire_unborn(
                    sys,
                    &self.user,
                    std::slice::from_ref(candidate),
                    &recorded,
                    &current,
                ) {
                    Ok(empty) => plan.empty.extend(empty),
                    Err(why) => {
                        network_failed |= why.is_network();
                        failed.push(why.to_string());
                    }
                }
            }
            standing =
                repos::partition_with_empty(sys, &self.user, &plan.repositories, &plan.empty)?;
        }
        if work == RepositoryWork::Refresh && configuration(sys, &self.user)?.is_none() {
            let refreshed =
                repos::refresh_existing(sys, &self.user, &standing, &recorded, &current)?;
            network_failed |= refreshed.network_failed;
            failed.extend(refreshed.failed);
            // A refreshed parent may declare different gitlinks. Never hydrate
            // dependencies from the pre-refresh parent's cached verdict.
            if refreshed.moved {
                standing =
                    repos::partition_with_empty(sys, &self.user, &plan.repositories, &plan.empty)?;
                if let Some(why) = repos::named(&standing.git_waits) {
                    self.reading.set(Some(Reading::Waits(Wait::GitReadiness)));
                    failed.push(why);
                }
            }
        }
        if standing.missing.is_empty()
            && standing.dependencies.is_empty()
            && standing.unborn.is_empty()
        {
            if failed.is_empty() {
                current()?;
                self.acknowledge_credential(sys, &plan.token, &current)?;
                current()?;
            }
            if network_failed
                && let Err(why) = super::RepositorySelection::recover_refused_credential(
                    sys,
                    &self.user,
                    &plan.token,
                    &current,
                )
            {
                failed.push(format!("credential recovery is unfinished: {why}"));
            }
            self.carry(Carried::Applied(plan));
            return if failed.is_empty() {
                Ok(())
            } else {
                Err(failed.join("; "))
            };
        }
        let guard = repos::CloneGuard::new(&current);
        let mut cloned = repos::clone_all(sys, &self.user, &standing.missing, Some(&guard));
        plan.empty.append(&mut cloned.empty);
        network_failed |= cloned.network_failed;
        cloned.failed.extend(failed);
        let home = Path::new(&self.user.home);
        // Read once the clones and dependencies have taken their room.
        let room =
            |left: &[String]| repos::short_of_room(sys.volume(home).as_ref(), left, CHECK_AGAIN);
        for (repo, path) in &standing.dependencies {
            if let Err(why) = current() {
                return Err(repos::stopped_after_clones(
                    why,
                    cloned.failed,
                    room(&cloned.left),
                    &cloned.hooks,
                ));
            }
            let mut progress = crate::repository_progress::Progress::new(&repo.name, path.clone());
            progress.placed = true;
            progress.enter(crate::repository_progress::Phase::Dependencies);
            sys.repository_progress(&progress);
            let repaired = super::dependencies::repair(sys, &self.user, path, repo, &current);
            progress.usable = repaired.is_ok();
            match &repaired {
                Ok(()) => progress.enter(crate::repository_progress::Phase::Ready),
                Err(why) => why.record_progress(&mut progress),
            }
            sys.repository_progress(&progress);
            if let Err(why) = repaired {
                network_failed |= why.is_network();
                cloned.failed.push(format!("~/{}: {why}", repo.name));
            }
        }
        let room = room(&cloned.left);
        // A failed row is read back by nothing: it says what the clones that went made of their
        // hook installers, after why it failed; a done one leaves that to the read-back's row.
        if let Err(why) = current() {
            return Err(repos::stopped_after_clones(
                why,
                cloned.failed,
                room,
                &cloned.hooks,
            ));
        }
        let mut failed = cloned.failed;
        if network_failed
            && let Err(why) = super::RepositorySelection::recover_refused_credential(
                sys,
                &self.user,
                &plan.token,
                &current,
            )
        {
            failed.push(format!("credential recovery is unfinished: {why}"));
        }
        if let Some(why) = room {
            // The failed row keeps the page's words for it (`init_observe::failure_note`).
            self.reading.set(Some(Reading::Waits(Wait::DiskSpace)));
            failed.push(why);
        }
        let acknowledged = if failed.is_empty() {
            current()
                .and_then(|()| self.acknowledge_credential(sys, &plan.token, &current))
                .and_then(|()| current())
        } else {
            Ok(())
        };
        // A long batch can outlive changes to worktrees it did not touch. Completion reads
        // every local destination and required dependency again against this same inventory.
        self.carry(Carried::Applied(plan));
        acknowledged?;
        if failed.is_empty() {
            *self.hooks.lock().unwrap_or_else(PoisonError::into_inner) = cloned.hooks;
            Ok(())
        } else {
            failed.extend(cloned.hooks);
            Err(failed.join("; "))
        }
    }

    fn refresh_scope(
        &self,
        sys: &dyn System,
    ) -> Result<aterm_secrets::converge::RefreshScope, String> {
        match &self.captured {
            Some(captured) => captured.inputs.refresh_scope(sys),
            None => self.prepared_inputs(sys).refresh_scope(sys),
        }
    }

    fn begin_refresh(&self, sys: &dyn System) -> Result<RepositoryWork, String> {
        use aterm_secrets::converge::{RefreshRequest, RepositoryRequest};
        if configuration(sys, &self.user)?.is_some() {
            return Ok(self.work);
        }
        if self.work == RepositoryWork::Repair {
            self.retain_repository_lease(sys)?;
            return Ok(RepositoryWork::Repair);
        }
        let scope = self.refresh_scope(sys)?;
        let execution = self.execution.clone().or_else(|| {
            self.captured
                .as_ref()
                .and_then(|captured| captured.inputs.execution.clone())
        });
        let execution = match execution {
            Some(execution) => execution,
            None => {
                let access = sys.repository_pass()?;
                Execution {
                    request: RepositoryRequest::Refresh(RefreshRequest::after(
                        Some(scope),
                        access.observed.version,
                    )),
                    pass: access
                        .pass
                        .ok_or("another repository/converge pass holds align.lock")?,
                }
            }
        };
        let RepositoryRequest::Refresh(request) = execution.request else {
            *self.lease.lock().unwrap_or_else(PoisonError::into_inner) = Some(execution.pass);
            return Ok(RepositoryWork::Repair);
        };
        let (work, ticket) =
            match execution
                .pass
                .admit(request, scope, sys.now().saturating_mul(1000))?
            {
                Admission::Refresh(ticket) => (RepositoryWork::Refresh, Some(ticket)),
                Admission::Fulfilled => (RepositoryWork::Repair, None),
                Admission::Waiting => {
                    return Err("repository refresh is waiting for its shared retry".into());
                }
            };
        *self.lease.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Arc::clone(&execution.pass));
        *self.refresh.lock().unwrap_or_else(PoisonError::into_inner) =
            ticket.map(|ticket| ActiveRefresh {
                pass: execution.pass,
                ticket,
            });
        Ok(work)
    }

    fn retain_repository_lease(&self, sys: &dyn System) -> Result<(), String> {
        if self
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return Ok(());
        }
        let execution = self.execution.as_ref().or_else(|| {
            self.captured
                .as_ref()
                .and_then(|capture| capture.inputs.execution.as_ref())
        });
        let pass = match execution {
            Some(execution) => Arc::clone(&execution.pass),
            None => sys
                .repository_pass()?
                .pass
                .ok_or("another repository/converge pass holds align.lock")?,
        };
        *self.lease.lock().unwrap_or_else(PoisonError::into_inner) = Some(pass);
        Ok(())
    }

    fn finish_refresh(&self, sys: &dyn System, complete: bool) -> Result<(), String> {
        let _lease = self
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let active = self
            .refresh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(active) = active else {
            return Ok(());
        };
        let current = if complete {
            self.ensure_current(sys).and_then(|()| {
                if self.refresh_scope(sys)? == active.ticket.scope() {
                    Ok(())
                } else {
                    Err("repository refresh scope changed before completion".into())
                }
            })
        } else {
            Ok(())
        };
        active.pass.finish(
            active.ticket,
            complete && current.is_ok(),
            sys.now().saturating_mul(1000),
        )?;
        current
    }
}

struct Plan {
    token: Arc<Secret>,
    repositories: Vec<repos::Repo>,
    /// Only a captured private selection can establish no work without checking GitHub.
    empty_inventory: bool,
    /// The listed repositories left out because another takes their folder
    /// ([`repos::Inventory::notes`]).
    notes: Vec<String>,
    empty: Vec<repos::EmptyCheckout>,
}

/// Why [`NativeRepositories::plan`] cannot go on: the engine's line, and the reason System
/// State words for a person where the step that stopped decided one. A step that decided
/// none leaves the page its own words for a waiting row.
#[derive(Clone, Debug)]
pub(super) struct Block {
    pub(super) why: String,
    wait: Option<Wait>,
}

impl Block {
    /// The probe's answer for this block (System State design §3.6): a person's step where only
    /// their act ends the wait ([`Wait::kind`]), so its row needs them and no pass retries it,
    /// else a wait that ends by itself. For `aterm init`'s default (`unasked`), a machine the
    /// vault wrote no token to has no stage to run, which asks nobody anything; `machine` names
    /// it ([`System::machine`]).
    fn probe(self, unasked: bool, machine: &str) -> Probe {
        match self.wait {
            Some(Wait::Token) if unasked => Probe::NotApplicable {
                why: format!(
                    "no GitHub token on this {} yet: the ALab vault's sync writes ~/{} \
                     (init.toml [github] token_file names another)",
                    machine,
                    super::TOKEN_FILE
                ),
            },
            Some(wait) if wait.kind() == FailureKind::NeedsPerson => Probe::Person {
                step: if wait == Wait::InTheWay {
                    REPOSITORIES_STEP
                } else {
                    GITHUB_STEP
                }
                .into(),
                how: self.why,
            },
            _ => Probe::Blocked { why: self.why },
        }
    }

    /// A step that stopped for `wait`, with its line.
    pub(super) fn of(wait: Wait) -> impl FnOnce(String) -> Self {
        move |why| Self {
            why,
            wait: Some(wait),
        }
    }

    /// A GitHub read that failed, by its kind: no answer, a token GitHub does not take, or
    /// anything else.
    fn github(error: GithubError) -> Self {
        Self::of(match error.failure {
            GithubFailure::Unreached => Wait::Unreached,
            GithubFailure::Unauthenticated => Wait::TokenRefused,
            GithubFailure::Other => Wait::GitHub,
        })(error.why)
    }

    /// The token source that could not be read: not there yet, or there and unusable.
    fn token(unread: TokenUnread) -> Self {
        Self::of(if unread.absent {
            Wait::Token
        } else {
            Wait::TokenUnusable
        })(unread.why)
    }
}

impl From<String> for Block {
    /// A step that decided no reason.
    fn from(why: String) -> Self {
        Self { why, wait: None }
    }
}

/// What a first-launch pass that left repositories for room tells the person to do once there is
/// room ([`repos::short_of_room`]): System State's run tries again by itself only for its first
/// tries.
const CHECK_AGAIN: &str = "press Check again in System State, or launch aterm again";

impl NativeRepositories {
    fn ensure_current(&self, sys: &dyn System) -> Result<(), String> {
        if let Some(captured) = &self.captured
            && !(captured.current)()?
        {
            return Err("the selected secrets vault changed or is still syncing; repository setup waits for a new ready snapshot".into());
        }
        let inputs = self
            .captured
            .as_ref()
            .map(|captured| &captured.inputs)
            .or_else(|| self.prepared.get());
        if let Some(inputs) = inputs.filter(|inputs| inputs.local.is_ok())
            && !inputs.is_current_now(sys)?
        {
            return Err(
                "repository input source changed; waiting for a new coherent version".into(),
            );
        }
        Ok(())
    }

    /// The token and private vault inventory, captured together. An explicitly configured
    /// profile instead reads its own live GitHub account and repository scope.
    fn plan(&self, sys: &dyn System) -> Result<Plan, Block> {
        self.ensure_current(sys).map_err(Block::from)?;
        let inputs = match &self.captured {
            Some(captured) if captured.inputs.user == self.user => captured.inputs.local.clone()?,
            Some(_) => return Err("the captured repository account changed".to_owned().into()),
            None if self.unasked
                && sys
                    .stat(&Path::new(&self.user.home).join(super::TOKEN_FILE))
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
                && configuration(sys, &self.user)
                    .map_err(Block::of(Wait::Settings))?
                    .is_none()
                && aterm_secrets::client::config(&aterm_secrets::client::Paths::for_home(
                    Path::new(&self.user.home),
                ))
                .map_err(|why| Block::from(why.to_string()))?
                .is_none() =>
            {
                // No token and no configured source yet: git and the home need not be read.
                return Err(Block::of(Wait::Token)(String::new()));
            }
            None => self.prepared_inputs(sys).local?,
        };
        let LocalInputs {
            configured,
            token,
            inventory,
        } = inputs;
        if let Some(inventory) = inventory {
            let repositories = inventory.into_repositories();
            self.ensure_current(sys).map_err(Block::from)?;
            return Ok(Plan {
                token,
                empty_inventory: repositories.is_empty(),
                repositories,
                notes: Vec::new(),
                empty: self.borrowed_empty(),
            });
        }
        let account = identity(sys, &self.user, &token).map_err(Block::github)?;
        if configured
            .as_ref()
            .is_some_and(|config| !account.eq_ignore_ascii_case(&config.account))
        {
            return Err(Block::of(Wait::OtherAccount)(
                "the synced token belongs to a different configured GitHub account; nothing changed".into(),
            ));
        }
        let (base, owner) = if let Some(config) = &configured {
            if config.owner.eq_ignore_ascii_case(&account) {
                (ACCESSIBLE.to_owned(), Some(config.owner.as_str()))
            } else {
                let (owner, _) = get(sys, &self.user, &token, &format!("/users/{}", config.owner))
                    .map_err(Block::github)?;
                if owner["login"]
                    .as_str()
                    .is_none_or(|owner| !owner.eq_ignore_ascii_case(&config.owner))
                    || owner["type"].as_str() != Some("Organization")
                {
                    return Err(Block::of(Wait::Settings)(
                        "a different configured repository owner must be a GitHub organization"
                            .into(),
                    ));
                }
                (
                    format!(
                        "/orgs/{}/repos?type=all&per_page=30&sort=full_name&direction=asc",
                        config.owner
                    ),
                    Some(config.owner.as_str()),
                )
            }
        } else {
            (ACCESSIBLE.into(), None)
        };
        // A page that failed keeps its kind; one read whole but not usable as an
        // inventory is GitHub's answer that could not be used.
        let failed = std::cell::Cell::new(GithubFailure::Other);
        let mut listed = repos::enumerate_pages(&base, owner, Some(&account), |endpoint| {
            get(sys, &self.user, &token, endpoint).map_err(|error| {
                failed.set(error.failure);
                error.why
            })
        })
        .map_err(|why| {
            Block::github(GithubError {
                failure: failed.get(),
                why,
            })
        })?;
        if configured.is_none() {
            listed
                .repositories
                .retain(|repo| !repo.fork && !repo.archived);
        }
        self.ensure_current(sys).map_err(Block::from)?;
        Ok(Plan {
            token,
            empty_inventory: false,
            repositories: listed.repositories,
            notes: listed.notes,
            empty: self.borrowed_empty(),
        })
    }

    fn borrowed_empty(&self) -> Vec<repos::EmptyCheckout> {
        let inputs = self
            .captured
            .as_ref()
            .map(|capture| &capture.inputs)
            .or_else(|| self.prepared.get());
        inputs
            .and_then(|inputs| {
                inputs.readback.as_ref().filter(|proof| {
                    proof.user == self.user
                        && proof.source.is_some()
                        && proof.source == inputs.source
                })
            })
            .map_or_else(Vec::new, |proof| proof.empty.clone())
    }
}

fn configuration(sys: &dyn System, user: &User) -> Result<Option<Github>, String> {
    let path = Path::new(&user.home).join(".config/aterm/init.toml");
    let bytes = match sys.read_owned_file(&path, user.uid, INPUT_MAX, false) {
        Ok(bytes) => bytes,
        Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(
                "cannot safely read ~/.config/aterm/init.toml; no repositories changed".into(),
            );
        }
    };
    // A table that turns the stage off is System State's choice there, not a scope here.
    let configured = match asked(decode(&bytes)?.github, user)? {
        Asked::Scoped(config) | Asked::Profile(config) => Some(config),
        Asked::Default | Asked::Off => None,
    };
    if configured.as_ref().is_some_and(|config| {
        config
            .token_file
            .file_name()
            .is_some_and(|name| name == "gh_access_token_alabsystems")
    }) {
        return Err("public release credentials are not a development repository source".into());
    }
    Ok(configured)
}

#[cfg(test)]
#[path = "native_tests.rs"]
pub(super) mod tests;
