// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who gets the profile by itself (`docs/DESIGN-system-state-2026-10-21.md`
//! §3.5). aterm's own Permissions are managed on every Mac; the rest of the
//! profile only on an ALab Mac: a login already in a vault, a Mac in an aterm
//! fleet, or one where the ALab vault answers under the key built into aterm.
//! Elsewhere the automatic plan is Permissions alone, saved as provisional, and
//! a later launch that finds the signal widens it as a new revision. A plan a
//! person shaped is never changed.

use super::*;
use aterm_types::macos_setup::{DIALOG_WAIT, MacosDialog};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[cfg(any(test, debug_assertions))]
mod watch_test;
#[cfg(any(test, debug_assertions))]
pub use watch_test::ProvisionalSetupWatchTest;

/// The Secrets vault row's note when a server answered at the ALab vault's
/// address and holds no ALab vault: there is none on this network.
pub(super) const NO_VAULT: &str = "No vault on this network";

/// The note when nothing answered at the ALab vault's address. That is not proof
/// there is none: macOS's Local Network switch, the network itself, or the
/// vault being down look the same from here.
pub(super) const NOT_REACHED: &str = "The ALab vault did not answer on this network";

/// How long one probe of the ALab vault waits: it answers within a few seconds
/// on its own LAN, and a Mac elsewhere must not hold its first launch longer.
const PROBE_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// What the last probe of the ALab vault in this process heard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VaultHere {
    /// No probe ran in this process.
    Unasked,
    /// The ALab vault answered under the key built into aterm.
    Found,
    /// A server answered at its address and holds no ALab vault.
    Absent,
    /// Nothing answered ([`NOT_REACHED`]).
    Unreached,
}

impl VaultHere {
    /// The Secrets row's note for a plan that names the ALab vault, when the probe
    /// leaves that row unjoined: an absent vault waits for a change of network, the
    /// next launch, a new plan or Check again; an unreached one is asked again after
    /// the Secrets retry pause.
    pub(super) const fn note(self) -> Option<&'static str> {
        match self {
            Self::Absent => Some(NO_VAULT),
            Self::Unreached => Some(NOT_REACHED),
            Self::Unasked | Self::Found => None,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Unasked => 0,
            Self::Found => 1,
            Self::Absent => 2,
            Self::Unreached => 3,
        }
    }

    const fn of(code: u8) -> Self {
        match code {
            1 => Self::Found,
            2 => Self::Absent,
            3 => Self::Unreached,
            _ => Self::Unasked,
        }
    }
}

/// [`VaultHere`] of the last probe in this process.
static LAST_PROBE: AtomicU8 = AtomicU8::new(0);

/// What the last probe of the ALab vault in this process heard.
pub(super) fn vault_here() -> VaultHere {
    VaultHere::of(LAST_PROBE.load(Ordering::Acquire))
}

/// Ask the ALab vault's address once ([`aterm_secrets::client::probe_default`]).
/// An answer from it is also the only proof an app has that macOS lets it use
/// the local network. Its first request can raise macOS's Local Network alert,
/// so the caller holds a [`MacosDialog`] ([`vault_here_now`] takes one).
pub(super) fn probe_vault() -> VaultHere {
    use aterm_secrets::client::ProbeOutcome;
    let here = match aterm_secrets::client::probe_default(PROBE_WAIT) {
        Ok(ProbeOutcome::Found(_)) => {
            access::lan_reached();
            VaultHere::Found
        }
        // Something on this LAN answered at the vault's `.local` address.
        Ok(ProbeOutcome::NotThisVault(_)) => {
            access::lan_reached();
            VaultHere::Absent
        }
        Ok(ProbeOutcome::Absent) => VaultHere::Absent,
        Err(_) => VaultHere::Unreached,
    };
    LAST_PROBE.store(here.code(), Ordering::Release);
    here
}

