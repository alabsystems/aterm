// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable first-launch plan. Every stage is independently retryable; a
//! password lives only in the caller's secret buffer and never enters a record.

use super::*;
use aterm_machines::init_secrets::Replaces;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::Plan;

mod check;
pub use check::{check_initial_setup, check_initial_setup_observed};
mod observation;
pub use observation::{InitialSetupObservation, InitialSetupObserved};
mod authorization;
pub use authorization::InitialSetupAuthorization;
mod secrets_retry;
use secrets_retry::Pace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Workstation,
    Spotlight,
    Permissions,
    Secrets,
    Repositories,
}

impl Stage {
    const ALL: [Self; 5] = [
        Self::Workstation,
        Self::Spotlight,
        Self::Permissions,
        Self::Secrets,
        Self::Repositories,
    ];

    const fn bit(self) -> u16 {
        1 << self as u8
    }

    const fn selected(self, plan: &Plan) -> bool {
        match self {
            Self::Workstation => !plan.selection.workstation().is_empty(),
            Self::Spotlight => plan.selection.contains(Setting::Spotlight),
            Self::Permissions => plan.selection.contains(Setting::Permissions),
            Self::Secrets => plan.selection.contains(Setting::Secrets),
            Self::Repositories => plan.selection.contains(Setting::Repositories),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Workstation => "Selected Mac settings",
            Self::Spotlight => "Spotlight and Rust documentation",
            Self::Permissions => "aterm permissions",
            Self::Secrets => aterm_types::macos_setup::SECRETS_VAULT_ROW,
            Self::Repositories => "GitHub repositories",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Progress {
    done: u16,
    completed: bool,
    admitted: bool,
    running: Option<Stage>,
    plan: [u8; 32],
    revision: u64,
    /// The vault this Mac was in when the run was admitted.
    admitted_vault: AdmittedVault,
}

/// Which vault this Mac was configured for when a run was admitted. A vault configured
/// since (joined from a terminal or by another tool) was not there when the run was chosen,
/// and the run's Secrets stage leaves it in place rather than join the plan's
/// ([`AdmittedVault::stands_in`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AdmittedVault {
    /// Not known: a run an older build admitted, or a configuration that could not be read.
    /// Its stage joins the plan's vault, as it did before this was kept.
    #[default]
    Unknown,
    /// None was configured.
    None,
    /// The vault of this identity ([`ConfiguredVault::identity`]).
    Configured([u8; 32]),
}

impl AdmittedVault {
    /// What a run admitted now keeps of the `configured` vault (a read of `source.toml`).
    fn of(configured: Result<Option<ConfiguredVault>, String>) -> Self {
        match configured {
            Ok(None) => Self::None,
            Ok(Some(vault)) => Self::Configured(vault.identity()),
            Err(_) => Self::Unknown,
        }
    }

    /// Whether the Secrets stage of a run admitted with this record leaves `configured`,
    /// the vault this Mac is in now, in place of the one `plan` names: it is not the plan's
    /// vault (the rule the stage's item joins by,
    /// [`aterm_machines::init_secrets::selected_vault`]), and it was configured after the
    /// run was admitted. A run admitted while it was configured is a switch someone asked
    /// for, and joins.
    fn stands_in(self, plan: &Plan, configured: &ConfiguredVault) -> bool {
        let since = match self {
            Self::Unknown => false,
            Self::None => true,
            Self::Configured(identity) => identity != configured.identity(),
        };
        since
            && matches!(
                aterm_machines::init_secrets::selected_vault(
                    &plan.vault_url,
                    &configured.home,
                    Some(&configured.config),
                ),
                Ok(None)
            )
    }

    /// The record's value: none for [`Self::Unknown`], which an older build's record reads as.
    fn record(self) -> Option<Value> {
        match self {
            Self::Unknown => None,
            Self::None => Some(Value::Null),
            Self::Configured(identity) => Some(aterm_secrets::b64::encode_padded(&identity).into()),
        }
    }
}

fn plan_digest(plan: &Plan) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(2 + plan.vault_url.len());
    bytes.extend_from_slice(&plan.selection.bits().to_le_bytes());
    bytes.extend_from_slice(plan.vault_url.as_bytes());
    aterm_digest::Sha256::digest(&bytes)
}

impl Progress {
    fn for_plan(plan: &Plan) -> Self {
        Self {
            completed: plan.selection.is_empty(),
            plan: plan_digest(plan),
            ..Self::default()
        }
    }

    fn belongs_to(self, plan: &Plan) -> bool {
        self.plan == plan_digest(plan)
    }

    fn next_for_plan(self, plan: &Plan) -> Result<Self, String> {
        Ok(Self {
            revision: next_revision(self.revision)?,
            ..Self::for_plan(plan)
        })
    }
}

fn next_revision(revision: u64) -> Result<u64, String> {
    revision
        .checked_add(1)
        .ok_or_else(|| "initial setup run revision is exhausted".into())
}

fn progress(root: &Path) -> Result<Option<Progress>, String> {
    let path = root.join("initial-setup.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|_| "invalid initial setup progress record")?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized initial setup profile".into());
    }
    let done = value["done"]
        .as_u64()
        .and_then(|bits| u16::try_from(bits).ok())
        .filter(|bits| bits & !31 == 0)
        .ok_or("unrecognized initial setup progress")?;
    let completed = match value["state"].as_str() {
        Some("pending") => false,
        Some("completed") => true,
        _ => return Err("unrecognized initial setup state".into()),
    };
    let admitted = match value.get("admitted") {
        None => completed,
        Some(value) => value.as_bool().ok_or("invalid initial setup admission")?,
    };
    let running = match value.get("running") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            Stage::ALL
                .into_iter()
                .find(|stage| value.as_u64() == Some(u64::from(stage.bit())))
                .ok_or("unrecognized running setup stage")?,
        ),
    };
    let plan = match value.get("plan") {
        None => [0; 32],
        Some(value) => value
            .as_str()
            .and_then(aterm_secrets::b64::decode_padded)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("invalid initial setup plan binding")?,
    };
    let revision = match value.get("revision") {
        None => 0,
        Some(value) => value.as_u64().ok_or("invalid initial setup run revision")?,
    };
    let admitted_vault = match value.get("admitted_vault") {
        None => AdmittedVault::Unknown,
        Some(Value::Null) => AdmittedVault::None,
        Some(value) => AdmittedVault::Configured(
            value
                .as_str()
                .and_then(aterm_secrets::b64::decode_padded)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or("invalid initial setup vault at admission")?,
        ),
    };
    // A visible rename from a failed write is not a durable stage completion.
    flush_record_with(&path, durable_directory)?;
    Ok(Some(Progress {
        done,
        completed,
        admitted,
        running,
        plan,
        revision,
        admitted_vault,
    }))
}

fn save_progress(root: &Path, progress: Progress) -> Result<(), String> {
    let mut record = json!({"profile":PROFILE,"state":if progress.completed {"completed"} else {"pending"},"admitted":progress.admitted,"done":progress.done,"running":progress.running.map(Stage::bit),"plan":aterm_secrets::b64::encode_padded(&progress.plan),"revision":progress.revision});
    // Left out for a run that keeps no vault, as an older build writes it; an older build
    // ignores the field.
    if let (Some(fields), Some(vault)) = (record.as_object_mut(), progress.admitted_vault.record())
    {
        fields.insert("admitted_vault".into(), vault);
    }
    write_json(&root.join("initial-setup.json"), &record)
}

fn saved_plan(root: &Path) -> Result<Option<Plan>, String> {
    Ok(saved_plan_record(root)?.map(|(plan, _)| plan))
}

fn saved_plan_record(root: &Path) -> Result<Option<(Plan, bool)>, String> {
    let path = root.join("automatic-plan.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|_| "invalid automatic setup plan")?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized automatic setup profile".into());
    }
    let selection = value["selection"]
        .as_u64()
        .and_then(|bits| u16::try_from(bits).ok())
        .and_then(Selection::from_bits)
        .ok_or("unrecognized automatic setup selection")?;
    let plan = Plan {
        selection,
        vault_url: match value.get("vault_url") {
            None => aterm_types::macos_setup::DEFAULT_VAULT_URL.into(),
            Some(value) => value
                .as_str()
                .ok_or("invalid automatic setup vault source")?
                .into(),
        },
    };
    validate_plan(&plan)?;
    flush_record_with(&path, durable_directory)?;
    Ok(Some((plan, value.get("vault_url").is_some())))
}

fn validate_plan(plan: &Plan) -> Result<(), String> {
    if !plan.selection.contains(Setting::Secrets) {
        return Ok(());
    }
    let home = Path::new("/");
    let (source, _) = aterm_secrets::transport::parse_source(&plan.vault_url, home)
        .map_err(|_| "the vault source must be an http(s) URL or an absolute or ~/ file path")?;
    aterm_secrets::transport::vault_name(&source)
        .map_err(|_| "the vault source must end in a valid vault name")?;
    Ok(())
}

/// Validate immutable setup input without reading state or probing the OS.
pub fn validate_initial_setup_plan(plan: &Plan) -> Result<(), String> {
    validate_plan(plan)
}

fn save_plan(root: &Path, plan: &Plan) -> Result<(), String> {
    validate_plan(plan)?;
    write_json(
        &root.join("automatic-plan.json"),
        &json!({"profile":PROFILE,"selection":plan.selection.bits(),"vault_url":plan.vault_url}),
    )
}

pub(super) fn save_explicit_workstation_plan(root: &Path) -> Result<(), String> {
    let stopped = stopped_at(root)?.is_some();
    let Some(mut progress) = progress(root)? else {
        let plan = Plan::from(Selection::all().workstation());
        if stopped {
            // Retire legacy run zero before clearing its stop. A successful
            // explicit profile pass completes this original-eight-only run,
            // whose plan selects no vault.
            return restart_in_vault(root, &plan, AdmittedVault::Unknown).map(|_| ());
        }
        save_workstation_authorization(root, &plan)?;
        return save_automatic_plan(root, plan.selection);
    };
    let mut plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) {
        progress = progress.next_for_plan(&plan)?;
    } else if stopped {
        progress.revision = next_revision(progress.revision)?;
    }
    let admitted = progress.admitted;
    if stopped {
        // An older worker may retain a password across its backoff. Publish
        // its retirement before replacing Cancel/Restore, including on crash.
        progress.admitted = false;
        save_progress(root, progress)?;
    }
    for setting in Setting::ALL {
        if setting.is_workstation() {
            plan.selection.set(setting, true);
        }
    }
    // Invalidate the old subset's completion before publishing its expanded
    // plan or clearing its done bit. If explicit preflight fails, the existing
    // worker must retry these settings rather than accept that stale marker.
    // A live run retains its revision and vault password; reviving a stopped
    // run above deliberately retires those workers.
    save_workstation_authorization(root, &plan)?;
    save_plan(root, &plan)?;
    progress.plan = plan_digest(&plan);
    progress.done &= !Stage::Workstation.bit();
    progress.completed = false;
    progress.running = None;
    progress.admitted = admitted;
    save_progress(root, progress)
}

pub(super) fn complete_explicit_workstation_plan(root: &Path) -> Result<(), String> {
    let Some(mut progress) = progress(root)? else {
        return Ok(());
    };
    let plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) || !progress.admitted {
        return Ok(());
    }
    progress.done |= Stage::Workstation.bit();
    progress.running = None;
    progress.completed = Stage::ALL
        .iter()
        .all(|stage| !stage.selected(&plan) || progress.done & stage.bit() != 0);
    save_progress(root, progress)
}

/// The vault this Mac is configured for (`source.toml`), which the plan System State
/// and the startup review offer is presented in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfiguredVault {
    config: aterm_secrets::client::Config,
    /// The home a plan's `~/` path resolves against.
    home: PathBuf,
}

impl ConfiguredVault {
    /// The vault configured under `paths`, or None when none is.
    fn read(paths: &aterm_secrets::client::Paths) -> Result<Option<Self>, String> {
        Ok(aterm_secrets::client::config(paths)
            .map_err(|error| error.to_string())?
            .map(|config| Self {
                config,
                home: paths.home.clone(),
            }))
    }

    /// What a run's progress keeps of this vault ([`AdmittedVault::Configured`]): a digest
    /// of its source and name, so the record holds no URL, query or pin.
    fn identity(&self) -> [u8; 32] {
        let source = match &self.config.source {
            aterm_secrets::transport::Source::Url(url) => format!("url\0{url}"),
            aterm_secrets::transport::Source::Path(path) => {
                format!("path\0{}", path.to_string_lossy())
            }
        };
        aterm_digest::Sha256::digest(format!("{source}\0{}", self.config.name).as_bytes())
    }

    /// What the Secrets row says when setup leaves this vault in place of the one `plan`
    /// names ([`AdmittedVault::stands_in`]): each by its name and where it is, never a URL's
    /// query or a pin. The same vault under another signer than the plan's pin admits is
    /// said so, not named twice.
    fn left_in_place(&self, plan: &Plan) -> String {
        let joined = vault_label(&self.config.name, &self.config.source);
        let named = aterm_secrets::transport::parse_source(&plan.vault_url, &self.home)
            .ok()
            .and_then(|(source, _)| {
                let name = aterm_secrets::transport::vault_name(&source).ok()?;
                Some((source, name))
            });
        match named {
            Some((source, name)) if source == self.config.source && name == self.config.name => {
                format!(
                    "this Mac joined {joined} after setup began, under another signer than the one setup names, so setup leaves it there; to join it under that signer, type the vault with its #<signer> in System State and press Apply"
                )
            }
            named => format!(
                "this Mac joined {joined} after setup began, so setup leaves it there; to move it to {}, type that vault in System State and press Apply",
                named.map_or_else(
                    || "the vault setup names".into(),
                    |(source, name)| vault_label(&name, &source)
                )
            ),
        }
    }

    /// `plan` with this vault as its source unless it names this vault already, where
    /// its source stays as typed (a pin with it). A saved source that names another vault
    /// gives way, so an Apply left as offered resumes the vault this Mac is in and never
    /// joins the one an earlier run saved; a person who means another vault types it.
    fn present(&self, mut plan: Plan) -> Plan {
        let names_this = aterm_secrets::transport::parse_source(&plan.vault_url, &self.home)
            .is_ok_and(|(source, _)| source == self.config.source);
        if !names_this {
            plan.vault_url = match &self.config.source {
                aterm_secrets::transport::Source::Url(url) => url.clone(),
                aterm_secrets::transport::Source::Path(path) => path.to_string_lossy().into_owned(),
            };
        }
        plan
    }
}

/// `vault <name> (<where>)`: a URL without its query or fragment, a path as it is.
fn vault_label(name: &str, source: &aterm_secrets::transport::Source) -> String {
    let at = match source {
        aterm_secrets::transport::Source::Url(url) => aterm_secrets::transport::describe(url),
        aterm_secrets::transport::Source::Path(path) => path.display().to_string(),
    };
    format!("vault {name} ({at})")
}

/// This user's configured vault ([`ConfiguredVault::read`]).
fn live_configured_vault() -> Result<Option<ConfiguredVault>, String> {
    let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
    ConfiguredVault::read(&paths)
}

/// The plan offered from the `saved` record, in the `configured` vault. A modern record
/// that names its source is kept as it is when the configuration cannot be read; a
/// legacy record or none needs it, since the LAN default stands in for its source.
fn offered_plan(
    saved: Option<(Plan, bool)>,
    configured: Result<Option<ConfiguredVault>, String>,
) -> Result<Plan, String> {
    match (saved, configured) {
        (Some((plan, true)), Ok(Some(vault))) => Ok(vault.present(plan)),
        (Some((plan, true)), _) => Ok(plan),
        (saved, configured) => {
            let plan = saved.map_or_else(Plan::default, |(plan, _)| plan);
            Ok(match configured? {
                Some(vault) => vault.present(plan),
                None => plan,
            })
        }
    }
}

/// Authorize the exact selected stages and source before any OS probes. A
/// password must be supplied separately to [`apply_initial_setup`] and is never saved.
/// Returns None if an earlier finished decision won, or the exact run revision
/// after this choice is durably admitted, including an empty checklist.
pub fn authorize_initial_setup(plan: &Plan) -> Result<Option<u64>, String> {
    authorize_initial_setup_observed(plan).result
}

/// Authorize with a result identity captured before releasing the profile lock.
pub fn authorize_initial_setup_observed(plan: &Plan) -> InitialSetupObserved<Option<u64>> {
    observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| authorize_reading(root, plan, live_configured_vault),
    )
}

/// Explicitly run the selected plan again, superseding an earlier completion,
/// cancellation or restoration. Original preference before-images are retained.
pub fn restart_initial_setup(plan: &Plan) -> Result<u64, String> {
    restart_initial_setup_observed(plan).result
}

/// Restart with a result identity captured before releasing the profile lock.
pub fn restart_initial_setup_observed(plan: &Plan) -> InitialSetupObserved<u64> {
    observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| restart_reading(root, plan, live_configured_vault),
    )
}

/// One explicit restart, retaining ownership only while its final admission
/// save needs recovery. Keep the original plan, password and UI receipt until
/// this returns a revision or is no longer pending; never reuse a finished attempt.
#[derive(Default)]
pub struct InitialSetupRestart {
    started: bool,
    final_commit: Option<RestartCommit>,
}

struct RestartCommit {
    plan: Plan,
    staged: Records,
    admitted: Records,
    publications: observation::Publications,
    progress: Progress,
}

impl InitialSetupRestart {
    /// Whether the same final admission step can still be confirmed or retried.
    /// Errors before that step do not leave a pending operation.
    pub const fn pending(&self) -> bool {
        self.final_commit.is_some()
    }

