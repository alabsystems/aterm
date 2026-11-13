// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who gets the profile by itself (`docs/DESIGN-system-state-2026-10-21.md`
//! §3.5, `docs/DESIGN-init-on-by-default-2026-10-25.md` §8.1). Every Mac syncs the
//! base plan — aterm's own Permissions, the Secrets vault lane (discovery and the
//! Sync rows ride it), Repositories and Rust in Spotlight (this login's rustup docs and aterm's
//! build caches kept out of Spotlight). The rest of the profile
//! — the workstation preferences, Dock and the Messages and FaceTime
//! sign-outs — only on an ALab Mac, which is one whose login opened a locker in the
//! ALab vault under this Mac's own freshly minted recipient key. Elsewhere the
//! automatic plan is the base plan, saved as provisional, and the launch that finds
//! the signal, this one or a later one, widens it to the whole profile as a new
//! revision. A plan the person shaped stays as chosen; a Stop, Cancel or Restore
//! holds automatic expansion too. This scoping never narrows a saved plan.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(any(test, debug_assertions))]
mod watch_test;
#[cfg(any(test, debug_assertions))]
pub use watch_test::ProvisionalSetupWatchTest;

/// The Secrets vault row's note when a server answered at the ALab vault's
/// address and holds no ALab vault: there is none on this network.
pub(super) const NO_VAULT: &str = "No vault on this network";

/// The note when nothing answered at the ALab vault's address. That is not proof
/// there is none: the network itself, a LAN broker not yet installed, or the vault
/// being down look the same from here.
pub(super) const NOT_REACHED: &str = "The ALab vault did not answer on this network";

/// How long one probe of the ALab vault takes at most, all of it: it answers within
/// milliseconds on its own LAN, and a Mac elsewhere must not hold its first launch longer.
const PROBE_WAIT: std::time::Duration = aterm_secrets::client::PROBE_DEADLINE;

/// What a probe of the ALab vault heard, as aterm reads it on this network ([`heard_at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum VaultHere {
    /// No probe ran, or none that ran speaks for this network.
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

    /// Whether it leaves the ALab vault off this network, as [`off_network`] reads a Secrets try:
    /// a server there holds none, or nobody answered. The Repositories stage then runs on this
    /// Mac's own GitHub credentials ([`vault_not_here`]).
    pub(super) const fn not_here(self) -> bool {
        matches!(self, Self::Absent | Self::Unreached)
    }
}

/// What a probe of the ALab vault heard, and the network it heard it on, as a Secrets try keeps
/// the network it ran on ([`secrets_retry::network`]; `None` where it could not be read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Probe {
    pub(super) here: VaultHere,
    pub(super) on: Option<String>,
}

/// The last explicit Check again probe of the ALab vault in this process ([`look_again`]).
static LAST_PROBE: std::sync::Mutex<Option<Probe>> = std::sync::Mutex::new(None);

/// What aterm last heard at the ALab vault's address on the network `reads` reads now, for System
/// State's vault row and the repositories beside it ([`heard_from`] this process's last probe).
pub(super) fn heard_at(root: Option<&Path>, reads: &dyn secrets_retry::Reads) -> VaultHere {
    let probe = LAST_PROBE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    heard_from(probe, root, reads)
}

/// [`heard_at`], with this process's last probe (`probe`): what it heard where it ran on the
/// network `reads` reads now, or else what the last Secrets try of the run in force at `root` kept
/// there ([`secrets_retry::last_rows`], read as [`off_network`] reads it). Each heard only its own
/// network, by the one rule [`secrets_retry::on_network`]: a relaunch inside the Secrets retry
/// pause asks nothing until the pause ends, so that launch has heard only what the record kept,
/// and a run stopped on one network, which asks nothing again by itself, has heard nothing on
/// another until Check again looks there ([`look_again`]), whether aterm reopened there or stayed
/// open; nor has a record of another run.
pub(super) fn heard_from(
    probe: Option<Probe>,
    root: Option<&Path>,
    reads: &dyn secrets_retry::Reads,
) -> VaultHere {
    if let Some(probe) = probe
        && secrets_retry::on_network(probe.on.as_deref(), reads)
    {
        return probe.here;
    }
    root.and_then(|root| secrets_retry::last_rows(root, revision_at(root).ok()?, reads))
        .and_then(|rows| off_network(&rows))
        .unwrap_or(VaultHere::Unasked)
}

/// The mark a worker holding a join password the person typed leaves beside its run's records:
/// the run's revision and the aterm process that holds it ([`holding_join_password`]). Every
/// aterm process sharing the state root reads it ([`join_typed`]); an older aterm reads none, and
/// leaves none.
pub(super) const JOIN_HELD: &str = "join-held.json";