/// [`probe_vault`] while no other macOS dialog this process started is up (§6:
/// one gesture at a time). One still up past the hang detector leaves the vault
/// unreached for this launch; a provisional plan is probed again on the next.
fn vault_here_now() -> VaultHere {
    let Some(dialog) = MacosDialog::when_none_up(DIALOG_WAIT) else {
        return VaultHere::Unreached;
    };
    // Only this process's first request to the LAN can raise macOS's Local Network alert: a
    // later probe waits out a dialog that is up, but puts none on screen, so it holds no slot
    // (a Debugger Turn On… pressed meanwhile is refused while one is held).
    let _dialog = (vault_here() == VaultHere::Unasked).then_some(dialog);
    probe_vault()
}

/// This login already belongs to a vault, or has asked to join the ALab one
/// under the key built into aterm. A pending request to any other vault is not
/// the ALab signal: anyone can ask.
fn vault_member() -> bool {
    use aterm_secrets::client::{self, Paths};
    Paths::live()
        .ok()
        .and_then(|paths| client::status(&paths).ok())
        .is_some_and(|status| member_of_a_vault(&status))
}

fn member_of_a_vault(status: &aterm_secrets::client::Status) -> bool {
    use aterm_secrets::client::Membership;
    match status.membership {
        Membership::In { .. } | Membership::Plain => true,
        Membership::Pending => status.config.as_ref().is_some_and(|config| {
            config.source
                == aterm_secrets::transport::Source::Url(
                    aterm_types::macos_setup::DEFAULT_VAULT_URL.into(),
                )
                && config.signer.is_some_and(|signer| {
                    aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
                        .is_some_and(|alab| alab == signer)
                })
        }),
        Membership::Unconfigured | Membership::Revoked { .. } => false,
    }
}

/// This Mac holds an aterm fleet's key (`aterm_net::fleet::KEY_FILE` in the
/// state directory, where `aterm fleet init|join` writes it).
fn fleet_member() -> bool {
    aterm_types::dirs::state_dir().is_some_and(|dir| dir.join("fleet.key").is_file())
}

/// Whether this is an ALab Mac (§3.5): local records first, then one bounded
/// probe of the ALab vault. Only a yes is kept for the process: a probe that
/// heard nothing (macOS's Local Network question still open, the network, the
/// vault down) proves nothing, so the next caller asks again.
pub fn alab_mac() -> bool {
    static ALAB: AtomicBool = AtomicBool::new(false);
    kept_if_found(&ALAB, || {
        vault_member() || fleet_member() || vault_here_now() == VaultHere::Found
    })
}

/// `found`, read only until it says yes once.
fn kept_if_found(kept: &AtomicBool, found: impl FnOnce() -> bool) -> bool {
    if kept.load(Ordering::Acquire) {
        return true;
    }
    let found = found();
    if found {
        kept.store(true, Ordering::Release);
    }
    found
}

/// The plan aterm admits by itself where nobody chose one: the whole profile on
/// an ALab Mac, aterm's own permissions everywhere else.
fn scoped(mut plan: Plan, alab: bool) -> Plan {
    if !alab {
        let mut selection = Selection::none();
        selection.set(Setting::Permissions, true);
        plan.selection = selection;
    }
    plan
}

impl InitialSetupAuthorization {
    /// The first launch's offer ([`Self::offer`]) and whether it is provisional (aterm
    /// chose it by itself, so a later launch may widen it). A saved plan is offered as
    /// saved; with none, the plan is scoped by [`alab_mac`], which may probe the network
    /// for a few seconds, so call it off the UI thread.
    pub fn offer_scoped(&mut self) -> Result<(Plan, bool), String> {
        let root = state_root()?;
        let plan = self.offer_reading(&root, live_configured_vault)?;
        if saved_plan_record(&root)?.is_some() {
            return Ok((plan, saved_plan_provisional(&root)?));
        }
        Ok((scoped(plan, alab_mac()), true))
    }
}

