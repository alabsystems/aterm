// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private vault repository inventory and native cloning through [`crate::git_credential`].
//! An explicit init.toml profile can instead ask `agh` for that account's live inventory.

use super::checkout_inputs::{PassWrites, Wrote};
use super::{
    Asked, Github, INPUT_MAX, TokenUnread, asked, decode, get, identity, keychain, read_token,
    repos,
};
use crate::engine::Place;
use crate::git_credential::{Git, Lookups};
use crate::init_observe::{LastReading, Reading, RepositoryCounts, Wait};
use crate::items::{Ctx, Item};
use crate::model::{ItemReport, Mode, Probe, User};
use crate::port::{GithubError, GithubFailure, Secret, System};
use crate::sync::repository_refresh::{Admission, Execution, RepositoryPass, Ticket};
use aterm_converge::{Class, Layer, Remedy, Requires, StoodAside};
use aterm_secrets::converge::RepositoryWork;
use aterm_types::macos_setup::FailureKind;
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// The repositories the signed-in account owns, private ones included. Its forks and archived
/// repositories are left out of the default set ([`NativeRepositories::plan`]); an init.toml
/// `[github]` owner widens it.
const ACCESSIBLE: &str =
    "/user/repos?affiliation=owner&visibility=all&per_page=30&sort=full_name&direction=asc";

/// The step a person takes for the GitHub settings a repository needs, as the engine names it
/// beside their remedy.
const GITHUB_STEP: &str = "github.settings";
/// Preserved checkout work is separate from the settings remedy.
const REPOSITORIES_STEP: &str = "github.repositories";

/// The line of a row that stood aside because another pass holds the repository lease
/// ([`Wait::OtherPass`]).
const OTHER_PASS: &str = "another aterm pass holds this account's repository lease (align.lock); \
                          this row reads again once it lets go";

