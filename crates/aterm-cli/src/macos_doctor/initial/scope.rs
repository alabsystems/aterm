// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who gets the profile by itself (`docs/DESIGN-system-state-2026-10-21.md`
//! §3.5). aterm's own Permissions are managed on every Mac; the rest of the
//! profile only on an ALab Mac: a login already in a vault, a Mac in an aterm
//! fleet, or one where the ALab vault answers under the key built into aterm.
//! Elsewhere the automatic plan is Permissions alone, saved as provisional, and
//! the launch that finds the signal, this one or a later one, widens it as a new
//! revision. A plan a person shaped is never changed.

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

/// How long one probe of the ALab vault takes at most, all of it: it answers within
/// milliseconds on its own LAN, and a Mac elsewhere must not hold its first launch longer.
const PROBE_WAIT: std::time::Duration = aterm_secrets::client::PROBE_DEADLINE;

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
/// the local network, and macOS refusing it the `.local` name the only proof it
/// does not: either is kept for later launches ([`access::lan_answered`]). Its
/// first request can raise macOS's Local Network alert, so the caller holds a
/// [`MacosDialog`] ([`vault_here_now`] takes one).
pub(super) fn probe_vault() -> VaultHere {
    let here = probe_lan();
    LAST_PROBE.store(here.code(), Ordering::Release);
    here
}

/// [`probe_vault`]'s look, keeping only what it heard of the LAN: what the last probe of this
/// process heard of the vault ([`vault_here`]) stays as it was. Check again's look
/// ([`super::probe_local_network_again`]).
pub(super) fn probe_lan() -> VaultHere {
    use access::LanAnswer;
    use aterm_secrets::client::ProbeOutcome;
    match aterm_secrets::client::probe_default(PROBE_WAIT) {
        Ok(ProbeOutcome::Found(_)) => {
            access::lan_answered(LanAnswer::Reached);
            VaultHere::Found
        }
        // Something on this LAN answered at the vault's `.local` address.
        Ok(ProbeOutcome::NotThisVault(_) | ProbeOutcome::AnotherVault(_)) => {
            access::lan_answered(LanAnswer::Reached);
            VaultHere::Absent
        }
        Ok(ProbeOutcome::Absent) => VaultHere::Absent,
        Err(why) => {
            if aterm_secrets::client::refused_by_local_network(&why) {
                access::lan_answered(LanAnswer::Refused);
            }
            VaultHere::Unreached
        }
    }
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
    /// saved, one an older aterm saved with aterm's permissions added ([`Self::offer_reading`]);
    /// with none, the plan is scoped by [`alab_mac`], which may probe the network
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
/// restored plan (a widen a Stop put back is the person's, [`put_back_held_widen`]),
/// or one that is already the whole profile. A Restore's record
/// holds it for as long as it stands ([`Restored`]), whatever was admitted since:
/// widening would set again the groups the Restore put back and sign Messages and
/// FaceTime out, which nothing undoes.
pub(super) fn upgrade_of(records: &Records, alab: impl FnOnce() -> bool) -> Option<Plan> {
    let Some((plan, true)) = records.plan.as_ref() else {
        return None;
    };
    if !records.provisional
        || records.stopped_run()
        || matches!(records.decision, Decision::Stopped(_))
        || records.restored.is_some()
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

/// Whether a look for the ALab signal has work ([`provisional_setup_widened`]): the saved
/// plan is aterm's own provisional one, which it offers to widen once this Mac shows the ALab
/// signal. Reads aterm's records only: no probe. An error when they cannot be read just now (a
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
    let records = watch_ticket(root)?.baseline_records()?;
    Ok(upgrade_of(&records, || true).is_some())
}

/// The watch's review of the records: a startup's ([`InitialSetupAuthorization::capture_at`]),
/// refused while the saved plan is ahead of its run. A choice or publication cut short left it
/// there, and the next launch settles it; until then whether it widens is unknown, not no.
fn watch_ticket(root: &Path) -> Result<InitialSetupAuthorization, String> {
    let ticket = InitialSetupAuthorization::capture_at(root)?;
    let records = ticket.baseline_records()?;
    plan_revision(
        records.progress,
        records.plan.as_ref().map(|(plan, _)| plan),
    )?;
    Ok(ticket)
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
    /// Said the moment this upgrade's write finds the profile lock another setup holds and
    /// starts waiting for it ([`Self::noting_wait`]).
    waits: Option<Box<dyn FnMut() + Send>>,
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

    /// Say `waits` the moment this upgrade's write finds the profile lock another setup holds
    /// and starts waiting for it, as a choice does ([`InitialSetupRestart::noting_wait`]):
    /// System State shows the review's Apply waiting from then, never a run going (design
    /// §3.3, §3.5). It replaces any earlier one.
    pub fn noting_wait(&mut self, waits: impl FnMut() + Send + 'static) {
        self.waits = Some(Box::new(waits));
    }

    /// Widen the provisional plan once its review admits it ([`provisional_setup_widened`]), or
    /// finish the held attempt's commit: what it widened, or `None` when nothing changed. It
    /// waits for the profile lock while another setup holds it, at most a choice's wait, and
    /// is then admitted against the records as they are, while it still widens the plan in
    /// force ([`InitialSetupAuthorization::upgrade_observed`]); past that wait it fails with
    /// [`SETUP_BUSY`], its attempt kept.
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
        let mut waits = self.waits.take();
        let persisted = state_root().and_then(|root| {
            self.persist_with(&root, alab_mac, live_configured_vault, |ticket, full| {
                ticket
                    .upgrade_observed(full, || {
                        if let Some(waits) = waits.as_mut() {
                            waits();
                        }
                    })
                    .result
            })
        });
        self.waits = waits;
        persisted
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
        let result = admit(ticket, full);
        // Read after the admission: one that met records another writer changed meanwhile, and
        // still widened the plan in force, widened their revision.
        let from = ticket.revision();
        if result.is_ok() || (!ticket.pending() && !ticket.publication_unknown()) {
            self.attempt = None;
        }
        result.map(|to| to.map(|to| SetupUpgrade { from, to }))
    }
}