/// Keep whether the worker of run `revision` in `root` holds a join password the person typed
/// (`held`): System State's Join sends one, which the worker keeps for the run's tries until one
/// reaches the vault and spends it, each pass lending it to the Secrets stage or its lane and
/// taking it back. A pass notes it as it begins and as it ends
/// ([`execute_initial_for_revision`]) in a mark naming this process ([`JOIN_HELD`]); a worker
/// holding none takes away its own run's mark only where it names this process, or one that no
/// longer runs, since another aterm sharing the state root may hold a password for the same run
/// and pass between this one's passes. The mark only steers a reading, so one that cannot be
/// written leaves the row as for aterm's own join.
pub(super) fn holding_join_password(root: &Path, revision: u64, held: bool) {
    let own = i32::try_from(std::process::id()).ok();
    let holder = held
        .then_some(own)
        .flatten()
        .and_then(aterm_uds::process::read_identity);
    let marked = join_held(root);
    if let Some(holder) = holder {
        if marked != Some((revision, holder.pid, holder.token())) {
            let _ = keep_join_held(root, revision, &holder);
        }
    } else if let Some((marked, pid, holder)) = marked
        && marked == revision
        // A mark under this pid is this process's, or an ended one's that this pid was given.
        && (own == Some(pid) || !still_runs(pid, &holder))
    {
        let _ = std::fs::remove_file(root.join(JOIN_HELD));
    }
}

/// Mark run `revision` in `root` as holding a join password the person typed, in process
/// `holder` ([`JOIN_HELD`]).
pub(super) fn keep_join_held(
    root: &Path,
    revision: u64,
    holder: &aterm_uds::process::Identity,
) -> Result<(), String> {
    write_json(
        &root.join(JOIN_HELD),
        &json!({"revision": revision, "pid": holder.pid, "holder": holder.token()}),
    )
}

/// The run [`JOIN_HELD`] marks in `root`, and the process holding it: its pid and its
/// [`aterm_uds::process::Identity::token`].
fn join_held(root: &Path) -> Option<(u64, i32, String)> {
    let bytes = crate::macos_doctor::read_record_bytes(&root.join(JOIN_HELD)).ok()?;
    let mark: Value = aterm_json::from_slice(&bytes).ok()?;
    Some((
        mark["revision"].as_u64()?,
        i32::try_from(mark["pid"].as_i64()?).ok()?,
        mark["holder"].as_str()?.to_owned(),
    ))
}

/// Whether the process a mark names still runs: the same pid, born when `holder` says, so a pid
/// macOS gave a later process says nothing.
fn still_runs(pid: i32, holder: &str) -> bool {
    aterm_uds::process::read_identity(pid).is_some_and(|now| now.token() == holder)
}

/// Whether a join password the person typed is held for a try at the vault: the run whose worker
/// held one in `root` as its last pass began or ended ([`JOIN_HELD`]) is the run in force there,
/// has neither finished nor been stopped, and the aterm process that held it still runs, this one
/// or another sharing the state root. Its worker ends on those same two readings
/// ([`revision_at`], [`finished_at`]) and drops the password with it, a process that quit or
/// crashed held none past its end, and a launch holds none until a Join sends one. A join typed in
/// an older aterm leaves no mark, so it reads here as aterm's own, which carries no password and
/// joins a vault only where one answers.
pub(super) fn join_typed(root: &Path) -> bool {
    join_held(root).is_some_and(|(revision, pid, holder)| {
        revision_at(root).is_ok_and(|current| current == revision)
            && finished_at(root).is_ok_and(|finished| !finished)
            && still_runs(pid, &holder)
    })
}

/// Check again's look at the ALab vault ([`super::look_for_the_vault_again`]): [`probe_lan`],
/// which a kept answer never stands for, kept as this process's last probe as the prior look
/// is, so the vault row reads the newest look on this network ([`heard_at`]), a stopped run's
/// too, which asks nothing again by itself.
pub(super) fn look_again() {
    look_again_with(&LAST_PROBE, probe_lan, &secrets_retry::Live);
}

/// [`look_again`]: what `look` heard, kept in `kept` on the network `reads` reads.
pub(super) fn look_again_with(
    kept: &std::sync::Mutex<Option<Probe>>,
    look: impl FnOnce() -> VaultHere,
    reads: &dyn secrets_retry::Reads,
) -> VaultHere {
    keep_probe(kept, look(), reads)
}