/// A nonempty automatic selection can publish a count with every repository here
/// ([`RepositoryCounts::all_here`]) only after the captured default source's pinned credential
/// route also works. Empty selection checks no sign-in.
/// Missing work is left to the native
/// batch's ordinary route check, rather than invoking the helper twice before cloning.
/// # Errors
/// The account, source, checkouts or credential route cannot be read safely, the route
/// does not return the captured token, or the captured source is no longer ready.
pub(crate) fn automatic_inventory_counts(
    sys: &dyn System,
    ctx: &Ctx,
) -> Result<Option<RepositoryCounts>, String> {
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
) -> Result<Option<RepositoryCounts>, String> {
    captured.validate_default_context(sys, ctx)?;
    if captured.local.is_err()
        && let Some(selection) = super::RepositorySelection::capture(sys, ctx)?
        && selection.count() == 0
    {
        captured.validate_selection(sys, ctx)?;
        selection.ensure_current(sys)?;
        return Ok(Some(RepositoryCounts::default()));
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
    let counts = repos::partition_with_empty(sys, &captured.user, listed, empty)?.counts();
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
    if !captured.is_current(sys)? {
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

/// Which of the stage's repository items a run binds. Acquisition always runs before the
/// refresh: a missing repository never waits behind fetches of the ones already here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepositorySteps {
    /// Acquisition ([`NativeRepositories`]), then the refresh of the checkouts that were here
    /// before it ([`RepositoryRefresh`]): System State's stage and `aterm init`.
    Both,
    /// Acquisition alone: the converge pass's `sync.clone`, and an `aterm init` whose `--only`
    /// names the acquisition's row but not the refresh's ([`Self::named_by`]).
    Acquisition,
    /// The refresh alone: the converge pass's `sync.refresh`, after `sync.clone`.
    Refresh,
}

impl RepositorySteps {
    const fn acquires(self) -> bool {
        matches!(self, Self::Both | Self::Acquisition)
    }

    const fn refreshes(self) -> bool {
        matches!(self, Self::Both | Self::Refresh)
    }

    /// The steps an `aterm init` run whose `--only` words are `only` binds for `user`'s
    /// repositories: acquisition alone where the words select its row
    /// (`init.github.native@<user>`) but not the refresh's (`init.github.refresh@<user>`), so
    /// the acquisition hands its lease to no refresh, which that run would never start, and
    /// lets it go itself ([`NativeRepositories::release_lease`]); both otherwise (no words
    /// keep every row, and the run's selection drops the ones its words do not name).
    pub(super) fn named_by(only: &[String], user: &str) -> Self {
        let selected = |id: &str| {
            only.iter()
                .any(|word| aterm_converge::selects(word, id, FAMILY, Some(Layer::Workstation)))
        };
        if selected(acquisition_id(user).as_str()) && !selected(refresh_id(user).as_str()) {
            Self::Acquisition
        } else {
            Self::Both
        }
    }
}

/// The family both repository rows share, which `--only github` names.
const FAMILY: &str = "github";

/// The acquisition's row id for `user` ([`NativeRepositories`]): `init.github.native@<user>`.
fn acquisition_id(user: &str) -> String {
    format!("{}.native@{user}", super::ID)
}

/// The refresh's row id for `user` ([`RepositoryRefresh`]): `init.github.refresh@<user>`.
fn refresh_id(user: &str) -> String {
    format!("{}.refresh@{user}", super::ID)
}

pub(super) fn run(sys: &dyn System, ctx: &Ctx, mode: Mode) -> Result<RepositoryRun, String> {
    run_with(
        sys,
        ctx,
        mode,
        RepositoryWork::Refresh,
        RepositorySteps::Both,
        None,
    )
}

pub(super) fn run_captured(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    work: RepositoryWork,
    steps: RepositorySteps,
    inputs: InitialRepositoryInputs,
    current: Box<dyn Fn() -> Result<bool, String> + Send + Sync>,
) -> Result<RepositoryRun, String> {
    run_with(
        sys,
        ctx,
        mode,
        work,
        steps,
        Some(Captured { inputs, current }),
    )
}

/// `aterm init`'s repository items for `user` with no `[github]` token source in init.toml: the
/// same stage the first launch runs, after the secrets item `after` names when one does (the
/// vault writes the token it reads), its acquisition and then, where `steps` says so, the
/// refresh of what was here before it. With no `[github]` table at all (`unasked`), a Mac with
/// no token is no stage of this run. An apply (`mode`) decides the repository lease before
/// either item reads anything; a plan never takes it.
#[expect(
    clippy::too_many_arguments,
    reason = "the stage's selection, its run's mode and the enclosing pass's lease"
)]
pub(super) fn items(
    user: &User,
    after: Option<String>,
    unasked: bool,
    mode: Mode,
    work: RepositoryWork,
    steps: RepositorySteps,
    execution: Option<Execution>,
    withdrawal_current: Option<Box<dyn Fn() -> Result<bool, String> + Send + Sync>>,
) -> Vec<Box<dyn Item>> {
    let core = Arc::new(NativeRepositories {
        after,
        unasked,
        mode,
        work,
        execution,
        local_current: withdrawal_current,
        refresh_follows: steps == RepositorySteps::Both,
        ..NativeRepositories::new(user.clone(), None)
    });
    let mut items: Vec<Box<dyn Item>> = Vec::new();
    if steps.acquires() {
        items.push(Box::new(Acquisition(Arc::clone(&core))));
    }
    if steps.refreshes() {
        items.push(Box::new(RepositoryRefresh::new(core)));
    }
    items
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
    steps: RepositorySteps,
    captured: Option<Captured>,
) -> Result<RepositoryRun, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("initial repository setup requires exactly one local user".into());
    };
    // Candidate observation cannot replace the ordinary Native blocker's typed report.
    // Only a successful local withdrawal reading bypasses the paired repository stage.
    if let Ok(Some(withdrawal)) = super::RepositoryCredentialWithdrawal::capture(sys, ctx) {
        if !steps.acquires() {
            // A withdrawal retires the credential's checkouts: there is nothing to refresh.
            return Ok(RepositoryRun {
                rows: Vec::new(),
                readback: None,
            });
        }
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
    if mode == Mode::Apply && steps.acquires() {
        // What a clone killed mid-way left (design §6.5): never a clone still running.
        repos::sweep_staging(sys, user);
    }
    // The keychain block goes first, whatever GitHub says below.
    let keychain = keychain::Keychain::new(user.clone());
    let item = Arc::new(NativeRepositories {
        mode,
        work,
        observe_remote: mode == Mode::Apply || work == RepositoryWork::Refresh,
        refresh_follows: steps == RepositorySteps::Both,
        ..NativeRepositories::new(user.clone(), captured)
    });
    let refresh = RepositoryRefresh::new(Arc::clone(&item));
    let place = || Place {
        layer: Layer::Workstation,
        class: Class::Install,
        requires: Requires::Account,
        remedy: Remedy::Command("aterm init".into()),
    };
    let mut items: Vec<(&dyn Item, Place)> = Vec::new();
    if steps.acquires() {
        items.push((&keychain as &dyn Item, place()));
        items.push((&*item as &dyn Item, place()));
    }
    if steps.refreshes() {
        items.push((&refresh as &dyn Item, place()));
    }
    let bound = crate::engine::bind(items, sys, ctx);
    let rows = if mode == Mode::Apply {
        let mut journal = sys.journal()?;
        aterm_converge::run(&bound, mode, journal.as_mut())
    } else {
        aterm_converge::run(&bound, mode, &mut aterm_converge::ReadOnly)
    }
    .map_err(|why| why.to_string())?;
    let readback = if mode == Mode::Apply && steps.acquires() {
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

/// The stage's acquisition (`init.github.native@<user>`): it clones missing repositories, gives
/// matching checkouts without a commit their origin's first commit, and repairs required
/// dependencies, whatever the caller's [`RepositoryWork`]. It alone reads presence and standing
/// for System State's GitHub repositories row and `sync.clone`. The refresh of checkouts already
/// here is a row of its own after it ([`RepositoryRefresh`]), over this same captured input and
/// lease.
struct NativeRepositories {
    user: User,
    /// The run's mode. An apply decides the repository lease before either item's probe reads a
    /// checkout or a remote ([`Self::lease_free`]); a plan never takes the lease, and reads
    /// instead whether another owner works the repositories now.
    mode: Mode,
    /// The caller's scheduling intent, which only [`RepositoryRefresh`] reads; it grants no
    /// authority over the captured source.
    work: RepositoryWork,
    /// An apply acquires missing work; a Refresh also admits read-only remote observation.
    /// A Plan/Repair readback alone cannot turn unseen remote state into missing work.
    observe_remote: bool,
    execution: Option<Execution>,
    /// The shared refresh ticket [`RepositoryRefresh`] holds between its admission and finish.
    refresh: Mutex<Option<ActiveRefresh>>,
    /// The repository lease an apply holds: acquisition's until its read-back, or the end of
    /// its row where no read-back comes ([`Item::row_ended`]), or, where the refresh follows it
    /// in this run ([`Self::refresh_follows`]), until the refresh row ends.
    lease: Mutex<Option<Lease>>,
    /// Whether this run binds the refresh after this acquisition: the acquisition then leaves
    /// its lease to the refresh, so no other pass takes the lock between the two rows.
    refresh_follows: bool,
    /// Why the last probe stood aside before it read anything: another pass holds the lease
    /// ([`Item::read_aside`]).
    lease_aside: Mutex<Option<StoodAside>>,
    /// Set by an apply that went through: the probe after it is its read-back, which decides no
    /// lease and ends the one the apply held.
    applied: AtomicBool,
    /// The checkouts here (ready or needing dependencies) when acquisition began: the only ones
    /// [`RepositoryRefresh`] refreshes. `None` where no acquisition ran, which leaves every
    /// checkout here to it.
    before: Mutex<Option<BTreeSet<PathBuf>>>,
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
    /// What the last probe read of the checkouts, whatever it decided: whether the private
    /// inventory is empty, and its partition. The refresh after it reads no worktree again.
    last: Mutex<Option<(bool, repos::Partition)>>,
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

/// The repository lease a run holds ([`NativeRepositories::lease`]).
struct Lease {
    pass: Arc<RepositoryPass>,
    /// The shared refresh record's epochs as this run found them when it took the lock itself;
    /// `None` for an enclosing pass's lease, whose execution carries its own request.
    observed: Option<aterm_secrets::converge::RefreshVersion>,
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
        let default = match super::asked(table) {
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
    /// local files and ready-vault evidence, and grants no lease over later effects. A vault
    /// pass holding the vault's state lock is waited for
    /// ([`aterm_secrets::client::MaterializationSnapshot::is_current`]).
    ///
    /// # Errors
    /// The current local source or vault state cannot be safely read, or a vault pass held the
    /// state lock past the wait.
    pub fn is_current(&self, sys: &dyn System) -> Result<bool, String> {
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
                if !snapshot.is_current(paths).map_err(|why| why.to_string())? {
                    return Ok(false);
                }
            }
        }
        self.source
            .as_ref()
            .map_or(Ok(false), |source| source.is_current(sys, &self.user))
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
        if self.local.is_ok() && !self.is_current(sys)? {
            return Err("captured repository inputs changed before selection readback".into());
        }
        Ok(())
    }

    fn read(sys: &dyn System, user: &User) -> Result<LocalInputs, Block> {
        crate::port::no_core_dumps();
        if sys.euid() != user.uid {
            return Err(String::from("repository setup must run as its local user").into());
        }
        repos::admit_destination(sys, user)?;
        let configured = configuration(sys, user).map_err(Block::of(Wait::Settings))?;
        let token = Arc::new(read_token(sys, user).map_err(Block::token)?);
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

    /// A stage for `user`. It begins no record of checkout facts: it reads through the one its
    /// pass's owner began ([`crate::port::CheckoutRecords::new_pass`]), so `sync.clone`'s stage
    /// and `sync.refresh`'s after it read each checkout once between them.
    fn new(user: User, captured: Option<Captured>) -> Self {
        Self {
            user,
            mode: Mode::Plan,
            work: RepositoryWork::Refresh,
            observe_remote: true,
            execution: None,
            refresh: Mutex::new(None),
            lease: Mutex::new(None),
            refresh_follows: false,
            lease_aside: Mutex::new(None),
            applied: AtomicBool::new(false),
            before: Mutex::new(None),
            reading: LastReading::default(),
            captured,
            prepared: OnceLock::new(),
            after: None,
            unasked: false,
            carried: Mutex::new(None),
            last: Mutex::new(None),
            hooks: Mutex::new(Vec::new()),
            selection: Mutex::new(None),
            aside: Mutex::new(None),
            acknowledge: Mutex::new(false),
            withdrawal: Mutex::new(None),
            local_withdrawal: None,
            local_current: None,
        }
    }

    /// Whether this run's last probe read an empty private inventory, and the checkouts it
    /// read, whatever it decided.
    fn last_read(&self) -> Option<(bool, repos::Partition)> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
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
        acquisition_id(&self.user.name)
    }
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn title(&self) -> String {
        "GitHub repositories from synced development credentials".into()
    }
    fn after(&self) -> Vec<String> {
        self.after.iter().cloned().collect()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = None;
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
        *self
            .lease_aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        let read_back = self.applied.swap(false, Ordering::SeqCst);
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
            // The lease is decided before anything else is read: a row another pass holds it
            // from reads no checkout and asks GitHub nothing, and stands aside until that pass
            // lets go. A read-back decides nothing: its apply held the lease. A system that
            // keeps no repository record cannot say, and reads on: an apply that needs the lease
            // fails there with that reason.
            if !read_back && matches!(self.lease_free(sys), Ok(false)) {
                return self.held_elsewhere();
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
                let lookups = self.lookups(sys, &plan)?;
                self.ensure_current(sys).map_err(Block::from)?;
                if lookups.ready() && self.observe_remote && !standing.unborn.is_empty() {
                    let recorded =
                        super::develop_checkouts(sys, &self.user).map_err(Block::from)?;
                    let mut preserved = Vec::new();
                    for candidate in &standing.unborn {
                        plan.empty.retain(|proof| !proof.is_for(&candidate.0));
                        // A read that fails withholds nothing else: the checkout stays owed, and
                        // acquisition reads its remote again, failing only its own line.
                        match repos::observe_unborn(sys, &self.user, candidate, &recorded, &|| {
                            self.ensure_current(sys)
                        }) {
                            Ok(repos::UnbornObservation::Empty(empty)) => plan.empty.push(*empty),
                            Ok(repos::UnbornObservation::Owed) | Err(_) => {}
                            Ok(repos::UnbornObservation::Preserved(why)) => {
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
                        // A local read that fails leaves the checkout owed: acquisition reads it
                        // again with its remote, failing only its own line.
                        if let Ok(Some(why)) = repos::local_unborn(
                            sys,
                            &self.user,
                            &(repo.clone(), path.clone()),
                            &recorded,
                        ) {
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
                *self.last.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((plan.empty_inventory, standing.clone()));
                // A dependency waiting for a capable Git holds back only its own repair: the row
                // waits for Git where nothing else is left to acquire.
                if standing.missing.is_empty()
                    && standing.dependencies.is_empty()
                    && standing.unborn.is_empty()
                    && let Some(why) = repos::named(&standing.git_waits)
                {
                    return Err(Block::of(Wait::GitReadiness)(why));
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
            let counts = standing.counts();
            self.reading.set(Some(Reading::Repositories {
                counts,
                signed_in: ready,
            }));
            let mut notes = plan.notes.clone();
            notes.extend(standing.notes.iter().cloned());
            let probe = if let Lookups::Not(why) = &lookups {
                Probe::Blocked {
                    why: format!(
                        "{}; aterm's GitHub route does not hand back the synced token: {why}; \
                         aterm never keeps the token in the login keychain",
                        counts.summary()
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
                        "{}; preserved work or dependency metadata blocks completion: {}",
                        counts.summary(),
                        repos::named(&standing.notes).unwrap_or_default()
                    ),
                }
            } else if counts.ready == counts.total() {
                let mut now = format!(
                    "{}; aterm's GitHub route hands back the synced token",
                    counts.summary()
                );
                if let Some(left) = repos::named(&notes) {
                    now.push_str(&format!("; left as they are: {left}"));
                }
                Probe::Ok { now }
            } else {
                Probe::Change {
                    from: counts.summary(),
                    to: "clone missing repositories, give checkouts without a commit their \
                         first, and repair required dependencies through aterm's GitHub route; \
                         preserve existing work"
                        .into(),
                }
            };
            let probe = if matches!(probe, Probe::Ok { .. }) {
                match super::RepositorySelection::acknowledgment_due(sys, &self.user) {
                    Ok(true) => {
                        *self
                            .acknowledge
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) = true;
                        Probe::Change {
                            from: "the delivered development credential has no consumer acknowledgment yet".into(),
                            to: "confirm the paired credential route".into(),
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
        // A change keeps the lease the probe took for the apply that follows; a row the engine
        // ends without that apply lets it go then (`row_ended`). A read-back ends the lease its
        // apply held, and so does a probe no apply follows, or each leaves it to the refresh
        // that follows.
        if read_back || !matches!(read, Probe::Change { .. }) {
            self.release_lease();
        }
        repos::with_hooks(read, &hooks)
    }

    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.apply_guarded(sys, ctx, &|| Ok(()))
    }

    fn apply_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        let applied = self.apply_owed(sys, ctx, authorized);
        // The engine reads back only an apply that went through and did not stand aside.
        self.applied.store(
            applied.is_ok() && self.stood_aside().is_none(),
            Ordering::SeqCst,
        );
        applied
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn read_aside(&self) -> Option<StoodAside> {
        self.lease_aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    /// The row ended with nothing after its last probe: the lease that probe or its apply kept
    /// for what did not follow (an apply the engine did not make, a read-back it skipped after a
    /// stand-aside) goes now, or passes to the refresh bound after it.
    fn row_ended(&self) {
        self.release_lease();
    }
}

impl NativeRepositories {
    /// [`Item::apply_guarded`]: what the probe found owed, under the lease it took.
    fn apply_owed(
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
                    self.ensure_current(sys)?;
                    authorized()
                };
                let _ = selection.request_credential(sys, &current, authorized)?;
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
                self.ensure_current(sys)?;
                authorized()
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
        let result = self.apply_batch(sys, ctx, authorized);
        if result.is_err() {
            // No read-back follows a failed apply to end (or hand on) the lease.
            self.release_lease();
        }
        result
    }

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

    /// What every effect of a batch checks first: the caller's authorization, the stage still
    /// on, the captured source current, and `[develop]`'s checkouts as `recorded` read them.
    fn batch_current(
        &self,
        sys: &dyn System,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
        recorded: &BTreeSet<PathBuf>,
    ) -> Result<(), String> {
        authorized()?;
        if !super::settings_managed(sys, &self.user)? {
            return Err("repository setup was disabled in init.toml during this pass".into());
        }
        self.ensure_current(sys)?;
        authorized()?;
        if *recorded != super::develop_checkouts(sys, &self.user)? {
            return Err("develop checkout selection changed during this pass".into());
        }
        Ok(())
    }

    /// The record of the pass's own writes to the watched checkouts ([`PassWrites`]), under
    /// the repository lease this run holds: none without one.
    fn own_writes(&self, sys: &dyn System) -> Option<PassWrites> {
        let pass = self
            .lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|lease| Arc::clone(&lease.pass))?;
        PassWrites::new(sys, &self.user, pass)
    }

    /// Repair the required dependencies of each of `dependencies`, checking `current` first:
    /// each failure's line, and whether any failed on the network. A stop leaves the rest. With
    /// `writes`, each repair is recorded as the pass's own write ([`PassWrites::around`]).
    fn repair_dependencies(
        &self,
        sys: &dyn System,
        dependencies: &[(repos::Repo, PathBuf)],
        current: &(dyn Fn() -> Result<(), String> + Sync),
        writes: Option<&PassWrites>,
    ) -> Result<(Vec<String>, bool), (String, Vec<String>)> {
        let mut failed = Vec::new();
        let mut network_failed = false;
        for (repo, path) in dependencies {
            if let Err(why) = current() {
                return Err((why, failed));
            }
            let mut progress = crate::repository_progress::Progress::new(&repo.name, path.clone());
            progress.placed = true;
            progress.enter(crate::repository_progress::Phase::Dependencies);
            sys.repository_progress(&progress);
            let repair = || {
                (
                    super::dependencies::repair(sys, &self.user, path, repo, current),
                    Wrote::Made,
                )
            };
            let repaired = match writes {
                Some(writes) => writes.around(sys, path, repair),
                None => repair().0,
            };
            progress.usable = repaired.is_ok();
            match &repaired {
                Ok(()) => progress.enter(crate::repository_progress::Phase::Ready),
                Err(why) => why.record_progress(&mut progress),
            }
            sys.repository_progress(&progress);
            if let Err(why) = repaired {
                network_failed |= why.is_network();
                failed.push(format!("~/{}: {why}", repo.name));
            }
        }
        Ok((failed, network_failed))
    }

    /// Acquire what the probe found owed: clone the missing repositories and give each matching
    /// checkout without a commit its origin's first commit, in one pool ([`repos::clone_all`]),
    /// then repair required dependencies. No checkout already here is fetched: that is
    /// [`RepositoryRefresh`]'s, after this. A dependency waiting for a capable Git holds back
    /// only its own repair; the row then stands aside waiting for Git.
    fn apply_batch(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
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
        let current = || self.batch_current(sys, authorized, &recorded);
        current()?;
        if plan.empty_inventory {
            self.carry(Carried::Applied(plan));
            return Ok(());
        }
        if let Lookups::Not(why) = self.lookups(sys, &plan)? {
            return Err(format!(
                "aterm's GitHub route does not hand back the synced token: {why}; no repository \
                 was cloned"
            ));
        }
        // The default source's acquisition holds the repository lease, and so does an explicit
        // profile's that writes into a checkout already here: the probe took it, and an apply
        // made without that probe takes it now. Another pass holding it stands this row aside,
        // as the probe would have: it is no failure.
        if (configuration(sys, &self.user)?.is_none() || !standing.unborn.is_empty())
            && !self.take_lease(sys)?
        {
            self.reading.set(Some(Reading::Waits(Wait::OtherPass)));
            *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(StoodAside::Lock(OTHER_PASS.into()));
            return Ok(());
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
        // A fresh Refresh probe has already observed its empty origins; repair uses only
        // this item's actual readback. A newly constructed item has neither observation.
        if !standing.unborn.is_empty() {
            standing =
                repos::partition_with_empty(sys, &self.user, &plan.repositories, &plan.empty)?;
            for candidate in &standing.unborn {
                plan.empty.retain(|proof| !proof.is_for(&candidate.0));
            }
        }
        // What the refresh after this row may fetch: only what was here before it.
        *self.before.lock().unwrap_or_else(PoisonError::into_inner) = Some(
            standing
                .present
                .iter()
                .chain(&standing.dependencies)
                .map(|(_, path)| path.clone())
                .collect(),
        );
        let guard = repos::CloneGuard::new(&current);
        let writes = self.own_writes(sys);
        let mut cloned = repos::clone_all(
            sys,
            &self.user,
            &standing.missing,
            &standing.unborn,
            &recorded,
            Some(&guard),
            writes.as_ref(),
        );
        plan.empty.append(&mut cloned.empty);
        let mut network_failed = cloned.network_failed;
        let home = Path::new(&self.user.home);
        // Read once the clones and dependencies have taken their room.
        let room =
            |left: &[String]| repos::short_of_room(sys.volume(home).as_ref(), left, CHECK_AGAIN);
        match self.repair_dependencies(sys, &standing.dependencies, &current, writes.as_ref()) {
            Ok((failed, network)) => {
                network_failed |= network;
                cloned.failed.extend(failed);
            }
            Err((why, failed)) => {
                cloned.failed.extend(failed);
                return Err(repos::stopped_after_clones(
                    why,
                    cloned.failed,
                    room(&cloned.left),
                    &cloned.hooks,
                ));
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
        let git_wait = repos::named(&standing.git_waits);
        let acknowledged = if failed.is_empty() && git_wait.is_none() {
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
        if !failed.is_empty() {
            if let Some(why) = git_wait {
                self.reading.set(Some(Reading::Waits(Wait::GitReadiness)));
                failed.push(why);
            }
            failed.extend(cloned.hooks);
            return Err(failed.join("; "));
        }
        *self.hooks.lock().unwrap_or_else(PoisonError::into_inner) = cloned.hooks;
        if let Some(why) = git_wait {
            // Everything else was acquired; the dependencies that wait for Git are read again
            // once it is ready. The read-back is not run: it would read the same wait.
            self.reading.set(Some(Reading::Waits(Wait::GitReadiness)));
            *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(StoodAside::Waiting(why));
        }
        Ok(())
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

    /// Admit this run's refresh against the shared refresh record ([`RepositoryPass::admit`]),
    /// holding the lease and the ticket until [`Self::finish_refresh`].
    fn begin_refresh(&self, sys: &dyn System) -> Result<Admitted, String> {
        use aterm_secrets::converge::{RefreshRequest, RepositoryRequest};
        let scope = self.refresh_scope(sys)?;
        let execution = self.execution.clone().or_else(|| {
            self.captured
                .as_ref()
                .and_then(|captured| captured.inputs.execution.clone())
        });
        let execution = match execution {
            Some(execution) => execution,
            None => {
                // The lease this run's acquisition or this refresh's probe took, or a new one;
                // none while another pass holds it, which this refresh then waits for.
                if !self.take_lease(sys)? {
                    return Ok(Admitted::Held);
                }
                let (pass, observed) = self
                    .lease
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .and_then(|lease| Some((Arc::clone(&lease.pass), lease.observed?)))
                    .ok_or("this run's repository lease carries no refresh observation")?;
                Execution {
                    request: RepositoryRequest::Refresh(RefreshRequest::after(
                        Some(scope),
                        observed,
                    )),
                    pass,
                }
            }
        };
        let RepositoryRequest::Refresh(request) = execution.request else {
            return Ok(Admitted::Fulfilled);
        };
        let ticket = match execution
            .pass
            .admit(request, scope, sys.now().saturating_mul(1000))?
        {
            Admission::Refresh(ticket) => ticket,
            Admission::Fulfilled => return Ok(Admitted::Fulfilled),
            Admission::Waiting => return Ok(Admitted::Waiting),
        };
        *self.lease.lock().unwrap_or_else(PoisonError::into_inner) = Some(Lease {
            pass: Arc::clone(&execution.pass),
            observed: None,
        });
        *self.refresh.lock().unwrap_or_else(PoisonError::into_inner) = Some(ActiveRefresh {
            pass: execution.pass,
            ticket,
        });
        Ok(Admitted::Refresh)
    }

    /// The enclosing pass's execution, which carries its lease: given to this run, or with its
    /// captured inputs.
    fn enclosing(&self) -> Option<&Execution> {
        self.execution.as_ref().or_else(|| {
            self.captured
                .as_ref()
                .and_then(|capture| capture.inputs.execution.as_ref())
        })
    }

    /// Whether this run may work the account's repositories now, decided before a probe reads a
    /// checkout or a remote. Within an enclosing pass, which holds the lease, it may. An apply
    /// takes this account's repository lease ([`Self::take_lease`]) and keeps it for what
    /// follows in its row, or in the refresh row after it ([`Item::row_ended`]); a plan never
    /// takes it and reads instead whether another owner works the repositories now
    /// ([`System::repository_pass_running`]). `false`: another pass holds it.
    ///
    /// # Errors
    /// As [`Self::take_lease`].
    fn lease_free(&self, sys: &dyn System) -> Result<bool, String> {
        if self.enclosing().is_some() {
            return Ok(true);
        }
        match self.mode {
            Mode::Apply => self.take_lease(sys),
            Mode::Plan => Ok(!sys.repository_pass_running()),
        }
    }

    /// Take the repository lease for this run unless it holds one: the enclosing pass's where
    /// there is one, else this account's own (`align.lock`, [`System::repository_pass`]), with
    /// the shared refresh record's epochs as this run found them. `false`: another pass holds
    /// it, and nothing was taken.
    ///
    /// # Errors
    /// The system keeps no repository record, or the lock cannot be admitted.
    fn take_lease(&self, sys: &dyn System) -> Result<bool, String> {
        let mut held = self.lease.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_some() {
            return Ok(true);
        }
        let lease = match self.enclosing() {
            Some(execution) => Lease {
                pass: Arc::clone(&execution.pass),
                observed: None,
            },
            None => {
                let access = sys.repository_pass()?;
                let Some(pass) = access.pass else {
                    return Ok(false);
                };
                Lease {
                    pass,
                    observed: Some(access.observed.version),
                }
            }
        };
        *held = Some(lease);
        Ok(true)
    }

    /// The probe's answer while another pass holds the repository lease: unusable for now, and
    /// standing aside on that lock ([`Item::read_aside`]), with System State's reading
    /// ([`Wait::OtherPass`]). The engine journals no failure for it.
    fn held_elsewhere(&self) -> Probe {
        self.reading.set(Some(Reading::Waits(Wait::OtherPass)));
        *self
            .lease_aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(StoodAside::Lock(OTHER_PASS.into()));
        Probe::Unusable {
            why: OTHER_PASS.into(),
        }
    }

    /// End the lease an acquisition's probe or apply held, at its read-back, its failure or the
    /// end of its row ([`Item::row_ended`]), unless the refresh after it in this run takes the
    /// lease over ([`Self::refresh_follows`]).
    fn release_lease(&self) {
        if !self.refresh_follows {
            self.end_lease();
        }
    }

    /// End whatever repository lease this run holds.
    fn end_lease(&self) {
        drop(
            self.lease
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }

    /// End the refresh [`Self::begin_refresh`] admitted, `complete` where it refreshed every
    /// checkout it took, and let its lease go.
    fn finish_refresh(&self, sys: &dyn System, complete: bool) -> Result<(), String> {
        let active = self
            .refresh
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let _lease = self
            .lease
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

/// What [`NativeRepositories::begin_refresh`] admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admitted {
    /// This run refreshes, under the ticket it now holds.
    Refresh,
    /// Another producer's refresh already answered this request: nothing to fetch.
    Fulfilled,
    /// The shared refresh failed and waits for its retry, which another run makes.
    Waiting,
    /// Another pass holds the repository lease: this run refreshes nothing until it lets go.
    Held,
}

/// [`NativeRepositories`] as a boxed item that shares its state with its
/// [`RepositoryRefresh`] ([`items`]).
struct Acquisition(Arc<NativeRepositories>);

impl Item for Acquisition {
    fn id(&self) -> String {
        self.0.id()
    }
    fn family(&self) -> &'static str {
        self.0.family()
    }
    fn title(&self) -> String {
        self.0.title()
    }
    fn after(&self) -> Vec<String> {
        self.0.after()
    }
    fn probe(&self, sys: &dyn System, ctx: &Ctx) -> Probe {
        self.0.probe(sys, ctx)
    }
    fn apply(&self, sys: &dyn System, ctx: &Ctx) -> Result<(), String> {
        self.0.apply(sys, ctx)
    }
    fn apply_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        self.0.apply_guarded(sys, ctx, current)
    }
    fn stood_aside(&self) -> Option<StoodAside> {
        self.0.stood_aside()
    }
    fn read_aside(&self) -> Option<StoodAside> {
        self.0.read_aside()
    }
    fn detail(&self, sys: &dyn System, ctx: &Ctx) -> Option<String> {
        self.0.detail(sys, ctx)
    }
    fn row_ended(&self) {
        self.0.row_ended();
    }
}

/// The refresh of the default source's checkouts already here (`init.github.refresh@<user>`), a
/// row of its own after acquisition: it fast-forwards each clean checkout that is behind its
/// origin ([`repos::refresh_existing`]), only those that were here before this run's acquisition
/// began, and reads and repairs the required dependencies of each it moved. It alone admits and
/// finishes the shared refresh ([`RepositoryPass::admit`]). Its fault fails only its own row,
/// and a shared refresh waiting for its retry stands only this row aside: neither holds back an
/// acquisition, which has run by then. Another pass holding the repository lease stands it
/// aside too, decided before its probe reads anything ([`NativeRepositories::lease_free`]). A
/// Repair run, an explicit `[github]` profile and a stage turned off refresh nothing.
struct RepositoryRefresh {
    core: Arc<NativeRepositories>,
    aside: Mutex<Option<StoodAside>>,
    /// Why the last probe stood aside before it read anything: another pass holds the lease
    /// ([`Item::read_aside`]).
    lease_aside: Mutex<Option<StoodAside>>,
    /// What this row waits on, where it stood aside: another pass's lease, or the shared
    /// refresh's retry ([`Wait::OtherPass`], [`Wait::SharedRetry`]).
    reading: LastReading,
    /// The checkouts the probe read, for the refresh that follows it.
    probed: Mutex<Option<repos::Partition>>,
    /// Whether the refresh ran: the probe after it is its read-back.
    ran: std::sync::atomic::AtomicBool,
    /// The checkouts as the refresh that ran left them: the probe's own read where it moved
    /// none, else the read the refresh made after moving one. Its read-back counts them.
    left: Mutex<Option<repos::Partition>>,
}

impl RepositoryRefresh {
    fn new(core: Arc<NativeRepositories>) -> Self {
        Self {
            core,
            aside: Mutex::new(None),
            lease_aside: Mutex::new(None),
            reading: LastReading::default(),
            probed: Mutex::new(None),
            ran: std::sync::atomic::AtomicBool::new(false),
            left: Mutex::new(None),
        }
    }

    /// Why this run refreshes nothing, or `None` where it refreshes.
    fn idle(&self, sys: &dyn System) -> Result<Option<&'static str>, String> {
        let core = &self.core;
        if core.local_withdrawal.is_some() {
            return Ok(Some(
                "a repository credential withdrawal runs; no checkout is refreshed",
            ));
        }
        if core.work != RepositoryWork::Refresh {
            return Ok(Some(
                "this run repairs local checkouts and reads no remote head of one already here",
            ));
        }
        if !super::settings_managed(sys, &core.user)? {
            return Ok(Some(
                "repository setup is disabled in init.toml's [github] table",
            ));
        }
        if configuration(sys, &core.user)?.is_some() {
            return Ok(Some(
                "an explicit [github] profile's checkouts are left as they are",
            ));
        }
        Ok(None)
    }

    /// `standing`'s checkouts here that this run's acquisition found here when it began, or all
    /// of them where no acquisition ran.
    fn earlier(&self, standing: &mut repos::Partition) {
        let before = self
            .core
            .before
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(before) = before.as_ref() {
            standing.present.retain(|(_, path)| before.contains(path));
            standing
                .dependencies
                .retain(|(_, path)| before.contains(path));
        }
    }

    /// The checkouts this refresh would take now.
    fn read(&self, sys: &dyn System) -> Result<(Plan, repos::Partition), Block> {
        let plan = self.core.plan(sys)?;
        let mut standing =
            repos::partition_with_empty(sys, &self.core.user, &plan.repositories, &plan.empty)
                .map_err(Block::from)?;
        self.earlier(&mut standing);
        Ok((plan, standing))
    }

    /// What the probe reads: whether the private inventory is empty, and the checkouts the
    /// refresh would take. Before the refresh runs, this run's acquisition's last read stands
    /// for them where it read them (its read-back, or its probe where it changed nothing); the
    /// read-back after the refresh counts what the refresh left ([`Self::left`]). No worktree
    /// is read again between the two items or for the refresh's read-back: a pass over every
    /// checkout here runs each worktree's reads once per acquisition read.
    fn probe_read(&self, sys: &dyn System) -> Result<(bool, repos::Partition), Block> {
        let reused = if self.ran.load(std::sync::atomic::Ordering::SeqCst) {
            self.left
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
                .map(|standing| (false, standing))
        } else {
            self.core
                .last_read()
                .map(|(empty_inventory, mut standing)| {
                    self.earlier(&mut standing);
                    (empty_inventory, standing)
                })
        };
        if let Some(read) = reused {
            self.core.ensure_current(sys).map_err(Block::from)?;
            return Ok(read);
        }
        let (plan, standing) = self.read(sys)?;
        Ok((plan.empty_inventory, standing))
    }

    /// Refresh, then end the run's lease, whatever the refresh did: it is the last row that
    /// holds it.
    fn refresh(
        &self,
        sys: &dyn System,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        let refreshed = self.refresh_admitted(sys, authorized);
        self.core.end_lease();
        refreshed
    }

    fn refresh_admitted(
        &self,
        sys: &dyn System,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.left.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.ran.store(true, std::sync::atomic::Ordering::SeqCst);
        authorized()?;
        self.core.ensure_current(sys)?;
        match self.core.begin_refresh(sys)? {
            Admitted::Refresh => {}
            Admitted::Fulfilled => {
                // Nothing was fetched: the checkouts stand as the probe read them.
                *self.left.lock().unwrap_or_else(PoisonError::into_inner) = self
                    .probed
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                return Ok(());
            }
            Admitted::Waiting => {
                self.reading.set(Some(Reading::Waits(Wait::SharedRetry)));
                *self.aside.lock().unwrap_or_else(PoisonError::into_inner) = Some(
                    StoodAside::Waiting("repository refresh waits for its shared retry".into()),
                );
                return Ok(());
            }
            Admitted::Held => {
                self.reading.set(Some(Reading::Waits(Wait::OtherPass)));
                *self.aside.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(StoodAside::Lock(OTHER_PASS.into()));
                return Ok(());
            }
        }
        let refreshed = self.refresh_batch(sys, authorized);
        let finished = self.core.finish_refresh(sys, refreshed.is_ok());
        refreshed.and(finished)
    }

    fn refresh_batch(
        &self,
        sys: &dyn System,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        let core = &self.core;
        let recorded = super::develop_checkouts(sys, &core.user)?;
        let current = || core.batch_current(sys, authorized, &recorded);
        current()?;
        let plan = core.plan(sys).map_err(|block| block.why)?;
        if plan.empty_inventory {
            return Ok(());
        }
        // The probe's read, which the engine ran just before this refresh, where it left one.
        let probed = self
            .probed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let standing = match probed {
            Some(standing) => standing,
            None => self.read(sys).map_err(|block| block.why)?.1,
        };
        if let Lookups::Not(why) = core.lookups(sys, &plan)? {
            return Err(format!(
                "aterm's GitHub route does not hand back the synced token: {why}; no checkout \
                 was refreshed"
            ));
        }
        let writes = core.own_writes(sys);
        let refreshed = repos::refresh_existing(
            sys,
            &core.user,
            &standing,
            &recorded,
            &current,
            writes.as_ref(),
        )?;
        let mut failed = refreshed.failed;
        let mut network_failed = refreshed.network_failed;
        // A refreshed parent may declare different gitlinks. Never repair dependencies from
        // the pre-refresh parent's cached verdict.
        let left = if refreshed.moved {
            let (_, moved) = self.read(sys).map_err(|block| block.why)?;
            if let Some(why) = repos::named(&moved.git_waits) {
                failed.push(why);
            }
            match core.repair_dependencies(sys, &moved.dependencies, &current, writes.as_ref()) {
                Ok((repairs, network)) => {
                    network_failed |= network;
                    failed.extend(repairs);
                }
                Err((why, repairs)) => {
                    failed.extend(repairs);
                    failed.insert(0, why);
                    return Err(failed.join("; "));
                }
            }
            moved
        } else {
            standing
        };
        *self.left.lock().unwrap_or_else(PoisonError::into_inner) = Some(left);
        if network_failed
            && let Err(why) = super::RepositorySelection::recover_refused_credential(
                sys,
                &core.user,
                &plan.token,
                &current,
            )
        {
            failed.push(format!("credential recovery is unfinished: {why}"));
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }
}

impl Item for RepositoryRefresh {
    fn id(&self) -> String {
        refresh_id(&self.core.user.name)
    }
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn title(&self) -> String {
        "GitHub repositories already here, fast-forwarded where clean and behind".into()
    }
    fn after(&self) -> Vec<String> {
        self.core.after()
    }

    fn probe(&self, sys: &dyn System, _ctx: &Ctx) -> Probe {
        let probe = self.probe_standing(sys);
        // A refresh that will not run leaves no lease the acquisition before it held.
        if !matches!(probe, Probe::Ok { .. }) {
            self.core.end_lease();
        }
        probe
    }

    fn apply(&self, _sys: &dyn System, _ctx: &Ctx) -> Result<(), String> {
        Err("the repository refresh plans no change; it runs as a refresh".into())
    }

    fn refresh_guarded(
        &self,
        sys: &dyn System,
        _ctx: &Ctx,
        current: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Option<Result<(), String>> {
        match self.idle(sys) {
            Ok(None) => Some(self.refresh(sys, current)),
            Ok(Some(_)) => None,
            Err(why) => Some(Err(why)),
        }
    }

    fn refresh_changes(&self, sys: &dyn System, _ctx: &Ctx) -> bool {
        matches!(self.idle(sys), Ok(None))
    }

    fn stood_aside(&self) -> Option<StoodAside> {
        self.aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn read_aside(&self) -> Option<StoodAside> {
        self.lease_aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }

    /// The last row that holds the run's lease ended: whatever it did, or did not get to do
    /// (a refresh the engine did not run), the lease goes.
    fn row_ended(&self) {
        self.core.end_lease();
    }
}

impl RepositoryRefresh {
    /// The probe's reading: whether this run refreshes, and of how many checkouts. Before it
    /// reads a checkout, a run that has not refreshed yet decides the repository lease
    /// ([`NativeRepositories::lease_free`]): the one this run's acquisition left it, else its
    /// own; while another pass holds it the row stands aside on it, having read nothing. The
    /// read-back after the refresh decides nothing.
    fn probe_standing(&self, sys: &dyn System) -> Probe {
        self.reading.set(None);
        *self
            .lease_aside
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        match self.idle(sys) {
            Ok(None) => {}
            Ok(Some(why)) => return Probe::NotApplicable { why: why.into() },
            Err(why) => return Probe::Blocked { why },
        }
        if !self.ran.load(Ordering::SeqCst) && matches!(self.core.lease_free(sys), Ok(false)) {
            self.reading.set(Some(Reading::Waits(Wait::OtherPass)));
            *self
                .lease_aside
                .lock()
                .unwrap_or_else(PoisonError::into_inner) =
                Some(StoodAside::Lock(OTHER_PASS.into()));
            return Probe::Unusable {
                why: OTHER_PASS.into(),
            };
        }
        *self.probed.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let (empty_inventory, standing) = match self.probe_read(sys) {
            Ok(read) => read,
            Err(block) if block.why.is_empty() => {
                return Probe::NotApplicable {
                    why: "no checkout is refreshed while repository setup waits".into(),
                };
            }
            Err(block) => {
                return Probe::NotApplicable {
                    why: format!(
                        "no checkout is refreshed while repository setup waits: {}",
                        block.why
                    ),
                };
            }
        };
        let here = standing.present.len() + standing.dependencies.len();
        if empty_inventory || here == 0 {
            return Probe::NotApplicable {
                why: "no checkout of the private inventory was here before this run".into(),
            };
        }
        *self.probed.lock().unwrap_or_else(PoisonError::into_inner) = Some(standing);
        let checkouts = if here == 1 { "checkout" } else { "checkouts" };
        Probe::Ok {
            now: format!(
                "{here} {checkouts} here; a refresh fast-forwards each clean one that is behind \
                 its origin"
            ),
        }
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
    /// their act on settings or a destination ends the wait ([`Wait::kind`]), so its row needs
    /// them and no pass retries it, else a wait that ends by itself. For `aterm init`'s default
    /// (`unasked`), a machine the vault delivered no token to has no stage to run, which asks nobody
    /// anything; `machine` names it ([`System::machine`]). A token wait is a blocked operation,
    /// never a person's: the default vault source files its need through
    /// [`super::RepositorySelection`]'s protocol.
    fn probe(self, unasked: bool, machine: &str) -> Probe {
        match self.wait {
            Some(Wait::Token) if unasked => Probe::NotApplicable {
                why: format!(
                    "no GitHub token on this {machine} yet: the ALab vault delivers its {} entry",
                    super::TOKEN_ENTRY
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
            && !inputs.is_current(sys)?
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
                    .vault_entry(Path::new(&self.user.home), super::TOKEN_ENTRY)
                    .is_ok_and(|token| token.is_none())
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
    let configured = match asked(decode(&bytes)?.github)? {
        Asked::Scoped(config) => Some(config),
        Asked::Default | Asked::Off => None,
    };
    Ok(configured)
}

#[cfg(test)]
#[path = "native_tests.rs"]
pub(super) mod tests;