/// The plan a provisional `saved` one widens to once this Mac shows the ALab
/// signal, or `None` when it stays: a person-shaped, stopped, cancelled or
/// restored plan, or one that is already the whole profile.
fn upgrade_of(records: &Records, alab: impl FnOnce() -> bool) -> Option<Plan> {
    let Some((plan, true)) = records.plan.as_ref() else {
        return None;
    };
    if !records.provisional
        || records.stopped_run()
        || matches!(records.decision, Decision::Stopped(_))
        || plan.selection == Selection::all()
        || !alab()
    {
        return None;
    }
    Some(Plan {
        selection: Selection::all(),
        vault_url: plan.vault_url.clone(),
    })
}

/// Whether [`upgrade_provisional_setup`] has work: the saved plan is aterm's own
/// provisional one, which it widens once this Mac shows the ALab signal. Reads
/// aterm's records only: no probe. An error when they cannot be read just now (a
/// pass writing them, say) or whose person is not settled yet.
pub fn provisional_setup_widens() -> Result<bool, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(false);
    }
    match person_scope() {
        PersonScope::Refused(_) => return Ok(false),
        PersonScope::Unresolved(why) => return Err(why),
        PersonScope::Person => {}
    }
    widens_at(&state_root()?)
}

fn widens_at(root: &Path) -> Result<bool, String> {
    let records = InitialSetupAuthorization::capture_at(root)?.baseline_records()?;
    Ok(upgrade_of(&records, || true).is_some())
}

/// A provisional plan an [`InitialSetupUpgrade`] widened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SetupUpgrade {
    /// The revision it widened: the one its ticket observed, which the commit
    /// went through only while it was still in force.
    pub from: u64,
    /// The revision it admitted.
    pub to: u64,
}

/// A provisional upgrade retaining its exact admission through a failed publication.
#[derive(Default)]
pub struct InitialSetupUpgrade {
    attempt: Option<(InitialSetupAuthorization, Plan)>,
}

impl InitialSetupUpgrade {
    /// A published prefix still owes this attempt's final admission even though the
    /// expanded plan is no longer eligible to widen.
    pub fn pending(&self) -> bool {
        self.attempt.is_some()
    }

    /// The revision the held attempt widens: the one its ticket observed.
    pub fn widens_from(&self) -> Option<u64> {
        self.attempt.as_ref().map(|(ticket, _)| ticket.revision())
    }

    /// The held attempt's exact published target. A read failure leaves its ownership unknown.
    pub fn published(&self) -> Result<Option<SetupUpgrade>, String> {
        self.published_at(&state_root()?)
    }

    fn published_at(&self, root: &Path) -> Result<Option<SetupUpgrade>, String> {
        match &self.attempt {
            Some((ticket, _)) => ticket.published_upgrade_at(root),
            None => Ok(None),
        }
    }

    /// Retire a definitively unpublished or replaced attempt without creating another revision.
    pub fn forget(&mut self) {
        self.attempt = None;
    }

    /// Widen the provisional plan ([`upgrade_provisional_setup`]), or finish the held
    /// attempt's commit: what it widened, or `None` when nothing changed.
    pub fn persist(&mut self) -> Result<Option<SetupUpgrade>, String> {
        if aterm_types::dirs::runs_under_private_state_root() {
            self.attempt = None;
            return Ok(None);
        }
        match person_scope() {
            PersonScope::Refused(why) => {
                self.attempt = None;
                return Err(why);
            }
            PersonScope::Unresolved(why) => return Err(why),
            PersonScope::Person => {}
        }
        self.persist_with(
            &state_root()?,
            alab_mac,
            live_configured_vault,
            |ticket, full| ticket.upgrade_observed(full).result,
        )
    }