/// Keep `here`, what a look at the ALab vault heard, in `kept` with the network `reads` reads as
/// the look ended, as a Secrets try keeps the network it ran on.
fn keep_probe(
    kept: &std::sync::Mutex<Option<Probe>>,
    here: VaultHere,
    reads: &dyn secrets_retry::Reads,
) -> VaultHere {
    let on = secrets_retry::network(reads);
    *kept
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Probe { here, on });
    here
}

/// Where this login keeps its looks for the ALab vault (its home), the network this Mac is on
/// now ([`aterm_secrets::heard::network`]) and the time (unix seconds).
fn look_place() -> Option<(std::path::PathBuf, Option<String>, u64)> {
    let home = aterm_secrets::client::Paths::live().ok()?.home;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    Some((home, aterm_secrets::heard::network_key(), now))
}

/// The vault look, keeping a definitive answer for the next look on this network
/// ([`aterm_machines::init_secrets::keep_look`]); its caller keeps what it heard of the vault
/// ([`LAST_PROBE`]). It is also Check again's look ([`look_again`]), which a kept answer never
/// stands for.
pub(super) fn probe_lan() -> VaultHere {
    use aterm_secrets::client::ProbeOutcome;
    let probed = aterm_secrets::client::probe_default(PROBE_WAIT);
    if let Some((home, network, now)) = look_place() {
        aterm_machines::init_secrets::keep_look(&home, network, now, &probed);
    }
    match probed {
        Ok(ProbeOutcome::Found(_)) => VaultHere::Found,
        // Something on this LAN answered at the vault's `.local` address, or a server there
        // answered with no vault (a 404 or 204), as the join's look reads it.
        Ok(
            ProbeOutcome::NotThisVault(_) | ProbeOutcome::AnotherVault(_) | ProbeOutcome::Absent,
        ) => VaultHere::Absent,
        Err(_) => VaultHere::Unreached,
    }
}

/// This login belongs to the ALab vault: a locker opened under this Mac's own
/// recipient key ([`member_of_the_alab_vault`]).
fn vault_member() -> bool {
    use aterm_secrets::client::{self, Paths};
    Paths::live()
        .ok()
        .and_then(|paths| client::status(&paths).ok())
        .is_some_and(|status| member_of_the_alab_vault(&status))
}

/// Only the ALab vault says a Mac is ALab's (DESIGN-init-on-by-default §8.1): a
/// membership of a vault pinned to its key ([`aterm_secrets::client::alab_vault`]),
/// held through a locker this Mac's own recipient key opens. An admin signed that
/// locker after this Mac existed, so no replayed envelope of another Mac produces
/// it. A pending request to the ALab vault starts a join and nothing more; anyone
/// can run a vault of their own, read a plain file, hold a fleet key, or ask to join
/// some vault, and none of that is a reason to set up a Mac as ALab's or sign it out
/// of Messages and FaceTime.
fn member_of_the_alab_vault(status: &aterm_secrets::client::Status) -> bool {
    aterm_machines::init_secrets::member_of_the_alab_vault(status)
}