    /// Persist this exact restart. Some is its durably admitted revision; None
    /// means it was superseded or already ended. Retry errors only while pending.
    /// `vault_typed`: the person typed the plan's vault in System State's field. One left
    /// as the field showed it asks for the vault System State shows now
    /// ([`shown_in_vault`]), so a tab shown before this Mac joined a vault from a terminal
    /// never moves it out of that vault; a typed one moves it.
    pub fn persist_observed(
        &mut self,
        plan: &Plan,
        vault_typed: bool,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_person_mutation(
            Err("another doctor profile pass is running".into()),
            |root| {
                self.persist_with(
                    root,
                    plan,
                    vault_typed,
                    live_configured_vault,
                    save_progress,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
            },
        )
    }

    /// `read`: which vault this Mac is in now, read once by a first attempt, whose run
    /// keeps it.
    fn persist_with(
        &mut self,
        root: &Path,
        plan: &Plan,
        vault_typed: bool,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
        commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        // The caller holds profile.lock for staging, confirmation, and any
        // final write. A durable stop wins even if other records are unreadable.
        if self.pending() {
            if !owns_publications(root, &self.final_commit.as_ref().unwrap().publications)? {
                self.final_commit = None;
                return Ok(None);
            }
            let current = observe(root)?;
            let owned = self.final_commit.as_ref().unwrap();
            if owned.plan != *plan
                || current.publications() != owned.publications
                || (current.records != owned.staged && current.records != owned.admitted)
            {
                self.final_commit = None;
                return Ok(None);
            }
            if current.records == owned.admitted {
                let revision = owned.progress.revision;
                self.final_commit = None;
                return Ok(Some(revision));
            }
        } else {
            if self.started {
                return Ok(None);
            }
            self.started = true;
            let configured = read();
            let admitted_plan = match &configured {
                Ok(Some(vault)) if !vault_typed => shown_in_vault(
                    plan.clone(),
                    &observation_at(root)
                        .map_err(|error| error.to_string())?
                        .records,
                    vault,
                ),
                _ => plan.clone(),
            };
            let mut next = stage_restart(root, &admitted_plan, AdmittedVault::of(configured))?;
            let staged = observe(root)?;
            if !staged
                .records
                .plan
                .as_ref()
                .is_some_and(|(saved, modern)| *modern && *saved == admitted_plan)
                || staged.records.progress != Some(next)
                || !matches!(
                    staged.records.decision,
                    Decision::Pending | Decision::Applied(true)
                )
            {
                return Err("the staged setup restart changed before admission".into());
            }
            next.admitted = true;
            let mut admitted = staged.records.clone();
            admitted.progress = Some(next);
            self.final_commit = Some(RestartCommit {
                plan: plan.clone(),
                staged: staged.records.clone(),
                admitted,
                publications: staged.publications(),
                progress: next,
            });
        }

        let next = self.final_commit.as_ref().unwrap().progress;
        match commit(root, next) {
            Ok(()) => {
                self.final_commit = None;
                Ok(Some(next.revision))
            }
            Err(why) => {
                // A failed flush can leave either the staged file or our new
                // admitted file visible. Pin its identity before unlocking,
                // without relying on reads or another flush to succeed now.
                match observation::publications(root) {
                    Ok(publications) => {
                        self.final_commit.as_mut().unwrap().publications = publications;
                    }
                    Err(_) => {
                        // An unbound failure must never adopt whatever becomes
                        // readable later as this operation's publication.
                        self.final_commit = None;
                    }
                }
                Err(why)
            }
        }
    }
}

fn owns_publications(root: &Path, expected: &observation::Publications) -> Result<bool, String> {
    let stop = stopped_at(root);
    if matches!(stop, Ok(Some(_))) {
        return Ok(false);
    }
    // A malformed replacement is still supersession. Only an error reading
    // our unchanged decision belongs to this operation and remains retryable.
    if observation::publications(root)? != *expected {
        return Ok(false);
    }
    stop?;
    Ok(true)
}

/// A restart from a scratch home, which is in no vault.
#[cfg(test)]
fn restart_at(root: &Path, plan: &Plan) -> Result<u64, String> {
    restart_reading(root, plan, || Ok(None))
}

/// Restart `plan`'s run in the vault `read` finds this Mac in now, which the run keeps.
fn restart_reading(
    root: &Path,
    plan: &Plan,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<u64, String> {
    restart_in_vault(root, plan, AdmittedVault::of(read()))
}

/// Restart `plan`'s run while this Mac is in `vault`, which the run keeps.
fn restart_in_vault(root: &Path, plan: &Plan, vault: AdmittedVault) -> Result<u64, String> {
    restart_with(root, plan, vault, save_progress)
}

fn restart_with(
    root: &Path,
    plan: &Plan,
    vault: AdmittedVault,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<u64, String> {
    let mut next = stage_restart(root, plan, vault)?;
    next.admitted = true;
    commit(root, next)?;
    Ok(next.revision)
}

fn stage_restart(root: &Path, plan: &Plan, vault: AdmittedVault) -> Result<Progress, String> {
    validate_plan(plan)?;
    let next = Progress {
        admitted_vault: vault,
        ..progress(root)?.unwrap_or_default().next_for_plan(plan)?
    };
    save_plan(root, plan)?;
    save_progress(root, next)?;
    // The new run's automatic attempts start afresh: its cooldown, and the items an
    // earlier run's attempt left held (`automatic_pass`), which a new revision already
    // retires, so that an unreadable record cannot outlive a person's rerun either.
    for (record, what) in [
        ("automatic-attempt.json", "the automatic retry cooldown"),
        (HELD_RECORD, "the held automatic setup items"),
        (secrets_retry::RECORD, "the secrets retry pause"),
    ] {
        match std::fs::remove_file(root.join(record)) {
            Ok(()) => durable_directory(root).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot reset {what}: {error}")),
        }
    }
    // Release an earlier stop only after the new plan and progress are durable.
    // The final admission record is still required before any setup can run.
    save_workstation_authorization(root, plan)?;
    Ok(next)
}

fn save_workstation_authorization(root: &Path, plan: &Plan) -> Result<(), String> {
    write_json(
        &root.join("first-run.json"),
        &if plan.selection.workstation().is_empty() {
            json!({"profile":PROFILE,"state":"applied","accounts_verified":true})
        } else {
            json!({"profile":PROFILE,"state":"pending"})
        },
    )
}

/// An authorization from a scratch home, which is in no vault.
#[cfg(test)]
fn authorize_at(root: &Path, plan: &Plan) -> Result<Option<u64>, String> {
    authorize_reading(root, plan, || Ok(None))
}

/// Authorize `plan` in the vault `read` finds this Mac in now, which the run keeps.
fn authorize_reading(
    root: &Path,
    plan: &Plan,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<Option<u64>, String> {
    authorize_with(root, plan, AdmittedVault::of(read()), save_progress)
}

fn authorize_with(
    root: &Path,
    plan: &Plan,
    vault: AdmittedVault,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<Option<u64>, String> {
    if finished_at(root)? {
        return Ok(None);
    }
    validate_plan(plan)?;
    let previous = saved_plan(root)?;
    let prior_progress = progress(root)?;
    let revision = next_revision(prior_progress.unwrap_or_default().revision)?;
    let mut current = if previous.as_ref() == Some(plan)
        && prior_progress.is_some_and(|progress| progress.belongs_to(plan))
    {
        prior_progress.unwrap_or_else(|| Progress::for_plan(plan))
    } else {
        Progress::for_plan(plan)
    };
    current.completed = plan.selection.is_empty();
    current.admitted = false;
    current.running = None;
    current.revision = revision;
    current.admitted_vault = vault;
    save_plan(root, plan)?;
    save_progress(root, current)?;
    // Original doctor APIs retain their own marker. Workstation completion must
    // not complete the independent vault, repository and permissions stages.
    if current.done & Stage::Workstation.bit() == 0 {
        save_workstation_authorization(root, plan)?;
    }
    // This is the multi-file authorization commit. Earlier records can be
    // visible after an interrupted write without releasing permission UI.
    current.admitted = true;
    commit(root, current)?;
    Ok(Some(current.revision))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Restored,
}

fn stopped_at(root: &Path) -> Result<Option<Stop>, String> {
    Ok(match decision_at(root)? {
        Decision::Stopped(stop) => Some(stop),
        _ => None,
    })
}

/// Whether the person cancelled or restored setup: nothing of it runs by itself again.
pub(super) fn stopped(root: &Path) -> Result<bool, String> {
    Ok(stopped_at(root)?.is_some())
}

pub(super) fn finished_at(root: &Path) -> Result<bool, String> {
    Ok(stopped_at(root)?.is_some() || completed_at(root)?)
}

fn completed_at(root: &Path) -> Result<bool, String> {
    match progress(root)? {
        Some(progress) => Ok(saved_plan(root)?.is_some_and(|plan| {
            progress.belongs_to(&plan) && progress.admitted && progress.completed
        })),
        // Preserve decisions made before unified setup existed.
        None => first_run_complete(root),
    }
}

/// Whether every selected setup stage finished, or the person cancelled or
/// restored it. Completed legacy workstation decisions remain honored.
pub fn initial_setup_finished() -> Result<bool, String> {
    finished_at(&state_root()?)
}

/// The durable identity of the current setup run. Retry workers must compare it
/// after reacquiring the automatic claim, before using any retained password.
/// Zero means no modern run exists. An interrupted plan/progress pair is refused
/// until authorization repairs it, rather than returning an older run's revision.
pub fn initial_setup_revision() -> Result<u64, String> {
    revision_at(&state_root()?)
}

pub(super) fn revision_at(root: &Path) -> Result<u64, String> {
    let Some(progress) = progress(root)? else {
        return Ok(0);
    };
    plan_revision(Some(progress), saved_plan(root)?.as_ref())
}

fn plan_revision(progress: Option<Progress>, plan: Option<&Plan>) -> Result<u64, String> {
    let Some(progress) = progress else {
        return Ok(0);
    };
    let plan = plan.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(plan) {
        return Err("the initial setup plan changed before its progress reset completed".into());
    }
    Ok(progress.revision)
}

/// A coherent observation of a durably admitted modern setup run. This does
/// not grant an execution lease; workers must still fence effects by revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialSetupAdmission {
    pub plan: Plan,
    pub revision: u64,
    pub completed: bool,
}

/// Read the admitted run without loading fallback vault configuration, probing
/// the OS or changing records. Staged choices and stopped runs return None;
/// a durable stop takes precedence over unreadable plan/progress records.
/// Otherwise unreadable or changing records are errors, not missing admission.
pub fn initial_setup_admission() -> Result<Option<InitialSetupAdmission>, String> {
    admission_at(&state_root()?)
}

fn admission_at(root: &Path) -> Result<Option<InitialSetupAdmission>, String> {
    if stopped_at(root)?.is_some() {
        return Ok(None);
    }
    let Records {
        plan: Some((plan, true)),
        progress: Some(progress),
        decision: Decision::Pending | Decision::Applied(true),
    } = observation_at(root)
        .map_err(|error| error.to_string())?
        .records
    else {
        return Ok(None);
    };
    Ok(
        (progress.admitted && progress.belongs_to(&plan)).then_some(InitialSetupAdmission {
            plan,
            revision: progress.revision,
            completed: progress.completed,
        }),
    )
}

/// One observation of the saved plan, its progress and the permission decision.
/// A finished run includes Cancel and Restore; callers must use the explicit
/// permission decision rather than infer permission guidance from completion.
#[derive(Debug)]
pub struct InitialSetupSnapshot {
    /// The saved plan; from [`initial_setup_snapshot`], in the vault this Mac is
    /// configured for unless its run still joins the vault it saved, which System
    /// State's fields start from.
    pub plan: Plan,
    pub finished: bool,
    pub rows: Vec<Row>,
    /// Zero means that no matching modern setup run exists.
    pub revision: u64,
    /// Whether existing native permission guidance may run. Errors or absent
    /// authorization must be treated as false by callers.
    pub permissions_allowed: bool,
    /// Identity of the same coherent records used for these rows and policy.
    pub observation: InitialSetupObservation,
}

/// A concurrent progress publication is expected; a record read failure is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitialSetupSnapshotError {
    Updating,
    Read(String),
}

impl std::fmt::Display for InitialSetupSnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Updating => formatter.write_str("initial setup records are changing"),
            Self::Read(why) => formatter.write_str(why),
        }
    }
}

impl std::error::Error for InitialSetupSnapshotError {}

/// Observe only existing setup records. This neither authorizes work nor
/// waits behind a profile pass; callers must discard results on read failure.
pub fn initial_setup_observation() -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation_at(&state_root().map_err(InitialSetupSnapshotError::Read)?)
}

/// Injected state root for composition tests; never inspects the live machine.
#[cfg(any(test, debug_assertions))]
pub fn initial_setup_observation_at(
    root: &Path,
) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation_at(root)
}

pub(super) fn observation_at(
    root: &Path,
) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation::at(root)
}

pub(super) fn observe_person_mutation<T>(
    busy: Result<T, String>,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    let root = state_root();
    let before = root
        .as_ref()
        .ok()
        .and_then(|root| observation_at(root).ok());
    if let Err(why) = ensure_person_scope() {
        return InitialSetupObserved::new(Err(why), before);
    }
    let root = match root {
        Ok(root) => root,
        Err(why) => return InitialSetupObserved::new(Err(why), before),
    };
    observe_mutation_at(&root, before, busy, operation)
}

pub(super) fn observe_mutation_at<T>(
    root: &Path,
    before: Option<InitialSetupObservation>,
    busy: Result<T, String>,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    let _lock = match HeldLock::try_take(root) {
        Ok(Some(lock)) => lock,
        Ok(None) => return InitialSetupObserved::new(busy, before),
        Err(why) => return InitialSetupObserved::new(Err(why), before),
    };
    let result = operation(root);
    // Capture errors too: Restore may have durably stopped the run before a
    // later recovery failure. The writer lock still excludes a newer choice.
    InitialSetupObserved::new(result, observation_at(root).ok())
}

pub(super) fn observe_read_only<T>(
    root: Result<PathBuf, String>,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    let root = match root {
        Ok(root) => root,
        Err(why) => return InitialSetupObserved::new(Err(why), None),
    };
    let before = observation_at(&root).ok();
    let idle_before = HeldLock::profile_active(&root).is_ok_and(|active| !active);
    let result = operation(&root);
    let after = observation_at(&root).ok();
    let idle_after = HeldLock::profile_active(&root).is_ok_and(|active| !active);
    // Restore publishes its stop before its effects. Matching records alone
    // cannot certify a Check that overlaps that already-running writer.
    let observation =
        before.filter(|before| idle_before && idle_after && Some(before) == after.as_ref());
    InitialSetupObserved::new(result, observation)
}

/// Read only aterm's records/configuration, without probing OS grants, accounts
/// or preferences. Concurrent record changes are refused for the caller to retry;
/// this never waits for a long-running profile pass to release its lock.
pub fn initial_setup_snapshot() -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let root = state_root().map_err(InitialSetupSnapshotError::Read)?;
    snapshot_in_vault(&root, live_configured_vault())
}

/// The snapshot of the records under `root`, its plan in the `configured` vault
/// ([`offered_plan`]), which System State's fields start from, except while the saved
/// plan's run still joins the vault it saved ([`joining_saved_vault`]): the run's retries
/// join that one, so the field keeps it as saved. Its rows, revision and permission policy
/// are the saved plan's: the progress they read belongs to it.
fn snapshot_in_vault(
    root: &Path,
    configured: Result<Option<ConfiguredVault>, String>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let saved = saved_plan_record(root).map_err(InitialSetupSnapshotError::Read)?;
    let fallback =
        offered_plan(saved, configured.clone()).map_err(InitialSetupSnapshotError::Read)?;
    let mut snapshot = snapshot_at(root, fallback)?;
    if let Ok(Some(vault)) = configured {
        snapshot.plan = shown_in_vault(snapshot.plan, &snapshot.observation.records, &vault);
    }
    Ok(snapshot)
}

/// `plan` with the vault System State shows while this Mac is in `configured` and its
/// records are `records`: that vault ([`ConfiguredVault::present`]), except while the
/// saved plan's run still joins the vault it saved ([`joining_saved_vault`]).
fn shown_in_vault(plan: Plan, records: &Records, configured: &ConfiguredVault) -> Plan {
    if joining_saved_vault(records, configured) {
        plan
    } else {
        configured.present(plan)
    }
}

/// Whether the saved plan's run still joins the vault it saved while this Mac is in
/// `configured`: a modern record that selects Secrets, its run admitted and neither
/// completed nor stopped, and its Secrets stage still to do, which does not leave
/// `configured` in place ([`AdmittedVault::stands_in`]). A vault typed in System State
/// whose join failed is such a run, and the worker joins it again on each retry.
fn joining_saved_vault(records: &Records, configured: &ConfiguredVault) -> bool {
    let Some((plan, true)) = &records.plan else {
        return false;
    };
    plan.selection.contains(Setting::Secrets)
        && !matches!(records.decision, Decision::Stopped(_))
        && records.progress.is_some_and(|progress| {
            progress.belongs_to(plan)
                && progress.admitted
                && !progress.completed
                && progress.done & Stage::Secrets.bit() == 0
                && !progress.admitted_vault.stands_in(plan, configured)
        })
}

/// Injected record root and fallback plan for composition tests.
#[cfg(any(test, debug_assertions))]
pub fn initial_setup_snapshot_at(
    root: &Path,
    fallback: Plan,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    snapshot_at(root, fallback)
}

/// The saved plan and recorded stage progress for System State. The boolean
/// means the automatic run has ended, including Cancel or Restore.
pub fn initial_setup_status() -> Result<(Plan, bool, Vec<Row>), String> {
    let snapshot = initial_setup_snapshot().map_err(|error| error.to_string())?;
    Ok((snapshot.plan, snapshot.finished, snapshot.rows))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Missing,
    Pending,
    Applied(bool),
    Stopped(Stop),
}

fn decision_at(root: &Path) -> Result<Decision, String> {
    let path = root.join("first-run.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Decision::Missing);
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value = aterm_json::from_slice(&bytes).map_err(|_| "invalid first-run decision")?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized first-run profile".into());
    }
    let decision = match value["state"].as_str() {
        Some("pending") => Decision::Pending,
        Some("applied") => Decision::Applied(value["accounts_verified"].as_bool() == Some(true)),
        Some("cancelled") => Decision::Stopped(Stop::Cancelled),
        Some("restored") => Decision::Stopped(Stop::Restored),
        _ => return Err("unrecognized first-run decision".into()),
    };
    flush_record_with(&path, durable_directory)?;
    Ok(decision)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Records {
    plan: Option<(Plan, bool)>,
    progress: Option<Progress>,
    decision: Decision,
}

impl Records {
    fn read(root: &Path) -> Result<Self, String> {
        Ok(Self {
            plan: saved_plan_record(root)?,
            progress: progress(root)?,
            decision: decision_at(root)?,
        })
    }
}

fn snapshot_at(
    root: &Path,
    fallback: Plan,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let observation = observation_at(root)?;
    let active = HeldLock::profile_active(root).map_err(InitialSetupSnapshotError::Read)?;
    Ok(snapshot_from_observation(fallback, active, observation))
}

#[cfg(test)]
fn snapshot_with(
    fallback: Plan,
    activity: impl FnOnce() -> Result<bool, String>,
    mut read: impl FnMut() -> Result<Records, String>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let records = read().map_err(InitialSetupSnapshotError::Read)?;
    if records != read().map_err(InitialSetupSnapshotError::Read)? {
        return Err(InitialSetupSnapshotError::Updating);
    }
    // This is a current liveness sample, not an execution lease. No shared
    // observation lock spans the record durability reads or the UI's work.
    let active = activity().map_err(InitialSetupSnapshotError::Read)?;
    Ok(snapshot_from_observation(
        fallback,
        active,
        observation::from_records(records),
    ))
}

fn snapshot_from_observation(
    fallback: Plan,
    active: bool,
    observation: InitialSetupObservation,
) -> InitialSetupSnapshot {
    let records = &observation.records;
    let plan = records
        .plan
        .as_ref()
        .map_or(fallback, |(plan, _)| plan.clone());
    let progress = records
        .progress
        .filter(|progress| progress.belongs_to(&plan));
    let stopped = match records.decision {
        Decision::Stopped(stop) => Some(stop),
        _ => None,
    };
    let completed = match records.progress {
        Some(_) => progress.is_some_and(|progress| progress.admitted && progress.completed),
        None => records.decision == Decision::Applied(true),
    };
    let finished = stopped.is_some() || completed;
    let permissions_allowed = stopped.is_none()
        && match records.progress {
            Some(_) => {
                records.plan.is_some()
                    && progress.is_some_and(|progress| progress.admitted)
                    && matches!(
                        records.decision,
                        Decision::Pending | Decision::Applied(true)
                    )
                    && plan.selection.contains(Setting::Permissions)
            }
            None => {
                !records.plan.as_ref().is_some_and(|(_, modern)| *modern)
                    && records.decision == Decision::Applied(true)
            }
        };
    let rows = Stage::ALL
        .iter()
        .filter(|stage| stage.selected(&plan))
        .map(|stage| {
            let completed = progress.is_some_and(|progress| progress.done & stage.bit() != 0);
            // Restore stops convergence before any preference writes. Its marker
            // therefore cannot certify either the profile or successful restoration.
            let restoring = stopped == Some(Stop::Restored) && *stage == Stage::Workstation;
            let recorded_running =
                progress.is_some_and(|progress| progress.running == Some(*stage));
            let interrupted = !finished && !completed && recorded_running && !active;
            Row {
                name: stage.name().into(),
                status: if restoring {
                    "stopped"
                } else if completed {
                    "applied"
                } else if finished {
                    "stopped"
                } else if recorded_running && active {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                detail: if restoring {
                    "Restore requested; see results.".into()
                } else if finished && !completed {
                    "Not applied by this run.".into()
                } else if interrupted {
                    "Not running; Apply to retry.".into()
                } else {
                    String::new()
                },
            }
        })
        .collect();
    InitialSetupSnapshot {
        plan,
        finished,
        rows,
        revision: progress.map_or(0, |progress| progress.revision),
        permissions_allowed,
        observation,
    }
}

#[cfg(test)]
fn status_at(root: &Path, fallback: Plan) -> Result<(Plan, bool, Vec<Row>), String> {
    let snapshot = snapshot_at(root, fallback).map_err(|error| error.to_string())?;
    Ok((snapshot.plan, snapshot.finished, snapshot.rows))
}

trait InitialHost {
    fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String>;
    fn spotlight(&mut self) -> Result<Vec<Row>, String>;
    fn permissions(&mut self) -> Result<Vec<Row>, String>;
    /// One try of the Secrets stage, whose join replaces only the vault configuration
    /// `replaces` admits. `password` is the join password System State was given, taken
    /// once a join used it and left for the next try otherwise.
    fn secrets(
        &mut self,
        plan: &Plan,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String>;
    /// What can change a Secrets try's answer without the person ([`secrets_retry`]):
    /// `None` paces nothing, a try each pass.
    fn secrets_pace(&mut self) -> Option<Pace>;
    /// The vault this Mac is configured for now ([`ConfiguredVault::read`]).
    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String>;
    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String>;
    fn repositories(&mut self) -> Result<Vec<Row>, String>;
    /// Reach what a Workstation pass left alone for want of Full Disk Access, once this
    /// process holds it ([`super::follow_up_at`]); `None` when nothing ran. The caller holds
    /// the profile lock.
    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String>;
}

/// This Mac's setup stages. `reads`: what paces the Secrets stage ([`secrets_retry::Live`]).
struct LiveInitial<R> {
    reads: R,
    spotlight_result: Option<Result<Vec<Row>, String>>,
    permissions_result: Option<Result<Vec<Row>, String>>,
}

impl LiveInitial<secrets_retry::Live> {
    /// This Mac's stages, the Secrets stage paced by this Mac's network and vault.
    const fn live() -> Self {
        Self {
            reads: secrets_retry::Live,
            spotlight_result: None,
            permissions_result: None,
        }
    }
}

impl<R> LiveInitial<R> {
    fn context(&self) -> Result<aterm_machines::items::Ctx, String> {
        aterm_machines::init::initial_setup_context(&aterm_machines::port::Live::new())
    }
}

/// The vault item's rows under the stage's own name, as the checklist shows it.
fn secrets_rows(rows: Vec<aterm_machines::model::ItemReport>) -> Vec<Row> {
    item_rows(rows)
        .into_iter()
        .map(|row| Row {
            name: Stage::Secrets.name().into(),
            ..row
        })
        .collect()
}

fn item_rows(rows: Vec<aterm_machines::model::ItemReport>) -> Vec<Row> {
    use aterm_machines::model::Outcome;
    rows.into_iter()
        .map(|row| Row {
            name: row.title,
            status: match row.outcome {
                Outcome::Ok | Outcome::NotApplicable => "ok",
                Outcome::Changed => "applied",
                Outcome::Planned => "drift",
                Outcome::Person | Outcome::Blocked | Outcome::Deferred => "blocked",
                Outcome::Unknown => "unverified",
                Outcome::Failed => "error",
            }
            .into(),
            detail: row
                .why
                .or_else(|| match (row.from, row.to) {
                    (Some(from), Some(to)) => Some(format!("{from} → {to}")),
                    (None, Some(to)) => Some(to),
                    (Some(from), None) => Some(from),
                    (None, None) => None,
                })
                .unwrap_or_else(|| "verified".into()),
        })
        .collect()
}

pub(super) fn read_permissions_live(apply: bool) -> Result<Vec<Row>, String> {
    use crate::{DebuggerMember, DebuggerMode, FdaState, Mark};
    use aterm_machines::port::System as _;
    use atpkg::prereq::CcVerdict;
    let (privacy, debugger, devtools, clt, ax) = std::thread::scope(|s| {
        let privacy = s.spawn(crate::privacy_facts);
        let debugger = s.spawn(crate::debugger_facts);
        let devtools = s.spawn(atpkg::devtools::facts);
        let clt = s.spawn(|| {
            let verdict = atpkg::prereq::probe(std::env::var_os("PATH").as_deref());
            if apply
                && matches!(verdict, CcVerdict::NoDeveloperDir | CcVerdict::NoDriver)
                && let Ok(exe) = std::env::current_exe().and_then(|e| e.canonicalize())
            {
                let sys = aterm_machines::port::Live::new();
                if sys.has_tty()
                    && let Ok(ctx) = aterm_machines::init::initial_setup_context(&sys)
                {
                    let item = aterm_machines::init_clt::CommandLineTools::new(
                        Box::new(|_| aterm_machines::init_clt::Found::Missing("missing".into())),
                        exe,
                    );
                    let _ = aterm_machines::items::Item::apply(&item, &sys, &ctx);
                    return atpkg::prereq::probe(std::env::var_os("PATH").as_deref());
                }
            }
            verdict
        });
        let ax = s.spawn(|| {
            let sys = aterm_machines::port::Live::new();
            let ready = aterm_machines::init_accounts::accessibility_ready(&sys).unwrap_or(false);
            if ready {
                return (
                    true,
                    "Accessibility & UI Automation granted → Desired: granted".to_owned(),
                );
            }
            let needed = aterm_machines::init_accounts::accessibility_needed(&sys)
                .unwrap_or(false);
            if apply {
                let _ = aterm_machines::init_accounts::request_accessibility(&sys);
                if aterm_machines::init_accounts::accessibility_ready(&sys).unwrap_or(false) {
                    return (
                        true,
                        "Accessibility & UI Automation granted → Desired: granted".to_owned(),
                    );
                }
            }
            if !needed {
                (
                    true,
                    "AppleEvents automation ready (Accessibility optional until account sign-out or alert remote-control click)".to_owned(),
                )
            } else {
                (
                    false,
                    "Accessibility not granted → Desired: enable aterm in System Settings > Privacy & Security > Accessibility".to_owned(),
                )
            }
        });
        (
            privacy.join().expect("privacy_facts thread"),
            debugger.join().expect("debugger_facts thread"),
            devtools.join().expect("devtools::facts thread"),
            clt.join().expect("prereq::probe thread"),
            ax.join().expect("accessibility thread"),
        )
    });
    let (debugger_mark, debugger_detail) = crate::debugger_row(&debugger);
    let devtools_ready = devtools
        .as_ref()
        .is_some_and(|facts| facts.grant() == aterm_containment::consent::DevToolGrant::Granted);
    let clt_ready = matches!(clt, CcVerdict::Ready { .. });
    let clt_detail = match &clt {
        CcVerdict::Ready { driver } => {
            format!("{} builds a C program → Desired: ready", driver.display())
        }
        _ => format!(
            "{} → Desired: ready (`aterm init --only base.clt`)",
            clt.line()
        ),
    };
    Ok(vec![
        readiness_row("Command Line Tools", clt_ready, clt_detail),
        readiness_row(
            "Full Disk Access",
            privacy.fda == FdaState::Granted,
            crate::privacy_row(&privacy).1,
        ),
        readiness_row(
            "Debugger access",
            debugger.probe.mode == DebuggerMode::Enabled
                && debugger_mark == Mark::Ok
                && matches!(
                    debugger.probe.member,
                    DebuggerMember::Admin | DebuggerMember::Developer
                ),
            debugger_detail,
        ),
        readiness_row(
            "Developer Tools",
            devtools_ready,
            devtools
                .as_ref()
                .and_then(atpkg::devtools::row)
                .map_or_else(|| "permission status not measured".into(), |row| row.text),
        ),
        readiness_row("Accessibility & UI Automation", ax.0, ax.1),
    ])
}

impl<R> LiveInitial<R> {
    pub(super) fn read_permissions(&self, apply: bool) -> Result<Vec<Row>, String> {
        read_permissions_live(apply)
    }
}

impl<R: secrets_retry::Reads> InitialHost for LiveInitial<R> {
    fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String> {
        let (sp_due, perm_due) = match (saved_plan(root), progress(root)) {
            (Ok(Some(plan)), Ok(Some(prog))) => (
                Stage::Spotlight.selected(&plan) && prog.done & Stage::Spotlight.bit() == 0,
                Stage::Permissions.selected(&plan) && prog.done & Stage::Permissions.bit() == 0,
            ),
            _ => (false, false),
        };
        if !sp_due && !perm_due {
            return apply_on_first_run_at(root);
        }
        let (ws, sp, perm) = std::thread::scope(|s| {
            let ws = s.spawn(|| apply_on_first_run_at(root));
            let sp = sp_due.then(|| {
                s.spawn(|| {
                    let sys = aterm_machines::port::Live::new();
                    let ctx = aterm_machines::init::initial_setup_context(&sys)?;
                    aterm_machines::init::run_initial_spotlight(
                        &sys,
                        &ctx,
                        aterm_machines::model::Mode::Apply,
                    )
                    .map(item_rows)
                })
            });
            let perm = perm_due.then(|| s.spawn(|| read_permissions_live(true)));
            (
                ws.join().expect("workstation apply thread"),
                sp.map(|h| h.join().expect("spotlight apply thread")),
                perm.map(|h| h.join().expect("permissions apply thread")),
            )
        });
        self.spotlight_result = sp;
        self.permissions_result = perm;
        ws
    }

    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        if let Some(res) = self.spotlight_result.take() {
            return res;
        }
        let sys = aterm_machines::port::Live::new();
        aterm_machines::init::run_initial_spotlight(
            &sys,
            &self.context()?,
            aterm_machines::model::Mode::Apply,
        )
        .map(item_rows)
    }

    fn permissions(&mut self) -> Result<Vec<Row>, String> {
        if let Some(res) = self.permissions_result.take() {
            return res;
        }
        self.read_permissions(true)
    }

    fn secrets(
        &mut self,
        plan: &Plan,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        // `aterm init`'s vault item, unattended: every change it makes (this Mac's key and
        // source, its join request, the subscriber, the files) comes after its line in the
        // converge journal, and a vault that does not answer is stood aside before any.
        let sys = aterm_machines::port::Live::new();
        let ctx = self.context()?;
        let ops = aterm_machines::init_secrets::unattended(&ctx)?;
        aterm_machines::init::run_initial_secrets(
            &sys,
            &ctx,
            ops,
            &plan.vault_url,
            replaces,
            password,
            APPROVAL_WAIT,
        )
        .map(secrets_rows)
    }

    fn secrets_pace(&mut self) -> Option<Pace> {
        Some(secrets_retry::pace(&self.reads))
    }

    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        live_configured_vault()
    }

    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        check::current_vault_rows(plan)
    }

    fn repositories(&mut self) -> Result<Vec<Row>, String> {
        let sys = aterm_machines::port::Live::new();
        aterm_machines::init_github::run_initial_repositories(
            &sys,
            &self.context()?,
            aterm_machines::model::Mode::Apply,
        )
        .map(item_rows)
    }

    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
        super::follow_up_locked_at(root)
    }
}