    fn persist_with(
        &mut self,
        root: &Path,
        alab: impl FnOnce() -> bool,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
        admit: impl FnOnce(&mut InitialSetupAuthorization, &Plan) -> Result<Option<u64>, String>,
    ) -> Result<Option<SetupUpgrade>, String> {
        if self.attempt.is_none() {
            self.attempt = prepare_upgrade(root, alab, read)?;
        }
        let Some((ticket, full)) = self.attempt.as_mut() else {
            return Ok(None);
        };
        if ticket.publication_unknown() {
            // Generic authorization is terminal, but a watch must retain the
            // unresolved shutdown debt and must not rebind a later publication.
            return Err("the failed setup publication could not be identified".into());
        }
        let from = ticket.revision();
        let result = admit(ticket, full);
        if result.is_ok() || (!ticket.pending() && !ticket.publication_unknown()) {
            self.attempt = None;
        }
        result.map(|to| to.map(|to| SetupUpgrade { from, to }))
    }
}

/// Widen a provisional plan to the whole profile when this launch finds the ALab
/// signal, as a new revision bound by its own reviewed-state ticket. Returns the
/// admitted revision, or `None` when nothing changed.
pub fn upgrade_provisional_setup() -> Result<Option<u64>, String> {
    InitialSetupUpgrade::default()
        .persist()
        .map(|upgrade| upgrade.map(|upgrade| upgrade.to))
}