/// The provisional plan in force and the plan it would widen to on this Mac now, saving
/// nothing: the whole profile, in the vault a fresh offer names, once this Mac shows the ALab
/// signal. `None` when the plan stays as it is ([`upgrade_of`]) or no signal is heard yet.
/// System State offers it as a review of its own before anything is widened (design §3.5); the
/// review's answer then widens it ([`InitialSetupUpgrade::persist`]) or declines it
/// ([`decline_provisional_setup_widen`]). It may probe the network for a few seconds, so call
/// it off the UI thread.
pub fn provisional_setup_widened() -> Result<Option<(Plan, Plan)>, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(None);
    }
    match person_scope() {
        PersonScope::Refused(_) => return Ok(None),
        PersonScope::Unresolved(why) => return Err(why),
        PersonScope::Person => {}
    }
    widened_at(&state_root()?, alab_mac, live_configured_vault)
}

fn widened_at(
    root: &Path,
    alab: impl FnOnce() -> bool,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<Option<(Plan, Plan)>, String> {
    let Some((ticket, full)) = prepare_upgrade(root, alab, read)? else {
        return Ok(None);
    };
    let in_force = ticket
        .baseline_records()?
        .presented_plan_record()
        .map(|(plan, _)| plan)
        .ok_or("the provisional plan in force is no longer saved")?;
    Ok(Some((in_force, full)))
}

/// A person's Cancel of the review that offered to widen the provisional plan in force
/// ([`provisional_setup_widened`]): that plan becomes theirs, saved as it is and no longer
/// provisional, so no look or launch widens it or offers to again (design §3.5). One write of
/// the plan's record, under the profile lock; its run, finished or not, stays as it was.
/// `Ok(false)` when the plan in force is no longer aterm's provisional one.
pub fn decline_provisional_setup_widen() -> Result<bool, String> {
    observe_person_choice(decline_widen_at).result
}

fn decline_widen_at(root: &Path) -> Result<bool, String> {
    let records = Records::read(root)?;
    let Some((plan, true)) = records.plan else {
        return Ok(false);
    };
    if !records.provisional {
        return Ok(false);
    }
    save_plan_as(root, &plan, false)?;
    Ok(true)
}

fn prepare_upgrade(
    root: &Path,
    alab: impl FnOnce() -> bool,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<Option<(InitialSetupAuthorization, Plan)>, String> {
    let mut ticket = watch_ticket(root)?;
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
#[derive(Clone)]
pub(super) struct UpgradeProgress {
    previous: Option<Progress>,
    /// The plan the upgrade widened, which a Stop that holds the widened run puts
    /// back ([`put_back_held_widen`]). `None` in a marker an older aterm wrote, or
    /// one whose predecessor is not that plan's run.
    previous_plan: Option<Plan>,
    pub(super) pending: Progress,
}

impl UpgradeProgress {
    pub(super) fn new(
        previous_plan: Option<Plan>,
        previous: Option<Progress>,
        pending: Progress,
    ) -> Self {
        Self {
            previous,
            previous_plan,
            pending,
        }
    }

    pub(super) fn save(&self, root: &Path, plan: &Plan) -> Result<(), String> {
        validate_plan(plan)?;
        settle_put_back(root)?;
        let mut record = plan_record(plan, true);
        let mut marker = json!({
            "version":1,
            "previous":self.previous.map(progress_checkpoint),
            "pending":progress_checkpoint(self.pending),
        });
        if let Some(widened) = &self.previous_plan {
            marker
                .as_object_mut()
                .unwrap()
                .insert("previous_plan".into(), plan_record(widened, true));
        }
        record
            .as_object_mut()
            .unwrap()
            .insert("upgrade".into(), marker);
        revision::reserve(root, self.pending.revision)?;
        write_json(&root.join("automatic-plan.json"), &record)
    }

    fn read(root: &Path, value: &Value, plan: &Plan) -> Result<Self, String> {
        known_version(&value["version"], "unrecognized automatic upgrade recovery")?;
        let previous = match value.get("previous") {
            Some(Value::Null) => None,
            Some(value) => Some(parse_progress(value)?),
            None => return Err("missing automatic upgrade predecessor".into()),
        };
        let pending = parse_progress(&value["pending"])?;
        let previous_revision = previous.unwrap_or_default().revision;
        // Older markers allocated exactly after their predecessor. A modern
        // allocation may have a gap after an interrupted reservation or a
        // legacy recovery, proven by its separate durable floor.
        let follows = pending.revision == next_revision(previous_revision)?
            || (pending.revision > previous_revision
                && revision::reserved(root, pending.revision)?);
        if plan.selection != Selection::all()
            || !pending.belongs_to(plan)
            || pending.admitted
            || pending.completed
            || pending.stopped
            || pending.running.is_some()
            || !follows
            || pending.done & !previous.unwrap_or_default().done != 0
        {
            return Err("invalid automatic upgrade recovery".into());
        }
        // Only a record of the plan whose run the predecessor is: the plan a put-back
        // restores must be the one that run belongs to.
        let previous_plan = value
            .get("previous_plan")
            .and_then(|record| parse_plan_record(record).ok())
            .map(|(widened, _)| widened)
            .filter(|widened| previous.is_some_and(|previous| previous.belongs_to(widened)));
        Ok(Self {
            previous,
            previous_plan,
            pending,
        })
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
    let Ok(recovery) = UpgradeProgress::read(root, marker, plan) else {
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
    if let Some(marker) = record.get(PUT_BACK) {
        return put_back_progress(&path, &record, marker, physical);
    }
    let Some(marker) = record.get("upgrade") else {
        return Ok(physical);
    };
    let (plan, modern) = parse_plan_record(&record)?;
    let for_plan = physical.filter(|progress| progress.belongs_to(&plan));
    let recovery = match UpgradeProgress::read(root, marker, &plan) {
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

/// The plan record's key for a put-back's recovery ([`put_back_held_widen`]).
const PUT_BACK: &str = "put_back";

/// Whether the plan's `record` is one a Stop put back ([`put_back_held_widen`]): the person's,
/// never widened again, whatever its `provisional` field says (an earlier build wrote it so).
pub(super) fn put_back(record: &Value) -> bool {
    record.get(PUT_BACK).is_some()
}

/// A widened run a Stop held, and the run of the plan it widened that takes its place.
struct PutBack {
    /// The plan the upgrade widened, which goes back in force as the person's: their Stop
    /// declined the widen, so no later look or launch widens it again ([`upgrade_of`]).
    plan: Plan,
    /// The held run as the put-back found it: its revision and the widened plan's digest
    /// say whether the progress record still holds it ([`Self::owed`]).
    held: Progress,
    /// The run the plan had when it was widened, under the held run's revision, the one
    /// its Stop names: a finished run stays finished and any other stays stopped, with the
    /// access steps it had.
    restored: Progress,
}

impl PutBack {
    /// The plan's record, a person's (never provisional), carrying the run it puts back.
    fn record(&self) -> Value {
        let mut record = plan_record(&self.plan, false);
        record.as_object_mut().unwrap().insert(
            PUT_BACK.into(),
            json!({
                "version":1,
                "held":progress_checkpoint(self.held),
                "restored":progress_checkpoint(self.restored),
            }),
        );
        record
    }

    /// The put-back `marker` of the plan's `record`: a person's plan this build writes, or the
    /// provisional one an earlier build wrote.
    fn read(record: &Value, marker: &Value) -> Result<Self, String> {
        let (plan, _) = parse_plan_record(record)?;
        known_version(&marker["version"], "unrecognized System State put-back")?;
        let held = parse_progress(&marker["held"])?;
        let restored = parse_progress(&marker["restored"])?;
        if !restored.belongs_to(&plan)
            || held.belongs_to(&plan)
            || restored.revision != held.revision
            || restored.running.is_some()
            || !restored.admitted
        {
            return Err("invalid System State put-back".into());
        }
        Ok(Self {
            plan,
            held,
            restored,
        })
    }

    /// Whether the progress record (`physical`) still holds the widened run: the
    /// put-back's own progress write is owed.
    fn owed(&self, physical: Option<Progress>) -> bool {
        physical.is_some_and(|physical| {
            physical.revision == self.held.revision && physical.plan == self.held.plan
        })
    }

    /// The writes after the plan's record: the first-run decision as the plan had it (a
    /// Restore's or a Cancel's stays), then its run, which ends the marker's use. `before`
    /// is asked ahead of each write.
    fn finish(
        &self,
        root: &Path,
        before: &mut impl FnMut(PutBackWrite) -> Result<(), String>,
    ) -> Result<(), String> {
        let applied = mac_settings_kept(&self.plan, self.restored.done);
        if let (decision @ (Decision::Pending | Decision::Applied(_)), None) =
            decision_record(root)?
            && matches!(decision, Decision::Applied(_)) != applied
        {
            before(PutBackWrite::Decision)?;
            save_workstation_decision(root, applied)?;
        }
        before(PutBackWrite::Progress)?;
        save_progress(root, self.restored)
    }
}

/// A put-back's writes, in order ([`put_back_held_widen_with`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PutBackWrite {
    /// The plan's record, carrying the run it puts back.
    Plan,
    /// The first-run decision as the plan had it.
    Decision,
    /// The progress record.
    Progress,
}

/// A Stop held the widened run `revision` as it registered, before its worker took a step
/// (System State's Stop pressed while a watch widened aterm's own plan): put back the plan the
/// upgrade widened (System State design §3.5). The widening started nothing the person
/// stopped, so the page shows what was in force before: that plan and its run as the widen
/// found it, under the stopped revision. The Stop declined the widen, so the plan goes back as
/// the person's: neither the next launch's upgrade nor a watch rearmed by Set up now widens it
/// again, which would reset the Dock and sign out as the person stopped. `false`, with nothing
/// changed, when
/// there is nothing to put back: the run is not that widened run, it finished a step of its
/// own, or a choice or a Restore came since. It takes profile.lock without waiting, so an error
/// when another pass holds it; the caller asks again once that run's worker ends.
pub fn put_back_held_initial_setup(revision: u64) -> Result<bool, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(false);
    }
    observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| put_back_held_widen(root, revision),
    )
    .result
}

/// Under profile.lock: [`put_back_held_initial_setup`]'s put-back of the widened run
/// `revision`, which the caller holds stopped.
///
/// Each write leaves whole records. The plan's record goes first and carries the run it puts
/// back, which [`upgrade_progress`] reads in place of the held run until the progress record
/// says so itself; a writer of the plan's record, and a Stop's fold, first finish what a
/// process that ended between the writes owes ([`settle_put_back`]).
fn put_back_held_widen(root: &Path, revision: u64) -> Result<bool, String> {
    put_back_held_widen_with(root, revision, |_| Ok(()))
}

/// [`put_back_held_widen`], asking `before` ahead of each write: an error ends it there, as a
/// process that ends between two writes does (a test's).
fn put_back_held_widen_with(
    root: &Path,
    revision: u64,
    mut before: impl FnMut(PutBackWrite) -> Result<(), String>,
) -> Result<bool, String> {
    let Some(back) = held_widen(root, revision)? else {
        return Ok(false);
    };
    before(PutBackWrite::Plan)?;
    revision::reserve(root, back.held.revision.max(back.restored.revision))?;
    write_json(&root.join("automatic-plan.json"), &back.record())?;
    back.finish(root, &mut before)?;
    Ok(true)
}

/// What [`put_back_held_widen`] puts back: only for the widened run `revision` itself, still
/// in force, admitted, unfinished and with nothing of its own done, over a plan whose run was
/// admitted, and while no Restore or Cancel holds the decision.
fn held_widen(root: &Path, revision: u64) -> Result<Option<PutBack>, String> {
    let Some(held) = progress(root)? else {
        return Ok(None);
    };
    if held.revision != revision {
        return Ok(None);
    }
    let Ok(bytes) = std::fs::read(root.join("automatic-plan.json")) else {
        return Ok(None);
    };
    let Ok(record) = aterm_json::from_slice::<Value>(&bytes) else {
        return Ok(None);
    };
    let Some(marker) = record.get("upgrade") else {
        return Ok(None);
    };
    let Ok((widened, true)) = parse_plan_record(&record) else {
        return Ok(None);
    };
    let Ok(UpgradeProgress {
        previous: Some(previous),
        previous_plan: Some(plan),
        pending,
    }) = UpgradeProgress::read(root, marker, &widened)
    else {
        return Ok(None);
    };
    let (decision, restored) = decision_record(root)?;
    if record.get("provisional") != Some(&Value::Bool(true))
        || !held.belongs_to(&widened)
        || held.revision != pending.revision
        || !held.admitted
        || held.completed
        || held.done != pending.done
        || !previous.admitted
        || restored.is_some()
        || !matches!(decision, Decision::Pending | Decision::Applied(_))
    {
        return Ok(None);
    }
    let mut run = previous;
    run.revision = held.revision;
    run.access = Some(previous.access_revision());
    run.running = None;
    run.stopped = previous.stopped || !previous.completed;
    Ok(Some(PutBack {
        plan,
        held,
        restored: run,
    }))
}

/// A put-back's plan is in force from its record's rename. Until the run it puts back is
/// published on its own, the progress record still holds the widened run, which reads as
/// the run put back; any other progress is read as it is.
fn put_back_progress(
    path: &Path,
    record: &Value,
    marker: &Value,
    physical: Option<Progress>,
) -> Result<Option<Progress>, String> {
    match PutBack::read(record, marker) {
        Ok(back) if back.owed(physical) => {
            flush_record_with(path, durable_name)?;
            Ok(Some(back.restored))
        }
        Ok(_) => Ok(physical),
        Err(_)
            if physical.is_some_and(|physical| {
                parse_plan_record(record).is_ok_and(|(plan, _)| physical.belongs_to(&plan))
            }) =>
        {
            Ok(physical)
        }
        Err(why) => Err(why),
    }
}

/// Under profile.lock, before the plan's record is replaced or a Stop folded in: finish a
/// put-back that a process ended part-way through ([`put_back_held_widen`]), so the run it
/// puts back is not lost with its marker.
pub(super) fn settle_put_back(root: &Path) -> Result<(), String> {
    let Ok(bytes) = std::fs::read(root.join("automatic-plan.json")) else {
        return Ok(());
    };
    let Ok(record) = aterm_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    let Some(marker) = record.get(PUT_BACK) else {
        return Ok(());
    };
    let Ok(back) = PutBack::read(&record, marker) else {
        return Ok(());
    };
    if back.owed(progress_file(root)?) {
        back.finish(root, &mut |_| Ok(()))?;
    }
    Ok(())
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
                for (selection, continues) in [
                    (None, None),
                    (Some(Selection::none()), Some(11)),
                    (Some(Selection::all()), None),
                ] {
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
                        continues,
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

    /// A prepared upgrade meets records another writer published since it observed them. Over
    /// a Restore it writes nothing; over the same provisional plan published again (a pass, a
    /// resume) it still widens the plan in force, so it is admitted against the records as they
    /// are now (design §3.5).
    #[test]
    fn a_prepared_provisional_upgrade_does_not_replace_a_restore_but_widens_the_same_plan() {
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
                let winner = observation_at(root.path()).unwrap();
                assert_eq!(ticket.upgrade_at(root.path(), &full).result.unwrap(), None);
                assert_eq!(observation_at(root.path()).unwrap(), winner);
            } else {
                save_plan_as(root.path(), &narrow, true).unwrap();
                assert!(
                    ticket
                        .upgrade_at(root.path(), &full)
                        .result
                        .unwrap()
                        .is_some()
                );
                assert_eq!(
                    saved_plan(root.path()).unwrap().unwrap().selection,
                    Selection::all()
                );
            }
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

    /// A look that hears the ALab signal saves nothing: it names the plan in force and the plan
    /// System State's review offers to widen it to (design §3.5). A person's Cancel of that
    /// review keeps the plan in force as it is, theirs for good: no look offers it again, its
    /// run stays as it was, and only its record changes.
    #[test]
    fn a_widen_is_offered_before_anything_widens_and_a_cancel_keeps_the_plan_for_good() {
        let root = aterm_tempfile::tempdir().unwrap();
        let narrow = provisional(root.path());
        let before = observation_at(root.path()).unwrap();
        assert_eq!(
            widened_at(root.path(), || true, || Ok(None)).unwrap(),
            Some((narrow.clone(), Plan::default()))
        );
        assert_eq!(
            widened_at(root.path(), || false, || panic!("no signal reads no vault")).unwrap(),
            None
        );
        assert_eq!(
            observation_at(root.path()).unwrap(),
            before,
            "a look saves nothing"
        );
        let revision = revision_at(root.path()).unwrap();
        let progress_before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        assert!(decline_widen_at(root.path()).unwrap());
        assert_eq!(saved_plan(root.path()).unwrap(), Some(narrow));
        assert!(!widens_at(root.path()).unwrap(), "the person's plan now");
        assert_eq!(widened_at(root.path(), || true, || Ok(None)).unwrap(), None);
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            progress_before,
            "its run stays as it was"
        );
        assert!(
            !decline_widen_at(root.path()).unwrap(),
            "nothing left to decline"
        );
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

    /// The review's Apply meets the profile lock busy past its wait (a retry pass, `aterm doctor
    /// --apply`, a Full Disk Access follow-up), and that pass saves its progress before it lets
    /// go. The held attempt's next look reads the records again: the plan in force is still the
    /// same provisional plan, so the widen is admitted against them, never dropped (design §3.5).
    #[test]
    fn a_widen_apply_that_met_a_busy_lock_is_admitted_over_a_pass_that_saved_meanwhile() {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional(root.path());
        let mut upgrade = InitialSetupUpgrade::default();
        let held = HeldLock::try_take(root.path()).unwrap().unwrap();
        let first = upgrade.persist_with(
            root.path(),
            || true,
            || Ok(None),
            |ticket, full| ticket.upgrade_at(root.path(), full).result,
        );
        assert_eq!(first, Err("busy".to_string()));
        assert!(upgrade.pending(), "the busy look keeps the attempt");
        // The pass that held the lock saves its progress (any write: a new publication).
        let history = progress(root.path()).unwrap().unwrap();
        save_progress(root.path(), history).unwrap();
        drop(held);
        let second = upgrade
            .persist_with(
                root.path(),
                || panic!("the held attempt is finished, not looked for again"),
                || Ok(None),
                |ticket, full| ticket.upgrade_at(root.path(), full).result,
            )
            .unwrap()
            .expect("the review's Apply is admitted");
        assert_eq!(second.from, history.revision);
        assert!(!upgrade.pending());
        assert_eq!(
            saved_plan(root.path()).unwrap().unwrap().selection,
            Selection::all()
        );
        let admitted = progress(root.path()).unwrap().unwrap();
        assert_eq!((admitted.revision, admitted.admitted), (second.to, true));
        assert!(!widens_at(root.path()).unwrap(), "nothing left to widen");
    }

    /// The same busy look, but the writer that held the lock changed the plan in force: a Stop of
    /// its run, the person's Cancel of the widen, a Restore, or another plan. The widen no longer
    /// applies to the plan in force, so the next look writes nothing and ends the attempt; the
    /// watch then refuses the review's Apply and the page shows the plan in force.
    #[test]
    fn a_widen_apply_that_met_a_busy_lock_is_not_admitted_over_a_changed_plan() {
        for change in ["stop", "cancel", "restore", "another plan"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let narrow = provisional(root.path());
            if change == "stop" {
                let mut unfinished = progress(root.path()).unwrap().unwrap();
                unfinished.done = 0;
                unfinished.completed = false;
                save_progress(root.path(), unfinished).unwrap();
            }
            let mut upgrade = InitialSetupUpgrade::default();
            let held = HeldLock::try_take(root.path()).unwrap().unwrap();
            let first = upgrade.persist_with(
                root.path(),
                || true,
                || Ok(None),
                |ticket, full| ticket.upgrade_at(root.path(), full).result,
            );
            assert_eq!(first, Err("busy".to_string()), "{change}");
            match change {
                "stop" => {
                    let revision = progress(root.path()).unwrap().unwrap().revision;
                    request_stop_at(root.path(), revision).unwrap();
                }
                "cancel" => assert!(decline_widen_at(root.path()).unwrap()),
                "restore" => write_json(
                    &root.path().join("first-run.json"),
                    &json!({"profile":PROFILE,"state":"restored"}),
                )
                .unwrap(),
                _ => {
                    let mut other = narrow.clone();
                    other.selection.set(Setting::Spotlight, true);
                    save_plan_as(root.path(), &other, true).unwrap();
                }
            }
            drop(held);
            let changed = observation_at(root.path()).unwrap();
            let second = upgrade.persist_with(
                root.path(),
                || panic!("the held attempt is finished, not looked for again"),
                || Ok(None),
                |ticket, full| ticket.upgrade_at(root.path(), full).result,
            );
            assert_eq!(second, Ok(None), "{change}");
            assert!(!upgrade.pending(), "{change}: the attempt ends");
            assert!(
                observation_at(root.path()).unwrap() == changed,
                "{change}: nothing written"
            );
        }
    }

    /// The review's Apply waits for the profile lock another setup holds, as a choice does, and
    /// says so the moment it starts waiting; once that setup lets go, having saved its progress,
    /// the widen is admitted in the same press. One whose wait runs out is refused busy, its
    /// ticket kept for the next look.
    #[test]
    fn a_widen_apply_waits_for_the_profile_lock_and_says_so() {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional(root.path());
        let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
            .unwrap()
            .unwrap();
        let mut held = Some(HeldLock::try_take(root.path()).unwrap().unwrap());
        let mut noted = 0;
        let admitted = ticket.upgrade_waiting(
            root.path(),
            None,
            noting_first(
                || noted += 1,
                || {
                    if held.is_some() {
                        let history = progress(root.path()).unwrap().unwrap();
                        save_progress(root.path(), history).unwrap();
                        held = None;
                    }
                    true
                },
            ),
            &full,
        );
        assert!(
            admitted.result.unwrap().is_some(),
            "admitted once the lock was free"
        );
        assert_eq!(noted, 1, "said once, as it started waiting");
        assert_eq!(
            saved_plan(root.path()).unwrap().unwrap().selection,
            Selection::all()
        );

        let root = aterm_tempfile::tempdir().unwrap();
        provisional(root.path());
        let (mut ticket, full) = prepare_upgrade(root.path(), || true, || Ok(None))
            .unwrap()
            .unwrap();
        let _held = HeldLock::try_take(root.path()).unwrap().unwrap();
        let refused = ticket.upgrade_waiting(root.path(), None, || false, &full);
        assert_eq!(refused.result, Err(SETUP_BUSY.to_string()));
        assert!(ticket.pending(), "kept for the next look");
    }

    /// A Stop that came during the last stage of aterm's provisional run was spent: the run
    /// completed, and its records hold no Stop. They still let the plan widen, so the next
    /// launch may offer the widen review; in the launch that pressed Stop the watch offers none
    /// (`provisional_setup_widens_here` in the binary).
    #[test]
    fn a_stop_spent_on_a_finished_provisional_run_leaves_its_widen_to_a_later_launch() {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional(root.path());
        let revision = progress(root.path()).unwrap().unwrap().revision;
        super::super::request_stop_at(root.path(), revision).unwrap();
        assert!(
            widens_at(root.path()).unwrap(),
            "the finished run's spent Stop holds no widening on record"
        );
    }

    /// aterm's provisional plan, its run `finished` or not, widened on the ALab signal, and a
    /// Stop asked for the widened run: the plan, its run as the widen found it, and the
    /// widened revision.
    fn widened_and_stopped(root: &Path, finished: bool) -> (Plan, Progress, u64) {
        let narrow = provisional(root);
        if !finished {
            let mut unfinished = progress(root).unwrap().unwrap();
            unfinished.done = 0;
            unfinished.completed = false;
            save_progress(root, unfinished).unwrap();
        }
        let before = progress(root).unwrap().unwrap();
        let (mut ticket, full) = prepare_upgrade(root, || true, || Ok(None))
            .unwrap()
            .unwrap();
        let widened = ticket.upgrade_at(root, &full).result.unwrap().unwrap();
        request_stop_at(root, widened).unwrap();
        (narrow, before, widened)
    }

    /// A Stop that holds the run a widen admitted, before it took a step, puts back the plan in
    /// force before the widen, with its run as it was, so the page turns no more groups to Set
    /// up (design §3.5); the Stop declined the widen, so the plan is the person's from then on.
    #[test]
    fn a_stop_does_not_turn_more_groups_to_set_up() {
        for finished in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let (narrow, before, widened) = widened_and_stopped(root.path(), finished);
            assert!(
                !put_back_held_widen(root.path(), widened - 1).unwrap(),
                "only the widened run"
            );
            assert!(put_back_held_widen(root.path(), widened).unwrap());
            assert_eq!(
                saved_plan_record(root.path()).unwrap(),
                Some((narrow.clone(), true))
            );
            assert!(
                !saved_plan_provisional(root.path()).unwrap(),
                "the person's: their Stop declined the widen"
            );
            let run = progress_file(root.path()).unwrap().unwrap();
            assert_eq!(progress(root.path()).unwrap(), Some(run));
            assert!(run.belongs_to(&narrow));
            assert_eq!(
                (run.revision, run.done, run.completed, run.access),
                (
                    widened,
                    before.done,
                    finished,
                    Some(before.access_revision())
                ),
                "the run as the widen found it, under the revision the Stop names"
            );
            assert_eq!(run.stopped, !finished, "an unfinished run stays stopped");
            assert_eq!(
                decision_at(root.path()).unwrap(),
                Decision::Applied(true),
                "Permissions alone owes no Mac settings"
            );
            assert!(finished_at(root.path()).unwrap());
            assert!(!widens_at(root.path()).unwrap(), "nothing widens it again");
            fold_stop_request(root.path()).unwrap();
            assert_eq!(stop_request_at(root.path()), None, "the Stop is spent");
            assert_eq!(progress(root.path()).unwrap(), Some(run));
            assert!(
                !put_back_held_widen(root.path(), widened).unwrap(),
                "nothing more to put back"
            );
        }
    }

    /// A Stop of the widened run declines the widen: once the plan is put back, the next
    /// launch's upgrade finds nothing to widen, so the Dock reset and the sign-outs the person
    /// stopped never run (design §3.5).
    #[test]
    fn a_stopped_widen_is_never_widened_again_by_a_later_launch() {
        for finished in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let (narrow, _, widened) = widened_and_stopped(root.path(), finished);
            assert!(put_back_held_widen(root.path(), widened).unwrap());
            // Relaunch: the Stop is spent on the run it held, and the launch looks again.
            fold_stop_request(root.path()).unwrap();
            assert!(
                prepare_upgrade(root.path(), || true, || Ok(None))
                    .unwrap()
                    .is_none(),
                "finished: {finished}"
            );
            assert_eq!(saved_plan(root.path()).unwrap(), Some(narrow));
        }
    }

    /// A put-back an earlier build saved provisional is the person's all the same: the Stop
    /// declined the widen, so once its run is finished no later look widens it again.
    #[test]
    fn an_earlier_builds_provisional_put_back_is_never_widened_again() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (_, _, widened) = widened_and_stopped(root.path(), true);
        assert!(put_back_held_widen(root.path(), widened).unwrap());
        let path = root.path().join("automatic-plan.json");
        let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record
            .as_object_mut()
            .unwrap()
            .insert("provisional".into(), Value::Bool(true));
        write_json(&path, &record).unwrap();
        fold_stop_request(root.path()).unwrap();
        assert!(
            prepare_upgrade(root.path(), || true, || Ok(None))
                .unwrap()
                .is_none(),
            "the plan a Stop put back is widened again"
        );
    }

    /// Set up now on the plan a Stop put back runs it as it stands, and the plan stays the
    /// person's: the watch Set up now rearms finds nothing to widen.
    #[test]
    fn set_up_now_after_a_stopped_widen_leaves_nothing_to_widen() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (narrow, _, widened) = widened_and_stopped(root.path(), false);
        assert!(put_back_held_widen(root.path(), widened).unwrap());
        let shown = snapshot_at(root.path(), narrow.clone()).unwrap();
        assert_eq!(shown.plan, narrow, "the page shows the plan put back");
        assert!(!shown.provisional, "as the person's");
        observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            InitialSetupRestart::default()
                .changing(Some(Selection::none()))
                .persist_with(
                    root,
                    &shown.plan,
                    false,
                    || Ok(None),
                    save_progress,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
        })
        .result
        .unwrap()
        .expect("Set up now admitted");
        assert!(!widens_at(root.path()).unwrap());
    }

    /// A widened run that finished a step of its own, or that a choice replaced, keeps what
    /// is in force.
    #[test]
    fn only_a_widened_run_that_did_nothing_is_put_back() {
        let root = aterm_tempfile::tempdir().unwrap();
        let (_, _, widened) = widened_and_stopped(root.path(), true);
        let mut ran = progress(root.path()).unwrap().unwrap();
        ran.done |= Stage::Workstation.bit();
        save_progress(root.path(), ran).unwrap();
        assert!(
            !put_back_held_widen(root.path(), widened).unwrap(),
            "it set something"
        );
        assert_eq!(
            saved_plan(root.path()).unwrap().unwrap().selection,
            Selection::all()
        );
        let root = aterm_tempfile::tempdir().unwrap();
        let (narrow, _, widened) = widened_and_stopped(root.path(), true);
        // A person's choice of that plan (Set up now or a group press after the Stop).
        let chosen = restart_at(root.path(), &narrow).unwrap();
        assert!(
            !put_back_held_widen(root.path(), widened).unwrap(),
            "a choice came since"
        );
        assert!(
            !put_back_held_widen(root.path(), chosen).unwrap(),
            "the person's run"
        );
        assert!(!saved_plan_provisional(root.path()).unwrap());
    }

    /// Each write of a put-back leaves whole records: cut short before its plan's record, the
    /// widened run stands stopped; after it, the plan put back is in force with its run, and
    /// the next writer of the plan's record, or of a Stop, finishes what was owed first.
    #[test]
    fn a_put_back_cut_short_at_any_write_leaves_whole_records() {
        let writers: [fn(&Path, &Plan); 2] = [
            |root, plan| save_plan_as(root, plan, true).unwrap(),
            |root, _| fold_stop_request(root).unwrap(),
        ];
        for cut in [
            PutBackWrite::Plan,
            PutBackWrite::Decision,
            PutBackWrite::Progress,
        ] {
            for writer in writers {
                let root = aterm_tempfile::tempdir().unwrap();
                let (narrow, before, widened) = widened_and_stopped(root.path(), true);
                let held = progress(root.path()).unwrap().unwrap();
                let ended = put_back_held_widen_with(root.path(), widened, |write| {
                    if write == cut {
                        Err("the process ended".into())
                    } else {
                        Ok(())
                    }
                });
                assert!(ended.is_err(), "{cut:?}");
                let records = Records::read(root.path()).unwrap();
                assert!(
                    finished_at(root.path()).unwrap(),
                    "{cut:?}: nothing resumes"
                );
                if cut == PutBackWrite::Plan {
                    assert_eq!(
                        records.plan.as_ref().map(|(plan, _)| plan.selection),
                        Some(Selection::all())
                    );
                    assert_eq!(records.progress, Some(held));
                    assert!(records.stopped_run());
                    continue;
                }
                assert_eq!(records.plan, Some((narrow.clone(), true)), "{cut:?}");
                let run = records.progress.unwrap();
                assert!(run.belongs_to(&narrow), "{cut:?}");
                assert_eq!(
                    (run.revision, run.done, run.completed),
                    (widened, before.done, true)
                );
                assert_eq!(revision_at(root.path()).unwrap(), widened);
                writer(root.path(), &narrow);
                assert_eq!(progress_file(root.path()).unwrap(), Some(run), "{cut:?}");
                assert_eq!(
                    decision_at(root.path()).unwrap(),
                    Decision::Applied(true),
                    "{cut:?}"
                );
            }
        }
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
            cancelled: None,
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

    /// A Restore makes the plan a person's (design §3.4), so no later launch widens it over the
    /// groups the Restore put back, even where it left aterm's own provisional plan of
    /// Permissions alone as it was; and Set up now from the page afterwards keeps it so. Widening
    /// would set Keyboard & Trackpad and Desktop & Dock again and sign Messages and FaceTime out,
    /// which nothing undoes.
    #[test]
    fn a_restored_provisional_plan_is_never_widened_after_set_up_now() {
        let root = aterm_tempfile::tempdir().unwrap();
        let narrow = provisional(root.path());
        assert!(widens_at(root.path()).unwrap(), "aterm's own plan widens");
        // Restore previous settings (macos_doctor::restore's two writes).
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        assert!(
            !saved_plan_provisional(root.path()).unwrap(),
            "the Restore saved the plan as a person's"
        );
        assert!(!widens_at(root.path()).unwrap(), "a Restore holds it");
        // Set up now, from the page that shows the plan in force.
        let shown = snapshot_at(root.path(), narrow.clone()).unwrap().plan;
        observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            InitialSetupRestart::default()
                .changing(Some(Selection::none()))
                .persist_with(
                    root,
                    &shown,
                    false,
                    || Ok(None),
                    save_progress,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
        })
        .result
        .unwrap()
        .expect("admitted");
        assert!(
            !widens_at(root.path()).unwrap(),
            "after a Restore and Set up now the plan is still a person's: {}",
            std::fs::read_to_string(root.path().join("automatic-plan.json")).unwrap()
        );
    }

    /// A Restore's record holds the ALab upgrade for as long as it stands, whatever decision an
    /// admission since wrote over it: a provisional plan beside it never widens.
    #[test]
    fn a_restore_record_holds_the_alab_upgrade_while_it_stands() {
        let root = aterm_tempfile::tempdir().unwrap();
        provisional(root.path());
        assert!(widens_at(root.path()).unwrap());
        for state in [RestoredState::Pending, RestoredState::Applied] {
            save_restored_decision(root.path(), state, Restored::at_restore(None)).unwrap();
            assert!(saved_plan_provisional(root.path()).unwrap());
            assert!(!widens_at(root.path()).unwrap(), "{state:?}");
        }
    }
}