/// Whether this is an ALab Mac (§3.5): a membership opened under this Mac's own
/// recipient key in the ALab vault ([`member_of_the_alab_vault`]), kept for the
/// process once seen. Reads this login's secrets records only, with no network
/// probe. Nothing else is the signal: a probe that found the vault's address, a
/// pending request to it, a plain file, another vault, or a fleet key (`aterm fleet
/// init` makes a fleet of anyone's Macs) each start a join at most. The Secrets lane
/// every Mac runs ([`scoped`]) does the join; this gates only the workstation
/// profile and the sign-outs.
pub fn alab_mac() -> bool {
    static ALAB: AtomicBool = AtomicBool::new(false);
    kept_if_found(&ALAB, vault_member)
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

/// The plan aterm admits by itself where nobody chose one: the whole profile on an
/// ALab Mac; elsewhere the base plan every Mac syncs
/// ([`aterm_types::macos_setup::BASE_PLAN`]) — aterm's own Permissions, the Secrets
/// vault lane (the discovery look and the Sync rows ride it) and Rust in Spotlight (this
/// login's rustup docs and aterm's build caches kept out of Spotlight). The workstation
/// preferences, Dock, Repositories and the Messages and FaceTime sign-outs wait for the
/// ALab signal ([`alab_mac`]); the base plan reaches every Mac so sync is on by default
/// (§8.1).
fn scoped(mut plan: Plan, alab: bool) -> Plan {
    if !alab {
        plan.selection = aterm_types::macos_setup::BASE_PLAN;
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

/// The plan aterm's own saved partial plan widens to, or `None` when it stays: the whole profile
/// once this Mac shows the ALab signal (`alab`); otherwise, for a plan an older aterm saved before
/// every Mac got the base plan (releases v0.130.0 to v0.144 saved aterm's permissions alone on a
/// Mac that was not yet an ALab member), the base plan joined to it
/// ([`aterm_types::macos_setup::BASE_PLAN`]), whatever `alab` says, so that Mac syncs and an ALab
/// Mac can join its vault and widen. A plan the person shaped, stopped,
/// cancelled or restored stays as it is (a widen a Stop put back stays held,
/// [`put_back_held_widen`]), and nothing narrows a plan. A Restore's record holds it for as long
/// as it stands ([`Restored`]), whatever was admitted since: widening would set again the groups
/// the Restore put back and sign Messages and FaceTime out, which nothing undoes.
pub(super) fn upgrade_of(records: &Records, alab: impl FnOnce() -> bool) -> Option<Plan> {
    let Some((plan, true)) = records.plan.as_ref() else {
        return None;
    };
    if !aterms_own(records)
        || records.stopped_run()
        || records.stop_request == Some(StopRequest::Unreadable)
        || records.restored.is_some()
        || plan.selection == Selection::all()
    {
        return None;
    }
    widened_plan(plan, alab())
}

/// A saved automatic selection only expands: to the whole profile on the ALab signal, or
/// to include the base plan that every Mac runs. Recovery accepts the same partial expansion.
fn widened_plan(previous: &Plan, alab: bool) -> Option<Plan> {
    let selection = if alab {
        Selection::all()
    } else {
        previous
            .selection
            .union(aterm_types::macos_setup::BASE_PLAN)
    };
    (selection != previous.selection).then(|| Plan {
        selection,
        vault_url: previous.vault_url.clone(),
    })
}

/// Whether a look for the ALab signal has work ([`provisional_setup_widened`]): a saved
/// automatic partial plan can widen once this Mac shows the ALab
/// signal. Reads aterm's records only: no probe. An error when they cannot be read just now (a
/// pass writing them, say) or whose person is not settled yet; a newer aterm's widen nothing.
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

/// A newer aterm's records widen nothing: they are final for this binary, so a watch of them
/// ends rather than read them again ([`NEWER_SETUP`]).
fn widens_at(root: &Path) -> Result<bool, String> {
    if expansion_declined(root)? {
        return Ok(false);
    }
    let records = match watch_ticket(root).and_then(|ticket| ticket.baseline_records()) {
        Ok(records) => records,
        Err(why) if setup_is_newer(&why) => return Ok(false),
        Err(why) => return Err(why),
    };
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

/// A provisional upgrade retaining its exact admission while a retry can finish it.
#[derive(Default)]
pub struct InitialSetupUpgrade {
    attempt: Option<(InitialSetupAuthorization, Plan)>,
    /// The attempt the last persist let go after a write whose publication it could not
    /// identify, kept only for what it published ([`Self::published`]): Stop evidence, never
    /// authority to write or admit again.
    unidentified: Option<InitialSetupAuthorization>,
    /// Said the moment this upgrade's write finds the profile lock another setup holds and
    /// starts waiting for it ([`Self::noting_wait`]).
    waits: Option<Box<dyn FnMut() + Send>>,
}

impl InitialSetupUpgrade {
    /// Whether this upgrade holds an attempt a retry can finish: a write that failed part-way,
    /// its publication identified, which the next persist finishes from where it stopped, or
    /// one that met the profile lock another setup holds and waits for it to let go
    /// ([`Self::persist`]). Every other end lets the attempt go.
    pub fn pending(&self) -> bool {
        self.attempt.is_some()
    }

    /// The revision the held attempt widens: the one its ticket observed.
    pub fn widens_from(&self) -> Option<u64> {
        self.attempt.as_ref().map(|(ticket, _)| ticket.revision())
    }

    /// The held attempt's exact published target, or, once the last persist let an attempt go
    /// after a write it could not identify, the target that attempt reserved for Stop: an
    /// error while that is unknown. A read failure leaves its ownership unknown.
    pub fn published(&self) -> Result<Option<SetupUpgrade>, String> {
        self.published_at(&state_root()?)
    }

    fn published_at(&self, root: &Path) -> Result<Option<SetupUpgrade>, String> {
        match (&self.attempt, &self.unidentified) {
            (Some((ticket, _)), _) | (None, Some(ticket)) => ticket.published_upgrade_at(root),
            (None, None) => Ok(None),
        }
    }

    /// Retire a definitively unpublished or replaced attempt without creating another revision.
    pub fn forget(&mut self) {
        self.attempt = None;
        self.unidentified = None;
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
    /// [`SETUP_BUSY`], its attempt kept. So is one whose write failed part-way. Every other
    /// error lets the attempt go ([`Self::pending`]): a newer aterm's records, which are final
    /// for this binary, records it cannot read before writing, or a write whose publication it
    /// cannot identify, which this binary cannot finish (the next launch settles a publication
    /// cut short; [`Self::published`] keeps what it reserved for Stop).
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
        self.unidentified = None;
        if self.attempt.is_none() {
            self.attempt = prepare_upgrade(root, alab, read)?;
        }
        let Some((ticket, full)) = self.attempt.as_mut() else {
            return Ok(None);
        };
        let result = admit(ticket, full);
        // Read after the admission: one that met records another writer changed meanwhile, and
        // still widened the plan in force, widened their revision.
        let from = ticket.revision();
        // Kept only while a retry can finish it: a write that failed part-way, its publication
        // identified, or one waiting for the profile lock another setup holds. A newer aterm's
        // records are final for this binary, as a Restore is, whatever the ticket says.
        let retry_finishes = match &result {
            Ok(_) => false,
            Err(why) if setup_is_newer(why) => false,
            Err(why) => ticket.write_unfinished() || (why == SETUP_BUSY && ticket.pending()),
        };
        if !retry_finishes {
            // A write this binary cannot identify is never written again, nor rebound to a
            // later publication: the next launch settles its records. Only what it reserved for
            // Stop is kept, as a startup admission's is.
            self.unidentified = self
                .attempt
                .take()
                .map(|(ticket, _)| ticket)
                .filter(InitialSetupAuthorization::publication_unknown);
        }
        result.map(|to| to.map(|to| SetupUpgrade { from, to }))
    }
}

/// The provisional plan in force and the plan it would widen to on this Mac now, saving
/// nothing: the whole profile, in the vault a fresh offer names, once this Mac shows the ALab
/// signal. `None` when the plan stays as it is ([`upgrade_of`]) or no signal is heard yet. The
/// watch then widens it at once ([`InitialSetupUpgrade::persist`]), with no offer and no
/// question (init-on-by-default design §6.1). It may probe the network for a few seconds, so
/// call it off the UI thread.
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

fn prepare_upgrade(
    root: &Path,
    alab: impl FnOnce() -> bool,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<Option<(InitialSetupAuthorization, Plan)>, String> {
    if expansion_declined(root)? {
        return Ok(None);
    }
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
        carry_older_person_access(root)?;
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
        // Only a record of the plan whose run the predecessor is: the plan a put-back
        // restores must be the one that run belongs to. Partial expansions also need this
        // provenance to prove they are the base-plan expansion admitted by upgrade_of.
        let previous_record = value.get("previous_plan");
        let previous_plan = previous_record
            .and_then(|record| parse_plan_record(record).ok())
            .map(|(widened, _)| widened)
            .filter(|widened| previous.is_some_and(|previous| previous.belongs_to(widened)));
        let base_expansion = previous_record
            .is_some_and(|record| record["provisional"].as_bool() == Some(true))
            && previous_plan
                .as_ref()
                .and_then(|previous| widened_plan(previous, false))
                .is_some_and(|mut expected| {
                    // prepare_upgrade presents the configured vault before admission. Only
                    // that captured authority permits its URL to replace the predecessor's.
                    if matches!(pending.admitted_vault, AdmittedVault::Configured(_)) {
                        expected.vault_url.clone_from(&plan.vault_url);
                    }
                    expected == *plan
                });
        if (plan.selection != Selection::all() && !base_expansion)
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
    let Ok(bytes) = crate::macos_doctor::read_record_bytes(&root.join("automatic-plan.json"))
    else {
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
    let Ok(bytes) = crate::macos_doctor::read_record_bytes(&path) else {
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

/// An explicit Stop of an expansion remains a hold across launches.
fn expansion_declined(root: &Path) -> Result<bool, String> {
    let path = root.join("automatic-plan.json");
    match super::super::read_record_bytes(&path) {
        Ok(bytes) => aterm_json::from_slice::<Value>(&bytes)
            .map(|record| put_back(&record))
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
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
    observe_person_mutation(Err(super::super::SETTING_UP.into()), |root| {
        put_back_held_widen(root, revision)
    })
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
    let Ok(bytes) = crate::macos_doctor::read_record_bytes(&root.join("automatic-plan.json"))
    else {
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
    let Ok(bytes) = crate::macos_doctor::read_record_bytes(&root.join("automatic-plan.json"))
    else {
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
#[path = "scope_tests.rs"]
mod tests;