fn prepare_upgrade(
    root: &Path,
    alab: impl FnOnce() -> bool,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<Option<(InitialSetupAuthorization, Plan)>, String> {
    let mut ticket = InitialSetupAuthorization::capture_at(root)?;
    let Some(mut full) = upgrade_of(&ticket.baseline_records()?, alab) else {
        return Ok(None);
    };
    // The provisional plan may predate a vault joined from the terminal. Use
    // the same source presentation and replacement authority as a fresh offer;
    // an unreadable current vault must not authorize replacing it unattended.
    let configured = read()?;
    full.vault_url = ticket.offer_reading(root, || Ok(configured))?.vault_url;
    Ok(Some((ticket, full)))
}

/// The unadmitted next run is published atomically with an upgrade's plan.
/// It bridges only the exact predecessor publication's contents; it never
/// authorizes effects, and a physical progress record for this plan wins.
#[derive(Clone, Copy)]
pub(super) struct UpgradeProgress {
    previous: Option<Progress>,
    pub(super) pending: Progress,
}

impl UpgradeProgress {
    pub(super) fn new(previous: Option<Progress>, pending: Progress) -> Self {
        Self { previous, pending }
    }

    pub(super) fn save(self, root: &Path, plan: &Plan) -> Result<(), String> {
        validate_plan(plan)?;
        let mut record = plan_record(plan, true);
        record.as_object_mut().unwrap().insert(
            "upgrade".into(),
            json!({
                "version":1,
                "previous":self.previous.map(progress_checkpoint),
                "pending":progress_checkpoint(self.pending),
            }),
        );
        write_json(&root.join("automatic-plan.json"), &record)
    }

    fn read(value: &Value, plan: &Plan) -> Result<Self, String> {
        if value["version"].as_u64() != Some(1) {
            return Err("unrecognized automatic upgrade recovery".into());
        }
        let previous = match value.get("previous") {
            Some(Value::Null) => None,
            Some(value) => Some(parse_progress(value)?),
            None => return Err("missing automatic upgrade predecessor".into()),
        };
        let pending = parse_progress(&value["pending"])?;
        if plan.selection != Selection::all()
            || !pending.belongs_to(plan)
            || pending.admitted
            || pending.completed
            || pending.stopped
            || pending.running.is_some()
            || pending.revision != next_revision(previous.unwrap_or_default().revision)?
            || pending.done & !previous.unwrap_or_default().done != 0
        {
            return Err("invalid automatic upgrade recovery".into());
        }
        Ok(Self { previous, pending })
    }
}

/// A recovery checkpoint preserves the exact optional access field, even when
/// it names its own revision. Ordinary progress keeps its canonical encoding.
fn progress_checkpoint(progress: Progress) -> Value {
    let mut record = progress_record(progress);
    if let Some(access) = progress.access {
        record
            .as_object_mut()
            .unwrap()
            .insert("access_revision".into(), Value::from(access));
    }
    record
}

/// A fresh admission of the same recovered plan must not discard its bridge
/// before its own progress rename. Different plans never inherit it, and once
/// physical progress catches up normal saves drop the marker.
pub(super) fn keep_upgrade(root: &Path, plan: &Plan, record: &mut Value) -> Result<(), String> {
    let Ok(bytes) = std::fs::read(root.join("automatic-plan.json")) else {
        return Ok(());
    };
    let Ok(existing) = aterm_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    if parse_plan_record(&existing).ok() != Some((plan.clone(), true)) {
        return Ok(());
    }
    let Some(marker) = existing.get("upgrade") else {
        return Ok(());
    };
    let Ok(recovery) = UpgradeProgress::read(marker, plan) else {
        return Ok(());
    };
    if progress_file(root)? == recovery.previous {
        record
            .as_object_mut()
            .unwrap()
            .insert("upgrade".into(), marker.clone());
    }
    Ok(())
}

/// Recover a readable, unadmitted run after the plan's rename but before its
/// standalone progress publication. Only a plan carrying the upgrade marker
/// enables this fallback; any other record returns `physical` as it is.
pub(super) fn upgrade_progress(
    root: &Path,
    physical: Option<Progress>,
) -> Result<Option<Progress>, String> {
    let path = root.join("automatic-plan.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(physical);
    };
    let Ok(record) = aterm_json::from_slice::<Value>(&bytes) else {
        return Ok(physical);
    };
    let Some(marker) = record.get("upgrade") else {
        return Ok(physical);
    };
    let (plan, modern) = parse_plan_record(&record)?;
    let for_plan = physical.filter(|progress| progress.belongs_to(&plan));
    let recovery = match UpgradeProgress::read(marker, &plan) {
        Ok(recovery) => recovery,
        Err(_) if for_plan.is_some() => return Ok(physical),
        Err(why) => return Err(why),
    };
    if !modern {
        if for_plan.is_some() {
            return Ok(physical);
        }
        return Err("an automatic upgrade must have a modern plan".into());
    }
    let request = stop_request_at(root);
    let predecessor_stopped = recovery
        .previous
        .is_some_and(|previous| previous.stopped_by(request));
    // Physical progress wins. A Stop already captured against the predecessor
    // still holds its immediate successor, even if its save ran after the last
    // admission write. It never holds a later, deliberately admitted revision.
    if let Some(mut current) = for_plan {
        if current.revision == recovery.pending.revision
            && !current.completed
            && (predecessor_stopped || current.stopped_by(request))
        {
            current.stopped = true;
            current.running = None;
        }
        return Ok(Some(current));
    }
    // A newer or unrelated progress publication is never replaced.
    if physical != recovery.previous && physical != Some(recovery.pending) {
        return Ok(physical);
    }
    let mut pending = recovery.pending;
    pending.stopped = pending.stopped_by(request)
        || predecessor_stopped
        || matches!(decision_at(root)?, Decision::Stopped(_));
    flush_record_with(&path, durable_name)?;
    Ok(Some(pending))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upgrade_checkpoint_round_trips_every_progress_field() {
        let mut desktop = Selection::none();
        desktop.set(Setting::Desktop, true);
        for access in [None, Some(12), Some(13)] {
            for only in [None, Some(Selection::none()), Some(desktop)] {
                for selection in [None, Some(Selection::none()), Some(Selection::all())] {
                    let progress = Progress {
                        done: Stage::Workstation.bit() | Stage::Permissions.bit(),
                        completed: false,
                        admitted: true,
                        running: Some(Stage::Secrets),
                        plan: [7; 32],
                        revision: 13,
                        stopped: true,
                        access,
                        admitted_vault: AdmittedVault::Configured([9; 32]),
                        only,
                        selection,
                    };
                    assert_eq!(
                        parse_progress(&progress_checkpoint(progress)).unwrap(),
                        progress
                    );
                }
            }
        }
    }

    fn provisional(root: &Path) -> Plan {
        let narrow = scoped(Plan::default(), false);
        authorize_at(root, &narrow).unwrap().unwrap();
        save_plan_as(root, &narrow, true).unwrap();
        let mut history = progress(root).unwrap().unwrap();
        history.done = Stage::Permissions.bit();
        history.completed = true;
        save_progress(root, history).unwrap();
        narrow
    }

    fn configured() -> ConfiguredVault {
        ConfiguredVault {
            config: aterm_secrets::client::Config {
                source: aterm_secrets::transport::Source::Url(
                    "https://reviewed.example/v1/team".into(),
                ),
                name: "team".into(),
                signer: Some([7; 32]),
                followed: Vec::new(),
                host: "test-mac".into(),
            },
            home: PathBuf::from("/test-home"),
        }
    }

    #[test]
    fn a_provisional_upgrade_admits_the_vault_joined_since_the_first_launch() {
        let root = aterm_tempfile::tempdir().unwrap();
        let narrow = provisional(root.path());
        let vault = configured();
        let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(Some(vault.clone())))
            .unwrap()
            .unwrap();
        let revision = ticket.revision();
        assert_eq!(
            ticket.upgrade_at(root.path(), &full).result.unwrap(),
            Some(revision + 1)
        );
        let saved = saved_plan(root.path()).unwrap().unwrap();
        assert_eq!(saved.selection, Selection::all());
        assert_ne!(
            saved.vault_url, narrow.vault_url,
            "the automatic upgrade must not join the default vault over a vault this Mac joined"
        );
        assert_eq!(
            aterm_machines::init_secrets::selected_vault(
                &saved.vault_url,
                &vault.home,
                Some(&vault.config),
            )
            .unwrap(),
            Some(&vault.config),
            "the saved Secrets and Repositories stages must name the configured vault"
        );
        let history = progress(root.path()).unwrap().unwrap();
        assert_eq!(
            history.admitted_vault,
            AdmittedVault::Configured(vault.identity())
        );
        assert_eq!(history.done, Stage::Permissions.bit());
    }

    #[test]
    fn a_provisional_upgrade_without_a_configured_vault_keeps_the_default() {
        let root = aterm_tempfile::tempdir().unwrap();
        let narrow = provisional(root.path());
        let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
            .unwrap()
            .unwrap();
        assert_eq!(full.vault_url, narrow.vault_url);
        assert!(
            ticket
                .upgrade_at(root.path(), &full)
                .result
                .unwrap()
                .is_some()
        );
        assert_eq!(
            progress(root.path()).unwrap().unwrap().admitted_vault,
            AdmittedVault::None,
            "a default-vault admission must still record the observed absence of another vault"
        );
    }

    #[test]
    fn a_prepared_provisional_upgrade_does_not_replace_a_new_publication_or_stop() {
        for stopped in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let narrow = provisional(root.path());
            let (mut ticket, full) =
                prepare_upgrade(root.path(), || true, || Ok(Some(configured())))
                    .unwrap()
                    .unwrap();
            if stopped {
                write_json(
                    &root.path().join("first-run.json"),
                    &json!({"profile":PROFILE,"state":"restored"}),
                )
                .unwrap();
            } else {
                // Equal content under a new publication is still another writer's decision.
                save_plan_as(root.path(), &narrow, true).unwrap();
            }
            let winner = observation_at(root.path()).unwrap();
            assert_eq!(ticket.upgrade_at(root.path(), &full).result.unwrap(), None);
            assert_eq!(observation_at(root.path()).unwrap(), winner);
        }
    }

    #[test]
    fn an_upgrade_requires_a_readable_vault_and_an_eligible_provisional_plan() {
        let root = aterm_tempfile::tempdir().unwrap();
        let narrow = provisional(root.path());
        let before = observation_at(root.path()).unwrap();
        assert!(
            prepare_upgrade(root.path(), || true, || Err("cannot read config".into())).is_err()
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);
        assert!(
            prepare_upgrade(
                root.path(),
                || false,
                || panic!("no upgrade needs no config read")
            )
            .unwrap()
            .is_none()
        );
        save_plan_as(root.path(), &narrow, false).unwrap();
        assert!(
            prepare_upgrade(
                root.path(),
                || true,
                || panic!("a person's plan needs no upgrade")
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn widens_says_whether_an_upgrade_would_widen_the_saved_plan() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert!(!widens_at(root.path()).unwrap(), "no plan saved yet");
        let narrow = provisional(root.path());
        assert!(widens_at(root.path()).unwrap());
        let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
            .unwrap()
            .unwrap();
        ticket
            .upgrade_at(root.path(), &full)
            .result
            .unwrap()
            .unwrap();
        assert!(!widens_at(root.path()).unwrap(), "the whole profile");
        InitialSetupAuthorization::capture_at(root.path())
            .unwrap()
            .persist_at(root.path(), &narrow)
            .result
            .unwrap()
            .expect("the person's narrow plan is durably admitted");
        assert!(!widens_at(root.path()).unwrap(), "a person's plan");

        // A partial publication is unknown, rather than a valid decision not to widen.
        save_plan_as(root.path(), &full, false).unwrap();
        assert!(widens_at(root.path()).is_err());
    }

    /// An upgrade whose commit stops part-way is held, saying the revision it widens, and the
    /// next look finishes that commit, not a fresh one, from the revision it began from.
    #[test]
    fn an_upgrade_that_stops_part_way_is_finished_from_where_it_began() {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional(root.path());
        let mut upgrade = InitialSetupUpgrade::default();
        let stopped = upgrade.persist_with(
            root.path(),
            || true,
            || Ok(None),
            |ticket, full| ticket.persist_for_watch_test(root.path(), full, true, Some((1, false))),
        );
        assert!(stopped.is_err());
        let base = upgrade.widens_from();
        assert!(
            base.is_some() && upgrade.pending(),
            "kept for the next look"
        );
        let finished = upgrade
            .persist_with(
                root.path(),
                || panic!("the held upgrade is finished, not looked for again"),
                || Ok(None),
                |ticket, full| ticket.upgrade_at(root.path(), full).result,
            )
            .unwrap()
            .expect("admitted");
        assert_eq!(Some(finished.from), base, "from the revision it began from");
        assert!(!upgrade.pending() && upgrade.widens_from().is_none());
        let admitted = progress(root.path()).unwrap().unwrap();
        assert_eq!((admitted.revision, admitted.admitted), (finished.to, true));
    }

    #[test]
    fn only_a_yes_is_kept_for_the_process() {
        let kept = AtomicBool::new(false);
        let asked = std::cell::Cell::new(0);
        let ask = |answer: bool| {
            kept_if_found(&kept, || {
                asked.set(asked.get() + 1);
                answer
            })
        };
        assert!(!ask(false));
        assert!(!ask(false), "a probe that heard nothing is asked again");
        assert!(ask(true));
        assert!(ask(false), "kept");
        assert_eq!(asked.get(), 3);
    }

    #[test]
    fn a_mac_outside_alab_manages_only_aterms_own_permissions() {
        let full = scoped(Plan::default(), true);
        assert_eq!(full.selection, Selection::all());
        let elsewhere = scoped(Plan::default(), false);
        assert!(elsewhere.selection.contains(Setting::Permissions));
        for setting in Setting::ALL {
            assert_eq!(
                elsewhere.selection.contains(setting),
                setting == Setting::Permissions,
                "{setting:?}"
            );
        }
        assert_eq!(elsewhere.vault_url, Plan::default().vault_url);
    }

    #[test]
    fn only_a_provisional_plan_widens_and_only_on_the_alab_signal() {
        let narrow = scoped(Plan::default(), false);
        let records = |provisional: bool, decision: Decision| Records {
            plan: Some((narrow.clone(), true)),
            progress: None,
            decision,
            restored: None,
            provisional,
            stop_request: None,
        };
        let widened = upgrade_of(&records(true, Decision::Pending), || true).unwrap();
        assert_eq!(widened.selection, Selection::all());
        assert_eq!(widened.vault_url, narrow.vault_url);
        assert_eq!(
            upgrade_of(&records(true, Decision::Pending), || false),
            None
        );
        assert_eq!(
            upgrade_of(&records(false, Decision::Pending), || true),
            None,
            "a plan a person shaped is never changed"
        );
        assert_eq!(
            upgrade_of(&records(true, Decision::Stopped(Stop::Cancelled)), || true),
            None,
            "Cancel stands"
        );
        let full = Records {
            plan: Some((Plan::default(), true)),
            ..records(true, Decision::Pending)
        };
        assert_eq!(upgrade_of(&full, || true), None);
        let mut stopped = Progress::for_plan(&narrow);
        stopped.admitted = true;
        stopped.stopped = true;
        let stopped = Records {
            progress: Some(stopped),
            ..records(true, Decision::Pending)
        };
        assert_eq!(upgrade_of(&stopped, || true), None, "Stop stands");
        let mut asked = Progress::for_plan(&narrow);
        asked.admitted = true;
        let asked = Records {
            progress: Some(asked),
            stop_request: Some(StopRequest::Revision(asked.revision)),
            ..records(true, Decision::Pending)
        };
        assert_eq!(
            upgrade_of(&asked, || true),
            None,
            "a Stop stands from the moment it is asked for"
        );
    }

    #[test]
    fn only_a_member_or_a_request_to_the_alab_vault_is_the_alab_signal() {
        use aterm_secrets::client::{Config, Membership, Status};
        use aterm_secrets::transport::Source;
        let alab: [u8; 32] = aterm_secrets::b64::decode_padded(aterm_secrets::ALAB_SIGNER)
            .unwrap()
            .try_into()
            .unwrap();
        let status = |membership: Membership, url: &str, signer: Option<[u8; 32]>| Status {
            config: Some(Config {
                source: Source::Url(url.into()),
                name: "alab".into(),
                signer,
                followed: Vec::new(),
                host: "mac".into(),
            }),
            machine: None,
            membership,
            names: Vec::new(),
        };
        let default = aterm_types::macos_setup::DEFAULT_VAULT_URL;
        let other = "https://vault.example/v1/team";
        assert!(member_of_a_vault(&status(Membership::Plain, other, None)));
        assert!(member_of_a_vault(&status(
            Membership::Pending,
            default,
            Some(alab)
        )));
        // Anyone can ask to join some vault, or the ALab address under another key.
        assert!(!member_of_a_vault(&status(
            Membership::Pending,
            other,
            Some(alab)
        )));
        assert!(!member_of_a_vault(&status(
            Membership::Pending,
            default,
            Some([7; 32])
        )));
        assert!(!member_of_a_vault(&status(
            Membership::Pending,
            default,
            None
        )));
        assert!(!member_of_a_vault(&status(
            Membership::Unconfigured,
            default,
            Some(alab)
        )));
    }

    #[test]
    fn a_vault_that_did_not_answer_is_not_called_absent() {
        assert_eq!(VaultHere::Absent.note(), Some(NO_VAULT));
        assert_eq!(VaultHere::Unreached.note(), Some(NOT_REACHED));
        assert_eq!(VaultHere::Found.note(), None);
        assert_eq!(VaultHere::Unasked.note(), None);
        for here in [
            VaultHere::Unasked,
            VaultHere::Found,
            VaultHere::Absent,
            VaultHere::Unreached,
        ] {
            assert_eq!(VaultHere::of(here.code()), here);
        }
    }
}