/// How long the setup's join waits for the vault to let this Mac in. An admin Mac's
/// subscriber reads the join queue every 2 s, so an open vault, or a request that proves the
/// join password, is approved and synced well within it; past it the row says the Mac waits,
/// and the subscriber finishes the join once approved (the stage's next try sees it at the
/// worker's next pass once the subscriber wrote it).
const APPROVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

fn readiness_row(name: &str, ready: bool, detail: String) -> Row {
    Row {
        name: name.into(),
        status: if ready { "ok" } else { "blocked" }.into(),
        detail,
    }
}

fn rows_ready(rows: &[Row]) -> bool {
    !rows.is_empty()
        && rows
            .iter()
            .all(|row| matches!(row.status.as_str(), "ok" | "applied"))
}

/// What the Secrets stage of a run does with the vault this Mac is in now.
enum SecretsTry {
    /// Leaves it in place of the one the plan names ([`AdmittedVault::stands_in`]).
    LeaveInPlace(ConfiguredVault),
    /// Runs the vault item, whose join replaces only the configuration this read found.
    RunItem(Replaces),
}

/// What the Secrets stage of a run admitted with `admitted` does with the vault this Mac is
/// in now. A run an older build admitted reads nothing, and its join replaces whatever its
/// probe reads.
fn secrets_try(
    host: &mut dyn InitialHost,
    plan: &Plan,
    admitted: AdmittedVault,
) -> Result<SecretsTry, String> {
    if admitted == AdmittedVault::Unknown {
        return Ok(SecretsTry::RunItem(Replaces::Any));
    }
    let configured = host
        .configured_vault()
        .map_err(|error| format!("cannot read which vault this Mac is in: {error}"))?;
    Ok(match configured {
        Some(vault) if admitted.stands_in(plan, &vault) => SecretsTry::LeaveInPlace(vault),
        configured => SecretsTry::RunItem(Replaces::Read(configured.map(|vault| vault.config))),
    })
}

fn execute_initial(
    host: &mut dyn InitialHost,
    root: &Path,
    password: &mut Option<SecretBytes>,
) -> Result<Option<Report>, String> {
    if finished_at(root)? {
        return Ok(None);
    }
    let Some(plan) = saved_plan(root)? else {
        return Ok(None);
    };
    let Some(mut progress) = progress(root)? else {
        // A legacy plan authorized its original workstation rows only.
        return host.workstation(root);
    };
    if !progress.belongs_to(&plan) || !progress.admitted {
        // A partially published choice, including a pending record from before
        // admission was explicit, needs authorization before any OS work.
        return Ok(None);
    }
    let mut report = Report {
        action: Action::Apply,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: root.join("original-preferences.json"),
    };
    for stage in Stage::ALL {
        if !stage.selected(&plan) || progress.done & stage.bit() != 0 {
            continue;
        }
        if stage == Stage::Repositories && Stage::Secrets.selected(&plan) {
            if progress.done & Stage::Secrets.bit() != 0 {
                // A subscriber can fail to materialize a later version while
                // repository setup is pending. Historical completion never
                // admits stale credentials into a new repository attempt. The
                // vault read is the one the Secrets stage left in place, if any.
                let current = secrets_try(host, &plan, progress.admitted_vault).and_then(|read| {
                    host.current_vault(&match read {
                        SecretsTry::LeaveInPlace(vault) => vault.present(plan.clone()),
                        SecretsTry::RunItem(_) => plan.clone(),
                    })
                });
                if !current.as_ref().is_ok_and(|rows| rows_ready(rows)) {
                    progress.done &= !Stage::Secrets.bit();
                    progress.running = None;
                    save_progress(root, progress)?;
                    match current {
                        Ok(rows) => report.rows.extend(rows),
                        Err(detail) => report.rows.push(Row {
                            name: Stage::Secrets.name().into(),
                            status: "unverified".into(),
                            detail,
                        }),
                    }
                }
            }
            if progress.done & Stage::Secrets.bit() == 0 {
                report.rows.push(readiness_row(
                    stage.name(),
                    false,
                    "waiting for the selected secrets vault to synchronize credentials".into(),
                ));
                continue;
            }
        }
        // A Secrets try that did not finish pauses the next until its time or a change
        // that can alter its answer; meanwhile its rows stand.
        let paced = if stage == Stage::Secrets {
            host.secrets_pace()
        } else {
            None
        };
        if let Some(rows) = paced
            .as_ref()
            .and_then(|pace| secrets_retry::paused(root, progress.revision, pace))
        {
            report.rows.extend(rows);
            continue;
        }
        progress.running = Some(stage);
        save_progress(root, progress)?;
        let result = match stage {
            Stage::Workstation => match host.workstation(root) {
                Ok(Some(result)) => Ok(result.rows),
                Ok(None) if first_run_complete(root)? => Ok(vec![readiness_row(
                    stage.name(),
                    true,
                    "selected Mac settings already verified".into(),
                )]),
                Ok(None) => Ok(vec![readiness_row(
                    stage.name(),
                    false,
                    "waiting for the next automatic Mac settings retry".into(),
                )]),
                Err(error) => Err(error),
            },
            Stage::Spotlight => host.spotlight(),
            Stage::Permissions => host.permissions(),
            // A vault this Mac joined after the run was admitted stays: nothing is joined,
            // made or journaled, and the row says how to move the Mac. The stage is done
            // once that vault reads synced with its subscriber running, as repository setup
            // reads it; until then its rows say what it waits for. One that cannot be read
            // is not joined over either; the try changed nothing, so it is no failure.
            Stage::Secrets => match secrets_try(host, &plan, progress.admitted_vault) {
                Ok(SecretsTry::LeaveInPlace(vault)) => {
                    let mut rows = vec![readiness_row(
                        stage.name(),
                        true,
                        vault.left_in_place(&plan),
                    )];
                    match host.current_vault(&vault.present(plan.clone())) {
                        Ok(read) => rows.extend(read),
                        Err(detail) => rows.push(Row {
                            name: stage.name().into(),
                            status: "unverified".into(),
                            detail,
                        }),
                    }
                    Ok(rows)
                }
                Ok(SecretsTry::RunItem(replaces)) => host.secrets(&plan, replaces, password),
                Err(detail) => Ok(vec![Row {
                    name: stage.name().into(),
                    status: "unverified".into(),
                    detail,
                }]),
            },
            Stage::Repositories => host.repositories(),
        };
        let (ready, mut rows) = match result {
            Ok(rows) => (rows_ready(&rows), rows),
            Err(detail) => (
                false,
                vec![Row {
                    name: stage.name().into(),
                    status: "error".into(),
                    detail,
                }],
            ),
        };
        if ready {
            progress.done |= stage.bit();
        }
        progress.running = None;
        save_progress(root, progress)?;
        if paced.is_some()
            && let Some(after) = host.secrets_pace()
        {
            secrets_retry::record(root, progress.revision, &after, ready, &mut rows)?;
        }
        report.rows.extend(rows);
    }
    // What a Workstation pass left alone in another app's container for want of Full Disk
    // Access (Notification Center's widgets) is reached once this process holds the grant:
    // once per pass, after the stages, so a grant the Permissions stage's hand-over brings,
    // or a record this pass of the Workstation stage writes, is reached in the same pass;
    // whether the Workstation stage is done or still pending (its automatic passes hold the
    // row as it is once it settled for its other keys, and say the list is this step's,
    // [`super::SETTLED_OWED`]); silently, never a stage of its own, so no stage waits on it
    // and nothing reruns without the grant.
    match host.withheld(root) {
        Ok(Some(follow_up)) => report.rows.extend(follow_up.rows),
        Ok(None) => {}
        Err(detail) => report.rows.push(Row {
            name: "Settings waiting for Full Disk Access".into(),
            status: "unverified".into(),
            detail,
        }),
    }
    if Stage::ALL
        .iter()
        .all(|stage| !stage.selected(&plan) || progress.done & stage.bit() != 0)
    {
        progress.completed = true;
        save_progress(root, progress)?;
    }
    Ok(Some(report))
}

/// Apply independently selected setup stages after authorization. Pending
/// enrollment, permissions and offline services remain retryable. This never
/// reads a terminal or stores the optional join password; a join that sent it
/// takes it (`None` afterwards), and one that failed leaves it for the next pass.
pub fn apply_initial_setup(
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
) -> Result<Option<Report>, String> {
    apply_initial_setup_observed(expected_revision, password).result
}

/// Apply with an observation captured under the existing profile lock.
pub fn apply_initial_setup_observed(
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
) -> InitialSetupObserved<Option<Report>> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return InitialSetupObserved::new(Ok(None), None);
    }
    observe_person_mutation(Ok(None), |root| {
        execute_initial_for_revision(&mut LiveInitial::live(), root, expected_revision, password)
    })
}

