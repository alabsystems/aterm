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
use aterm_converge::{Class, Layer, Remedy, Requires};
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

/// The automatic lane can publish a complete count only after the captured default
/// source's pinned credential route also works. Missing work is left to the native
/// batch's ordinary route check, rather than invoking the helper twice before cloning.
/// # Errors
/// The account, source, checkouts or credential route cannot be read safely, the route
/// does not return the captured token, or the captured source is no longer ready.
pub(crate) fn automatic_inventory_counts(
    sys: &dyn System,
    ctx: &Ctx,
) -> Result<Option<(usize, usize)>, String> {
    if !super::uses_vault_inventory(sys, ctx)? {
        return Ok(None);
    }
    let captured = InitialRepositoryInputs::capture_ready(sys, ctx)?;
    automatic_inventory_counts_captured(sys, ctx, &captured)
}

/// Reuse an existing captured default source. A caller's materialization fence is
/// still its responsibility; this checks the capture's own source before returning.
pub(crate) fn automatic_inventory_counts_captured(
    sys: &dyn System,
    ctx: &Ctx,
    captured: &InitialRepositoryInputs,
) -> Result<Option<(usize, usize)>, String> {
    captured.validate_default_context(sys, ctx)?;
    let local = captured.local.as_ref().map_err(|block| block.why.clone())?;
    let inventory = local
        .inventory
        .as_ref()
        .ok_or("the default repository source changed while its inputs were captured")?;
    let listed = inventory.repositories();
    let standing = repos::partition(sys, &captured.user, listed)?;
    let counts = (standing.present.len(), listed.len());
    // capture_ready released the shared materialization lock before any helper runs.
    let lookups = if counts.0 == counts.1 {
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

pub(super) fn run(sys: &dyn System, ctx: &Ctx, mode: Mode) -> Result<Vec<ItemReport>, String> {
    run_with(sys, ctx, mode, RepositoryWork::Refresh, None)
}

pub(super) fn run_captured(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    work: RepositoryWork,
    inputs: InitialRepositoryInputs,
    current: Box<dyn Fn() -> Result<bool, String> + Send + Sync>,
) -> Result<Vec<ItemReport>, String> {
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
) -> Box<dyn Item> {
    Box::new(NativeRepositories {
        after,
        unasked,
        work,
        execution,
        ..NativeRepositories::new(user.clone(), None)
    })
}

fn run_with(
    sys: &dyn System,
    ctx: &Ctx,
    mode: Mode,
    work: RepositoryWork,
    captured: Option<Captured>,
) -> Result<Vec<ItemReport>, String> {
    let [user] = ctx.users.as_slice() else {
        return Err("initial repository setup requires exactly one local user".into());
    };
    if mode == Mode::Apply {
        // What a clone killed mid-way left (design §6.5): never a clone still running.
        repos::sweep_staging(sys, user);
    }
    // The keychain block goes first, whatever GitHub says below.
    let keychain = keychain::Keychain::new(user.clone());
    let item = NativeRepositories {
        work,
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
    if mode == Mode::Apply {
        let mut journal = sys.journal()?;
        aterm_converge::run(&bound, mode, journal.as_mut())
    } else {
        aterm_converge::run(&bound, mode, &mut aterm_converge::ReadOnly)
    }
    .map_err(|why| why.to_string())
}

struct NativeRepositories {
    user: User,
    /// The caller's scheduling intent; it grants no authority over the captured source.
    work: RepositoryWork,
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
        }
    }

    fn ready(sys: &dyn System, user: &User) -> Self {
        let failed = |why: String| Self {
            user: user.clone(),
            local: Err(why.into()),
            source: None,
            authority: InputAuthority::Caller,
            execution: None,
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
        if !repos::git_installed(sys) {
            return Err(Block::of(Wait::GitReadiness)(repos::no_git(sys).into()));
        }
        repos::admit_home(sys, user)?;
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
            Some(Carried::Applied(plan)) => {
                self.ensure_current(sys).map_err(Block::from)?;
                plan
            }
            _ => self.plan(sys)?,
        };
        let standing = repos::partition(sys, &self.user, &plan.repositories)
            .map_err(Block::of(Wait::InTheWay))?;
        Ok((plan, standing))
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
        // What the last apply's clones made of their hook installers: whatever this read says,
        // the switch turned off since included, its row says that too.
        let hooks = std::mem::take(&mut *self.hooks.lock().unwrap_or_else(PoisonError::into_inner));
        let read = (|| -> Probe {
            self.reading.set(None);
            if matches!(super::settings_managed(sys, &self.user), Ok(false)) {
                return Probe::NotApplicable {
                    why: "repository setup is disabled in init.toml's [github] table".into(),
                };
            }
            let check = (|| {
                let (plan, standing) = self.read(sys)?;
                if let Some(why) = repos::named(&standing.git_waits) {
                    return Err(Block::of(Wait::GitReadiness)(why));
                }
                let lookups = self.lookups(sys, &plan)?;
                self.ensure_current(sys).map_err(Block::from)?;
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
                Ok::<_, Block>((plan, standing, lookups))
            })();
            let (plan, standing, lookups) = match check {
                Ok(read) => read,
                Err(block) => {
                    // Why it cannot go on, where this probe decided it: System State's note.
                    self.reading.set(block.wait.map(Reading::Waits));
                    return block.probe(self.unasked, sys.macos());
                }
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

    fn refresh(&self, sys: &dyn System, ctx: &Ctx) -> Option<Result<(), String>> {
        self.refresh_guarded(sys, ctx, &|| Ok(()))
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
        self.work == RepositoryWork::Refresh && matches!(configuration(sys, &self.user), Ok(None))
    }

    fn apply_guarded(
        &self,
        sys: &dyn System,
        ctx: &Ctx,
        authorized: &(dyn Fn() -> Result<(), String> + Sync),
    ) -> Result<(), String> {
        authorized()?;
        self.ensure_current(sys)?;
        let work = self.begin_refresh(sys)?;
        let result = self.apply_batch(sys, ctx, authorized, work);
        if result.is_err() {
            self.finish_refresh(sys, false)?;
        }
        result
    }

    fn detail(&self, _sys: &dyn System, _ctx: &Ctx) -> Option<String> {
        self.reading.detail()
    }
}

impl NativeRepositories {
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
        let (plan, mut standing) = match self.take() {
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
        let recorded =
            if work == RepositoryWork::Refresh && configuration(sys, &self.user)?.is_none() {
                Some(super::develop_checkouts(sys, &self.user)?)
            } else {
                None
            };
        let current = || {
            authorized()?;
            if !super::settings_managed(sys, &self.user)? {
                return Err("repository setup was disabled in init.toml while cloning".into());
            }
            self.ensure_current(sys)?;
            if let Some(recorded) = &recorded
                && *recorded != super::develop_checkouts(sys, &self.user)?
            {
                return Err(
                    "develop checkout selection changed while repositories were being refreshed"
                        .into(),
                );
            }
            Ok(())
        };
        current()?;
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
        let mut failed = Vec::new();
        if let Some(recorded) = &recorded {
            let refreshed =
                repos::refresh_existing(sys, &self.user, &standing, recorded, &current)?;
            failed.extend(refreshed.failed);
            // A refreshed parent may declare different gitlinks. Never hydrate
            // dependencies from the pre-refresh parent's cached verdict.
            if refreshed.moved {
                standing = repos::partition(sys, &self.user, &plan.repositories)?;
                if let Some(why) = repos::named(&standing.git_waits) {
                    self.reading.set(Some(Reading::Waits(Wait::GitReadiness)));
                    failed.push(why);
                }
            }
        }
        if standing.missing.is_empty() && standing.dependencies.is_empty() {
            self.carry(Carried::Applied(plan));
            return if failed.is_empty() {
                Ok(())
            } else {
                Err(failed.join("; "))
            };
        }
        let guard = repos::CloneGuard::new(&current);
        let mut cloned = repos::clone_all(sys, &self.user, &standing.missing, Some(&guard));
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
            if let Err(why) = super::dependencies::repair(sys, &self.user, path, repo, &current) {
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
        // A long batch can outlive changes to worktrees it did not touch. Completion reads
        // every local destination and required dependency again against this same inventory.
        self.carry(Carried::Applied(plan));
        if let Some(why) = room {
            // The failed row keeps the page's words for it (`init_observe::failure_note`).
            self.reading.set(Some(Reading::Waits(Wait::DiskSpace)));
            failed.push(why);
        }
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
        if self.work != RepositoryWork::Refresh || configuration(sys, &self.user)?.is_some() {
            return Ok(self.work);
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
    /// The listed repositories left out because another takes their folder
    /// ([`repos::Inventory::notes`]).
    notes: Vec<String>,
}

/// Why [`NativeRepositories::plan`] cannot go on: the engine's line, and the reason System
/// State words for a person where the step that stopped decided one. A step that decided
/// none leaves the page its own words for a waiting row.
#[derive(Clone, Debug)]
struct Block {
    why: String,
    wait: Option<Wait>,
}

impl Block {
    /// The probe's answer for this block (System State design §3.6): a person's step where only
    /// their act ends the wait ([`Wait::kind`]), so its row needs them and no pass retries it,
    /// else a wait that ends by itself. For `aterm init`'s default (`unasked`), a machine the
    /// vault wrote no token to has no stage to run, which asks nobody anything; `mac` names it.
    fn probe(self, unasked: bool, mac: bool) -> Probe {
        match self.wait {
            Some(Wait::Token) if unasked => Probe::NotApplicable {
                why: format!(
                    "no GitHub token on this {} yet: the ALab vault's sync writes ~/{} \
                     (init.toml [github] token_file names another)",
                    if mac { "Mac" } else { "machine" },
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
    fn of(wait: Wait) -> impl FnOnce(String) -> Self {
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
                repositories,
                notes: Vec::new(),
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
            repositories: listed.repositories,
            notes: listed.notes,
        })
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
pub(super) mod tests {
    use super::*;
    use crate::git_credential::GITHUB;
    use crate::git_credential::script;
    use crate::items::defaults::tests::user;
    use crate::port::{Fake, GithubResponse, Ran, Stat};

    const TOKEN: &str = "fixture_development_token_only";

    fn stat(directory: bool, mode: u32) -> Stat {
        Stat {
            uid: 501,
            gid: 20,
            mode,
            dir: directory,
            symlink: false,
            len: TOKEN.len() as u64,
        }
    }

    fn fixture() -> (Fake, NativeRepositories) {
        let (sys, item) = bare();
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        script::launcher(&sys, &item.user);
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        metadata(&sys, &item.user, aterm_json::json!([]));
        answer(
            &sys,
            &item.user,
            "/user",
            aterm_json::json!({"login":"alice"}),
        );
        (sys, item)
    }

    /// Alice's own home and nothing else: no git, no token, no GitHub answer.
    fn bare() -> (Fake, NativeRepositories) {
        let mut sys = Fake::root();
        sys.euid = 501;
        let item = NativeRepositories::new(user("alice", 501), None);
        // Child publication must not invent the account's already-existing
        // ancestry and thereby change the captured source's prefix policy.
        for path in ["/", "/Users"] {
            sys.stat_of(
                path,
                Stat {
                    uid: 0,
                    gid: 0,
                    ..stat(true, 0o755)
                },
            );
        }
        sys.stat_of(&item.user.home, stat(true, 0o700));
        (sys, item)
    }

    fn source(sys: &Fake, path: &str) {
        sys.file(path, TOKEN.as_bytes())
            .stat_of(path, stat(false, 0o600));
    }

    /// `user`'s account on `sys` listing one repository of hers, `name`, already a worktree in
    /// `~`, with her synced token and aterm's route handing it back, and the git `sys` runs (the
    /// distribution's on Linux): the stage init runs with no init.toml, converged.
    pub(in crate::init_github) fn converged(sys: &Fake, user: &User, name: &str) {
        let git = if sys.macos() {
            crate::items::github::CLT_GIT
        } else {
            crate::items::github::SYSTEM_GIT
        };
        sys.file(git, b"synthetic git");
        script::launcher(sys, user);
        sys.stat_of(&user.home, stat(true, 0o700));
        let home = Path::new(&user.home);
        source(
            sys,
            home.join(crate::init_github::TOKEN_FILE).to_str().unwrap(),
        );
        answer(sys, user, "/user", aterm_json::json!({"login": user.name}));
        answer(
            sys,
            user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository(&user.name, name, 1)]),
        );
        let path = home.join(name);
        sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
        super::repos::tests::script_existing(sys, user, &path, name);
        script::lookups_hold(sys, user, GITHUB, &user.name, TOKEN);
        script::alike(sys, user);
    }

    #[test]
    fn a_captured_local_blocker_is_not_reread_after_capture() {
        let (sys, item) = bare();
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let captured = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        // A later local reading could succeed. The frozen capture must still
        // report its original blocker, without reading files again.
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        answer(
            &sys,
            &item.user,
            "/user",
            aterm_json::json!({"login":"alice"}),
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        let rows = run_captured(
            &sys,
            &ctx,
            Mode::Plan,
            RepositoryWork::Refresh,
            captured,
            Box::new(|| Ok(true)),
        )
        .unwrap();
        assert_eq!(Reading::of(stage(&rows)), Some(Reading::Waits(Wait::Token)));
        // The vault this Mac reads holds no GitHub token: only a person puts one there.
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Person);
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    fn answer(sys: &Fake, user: &User, endpoint: &str, body: aterm_json::Value) {
        // Default fixtures represent the same complete inventory delivered privately by the
        // vault. Explicitly scoped profiles below still exercise GitHub's transport.
        if endpoint == format!("{ACCESSIBLE}&page=1") {
            metadata(sys, user, body.clone());
        }
        sys.on_github_api(
            user,
            endpoint,
            Ok(GithubResponse {
                status: 200,
                headers: Vec::new(),
                body: aterm_json::to_vec(&body).unwrap(),
            }),
        );
    }

    fn metadata(sys: &Fake, user: &User, repositories: aterm_json::Value) {
        let path = super::super::inventory::file(&user.home);
        let bytes = aterm_json::to_vec(&aterm_json::json!({
            "schema":2,"account":user.name,"repositories":repositories
        }))
        .unwrap();
        sys.file(path.to_str().unwrap(), &bytes).stat_of(
            path.to_str().unwrap(),
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
    }

    fn scope(sys: &Fake, user: &User) {
        let path = Path::new(&user.home).join(".config/aterm/init.toml");
        let bytes = format!("[github]\naccount='{}'\n", user.name);
        sys.file(path.to_str().unwrap(), bytes.as_bytes()).stat_of(
            path.to_str().unwrap(),
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
    }

    fn scoped_fixture() -> (Fake, NativeRepositories) {
        let (sys, item) = fixture();
        scope(&sys, &item.user);
        (sys, item)
    }

    fn repository(owner: &str, name: &str, id: u64) -> aterm_json::Value {
        aterm_json::json!({"id":id,"name":name,"owner":{"login":owner},"full_name":format!("{owner}/{name}"),"private":true,"clone_url":format!("https://github.com/{owner}/{name}.git"),"default_branch":"main"})
    }

    fn required_repository(sys: &Fake, who: &User, name: &str, paths: &[&str]) {
        let mut value = repository("alice", name, 1);
        value
            .as_object_mut()
            .unwrap()
            .insert("required_submodules".into(), aterm_json::json!(paths));
        metadata(sys, who, aterm_json::json!([value]));
        super::super::dependencies::tests::declarations(
            sys,
            who,
            &Path::new(&who.home).join(name),
            paths,
        );
    }

    /// Capability refusal is a local tool wait, and a ready managed Git resolves it without
    /// changing the private inventory. Dirty dependency work remains a personal conflict.
    #[test]
    fn required_dependencies_wait_for_managed_git_and_preserve_dirty_work() {
        use super::super::dependencies::tests::{PIN, existing};
        use aterm_types::macos_setup::{Setting, Standing};

        let names = ["parent".to_owned()];
        let (sys, item) = listing(&names, &names);
        sys.logged_in(&item.user);
        let parent = Path::new(&item.user.home).join("parent");
        let child = parent.join("vendor/library");
        sys.stat_of(parent.join("vendor").to_str().unwrap(), stat(true, 0o700));
        sys.stat_of(child.to_str().unwrap(), stat(true, 0o700));
        sys.file(child.join(".git").to_str().unwrap(), b"inert metadata")
            .stat_of(child.join(".git").to_str().unwrap(), stat(false, 0o600));
        required_repository(&sys, &item.user, "parent", &["vendor/library"]);
        existing(&sys, &item.user, &child, "library", PIN);
        sys.simulate(|_, command| {
            (command.program == Path::new(crate::items::github::CLT_GIT)
                && command.args.iter().any(|arg| arg == "--no-lazy-fetch"))
            .then(|| Ran::exit(129, "unknown option: --no-lazy-fetch"))
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let pending = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(stage(&pending).outcome, crate::model::Outcome::Blocked);
        assert_eq!(
            Reading::of(stage(&pending)),
            Some(Reading::Waits(Wait::GitReadiness))
        );
        assert_eq!(Wait::GitReadiness.kind(), FailureKind::Transient);
        assert_eq!(
            crate::init_observe::observation(Setting::Repositories, &[stage(&pending)]).standing,
            Standing::Waiting
        );
        let untouched = |calls: &[String]| {
            calls.iter().all(|call| {
                !call.contains(" clone ")
                    && !call.contains(" fetch ")
                    && !call.contains(" checkout ")
                    && !call.contains(" submodule ")
            })
        };
        assert!(untouched(&sys.calls()), "{:?}", sys.calls());
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains("--no-lazy-fetch") && call.ends_with(" --version"))
                .count(),
            1
        );

        let prefix = Path::new(&item.user.home).join("Library/Application Support/aterm/pkg");
        let git_store = prefix.join("store/git");
        sys.link(git_store.join("current").to_str().unwrap(), "12");
        sys.stat_of(
            git_store.join("12.ready").to_str().unwrap(),
            stat(false, 0o600),
        );
        sys.stat_of(
            git_store.join("12/bin/git").to_str().unwrap(),
            stat(false, 0o755),
        );
        sys.stat_of(prefix.join("bin/git").to_str().unwrap(), stat(false, 0o755));
        // Re-register read answers for the newly selected executable; policy stays identical.
        repos::tests::script_existing(&sys, &item.user, &parent, "parent");
        required_repository(&sys, &item.user, "parent", &["vendor/library"]);
        existing(&sys, &item.user, &child, "library", PIN);
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let ready = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(
            stage(&ready).outcome,
            crate::model::Outcome::Ok,
            "{ready:?}"
        );
        assert_eq!(
            Reading::of(stage(&ready)),
            Some(Reading::Repositories {
                present: 1,
                count: 1,
                signed_in: true
            })
        );
        assert!(untouched(&sys.calls()), "{:?}", sys.calls());

        sys.file(
            child.join("fixture.rs").to_str().unwrap(),
            b"the person's changes",
        );
        let dirty_child = child.clone();
        sys.simulate(move |_, command| {
            if command.args.iter().any(|arg| arg == "--porcelain=v1") {
                Some(Ran::ok(" M fixture.rs\0"))
            } else if command
                .args
                .iter()
                .any(|arg| arg == dirty_child.as_os_str())
                && command.args.windows(3).any(|args| {
                    args[0] == "rev-parse" && args[1] == "--verify" && args[2] == "HEAD^{commit}"
                })
            {
                Some(Ran::ok("2222222222222222222222222222222222222222"))
            } else {
                None
            }
        });
        let personal = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(
            stage(&personal).outcome,
            crate::model::Outcome::Person,
            "{personal:?}"
        );
        assert_eq!(
            crate::init_observe::observation(Setting::Repositories, &[stage(&personal)]).standing,
            Standing::NeedsYou
        );
        assert_eq!(
            sys.read_file(&child.join("fixture.rs")).unwrap(),
            b"the person's changes"
        );
        assert!(untouched(&sys.calls()), "{:?}", sys.calls());
    }

    #[test]
    fn native_bootstrap_counts_and_repairs_required_gitlinks_on_new_and_existing_parents() {
        for already_present in [false, true] {
            let names = ["parent".to_owned()];
            let (sys, item) = listing(&names, if already_present { &names } else { &[] });
            required_repository(&sys, &item.user, "parent", &["vendor/library"]);
            let parent = Path::new(&item.user.home).join("parent");
            super::super::dependencies::tests::download(
                &sys,
                &item.user,
                &parent,
                "vendor/library",
                u64::from(!already_present),
            );
            let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            assert_eq!(
                super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
                Some((0, 1))
            );
            let rows = run(&sys, &ctx, Mode::Apply).unwrap();
            assert_eq!(
                stage(&rows).outcome,
                crate::model::Outcome::Changed,
                "{already_present}: {rows:?}"
            );
            assert_eq!(
                Reading::of(stage(&rows)),
                Some(Reading::Repositories {
                    present: 1,
                    count: 1,
                    signed_in: true
                })
            );
            assert_eq!(
                super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
                Some((1, 1))
            );
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains(" clone "))
                    .count(),
                if already_present { 1 } else { 2 }
            );
            assert!(sys.stat(&parent.join("vendor/library")).is_ok());
            assert!(sys.stat(&parent.join("optional/huge")).is_err());
        }
    }

    #[test]
    fn native_dependency_failure_stays_unfinished_and_the_next_pass_repairs_only_the_child() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let names = ["parent".to_owned()];
        let (sys, item) = listing(&names, &names);
        required_repository(&sys, &item.user, "parent", &["vendor/library"]);
        let parent = Path::new(&item.user.home).join("parent");
        for sequence in 0..2 {
            super::super::dependencies::tests::download(
                &sys,
                &item.user,
                &parent,
                "vendor/library",
                sequence,
            );
        }
        let once = AtomicBool::new(false);
        sys.simulate(move |_, command| {
            (command.display().contains(" clone ") && !once.swap(true, Ordering::SeqCst))
                .then(|| Ran::exit(128, "synthetic transport unavailable"))
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Failed,
            "{rows:?}"
        );
        assert_eq!(
            super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
            Some((0, 1))
        );
        assert!(sys.stat(&parent).is_ok());
        assert!(sys.stat(&parent.join("vendor/library")).is_err());
        let rows = run(&sys, &ctx, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{rows:?}"
        );
        assert_eq!(
            super::super::automatic_inventory_counts(&sys, &ctx).unwrap(),
            Some((1, 1))
        );
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count(),
            2
        );
    }

    #[test]
    fn blocked_required_dependency_keeps_its_counts_and_names_preserved_work_not_sign_in() {
        let names = ["parent".to_owned()];
        let (sys, item) = listing(&names, &names);
        required_repository(&sys, &item.user, "parent", &["vendor/library"]);
        let vendor = Path::new(&item.user.home).join("parent/vendor");
        let path = vendor.join("library");
        sys.stat_of(vendor.to_str().unwrap(), stat(true, 0o700));
        sys.file(path.to_str().unwrap(), b"preserved owner work")
            .stat_of(path.to_str().unwrap(), stat(false, 0o600));
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        match item.probe(&sys, &ctx) {
            Probe::Person { step, how } => {
                assert_eq!(step, REPOSITORIES_STEP);
                assert!(
                    how.contains("0 of 1 repositories ready") && how.contains("preserved work"),
                    "{how}"
                );
                assert!(!how.contains("sign-in"));
                assert!(how.contains("vendor/library"), "{how}");
            }
            other => panic!("{other:?}"),
        }
        let reading = Reading::Repositories {
            present: 0,
            count: 1,
            signed_in: true,
        };
        assert_eq!(item.detail(&sys, &ctx), Some(reading.token()));
        assert_eq!(reading.word().as_deref(), Some("0 of 1"));
        assert_eq!(sys.read_file(&path).unwrap(), b"preserved owner work");
        assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
    }

    #[test]
    fn cached_preserved_dependency_work_is_rechecked_without_a_source_change_or_manual_init() {
        use crate::init::{Holds, OwedClone};
        let names = ["parent".to_owned()];
        let (sys, item) = listing(&names, &names);
        sys.logged_in(&item.user);
        required_repository(&sys, &item.user, "parent", &["vendor/library"]);
        let vendor = Path::new(&item.user.home).join("parent/vendor");
        let path = vendor.join("library");
        sys.stat_of(vendor.to_str().unwrap(), stat(true, 0o700));
        sys.file(path.to_str().unwrap(), b"owner's occupied path")
            .stat_of(path.to_str().unwrap(), stat(false, 0o600));
        super::super::dependencies::tests::download(
            &sys,
            &item.user,
            vendor.parent().unwrap(),
            "vendor/library",
            0,
        );
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let source = super::super::token_read(&sys, &ctx).unwrap().unwrap().mark;
        let state = aterm_tempfile::tempdir().unwrap();
        crate::init::owe_clone(state.path(), &Holds::NONE).unwrap();
        assert!(crate::init::clone_owed(&sys, state.path(), RepositoryWork::Refresh).is_err());
        // The persisted marker is the old untyped format, and must not be treated as an
        // eternal prohibition after the owner corrects only the local dependency path.
        let record = state
            .path()
            .join(aterm_converge::FileJournal::DIR)
            .join("clone-owed");
        let cached = std::fs::read(&record).unwrap();
        assert!(String::from_utf8_lossy(&cached).contains("\nmark "));
        assert!(matches!(
            crate::init::owed_clone_captured(
                &sys,
                state.path(),
                &Holds::NONE,
                InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
                || Ok(true),
            )
            .unwrap(),
            OwedClone::Person(_)
        ));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains(" clone ") || call.contains("agh GET"))
        );
        assert_eq!(
            std::fs::read(&record).unwrap(),
            cached,
            "re-probing is read-only"
        );
        sys.remove(&path).unwrap();
        assert_eq!(
            super::super::token_read(&sys, &ctx).unwrap().unwrap().mark,
            source
        );
        assert!(matches!(
            crate::init::owed_clone_captured(
                &sys,
                state.path(),
                &Holds::NONE,
                InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
                || Ok(true),
            )
            .unwrap(),
            OwedClone::Due(_)
        ));
        assert!(
            sys.stat(&path).is_err(),
            "a due read still changes no worktree"
        );
        crate::init::clone_owed(&sys, state.path(), RepositoryWork::Refresh).unwrap();
        assert_eq!(
            super::super::token_read(&sys, &ctx).unwrap().unwrap().mark,
            source
        );
        assert_eq!(
            crate::init::owed_clone_captured(
                &sys,
                state.path(),
                &Holds::NONE,
                InitialRepositoryInputs::capture(&sys, &ctx).unwrap(),
                || Ok(true),
            )
            .unwrap(),
            OwedClone::Verified {
                present: 1,
                total: 1
            }
        );
        assert!(!record.exists());
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count(),
            1
        );
    }

    #[test]
    fn preseeded_checkouts_do_not_verify_an_unusable_preserved_credential_helper() {
        use crate::init::{Holds, OwedClone};
        let names = ["parent".to_owned()];
        let (sys, item) = listing(&names, &names);
        sys.logged_in(&item.user);
        let launcher =
            aterm_types::installed_command::legacy(Path::new(&item.user.home))[0].clone();
        let preserved = b"preserved older command";
        sys.file(launcher.to_str().unwrap(), preserved);
        let missing = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let route_missing = Arc::clone(&missing);
        sys.simulate(move |_, command| {
            (command.display().contains("credential fill")
                && route_missing.load(std::sync::atomic::Ordering::SeqCst))
            .then(|| {
                Ran::exit(
                    128,
                    "fatal: could not read Password: terminal prompts disabled",
                )
            })
        });
        assert!(matches!(
            super::super::vault_git_credentials(&sys).unwrap(),
            crate::sync::GithubRead::NotApplicable(_)
        ));
        let state = aterm_tempfile::tempdir().unwrap();
        let owed = crate::init::owed_clone(&sys, state.path(), &Holds::NONE).unwrap();
        assert_eq!(sys.read_file(&launcher).unwrap(), preserved);
        assert!(!sys.calls().iter().any(|call| {
            call.contains(" clone ") || call.contains("agh GET") || call.contains("--unset")
        }));
        assert!(
            matches!(owed, OwedClone::Waiting(_)),
            "matching checkouts cannot verify the unusable preserved route: {owed:?}"
        );
        missing.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            crate::init::owed_clone(&sys, state.path(), &Holds::NONE).unwrap(),
            OwedClone::Verified {
                present: 1,
                total: 1
            },
            "repair of only the preserved route can now verify the same checkouts"
        );
        assert_eq!(sys.read_file(&launcher).unwrap(), preserved);
    }

    #[test]
    fn automatic_readiness_runs_no_helper_for_missing_work_or_ineligible_sources() {
        use crate::init::{Holds, OwedClone};
        use aterm_types::macos_setup::Setting;
        for choice in ["missing", "dependency", "stop", "off", "custom"] {
            let names = ["parent".to_owned()];
            let present = if choice == "missing" {
                &[][..]
            } else {
                &names[..]
            };
            let (sys, item) = listing(&names, present);
            sys.logged_in(&item.user);
            let mut holds = Holds::NONE;
            match choice {
                "dependency" => {
                    required_repository(&sys, &item.user, "parent", &["vendor/library"])
                }
                "stop" => holds.stopped = Some("setup stopped".into()),
                "off" => holds.off.set(Setting::Repositories, true),
                "custom" => scope(&sys, &item.user),
                _ => {}
            }
            sys.simulate(|_, command| {
                assert!(
                    !command.display().contains("credential fill"),
                    "no route claim is needed"
                );
                None
            });
            let state = aterm_tempfile::tempdir().unwrap();
            let owed = crate::init::owed_clone(&sys, state.path(), &holds).unwrap();
            assert!(
                match choice {
                    "missing" | "dependency" => matches!(owed, OwedClone::Due(_)),
                    "stop" | "off" => matches!(owed, OwedClone::Held(_)),
                    "custom" => matches!(owed, OwedClone::Nothing(_)),
                    _ => unreachable!(),
                },
                "{choice}: {owed:?}"
            );
        }
    }

    #[test]
    fn captured_readback_refuses_another_login_before_using_its_inputs() {
        let names = ["parent".to_owned()];
        let (mut sys, item) = listing(&names, &names);
        sys.logged_in(&item.user);
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        let other = user("other", 502);
        sys.euid = other.uid;
        sys.logged_in(&other);
        sys.simulate(|_, command| {
            assert!(!command.display().contains("credential fill"));
            None
        });
        let state = aterm_tempfile::tempdir().unwrap();
        let read = crate::init::owed_clone_captured(
            &sys,
            state.path(),
            &crate::init::Holds::NONE,
            inputs,
            || panic!("another account cannot reach the captured vault fence"),
        )
        .unwrap();
        assert!(matches!(read, crate::init::OwedClone::Waiting(_)));
    }

    struct ReadbackFixture {
        paths: aterm_secrets::client::Paths,
        source: std::path::PathBuf,
        source_text: String,
        sys: Fake,
        who: User,
        ctx: Ctx,
        token: std::path::PathBuf,
        _root: aterm_tempfile::TempDir,
    }

    fn readback_fixture(git: bool) -> ReadbackFixture {
        use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let source = root.path().join("team.secrets");
        let paths = Paths::for_home(&home);
        let inventory = aterm_json::json!({
            "schema":2,"account":"alice","repositories":[repository("alice", "parent", 1)]
        });
        let source_text = format!(
            "gh_access_token={TOKEN}\ngithub-repositories.json={inventory}\nunrelated=before\n"
        );
        aterm_uds::atomic_file::write(&source, source_text.as_bytes(), 0o600).unwrap();
        client::join(
            &paths,
            &JoinOptions {
                source: source.to_string_lossy().into_owned(),
                host: Some("fixture".into()),
                interactive: false,
                wait: std::time::Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        let who = User {
            home: home.to_string_lossy().into_owned(),
            ..user("alice", 501)
        };
        let mut sys = Fake::root();
        sys.euid = who.uid;
        sys.logged_in(&who);
        sys.stat_of(home.to_str().unwrap(), stat(true, 0o700));
        if git {
            sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        }
        let token = home.join(super::super::TOKEN_FILE);
        sys.file(token.to_str().unwrap(), TOKEN.as_bytes())
            .stat_of(token.to_str().unwrap(), stat(false, 0o600));
        metadata(&sys, &who, inventory["repositories"].clone());
        script::launcher(&sys, &who);
        script::lookups_hold(&sys, &who, GITHUB, "alice", TOKEN);
        let parent = home.join("parent");
        sys.stat_of(parent.to_str().unwrap(), stat(true, 0o700));
        repos::tests::script_existing(&sys, &who, &parent, "parent");
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&who));
        ReadbackFixture {
            paths,
            source,
            source_text,
            sys,
            who,
            ctx,
            token,
            _root: root,
        }
    }

    #[test]
    fn a_ready_vault_preserves_missing_git_and_hold_dispositions_in_captured_readback() {
        use crate::init::{Holds, OwedClone};
        use aterm_secrets::client;
        use aterm_types::macos_setup::Setting;
        for changed in ["no-git", "held-no-git", "stale-no-git"] {
            let fixture = readback_fixture(false);
            let config = client::config(&fixture.paths).unwrap().unwrap();
            let captured = client::with_materialization_ready_for_entries(
                &fixture.paths,
                &config,
                &super::super::VAULT_INPUT_ENTRIES,
                || {
                    InitialRepositoryInputs::capture(&fixture.sys, &fixture.ctx)
                        .map_err(aterm_secrets::Error::new)
                },
            )
            .unwrap()
            .unwrap();
            assert!(matches!(
                captured.inputs.local,
                Err(Block {
                    wait: Some(Wait::GitReadiness),
                    ..
                })
            ));
            let state = aterm_tempfile::tempdir().unwrap();
            let mut holds = Holds::NONE;
            holds
                .off
                .set(Setting::Repositories, changed == "held-no-git");
            let expected = crate::init::owed_clone(&fixture.sys, state.path(), &holds).unwrap();
            assert!(matches!(
                expected,
                OwedClone::Waiting(_) | OwedClone::Held(_)
            ));
            if changed == "stale-no-git" {
                aterm_uds::atomic_file::write(
                    &fixture.source,
                    fixture
                        .source_text
                        .replace(TOKEN, "fixture-rotated-token")
                        .as_bytes(),
                    0o600,
                )
                .unwrap();
                client::sync(&fixture.paths, None).unwrap();
            }
            let checked = std::cell::Cell::new(None);
            let actual = crate::init::owed_clone_captured(
                &fixture.sys,
                state.path(),
                &holds,
                captured.inputs,
                || {
                    let current = captured
                        .snapshot
                        .is_current_now(&fixture.paths)
                        .map_err(|why| why.to_string())?;
                    checked.set(Some(current));
                    Ok(current)
                },
            )
            .unwrap();
            assert_eq!(
                checked.get(),
                Some(changed != "stale-no-git"),
                "the full fence always runs"
            );
            if changed == "stale-no-git" {
                assert!(matches!(actual, OwedClone::Waiting(_)));
                assert_ne!(
                    actual, expected,
                    "a stale vault still replaces the old disposition"
                );
            } else {
                assert_eq!(actual, expected, "a current vault retains {changed}");
            }
            assert!(
                !fixture
                    .sys
                    .calls()
                    .iter()
                    .any(|call| call.contains("credential fill") || call.contains(" clone "))
            );
        }
    }

    #[test]
    fn captured_repository_readback_at_the_console_reads_no_workstation_census() {
        use aterm_secrets::client;
        let fixture = readback_fixture(true);
        fixture.sys.stat_of("/dev/console", stat(false, 0o600));
        assert!(!crate::console::remote(&fixture.sys));
        fixture.sys.on(
            &crate::facts::hidutil_keyboards().display(),
            Ran::ok("Services:\n"),
        );
        let config = client::config(&fixture.paths).unwrap().unwrap();
        let captured = client::with_materialization_ready_for_entries(
            &fixture.paths,
            &config,
            &super::super::VAULT_INPUT_ENTRIES,
            || {
                InitialRepositoryInputs::capture(&fixture.sys, &fixture.ctx)
                    .map_err(aterm_secrets::Error::new)
            },
        )
        .unwrap()
        .unwrap();
        let state = aterm_tempfile::tempdir().unwrap();
        let reading = crate::init::owed_clone_captured(
            &fixture.sys,
            state.path(),
            &crate::init::Holds::NONE,
            captured.inputs,
            || {
                captured
                    .snapshot
                    .is_current_now(&fixture.paths)
                    .map_err(|why| why.to_string())
            },
        )
        .unwrap();
        assert!(matches!(reading, crate::init::OwedClone::Verified { .. }));
        assert!(
            fixture
                .sys
                .calls()
                .iter()
                .all(|call| { !call.contains("hidutil") && call != "keyboard-service-ids" }),
            "repository readback collected workstation facts: {:?}",
            fixture.sys.calls()
        );
    }

    #[test]
    fn captured_automatic_readiness_fences_unlocked_helper_work_with_one_scoped_vault() {
        use crate::init::{Holds, OwedClone};
        use aterm_secrets::client;
        for changed in [
            "none",
            "unrelated",
            "token",
            "writer",
            "profile",
            "capture-profile",
        ] {
            let ReadbackFixture {
                paths,
                source,
                source_text,
                sys,
                who,
                ctx,
                token,
                _root,
            } = readback_fixture(true);
            if changed == "capture-profile" {
                let selected = who.clone();
                let token_path = token.clone();
                sys.on_owned_read(move |sys, path| {
                    if path == token_path {
                        scope(sys, &selected);
                    }
                });
            }
            let config = client::config(&paths).unwrap().unwrap();
            let captured = client::with_materialization_ready_for_entries(
                &paths,
                &config,
                &super::super::VAULT_INPUT_ENTRIES,
                || InitialRepositoryInputs::capture(&sys, &ctx).map_err(aterm_secrets::Error::new),
            )
            .unwrap()
            .unwrap();
            let final_paths = paths.clone();
            let selected = who.clone();
            let held = Arc::new(Mutex::new(None));
            let retain = Arc::clone(&held);
            sys.simulate(move |sys, command| {
                if command.display().contains("credential fill") {
                    // Acquiring the actual writer lock proves the helper runs outside it.
                    let writer = client::test_hold_state_lock(&paths).unwrap();
                    if changed == "writer" {
                        *retain.lock().unwrap() = Some(writer);
                    } else {
                        drop(writer);
                        if changed == "profile" {
                            scope(sys, &selected);
                        }
                        if matches!(changed, "token" | "unrelated") {
                            aterm_uds::atomic_file::write(
                                &source,
                                if changed == "token" {
                                    source_text.replace(TOKEN, "fixture-rotated-token")
                                } else {
                                    source_text.replace("unrelated=before", "unrelated=after")
                                }
                                .as_bytes(),
                                0o600,
                            )
                            .unwrap();
                            client::sync(&paths, None).unwrap();
                        }
                    }
                }
                None
            });
            let state = aterm_tempfile::tempdir().unwrap();
            let owed = crate::init::owed_clone_captured(
                &sys,
                state.path(),
                &Holds::NONE,
                captured.inputs,
                || {
                    captured
                        .snapshot
                        .is_current_now(&final_paths)
                        .map_err(|why| why.to_string())
                },
            )
            .unwrap();
            assert_eq!(
                matches!(owed, OwedClone::Verified { .. }),
                matches!(changed, "none" | "unrelated"),
                "{changed}: {owed:?}"
            );
            assert!(
                matches!(changed, "none" | "unrelated") || matches!(owed, OwedClone::Waiting(_))
            );
            // A held writer remains held until after the nonwaiting final observation.
            drop(held.lock().unwrap().take());
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains("credential fill"))
                    .count(),
                usize::from(changed != "capture-profile")
            );
            assert!(
                !sys.calls()
                    .iter()
                    .any(|call| call.contains(" clone ") || call.contains("agh GET"))
            );
        }
    }

    #[test]
    fn native_dependency_batch_checks_stop_and_real_input_changes_after_each_download() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for change in ["none", "stop", "token", "inventory"] {
            let names = ["parent".to_owned()];
            let (sys, item) = listing(&names, &names);
            required_repository(
                &sys,
                &item.user,
                "parent",
                &["vendor/first", "vendor/second"],
            );
            let parent = Path::new(&item.user.home).join("parent");
            super::super::dependencies::tests::download(
                &sys,
                &item.user,
                &parent,
                "vendor/first",
                0,
            );
            super::super::dependencies::tests::download(
                &sys,
                &item.user,
                &parent,
                "vendor/second",
                1,
            );
            let stop = Arc::new(AtomicBool::new(false));
            let pressed = Arc::clone(&stop);
            let who = item.user.clone();
            sys.simulate(move |sys, command| {
                if command.display().contains(" clone ") {
                    match change {
                        "stop" => pressed.store(true, Ordering::SeqCst),
                        "token" => {
                            sys.file(
                                &format!("{}/{}", who.home, super::super::TOKEN_FILE),
                                b"fixture_rotated_token",
                            );
                        }
                        "inventory" => metadata(sys, &who, aterm_json::json!([])),
                        _ => {}
                    }
                }
                None
            });
            let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
            let applied = item.apply_guarded(&sys, &ctx, &|| {
                if stop.load(Ordering::SeqCst) {
                    Err("late Stop".into())
                } else {
                    Ok(())
                }
            });
            assert_eq!(applied.is_ok(), change == "none", "{change}: {applied:?}");
            assert_eq!(
                sys.stat(&parent.join("vendor/first")).is_ok(),
                change == "none",
                "{change}"
            );
            assert_eq!(
                sys.stat(&parent.join("vendor/second")).is_ok(),
                change == "none",
                "{change}"
            );
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains(" clone "))
                    .count(),
                if change == "none" { 2 } else { 1 },
                "{change}"
            );
            assert!(sys.stat(&parent).is_ok());
        }
    }

    #[test]
    fn default_credential_reads_private_vault_inventory_without_github_discovery() {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([
                repository("alice", "personal", 1),
                repository("team", "work", 2),
            ]),
        );
        let plan = item.plan(&sys).unwrap();
        assert_eq!(
            plan.repositories
                .iter()
                .map(|repo| (repo.owner.as_str(), repo.name.as_str()))
                .collect::<Vec<_>>(),
            [("alice", "personal"), ("team", "work")]
        );
        assert!(sys.calls().iter().all(|call| !call.contains("/gh ")
            && !call.contains(" agh GET ")
            && !call.contains("auth login")
            && !call.contains(TOKEN)
            && !call.contains("alabsystems")
            && !call.contains("release-credentials")));
        assert!(
            sys.fed().is_empty(),
            "the complete-input pipe retains no token copy"
        );
    }

    #[test]
    fn missing_or_invalid_private_inventory_never_falls_back_to_public_discovery() {
        let (sys, item) = bare();
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        answer(
            &sys,
            &item.user,
            "/user",
            aterm_json::json!({"login":"alice"}),
        );
        let blocked = item.plan(&sys).err().unwrap();
        assert_eq!(blocked.wait, Some(Wait::RepositoryInventory));
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));

        let path = super::super::inventory::file(&item.user.home);
        sys.file(path.to_str().unwrap(), b"not JSON")
            .stat_of(path.to_str().unwrap(), stat(false, 0o600));
        assert_eq!(
            item.plan(&sys).err().unwrap().wait,
            Some(Wait::RepositoryInventory)
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    #[test]
    fn a_captured_private_inventory_stays_with_its_token_snapshot() {
        let (sys, mut item) = fixture();
        metadata(
            &sys,
            &item.user,
            aterm_json::json!([repository("alice", "first", 1)]),
        );
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        metadata(
            &sys,
            &item.user,
            aterm_json::json!([repository("alice", "later", 2)]),
        );
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        let inputs = &item.captured.as_ref().unwrap().inputs;
        assert_eq!(
            inputs
                .local
                .as_ref()
                .unwrap()
                .inventory
                .as_ref()
                .unwrap()
                .repositories()[0]
                .name,
            "first"
        );
        assert!(!inputs.is_current(&sys).unwrap());
        assert!(
            item.plan(&sys)
                .err()
                .unwrap()
                .why
                .contains("source changed")
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    #[test]
    fn captured_private_inventory_clones_in_the_accounts_background_session() {
        let names = ["background-code".to_owned()];
        let (sys, item) = listing(&names, &[]);
        let mut context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        context.console = None;
        let captured = InitialRepositoryInputs::capture(&sys, &context).unwrap();
        let rows = run_captured(
            &sys,
            &context,
            Mode::Apply,
            RepositoryWork::Refresh,
            captured,
            Box::new(|| Ok(true)),
        )
        .unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{rows:?}"
        );
        assert_eq!(
            Reading::of(stage(&rows)),
            Some(Reading::Repositories {
                present: 1,
                count: 1,
                signed_in: true
            })
        );
        assert!(sys.calls().iter().any(|call| call.contains(" clone ")));
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
    }

    #[test]
    fn a_native_repository_batch_respects_late_stop_but_not_an_explicit_older_hold() {
        use crate::init::Holds;
        use aterm_types::macos_setup::{Selection, Setting};
        use std::sync::atomic::{AtomicBool, Ordering};
        for late_stop in [false, true] {
            let repositories = names("batch", 6);
            let (sys, item) = listing(&repositories, &[]);
            let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let stop = Arc::new(AtomicBool::new(false));
            let pressed = Arc::clone(&stop);
            sys.simulate(move |_, command| {
                if late_stop && command.display().contains(" clone ") {
                    pressed.store(true, Ordering::SeqCst);
                }
                None
            });
            let mut off = Selection::none();
            off.set(Setting::Repositories, true);
            let start = Holds { off, ..Holds::NONE };
            let current = || {
                let now = Holds {
                    stopped: stop.load(Ordering::SeqCst).then(|| "late Stop".to_owned()),
                    stopped_revision: Some(2),
                    ..start.clone()
                };
                if now.later(&start, Setting::Repositories) {
                    Err(now.why(Setting::Repositories))
                } else {
                    Ok(())
                }
            };
            assert!(matches!(item.probe(&sys, &ctx), Probe::Change { .. }));
            let applied = item.apply_guarded(&sys, &ctx, &current);
            assert_eq!(applied.is_err(), late_stop, "{applied:?}");
            let published = repositories
                .iter()
                .filter(|name| sys.stat(&Path::new(&item.user.home).join(name)).is_ok())
                .count();
            assert_eq!(published, if late_stop { 0 } else { 6 });
            let clones = sys
                .calls()
                .iter()
                .filter(|call| call.contains(" clone "))
                .count();
            if late_stop {
                assert!(
                    (1..=LANES_SEEN).contains(&clones),
                    "{clones} clones started after Stop"
                );
            } else {
                assert_eq!(clones, 6);
            }
        }
    }

    #[test]
    fn scoped_vault_inputs_publish_through_unrelated_sync_but_refuse_changed_inputs() {
        use aterm_secrets::client::{self, JoinOptions, JoinPassword, Paths};
        for ordinary in [false, true] {
            for changed in ["unrelated", "token", "inventory", "missing", "incomplete"] {
                let root = aterm_tempfile::tempdir().unwrap();
                let home = root.path().join("home");
                let source = root.path().join("team.secrets");
                let paths = Paths::for_home(&home);
                let inventory = aterm_json::to_string(&aterm_json::json!({
                    "schema":2,"account":"alice","repositories":[repository("alice", "new", 1)]
                }))
                .unwrap();
                let source_text = format!(
                    "gh_access_token={TOKEN}\ngithub-repositories.json={inventory}\nunrelated=before\n"
                );
                aterm_uds::atomic_file::write(&source, source_text.as_bytes(), 0o600).unwrap();
                client::join(
                    &paths,
                    &JoinOptions {
                        source: source.to_string_lossy().into_owned(),
                        host: Some("fixture".into()),
                        interactive: false,
                        wait: std::time::Duration::ZERO,
                        confirm: &|_| false,
                        password: JoinPassword::Given(None),
                        fetched: None,
                        requested: &|| {},
                        say: &|_| {},
                    },
                )
                .unwrap();
                let who = User {
                    home: home.to_string_lossy().into_owned(),
                    ..user("alice", 501)
                };
                let mut sys = Fake::root();
                sys.euid = who.uid;
                sys.stat_of(home.to_str().unwrap(), stat(true, 0o700));
                sys.file(crate::items::github::CLT_GIT, b"synthetic git");
                let token_path = home.join(super::super::TOKEN_FILE);
                sys.file(token_path.to_str().unwrap(), TOKEN.as_bytes())
                    .stat_of(token_path.to_str().unwrap(), stat(false, 0o600));
                metadata(
                    &sys,
                    &who,
                    aterm_json::json!([repository("alice", "new", 1)]),
                );
                script::launcher(&sys, &who);
                script::lookups_hold(&sys, &who, GITHUB, "alice", TOKEN);
                script::alike(&sys, &who);
                repos::tests::script_clone_named(&sys, &who, "new", 0, false);
                repos::tests::script_existing(&sys, &who, &home.join("new"), "new");
                let mut ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&who));
                ctx.console = None;
                let configured = client::config(&paths).unwrap().unwrap();
                let captured = client::with_materialization_ready_for_entries(
                    &paths,
                    &configured,
                    &super::super::VAULT_INPUT_ENTRIES,
                    || {
                        InitialRepositoryInputs::capture(&sys, &ctx)
                            .map_err(aterm_secrets::Error::new)
                    },
                )
                .unwrap()
                .unwrap();
                let syncing = paths.clone();
                let once = std::sync::atomic::AtomicBool::new(false);
                sys.simulate(move |_, command| {
                    if command.display().contains(" clone ")
                        && !once.swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        let next = match changed {
                            "unrelated" => {
                                source_text.replace("unrelated=before", "unrelated=after")
                            }
                            "token" => source_text.replace(TOKEN, "fixture-rotated-token"),
                            "inventory" => source_text.replace(&inventory, "{}"),
                            "missing" => source_text
                                .lines()
                                .filter(|line| !line.starts_with("github-repositories.json="))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            "incomplete" => {
                                std::fs::create_dir_all(syncing.home.join(".config/gh/hosts.yml"))
                                    .unwrap();
                                format!("{source_text}~/.config/gh/hosts.yml=cannot-materialize\n")
                            }
                            _ => unreachable!(),
                        };
                        aterm_uds::atomic_file::write(&source, next.as_bytes(), 0o600).unwrap();
                        client::sync(&syncing, None).unwrap();
                    }
                    None
                });
                let rows = if ordinary {
                    run(&sys, &ctx, Mode::Apply).unwrap()
                } else {
                    run_captured(
                        &sys,
                        &ctx,
                        Mode::Apply,
                        RepositoryWork::Refresh,
                        captured.inputs,
                        Box::new(move || {
                            captured
                                .snapshot
                                .is_current(&paths)
                                .map_err(|why| why.to_string())
                        }),
                    )
                    .unwrap()
                };
                let published = changed == "unrelated";
                assert_eq!(
                    stage(&rows).outcome == crate::model::Outcome::Changed,
                    published,
                    "{changed}: {rows:?}"
                );
                assert_eq!(sys.stat(&home.join("new")).is_ok(), published, "{changed}");
                assert_eq!(
                    sys.calls()
                        .iter()
                        .filter(|call| call.contains(" clone "))
                        .count(),
                    1,
                    "{changed}"
                );
            }
        }
    }

    #[test]
    fn ordinary_local_repository_batches_refuse_input_changes_and_allow_unrelated_settings() {
        use std::sync::atomic::{AtomicBool, Ordering};
        for change in ["none", "inventory", "token", "profile", "unrelated"] {
            let names = names("local", 6);
            let (sys, item) = listing(&names, &[]);
            let who = item.user.clone();
            let profile = format!("{}/.config/aterm/init.toml", who.home);
            sys.file(&profile, b"").stat_of(
                &profile,
                Stat {
                    len: 0,
                    ..stat(false, 0o600)
                },
            );
            let once = AtomicBool::new(false);
            let boundary = Mutex::new(());
            sys.simulate(move |sys, command| {
                if command.display().contains(" clone ") {
                    let _mutation = boundary.lock().unwrap();
                    if once.swap(true, Ordering::SeqCst) {
                        return None;
                    }
                    match change {
                        "inventory" => metadata(sys, &who, aterm_json::json!([])),
                        "token" => {
                            sys.file(
                                &format!("{}/{}", who.home, super::super::TOKEN_FILE),
                                b"fixture_rotated_local_token",
                            );
                        }
                        "profile" | "unrelated" => {
                            let path = format!("{}/.config/aterm/init.toml", who.home);
                            let bytes = if change == "profile" {
                                b"[github]\naccount='alice'\nowner='team'\n".as_slice()
                            } else {
                                b"[develop]\ncheckouts=[]\n".as_slice()
                            };
                            sys.file(&path, bytes).stat_of(
                                &path,
                                Stat {
                                    len: bytes.len() as u64,
                                    ..stat(false, 0o600)
                                },
                            );
                        }
                        _ => {}
                    }
                }
                None
            });
            let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &ctx, Mode::Apply).unwrap();
            let complete = matches!(change, "none" | "unrelated");
            assert_eq!(
                stage(&rows).outcome == crate::model::Outcome::Changed,
                complete,
                "{change}: {rows:?}"
            );
            assert_eq!(
                names
                    .iter()
                    .filter(|name| sys.stat(&Path::new(&item.user.home).join(name)).is_ok())
                    .count(),
                if complete { 6 } else { 0 },
                "{change}"
            );
        }
    }

    #[test]
    fn an_unready_lazy_capture_can_retry_but_a_successful_generation_cannot_rebase() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (sys, item) = fixture();
        let path = Path::new(&item.user.home).join(super::super::TOKEN_FILE);
        let path_for_hook = path.clone();
        let changed = AtomicBool::new(false);
        sys.on_owned_read(move |sys, path| {
            if path == path_for_hook && !changed.swap(true, Ordering::SeqCst) {
                sys.file(path.to_str().unwrap(), b"fixture_changed_while_capturing");
            }
        });
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(
            matches!(item.probe(&sys, &ctx), Probe::Blocked { .. }),
            "a mixed capture must remain unfinished"
        );
        assert!(
            item.prepared.get().is_none(),
            "an unready capture must not be cached"
        );
        assert!(item.plan(&sys).is_ok(), "a coherent retry may be admitted");
        sys.file(path.to_str().unwrap(), TOKEN.as_bytes());
        assert!(
            item.plan(&sys).is_err(),
            "a successful capture never silently adopts a new generation"
        );
    }

    /// A private inventory that cannot place every intended repository in home never declares
    /// the retained subset complete, even when one matching checkout is already present.
    #[test]
    fn colliding_private_inventory_leaves_every_checkout_untouched_and_unfinished() {
        use super::repos::tests::script_existing;
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([
                repository("team", "Shared", 1),
                repository("alice", "shared", 2),
            ]),
        );
        let path = Path::new(&item.user.home).join("shared");
        sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
        script_existing(&sys, &item.user, &path, "shared");
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Blocked,
            "{rows:?}"
        );
        assert_eq!(
            Reading::of(stage(&rows)),
            Some(Reading::Waits(Wait::RepositoryInventory))
        );
        assert!(sys.calls().iter().all(|call| !call.contains(" clone ")
            && !call.contains("--replace-all")
            && !call.contains("credential approve")));
    }

    #[test]
    fn native_inventory_follows_agh_pagination_before_any_mutation() {
        let (sys, item) = scoped_fixture();
        let page = (0..30)
            .map(|index| repository("alice", &format!("repo-{index}"), index + 1))
            .collect::<Vec<_>>();
        let next = format!("https://api.github.com{ACCESSIBLE}&page=2");
        sys.on_github_api(
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            Ok(GithubResponse {
                status: 200,
                headers: vec![(
                    "Link".into(),
                    format!("<{next}>; rel=\"next\", <{next}>; rel=\"last\""),
                )],
                body: aterm_json::to_vec(&page).unwrap(),
            }),
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=2"),
            aterm_json::json!([repository("alice", "work", 31)]),
        );
        let plan = item.plan(&sys).unwrap();
        assert_eq!(plan.repositories.len(), 31);
        assert_eq!(plan.repositories.last().unwrap().owner, "alice");
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve") && !call.contains(" clone "))
        );
    }

    #[test]
    fn failed_native_inventory_never_persists_credentials_or_clones() {
        let (sys, item) = scoped_fixture();
        sys.on_github_api(
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            Err("synthetic incomplete GitHub response".into()),
        );
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(
            item.apply(&sys, &context)
                .unwrap_err()
                .contains("incomplete")
        );
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve")
                    && !call.contains("--replace-all")
                    && !call.contains(" clone "))
        );
    }

    #[test]
    fn explicit_admitted_profile_uses_its_source_and_scope_and_refuses_identity_mismatch() {
        let (sys, item) = fixture();
        let profile = "/Users//alice/.config/aterm/init.toml";
        let bytes = b"[github]\naccount='alice'\nowner='team'\ntoken_file='~/.secrets/custom_development'\n";
        sys.file(profile, bytes).stat_of(
            profile,
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
        source(&sys, "/Users//alice/.secrets/custom_development");
        answer(
            &sys,
            &item.user,
            "/users/team",
            aterm_json::json!({"login":"team","type":"Organization"}),
        );
        answer(
            &sys,
            &item.user,
            "/orgs/team/repos?type=all&per_page=30&sort=full_name&direction=asc&page=1",
            aterm_json::json!([repository("team", "work", 1)]),
        );
        assert_eq!(item.plan(&sys).unwrap().repositories[0].owner, "team");
        assert!(!sys.calls().iter().any(|call| call.contains(ACCESSIBLE)));

        let (sys, item) = fixture();
        let bytes = b"[github]\naccount='someone-else'\nowner='someone-else'\ntoken_file='~/.secrets/gh_access_token'\n";
        sys.file(profile, bytes).stat_of(
            profile,
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
        let block = item.plan(&sys).err().unwrap();
        assert!(block.why.contains("different configured"));
        assert!(!block.why.contains(TOKEN));
        assert_eq!(block.wait, Some(Wait::OtherAccount));
        assert!(
            !sys.calls()
                .iter()
                .any(|call| call.contains("/repos?") || call.contains("credential approve"))
        );
    }

    /// Every blocked step's reason is decided where it stopped, never from the engine's line, and
    /// the Repositories row's note is its words; a step that decides none gives no note. Where
    /// only a person ends it (a sign-in, aterm's GitHub settings, a folder in the way, Apple's
    /// Command Line Tools), the step is theirs and the row needs them, with their remedy.
    #[test]
    fn every_blocked_step_says_why_in_system_states_words() {
        use crate::init_observe::observation;
        use aterm_types::macos_setup::{Setting, Standing};
        let profile = "/Users//alice/.config/aterm/init.toml";
        let with_profile = |sys: &Fake, bytes: &[u8]| {
            sys.file(profile, bytes).stat_of(
                profile,
                Stat {
                    len: bytes.len() as u64,
                    ..stat(false, 0o600)
                },
            );
        };
        // Git and the token, without GitHub's answer to `/user`.
        let signed_out = || {
            let (sys, item) = bare();
            sys.file(crate::items::github::CLT_GIT, b"synthetic git");
            source(&sys, "/Users//alice/.secrets/gh_access_token");
            scope(&sys, &item.user);
            (sys, item)
        };
        let cases: Vec<(&str, (Fake, NativeRepositories), Option<Wait>)> = vec![
            ("no git", bare(), Some(Wait::GitReadiness)),
            (
                "no token yet",
                {
                    let (sys, item) = bare();
                    sys.file(crate::items::github::CLT_GIT, b"synthetic git");
                    (sys, item)
                },
                Some(Wait::Token),
            ),
            (
                "a token others can read",
                {
                    let (sys, item) = signed_out();
                    sys.stat_of("/Users//alice/.secrets/gh_access_token", stat(false, 0o644));
                    (sys, item)
                },
                Some(Wait::TokenUnusable),
            ),
            (
                "GitHub unreached",
                {
                    let (sys, item) = signed_out();
                    sys.fail_github_api(&item.user, "/user", GithubFailure::Unreached);
                    (sys, item)
                },
                Some(Wait::Unreached),
            ),
            (
                "a token GitHub refuses",
                {
                    let (sys, item) = signed_out();
                    sys.fail_github_api(&item.user, "/user", GithubFailure::Unauthenticated);
                    (sys, item)
                },
                Some(Wait::TokenRefused),
            ),
            (
                "GitHub's other failure",
                {
                    let (sys, item) = signed_out();
                    sys.fail_github_api(&item.user, "/user", GithubFailure::Other);
                    (sys, item)
                },
                Some(Wait::GitHub),
            ),
            (
                "another account",
                {
                    let (sys, item) = fixture();
                    with_profile(
                        &sys,
                        b"[github]\naccount='someone-else'\nowner='someone-else'\ntoken_file='~/.secrets/gh_access_token'\n",
                    );
                    (sys, item)
                },
                Some(Wait::OtherAccount),
            ),
            (
                "settings it cannot read",
                {
                    let (sys, item) = fixture();
                    with_profile(&sys, b"[github]\nowner='alice'\n");
                    (sys, item)
                },
                Some(Wait::Settings),
            ),
            (
                "the inventory unreached",
                {
                    let (sys, item) = scoped_fixture();
                    sys.fail_github_api(
                        &item.user,
                        &format!("{ACCESSIBLE}&page=1"),
                        GithubFailure::Unreached,
                    );
                    (sys, item)
                },
                Some(Wait::Unreached),
            ),
            (
                "no room on the disk",
                {
                    let (mut sys, item) = fixture();
                    sys.volume = Some(volume(9 * GIB));
                    answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
                    script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
                    script::alike(&sys, &item.user);
                    (sys, item)
                },
                Some(Wait::DiskSpace),
            ),
            (
                "another login",
                {
                    let (mut sys, item) = fixture();
                    sys.euid = 502;
                    (sys, item)
                },
                None,
            ),
        ];
        for (case, (sys, item), wait) in cases {
            // The pass System State's reading runs: a plan.
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, Mode::Plan).unwrap();
            // Only a person ends a wait for their sign-in, settings or folder: their step, which
            // parks a run (design §3.6); GitHub not answering ends by itself.
            let person = wait.is_some_and(|wait| wait.kind() == FailureKind::NeedsPerson);
            let (outcome, standing) = if person {
                (crate::model::Outcome::Person, Standing::NeedsYou)
            } else {
                (crate::model::Outcome::Blocked, Standing::Waiting)
            };
            assert_eq!(stage(&rows).outcome, outcome, "{case}");
            assert_eq!(
                Reading::of(stage(&rows)),
                wait.map(Reading::Waits),
                "{case}: {rows:?}"
            );
            let seen = observation(Setting::Repositories, &[stage(&rows)]);
            assert_eq!(seen.standing, standing, "{case}");
            assert_eq!(seen.note.as_deref(), wait.map(Wait::note), "{case}");
            let line = stage(&rows).why.as_deref().unwrap();
            assert!(
                seen.note.as_deref().is_none_or(|note| !note.contains(line)),
                "{case}"
            );
        }
    }

    /// A step that fails as it applies keeps the reason it decided (System State design §3.6):
    /// GitHub not answering as the run lists the repositories leaves the failed row the page's
    /// words for it (`init_observe::failure_note`), never GitHub's own.
    #[test]
    fn an_apply_github_did_not_answer_keeps_the_reason_it_decided() {
        let (sys, item) = bare();
        sys.file(crate::items::github::CLT_GIT, b"synthetic git");
        source(&sys, "/Users//alice/.secrets/gh_access_token");
        scope(&sys, &item.user);
        sys.fail_github_api(&item.user, "/user", GithubFailure::Unreached);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(item.apply(&sys, &context).is_err(), "GitHub did not answer");
        assert_eq!(
            item.detail(&sys, &context),
            Some(Reading::Waits(Wait::Unreached).token())
        );
    }

    /// git's settings with the keychain block an earlier `aterm init` wrote, for the keychain
    /// row's probe and apply, then the vault's alone, and the block's replacement answering. The
    /// writes, in the order they run.
    fn keychain_block_then_vault(sys: &Fake, user: &User) -> Vec<String> {
        script::keychain_block(sys, user, "alice");
        script::keychain_block(sys, user, "alice");
        script::alike(sys, user);
        script::replaces_keychain_block(sys, user)
    }

    /// The stage's own row: the keychain row comes before it ([`run_with`]).
    fn stage(rows: &[crate::model::ItemReport]) -> &crate::model::ItemReport {
        rows.last().expect("the stage's row")
    }

    /// `[github] enabled = false` holds on this entry point too, which System State reaches with
    /// no look at init.toml: the block stays, and nothing writes to git's settings. An init.toml
    /// that cannot be admitted says nothing either way, so the row cannot read and writes nothing.
    #[test]
    fn the_off_switch_keeps_the_keychain_block_on_the_stages_own_run() {
        for (profile, off) in [
            ("[github]\nenabled = false\n", true),
            ("[github]\nowner = \"alice\"\n", false),
        ] {
            let (sys, item) = fixture();
            answer(
                &sys,
                &item.user,
                &format!("{ACCESSIBLE}&page=1"),
                aterm_json::json!([]),
            );
            keychain_block_then_vault(&sys, &item.user);
            sys.write_user_file(
                &item.user,
                &Path::new(&item.user.home).join(".config/aterm/init.toml"),
                profile.as_bytes(),
                0o600,
            )
            .unwrap();
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, Mode::Apply);
            let calls = sys.calls();
            assert!(
                calls
                    .iter()
                    .all(|call| !call.contains("--unset-all") && !call.contains(" --add ")),
                "{profile}: {calls:?}"
            );
            if off {
                let rows = rows.unwrap();
                assert_eq!(
                    rows[0].outcome,
                    crate::model::Outcome::NotApplicable,
                    "{rows:?}"
                );
                assert_eq!(stage(&rows).outcome, crate::model::Outcome::NotApplicable);
                assert!(
                    !sys.calls()
                        .iter()
                        .any(|call| call.contains(" clone ") || call.contains(" agh GET "))
                );
            }
        }
    }

    /// The keychain block an earlier `aterm init` wrote gives way before anything of the stage
    /// runs, and whatever becomes of the stage: with no token on the Mac, and with GitHub out of
    /// reach. The login keychain is never asked, nothing is approved into it, and the token
    /// enters no command record.
    #[test]
    fn the_keychain_block_gives_way_first_whatever_the_token_or_github_say() {
        type Setup = fn() -> (Fake, NativeRepositories);
        let cases: [(&str, Setup); 2] = [
            ("no token", || {
                let (sys, item) = bare();
                sys.file(crate::items::github::CLT_GIT, b"synthetic git");
                script::launcher(&sys, &item.user);
                (sys, item)
            }),
            ("GitHub out of reach", || {
                let (sys, item) = scoped_fixture();
                sys.fail_github_api(
                    &item.user,
                    &format!("{ACCESSIBLE}&page=1"),
                    crate::port::GithubFailure::Unreached,
                );
                (sys, item)
            }),
        ];
        for (label, setup) in cases {
            let (sys, item) = setup();
            let writes = keychain_block_then_vault(&sys, &item.user);
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, Mode::Apply).unwrap();
            assert_eq!(
                rows[0].outcome,
                crate::model::Outcome::Changed,
                "{label}: {rows:?}"
            );
            assert_ne!(
                stage(&rows).outcome,
                crate::model::Outcome::Changed,
                "{label}"
            );
            let calls = sys.calls();
            let at = |key: &str| calls.iter().position(|call| call == key);
            for write in &writes {
                assert!(at(write).is_some(), "{label}: {calls:?}");
            }
            if let Some(asked) = calls.iter().position(|call| call.contains("agh GET")) {
                assert!(at(&writes[writes.len() - 1]) < Some(asked), "{calls:?}");
            }
            assert!(
                calls.iter().all(|call| !call.contains(TOKEN)
                    && !call.contains("/gh ")
                    && !call.contains("credential fill")
                    && !call.contains("credential approve")
                    && !call.contains("credential-osxkeychain")
                    && !call.contains("--replace-all")
                    && !call.contains("auth login")),
                "{label}: {calls:?}"
            );
            assert!(sys.fed().is_empty(), "{label}");
        }
    }

    /// aterm's route answers past a keychain helper it did not write (CLT's own, with no vault
    /// reset): the stage needs nothing of the person's git settings, which stay as they are.
    #[test]
    fn a_keychain_helper_aterm_did_not_write_neither_stops_the_stage_nor_is_asked() {
        let context = crate::items::defaults::tests::ctx(&[user("alice", 501)]);
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::unconfigured(&sys, &item.user);
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::NotApplicable);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        let calls = sys.calls();
        assert!(
            calls.iter().all(|call| !call.contains("--unset-all")
                && !call.contains("--add")
                && !call.contains("credential approve")
                && !call.contains("--global")
                && !call.contains("osxkeychain")),
            "{calls:?}"
        );
    }

    #[test]
    fn an_apply_journals_in_the_journal_of_the_system_it_runs_on_and_a_plan_in_none() {
        let journaled = |sys: &Fake| -> Vec<String> {
            sys.written(Fake::JOURNAL)
                .map(|bytes| String::from_utf8(bytes).unwrap())
                .unwrap_or_default()
                .lines()
                .map(|line| {
                    let line: aterm_json::Value = aterm_json::from_str(line).unwrap();
                    line["id"].as_str().unwrap().to_owned()
                })
                .collect()
        };
        for mode in [Mode::Plan, Mode::Apply] {
            let (sys, item) = fixture();
            answer(
                &sys,
                &item.user,
                &format!("{ACCESSIBLE}&page=1"),
                aterm_json::json!([]),
            );
            keychain_block_then_vault(&sys, &item.user);
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, mode).unwrap();
            let want = if mode == Mode::Apply {
                (
                    crate::model::Outcome::Changed,
                    vec![format!("init.github.keychain@{}", item.user.name)],
                )
            } else {
                (crate::model::Outcome::Planned, Vec::new())
            };
            assert_eq!(
                (rows[0].outcome, journaled(&sys)),
                want,
                "{mode:?}: {rows:?}"
            );
        }
    }

    /// aterm's route is read, never rewritten: one that hands back the synced token is ready,
    /// and one that hands back another, or none, stops the row, which says why, with nothing
    /// written and every worktree in place: no stage is done while a clone would fail. Nothing
    /// goes into the keychain or rewrites git's settings.
    #[test]
    fn lookups_are_read_and_never_rewritten() {
        let context = |item: &NativeRepositories| {
            crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user))
        };
        let unwritten = |sys: &Fake| {
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("credential approve")
                        && !call.contains("--global")
                        && !call.contains("--unset-all")
                        && !call.contains("--replace-all")),
                "{:?}",
                sys.calls()
            );
        };
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        match item.probe(&sys, &context(&item)) {
            Probe::Ok { now } => assert!(now.contains("hands back the synced token"), "{now}"),
            other => panic!("{other:?}"),
        }
        item.apply(&sys, &context(&item)).unwrap();
        unwritten(&sys);

        // Lookups that hand back a token the vault rotated out, and ones that hand back none.
        for (held, said) in [
            (Some("a_rotated_out_value"), "another credential"),
            (None, "hands back no token"),
        ] {
            let (sys, item) = fixture();
            answer(
                &sys,
                &item.user,
                &format!("{ACCESSIBLE}&page=1"),
                aterm_json::json!([]),
            );
            match held {
                Some(held) => script::lookups_hold(&sys, &item.user, GITHUB, "alice", held),
                None => script::lookups_hold_nothing(&sys, &item.user),
            }
            match item.probe(&sys, &context(&item)) {
                Probe::Blocked { why } => {
                    assert!(why.contains(said), "{why}");
                    assert!(!why.contains("a_rotated_out_value"), "{why}");
                }
                other => panic!("{other:?}"),
            }
            unwritten(&sys);
        }
    }

    const GIB: u64 = aterm_capacity::floor::GIB;

    /// A data volume of 100 GiB with `free` bytes free and a floor of 8 GiB.
    fn volume(free: u64) -> aterm_capacity::reading::Volume {
        aterm_capacity::reading::Volume {
            mount: "/System/Volumes/Data".into(),
            total: 100 * GIB,
            free,
            floor: 8 * GIB,
            room: 8 * GIB,
        }
    }

    /// One repository GitHub sizes at 2 GiB, whose clone takes about 10 GiB.
    fn huge() -> aterm_json::Value {
        aterm_json::json!([{"id":1,"name":"huge","owner":{"login":"alice"},
            "full_name":"alice/huge","private":true,
            "clone_url":"https://github.com/alice/huge.git",
            "default_branch":"main","size":2 * 1024 * 1024}])
    }

    /// The first-launch inventory leaves a repository the disk has no room for above the disk
    /// watch's floor and names it; with no reading of the volume it tries the clone.
    #[test]
    fn a_repository_the_disk_has_no_room_for_is_not_cloned() {
        // One that fits beside one that does not: the plan changes, so the one that fits arrives.
        let (mut sys, item) = fixture();
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        sys.volume = Some(volume(12 * GIB));
        let mut both = huge();
        both.as_array_mut()
            .unwrap()
            .push(repository("alice", "small", 2));
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), both);
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let probed = item.probe(&sys, &context);
        assert!(matches!(probed, Probe::Change { .. }), "{probed:?}");

        for read in [true, false] {
            let (mut sys, item) = fixture();
            sys.volume = read.then(|| volume(12 * GIB));
            answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
            script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
            script::alike(&sys, &item.user);
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            // Room comes back by itself, so the row waits and the run tries again; it never
            // parks the run as a person's step would (design §3.6).
            let probed = item.probe(&sys, &context);
            assert_eq!(matches!(probed, Probe::Blocked { .. }), read, "{probed:?}");
            let why = item.apply(&sys, &context).unwrap_err();
            let tried = sys.calls().iter().any(|call| call.contains("alice/huge"));
            if read {
                assert_eq!(
                    why,
                    "1 repository not cloned, for want of disk space: ~/huge (about 10 GiB); \
                     /System/Volumes/Data has 12 GiB free and keeps 8 GiB free; free space, \
                     then press Check again in System State, or launch aterm again"
                );
                assert!(!tried, "{:?}", sys.calls());
            } else {
                assert!(tried, "{why}: {:?}", sys.calls());
            }
        }

        // The disk fills between the plan and the pass: the failed row keeps the reason the
        // pass decided, for the page (`init_observe::failure_note`).
        let (mut sys, item) = fixture();
        sys.volume = Some(volume(60 * GIB));
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let probed = item.probe(&sys, &context);
        assert!(matches!(probed, Probe::Change { .. }), "{probed:?}");
        sys.volume = Some(volume(12 * GIB));
        assert!(item.apply(&sys, &context).is_err());
        assert_eq!(
            item.detail(&sys, &context),
            Some(Reading::Waits(Wait::DiskSpace).token())
        );

        // A vault snapshot that went stale outranks the room: the run waits for the new one,
        // whose repositories and token may differ, instead of for disk space.
        let (mut sys, mut item) = fixture();
        sys.volume = Some(volume(12 * GIB));
        answer(&sys, &item.user, &format!("{ACCESSIBLE}&page=1"), huge());
        let inputs = InitialRepositoryInputs::capture(&sys, &context).unwrap();
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        let sys = Arc::new(sys);
        let observed = Arc::clone(&sys);
        // Stale from the sign-in lookup on, which the plan's own checks come before.
        item.captured = Some(Captured {
            inputs,
            current: Box::new(move || {
                Ok(!observed
                    .calls()
                    .iter()
                    .any(|call| call.contains("credential fill")))
            }),
        });
        match item.probe(&*sys, &context) {
            Probe::Blocked { why } => assert!(why.contains("vault changed"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(item.detail(&*sys, &context), None);
    }

    #[test]
    fn lookups_that_hand_back_another_token_stop_before_any_clone_and_withhold_output() {
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository("alice", "new", 1)]),
        );
        script::alike(&sys, &item.user);
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", "unrelated_private_value");
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let why = item.apply(&sys, &context).unwrap_err();
        assert!(why.contains("does not hand back the synced token"), "{why}");
        assert!(!why.contains("unrelated_private_value") && !why.contains(TOKEN));
        assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
    }

    #[test]
    fn captured_repository_inputs_do_not_reread_a_new_synced_token() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        let token_path = format!("{}/.secrets/gh_access_token", item.user.home);
        let newer = "fixture_partial_sync_token_only";
        sys.file(&token_path, newer.as_bytes()).stat_of(
            &token_path,
            Stat {
                len: newer.len() as u64,
                ..stat(false, 0o600)
            },
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        let inputs = &item.captured.as_ref().unwrap().inputs;
        assert_eq!(
            inputs.local.as_ref().unwrap().token.bytes(),
            TOKEN.as_bytes()
        );
        assert!(!inputs.is_current(&sys).unwrap());
        assert!(
            item.plan(&sys)
                .err()
                .unwrap()
                .why
                .contains("source changed")
        );
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(TOKEN) && !call.contains(newer))
        );
    }

    #[test]
    fn a_vault_version_changed_during_inventory_refuses_all_local_effects() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        let checked = std::sync::atomic::AtomicUsize::new(0);
        item.captured = Some(Captured {
            inputs,
            current: Box::new(move || {
                Ok(checked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0)
            }),
        });
        let why = item.apply(&sys, &ctx).unwrap_err();
        assert!(why.contains("changed"), "{why}");
        assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve")
                    && !call.contains("--global")
                    && !call.contains(" clone "))
        );
    }

    #[test]
    fn a_ready_captured_run_without_clones_preserves_compatible_live_helpers() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        item.apply(&sys, &ctx).unwrap();
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains("credential approve") && !call.contains("--global"))
        );
    }

    /// A captured run's clones go through aterm's route and never write the login keychain.
    #[test]
    fn captured_clones_never_write_the_login_keychain() {
        let (sys, mut item) = fixture();
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([repository("alice", "new", 1)]),
        );
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        item.captured = Some(Captured {
            inputs,
            current: Box::new(|| Ok(true)),
        });
        // The clone itself is not scripted: it stops at its first git run.
        assert!(item.apply(&sys, &ctx).is_err());
        let calls = sys.calls();
        assert!(
            calls.iter().all(|call| !call.contains("credential approve")
                && !call.contains("osxkeychain")
                && !call.contains(TOKEN)
                && !call.contains("--global")
                && !call.contains(" clone ")),
            "{calls:?}"
        );
    }

    /// A Mac whose GitHub account lists `listed` (each alice's, sized `0`, in GitHub's order),
    /// of which `present` are already worktrees in `~` and every other one is scripted to clone
    /// into the staging directories a pass makes in list order; aterm's route hands back the
    /// synced token.
    fn listing(listed: &[String], present: &[String]) -> (Fake, NativeRepositories) {
        listing_with(listed, present, &[])
    }

    /// [`listing`], with the worktrees named in `theirs` cloned from someone else's origin.
    fn listing_with(
        listed: &[String],
        present: &[String],
        theirs: &[&str],
    ) -> (Fake, NativeRepositories) {
        use super::repos::tests::{script_clone_named, script_existing};
        let (sys, item) = fixture();
        let home = Path::new(&item.user.home);
        let values: Vec<aterm_json::Value> = listed
            .iter()
            .zip(1..)
            .map(|(name, id)| repository("alice", name, id))
            .collect();
        metadata(&sys, &item.user, aterm_json::Value::Array(values));
        let mut sequence = 0;
        for name in listed {
            let path = home.join(name);
            if theirs.contains(&name.as_str()) || present.contains(name) {
                sys.stat_of(path.to_str().unwrap(), stat(true, 0o700));
            } else if !repos::HOME_FOLDERS
                .iter()
                .any(|folder| folder.eq_ignore_ascii_case(name))
            {
                script_clone_named(&sys, &item.user, name, sequence, false);
                sequence += 1;
            }
            script_existing(&sys, &item.user, &path, name);
        }
        if !theirs.is_empty() {
            let foreign: Vec<_> = theirs
                .iter()
                .map(|name| {
                    (
                        crate::checkout::git(&sys, &item.user, Some(&home.join(name)))
                            .args(["config", "--get-all", "remote.origin.url"])
                            .display(),
                        format!("https://github.com/someone/{name}.git\n"),
                    )
                })
                .collect();
            sys.simulate(move |_, command| {
                let key = command.display();
                foreign
                    .iter()
                    .find(|(expected, _)| expected == &key)
                    .map(|(_, origin)| Ran::ok(origin))
            });
        }
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        script::alike(&sys, &item.user);
        (sys, item)
    }

    fn names(prefix: &str, count: usize) -> Vec<String> {
        (0..count)
            .map(|index| format!("{prefix}-{index:03}"))
            .collect()
    }

    /// Default setup must refresh a clean acquired checkout, even when its local
    /// origin refs still describe the old commit. Only the transport port substitutes
    /// this private bare origin; the item admits the normal private GitHub identity.
    /// Independent stores overlap within four lanes, while source withdrawal,
    /// storage replacement and offline failures stop queued work without moving it.
    #[test]
    fn default_apply_refreshes_a_clean_checkout_beyond_its_cached_origin() {
        use crate::port::Live;
        for case in [
            "clean",
            "dirty",
            "topic",
            "recorded",
            "shallow",
            "partial",
            "retargeted",
            "late retargeted",
            "changed source",
            "late recorded",
            "unchanged develop",
            "parallel",
            "parallel stopped",
            "parallel rebound",
            "parallel offline",
            "serial unknown",
            "serial shared",
        ] {
            let live = Live::new();
            let repos = crate::checkout::tests::Repos::new(&live, "native-refresh");
            let before = repos.head(&repos.work);
            let mut works = vec![repos.work.clone()];
            let parallel = case.starts_with("parallel");
            let multiple = parallel || case.starts_with("serial");
            if multiple {
                for index in 1..6 {
                    let path = Path::new(&repos.who.home).join(format!("work{index}"));
                    repos.clone_into(&repos.origin, &path);
                    works.push(path);
                }
            }
            for path in &works {
                let name = path.file_name().unwrap().to_str().unwrap();
                let origin = format!("https://github.com/{}/{name}.git", repos.who.name);
                repos.g(path, &["remote", "set-url", "origin", &origin]);
            }
            let tip = repos.advance("new-source.txt", "newly published source\n");
            match case {
                "dirty" => std::fs::write(repos.work.join("README"), b"preserved edits\n").unwrap(),
                "topic" => {
                    repos.g(&repos.work, &["switch", "--quiet", "-c", "topic"]);
                }
                "shallow" => {
                    std::fs::write(repos.work.join(".git/shallow"), format!("{before}\n")).unwrap()
                }
                "partial" => {
                    repos.g(
                        &repos.work,
                        &["config", "extensions.partialClone", "origin"],
                    );
                }
                "retargeted" => {
                    repos.g(&repos.work, &["config", "branch.main.remote", "other"]);
                }
                _ => {}
            }
            assert_eq!(repos.head(&repos.work), before);
            assert_eq!(
                repos.g(&repos.work, &["rev-parse", "refs/remotes/origin/main"]),
                before
            );

            let mut sys = Fake::root();
            sys.euid = repos.who.uid;
            sys.logged_in(&repos.who)
                .stat_of(
                    &repos.who.home,
                    Stat {
                        uid: repos.who.uid,
                        gid: repos.who.gid,
                        ..stat(true, 0o700)
                    },
                )
                .file(crate::items::github::CLT_GIT, b"selected fixture git")
                .stat_of(
                    repos.work.to_str().unwrap(),
                    Stat {
                        uid: repos.who.uid,
                        gid: repos.who.gid,
                        ..stat(true, 0o700)
                    },
                );
            for work in &works {
                for path in [work.clone(), work.join(".git"), work.join(".git/objects")] {
                    sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
                    if case != "serial unknown" {
                        let subject = if case == "serial shared" && path.ends_with(".git/objects") {
                            repos.work.join(".git/objects")
                        } else {
                            path.clone()
                        };
                        sys.file_id_of(&path, live.file_id(&subject).unwrap());
                    }
                }
            }
            script::launcher(&sys, &repos.who);
            source(
                &sys,
                Path::new(&repos.who.home)
                    .join(super::super::TOKEN_FILE)
                    .to_str()
                    .unwrap(),
            );
            metadata(
                &sys,
                &repos.who,
                aterm_json::Value::Array(
                    works
                        .iter()
                        .enumerate()
                        .map(|(index, path)| {
                            repository(
                                &repos.who.name,
                                path.file_name().unwrap().to_str().unwrap(),
                                index as u64 + 1,
                            )
                        })
                        .collect(),
                ),
            );
            if case == "recorded" {
                let path = Path::new(&repos.who.home).join(".config/aterm/init.toml");
                let bytes = b"[develop]\ncheckouts=['~/work']\n";
                sys.file(path.to_str().unwrap(), bytes).stat_of(
                    path.to_str().unwrap(),
                    Stat {
                        uid: repos.who.uid,
                        gid: repos.who.gid,
                        len: bytes.len() as u64,
                        ..stat(false, 0o600)
                    },
                );
            }
            script::lookups_hold(&sys, &repos.who, GITHUB, &repos.who.name, TOKEN);
            script::alike(&sys, &repos.who);
            let paths = works.clone();
            let bare = repos.origin.clone();
            let inventory = super::super::inventory::file(&repos.who.home);
            let profile = Path::new(&repos.who.home).join(".config/aterm/init.toml");
            let (uid, gid) = (repos.who.uid, repos.who.gid);
            let who = repos.who.clone();
            let gate = Arc::new((Mutex::new((0_usize, 0_usize)), std::sync::Condvar::new()));
            let seen = Arc::clone(&gate);
            sys.simulate(move |sys, command| {
                let work = paths
                    .iter()
                    .find(|path| command.args.iter().any(|arg| arg == path.as_os_str()))?;
                let mut command = command.clone();
                let remote = command.args.iter().any(|arg| arg == "--symref");
                if remote {
                    if multiple {
                        let (lock, turn) = &*seen;
                        let mut state = lock.lock().unwrap();
                        state.0 += 1;
                        state.1 = state.1.max(state.0);
                        turn.notify_all();
                        if parallel {
                            let (state, waited) = turn
                                .wait_timeout_while(
                                    state,
                                    std::time::Duration::from_secs(60),
                                    |state| state.1 < LANES_SEEN,
                                )
                                .unwrap();
                            assert!(
                                !waited.timed_out(),
                                "four repository refreshes never ran at once"
                            );
                            drop(state);
                        }
                    }
                    for arg in &mut command.args {
                        if arg == "origin" {
                            *arg = bare.as_os_str().to_owned();
                        }
                    }
                } else if command.args.iter().any(|arg| arg == "fetch") {
                    for arg in &mut command.args {
                        if arg == "origin" {
                            *arg = bare.as_os_str().to_owned();
                        }
                    }
                    command
                        .args
                        .push("+refs/heads/*:refs/remotes/origin/*".into());
                }
                let ran = if remote && case == "parallel offline" {
                    let mut ran = Ran::exit(128, "");
                    ran.stderr = b"Could not resolve host: fixture.invalid".to_vec();
                    ran
                } else {
                    Live::new().run_complete_stdout(&command).unwrap()
                };
                if remote && multiple {
                    seen.0.lock().unwrap().0 -= 1;
                }
                if remote && case == "parallel stopped" {
                    sys.file(
                        inventory.to_str().unwrap(),
                        b"source withdrawn during remote read",
                    );
                }
                if remote && case == "parallel rebound" {
                    let subject = work.join(".git/objects");
                    let mut identity = Live::new().file_id(&subject).unwrap();
                    identity.ino = identity.ino.wrapping_add(1);
                    sys.file_id_of(&subject, identity);
                }
                if case == "late retargeted" && command.args.iter().any(|arg| arg == "--symref") {
                    let live = Live::new();
                    let changed = live
                        .run_complete_stdout(&crate::checkout::git(&live, &who, Some(work)).args([
                            "config",
                            "branch.main.remote",
                            "other",
                        ]))
                        .unwrap();
                    assert!(changed.success());
                }
                if case == "changed source" && command.args.iter().any(|arg| arg == "--symref") {
                    sys.file(
                        inventory.to_str().unwrap(),
                        b"source withdrawn during remote read",
                    );
                }
                if matches!(case, "late recorded" | "unchanged develop")
                    && command.args.iter().any(|arg| arg == "--symref")
                {
                    let bytes = if case == "late recorded" {
                        b"[develop]\ncheckouts=['~/work']\n".as_slice()
                    } else {
                        b"[develop]\ncheckouts=[]\n".as_slice()
                    };
                    sys.file(profile.to_str().unwrap(), bytes).stat_of(
                        profile.to_str().unwrap(),
                        Stat {
                            uid,
                            gid,
                            len: bytes.len() as u64,
                            ..stat(false, 0o600)
                        },
                    );
                }
                Some(ran)
            });
            let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&repos.who));
            let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
            let check = run_captured(
                &sys,
                &ctx,
                Mode::Plan,
                RepositoryWork::Refresh,
                inputs.clone(),
                Box::new(|| Ok(true)),
            )
            .unwrap();
            assert_eq!(
                stage(&check).outcome,
                crate::model::Outcome::Ok,
                "{check:?}"
            );
            assert_eq!(repos.head(&repos.work), before);
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("--symref") && !call.contains(" fetch "))
            );

            // A checkout-only repair wake uses the same real runner but never forces
            // fresh remote heads of already acquired parents.
            let repaired = run_captured(
                &sys,
                &ctx,
                Mode::Apply,
                RepositoryWork::Repair,
                inputs.clone(),
                Box::new(|| Ok(true)),
            )
            .unwrap();
            assert_eq!(
                stage(&repaired).outcome,
                crate::model::Outcome::Ok,
                "{case}: {repaired:?}"
            );
            assert_eq!(repos.head(&repos.work), before);
            assert!(
                sys.calls()
                    .iter()
                    .all(|call| !call.contains("--symref") && !call.contains(" fetch "))
            );

            let applied = run_captured(
                &sys,
                &ctx,
                Mode::Apply,
                RepositoryWork::Refresh,
                inputs,
                Box::new(|| Ok(true)),
            )
            .unwrap();
            if matches!(
                case,
                "parallel stopped" | "parallel rebound" | "parallel offline"
            ) {
                assert_eq!(
                    stage(&applied).outcome,
                    crate::model::Outcome::Failed,
                    "{case}: {applied:?}"
                );
                assert_eq!(gate.0.lock().unwrap().1, LANES_SEEN);
                assert_eq!(
                    sys.calls()
                        .iter()
                        .filter(|call| call.contains("--symref"))
                        .count(),
                    LANES_SEEN,
                    "no queued repository starts after the batch stops"
                );
                assert!(
                    sys.calls()
                        .iter()
                        .all(|call| !call.contains(" fetch ") && !call.contains(" merge "))
                );
                for path in &works {
                    assert_eq!(repos.head(path), before);
                }
                continue;
            }
            if matches!(case, "changed source" | "late recorded" | "late retargeted") {
                assert_eq!(
                    stage(&applied).outcome,
                    crate::model::Outcome::Failed,
                    "{applied:?}"
                );
                assert!(sys.calls().iter().any(|call| call.contains("--symref")));
                assert!(
                    sys.calls()
                        .iter()
                        .all(|call| !call.contains(" fetch ") && !call.contains(" merge "))
                );
                assert_eq!(repos.head(&repos.work), before);
                continue;
            }
            assert_eq!(
                stage(&applied).outcome,
                crate::model::Outcome::Ok,
                "{applied:?}"
            );
            assert_eq!(
                repos.head(&repos.work),
                if matches!(case, "clean" | "unchanged develop") || multiple {
                    tip.clone()
                } else {
                    before.clone()
                },
                "default Apply must refresh acquired clean code"
            );
            if multiple {
                assert_eq!(
                    gate.0.lock().unwrap().1,
                    if parallel { LANES_SEEN } else { 1 }
                );
                for path in &works {
                    assert_eq!(repos.head(path), tip);
                    assert!(repos.status(path).is_empty());
                }
            }
            if !matches!(case, "clean" | "unchanged develop") && !multiple {
                assert!(
                    sys.calls().iter().all(|call| !call.contains("--symref")
                        && !call.contains(" fetch ")
                        && !call.contains(" merge ")),
                    "{case}: {:?}",
                    sys.calls()
                );
            }
            if case == "dirty" {
                assert_eq!(
                    std::fs::read(repos.work.join("README")).unwrap(),
                    b"preserved edits\n"
                );
            } else {
                assert!(repos.status(&repos.work).is_empty());
            }
        }
    }

    #[test]
    fn node_and_subscriber_share_a_source_refresh_wave_using_the_alignment_record() {
        use super::super::RepositoryRefreshCursor;
        use crate::port::Live;
        use crate::sync::repository_refresh::{Execution, RepositoryPass, observe};
        use aterm_secrets::converge::{RefreshRequest, RepositoryRequest};

        let live = Live::new();
        let repos = crate::checkout::tests::Repos::new(&live, "native-shared-refresh");
        let first = repos.head(&repos.work);
        repos.g(
            &repos.work,
            &[
                "remote",
                "set-url",
                "origin",
                &format!("https://github.com/{}/work.git", repos.who.name),
            ],
        );
        let second = repos.advance("second.txt", "source two\n");
        assert_eq!(repos.head(&repos.work), first);
        assert_eq!(
            repos.g(&repos.work, &["rev-parse", "refs/remotes/origin/main"]),
            first
        );

        let mut sys = Fake::root();
        sys.euid = repos.who.uid;
        sys.logged_in(&repos.who)
            .stat_of(
                &repos.who.home,
                Stat {
                    uid: repos.who.uid,
                    gid: repos.who.gid,
                    ..stat(true, 0o700)
                },
            )
            .file(crate::items::github::CLT_GIT, b"selected fixture git")
            .stat_of(
                repos.work.to_str().unwrap(),
                Stat {
                    uid: repos.who.uid,
                    gid: repos.who.gid,
                    ..stat(true, 0o700)
                },
            );
        for path in [repos.work.join(".git"), repos.work.join(".git/objects")] {
            sys.stat_of(path.to_str().unwrap(), live.stat(&path).unwrap());
        }
        script::launcher(&sys, &repos.who);
        source(
            &sys,
            Path::new(&repos.who.home)
                .join(super::super::TOKEN_FILE)
                .to_str()
                .unwrap(),
        );
        metadata(
            &sys,
            &repos.who,
            aterm_json::json!([repository(&repos.who.name, "work", 1)]),
        );
        script::lookups_hold(&sys, &repos.who, GITHUB, &repos.who.name, TOKEN);
        script::alike(&sys, &repos.who);
        let work = repos.work.clone();
        let bare = repos.origin.clone();
        sys.simulate(move |_, command| {
            if !command.args.iter().any(|arg| arg == work.as_os_str()) {
                return None;
            }
            let mut command = command.clone();
            let fetching = command.args.iter().any(|arg| arg == "fetch");
            if fetching || command.args.iter().any(|arg| arg == "--symref") {
                for arg in &mut command.args {
                    if arg == "origin" {
                        *arg = bare.as_os_str().to_owned();
                    }
                }
                if fetching {
                    command
                        .args
                        .push("+refs/heads/*:refs/remotes/origin/*".into());
                }
            }
            Some(Live::new().run_complete_stdout(&command).unwrap())
        });

        let state = aterm_tempfile::tempdir().unwrap();
        let now = sys.now().saturating_mul(1_000);
        assert!(crate::sync::read_record(state.path()).is_none());
        // Receipt samples precede both producers' initial actual source readings.
        let node_before = observe(state.path(), now);
        let subscriber_before = observe(state.path(), now);
        let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&repos.who));
        let inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        let scope = inputs.refresh_scope(&sys).unwrap();
        let initial_mark = super::super::repository_input_mark(&sys).unwrap();
        assert_eq!(
            initial_mark.refresh_scope(),
            Some(scope),
            "the producer and the effect runner derive the same production scope"
        );
        let mut node_cursor = RepositoryRefreshCursor::new(node_before, Some(&initial_mark));
        let mut subscriber_cursor =
            RepositoryRefreshCursor::new(subscriber_before, Some(&initial_mark));
        let Some(RepositoryRequest::Refresh(node)) =
            node_cursor.observe(Some(&initial_mark), node_before, None)
        else {
            panic!("the node's first actual source ensures freshness");
        };
        let Some(RepositoryRequest::Refresh(subscriber)) =
            subscriber_cursor.observe(Some(&initial_mark), subscriber_before, None)
        else {
            panic!("the subscriber's first actual source ensures freshness");
        };
        let apply = |inputs: InitialRepositoryInputs, request: RefreshRequest| {
            let access = RepositoryPass::open(state.path(), now).unwrap();
            let pass = access
                .pass
                .expect("the prior owner released the file lease");
            let rows = run_captured(
                &sys,
                &ctx,
                Mode::Apply,
                RepositoryWork::Refresh,
                inputs.with_repository_execution(Some(Execution {
                    request: RepositoryRequest::Refresh(request),
                    pass: Arc::clone(&pass),
                })),
                Box::new(|| Ok(true)),
            )
            .unwrap();
            (rows, pass.record())
        };
        let network = || {
            let calls = sys.calls();
            (
                calls
                    .iter()
                    .filter(|call| call.contains("--symref"))
                    .count(),
                calls.iter().filter(|call| call.contains(" fetch ")).count(),
            )
        };
        let (rows, empty) = apply(inputs.clone(), node);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(repos.head(&repos.work), second);
        assert_eq!(network(), (1, 1));
        assert_eq!(empty.build, "");
        assert_eq!(empty.spec, 0);
        assert_eq!(empty.attempted_ms, 0);
        assert_eq!(empty.converged_ms, None);
        assert!(empty.builds.is_empty() && empty.routes.is_empty() && empty.rows.is_empty());
        assert_eq!(empty.base, None);
        assert_eq!(
            crate::sync::due(Some(&empty), "future-build", &[], now),
            crate::sync::Due::NewBuild
        );

        // Repository-only completion cannot stamp the generic pass's build, routes or base.
        let expected = {
            let pass = RepositoryPass::open(state.path(), now)
                .unwrap()
                .pass
                .unwrap();
            let generic = crate::sync::Record {
                build: "kept-build".into(),
                spec: crate::sync::SPEC,
                builds: vec!["earlier-build".into()],
                attempted_ms: now - 1_000,
                converged_ms: Some(now - 500),
                routes: vec!["192.0.2.1".into()],
                rows: vec![crate::sync::RecordRow {
                    id: "kept-row".into(),
                    outcome: crate::model::Outcome::Ok,
                    why: Some("kept result".into()),
                }],
                base: Some(crate::sync::BaseRecord {
                    build: "kept-base".into(),
                    pending: vec!["kept-item".into()],
                }),
                repositories: None,
            };
            pass.record_pass(generic).unwrap();
            pass.record()
        };
        let (rows, shared) = apply(inputs.clone(), subscriber);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(repos.head(&repos.work), second);
        assert_eq!(
            network(),
            (1, 1),
            "the follower performs no second remote wave"
        );
        assert_eq!(shared, expected);

        let latest = observe(state.path(), now);
        assert!(
            node_cursor
                .observe(Some(&initial_mark), latest, None)
                .is_none()
        );
        assert!(
            subscriber_cursor
                .observe(Some(&initial_mark), latest, None)
                .is_none()
        );
        let forced = RefreshRequest::after(Some(scope), latest.version);
        let third = repos.advance("third.txt", "source three\n");
        let (rows, refreshed) = apply(inputs.clone(), forced);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(repos.head(&repos.work), third);
        assert_eq!(
            network(),
            (2, 2),
            "a later explicit boundary refreshes the same source again"
        );
        let generic = |mut record: crate::sync::Record| {
            record.repositories = None;
            record
        };
        assert_eq!(generic(refreshed.clone()), generic(expected.clone()));
        assert_eq!(
            super::super::repository_input_mark(&sys)
                .unwrap()
                .refresh_scope(),
            Some(scope)
        );

        let before_change = observe(state.path(), now);
        metadata(
            &sys,
            &repos.who,
            aterm_json::json!([repository(&repos.who.name, "work", 2)]),
        );
        let changed = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        let changed_scope = changed.refresh_scope(&sys).unwrap();
        assert_ne!(changed_scope, scope);
        let changed_mark = super::super::repository_input_mark(&sys).unwrap();
        assert_eq!(changed_mark.refresh_scope(), Some(changed_scope));
        let fourth = repos.advance("fourth.txt", "source four\n");
        let (rows, refused) = apply(changed.clone(), forced);
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Failed,
            "{rows:?}"
        );
        assert_eq!(repos.head(&repos.work), third);
        assert_eq!(
            network(),
            (2, 2),
            "an old request cannot refresh a new admitted source"
        );
        assert_eq!(refused, refreshed);
        let Some(RepositoryRequest::Refresh(changed_request)) = node_cursor.observe(
            Some(&changed_mark),
            before_change,
            Some(RepositoryWork::Refresh),
        ) else {
            panic!("the node observes the actual source transition");
        };
        // A→B→A before any admission still owes a new generation when its bytes return.
        metadata(
            &sys,
            &repos.who,
            aterm_json::json!([repository(&repos.who.name, "work", 1)]),
        );
        let returned_mark = super::super::repository_input_mark(&sys).unwrap();
        let Some(RepositoryRequest::Refresh(returned)) = node_cursor.observe(
            Some(&returned_mark),
            before_change,
            Some(RepositoryWork::Refresh),
        ) else {
            panic!("returning to the earlier bytes retains the transition");
        };
        let unadmitted = changed_request.merge(returned);
        assert_eq!(unadmitted.scope, Some(scope));
        assert!(
            !before_change.fulfills(unadmitted),
            "ABA cannot inherit the old A fulfillment"
        );
        metadata(
            &sys,
            &repos.who,
            aterm_json::json!([repository(&repos.who.name, "work", 2)]),
        );
        let final_mark = super::super::repository_input_mark(&sys).unwrap();
        let Some(RepositoryRequest::Refresh(final_request)) = node_cursor.observe(
            Some(&final_mark),
            before_change,
            Some(RepositoryWork::Refresh),
        ) else {
            panic!("the final B source is the desired generation");
        };
        let changed_request = unadmitted.merge(final_request);
        let (rows, changed_record) = apply(changed.clone(), changed_request);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(repos.head(&repos.work), fourth);
        assert_eq!(network(), (3, 3));
        assert!(observe(state.path(), now).fulfills(changed_request));

        // This producer sees B only after the node already fulfilled B. Its old actual
        // source's epoch must bind the request, not the just-read B receipt's newer epoch.
        let after_change = observe(state.path(), now);
        let Some(RepositoryRequest::Refresh(delayed)) = subscriber_cursor.observe(
            Some(&final_mark),
            after_change,
            Some(RepositoryWork::Refresh),
        ) else {
            panic!("the delayed subscriber also observes the actual transition");
        };
        assert_eq!(delayed, changed_request);
        assert!(delayed.base_input < after_change.version.input);
        let (rows, delayed_record) = apply(changed, delayed);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(repos.head(&repos.work), fourth);
        assert_eq!(
            network(),
            (3, 3),
            "the delayed actual source observer shares the completed B wave"
        );
        assert_eq!(delayed_record, changed_record);
        assert_eq!(generic(changed_record), generic(expected.clone()));

        // A semantic develop selection can move A→B→A while this observer's actual
        // source stays A. A successful A sample against B's newer receipt owes A now
        // and raises the floor for a subsequent B transition.
        let profile = Path::new(&repos.who.home).join(".config/aterm/init.toml");
        let set_develop = |checkouts: &str| {
            let bytes = format!("[develop]\ncheckouts={checkouts}\n");
            sys.file(profile.to_str().unwrap(), bytes.as_bytes())
                .stat_of(
                    profile.to_str().unwrap(),
                    Stat {
                        uid: repos.who.uid,
                        gid: repos.who.gid,
                        len: bytes.len() as u64,
                        ..stat(false, 0o600)
                    },
                );
        };
        let before_semantic = observe(state.path(), now);
        set_develop("[]");
        let semantic_a = super::super::repository_input_mark(&sys).unwrap();
        assert_eq!(semantic_a.refresh_scope(), Some(changed_scope));
        let mut late = RepositoryRefreshCursor::new(before_semantic, Some(&semantic_a));
        let Some(RepositoryRequest::Refresh(initial)) =
            late.observe(Some(&semantic_a), before_semantic, None)
        else {
            panic!("the late actor first ensures the actual A generation");
        };
        assert!(before_semantic.fulfills(initial));
        assert!(
            node_cursor
                .observe(Some(&semantic_a), before_semantic, None)
                .is_none()
        );

        set_develop("['~/parked']");
        let semantic_b = super::super::repository_input_mark(&sys).unwrap();
        let semantic_inputs = InitialRepositoryInputs::capture(&sys, &ctx).unwrap();
        assert_eq!(
            semantic_b.refresh_scope(),
            Some(semantic_inputs.refresh_scope(&sys).unwrap())
        );
        assert_ne!(semantic_b.refresh_scope(), semantic_a.refresh_scope());
        let Some(RepositoryRequest::Refresh(winner)) = node_cursor.observe(
            Some(&semantic_b),
            before_semantic,
            Some(RepositoryWork::Refresh),
        ) else {
            panic!("the node refreshes the new semantic selection");
        };
        let before_network = network();
        let (rows, _) = apply(semantic_inputs.clone(), winner);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(network().0, before_network.0 + 1);
        let winner_receipt = observe(state.path(), now);
        assert!(winner_receipt.fulfills(winner));

        set_develop("[]");
        let still_a = super::super::repository_input_mark(&sys).unwrap();
        assert_eq!(still_a.refresh_scope(), semantic_a.refresh_scope());
        let Some(RepositoryRequest::Refresh(missed_a)) =
            late.observe(Some(&still_a), winner_receipt, None)
        else {
            panic!("unchanged actual A against a newer foreign B receipt owes A immediately");
        };
        assert_eq!(missed_a.scope, semantic_a.refresh_scope());
        assert_eq!(missed_a.base_input, winner_receipt.version.input);
        set_develop("['~/parked']");
        let back_to_b = super::super::repository_input_mark(&sys).unwrap();
        let Some(RepositoryRequest::Refresh(new_b)) = late.observe(
            Some(&back_to_b),
            winner_receipt,
            Some(RepositoryWork::Refresh),
        ) else {
            panic!("the later B transition retains the newer receipt floor");
        };
        let requested = missed_a.merge(new_b);
        assert!(
            !winner_receipt.fulfills(requested),
            "the previous B wave cannot acknowledge a later A→B transition"
        );
        let before_network = network();
        let (rows, semantic_record) = apply(semantic_inputs.clone(), requested);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(network().0, before_network.0 + 1);
        assert!(observe(state.path(), now).fulfills(requested));
        assert_eq!(generic(semantic_record), generic(expected));

        // An owned record reset cannot make a retained input/attempt fence require
        // repeated remote waves merely to count back up to its old epochs.
        let before_reset = observe(state.path(), now);
        let retained_scope = semantic_inputs.refresh_scope(&sys).unwrap();
        let retained = RefreshRequest::inputs(retained_scope, before_reset.version).merge(
            RefreshRequest::after(Some(retained_scope), before_reset.version),
        );
        {
            let _fence = RepositoryPass::open(state.path(), now)
                .unwrap()
                .pass
                .unwrap();
            std::fs::remove_file(
                state
                    .path()
                    .join(aterm_converge::FileJournal::DIR)
                    .join(crate::sync::RECORD),
            )
            .unwrap();
        }
        assert!(crate::sync::read_record(state.path()).is_none());
        let fifth = repos.advance("fifth.txt", "source five\n");
        let before_network = network();
        let (rows, reset_record) = apply(semantic_inputs.clone(), retained);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(repos.head(&repos.work), fifth);
        assert_eq!(network(), (before_network.0 + 1, before_network.1 + 1));
        let reset_receipt = observe(state.path(), now);
        assert!(
            reset_receipt.fulfills(retained),
            "one wave exceeds both retained epoch fences after reset"
        );
        assert!(reset_receipt.version.input > retained.base_input);
        assert!(reset_receipt.version.attempt > retained.after_attempt.unwrap());
        assert_eq!(generic(reset_record.clone()), generic(empty));
        assert_eq!(
            crate::sync::due(Some(&reset_record), "kept-build", &[], now),
            crate::sync::Due::NewBuild
        );
        let before_network = network();
        let (rows, reset_follower) = apply(semantic_inputs, retained);
        assert_eq!(stage(&rows).outcome, crate::model::Outcome::Ok, "{rows:?}");
        assert_eq!(network(), before_network);
        assert_eq!(reset_follower, reset_record);
    }

    #[test]
    fn private_inventory_branch_history_does_not_strand_new_checkouts() {
        for cached in [aterm_json::json!("old-default"), aterm_json::Value::Null] {
            let (sys, item) = listing(&["new".into()], &[]);
            let mut value = repository("alice", "new", 1);
            value
                .as_object_mut()
                .unwrap()
                .insert("default_branch".into(), cached);
            metadata(&sys, &item.user, aterm_json::json!([value]));
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, Mode::Apply).unwrap();
            assert_eq!(
                stage(&rows).outcome,
                crate::model::Outcome::Changed,
                "{rows:?}"
            );
            assert_eq!(
                Reading::of(stage(&rows)),
                Some(Reading::Repositories {
                    present: 1,
                    count: 1,
                    signed_in: true
                })
            );
            assert!(
                sys.stat(&Path::new(&item.user.home).join("new"))
                    .unwrap()
                    .dir
            );
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains(" clone "))
                    .count(),
                1
            );
            assert!(!sys.calls().iter().any(|call| call.contains(" agh GET ")));
        }
    }

    /// How many of `calls` ran git as alice in `path` with `args`.
    fn ran_in(calls: &[String], path: &Path, args: &str) -> usize {
        let at = format!("-C {} {args}", path.display());
        calls.iter().filter(|call| call.ends_with(&at)).count()
    }

    const TOP_LEVEL: &str = "rev-parse --is-inside-work-tree --show-prefix --show-toplevel";

    /// One whole pass over 50 repositories already in `~` and 10 to clone: private metadata is
    /// captured once, and completion rechecks all worktrees locally after cloning. Each
    /// missing repository has one remote inventory and one clone; local route admission is
    /// repeated before network operations.
    #[test]
    fn a_whole_pass_reuses_private_inventory_but_rechecks_all_local_worktrees() {
        let (present, missing) = (names("repo", 50), names("new", 10));
        let listed: Vec<String> = present.iter().chain(&missing).cloned().collect();
        let (sys, item) = listing(&listed, &present);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Changed,
            "{rows:?}"
        );
        assert_eq!(
            stage(&rows).to.as_deref(),
            Some("60 matching worktrees present; aterm's GitHub route hands back the synced token")
        );
        let calls = sys.calls();
        let count = |what: &str| calls.iter().filter(|call| call.ends_with(what)).count();
        assert_eq!(count(" agh GET /user"), 0, "{calls:?}");
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains(" agh GET /user/repos"))
                .count(),
            0,
            "repository inventory is internal vault metadata"
        );
        let home = Path::new(&item.user.home);
        for name in &present {
            let path = home.join(name);
            assert_eq!(ran_in(&calls, &path, TOP_LEVEL), 2, "{name}");
            assert_eq!(
                ran_in(&calls, &path, "config --get-all remote.origin.url"),
                2,
                "{name}"
            );
        }
        for name in &missing {
            assert_eq!(ran_in(&calls, &home.join(name), TOP_LEVEL), 1, "{name}");
        }
        assert_eq!(
            calls
                .iter()
                .filter(|call| call.contains("ls-remote --symref"))
                .count(),
            missing.len(),
            "{calls:#?}"
        );
        assert_eq!(
            calls.iter().filter(|call| call.contains(" clone ")).count(),
            missing.len(),
            "{calls:#?}"
        );
    }

    /// The native stage's guards meet the installers: a guard that stops the rest once its clone
    /// ran its installer fails the row with that outcome after why, and a switch turned off
    /// before the read-back answers not applicable with it. NEGATIVE CONTROL: the switch read
    /// before the outcomes are taken drops the line from that answer.
    #[test]
    fn a_guard_or_switch_after_the_clone_still_says_what_its_installer_did() {
        use std::sync::atomic::{AtomicBool, Ordering};
        const INSTALLED: &str = "Trust git hooks: installed (core.hooksPath = scripts/hooks)";
        let says = format!("$HOME/trust: ran its {}: {INSTALLED:?}", repos::HOOK_INSTALLER);
        for stop in [true, false] {
            let (sys, item) = listing(&["trust".to_owned()], &[]);
            let home = Path::new(&item.user.home);
            let (staged, clone) = repos::tests::clone_key(&sys, &item.user, "trust", 0);
            sys.on_stat(&clone, &staged.join("scripts"), stat(true, 0o755));
            sys.on_write(
                &clone,
                &staged.join(repos::HOOK_INSTALLER),
                b"#!/usr/bin/env bash\n",
            );
            let installer =
                repos::installer_command(&sys, &item.user, &home.join("trust")).display();
            sys.on(&installer, Ran::ok(&format!("{INSTALLED}\n")));
            let ran = Arc::new(AtomicBool::new(false));
            let seen = Arc::clone(&ran);
            sys.simulate(move |_, command| {
                if command.display() == installer {
                    seen.store(true, Ordering::SeqCst);
                }
                None
            });
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            assert!(matches!(item.probe(&sys, &context), Probe::Change { .. }));
            let authorized = || {
                if stop && ran.load(Ordering::SeqCst) {
                    Err("late Stop".to_owned())
                } else {
                    Ok(())
                }
            };
            let applied = item.apply_guarded(&sys, &context, &authorized);
            assert!(ran.load(Ordering::SeqCst), "the installer ran");
            if stop {
                let why = applied.unwrap_err();
                assert_eq!(why, format!("late Stop; {says}"));
                continue;
            }
            applied.unwrap();
            sys.write_user_file(
                &item.user,
                &home.join(".config/aterm/init.toml"),
                b"[github]\nenabled = false\n",
                0o600,
            )
            .unwrap();
            match item.probe(&sys, &context) {
                Probe::NotApplicable { why } => assert_eq!(
                    why,
                    format!("repository setup is disabled in init.toml's [github] table; {says}")
                ),
                other => panic!("{other:?}"),
            }
        }
    }

    /// The stage's row says what a clone it made did with the hook installer it carries: the
    /// read-back's line ends with the installer's own first line, and a failing installer leaves
    /// the stage changed, its clone in place. CONTROL: a clone with no installer adds nothing.
    #[test]
    fn the_stage_row_says_what_a_new_clones_hook_installer_did() {
        const INSTALLED: &str = "Trust git hooks: installed (core.hooksPath = scripts/hooks)";
        let present = "1 matching worktrees present; aterm's GitHub route hands back the synced \
                       token";
        for (carries, reply, says) in [
            (
                true,
                Ran::ok(&format!("{INSTALLED}\n")),
                format!(
                    "{present}; $HOME/trust: ran its {}: {INSTALLED:?}",
                    repos::HOOK_INSTALLER
                ),
            ),
            (
                true,
                Ran::exit(1, "ERROR: refused\n"),
                format!(
                    "{present}; $HOME/trust: its {} failed (exit 1: \"ERROR: refused\"); the clone \
                     stands",
                    repos::HOOK_INSTALLER
                ),
            ),
            (false, Ran::ok(INSTALLED), present.to_owned()),
        ] {
            let (sys, item) = listing(&["trust".to_owned()], &[]);
            let home = Path::new(&item.user.home);
            if carries {
                let (staged, clone) = repos::tests::clone_key(&sys, &item.user, "trust", 0);
                let scripts = staged.join("scripts");
                sys.on_stat(&clone, &scripts, stat(true, 0o755));
                sys.on_write(
                    &clone,
                    &staged.join(repos::HOOK_INSTALLER),
                    b"#!/usr/bin/env bash\n",
                );
            }
            let installer = repos::installer_command(&sys, &item.user, &home.join("trust"));
            sys.on(&installer.display(), reply);
            let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let rows = run(&sys, &context, Mode::Apply).unwrap();
            assert_eq!(
                stage(&rows).outcome,
                crate::model::Outcome::Changed,
                "{rows:?}"
            );
            assert_eq!(stage(&rows).to.as_deref(), Some(says.as_str()), "{rows:?}");
            assert_eq!(
                sys.calls().contains(&installer.display()),
                carries,
                "{:?}",
                sys.calls()
            );
        }
    }

    /// Up to four clones run at once: each of the first four waits, inside its clone, for the
    /// others to start (a one-minute hang detector, never a latency budget), which a pass that
    /// clones one at a time never lets happen; a fifth never runs beside them. No more staging
    /// directories exist than clones running, so a pass cut short leaves at most four behind.
    #[test]
    fn four_clones_run_at_once() {
        use std::sync::{Arc, Condvar, Mutex};
        let missing = names("new", 6);
        let (sys, item) = listing(&missing, &[]);
        let gate = Arc::new((Mutex::new((0_usize, 0_usize)), Condvar::new()));
        let seen = Arc::clone(&gate);
        sys.simulate(move |fake, cmd| {
            if !cmd.display().contains(" clone --no-single-branch ") {
                return None;
            }
            let calls = fake.calls();
            let marked = |what: &str| calls.iter().filter(|call| call.starts_with(what)).count();
            let staged = marked("scratch allocated ") - marked("scratch closed ");
            assert!(staged <= LANES_SEEN, "{staged} staging directories at once");
            let (lock, turn) = &*seen;
            let mut state = lock.lock().unwrap();
            state.0 += 1;
            state.1 = state.1.max(state.0);
            turn.notify_all();
            let (mut state, waited) = turn
                .wait_timeout_while(state, std::time::Duration::from_secs(60), |state| {
                    state.1 < LANES_SEEN
                })
                .unwrap();
            assert!(!waited.timed_out(), "four clones never ran at once");
            state.0 -= 1;
            None
        });
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        item.apply(&sys, &context).unwrap();
        assert_eq!(gate.0.lock().unwrap().1, LANES_SEEN);
        let home = Path::new(&item.user.home);
        for name in &missing {
            assert!(sys.stat(&home.join(name)).is_ok(), "{name}");
        }
    }

    /// The most clones a pass runs at once.
    const LANES_SEEN: usize = 4;

    /// A clone that no longer reads as the repository's own worktree there during read-back
    /// (another origin took its place once it was
    /// published) is still missing: the row fails instead of counting it present or dropping it.
    #[test]
    fn a_clone_the_read_back_cannot_verify_is_still_missing() {
        let (sys, item) = listing(&["new".to_owned()], &[]);
        let origin = format!(
            "-C {}/new config --get-all remote.origin.url",
            item.user.home
        );
        sys.simulate(move |_, cmd| {
            cmd.display()
                .ends_with(&origin)
                .then(|| Ran::ok("https://github.com/someone/new.git\n"))
        });
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Failed,
            "{rows:?}"
        );
        assert_eq!(
            Reading::of(stage(&rows)),
            Some(Reading::Repositories {
                present: 0,
                count: 1,
                signed_in: true,
            })
        );
        assert!(
            sys.calls()
                .iter()
                .any(|call| call.starts_with("rename-noreplace ")),
            "the clone was published before the read-back: {:?}",
            sys.calls()
        );
    }

    #[test]
    fn readback_rechecks_preexisting_worktrees_and_dependencies_after_a_clone_batch() {
        use super::super::dependencies::tests::{PIN, existing};
        use std::sync::atomic::{AtomicBool, Ordering};
        for dependency in [false, true] {
            let names = ["parent".to_owned(), "new".to_owned()];
            let (sys, item) = listing(&names, &["parent".to_owned()]);
            let home = Path::new(&item.user.home);
            let parent = home.join("parent");
            let child = parent.join("vendor/library");
            if dependency {
                required_repository(&sys, &item.user, "parent", &["vendor/library"]);
                let mut required = repository("alice", "parent", 1);
                required.as_object_mut().unwrap().insert(
                    "required_submodules".into(),
                    aterm_json::json!(["vendor/library"]),
                );
                metadata(
                    &sys,
                    &item.user,
                    aterm_json::json!([required, repository("alice", "new", 2),]),
                );
                sys.stat_of(parent.join("vendor").to_str().unwrap(), stat(true, 0o700));
                sys.stat_of(child.to_str().unwrap(), stat(true, 0o700));
                sys.file(child.join(".git").to_str().unwrap(), b"inert metadata")
                    .stat_of(child.join(".git").to_str().unwrap(), stat(false, 0o600));
                existing(&sys, &item.user, &child, "library", PIN);
                super::super::dependencies::tests::download(
                    &sys,
                    &item.user,
                    &parent,
                    "vendor/library",
                    1,
                );
            } else {
                repos::tests::script_clone_named(&sys, &item.user, "parent", 1, false);
            }
            let changed = AtomicBool::new(false);
            let removed = if dependency { child } else { parent };
            sys.simulate(move |sys, command| {
                if command.display().contains(" clone ") && !changed.swap(true, Ordering::SeqCst) {
                    // Another local process removes an old checkout while a different
                    // repository downloads. The credential and inventory do not change.
                    if dependency {
                        sys.remove(&removed.join(".git")).unwrap();
                    }
                    sys.remove(&removed).unwrap();
                }
                None
            });
            let ctx = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
            let source = super::super::token_read(&sys, &ctx).unwrap().unwrap().mark;
            let rows = run(&sys, &ctx, Mode::Apply).unwrap();
            assert_eq!(
                stage(&rows).outcome,
                crate::model::Outcome::Failed,
                "dependency={dependency}: {rows:?}"
            );
            assert_eq!(
                Reading::of(stage(&rows)),
                Some(Reading::Repositories {
                    present: 1,
                    count: 2,
                    signed_in: true,
                }),
                "dependency={dependency}: {rows:?}"
            );
            assert_eq!(
                super::super::token_read(&sys, &ctx).unwrap().unwrap().mark,
                source
            );
            let resumed = run(&sys, &ctx, Mode::Apply).unwrap();
            assert_eq!(
                stage(&resumed).outcome,
                crate::model::Outcome::Changed,
                "dependency={dependency}: {resumed:?}"
            );
            assert_eq!(
                Reading::of(stage(&resumed)),
                Some(Reading::Repositories {
                    present: 2,
                    count: 2,
                    signed_in: true,
                })
            );
            assert_eq!(
                sys.calls()
                    .iter()
                    .filter(|call| call.contains(" clone "))
                    .count(),
                2
            );
        }
    }

    /// Existing destinations stay untouched while the others clone. They remain part of the
    /// expected count and keep the stage unfinished; preserving a folder never completes it.
    #[test]
    fn blocked_destinations_remain_unfinished_while_the_other_repositories_clone() {
        let listed: Vec<String> = ["Documents", "infra", "new"].map(str::to_owned).to_vec();
        let (sys, item) = listing_with(&listed, &[], &["infra"]);
        let home = Path::new(&item.user.home);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(
            stage(&rows).outcome,
            crate::model::Outcome::Failed,
            "{rows:?}"
        );
        assert_eq!(
            stage(&rows)
                .explain
                .as_ref()
                .map(|explain| explain.why.as_str()),
            Some(
                "1 of 3 repositories ready; preserved work or dependency metadata blocks completion: \
                 ~/Documents is a standard home folder; left untouched; \
                 ~/infra has a different or credential-bearing GitHub origin; left untouched"
            )
        );
        assert!(sys.stat(&home.join("new")).is_ok());
        let calls = sys.calls();
        let documents = home.join("Documents");
        assert!(
            calls
                .iter()
                .all(|call| !call.contains(&documents.display().to_string())),
            "{calls:?}"
        );
        assert!(
            calls
                .iter()
                .all(|call| !call.contains("rename-noreplace") || call.ends_with("/new")),
            "{calls:?}"
        );
    }

    #[test]
    fn an_entirely_blocked_inventory_never_claims_zero_repositories_are_complete() {
        let listed = names("occupied", 53);
        let theirs: Vec<&str> = listed.iter().map(String::as_str).collect();
        let (sys, item) = listing_with(&listed, &[], &theirs);
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        assert!(matches!(item.probe(&sys, &context), Probe::Person { .. }));
        assert_eq!(
            item.detail(&sys, &context),
            Some(
                Reading::Repositories {
                    present: 0,
                    count: 53,
                    signed_in: true
                }
                .token()
            )
        );
        assert!(!sys.calls().iter().any(|call| call.contains(" clone ")));
    }

    /// `aterm init`'s default stage on a Mac the vault wrote no GitHub token to asks nobody
    /// anything: its row is not applicable, and neither git nor GitHub is asked. An explicit
    /// request for the stage waits for Git's automatic installation on that Mac (the control).
    #[test]
    fn init_s_default_stage_without_a_token_is_not_applicable_and_asks_nothing() {
        let (sys, bare) = bare();
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&bare.user));
        match item(&bare.user, None, true, RepositoryWork::Refresh, None).probe(&sys, &context) {
            Probe::NotApplicable { why } => {
                assert!(why.contains("~/.secrets/gh_access_token"), "{why}");
                assert!(why.contains("on this Mac yet"), "{why}");
            }
            other => panic!("{other:?}"),
        }
        assert!(
            sys.calls()
                .iter()
                .all(|call| !call.contains(" agh GET ")
                    && !call.contains(crate::items::github::CLT_GIT)),
            "{:?}",
            sys.calls()
        );
        assert!(matches!(
            item(&bare.user, None, false, RepositoryWork::Refresh, None).probe(&sys, &context),
            Probe::Blocked { .. }
        ));
        // On Linux the row names the machine, not a Mac.
        let (mut linux, _) = self::bare();
        linux.linux = true;
        match item(&bare.user, None, true, RepositoryWork::Refresh, None).probe(&linux, &context) {
            Probe::NotApplicable { why } => assert!(why.contains("on this machine yet"), "{why}"),
            other => panic!("{other:?}"),
        }
        // So does an init.toml `[github]` account's step, whose token file is not there yet
        // (with the distribution's git there, so the step reaches the token).
        linux.file(crate::items::github::SYSTEM_GIT, b"synthetic git");
        match item(&bare.user, None, false, RepositoryWork::Refresh, None).probe(&linux, &context) {
            Probe::Person { how, .. } => {
                assert!(how.contains("is not on this machine yet"), "{how}");
            }
            other => panic!("{other:?}"),
        }
    }

    /// The default set is the account's own repositories that are neither forks nor archived;
    /// an init.toml `[github]` owner keeps them all (the control).
    #[test]
    fn forks_and_archived_repositories_are_left_out_unless_init_toml_names_the_owner() {
        let mut listed = vec![
            repository("alice", "fork", 1),
            repository("alice", "kept", 2),
            repository("alice", "old", 3),
        ];
        for (index, key) in [(0, "fork"), (2, "archived")] {
            listed[index]
                .as_object_mut()
                .unwrap()
                .insert(key.into(), aterm_json::Value::Bool(true));
        }
        let names = |item: &NativeRepositories, sys: &Fake| -> Vec<String> {
            item.plan(sys)
                .unwrap()
                .repositories
                .into_iter()
                .map(|repo| repo.name)
                .collect()
        };
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::Value::Array(listed.clone()),
        );
        assert_eq!(names(&item, &sys), ["kept"]);

        let (sys, item) = fixture();
        let profile = "/Users//alice/.config/aterm/init.toml";
        let bytes = b"[github]\naccount = 'alice'\n";
        sys.file(profile, bytes).stat_of(
            profile,
            Stat {
                len: bytes.len() as u64,
                ..stat(false, 0o600)
            },
        );
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::Value::Array(listed),
        );
        assert_eq!(names(&item, &sys), ["fork", "kept", "old"]);
    }

    /// The keychain block is replaced only once the engine journaled the commands that put it
    /// back, the settings exactly as they were written. A helper of someone else's that git
    /// already uses (git's `store`, here, which reads no keychain) stays as it is, and the clones
    /// carry aterm's route past it, with the person's own URL rewrite (the control).
    #[test]
    fn the_keychain_block_is_journaled_before_it_is_replaced_and_someone_elses_helper_stays() {
        let journaled = |sys: &Fake| -> Vec<aterm_json::Value> {
            sys.written(Fake::JOURNAL)
                .map(|bytes| String::from_utf8(bytes).unwrap())
                .unwrap_or_default()
                .lines()
                .map(|line| aterm_json::from_str(line).unwrap())
                .collect()
        };
        let (sys, item) = fixture();
        answer(
            &sys,
            &item.user,
            &format!("{ACCESSIBLE}&page=1"),
            aterm_json::json!([]),
        );
        let writes = keychain_block_then_vault(&sys, &item.user);
        script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
        let first = writes[0].clone();
        sys.simulate(move |fake, cmd| {
            if cmd.display() == first {
                assert!(
                    fake.written(Fake::JOURNAL)
                        .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains("--add")),
                    "the block was replaced before its revert was journaled"
                );
            }
            None
        });
        let context = crate::items::defaults::tests::ctx(std::slice::from_ref(&item.user));
        let rows = run(&sys, &context, Mode::Apply).unwrap();
        assert_eq!(rows[0].outcome, crate::model::Outcome::Changed, "{rows:?}");
        let lines = journaled(&sys);
        let revert = lines[0]["revert"].as_str().unwrap();
        assert!(
            revert.contains(
                "git config --file /Users//alice/.gitconfig --add \
                 credential.https://github.com.helper ''; git config --file \
                 /Users//alice/.gitconfig --add credential.https://github.com.helper osxkeychain; \
                 git config --file /Users//alice/.gitconfig --add \
                 credential.https://github.com.username alice; git config --file \
                 /Users//alice/.gitconfig --add credential.https://github.com.usehttppath false \
                 (puts back"
            ),
            "{revert}"
        );
        assert!(writes.iter().all(|write| sys.calls().contains(write)));

        // Control: a helper of the person's own for github.com (git's `store`, no keychain) is
        // someone else's. The one missing repository clones through aterm's route, an SSH
        // rewrite of the person's included.
        for rewritten in [false, true] {
            let (sys, item) = fixture();
            answer(
                &sys,
                &item.user,
                &format!("{ACCESSIBLE}&page=1"),
                aterm_json::json!([repository("alice", "new", 1)]),
            );
            super::repos::tests::script_clone_named_route(
                &sys, &item.user, "new", 0, false, rewritten,
            );
            super::repos::tests::script_existing(
                &sys,
                &item.user,
                &Path::new(&item.user.home).join("new"),
                "new",
            );
            let helper = "credential.https://github.com.helper";
            sys.on(
                &script::helpers(&item.user),
                Ran::ok(&script::printed(&[
                    (script::CLT_CONFIG, "credential.helper", "osxkeychain"),
                    ("/Users//alice/.gitconfig", helper, ""),
                    ("/Users//alice/.gitconfig", helper, "store"),
                ])),
            );
            script::lookups_hold(&sys, &item.user, GITHUB, "alice", TOKEN);
            let rows = run(&sys, &context, Mode::Apply).unwrap();
            assert_eq!(rows[0].outcome, crate::model::Outcome::NotApplicable);
            assert_eq!(
                stage(&rows).outcome,
                crate::model::Outcome::Changed,
                "{rows:?}"
            );
            let now = stage(&rows).to.as_deref().unwrap();
            assert!(now.contains("hands back the synced token"), "{now}");
            assert!(!now.contains("store"), "{now}");
            let calls = sys.calls();
            assert!(
                calls.iter().all(|call| !call.contains("--global")
                    && !call.contains("--unset-all")
                    && !call.contains("credential approve")),
                "{calls:?}"
            );
            let clone = calls
                .iter()
                .find(|call| call.contains(" clone "))
                .expect("the clone ran");
            assert!(
                clone.contains("secrets git-credential") && clone.contains(helper),
                "{clone}"
            );
            assert_eq!(journaled(&sys)[0]["revert"], aterm_json::Value::Null);
        }
    }
}