fn execute_initial_for_revision(
    host: &mut dyn InitialHost,
    root: &Path,
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
) -> Result<Option<Report>, String> {
    // The caller holds profile.lock: a legacy doctor Apply can revive a stopped
    // run without taking automatic.lock after a scheduler's earlier revision read.
    if revision_at(root)? != expected_revision {
        return Ok(None);
    }
    execute_initial(host, root, password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Vec<Stage>,
        pending: Option<Stage>,
        failure: Option<Stage>,
        password_seen: bool,
        observe: Option<PathBuf>,
        vault_checks: usize,
        vault_blocked: bool,
        vault_error: bool,
        vault_url: Option<String>,
        /// What [`InitialHost::secrets_pace`] reads: `None` tries the stage each pass.
        pace: Option<Pace>,
        /// The vault this Mac is in ([`InitialHost::configured_vault`]).
        configured: Option<ConfiguredVault>,
        /// Its configuration cannot be read.
        configured_unreadable: bool,
        /// The source the last [`InitialHost::current_vault`] read was asked about.
        checked_url: Option<String>,
    }

    impl Fake {
        fn stage(&mut self, stage: Stage) -> Result<Vec<Row>, String> {
            self.calls.push(stage);
            if let Some(root) = &self.observe {
                let (_, finished, rows) = status_at(root, Plan::default()).unwrap();
                assert!(!finished);
                assert!(
                    rows.iter()
                        .any(|row| row.name == stage.name() && row.status == "running")
                );
            }
            if self.failure == Some(stage) {
                return Err("offline; retry later".into());
            }
            Ok(vec![readiness_row(
                stage.name(),
                self.pending != Some(stage),
                "injected stage outcome".into(),
            )])
        }
    }

    impl InitialHost for Fake {
        fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String> {
            if first_run_complete(root)? {
                return Ok(None);
            }
            let rows = self.stage(Stage::Workstation)?;
            if rows_ready(&rows) {
                write_json(
                    &root.join("first-run.json"),
                    &json!({"profile":PROFILE,"state":"applied","accounts_verified":true}),
                )?;
            }
            Ok(Some(Report {
                action: Action::Apply,
                rows,
                manual_steps: Vec::new(),
                backup: root.join("original-preferences.json"),
            }))
        }
        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            self.stage(Stage::Spotlight)
        }
        fn permissions(&mut self) -> Result<Vec<Row>, String> {
            self.stage(Stage::Permissions)
        }
        fn secrets(
            &mut self,
            plan: &Plan,
            _replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            self.password_seen = password.is_some();
            self.vault_url = Some(plan.vault_url.clone());
            self.stage(Stage::Secrets)
        }
        fn secrets_pace(&mut self) -> Option<Pace> {
            self.pace.clone()
        }
        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            if self.configured_unreadable {
                return Err("source.toml: no name".into());
            }
            Ok(self.configured.clone())
        }
        fn repositories(&mut self) -> Result<Vec<Row>, String> {
            self.stage(Stage::Repositories)
        }

        fn withheld(&mut self, _root: &Path) -> Result<Option<Report>, String> {
            Ok(None)
        }

        fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
            self.vault_checks += 1;
            self.checked_url = Some(plan.vault_url.clone());
            if self.vault_error {
                return Err("injected local vault readiness failure".into());
            }
            Ok(vec![readiness_row(
                Stage::Secrets.name(),
                !self.vault_blocked,
                "injected current vault readiness".into(),
            )])
        }
    }

    fn selected(settings: &[Setting]) -> Plan {
        let mut selection = Selection::none();
        for setting in settings {
            selection.set(*setting, true);
        }
        Plan::from(selection)
    }

    #[test]
    fn an_old_startup_offer_cannot_replace_a_newer_pending_doctor_plan() {
        let root = aterm_tempfile::tempdir().unwrap();
        let offered = Plan::default();
        let baseline = observation_at(root.path()).unwrap();
        let mut authorization = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), 0);
        // Restore stops first even when recovery preflight fails. Doctor Apply
        // then revives only its eight groups before hitting the same bad backup.
        std::fs::write(
            root.path().join("original-preferences.json"),
            b"invalid backup",
        )
        .unwrap();
        assert!(super::super::tests::restore_without_effects(root.path()).is_err());
        save_explicit_workstation_plan(root.path()).unwrap();
        let doctor_plan = saved_plan(root.path()).unwrap().unwrap();
        assert_eq!(doctor_plan.selection, Selection::all().workstation());
        assert_eq!(revision_at(root.path()).unwrap(), 1);
        assert!(!finished_at(root.path()).unwrap());
        let before = observation_at(root.path()).unwrap();
        assert_ne!(before, baseline);

        // The intervening stop is already gone when the old window's untouched
        // timer returns Begin. Its captured publications must still fence it.
        let accepted = authorization
            .persist_at(root.path(), &offered)
            .result
            .unwrap();
        let mut old_worker = Fake {
            failure: Some(Stage::Workstation),
            ..Fake::default()
        };
        if let Some(revision) = accepted {
            execute_initial_for_revision(&mut old_worker, root.path(), revision, &mut None)
                .unwrap();
        }
        assert!(
            old_worker.calls.is_empty(),
            "a stale offer ran stages from its older plan"
        );
        assert_eq!(accepted, None);
        assert_eq!(revision_at(root.path()).unwrap(), 1);
        assert_eq!(saved_plan(root.path()).unwrap(), Some(doctor_plan));
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }

    fn restart_attempt(
        attempt: &mut InitialSetupRestart,
        root: &Path,
        plan: &Plan,
        commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_mutation_at(
            root,
            observation_at(root).ok(),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                attempt.persist_with(root, plan, true, || Ok(None), commit, observe)
            },
        )
    }

    fn observe_restart(root: &Path) -> Result<InitialSetupObservation, String> {
        observation_at(root).map_err(|error| error.to_string())
    }

    /// System State's Apply of `plan` (`vault_typed`: its vault typed in the field) while
    /// this Mac is in the vault `read` finds: the admitted run's revision.
    fn system_state_apply(
        root: &Path,
        plan: &Plan,
        vault_typed: bool,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
    ) -> u64 {
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::default().persist_with(
                root,
                plan,
                vault_typed,
                read,
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .expect("admitted")
    }

    #[test]
    fn restart_final_save_recovers_before_or_after_publication_without_a_new_revision() {
        for empty in [false, true] {
            for published in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let plan = selected(if empty { &[] } else { &[Setting::Secrets] });
                let original = authorize_at(root.path(), &plan).unwrap().unwrap();
                let mut attempt = InitialSetupRestart::default();
                let failed = restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        if published {
                            save_progress(root, next)?;
                        }
                        Err("injected final namespace flush failure".into())
                    },
                    observe_restart,
                );
                assert!(failed.result.is_err());
                assert!(attempt.pending());
                let staged = progress(root.path()).unwrap().unwrap();
                assert_eq!(staged.revision, original + 1);
                assert_eq!(staged.admitted, published);
                let before = observation_at(root.path()).unwrap();
                let commits = std::cell::Cell::new(0);
                let recovered = restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        commits.set(commits.get() + 1);
                        save_progress(root, next)
                    },
                    observe_restart,
                );
                assert_eq!(recovered.result.unwrap(), Some(staged.revision));
                assert!(!attempt.pending());
                assert_eq!(commits.get(), usize::from(!published));
                assert_eq!(revision_at(root.path()).unwrap(), staged.revision);
                assert_eq!(saved_plan(root.path()).unwrap(), Some(plan));
                if published {
                    assert_eq!(observation_at(root.path()).unwrap(), before);
                }
                let password = SecretBytes::from(b"test-only-retained-password".as_slice());
                let mut host = Fake::default();
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    staged.revision,
                    &mut Some(password.clone()),
                )
                .unwrap();
                assert_eq!(
                    host.calls,
                    if empty {
                        Vec::new()
                    } else {
                        vec![Stage::Secrets]
                    }
                );
                assert_eq!(host.password_seen, !empty);
                assert!(finished_at(root.path()).unwrap());
            }
        }
    }

    #[test]
    fn repeated_restart_commit_and_confirmation_errors_retain_the_owned_publications() {
        for published in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets]);
            let mut attempt = InitialSetupRestart::default();
            assert!(
                restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        if published {
                            save_progress(root, next)?;
                        }
                        Err("injected final flush failure".into())
                    },
                    observe_restart,
                )
                .result
                .is_err()
            );
            let owned = observation_at(root.path()).unwrap();
            for _ in 0..2 {
                let failed = restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |_, _| Err("injected repeated final flush failure".into()),
                    |root| {
                        if published {
                            Err("injected confirmation directory flush failure".into())
                        } else {
                            observe_restart(root)
                        }
                    },
                );
                assert!(failed.result.is_err());
                assert!(attempt.pending());
                assert_eq!(observation_at(root.path()).unwrap(), owned);
            }
            let recovered = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                save_progress,
                observe_restart,
            );
            assert_eq!(recovered.result.unwrap(), Some(1));
            assert!(!attempt.pending());
            assert_eq!(revision_at(root.path()).unwrap(), 1);
        }
    }

    #[test]
    fn restart_prefinal_failures_never_confirm_an_older_same_plan_admission() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut prior = progress(root.path()).unwrap().unwrap();
        prior.revision = u64::MAX;
        save_progress(root.path(), prior).unwrap();
        let before = observation_at(root.path()).unwrap();
        let mut attempt = InitialSetupRestart::default();
        let failed = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |_, _| panic!("a preflight failure cannot reach final admission"),
            observe_restart,
        );
        assert!(failed.result.is_err());
        assert!(!attempt.pending());
        assert_eq!(observation_at(root.path()).unwrap(), before);
        assert_eq!(
            admission_at(root.path()).unwrap().unwrap().revision,
            u64::MAX
        );
        assert_eq!(
            restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                save_progress,
                observe_restart
            )
            .result
            .unwrap(),
            None
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);

        let staged = aterm_tempfile::tempdir().unwrap();
        let mut attempt = InitialSetupRestart::default();
        assert!(
            restart_attempt(
                &mut attempt,
                staged.path(),
                &plan,
                |_, _| panic!("an unconfirmed staged run cannot reach final admission"),
                |_| Err("injected staged observation failure".into()),
            )
            .result
            .is_err()
        );
        assert!(!attempt.pending());
        assert!(admission_at(staged.path()).unwrap().is_none());
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, staged.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
    }

    #[test]
    fn restart_recovery_retires_after_a_stop_new_run_or_same_content_publication() {
        for replacement in 0..5 {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets]);
            let mut attempt = InitialSetupRestart::default();
            assert!(
                restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        save_progress(root, next)?;
                        Err("injected published admission failure".into())
                    },
                    observe_restart,
                )
                .result
                .is_err()
            );
            {
                let _writer = HeldLock::take(root.path()).unwrap();
                match replacement {
                    0 | 1 => {
                        write_json(
                            &root.path().join("first-run.json"),
                            &json!({"profile":PROFILE,"state":if replacement == 0 {"cancelled"} else {"restored"}}),
                        ).unwrap();
                        let progress = root.path().join("initial-setup.json");
                        std::fs::remove_file(&progress).unwrap();
                        std::fs::create_dir(progress).unwrap();
                    }
                    2 => {
                        restart_at(root.path(), &plan).unwrap();
                    }
                    3 => {
                        save_progress(root.path(), progress(root.path()).unwrap().unwrap())
                            .unwrap();
                    }
                    _ => {
                        aterm_machines::port::write_atomic(
                            &root.path().join("first-run.json"),
                            b"invalid decision",
                            0o600,
                        )
                        .unwrap();
                    }
                }
            }
            let before = observation_at(root.path()).ok();
            let progress_bytes = std::fs::read(root.path().join("initial-setup.json")).ok();
            let decision_bytes = std::fs::read(root.path().join("first-run.json")).unwrap();
            let recovered = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |_, _| panic!("supersession cannot write the old admission"),
                observe_restart,
            );
            assert_eq!(recovered.result.unwrap(), None);
            assert!(!attempt.pending());
            assert_eq!(observation_at(root.path()).ok(), before);
            assert_eq!(
                std::fs::read(root.path().join("initial-setup.json")).ok(),
                progress_bytes
            );
            if replacement <= 1 {
                assert!(root.path().join("initial-setup.json").is_dir());
            }
            assert_eq!(
                std::fs::read(root.path().join("first-run.json")).unwrap(),
                decision_bytes
            );
        }
    }

    #[test]
    fn failed_restart_publication_capture_cannot_bind_a_later_readable_record() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let mut attempt = InitialSetupRestart::default();
        let final_progress = std::cell::Cell::new(None);
        assert!(
            restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |root, next| {
                    final_progress.set(Some(next));
                    let path = root.join("initial-setup.json");
                    std::fs::remove_file(&path).unwrap();
                    std::fs::create_dir(path).unwrap();
                    Err("injected inaccessible publication".into())
                },
                observe_restart,
            )
            .result
            .is_err()
        );
        assert!(!attempt.pending());
        std::fs::remove_dir(root.path().join("initial-setup.json")).unwrap();
        save_progress(root.path(), final_progress.get().unwrap()).unwrap();
        let before = observation_at(root.path()).unwrap();
        let retired = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |_, _| panic!("an unbound failure cannot adopt a later publication"),
            observe_restart,
        );
        assert_eq!(retired.result.unwrap(), None);
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }

    #[test]
    fn admitted_run_observation_preserves_the_source_revision_and_stage_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Repositories]);
        plan.vault_url = "https://vault.example/v1/team".into();
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.done = Stage::Workstation.bit();
        saved.running = Some(Stage::Secrets);
        save_progress(root.path(), saved).unwrap();
        let before = observation_at(root.path()).unwrap();

        assert_eq!(
            admission_at(root.path()).unwrap(),
            Some(InitialSetupAdmission {
                plan: plan.clone(),
                revision,
                completed: false,
            })
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);
        assert_eq!(progress(root.path()).unwrap(), Some(saved));
        assert!(!root.path().join("profile.lock").exists());

        saved.done |= Stage::Secrets.bit() | Stage::Repositories.bit();
        saved.running = None;
        saved.completed = true;
        save_progress(root.path(), saved).unwrap();
        assert_eq!(
            admission_at(root.path()).unwrap(),
            Some(InitialSetupAdmission {
                plan,
                revision,
                completed: true,
            })
        );
    }

    #[test]
    fn staged_mismatched_legacy_and_missing_choices_are_not_admitted_runs() {
        let absent = aterm_tempfile::tempdir().unwrap();
        let missing_root = absent.path().join("not-created");
        assert_eq!(admission_at(&missing_root).unwrap(), None);
        assert!(!missing_root.exists());

        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let saved = progress(root.path()).unwrap().unwrap();
        save_progress(
            root.path(),
            Progress {
                admitted: false,
                ..saved
            },
        )
        .unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);

        save_progress(root.path(), saved).unwrap();
        let mut replacement = plan.clone();
        replacement.vault_url = "https://vault.example/v1/other".into();
        save_plan(root.path(), &replacement).unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);

        save_automatic_plan(root.path(), plan.selection).unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);
        save_plan(root.path(), &plan).unwrap();
        for decision in [Decision::Missing, Decision::Applied(false)] {
            let path = root.path().join("first-run.json");
            if decision == Decision::Missing {
                std::fs::remove_file(&path).unwrap();
            } else {
                write_json(
                    &path,
                    &json!({"profile":PROFILE,"state":"applied","accounts_verified":false}),
                )
                .unwrap();
            }
            assert_eq!(admission_at(root.path()).unwrap(), None);
        }
        save_workstation_authorization(root.path(), &plan).unwrap();
        for record in ["automatic-plan.json", "initial-setup.json"] {
            let path = root.path().join(record);
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            assert_eq!(admission_at(root.path()).unwrap(), None);
            std::fs::write(path, bytes).unwrap();
        }
        assert!(admission_at(root.path()).unwrap().is_some());
    }

    #[test]
    fn an_empty_admission_published_before_an_error_can_be_confirmed_without_rewriting_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let empty = selected(&[]);
        assert!(
            authorize_with(
                root.path(),
                &empty,
                AdmittedVault::None,
                |root, progress| {
                    save_progress(root, progress)?;
                    Err("injected failure after publishing the final admission".into())
                }
            )
            .is_err()
        );
        let before = observation_at(root.path()).unwrap();
        let admitted = admission_at(root.path()).unwrap().unwrap();
        assert_eq!(admitted.plan, empty);
        assert!(admitted.completed);
        assert_eq!(admitted.revision, revision_at(root.path()).unwrap());
        assert_eq!(observation_at(root.path()).unwrap(), before);
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());

        for state in ["cancelled", "restored"] {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            assert_eq!(admission_at(root.path()).unwrap(), None);
        }
        let next_plan = selected(&[Setting::Secrets]);
        let next_revision = restart_at(root.path(), &next_plan).unwrap();
        assert_eq!(
            admission_at(root.path()).unwrap(),
            Some(InitialSetupAdmission {
                plan: next_plan,
                revision: next_revision,
                completed: false,
            })
        );
        assert_ne!(next_revision, admitted.revision);

        let unpublished = aterm_tempfile::tempdir().unwrap();
        assert!(
            authorize_with(unpublished.path(), &empty, AdmittedVault::None, |_, _| {
                Err("injected failure before publishing the final admission".into())
            })
            .is_err()
        );
        assert_eq!(admission_at(unpublished.path()).unwrap(), None);
        assert!(!finished_at(unpublished.path()).unwrap());
    }

    #[test]
    fn unreadable_admission_records_return_errors_instead_of_absence() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        for record in [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ] {
            let path = root.path().join(record);
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, b"invalid setup record").unwrap();
            assert!(admission_at(root.path()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"invalid setup record");
            std::fs::write(path, bytes).unwrap();
        }
        assert!(admission_at(root.path()).unwrap().is_some());
    }

    #[test]
    fn a_durable_stop_ends_admission_even_when_plan_or_progress_is_unreadable() {
        for record in ["automatic-plan.json", "initial-setup.json"] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
            let path = root.path().join(record);
            std::fs::write(&path, b"invalid setup record").unwrap();
            assert!(admission_at(root.path()).is_err());
            for state in ["cancelled", "restored"] {
                write_json(
                    &root.path().join("first-run.json"),
                    &json!({"profile":PROFILE,"state":state}),
                )
                .unwrap();
                assert!(finished_at(root.path()).unwrap());
                assert_eq!(admission_at(root.path()).unwrap(), None);
                assert_eq!(std::fs::read(&path).unwrap(), b"invalid setup record");
            }
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":"pending"}),
            )
            .unwrap();
            assert!(admission_at(root.path()).is_err());
        }
    }

    #[test]
    fn observations_are_read_only_and_unchanged_records_remain_equal() {
        let root = aterm_tempfile::tempdir().unwrap();
        let absent = observation_at(root.path()).unwrap();
        assert_eq!(observation_at(root.path()).unwrap(), absent);
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let current = observation_at(root.path()).unwrap();
        assert_ne!(current, absent);
        assert_eq!(current.clone(), observation_at(root.path()).unwrap());
        assert_eq!(
            snapshot_at(root.path(), Plan::default())
                .unwrap()
                .observation,
            current
        );
    }

    #[test]
    fn observed_read_only_pass_expires_across_a_pending_stage_aba() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let original = Records::read(root.path()).unwrap();
        let before = observation_at(root.path()).unwrap();
        let stable = observe_read_only(Ok(root.path().into()), |_| Ok(7));
        assert_eq!(stable.result.unwrap(), 7);
        assert_eq!(stable.observation, Some(before.clone()));

        let changed = observe_read_only(Ok(root.path().into()), |root| {
            let _writer = HeldLock::take(root)?;
            let mut host = Fake {
                pending: Some(Stage::Secrets),
                ..Fake::default()
            };
            let report = execute_initial(&mut host, root, &mut None)?.unwrap();
            assert!(report.needs_attention());
            assert_eq!(host.calls, [Stage::Secrets]);
            Ok(7)
        });
        assert_eq!(changed.result.unwrap(), 7);
        assert!(changed.observation.is_none());
        assert_eq!(Records::read(root.path()).unwrap(), original);
        assert_ne!(observation_at(root.path()).unwrap(), before);
    }

    #[test]
    fn observed_check_cannot_bind_during_a_profile_pass_with_unchanged_records() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let context = observation_at(root.path()).unwrap();
        let writer = HeldLock::take(root.path()).unwrap();
        let observed = observe_read_only(Ok(root.path().into()), |_| Ok(7));
        assert_eq!(observed.result.unwrap(), 7);
        assert!(observed.observation.is_none());
        assert_eq!(observation_at(root.path()).unwrap(), context);
        drop(writer);
        let observed = observe_read_only(Ok(root.path().into()), |_| Ok(7));
        assert_eq!(observed.result.unwrap(), 7);
        assert_eq!(observed.observation, Some(context));
    }

    #[test]
    fn observed_restore_error_is_bound_under_lock_and_expires_after_a_new_run() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let before = observation_at(root.path()).unwrap();
        let cancelled =
            InitialSetupCancellation::default().persist_observed_at(root.path(), Some(before));
        assert!(cancelled.result.unwrap());
        let cancelled = cancelled.observation.unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        std::fs::write(
            root.path().join("original-preferences.json"),
            b"invalid backup",
        )
        .unwrap();
        let restored = observe_mutation_at(
            root.path(),
            Some(cancelled.clone()),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                super::super::tests::restore_without_effects(root)
            },
        );
        assert!(restored.result.is_err());
        let restored = restored.observation.unwrap();
        assert_ne!(restored, cancelled);
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.finished);
        assert_eq!(snapshot.observation, restored);

        // Publication can be delayed until after another process accepts Apply.
        // The old failure must retain its original context rather than borrow it.
        restart_at(root.path(), &plan).unwrap();
        assert_ne!(observation_at(root.path()).unwrap(), restored);
    }

    #[test]
    fn observed_busy_failure_keeps_only_its_pre_attempt_context() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let before = observation_at(root.path()).unwrap();
        let writer = HeldLock::take(root.path()).unwrap();
        let refused: InitialSetupObserved<()> = observe_mutation_at(
            root.path(),
            Some(before.clone()),
            Err("busy".into()),
            |_| panic!("a busy profile must not run the operation"),
        );
        assert!(refused.result.is_err());
        assert_eq!(refused.observation, Some(before));
        drop(writer);
        restart_at(root.path(), &plan).unwrap();
        assert_ne!(
            refused.observation,
            Some(observation_at(root.path()).unwrap())
        );
    }

    #[test]
    fn observation_read_failures_never_relabel_an_operation_result() {
        for directory in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let path = root.path().join("first-run.json");
            if directory {
                std::fs::create_dir(&path).unwrap();
            } else {
                std::fs::write(&path, b"invalid decision").unwrap();
            }
            assert!(matches!(
                observation_at(root.path()),
                Err(InitialSetupSnapshotError::Read(_))
            ));
            let observed: InitialSetupObserved<()> =
                observe_read_only(Ok(root.path().into()), |_| Err("original failure".into()));
            assert_eq!(observed.result.unwrap_err(), "original failure");
            assert!(observed.observation.is_none());
        }
    }

    #[test]
    fn a_saved_running_stage_needs_a_live_profile_pass() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.running = Some(Stage::Secrets);
        save_progress(root.path(), saved).unwrap();
        {
            let _active = HeldLock::take(root.path()).unwrap();
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(snapshot.rows[0].status, "running");
        }
        for missing_lock in [false, true] {
            if missing_lock {
                std::fs::remove_file(root.path().join("profile.lock")).unwrap();
            }
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(
                snapshot.rows[0].status, "pending",
                "an ended worker cannot remain live after restart"
            );
            assert!(!snapshot.rows[0].detail.is_empty());
            assert!(!snapshot.finished);
            assert_eq!(snapshot.revision, saved.revision);
            assert_eq!(progress(root.path()).unwrap().unwrap(), saved);
            if missing_lock {
                assert!(!root.path().join("profile.lock").exists());
            }
        }
    }

    #[test]
    fn a_snapshot_samples_profile_activity_after_its_record_reads() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.running = Some(Stage::Secrets);
        save_progress(root.path(), saved).unwrap();
        let reads = std::cell::Cell::new(0);
        let writer = std::cell::RefCell::new(None);
        let snapshot = snapshot_with(
            Plan::default(),
            || {
                assert_eq!(reads.get(), 2);
                HeldLock::profile_active(root.path())
            },
            || {
                let record = Records::read(root.path())?;
                reads.set(reads.get() + 1);
                if reads.get() == 2 {
                    *writer.borrow_mut() = Some(HeldLock::take(root.path())?);
                }
                Ok(record)
            },
        )
        .unwrap();
        assert_eq!(snapshot.rows[0].status, "running");
        drop(writer.into_inner());
        assert!(!HeldLock::profile_active(root.path()).unwrap());
        let _next_writer = HeldLock::take(root.path()).unwrap();
    }

    #[test]
    fn stale_cancel_retry_cannot_stop_a_newer_pending_run() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let original = revision_at(root.path()).unwrap();
        let mut cancel = InitialSetupCancellation::default();
        // Another process is still applying this run when Cancel is chosen.
        let active = HeldLock::take(root.path()).unwrap();
        assert!(cancel.persist_at(root.path()).is_err());
        drop(active);

        // That process can accept a new Apply while the old Cancel retains
        // only its own process's busy reservation during the retry backoff.
        restart_at(root.path(), &plan).unwrap();
        assert!(revision_at(root.path()).unwrap() > original);
        assert!(!cancel.persist_at(root.path()).unwrap());
        assert!(
            !finished_at(root.path()).unwrap(),
            "the old retry stopped the new run"
        );
    }

    #[test]
    fn explicit_mac_apply_retires_workers_when_it_revives_a_stopped_modern_run() {
        for stop in ["cancelled", "restored"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets]);
            authorize_at(root.path(), &plan).unwrap();
            let original = revision_at(root.path()).unwrap();
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":stop}),
            )
            .unwrap();
            save_explicit_workstation_plan(root.path()).unwrap();
            let current = revision_at(root.path()).unwrap();
            assert!(
                current > original,
                "{stop}: explicit apply revived an older worker's password"
            );
            // The scheduler can have read the old revision immediately before
            // this revival. Its actual effect boundary must check under lock.
            let _profile = HeldLock::take(root.path()).unwrap();
            let password = SecretBytes::from(b"test-only-old-run-password".as_slice());
            let mut host = Fake::default();
            assert!(
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    original,
                    &mut Some(password.clone())
                )
                .unwrap()
                .is_none()
            );
            assert!(host.calls.is_empty());
            assert!(!host.password_seen);
            execute_initial_for_revision(&mut host, root.path(), current, &mut None).unwrap();
            assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        }
    }

    #[test]
    fn explicit_mac_apply_retires_legacy_run_zero_after_an_earlier_stop() {
        for stop in ["cancelled", "restored"] {
            let root = aterm_tempfile::tempdir().unwrap();
            assert_eq!(revision_at(root.path()).unwrap(), 0);
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":stop}),
            )
            .unwrap();
            save_explicit_workstation_plan(root.path()).unwrap();
            assert_ne!(
                revision_at(root.path()).unwrap(),
                0,
                "{stop}: legacy run zero survived"
            );
        }
    }

    #[test]
    fn retained_cancel_retries_the_same_run_and_accepts_its_already_finished_decision() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let mut cancel = InitialSetupCancellation::default();
        let active = HeldLock::take(root.path()).unwrap();
        assert!(cancel.persist_at(root.path()).is_err());
        drop(active);
        assert!(cancel.persist_at(root.path()).unwrap());
        assert!(finished_at(root.path()).unwrap());
        assert!(cancel.persist_at(root.path()).unwrap());
    }

    #[test]
    fn explicit_workstation_completion_does_not_complete_independent_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Permissions, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        save_explicit_workstation_plan(root.path()).unwrap();
        complete_explicit_workstation_plan(root.path()).unwrap();
        let current = progress(root.path()).unwrap().unwrap();
        assert_eq!(current.revision, revision);
        assert_eq!(current.done, Stage::Workstation.bit());
        assert!(!current.completed);
        assert!(!finished_at(root.path()).unwrap());
    }

    #[test]
    fn permission_policy_requires_authorization_without_creating_missing_state() {
        let root = aterm_tempfile::tempdir().unwrap();
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!snapshot.permissions_allowed);
        assert!(!snapshot.finished);
        assert_eq!(snapshot.revision, 0);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);

        // A new plan may be published before its progress record. An older
        // successful Mac decision must not authorize the new permission group.
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"applied","accounts_verified":true}),
        )
        .unwrap();
        save_plan(root.path(), &Plan::default()).unwrap();
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
    }

    #[test]
    fn permission_policy_survives_completed_selected_setup_without_rerunning_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Permissions]);
        authorize_at(root.path(), &plan).unwrap();
        let pending = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(pending.permissions_allowed);
        assert!(!pending.finished);
        assert_ne!(pending.revision, 0);

        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let completed = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(completed.permissions_allowed);
        assert!(completed.finished);
        assert_eq!(completed.revision, pending.revision);
        assert_eq!(completed.plan, plan);
        assert!(completed.rows.iter().all(|row| row.status == "applied"));
    }

    #[test]
    fn permission_policy_honors_stops_and_the_latest_explicit_selection() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Permissions, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let revision = snapshot_at(root.path(), Plan::default()).unwrap().revision;
        for state in ["cancelled", "restored"] {
            // Restore publishes this before preference work and may fail later.
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            let stopped = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(!stopped.permissions_allowed);
            assert!(stopped.finished);
            assert_eq!(stopped.revision, revision);
        }

        restart_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let unchecked = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!unchecked.permissions_allowed);
        assert!(unchecked.revision > revision);
        restart_at(root.path(), &plan).unwrap();
        let selected = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(selected.permissions_allowed);
        assert!(selected.revision > unchecked.revision);
    }

    #[test]
    fn permission_policy_preserves_only_verified_legacy_completion() {
        let root = aterm_tempfile::tempdir().unwrap();
        save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
        for (state, verified, allowed) in [
            ("applied", true, true),
            ("applied", false, false),
            ("pending", false, false),
            ("cancelled", true, false),
            ("restored", true, false),
        ] {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
            )
            .unwrap();
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(snapshot.permissions_allowed, allowed, "{state}/{verified}");
            assert_eq!(snapshot.revision, 0);
        }
    }

    #[test]
    fn permission_policy_refuses_a_snapshot_that_changes_during_observation() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Permissions]);
        authorize_at(root.path(), &plan).unwrap();
        let before = Records::read(root.path()).unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"cancelled"}),
        )
        .unwrap();
        let after = Records::read(root.path()).unwrap();
        let mut reads = [before, after].into_iter();
        assert!(matches!(
            snapshot_with(Plan::default(), || Ok(false), || Ok(reads.next().unwrap())),
            Err(InitialSetupSnapshotError::Updating)
        ));
        assert!(matches!(
            snapshot_with(
                Plan::default(),
                || Ok(false),
                || Err("injected read failure".into())
            ),
            Err(InitialSetupSnapshotError::Read(_))
        ));
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
    }

    #[test]
    fn permission_policy_waits_for_the_final_admission_commit_including_empty_choices() {
        for plan in [selected(&[Setting::Permissions]), selected(&[])] {
            let root = aterm_tempfile::tempdir().unwrap();
            assert!(
                authorize_with(root.path(), &plan, AdmittedVault::None, |_, _| {
                    Err("injected final admission failure".into())
                })
                .is_err()
            );
            let staged = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(!staged.permissions_allowed);
            assert!(!staged.finished);
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), &mut None)
                    .unwrap()
                    .is_none()
            );
            assert!(host.calls.is_empty());

            assert!(authorize_at(root.path(), &plan).unwrap().is_some());
            let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(
                admitted.permissions_allowed,
                plan.selection.contains(Setting::Permissions)
            );
            assert_eq!(admitted.finished, plan.selection.is_empty());
            assert!(admitted.revision > staged.revision);
            execute_initial(&mut host, root.path(), &mut None).unwrap();
            let completed = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(completed.finished);
            assert!(
                authorize_at(root.path(), &Plan::default())
                    .unwrap()
                    .is_none()
            );
            assert_eq!(revision_at(root.path()).unwrap(), completed.revision);
        }
    }

    #[test]
    fn permission_policy_cannot_reuse_old_admission_during_a_plan_restart() {
        let root = aterm_tempfile::tempdir().unwrap();
        let old = selected(&[Setting::Widgets]);
        authorize_at(root.path(), &old).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let next = selected(&[Setting::Widgets, Setting::Permissions]);

        // The first publication still has the old, admitted progress. Its plan
        // binding must hold permissions until the reset and admission complete.
        save_plan(root.path(), &next).unwrap();
        let published = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!published.permissions_allowed);
        assert_eq!(published.revision, 0);
        assert!(
            restart_with(root.path(), &next, AdmittedVault::None, |_, _| {
                Err("injected restart admission failure".into())
            })
            .is_err()
        );
        let uncommitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!uncommitted.permissions_allowed);
        assert!(!uncommitted.finished);
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());

        restart_at(root.path(), &next).unwrap();
        let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(admitted.permissions_allowed);
        assert!(admitted.revision > uncommitted.revision);
    }

    #[test]
    fn permission_policy_migrates_completed_history_but_pending_history_needs_authorization() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Permissions]);
        authorize_at(root.path(), &plan).unwrap();
        let path = root.path().join("initial-setup.json");
        let remove_admission = || {
            let bytes = std::fs::read(&path).unwrap();
            let mut record: Value = aterm_json::from_slice(&bytes).unwrap();
            record.as_object_mut().unwrap().remove("admitted");
            write_json(&path, &record).unwrap();
        };
        remove_admission();
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(authorize_at(root.path(), &plan).unwrap().is_some());
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        remove_admission();
        let completed = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(completed.permissions_allowed);
        assert!(completed.finished);
    }

    #[test]
    fn unified_setup_orders_sync_before_repositories_and_never_persists_the_password() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan::default();
        authorize_at(root.path(), &plan).unwrap();
        let _profile = HeldLock::take(root.path()).unwrap();
        let password = SecretBytes::from(b"test-only-join-password".as_slice());
        let mut host = Fake {
            observe: Some(root.path().into()),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut Some(password.clone()))
            .unwrap()
            .unwrap();
        assert!(!report.needs_attention());
        assert_eq!(host.calls, Stage::ALL);
        assert!(host.password_seen);
        assert!(finished_at(root.path()).unwrap());
        for path in [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ] {
            let bytes = std::fs::read(root.path().join(path)).unwrap();
            assert!(
                !bytes
                    .windows(password.len())
                    .any(|window| window == password.as_slice())
            );
        }
    }

    #[test]
    fn pending_vault_blocks_only_repositories_and_retries_without_repeating_finished_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .unwrap()
                .needs_attention()
        );
        assert_eq!(host.calls, Stage::ALL[..4]);
        assert!(first_run_complete(root.path()).unwrap());
        assert!(!finished_at(root.path()).unwrap());
        // Another launch must still be able to claim this unfinished plan.
        assert!(claim_automatic_setup_at(root.path()).unwrap().is_some());
        host.calls.clear();
        host.pending = None;
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_later_incomplete_vault_blocks_repository_retries_and_reopens_only_its_stage() {
        for readiness_error in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &Plan::default()).unwrap();
            let mut host = Fake {
                failure: Some(Stage::Repositories),
                ..Fake::default()
            };
            execute_initial(&mut host, root.path(), &mut None).unwrap();
            assert_eq!(host.calls, Stage::ALL);
            assert_eq!(host.vault_checks, 1);
            let completed = progress(root.path()).unwrap().unwrap().done;
            assert_ne!(completed & Stage::Secrets.bit(), 0);
            assert_eq!(completed & Stage::Repositories.bit(), 0);
            let mut interrupted = progress(root.path()).unwrap().unwrap();
            interrupted.running = Some(Stage::Repositories);
            save_progress(root.path(), interrupted).unwrap();

            // A background sync now has incomplete files (or unreadable state).
            // No repository configuration, credential or clone operation may run.
            host.calls.clear();
            host.failure = None;
            host.vault_blocked = !readiness_error;
            host.vault_error = readiness_error;
            let report = execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .unwrap();
            assert!(host.calls.is_empty());
            assert_eq!(host.vault_checks, 2);
            assert!(report.needs_attention());
            assert_eq!(
                progress(root.path()).unwrap().unwrap().done,
                completed & !Stage::Secrets.bit()
            );
            assert_eq!(progress(root.path()).unwrap().unwrap().running, None);
            assert!(!finished_at(root.path()).unwrap());

            // The next pass repairs the vault before its fresh admission check;
            // preferences, Spotlight and permissions retain their prior history.
            host.vault_blocked = false;
            host.vault_error = false;
            execute_initial(&mut host, root.path(), &mut None).unwrap();
            assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
            assert_eq!(host.vault_checks, 3);
            assert!(finished_at(root.path()).unwrap());
        }
    }

    #[test]
    fn subset_and_empty_plans_never_visit_unselected_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Repositories]);
        authorize_at(root.path(), &plan).unwrap();
        assert_eq!(saved_plan(root.path()).unwrap(), Some(plan));
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Repositories]);
        assert!(!host.password_seen);
        assert_eq!(host.vault_checks, 0);

        let empty = aterm_tempfile::tempdir().unwrap();
        authorize_at(empty.path(), &Plan::from(Selection::none())).unwrap();
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, empty.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(finished_at(empty.path()).unwrap());
    }

    /// The worker's stages over the doctor of a widgets row whose Notification Center key
    /// waits for Full Disk Access ([`super::super::tests::Withholding`]): its Workstation
    /// stage is that doctor's automatic pass, and its follow-up after the stages that
    /// doctor's; every other stage is [`Fake`]'s. With `grant_in_permissions`, macOS gives
    /// aterm the grant while the Permissions stage runs, as its hand-over brings it.
    struct WithholdingStages {
        doctor: super::super::tests::Withholding,
        rest: Fake,
        grant_in_permissions: bool,
    }

    impl WithholdingStages {
        fn over(doctor: super::super::tests::Withholding) -> Self {
            Self {
                doctor,
                rest: Fake::default(),
                grant_in_permissions: false,
            }
        }
    }

    impl InitialHost for WithholdingStages {
        fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.doctor.pass(root)
        }
        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            self.rest.spotlight()
        }
        fn permissions(&mut self) -> Result<Vec<Row>, String> {
            if self.grant_in_permissions {
                self.doctor.grant();
            }
            self.rest.permissions()
        }
        fn secrets(
            &mut self,
            plan: &Plan,
            replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            self.rest.secrets(plan, replaces, password)
        }
        fn secrets_pace(&mut self) -> Option<Pace> {
            self.rest.secrets_pace()
        }
        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            self.rest.configured_vault()
        }
        fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
            self.rest.current_vault(plan)
        }
        fn repositories(&mut self) -> Result<Vec<Row>, String> {
            self.rest.repositories()
        }
        fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.doctor.follow_up(root)
        }
    }

    /// The real item boundary through the worker's stage loop: the widgets row is one item of
    /// the Window Manager's key and Notification Center's list, the Dock never verifies, and
    /// the Mac has no Full Disk Access. The first retry hides the desktop widgets and leaves
    /// Notification Center alone; the person shows their desktop widgets again; thirty more
    /// retries, the stage still pending, write no Window Manager key and relaunch the Dock
    /// only for the Dock's own second try. The grant then arrives while the stage is still
    /// pending: the follow-up after that retry's stage pass clears Notification Center and
    /// relaunches it once, and writes no Window Manager key and relaunches no Dock; no later
    /// retry touches either again. Negative controls: a Mac that never grants it keeps
    /// Notification Center as it is; following up only once the stage is done (the old gate
    /// in `execute_initial`) never clears it while the stage is pending.
    #[test]
    fn the_grant_that_arrives_while_the_stage_is_pending_clears_notification_center_once() {
        for granted in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Dock]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(
                super::super::tests::Withholding::widgets_and_a_dock_that_never_verifies(),
            );
            let retry = |stages: &mut WithholdingStages| {
                execute_initial_for_revision(stages, root.path(), revision, &mut None)
                    .unwrap()
                    .expect("setup still runs");
            };
            retry(&mut stages);
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert_eq!(stages.doctor.notification_center_writes(), 0);
            let docks = stages.doctor.dock_relaunches();
            stages.doctor.show_desktop_widgets();
            for _ in 0..30 {
                retry(&mut stages);
            }
            assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert!(stages.doctor.desktop_widgets_shown());
            assert_eq!(stages.doctor.notification_center_writes(), 0);
            assert_eq!(
                stages.doctor.dock_relaunches(),
                docks + 2,
                "the Dock's second try: its relaunch and its rollback's"
            );
            let docks = stages.doctor.dock_relaunches();
            if granted {
                stages.doctor.grant();
            }
            for _ in 0..30 {
                retry(&mut stages);
            }
            assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
            assert_eq!(stages.doctor.notification_center_cleared(), granted);
            assert_eq!(
                stages.doctor.notification_center_writes(),
                usize::from(granted)
            );
            assert_eq!(
                stages.doctor.notification_center_relaunches(),
                usize::from(granted)
            );
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert!(stages.doctor.desktop_widgets_shown());
            assert_eq!(stages.doctor.dock_relaunches(), docks);
            assert_eq!(
                super::super::tests::Withholding::withheld(root.path()).is_empty(),
                granted
            );
        }
    }

    /// The widgets row a retry reports never says aterm set Notification Center's widget list
    /// and the person changed it since (aterm never set it, and nobody changed it), when the
    /// grant arrives while the stage is pending (the Dock never verifies), and each retry
    /// adds at most one follow-up row. The first retry with the grant holds the row as it is
    /// and says the list is the follow-up's ([`SETTLED_OWED`]), beside the follow-up's own
    /// row after its stages. When the clear takes, later retries find the list cleared and
    /// their rows name it no more, nor run the follow-up; when Notification Center never
    /// keeps it (two tries, ten minutes apart, then held), every retry says so again, beside
    /// one follow-up row. Negative controls: holding the row with [`SETTLED`] alone
    /// (`Automatic::holds` ignoring what is owed) fails both arms; a follow-up called twice
    /// in a retry adds a second follow-up row.
    #[test]
    fn the_widgets_row_never_says_aterm_set_the_list_the_follow_up_still_owes() {
        use super::super::tests::{WIDGETS, Withholding, nc_widgets};
        let list = nc_widgets().describe();
        let owed = format!("{list}; left as it is: {SETTLED_OWED}");
        for keeps in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Dock]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages =
                WithholdingStages::over(Withholding::widgets_and_a_dock_that_never_verifies());
            if !keeps {
                stages.doctor.notification_center_never_keeps();
            }
            let retry = |stages: &mut WithholdingStages| {
                execute_initial_for_revision(stages, root.path(), revision, &mut None)
                    .unwrap()
                    .expect("setup still runs")
            };
            retry(&mut stages);
            stages.doctor.grant();
            for at in 0..15 {
                let report = retry(&mut stages);
                assert!(
                    !report.rows.iter().any(|row| row.detail.contains(SETTLED)),
                    "{keeps} {at}: {}",
                    report.to_text()
                );
                let followed_up = !keeps || at == 0;
                let named = report
                    .rows
                    .iter()
                    .any(|row| row.name == WIDGETS && row.status == "ok" && row.detail == owed);
                assert_eq!(named, followed_up, "{keeps} {at}: {}", report.to_text());
                assert_eq!(
                    report.rows.iter().filter(|row| row.name == WIDGETS).count(),
                    1 + usize::from(followed_up),
                    "the Workstation stage's row, and the follow-up's: {keeps} {at}: {}",
                    report.to_text()
                );
            }
            assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert_eq!(stages.doctor.notification_center_cleared(), keeps);
            assert_eq!(
                stages.doctor.notification_center_writes(),
                if keeps { 1 } else { 4 },
                "a clear that takes once; else two tries and their rollbacks"
            );
            assert_eq!(Withholding::withheld(root.path()).is_empty(), keeps);
        }
    }

    /// The worker's stages over a widgets row whose Notification Center key waits for Full
    /// Disk Access, with the aterm permissions stage waiting for the same grant: the
    /// Workstation stage ends without the key, and no retry touches it while the grant is
    /// missing. The grant then arrives during a retry's Permissions stage, after its
    /// Workstation stage is long done: the follow-up after that retry's stages clears
    /// Notification Center's widgets, once, as setup completes, and nothing runs after it.
    /// Negative controls: a Mac that never grants it finishes setup with the key untouched;
    /// no follow-up after the stages (one before them alone, or none) finishes setup with
    /// Notification Center uncleared, and nothing clears it after.
    #[test]
    fn the_grant_that_arrives_during_the_permissions_stage_is_followed_up_as_setup_completes() {
        for granted in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Permissions]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages {
                rest: Fake {
                    pending: Some(Stage::Permissions),
                    ..Fake::default()
                },
                ..WithholdingStages::over(super::super::tests::Withholding::widgets())
            };
            for _ in 0..3 {
                execute_initial_for_revision(&mut stages, root.path(), revision, &mut None)
                    .unwrap();
            }
            assert_eq!(stages.doctor.notification_center_writes(), 0);
            assert!(!finished_at(root.path()).unwrap());
            stages.grant_in_permissions = granted;
            stages.rest.pending = None;
            execute_initial_for_revision(&mut stages, root.path(), revision, &mut None)
                .unwrap()
                .expect("the retry runs");
            assert!(finished_at(root.path()).unwrap());
            assert_eq!(stages.doctor.notification_center_cleared(), granted);
            assert_eq!(
                stages.doctor.notification_center_writes(),
                usize::from(granted)
            );
            assert_eq!(
                super::super::tests::Withholding::withheld(root.path()).is_empty(),
                granted
            );
            for _ in 0..2 {
                assert!(
                    execute_initial_for_revision(&mut stages, root.path(), revision, &mut None)
                        .unwrap()
                        .is_none()
                );
            }
            assert_eq!(
                stages.doctor.notification_center_writes(),
                usize::from(granted)
            );
        }
    }

    /// The vault's remedy, Apply with the vault row on and every Mac settings row off, runs no
    /// Mac settings and withdraws nothing setup still owes: once the grant arrives, that run's
    /// follow-up clears Notification Center's widgets. A choice that still runs Mac settings
    /// without the widgets row withdraws them. Negative control: reading any choice without
    /// the row as a withdrawal leaves them uncleared after the remedy.
    #[test]
    fn the_vault_remedy_withdraws_no_clear_setup_owes() {
        use super::super::tests::{WIDGETS, Withholding};
        for (what, remedy, cleared) in [
            ("the vault alone", selected(&[Setting::Secrets]), true),
            (
                "the Dock and the vault",
                selected(&[Setting::Dock, Setting::Secrets]),
                false,
            ),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Secrets]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(Withholding::widgets());
            execute_initial_for_revision(&mut stages, root.path(), revision, &mut None).unwrap();
            assert_eq!(Withholding::withheld(root.path()), [WIDGETS], "{what}");
            let revision = restart_at(root.path(), &remedy).unwrap();
            stages.doctor.grant();
            execute_initial_for_revision(&mut stages, root.path(), revision, &mut None).unwrap();
            assert_eq!(
                stages.doctor.notification_center_cleared(),
                cleared,
                "{what}"
            );
            assert!(Withholding::withheld(root.path()).is_empty(), "{what}");
        }
    }

    /// The follow-up's tries belong to the setup run that spent them, as the run's own
    /// record's do: a run the startup review admits after explicit passes spent them, or one
    /// System State's Apply restarts, starts them afresh and keeps what the record owes,
    /// while the same run keeps them. Negative control: reading the tries whatever run spent
    /// them holds the new run's first try back.
    #[test]
    fn a_new_setup_run_starts_the_follow_ups_tries_afresh() {
        use super::super::{load_withheld, record_withheld};
        let root = aterm_tempfile::tempdir().unwrap();
        let widgets = super::super::tests::WIDGETS.to_owned();
        let spend = |root: &Path| {
            record_withheld(root, std::slice::from_ref(&widgets), &[], &[], ("b", 0)).unwrap();
            for at in [0, 600] {
                record_withheld(root, &[], &[], std::slice::from_ref(&widgets), ("b", at)).unwrap();
            }
            let owed = load_withheld(root).unwrap();
            assert_eq!(owed.failed.len(), 1, "{:?}", owed.failed);
            assert_eq!(owed.failed[0].tries, 2);
        };
        // Explicit passes before any run was admitted, then the startup review's answer.
        spend(root.path());
        let plan = selected(&[Setting::Widgets]);
        authorize_at(root.path(), &plan).unwrap().unwrap();
        let owed = load_withheld(root.path()).unwrap();
        assert_eq!(owed.items, std::slice::from_ref(&widgets));
        assert!(owed.failed.is_empty(), "{:?}", owed.failed);
        // Spent in this run, they stay spent until Apply restarts it.
        spend(root.path());
        restart_at(root.path(), &plan).unwrap();
        let owed = load_withheld(root.path()).unwrap();
        assert_eq!(owed.items, [widgets]);
        assert!(owed.failed.is_empty(), "{:?}", owed.failed);
    }

    /// One budget of tries for Notification Center's clear, whichever pass makes each, on the
    /// real item boundary through the worker's stage loop: the widgets row's first try,
    /// without the grant, does not verify (its Window Manager key's reader is slow), and once
    /// the grant arrives Notification Center never keeps a clear. The follow-up makes the
    /// first try; the setup pass, whose own wait ends first, waits for the follow-up's too and
    /// makes the second; then neither tries again in that build. So the clear is tried exactly
    /// twice, never twice within [`SECOND_TRY_AFTER`], and no retry makes two tries. Negative
    /// control: holding the setup pass by its own record's tries alone makes the second try
    /// less than ten minutes after the first.
    #[test]
    fn notification_centers_clear_is_tried_twice_per_build_whichever_pass_makes_each_try() {
        use super::super::tests::Withholding;
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages =
            WithholdingStages::over(Withholding::widgets_whose_first_try_does_not_verify());
        let retry = |stages: &mut WithholdingStages| {
            execute_initial_for_revision(stages, root.path(), revision, &mut None)
                .unwrap()
                .expect("setup still runs");
        };
        retry(&mut stages);
        assert_eq!(stages.doctor.notification_center_writes(), 0);
        assert_eq!(
            Withholding::withheld(root.path()),
            [super::super::tests::WIDGETS]
        );
        stages.doctor.grant();
        stages.doctor.notification_center_never_keeps();
        let mut tries = Vec::new();
        for _ in 0..40 {
            let writes = stages.doctor.notification_center_writes();
            retry(&mut stages);
            match stages.doctor.notification_center_writes() - writes {
                0 => {}
                2 => tries.push(stages.doctor.now()),
                more => panic!("a try and its rollback in one retry, not {more} writes"),
            }
        }
        assert!(!finished_at(root.path()).unwrap(), "the stage is held");
        assert_eq!(tries.len(), 2, "{tries:?}");
        let apart = tries[1].duration_since(tries[0]).unwrap();
        assert!(apart >= SECOND_TRY_AFTER, "{apart:?}");
    }

    /// System State's Apply, which the held row names, starts the clear's tries afresh: once
    /// both tries of a build are spent, a restart of the same plan keeps Notification Center's
    /// list owed and its first retry tries the clear again, with a budget of two as the first
    /// run had. Negative control: a restart that leaves the follow-up record's tries in place
    /// holds the whole widgets row (and the follow-up) HELD in every later run of the build.
    #[test]
    fn apply_in_system_state_tries_a_held_clear_again() {
        use super::super::tests::Withholding;
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets]);
        let mut revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages =
            WithholdingStages::over(Withholding::widgets_whose_first_try_does_not_verify());
        let retry = |stages: &mut WithholdingStages, revision: u64| {
            let writes = stages.doctor.notification_center_writes();
            execute_initial_for_revision(stages, root.path(), revision, &mut None)
                .unwrap()
                .expect("setup still runs");
            stages.doctor.notification_center_writes() - writes
        };
        retry(&mut stages, revision);
        stages.doctor.grant();
        stages.doctor.notification_center_never_keeps();
        for run in 0..2 {
            let tried: usize = (0..40).map(|_| retry(&mut stages, revision)).sum();
            assert_eq!(tried, 4, "run {run}: two tries, each with its rollback");
            assert_eq!(
                Withholding::withheld(root.path()),
                [super::super::tests::WIDGETS],
                "run {run}: still owed"
            );
            revision = restart_at(root.path(), &plan).unwrap();
            assert_eq!(
                Withholding::withheld(root.path()),
                [super::super::tests::WIDGETS],
                "run {run}: the restart keeps it owed"
            );
        }
    }

    /// THE VAULT LINES' REMEDY TOUCHES THE VAULT AND NOTHING THE PERSON KEEPS. After the
    /// default setup finished, System State's Apply with the vault row on and the Mac
    /// settings rows off, the other rows as they were (what the vault's digest lines ask),
    /// runs no Mac settings pass (no Dock or sign-out), keeps first-run complete, and keeps
    /// aterm's permission asks allowed, since the permissions row stays on. The Secrets row
    /// alone would end those asks on every later launch, and every row on runs the Mac
    /// settings again.
    #[test]
    fn the_vault_lines_apply_leaves_the_mac_settings_and_the_permission_asks_alone() {
        let root = aterm_tempfile::tempdir().unwrap();
        let allowed = || {
            snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        };
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert!(allowed());

        let mut remedy = Plan::default();
        for setting in Setting::ALL {
            if setting.is_workstation() {
                remedy.selection.set(setting, false);
            }
        }
        remedy.selection.set(Setting::Secrets, true);
        let revision = restart_at(root.path(), &remedy).unwrap();
        assert!(first_run_complete(root.path()).unwrap());
        assert!(allowed());
        let mut host = Fake::default();
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        assert!(host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "{:?}",
            host.calls
        );
        assert!(finished_at(root.path()).unwrap());
        assert!(first_run_complete(root.path()).unwrap());
        assert!(allowed(), "the permission asks stay allowed");

        restart_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        assert!(!allowed(), "the Secrets row alone ends them");
        let revision = restart_at(root.path(), &Plan::default()).unwrap();
        assert!(!first_run_complete(root.path()).unwrap());
        let mut host = Fake::default();
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        assert!(host.calls.contains(&Stage::Workstation), "{:?}", host.calls);
    }

    /// The worker's stage loop over a Mac setting macOS never keeps: every retry runs
    /// the Workstation stage again through the worker's own entry, and it stays pending,
    /// but only two retries ten minutes apart make the change, relaunch the Dock and put
    /// the change back. Every other retry writes and relaunches nothing while the other
    /// stages go on retrying. Negative control: an automatic pass that holds nothing
    /// (`host.hold(&[])` in `automatic_pass`) relaunches the Dock on every retry, as the
    /// worker did every minute on 2026-10-22.
    #[test]
    fn a_workstation_stage_that_never_verifies_disrupts_twice_across_worker_retries() {
        struct Stages {
            doctor: super::super::tests::NeverVerifies,
            rest: Fake,
        }
        impl InitialHost for Stages {
            fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.doctor.pass(root)
            }
            fn spotlight(&mut self) -> Result<Vec<Row>, String> {
                self.rest.spotlight()
            }
            fn permissions(&mut self) -> Result<Vec<Row>, String> {
                self.rest.permissions()
            }
            fn secrets(
                &mut self,
                plan: &Plan,
                replaces: Replaces,
                password: &mut Option<SecretBytes>,
            ) -> Result<Vec<Row>, String> {
                self.rest.secrets(plan, replaces, password)
            }
            fn secrets_pace(&mut self) -> Option<Pace> {
                self.rest.secrets_pace()
            }
            fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
                self.rest.configured_vault()
            }
            fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
                self.rest.current_vault(plan)
            }
            fn repositories(&mut self) -> Result<Vec<Row>, String> {
                self.rest.repositories()
            }
            fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.rest.withheld(root)
            }
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Dock, Setting::Widgets, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = Stages {
            doctor: super::super::tests::NeverVerifies::dock(),
            rest: Fake {
                pending: Some(Stage::Secrets),
                ..Fake::default()
            },
        };
        // Each retry is a minute after the last; `why` is what the held Dock row says.
        let retries = |stages: &mut Stages, count: usize, why: &str| {
            for retry in 0..count {
                let report = execute_initial_for_revision(stages, root.path(), revision, &mut None)
                    .unwrap()
                    .unwrap();
                assert!(report.needs_attention(), "retry {retry}");
                assert!(
                    report
                        .rows
                        .iter()
                        .any(|row| row.status == "blocked" && row.detail == why),
                    "retry {retry}: {}",
                    report.to_text()
                );
            }
        };
        let first = execute_initial_for_revision(&mut stages, root.path(), revision, &mut None)
            .unwrap()
            .unwrap();
        assert!(first.needs_attention());
        let once = (stages.doctor.changes(), stages.doctor.relaunches());
        assert_eq!(once.1, 1);
        retries(&mut stages, 9, WAITING);
        assert_eq!(
            (stages.doctor.changes(), stages.doctor.relaunches()),
            once,
            "nothing between the tries"
        );
        // The first retry ten minutes after the first try is the second; after it, none.
        execute_initial_for_revision(&mut stages, root.path(), revision, &mut None).unwrap();
        let twice = (stages.doctor.changes(), stages.doctor.relaunches());
        assert_eq!(twice.1, 2);
        assert!(twice.0 > once.0);
        retries(&mut stages, 20, HELD);
        assert_eq!((stages.doctor.changes(), stages.doctor.relaunches()), twice);
        assert_eq!(
            stages
                .rest
                .calls
                .iter()
                .filter(|stage| **stage == Stage::Secrets)
                .count(),
            31,
            "the pending vault keeps retrying"
        );
        assert!(!finished_at(root.path()).unwrap());
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
            0,
            "a held setting keeps its stage open for the person"
        );
    }

    /// A rerun of the plan (System State's Apply) starts its automatic attempts afresh,
    /// so a held-items record the worker cannot read, which fails every automatic pass,
    /// does not outlive the person's rerun. Negative control: leaving [`HELD_RECORD`]
    /// out of `stage_restart`'s resets keeps the unreadable record, and the rerun's
    /// Workstation stage fails.
    #[test]
    fn a_rerun_retires_an_unreadable_held_record() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Dock, Setting::Widgets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        std::fs::write(root.path().join(HELD_RECORD), b"not json").unwrap();
        let mut doctor = super::super::tests::NeverVerifies::dock();
        assert!(
            doctor.pass(root.path()).is_err(),
            "an unreadable record holds the pass"
        );
        let rerun = restart_at(root.path(), &plan).unwrap();
        assert_ne!(rerun, revision);
        assert!(!root.path().join(HELD_RECORD).exists());
        let report = doctor
            .pass(root.path())
            .expect("the rerun's pass runs")
            .expect("its setup is pending");
        assert!(report.needs_attention(), "{}", report.to_text());
        assert_eq!(doctor.relaunches(), 1, "it tried the Dock once");
    }

    #[test]
    fn failed_stages_remain_pending_and_cancel_or_restore_stops_every_stage() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[
            Setting::Permissions,
            Setting::Secrets,
            Setting::Repositories,
        ]);
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake {
            failure: Some(Stage::Secrets),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(report.needs_attention());
        assert_eq!(host.calls, [Stage::Permissions, Stage::Secrets]);
        assert!(!finished_at(root.path()).unwrap());
        // Mac completion alone cannot prevent a later Cancel from persisting.
        assert!(first_run_complete(root.path()).unwrap());
        cancel_automatic_setup_at(root.path()).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            before
        );
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn explicit_mac_apply_retains_modern_stages_and_legacy_completion_stays_honored() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Repositories]);
        plan.vault_url = "https://vault.example/v1/team".into();
        authorize_at(root.path(), &plan).unwrap();
        save_explicit_workstation_plan(root.path()).unwrap();
        let revised = saved_plan(root.path()).unwrap().unwrap();
        assert_eq!(
            revised.selection.workstation(),
            Selection::all().workstation()
        );
        assert!(revised.selection.contains(Setting::Secrets));
        assert!(revised.selection.contains(Setting::Repositories));
        assert!(!revised.selection.contains(Setting::Permissions));
        assert_eq!(revised.vault_url, plan.vault_url);

        let legacy = aterm_tempfile::tempdir().unwrap();
        for state in ["cancelled", "restored", "applied"] {
            write_json(
                &legacy.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state,"accounts_verified":true}),
            )
            .unwrap();
            authorize_at(legacy.path(), &Plan::default()).unwrap();
            assert!(finished_at(legacy.path()).unwrap());
            assert!(!legacy.path().join("initial-setup.json").exists());
        }
    }

    #[test]
    fn restore_stops_completed_workstation_reporting_without_undoing_other_stage_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert!(rows.iter().all(|row| row.status == "applied"));
        let history = std::fs::read(root.path().join("initial-setup.json")).unwrap();

        // This is also the durable state when Restore's preference writes fail:
        // the stop marker precedes those writes and says nothing about success.
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert_eq!(rows.len(), Stage::ALL.len());
        for (stage, row) in Stage::ALL.iter().zip(rows) {
            assert_eq!(
                row.status,
                if *stage == Stage::Workstation {
                    "stopped"
                } else {
                    "applied"
                }
            );
        }
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            history
        );
    }

    fn restore_preflight_failure_stops_every_stage(record: &str) {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[
            Setting::Widgets,
            Setting::Permissions,
            Setting::Secrets,
            Setting::Repositories,
        ]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let history = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        let invalid = root.path().join(record);
        std::fs::write(&invalid, b"invalid recovery record").unwrap();
        assert!(super::super::tests::restore_without_effects(root.path()).is_err());

        // A fresh reader after process exit must retain the stop even though
        // the recovery record prevented Restore from changing preferences.
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.finished, "Restore did not stop the durable run");
        assert!(!snapshot.permissions_allowed);
        assert_eq!(snapshot.plan, plan);
        assert_eq!(snapshot.revision, revision);
        assert!(snapshot.rows.iter().all(|row| row.status == "stopped"));
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            history
        );
        assert_eq!(std::fs::read(&invalid).unwrap(), b"invalid recovery record");
        let mut automatic = Fake::default();
        assert!(
            execute_initial(&mut automatic, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(automatic.calls.is_empty());
        assert!(authorize_at(root.path(), &plan).unwrap().is_none());
    }

    #[test]
    fn restore_stops_automatic_setup_before_reading_a_corrupt_backup() {
        restore_preflight_failure_stops_every_stage("original-preferences.json");
    }

    #[test]
    fn restore_stops_automatic_setup_before_reading_corrupt_activation() {
        restore_preflight_failure_stops_every_stage("pending-refresh.json");
    }

    #[test]
    fn restore_stop_write_failure_refuses_recovery_preflight_and_effects() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(
            root.path(),
            &selected(&[Setting::Widgets, Setting::Secrets]),
        )
        .unwrap();
        let backup = root.path().join("original-preferences.json");
        save_backup(
            &backup,
            &[Saved {
                target: Target::new("com.apple.dock", "show-recents"),
                value: Some(PValue::Bool(true)),
                granted: true,
            }],
        )
        .unwrap();
        let before = std::fs::read(&backup).unwrap();
        let decision = root.path().join("first-run.json");
        std::fs::remove_file(&decision).unwrap();
        std::fs::create_dir(&decision).unwrap();
        assert!(super::super::tests::restore_without_effects(root.path()).is_err());
        assert!(
            !root.path().join("pending-refresh.json").exists(),
            "a refused stop must precede recovery preparation"
        );
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        let mut automatic = Fake::default();
        assert!(execute_initial(&mut automatic, root.path(), &mut None).is_err());
        assert!(automatic.calls.is_empty());
    }

    #[test]
    fn cancel_keeps_applied_preferences_and_stops_only_unfinished_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        cancel_automatic_setup_at(root.path()).unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, "applied");
        assert_eq!(rows[1].status, "stopped");
    }

    #[test]
    fn failed_explicit_preflight_cannot_accept_the_earlier_subset_completion() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert!(first_run_complete(root.path()).unwrap());
        let revision = revision_at(root.path()).unwrap();

        save_explicit_workstation_plan(root.path()).unwrap();
        // Inject the error a preference preflight can return after the explicit
        // plan was staged, before it reaches its own pending-marker write.
        host.calls.clear();
        host.failure = Some(Stage::Workstation);
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(report.needs_attention());
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert!(!first_run_complete(root.path()).unwrap());
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
            0
        );
        assert_eq!(revision_at(root.path()).unwrap(), revision);

        host.calls.clear();
        host.failure = None;
        host.pending = None;
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn an_explicit_restart_reopens_stopped_stages_and_preserves_the_original_backup() {
        let root = aterm_tempfile::tempdir().unwrap();
        let original = root.path().join("original-preferences.json");
        std::fs::write(&original, b"the original preference snapshot").unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"cancelled"}),
        )
        .unwrap();
        write_json(
            &root.path().join("automatic-attempt.json"),
            &json!({"profile":PROFILE,"accessibility_ready":false}),
        )
        .unwrap();
        let plan = selected(&[Setting::Spotlight, Setting::Secrets]);
        restart_at(root.path(), &plan).unwrap();
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"the original preference snapshot"
        );
        assert!(!root.path().join("automatic-attempt.json").exists());
        let (saved, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert_eq!(saved, plan);
        assert!(!finished);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.status == "pending"));
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert!(rows.iter().all(|row| row.status == "applied"));
        assert_eq!(host.calls, [Stage::Spotlight, Stage::Secrets]);
    }

    /// The vault at `url` (an http(s) URL ending in its name) configured under the scratch
    /// `paths`, as a join writes it, read back.
    fn configure_vault(paths: &aterm_secrets::client::Paths, url: &str) -> ConfiguredVault {
        use std::os::unix::fs::PermissionsExt;
        let name = url.rsplit('/').next().unwrap();
        std::fs::create_dir_all(&paths.dir).unwrap();
        std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = paths.dir.join("source.toml");
        std::fs::write(
            &source,
            format!(
                "url = \"{url}\"\nname = \"{name}\"\nsigner = \"{}\"\nhost = \"m99\"\n",
                aterm_secrets::b64::encode_padded(&[7; 32])
            ),
        )
        .unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        ConfiguredVault::read(paths).unwrap().expect("configured")
    }

    /// THE OFFERED PLAN IS IN THE VAULT THIS MAC IS IN. A finished run saved the ALab
    /// vault; this Mac joined another since (from a terminal). System State's snapshot and
    /// the startup review offer the configured source, with the saved plan's rows and
    /// revision; an Apply left as offered saves it, and its Secrets stage resumes that vault
    /// where the saved source would have joined ALab. A saved source naming the configured
    /// vault stays as typed (its pin with it), and a modern plan stays as saved when the
    /// configuration cannot be read.
    #[test]
    fn an_unedited_apply_resumes_the_configured_vault_instead_of_the_saved_one() {
        use aterm_secrets::client::{Membership, Paths, Status};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        let alab = aterm_types::macos_setup::DEFAULT_VAULT_URL;
        let team = "http://vault.example:7484/v1/team";
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(ConfiguredVault::read(&paths).unwrap(), None);
        let unconfigured = snapshot_in_vault(root.path(), Ok(None)).unwrap();
        assert_eq!(unconfigured.plan, Plan::default());

        let vault = configure_vault(&paths, team);
        let configured = Some(vault.clone());
        let snapshot = snapshot_in_vault(root.path(), Ok(configured.clone())).unwrap();
        assert_eq!(snapshot.plan.vault_url, team);
        assert_eq!(snapshot.plan.selection, Plan::default().selection);
        assert_eq!(
            (
                snapshot.finished,
                snapshot.revision,
                format!("{:?}", snapshot.rows)
            ),
            (
                unconfigured.finished,
                unconfigured.revision,
                format!("{:?}", unconfigured.rows)
            ),
            "the rows read the saved plan's progress"
        );
        assert!(snapshot.rows.iter().all(|row| row.status == "applied"));
        let saved = || saved_plan_record(root.path()).unwrap();
        assert_eq!(
            offered_plan(saved(), Ok(configured.clone()))
                .unwrap()
                .vault_url,
            team
        );
        assert_eq!(
            offered_plan(saved(), Err("unreadable source.toml".into())).unwrap(),
            Plan::default()
        );
        assert!(offered_plan(None, Err("unreadable source.toml".into())).is_err());

        let revision = restart_at(root.path(), &snapshot.plan).unwrap();
        let mut host = Fake::default();
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        let ran = host.vault_url.expect("the Secrets stage ran");
        assert_eq!(ran, team);
        let status = Status {
            config: aterm_secrets::client::config(&paths).unwrap(),
            machine: None,
            membership: Membership::In { seq: 3, heard: 3 },
            names: Vec::new(),
        };
        // The rule the stage's vault item joins by: the configured vault is read as it is,
        // and any other source is joined.
        let reads = |url: &str| {
            aterm_machines::init_secrets::selected_vault(url, home.path(), status.config.as_ref())
                .unwrap()
                .is_some()
        };
        assert!(reads(&ran), "resumes {ran}");
        assert!(!reads(alab), "joins {alab}");

        let pinned = Plan {
            vault_url: format!("{team}#{}", aterm_secrets::b64::encode_padded(&[7; 32])),
            ..Plan::default()
        };
        assert_eq!(vault.present(pinned.clone()), pinned);
        let file = ConfiguredVault {
            config: aterm_secrets::client::Config {
                source: aterm_secrets::transport::Source::Path(home.path().join("team.vault")),
                name: "team".into(),
                signer: None,
                followed: Vec::new(),
                host: "m99".into(),
            },
            home: home.path().to_path_buf(),
        };
        let typed = Plan {
            vault_url: "~/team.vault".into(),
            ..Plan::default()
        };
        assert_eq!(file.present(typed.clone()), typed);
        assert_eq!(
            file.present(Plan::default()).vault_url,
            home.path().join("team.vault").to_string_lossy()
        );
    }

    /// A VAULT TYPED IN SYSTEM STATE STAYS THERE WHILE SETUP STILL JOINS IT. This Mac is in
    /// one vault, and an Apply asked for another whose join failed. The run's retries join
    /// the typed vault, so every status read keeps it in the field beside its error, never
    /// the vault this Mac is in, and an Apply left as shown asks for it again. Once the run
    /// stops (Cancel), nothing joins it, and the snapshot offers the vault this Mac is in.
    #[test]
    fn a_vault_typed_in_system_state_stays_while_setup_still_joins_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let team = "http://vault.example:7484/v1/team";
        let other = "http://other.example:7484/v1/other";
        let configured = Some(configure_vault(&paths, team));
        let shown = || snapshot_in_vault(root.path(), Ok(configured.clone())).unwrap();
        // Each Apply is made in the vault this Mac is in, its vault typed when the field no
        // longer shows what the snapshot showed.
        let apply = |plan: &Plan| {
            let typed = plan.vault_url != shown().plan.vault_url;
            system_state_apply(root.path(), plan, typed, || Ok(configured.clone()))
        };
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert_eq!(shown().plan.vault_url, team);

        let typed = Plan {
            vault_url: other.into(),
            ..Plan::default()
        };
        let failing = || Fake {
            failure: Some(Stage::Secrets),
            ..Fake::default()
        };
        let revision = apply(&typed);
        assert_eq!(shown().plan, typed);
        // The Apply's pass, then the worker's retry of the same run.
        for pass in 0..2 {
            let mut host = failing();
            execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
            assert_eq!(host.vault_url.as_deref(), Some(other), "pass {pass}");
            let snapshot = shown();
            assert!(!snapshot.finished);
            assert_eq!(snapshot.plan, typed, "pass {pass}");
            assert!(
                snapshot
                    .rows
                    .iter()
                    .any(|row| row.name == Stage::Secrets.name() && row.status == "pending"),
                "{:?}",
                snapshot.rows
            );
        }
        // An Apply left as shown asks for the typed vault again.
        let revision = apply(&shown().plan);
        let mut host = failing();
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        assert_eq!(host.vault_url.as_deref(), Some(other));
        assert_eq!(shown().plan, typed);

        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"cancelled"}),
        )
        .unwrap();
        assert_eq!(shown().plan.vault_url, team);
    }

    #[test]
    fn an_unchecked_vault_source_cannot_block_selected_preferences() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Widgets]);
        plan.vault_url = "unfinished source".into();
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Workstation]);
        assert!(finished_at(root.path()).unwrap());
        let before = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ]
        .map(|name| std::fs::read(root.path().join(name)).unwrap());
        plan.selection.set(Setting::Secrets, true);
        assert!(restart_at(root.path(), &plan).is_err());
        let after = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ]
        .map(|name| std::fs::read(root.path().join(name)).unwrap());
        assert_eq!(
            before, after,
            "a selected invalid vault cannot authorize changes"
        );
    }

    #[test]
    fn an_invalid_vault_source_changes_no_authorization_and_its_error_contains_no_value() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan {
            vault_url: "https://user:fixture-private@vault.example/v1/team".into(),
            ..Plan::default()
        };
        assert!(validate_initial_setup_plan(&plan).is_err());
        let error = authorize_at(root.path(), &plan).unwrap_err();
        assert!(!error.contains("fixture-private"));
        assert!(!root.path().join("automatic-plan.json").exists());
        assert!(!root.path().join("initial-setup.json").exists());
    }

    #[test]
    fn a_plan_published_before_a_failed_progress_reset_requires_authorization() {
        let root = aterm_tempfile::tempdir().unwrap();
        let old = selected(&[Setting::Widgets, Setting::Spotlight]);
        authorize_at(root.path(), &old).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let new = selected(&[Setting::Dock, Setting::Secrets]);
        // Simulate a crash after the plan rename, before its progress reset.
        save_plan(root.path(), &new).unwrap();
        assert!(!finished_at(root.path()).unwrap());
        let (_, _, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(rows.iter().all(|row| row.status == "pending"));
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(!finished_at(root.path()).unwrap());
        assert!(authorize_at(root.path(), &new).unwrap().is_some());
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn identical_plan_restarts_retire_older_workers_but_stage_updates_keep_the_revision() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), 0);
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let first = revision_at(root.path()).unwrap();
        assert_eq!(first, 1);
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), first);
        // A new offer can carry a corrected password for the same plan.
        authorize_at(root.path(), &plan).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), first + 1);
        restart_at(root.path(), &plan).unwrap();
        let rerun = revision_at(root.path()).unwrap();
        assert_eq!(rerun, first + 2);
        host.pending = None;
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(revision_at(root.path()).unwrap(), rerun);
        restart_at(root.path(), &plan).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), rerun + 1);
        assert!(!finished_at(root.path()).unwrap());
    }

    #[test]
    fn revision_reads_refuse_interrupted_plan_resets_and_counters_never_wrap() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let next = selected(&[Setting::Repositories]);
        save_plan(root.path(), &next).unwrap();
        assert!(revision_at(root.path()).is_err());
        authorize_at(root.path(), &next).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), 2);
        let mut latest = progress(root.path()).unwrap().unwrap();
        latest.revision = u64::MAX;
        save_progress(root.path(), latest).unwrap();
        let before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        assert!(restart_at(root.path(), &plan).is_err());
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            before
        );
        assert_eq!(saved_plan(root.path()).unwrap(), Some(next));
        assert_eq!(revision_at(root.path()).unwrap(), u64::MAX);
    }

    /// The Secrets stage over a vault on a network: [`aterm_machines::init_secrets`]'s
    /// item, run by the stage's own entry, against this Mac's vault state as the client
    /// keeps it. `log` holds each journal line and each change, in order.
    mod vault {
        use super::*;
        use aterm_machines::init_secrets::SecretsOps;
        use aterm_secrets::client::{Config, JoinOutcome, Look, Membership, ProbeOutcome, Status};
        use aterm_secrets::service::State;
        use aterm_secrets::transport::Source;
        use std::sync::{Arc, Mutex};

        pub(super) const NO_ROUTE: &str =
            "fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65)";

        pub(super) struct Lan {
            /// Whether the vault answers this Mac.
            pub(super) answers: Mutex<bool>,
            /// The network this Mac is on, as the stage's marks read it.
            pub(super) network: Mutex<String>,
            status: Mutex<Status>,
            subscribed: Mutex<bool>,
            /// How often the stage asked whether the vault answers.
            pub(super) asked: Mutex<u32>,
            /// Why each join fails (`None`: it queues its request).
            pub(super) refusal: Mutex<Option<String>>,
            /// Where this aterm runs from when that goes away (a disk image): every
            /// install is then refused, as `aterm_secrets::service::service_program` does.
            pub(super) fleeting: Mutex<Option<PathBuf>>,
            log: Mutex<Vec<String>>,
        }

        impl Lan {
            pub(super) fn new(answers: bool, network: &str) -> Arc<Self> {
                Arc::new(Self {
                    answers: Mutex::new(answers),
                    network: Mutex::new(network.into()),
                    status: Mutex::new(Status {
                        config: None,
                        machine: None,
                        membership: Membership::Unconfigured,
                        names: Vec::new(),
                    }),
                    subscribed: Mutex::new(false),
                    asked: Mutex::new(0),
                    refusal: Mutex::new(None),
                    fleeting: Mutex::new(None),
                    log: Mutex::new(Vec::new()),
                })
            }

            pub(super) fn log(&self) -> Vec<String> {
                self.log.lock().unwrap().clone()
            }

            /// An admin approved this Mac, and its subscriber wrote the version that let it
            /// in.
            pub(super) fn approve(&self, heard: u64) {
                let mut status = self.status.lock().unwrap();
                status.membership = Membership::In { seq: 7, heard };
                status.names = vec!["GH_TOKEN".into()];
            }

            /// This Mac's vault state now.
            pub(super) fn status_now(&self) -> Status {
                self.status.lock().unwrap().clone()
            }

            /// Set this Mac's vault state, and whether its subscriber runs.
            pub(super) fn set(&self, status: Status, subscribed: bool) {
                *self.status.lock().unwrap() = status;
                *self.subscribed.lock().unwrap() = subscribed;
            }

            /// This Mac is in the vault at `source`, as an approved join leaves it, and
            /// runs no subscriber.
            pub(super) fn in_vault(&self, source: &str, heard: u64) {
                let mut status = member(source, Membership::In { seq: 7, heard });
                status.names = vec!["GH_TOKEN".into()];
                self.set(status, false);
            }
        }

        /// This Mac's vault state after a join of the vault at `source`, with its key.
        fn member(source: &str, membership: Membership) -> Status {
            let recipient = aterm_secrets::age::Identity::from_secret_bytes([31; 32]).recipient();
            let code = aterm_secrets::client::join_code(&recipient);
            let source = Source::Url(source.into());
            Status {
                config: Some(Config {
                    name: aterm_secrets::transport::vault_name(&source).unwrap(),
                    source,
                    signer: Some([3; 32]),
                    followed: Vec::new(),
                    host: "m22".into(),
                }),
                machine: Some((recipient, code)),
                membership,
                names: Vec::new(),
            }
        }

        /// What paces the stage over `lan`: the production marks ([`secrets_retry::pace`])
        /// of its network and, with `vault`, its vault state, at `build` and `now`.
        pub(super) struct LanReads {
            pub(super) lan: Arc<Lan>,
            pub(super) vault: bool,
            pub(super) build: String,
            pub(super) now: u64,
        }

        impl secrets_retry::Reads for LanReads {
            fn build(&self) -> String {
                self.build.clone()
            }
            fn now(&self) -> u64 {
                self.now
            }
            fn routes(&self) -> String {
                self.lan.network.lock().unwrap().clone()
            }
            fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
                self.vault
                    .then(|| Arc::clone(&self.lan) as Arc<dyn SecretsOps>)
            }
        }

        impl SecretsOps for Lan {
            fn status(&self) -> aterm_secrets::Result<Status> {
                Ok(self.status.lock().unwrap().clone())
            }
            fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
                Ok(self.status.lock().unwrap().config.clone())
            }
            fn subscriber(&self) -> aterm_secrets::Result<State> {
                Ok(if *self.subscribed.lock().unwrap() {
                    State::Installed {
                        pid: Some(4242),
                        unit: PathBuf::from("/test-home/Library/LaunchAgents/subscriber.plist"),
                    }
                } else {
                    State::Absent
                })
            }
            fn terminal(&self) -> bool {
                false
            }
            fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
                Err(aterm_secrets::Error::new(
                    "the setup never looks for a vault",
                ))
            }
            fn reachable(&self, _source: &Source) -> Look {
                *self.asked.lock().unwrap() += 1;
                if *self.answers.lock().unwrap() {
                    Look::Answers
                } else {
                    Look::Unreached(NO_ROUTE.into())
                }
            }
            fn keep(&self, into: &Path, replaces: &Replaces) -> aterm_secrets::Result<()> {
                if let Replaces::Read(expected) = replaces
                    && self.status.lock().unwrap().config != *expected
                {
                    return Err(aterm_secrets::Error::new("the fixture's vault changed"));
                }
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("keep {}", into.display()));
                Ok(())
            }
            fn join(
                &self,
                _sys: &dyn aterm_machines::port::System,
                (source, replaces): (&str, &Replaces),
                _password: Option<&SecretBytes>,
                interactive: bool,
                _wait: std::time::Duration,
                requested: &dyn Fn() -> bool,
            ) -> aterm_secrets::Result<JoinOutcome> {
                assert!(!interactive, "the setup's join is unattended");
                self.log.lock().unwrap().push(format!("join {source}"));
                if let Some(why) = self.refusal.lock().unwrap().clone() {
                    return Err(aterm_secrets::Error::new(why));
                }
                let status = member(source, Membership::Pending);
                let code = status.machine.as_ref().map(|(_, code)| code.clone());
                {
                    let mut current = self.status.lock().unwrap();
                    if let Replaces::Read(expected) = replaces
                        && current.config != *expected
                    {
                        return Err(aterm_secrets::Error::new("the fixture's vault changed"));
                    }
                    *current = status;
                }
                requested();
                Ok(JoinOutcome::Pending {
                    code: code.expect("a join makes a key"),
                    host: "m22".into(),
                })
            }
            fn install_subscriber(&self) -> aterm_secrets::Result<String> {
                self.log.lock().unwrap().push("install".into());
                if let Some(exe) = self.fleeting() {
                    aterm_secrets::service::service_program(&exe, &|_| false)?;
                }
                *self.subscribed.lock().unwrap() = true;
                Ok("installed the subscriber".into())
            }
            fn fleeting(&self) -> Option<PathBuf> {
                self.fleeting.lock().unwrap().clone()
            }
            fn synced(&self) -> aterm_secrets::Result<bool> {
                Ok(true)
            }
            fn sync(&self) -> aterm_secrets::Result<()> {
                self.log.lock().unwrap().push("sync".into());
                Ok(())
            }
            fn say(&self, _line: &str) {
                // The live port of the setup's worker is quiet (`init_secrets::unattended`).
            }
        }

        /// The converge journal, into the vault's log (`refuse`: it refuses every line, as
        /// a full disk does).
        struct Lines(Arc<Lan>, bool);

        impl aterm_converge::Journal for Lines {
            fn before_change(
                &mut self,
                id: &str,
                _from: &str,
                _to: &str,
                revert: Option<&str>,
            ) -> Result<(), String> {
                if self.1 {
                    return Err("No space left on device (os error 28)".into());
                }
                assert!(revert.is_some_and(|revert| revert.contains("aterm secrets agent off")));
                self.0.log.lock().unwrap().push(format!("line {id}"));
                Ok(())
            }
        }

        /// The epoch second of the worker's first pass.
        pub(super) const START: u64 = 1_800_000_000;

        /// System State's setup with only the Secrets stage left to run, over `lan`.
        pub(super) struct Vaulted {
            pub(super) lan: Arc<Lan>,
            rest: Fake,
            /// The journal refuses every line.
            pub(super) refuse: bool,
            /// The stage is paced ([`InitialHost::secrets_pace`] answers).
            pub(super) paced: bool,
            /// The marks read this Mac's vault state as well as its network, as the live
            /// ones do.
            pub(super) marks_vault: bool,
            pub(super) build: String,
            pub(super) now: u64,
            /// When each try came, in seconds since the epoch.
            tries: Vec<u64>,
            /// The vault the person joins from a terminal just after the stage's next read
            /// of which vault this Mac is in ([`InitialHost::configured_vault`]).
            pub(super) joins_after_read: Option<&'static str>,
        }

        impl Vaulted {
            pub(super) fn new(lan: &Arc<Lan>) -> Self {
                Self {
                    lan: Arc::clone(lan),
                    rest: Fake::default(),
                    refuse: false,
                    paced: true,
                    marks_vault: true,
                    build: "1.0.0+gtest".into(),
                    now: START,
                    tries: Vec::new(),
                    joins_after_read: None,
                }
            }

            /// The minute of each try, counted from the first pass.
            pub(super) fn minutes(&self) -> Vec<u64> {
                self.tries.iter().map(|at| (at - START) / 60).collect()
            }

            /// What the live host's pace would read here now (`vault`: its vault state too).
            pub(super) fn reads(&self, vault: bool) -> LanReads {
                LanReads {
                    lan: Arc::clone(&self.lan),
                    vault,
                    build: self.build.clone(),
                    now: self.now,
                }
            }
        }

        fn context() -> aterm_machines::items::Ctx {
            let user = aterm_machines::model::User {
                name: "tester".into(),
                uid: 501,
                gid: 20,
                home: "/test-home".into(),
                shell: "/bin/zsh".into(),
                admin: true,
            };
            aterm_machines::items::Ctx {
                facts: aterm_machines::model::HostFacts::default(),
                users: vec![user.clone()],
                console: Some(user),
                host_name: "m22".into(),
                cache_dir: PathBuf::from("/test-cache"),
                disrupt: false,
            }
        }

        impl InitialHost for Vaulted {
            fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.rest.workstation(root)
            }
            fn spotlight(&mut self) -> Result<Vec<Row>, String> {
                self.rest.spotlight()
            }
            fn permissions(&mut self) -> Result<Vec<Row>, String> {
                self.rest.permissions()
            }
            fn secrets(
                &mut self,
                plan: &Plan,
                replaces: Replaces,
                password: &mut Option<SecretBytes>,
            ) -> Result<Vec<Row>, String> {
                self.tries.push(self.now);
                let ops: Arc<dyn SecretsOps> = self.lan.clone();
                aterm_machines::init::run_initial_secrets_with(
                    &super::super::super::tests::Inert::default(),
                    &context(),
                    ops,
                    &plan.vault_url,
                    replaces,
                    password,
                    APPROVAL_WAIT,
                    &mut Lines(Arc::clone(&self.lan), self.refuse),
                )
                .map(secrets_rows)
            }
            fn secrets_pace(&mut self) -> Option<Pace> {
                self.paced
                    .then(|| secrets_retry::pace(&self.reads(self.marks_vault)))
            }
            fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
                let read = self.lan.status_now().config.map(|config| ConfiguredVault {
                    config,
                    home: PathBuf::from("/test-home"),
                });
                if let Some(source) = self.joins_after_read.take() {
                    self.lan.in_vault(source, START);
                }
                Ok(read)
            }
            /// The live read of this Mac's vault state ([`check::vault_rows`]): the files its
            /// vault wrote are as written, and its subscriber runs or not.
            fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
                let subscribed = *self.lan.subscribed.lock().unwrap();
                check::vault_rows(
                    plan,
                    Path::new("/test-home"),
                    &self.lan.status_now(),
                    |_| Ok(true),
                    || Ok(subscribed),
                )
            }
            fn repositories(&mut self) -> Result<Vec<Row>, String> {
                self.rest.repositories()
            }
            fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.rest.withheld(root)
            }
        }

        /// A run that selects the Secrets stage alone, authorized.
        pub(super) fn authorized() -> (aterm_tempfile::TempDir, u64) {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &selected(&[Setting::Secrets]))
                .unwrap()
                .unwrap();
            (root, revision)
        }

        /// The worker's passes of run `revision`, a minute apart from [`START`], through
        /// minute `last`, `before(minute, host)` run before each: each pass's Secrets row,
        /// until the run is done.
        pub(super) fn passes(
            host: &mut Vaulted,
            root: &Path,
            revision: u64,
            password: &mut Option<SecretBytes>,
            minutes: std::ops::RangeInclusive<u64>,
            mut before: impl FnMut(u64, &mut Vaulted),
        ) -> Vec<Row> {
            let mut rows = Vec::new();
            for minute in minutes {
                host.now = START + minute * 60;
                before(minute, host);
                let Some(report) =
                    execute_initial_for_revision(host, root, revision, password).unwrap()
                else {
                    break;
                };
                rows.extend(
                    report
                        .rows
                        .into_iter()
                        .filter(|row| row.name == Stage::Secrets.name()),
                );
            }
            rows
        }
    }

    fn join_password() -> SecretBytes {
        SecretBytes::from(b"test-only-setup-join-password".as_slice())
    }

    /// A vault that does not answer (the owner's Mac off the ALab LAN, 2026-10-22, asked
    /// every minute) is asked at growing pauses: a minute, two, then every four, and
    /// nothing is journaled or changed at any try. Between tries the stage shows its last
    /// try's row as it was, so the worker's log keeps one line until the next. Negative
    /// control: a stage that is not paced asks at every pass.
    #[test]
    fn an_unreachable_vault_is_asked_at_growing_pauses_and_nothing_is_changed() {
        use vault::{Lan, NO_ROUTE, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        let mut password = Some(join_password());
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            0..=20,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0, 1, 3, 7, 11, 15, 19]);
        assert_eq!(*lan.asked.lock().unwrap(), 7);
        assert!(
            lan.log().is_empty(),
            "journaled or changed: {:?}",
            lan.log()
        );
        assert!(password.is_some(), "kept for a try that reaches the vault");
        assert_eq!(rows.len(), 21);
        let offline = format!("the vault does not answer, so nothing was changed: {NO_ROUTE}; ");
        assert!(
            rows.iter()
                .all(|row| row.status == "blocked" && row.detail.starts_with(&offline)),
            "{rows:?}"
        );
        assert!(rows[0].detail.ends_with(
            "aterm tries again by itself a minute after this try, and at once when this Mac's network or vault changes or on Apply in System State"
        ));
        assert_eq!(
            rows[2].detail, rows[1].detail,
            "a paused pass shows its try"
        );
        assert!(rows[2].detail.contains("2 minutes after this try"));
        assert!(rows[20].detail.contains("4 minutes after this try"));
        let distinct: std::collections::BTreeSet<&str> =
            rows.iter().map(|row| row.detail.as_str()).collect();
        assert_eq!(distinct.len(), 3, "{distinct:?}");

        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut unpaced = Vaulted::new(&lan);
        unpaced.paced = false;
        passes(
            &mut unpaced,
            root.path(),
            revision,
            &mut None,
            0..=20,
            |_, _| {},
        );
        assert_eq!(unpaced.minutes(), (0..=20).collect::<Vec<_>>());
        assert_eq!(*lan.asked.lock().unwrap(), 21);
        assert!(lan.log().is_empty());
    }

    /// A pause ends at once with a change that can alter the answer: another network,
    /// another build of aterm, or the person's Apply (a new run). Each starts the pauses
    /// again from a minute.
    #[test]
    fn a_paused_try_comes_at_once_on_a_new_network_build_or_run() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=14,
            |minute, host| match minute {
                5 => *host.lan.network.lock().unwrap() = "office".into(),
                10 => host.build = "1.0.0+gnext".into(),
                _ => {}
            },
        );
        assert_eq!(host.minutes(), [0, 1, 3, 5, 6, 8, 10, 11, 13]);

        let plan = selected(&[Setting::Secrets]);
        let rerun = restart_at(root.path(), &plan).unwrap();
        assert!(!root.path().join(secrets_retry::RECORD).exists());
        let mut host = Vaulted::new(&lan);
        host.now = vault::START + 14 * 60;
        execute_initial_for_revision(&mut host, root.path(), rerun, &mut None).unwrap();
        assert_eq!(host.minutes(), [14], "the person's Apply tries at once");
    }

    /// A Mac waiting for an admin's approval is picked up at the worker's next pass once
    /// its subscriber wrote the version that let it in (its vault state changed), and
    /// never later than [`secrets_retry::LONGEST`] after its last try even when nothing
    /// the marks read changed. The join that asked made each of its changes after the
    /// journal's line and spent the password. Negative control: marks that read only the
    /// network pick it up later, at the paused try.
    #[test]
    fn an_approval_is_picked_up_at_the_next_pass_and_within_the_longest_pause() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        for marks_vault in [true, false] {
            let (root, revision) = authorized();
            let lan = Lan::new(true, "home");
            let mut host = Vaulted::new(&lan);
            host.marks_vault = marks_vault;
            let mut password = Some(join_password());
            let approved = 8;
            let rows = passes(
                &mut host,
                root.path(),
                revision,
                &mut password,
                0..=30,
                |minute, host| {
                    if minute == approved {
                        host.lan.approve(START + minute * 60);
                    }
                },
            );
            let tries = host.minutes();
            let picked = *tries.last().unwrap();
            assert_eq!(tries[..4], [0, 1, 3, 7], "{tries:?}");
            if marks_vault {
                assert_eq!(picked, approved, "{tries:?}");
            } else {
                assert_eq!(picked, 11, "{tries:?}");
            }
            assert!(
                (picked - tries[tries.len() - 2]) * 60 <= secrets_retry::LONGEST.as_secs(),
                "{tries:?}"
            );
            let last = rows.last().unwrap();
            assert_eq!(last.status, "ok", "{rows:?}");
            assert!(finished_at(root.path()).unwrap());
            assert!(!root.path().join(secrets_retry::RECORD).exists());
            assert!(
                rows[1].status == "blocked" && rows[1].detail.contains("aterm secrets approve"),
                "{rows:?}"
            );
            assert_eq!(
                lan.log(),
                [
                    "line init.secrets@tester".to_owned(),
                    format!("join {}", aterm_types::macos_setup::DEFAULT_VAULT_URL),
                    "install".to_owned(),
                ]
            );
            assert!(password.is_none(), "the join's proof is with the vault");
            assert_eq!(*lan.asked.lock().unwrap(), 1, "only the join reached out");
        }
    }

    /// The live host paces every Secrets try by what its reads read now
    /// ([`secrets_retry::pace`]): another network, a change in this Mac's vault state (an
    /// approval its subscriber wrote) or in whether the subscriber runs, and another build
    /// each change the pace; the subscriber hearing from the vault does not. Negative
    /// control: marks that do not read the vault state miss the approval.
    #[test]
    fn the_live_host_paces_each_secrets_try_by_the_network_and_the_vault_state() {
        use aterm_secrets::client::{Membership, Status};
        use std::sync::Arc;
        use vault::{Lan, LanReads};
        let lan = Lan::new(false, "away");
        let reads = |vault: bool| LanReads {
            lan: Arc::clone(&lan),
            vault,
            build: "1.0.0+gtest".into(),
            now: vault::START,
        };
        let mut host = LiveInitial {
            reads: reads(true),
            spotlight_result: None,
            permissions_result: None,
        };
        let first = host.secrets_pace().expect("the live host paces every try");
        assert_eq!(first, secrets_retry::pace(&reads(true)));
        assert_eq!(first.build, "1.0.0+gtest");
        assert_eq!(first.now, vault::START);
        let mut host_marks = vec![host.secrets_pace().unwrap().marks];
        assert_eq!(host_marks[0], first.marks, "nothing changed");

        *lan.network.lock().unwrap() = "office".into();
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        let pending = Status {
            membership: Membership::Pending,
            ..lan.status_now()
        };
        lan.set(pending, true);
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        lan.approve(vault::START);
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        // The subscriber heard from the vault again: no change.
        lan.approve(vault::START + 600);
        assert_eq!(
            secrets_retry::pace(&host.reads).marks,
            *host_marks.last().unwrap()
        );
        let joined = lan.status_now();
        lan.set(joined, false);
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        let distinct: std::collections::BTreeSet<&String> = host_marks.iter().collect();
        assert_eq!(distinct.len(), host_marks.len(), "{host_marks:?}");

        // The control: without the vault state, the approval changes nothing.
        let unread = || secrets_retry::pace(&reads(false)).marks;
        let before = unread();
        lan.approve(vault::START + 900);
        let pending = Status {
            membership: Membership::Pending,
            ..lan.status_now()
        };
        lan.set(pending, true);
        assert_eq!(unread(), before);
    }

    /// Two aterm builds taking turns (an updated app beside a session of the build before)
    /// share the pause of a vault that does not answer once each has tried: the vault is
    /// asked about as often as by one build, not at every pass. Negative control: a build
    /// that has not tried yet tries at once.
    #[test]
    fn builds_taking_turns_share_the_pause_of_a_vault_that_does_not_answer() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=20,
            |minute, host| {
                host.build = if minute % 2 == 0 {
                    "1.0.0+gold".into()
                } else {
                    "1.0.0+gnew".into()
                };
            },
        );
        assert_eq!(host.minutes(), [0, 1, 2, 4, 8, 12, 16, 20]);
        assert_eq!(*lan.asked.lock().unwrap(), 8);
        assert!(lan.log().is_empty());

        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            21..=21,
            |_, host| host.build = "1.0.0+gthird".into(),
        );
        assert_eq!(*host.minutes().last().unwrap(), 21, "the control");
    }

    /// A join that fails for a reason only a person can fix (a vault whose signer nothing
    /// pins) is journaled and made twice, then waits for a change: a day of passes writes two
    /// journal lines, not one every four minutes, and the row says what brings the next try.
    /// A new network brings two more. Negative control: a vault that does not answer, whose
    /// tries journal nothing, is still asked at its pace.
    #[test]
    fn a_join_that_keeps_failing_is_journaled_twice_then_waits_for_a_change() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "home");
        *lan.refusal.lock().unwrap() = Some(
            "http://alab-secrets.local:7484/v1/alab is signed by 0123-4567, which nothing pins: run this in a terminal to confirm it, or add #<signer> to the URL".into(),
        );
        let mut host = Vaulted::new(&lan);
        let mut password = Some(join_password());
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            0..=1440,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0, 1]);
        let lines = |lan: &Lan| lan.log().iter().filter(|e| e.starts_with("line ")).count();
        assert_eq!(lines(&lan), 2, "{:?}", lan.log());
        assert!(password.is_some(), "kept: no join spent it");
        let last = rows.last().unwrap();
        assert_eq!(last.status, "error", "{rows:?}");
        assert!(
            last.detail.ends_with(
                "aterm tried this twice by itself and it did not take, so it tries again only when this Mac's network or vault changes, after an update of aterm, or on Apply in System State"
            ),
            "{last:?}"
        );

        passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            1441..=1500,
            |minute, host| {
                if minute == 1441 {
                    *host.lan.network.lock().unwrap() = "office".into();
                }
            },
        );
        assert_eq!(host.minutes(), [0, 1, 1441, 1442]);
        assert_eq!(lines(&lan), 4);

        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=60,
            |_, _| {},
        );
        assert_eq!(host.minutes().len(), 17, "{:?}", host.minutes());
        assert_eq!(lines(&lan), 0);
    }

    /// System State's setup opened from aterm's disk image installs no subscriber: its
    /// Secrets row waits (blocked, never an error, so no try counts toward the hold), says to
    /// open aterm from Applications, and is paced as any wait; the first pass of an aterm
    /// opened from where it is installed tries at once and installs it. Negative control: an
    /// aterm still on its disk image keeps the pause.
    #[test]
    fn a_setup_from_a_disk_image_waits_and_tries_at_once_from_applications() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        let image = PathBuf::from("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
        for opened in [Some(5), None] {
            let (root, revision) = authorized();
            let lan = Lan::new(true, "home");
            lan.in_vault(&selected(&[Setting::Secrets]).vault_url, START);
            *lan.fleeting.lock().unwrap() = Some(image.clone());
            let mut host = Vaulted::new(&lan);
            let rows = passes(
                &mut host,
                root.path(),
                revision,
                &mut None,
                0..=8,
                |minute, host| {
                    if Some(minute) == opened {
                        *host.lan.fleeting.lock().unwrap() = None;
                    }
                },
            );
            let waits = &rows[..opened.map_or(rows.len(), |at| at as usize)];
            assert!(
                waits.iter().all(|row| row.status == "blocked"
                    && row
                        .detail
                        .starts_with(&format!("this aterm runs from {}", image.display()))
                    && row
                        .detail
                        .contains("move aterm to Applications and open it from there")
                    && !row.detail.contains("--program")),
                "{rows:?}"
            );
            let installs = lan.log().iter().filter(|event| *event == "install").count();
            if opened.is_some() {
                assert_eq!(host.minutes(), [0, 1, 3, 5]);
                assert_eq!(installs, 4);
                assert_eq!(rows.last().unwrap().status, "applied", "{rows:?}");
            } else {
                assert_eq!(host.minutes(), [0, 1, 3, 7]);
                assert_eq!(installs, 4);
                assert_eq!(rows.len(), 9);
            }
        }
    }

    /// A refused journal line refuses the change: no key, no request, no subscriber, the
    /// password kept, and the row says why. Negative control: the next try, its line
    /// taken, joins.
    #[test]
    fn a_refused_journal_line_refuses_the_secrets_stages_change() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "home");
        let mut host = Vaulted::new(&lan);
        host.refuse = true;
        let mut password = Some(join_password());
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            0..=0,
            |_, _| {},
        );
        assert_eq!(rows[0].status, "error", "{rows:?}");
        assert!(
            rows[0]
                .detail
                .starts_with("not applied: the journal refused the record"),
            "{rows:?}"
        );
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert!(password.is_some());

        host.refuse = false;
        passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            1..=1,
            |_, _| {},
        );
        assert_eq!(lan.log().len(), 3, "{:?}", lan.log());
        assert!(lan.log()[0].starts_with("line "));
        assert!(password.is_none());
    }

    /// The vault a person joins from a terminal after setup began, its URL with a query.
    const JOINED_SINCE: &str = "http://vault.example:7484/v1/team?k=fixture-query";

    /// A VAULT JOINED AFTER SETUP BEGAN STAYS. First-run setup was admitted for the ALab
    /// vault while this Mac was in none, and off the ALab network its Secrets stage stood
    /// aside. The person then joined their own vault from a terminal. When ALab answers
    /// again (an office visit), the worker's retry joins nothing: no key, request or journal
    /// line. Its row names the vault this Mac is in, never its URL's query, and how to move
    /// it, beside that vault's own rows, read as repository setup reads them: the stage is
    /// done, and the run finishes, once that vault's subscriber runs. Before that retry
    /// System State's field offers the vault this Mac is in, so an Apply left as shown keeps
    /// it. Negative controls: a stage that always runs the vault item joins ALab here, a
    /// snapshot blind to the vault kept at admission offers ALab, and a stage that reads
    /// nothing of the vault it leaves finishes with no subscriber running.
    #[test]
    fn a_vault_joined_after_setup_began_stays_when_the_setup_vault_answers() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
        assert!(record.contains(r#""admitted_vault":null"#), "{record}");
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(rows[0].status, "blocked", "{rows:?}");

        lan.in_vault(JOINED_SINCE, START);
        let joined = lan.status_now();
        let shown = snapshot_in_vault(root.path(), host.configured_vault()).unwrap();
        assert!(!shown.finished);
        assert_eq!(shown.plan.vault_url, JOINED_SINCE);

        *lan.answers.lock().unwrap() = true;
        host.now = START + 60;
        let report = execute_initial_for_revision(&mut host, root.path(), revision, &mut None)
            .unwrap()
            .unwrap();
        assert_eq!(host.minutes(), [0], "the vault item ran again");
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert_eq!(lan.status_now(), joined);
        let [left, vault, subscriber] = report.rows.as_slice() else {
            panic!("{:?}", report.rows)
        };
        assert_eq!(
            (left.name.as_str(), left.status.as_str()),
            ("Secrets vault", "ok")
        );
        assert_eq!(
            left.detail,
            "this Mac joined vault team (http://vault.example:7484/v1/team) after setup began, so setup leaves it there; to move it to vault alab (http://alab-secrets.local:7484/v1/alab), type that vault in System State and press Apply"
        );
        assert_eq!(vault.status, "ok", "{vault:?}");
        assert_eq!(
            (subscriber.name.as_str(), subscriber.status.as_str()),
            ("Secrets subscriber", "blocked")
        );
        assert!(!finished_at(root.path()).unwrap());

        // The subscriber the terminal's join installed runs: the next pass reads it.
        lan.set(joined.clone(), true);
        host.now = START + 120;
        let report = execute_initial_for_revision(&mut host, root.path(), revision, &mut None)
            .unwrap()
            .unwrap();
        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert_eq!(host.minutes(), [0]);
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert!(finished_at(root.path()).unwrap());
        assert!(!root.path().join(secrets_retry::RECORD).exists());
    }

    /// A vault left in place that waits for an admin's approval is not read as synced: the
    /// stage's rows say it waits, the run stays open and nothing is joined, and once the
    /// approval reaches it the stage is done. Negative control: a stage that reads nothing
    /// of the vault it leaves reports it ready while it waits.
    #[test]
    fn a_vault_left_in_place_that_waits_for_approval_is_not_done() {
        use aterm_secrets::client::Membership;
        use vault::{Lan, START, Vaulted, authorized};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut waiting = lan.status_now();
        waiting.membership = Membership::Pending;
        lan.set(waiting, true);
        let mut host = Vaulted::new(&lan);
        let report = execute_initial_for_revision(&mut host, root.path(), revision, &mut None)
            .unwrap()
            .unwrap();
        assert!(host.minutes().is_empty(), "the vault item ran");
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        let vault = report
            .rows
            .iter()
            .find(|row| row.detail.starts_with("waiting for vault approval, code "))
            .unwrap_or_else(|| panic!("{:?}", report.rows));
        assert_eq!(
            (vault.name.as_str(), vault.status.as_str()),
            ("Secrets vault", "blocked")
        );
        assert!(!rows_ready(&report.rows));
        assert!(!finished_at(root.path()).unwrap());

        lan.approve(START);
        host.now = START + 60;
        let report = execute_initial_for_revision(&mut host, root.path(), revision, &mut None)
            .unwrap()
            .unwrap();
        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert!(finished_at(root.path()).unwrap());
        assert!(lan.log().is_empty(), "{:?}", lan.log());
    }

    /// The vault the plan names under another signer than its pin admits, joined since, is
    /// said so rather than named twice. Negative control: the plain wording names it as the
    /// vault left in place and as the one to move to.
    #[test]
    fn the_same_vault_under_another_signer_is_said_so() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let team = "https://team.example/v1/team";
        let vault = configure_vault(&paths, team);
        let pinned = Plan {
            vault_url: format!("{team}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
            ..selected(&[Setting::Secrets])
        };
        assert!(AdmittedVault::None.stands_in(&pinned, &vault));
        assert_eq!(
            vault.left_in_place(&pinned),
            "this Mac joined vault team (https://team.example/v1/team) after setup began, under another signer than the one setup names, so setup leaves it there; to join it under that signer, type the vault with its #<signer> in System State and press Apply"
        );
    }

    /// AN APPLY FROM A TAB SHOWN BEFORE A TERMINAL JOIN KEEPS THAT VAULT. Setup finished in
    /// the ALab vault, and the System State tab still shows it. The person joins their own
    /// vault from a terminal, then ticks Spotlight in that tab and presses Apply without
    /// touching the vault field: the run asks for the vault this Mac is in now, so its
    /// Secrets stage resumes that one rather than join ALab over it. A vault typed in the
    /// same tab still moves the Mac. Negative control: an Apply admitted with its field as
    /// submitted, typed or not, joins ALab.
    #[test]
    fn an_apply_from_a_tab_shown_before_a_terminal_join_keeps_that_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let alab = Plan::default().vault_url;
        configure_vault(&paths, &alab);
        let read = || ConfiguredVault::read(&paths);
        authorize_reading(root.path(), &selected(&[Setting::Secrets]), read).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let shown = snapshot_in_vault(root.path(), read()).unwrap().plan;
        assert_eq!(shown.vault_url, alab);

        let team = "http://vault.example:7484/v1/team";
        configure_vault(&paths, team);
        let ticked = Plan {
            vault_url: shown.vault_url.clone(),
            ..selected(&[Setting::Secrets, Setting::Spotlight])
        };
        let revision = system_state_apply(root.path(), &ticked, false, read);
        assert_eq!(saved_plan(root.path()).unwrap().unwrap().vault_url, team);
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        assert_eq!(host.vault_url.as_deref(), Some(team));

        let other = "https://other.example/v1/other";
        let typed = Plan {
            vault_url: other.into(),
            ..ticked
        };
        let revision = system_state_apply(root.path(), &typed, true, read);
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        assert_eq!(host.vault_url.as_deref(), Some(other));
    }

    /// The Secrets stage's join replaces only the vault configuration the stage read: a
    /// vault joined from a terminal just after that read, before the vault item reads it, is
    /// not joined over (no line, key or join, and an unverified row that counts no failure),
    /// and the next try, reading it, leaves it in place. Negative control: a vault item that
    /// ignores the stage's read joins ALab over it.
    #[test]
    fn a_vault_joined_after_the_stage_read_it_is_not_joined_over() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "office");
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        host.joins_after_read = Some(JOINED_SINCE);
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        let [row] = rows.as_slice() else {
            panic!("{rows:?}")
        };
        assert_eq!(row.status, "unverified", "{row:?}");
        assert_eq!(
            row.detail,
            "this Mac's vault changed after it was read (now vault team (http://vault.example:7484/v1/team)), so nothing was changed; the next try reads it"
        );
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert!(!finished_at(root.path()).unwrap());

        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            1..=1,
            |_, _| {},
        );
        assert!(
            rows[0].detail.starts_with("this Mac joined vault team"),
            "{rows:?}"
        );
        assert!(lan.log().is_empty(), "{:?}", lan.log());
    }

    /// Repository setup reads the vault the Secrets stage left in place, not the one the
    /// plan names, so a run of every stage finishes in the vault this Mac joined since,
    /// without the stage's join. Negative control: a repository gate that reads the plan's
    /// vault asks about ALab, which the live read finds this Mac not in.
    #[test]
    fn repository_setup_reads_the_vault_the_secrets_stage_left_in_place() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let team = "http://vault.example:7484/v1/team";
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            configured: Some(configure_vault(&paths, team)),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert!(!host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
        assert_eq!(host.vault_url, None, "joined");
        assert_eq!(host.checked_url.as_deref(), Some(team));
        assert!(
            host.calls.contains(&Stage::Repositories),
            "{:?}",
            host.calls
        );
        assert!(finished_at(root.path()).unwrap());
    }

    /// A configuration that cannot be read is not joined over: the Secrets stage runs no
    /// vault item and its row is unverified, an unfinished try that changed nothing, so it
    /// is retried at its pace and never counts toward the hold of a failing change. A run an
    /// older build admitted does not read it. Negative control: a read failure reported as
    /// the stage's error.
    #[test]
    fn an_unreadable_vault_configuration_joins_nothing_and_counts_no_failure() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let mut host = Fake {
            configured_unreadable: true,
            pace: Some(Pace {
                build: "1.0.0+gtest".into(),
                marks: "unreadable".into(),
                now: vault::START,
            }),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(host.calls.is_empty(), "{:?}", host.calls);
        let [row] = report.rows.as_slice() else {
            panic!("{:?}", report.rows)
        };
        assert_eq!(row.status, "unverified");
        assert!(
            row.detail
                .starts_with("cannot read which vault this Mac is in: source.toml: no name; "),
            "{row:?}"
        );
        let attempt: Value = aterm_json::from_slice(
            &std::fs::read(root.path().join(secrets_retry::RECORD)).unwrap(),
        )
        .unwrap();
        assert_eq!(attempt["failures"].as_u64(), Some(0), "{attempt:?}");
        assert!(!finished_at(root.path()).unwrap());
        host.pace = None;

        let older = Progress {
            admitted_vault: AdmittedVault::Unknown,
            ..progress(root.path()).unwrap().unwrap()
        };
        save_progress(root.path(), older).unwrap();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Secrets]);
    }

    /// A VAULT TYPED IN SYSTEM STATE STILL MOVES THE MAC. This Mac is in one vault, and the
    /// person typed another in System State and pressed Apply: the run, admitted while this
    /// Mac was in the first, joins the second as asked, keeping the first's settings to put
    /// back. Its record keeps a digest of the first, never its URL. Negative control: a rule
    /// that leaves any vault but the plan's in place joins nothing here.
    #[test]
    fn a_vault_typed_in_system_state_while_in_another_still_moves_the_mac() {
        use vault::{Lan, START, Vaulted, passes};
        let root = aterm_tempfile::tempdir().unwrap();
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut host = Vaulted::new(&lan);
        let admitted = AdmittedVault::of(host.configured_vault());
        assert!(matches!(admitted, AdmittedVault::Configured(_)));
        let plan = selected(&[Setting::Secrets]);
        let revision = restart_in_vault(root.path(), &plan, admitted).unwrap();
        let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
        assert!(record.contains("admitted_vault"), "{record}");
        assert!(!record.contains("vault.example") && !record.contains("fixture-query"));
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0]);
        let log = lan.log();
        assert!(log[0].starts_with("line "), "{log:?}");
        assert!(
            log.iter().any(|event| event.starts_with("keep ")),
            "{log:?}"
        );
        assert!(log.contains(&format!("join {}", plan.vault_url)), "{log:?}");
    }

    /// The plan's own vault is read as before: a run admitted while this Mac was in none,
    /// whose plan names the vault the person then joined from a terminal, runs the vault item,
    /// which reads that vault and starts its subscriber, joining nothing. Negative control: a
    /// rule that leaves every vault configured since in place runs no vault item here.
    #[test]
    fn a_run_whose_plan_names_the_vault_joined_since_reads_it_as_before() {
        use vault::{Lan, START, Vaulted, passes};
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan {
            vault_url: JOINED_SINCE.into(),
            ..selected(&[Setting::Secrets])
        };
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut host = Vaulted::new(&lan);
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0]);
        let log = lan.log();
        assert!(log.iter().any(|event| event == "install"), "{log:?}");
        assert!(
            !log.iter().any(|event| event.starts_with("join ")),
            "{log:?}"
        );
        assert!(rows_ready(&rows), "{rows:?}");
        assert!(finished_at(root.path()).unwrap());
    }

    /// A run an older build admitted, whose record keeps no vault (and which such a build
    /// writes back without one), joins the plan's vault as it did then, whatever vault this
    /// Mac is in. Negative control: a record without the field read as admitted in no vault
    /// leaves the vault in place.
    #[test]
    fn a_run_an_older_build_admitted_joins_as_before() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let older = Progress {
            admitted_vault: AdmittedVault::Unknown,
            ..progress(root.path()).unwrap().unwrap()
        };
        save_progress(root.path(), older).unwrap();
        let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
        assert!(!record.contains("admitted_vault"), "{record}");
        assert_eq!(progress(root.path()).unwrap(), Some(older));
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0]);
        let joined = format!("join {}", selected(&[Setting::Secrets]).vault_url);
        assert!(lan.log().contains(&joined), "{:?}", lan.log());
    }

    /// Every admission keeps the vault its reader finds this Mac in when the run is chosen,
    /// through the entry its public door passes `live_configured_vault`: the startup
    /// review's (as its offer was loaded), System State's Apply and its retried restart, and
    /// a doctor authorization; a stage's progress keeps it after. Negative control: an
    /// admission that drops it keeps none.
    #[test]
    fn every_admission_keeps_the_vault_this_mac_is_in() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        configure_vault(&paths, "http://vault.example:7484/v1/team");
        let read = || ConfiguredVault::read(&paths);
        let vault = AdmittedVault::of(read());
        assert!(matches!(vault, AdmittedVault::Configured(_)));
        let plan = selected(&[Setting::Secrets, Setting::Spotlight]);
        let kept = |root: &Path| progress(root).unwrap().unwrap().admitted_vault;
        for index in 0..4 {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = match index {
                0 => {
                    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                    ticket.offer_reading(root.path(), read).unwrap();
                    ticket.persist_at(root.path(), &plan).result.unwrap()
                }
                1 => Some(restart_reading(root.path(), &plan, read).unwrap()),
                2 => observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                    InitialSetupRestart::default().persist_with(
                        root,
                        &plan,
                        true,
                        read,
                        save_progress,
                        observe_restart,
                    )
                })
                .result
                .unwrap(),
                _ => authorize_reading(root.path(), &plan, read).unwrap(),
            }
            .expect("admitted");
            assert_eq!(kept(root.path()), vault, "admission {index}");
            let mut host = Fake {
                pending: Some(Stage::Secrets),
                ..Fake::default()
            };
            execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
            assert_eq!(host.calls, [Stage::Spotlight, Stage::Secrets]);
            assert_eq!(
                kept(root.path()),
                vault,
                "after a pass of admission {index}"
            );
        }
    }

    /// THE STARTUP REVIEW KEEPS THE VAULT ITS OFFER WAS LOADED IN, not the one at its
    /// capture. Captured in no vault (the review then backs off while System State is busy),
    /// the person joins a vault from a terminal and the offer loads in it: a vault typed in
    /// that offer is a switch they asked for, so the run keeps the vault the offer was loaded
    /// in and its stage joins the typed one. An offer loaded in no vault keeps none, so a
    /// vault joined while it is shown stays. Negative control: a ticket that keeps the vault
    /// of its capture leaves the vault in place of the typed one.
    #[test]
    fn the_startup_review_keeps_the_vault_its_offer_was_loaded_in() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let team = "http://vault.example:7484/v1/team";
        let other = "https://other.example/v1/other";
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        configure_vault(&paths, team);
        let read = || ConfiguredVault::read(&paths);
        let offered = ticket.offer_reading(root.path(), read).unwrap();
        assert_eq!(offered.vault_url, team);
        let typed = Plan {
            vault_url: other.into(),
            ..offered
        };
        let revision = ticket
            .persist_at(root.path(), &typed)
            .result
            .unwrap()
            .expect("admitted");
        assert_eq!(
            progress(root.path()).unwrap().unwrap().admitted_vault,
            AdmittedVault::of(read())
        );
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        assert!(host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
        assert_eq!(host.vault_url.as_deref(), Some(other));

        let root = aterm_tempfile::tempdir().unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let offered = ticket.offer_reading(root.path(), || Ok(None)).unwrap();
        let revision = ticket
            .persist_at(root.path(), &offered)
            .result
            .unwrap()
            .expect("admitted");
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None).unwrap();
        assert!(!host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
        assert!(finished_at(root.path()).unwrap());
    }
}
