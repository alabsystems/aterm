// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's side of System State's Permissions group
//! (`docs/DESIGN-system-state-2026-10-21.md` §3.6, §6): the access steps of an
//! approved setup revision, which macOS gesture is up, a press on a Needs-you
//! row, and the one quiet signal in aterm's menu-bar menu.
//!
//! * The admitted plan's access revision ([`App::access_revision`]; a change to another group
//!   keeps it) is the standing yes. It runs, once per revision, the Local Network trigger, the
//!   Full Disk Access page, the Developer Tools page (once Full Disk Access reads on), the
//!   debugger (whose dialog comes once per Mac), and the folders (only while Full Disk Access is
//!   off). Each but the debugger brings up only macOS's own question, so any admitted plan runs
//!   it; the debugger step, an administrator-authorized change aterm starts, needs this
//!   window's own admission (`App::admitted_access`).
//! * A step puts something on screen only at [`App::access_step_moment`], one gesture at a
//!   time; while one is up the window holds a [`MacosDialog`], so the setup worker's own
//!   prompts wait too.
//! * A panel's Not now holds the window's page, and a control client's act on its row, for
//!   the rest of its revision (`devtools_helper::declined_for`). A new revision, even one
//!   approved while aterm runs ([`App::rearm_access_steps`]), asks again.
//! * A Needs-you row's press opens its page past any Not now. A control client's act never
//!   mints the yes, and its pages wait for the step moment
//!   ([`App::open_access_from_system_state`]).
//! * Each step appends `<revision> <row> <outcome>` to `<state>/system-state-access`
//!   ([`aterm_types::macos_setup::access_record`]), which System State reads.
//! * The menu line ([`menu_line`]) is the only signal posted; the digest entries it replaced
//!   are withdrawn ([`RETIRED_KEYS`]).
//! * A `[privacy]` switch off, and a debugger the person turned off, are told to the setup
//!   worker as the row's wanted state ([`App::publish_access_opt_outs`]).

#[cfg(any(target_os = "macos", test))]
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use aterm_types::macos_setup::opt_out::OptOut;
use aterm_types::macos_setup::{
    Access, AccessOutcome, MacosDialog, Selection, Setting, Standing, Subject,
};

use crate::status_item::SystemStateLine;
use crate::{App, SetupStatus, devtools_helper};

/// How long the Local Network alert counts as up after the trigger. macOS reports neither the
/// alert nor its answer, so this is a bound, not a reading: the next step waits this long, and
/// an alert still unanswered after it may have System Settings beside it.
pub(crate) const LOCAL_NETWORK_ALERT_HOLD: Duration = Duration::from_secs(30);

/// How long a Local Network trigger that found no LAN to name waits before it
/// looks again (the Wi-Fi may join after launch).
const LOCAL_NETWORK_RETRY: Duration = Duration::from_secs(60);

/// How long a System Settings pane aterm opened with no panel of its own beside it (Local
/// Network, Accessibility) counts as up: macOS says nothing when it closes.
const PANE_HOLD: Duration = Duration::from_secs(30);

/// The port the Local Network trigger connects a UDP socket to: discard. A UDP
/// connect sends nothing; it only names the peer, which is what macOS asks
/// about.
const LOCAL_NETWORK_PORT: u16 = 9;

/// The Permissions rows an opt-out can settle ([`App::access_opt_outs`]).
/// Accessibility is not one: only the sign-outs need it, and they say so.
const OPT_OUT_ROWS: [Access; 4] = [
    Access::FullDiskAccess,
    Access::DeveloperTools,
    Access::Debugger,
    Access::LocalNetwork,
];

/// A Permissions row's description while a `[privacy]` switch is off.
pub(crate) const PRIVACY_OFF: &str =
    "You turned aterm's privacy steps off in aterm.toml ([privacy]).";

/// The Debugger row's description once its off reading is the person's choice, published as
/// [`OptOut::WhileOff`] so it shows only while the row reads off.
pub(crate) const DEBUGGER_OFF: &str = "You turned debugger access off, so aterm leaves it off.";

/// Digest keys System State replaced (§4): the Full Disk Access question and its regression,
/// the Developer Tools line, the workstation lines and the vault's join and sync lines. An
/// entry an older aterm left open under one is withdrawn once per process, never ended as
/// fixed, since nothing here read its cause end.
pub(crate) const RETIRED_KEYS: [&str; 8] = [
    "privacy.fda.needed",
    "privacy.fda.regressed",
    "privacy.devtools.not-granted",
    "privacy.workstation.settings-incomplete",
    "privacy.workstation.account-signout",
    "privacy.secrets.join-failed",
    "privacy.secrets.join-unfinished",
    "privacy.secrets.not-synced",
];

/// The vault join's waiting entry, which carries the approval code an admin Mac needs: it ends
/// only once a status has read the Secrets row.
pub(crate) const JOIN_PENDING_KEY: &str = "privacy.secrets.join-pending";

/// What the access steps reach outside the App, injected so the steps are
/// tested without a LAN, a state directory or the process-wide dialog count.
#[derive(Clone, Copy)]
pub(crate) struct StepsReach {
    /// Raise macOS's Local Network question: `true` when a LAN peer was named.
    pub(crate) trigger_local_network: fn() -> bool,
    /// Open a named access pane; injected so headless fixtures reach no OS API.
    pub(crate) open_pane: fn(&App, Access) -> bool,
    /// Append one step's outcome to the record.
    pub(crate) record: fn(u64, Access, AccessOutcome),
    /// The latest outcome recorded for a row in a revision.
    pub(crate) recorded: fn(u64, Access) -> Option<AccessOutcome>,
    /// Whether System State is the visible view of a focused window.
    pub(crate) frontmost: fn(&App) -> bool,
    /// The quiet-moment rules ([`App::devtools_moment_is_quiet`]).
    pub(crate) quiet: fn(&App, Instant) -> bool,
    /// Whether any macOS dialog this process started is up
    /// ([`aterm_types::macos_setup::macos_dialog_up`]).
    pub(crate) dialog_up: fn() -> bool,
    /// Whether the window holds a [`MacosDialog`] while its own gesture is up
    /// (a live windowed instance only: the count is process-wide).
    pub(crate) holds_dialog: bool,
}

impl std::fmt::Debug for StepsReach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StepsReach")
            .field("holds_dialog", &self.holds_dialog)
            .finish_non_exhaustive()
    }
}

impl StepsReach {
    /// Nothing reached: no trigger, no record, never frontmost, no dialog.
    pub(crate) const INERT: Self = Self {
        trigger_local_network: || false,
        open_pane: |_, _| false,
        record: |_, _, _| {},
        recorded: |_, _| None,
        frontmost: |_| false,
        quiet: |_, _| false,
        dialog_up: || false,
        holds_dialog: false,
    };

    /// The live arms of a windowed macOS instance.
    pub(crate) fn live() -> Self {
        Self {
            trigger_local_network: live_trigger_local_network,
            open_pane: live_open_access_pane,
            record: live_record,
            recorded: live_recorded,
            frontmost: App::system_state_frontmost,
            quiet: App::devtools_moment_is_quiet,
            dialog_up: aterm_types::macos_setup::macos_dialog_up,
            holds_dialog: true,
        }
    }
}

/// The steps' own state in the App.
pub(crate) struct AccessSteps {
    reach: StepsReach,
    /// The revision whose Local Network step this process finished (triggered,
    /// found recorded, or found no LAN to name).
    local_network_done: Option<u64>,
    /// The revision whose record this process read for Local Network.
    local_network_read: Option<u64>,
    /// Until when the Local Network alert counts as up.
    local_network_until: Option<Instant>,
    /// The revision whose Local Network trigger found nothing to connect to (no LAN yet, or no
    /// socket), and when to try it again.
    local_network_retry: Option<(u64, Instant)>,
    /// The opt-outs last told to the setup worker ([`App::publish_access_opt_outs`]),
    /// in [`OPT_OUT_ROWS`] order; `None` before the first.
    opt_outs: Option<[Option<OptOut>; 4]>,
    /// A control client's act on the Local Network or Accessibility row, held
    /// for the step moment, and when it was asked (it lapses after
    /// [`devtools_helper::ASKED_LAPSE`]).
    pane_act: Option<(Access, Instant)>,
    /// Until when a pane aterm opened for one of those rows counts as up.
    pane_until: Option<Instant>,
    /// A verified Full Disk Access grant reads off (§3.6): the menu line names it,
    /// unless the approved revision's Full Disk Access step is declined
    /// ([`App::fda_declined_in_revision`]).
    fda_regressed: bool,
    /// Whether the window's own Full Disk Access step is declined under the
    /// marker key it was read for (the approved revision's): read once per key,
    /// and again after a decline this process notes.
    fda_declined: Option<(String, bool)>,
    /// Held while one of the window's own macOS gestures is up.
    dialog: Option<MacosDialog>,
    /// The menu line as last composed.
    line: Option<SystemStateLine>,
    /// Outcomes this process recorded, so a repeated reading writes one line.
    recorded: Vec<(u64, Access, AccessOutcome)>,
    /// The retired digest keys were ended since this process launched, or since
    /// it adopted its parent's entries at Commit ([`App::rearm_retired_keys`]).
    retired: bool,
    /// The vault join's waiting entry was ended since then ([`JOIN_PENDING_KEY`]).
    join_pending_retired: bool,
    /// The launch folder pass's gate while it walks: shut, so the pass parks between folders,
    /// while System State is not frontmost or a `[privacy]` switch that started it is off.
    warmup_gate: Option<crate::consent_warmup::WarmupGate>,
    /// The approved access revision this process last saw: another one is a new
    /// approval in this process ([`App::rearm_access_steps`]).
    seen_revision: Option<u64>,
    /// The launch check found Full Disk Access off on a Mac where aterm never
    /// verified it, and no grant was verified here since
    /// ([`App::note_fda_unverified_here`]): a new approval brings the window's
    /// own page back.
    fda_unverified_here: bool,
}

impl AccessSteps {
    /// Steps with `reach`.
    pub(crate) fn new(reach: StepsReach) -> Self {
        Self {
            reach,
            local_network_done: None,
            local_network_read: None,
            local_network_until: None,
            local_network_retry: None,
            opt_outs: None,
            pane_act: None,
            pane_until: None,
            fda_regressed: false,
            fda_declined: None,
            dialog: None,
            line: None,
            recorded: Vec::new(),
            retired: false,
            join_pending_retired: false,
            warmup_gate: None,
            seen_revision: None,
            fda_unverified_here: false,
        }
    }

    /// Inert steps: the test App and a headless instance.
    pub(crate) fn inert() -> Self {
        Self::new(StepsReach::INERT)
    }

    /// Swap the reach (tests).
    #[cfg(test)]
    pub(crate) fn set_reach(&mut self, reach: StepsReach) {
        self.reach = reach;
    }

    /// Whether the menu line names a regressed Full Disk Access grant (tests).
    #[cfg(test)]
    pub(crate) const fn fda_regressed(&self) -> bool {
        self.fda_regressed
    }

    fn local_network_alert_up(&self, now: Instant) -> bool {
        self.local_network_until.is_some_and(|until| now < until)
    }

    fn pane_up(&self, now: Instant) -> bool {
        self.pane_until.is_some_and(|until| now < until)
    }
}

/// One of this Mac's IPv4 interfaces, as the Local Network trigger judges it.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LanInterface {
    /// The interface's address.
    pub(crate) address: Ipv4Addr,
    /// Its netmask (an empty mask reads `0.0.0.0`).
    pub(crate) mask: Ipv4Addr,
    /// `IFF_BROADCAST` and not `IFF_POINTOPOINT`: a LAN segment, not a tunnel
    /// (a VPN's `utun` with a private address is no local network).
    pub(crate) broadcast: bool,
}

/// The unicast LAN address the Local Network trigger names (pure): from this
/// Mac's IPv4 interfaces that are up, not loopback and broadcast-capable (Apple
/// TN3179's own filter), a private one first (10/8, 172.16/12, 192.168/16),
/// else link-local (169.254/16); the subnet's first host, or its second when
/// the first is this Mac. `None` when no such subnet has a mask from /8 to /30
/// (a /31, a /32, an empty mask, a mask with holes, or none at all).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn lan_target(interfaces: &[LanInterface]) -> Option<Ipv4Addr> {
    let pick = |wanted: fn(Ipv4Addr) -> bool| {
        interfaces.iter().find_map(|interface| {
            let (address, mask) = (interface.address, interface.mask);
            let prefix = u32::from(mask).leading_ones();
            if !interface.broadcast
                || !wanted(address)
                || u32::from(mask).count_ones() != prefix
                || !(8..=30).contains(&prefix)
            {
                return None;
            }
            let net = u32::from(address) & u32::from(mask);
            let first = Ipv4Addr::from(net + 1);
            Some(if first == address {
                Ipv4Addr::from(net + 2)
            } else {
                first
            })
        })
    };
    pick(|address| address.is_private()).or_else(|| pick(|address| address.is_link_local()))
}

/// Whether System State manages `subject` under `selection`: its setting is
/// selected and, for an access row, the selection needs it.
fn managed(subject: Subject, selection: Selection) -> bool {
    selection.contains(subject.setting())
        && match subject {
            Subject::Access(access) => access.needed_for(selection),
            Subject::Setting(_) => true,
        }
}

/// The one quiet signal (§3.6, pure): `System State: <row> needs you`, from the managed rows
/// that wait for a person plus a verified Full Disk Access grant that reads off. A failed row,
/// or a step the last run could not finish, reads `System State: <row> failed` (`1 step
/// failed`) once the run no longer retries it by itself (§3.5); several are counted
/// (`2 need you`, `1 needs you, 1 failed`). The note is the Secrets row's while it needs you
/// (a waiting vault approval code, or a move to Applications). `None` when nothing does.
pub(crate) fn menu_line(status: &SetupStatus, fda_regressed: bool) -> Option<SystemStateLine> {
    let selection = status
        .plan
        .as_ref()
        .map_or_else(Selection::none, |plan| plan.selection);
    let managed_with = |standing: Standing| {
        status
            .observations_for(status.plan.as_ref().map(|plan| plan.vault_url.as_str()))
            .filter(move |observation| {
                observation.standing == standing && managed(observation.subject, selection)
            })
            .map(|observation| observation.subject)
    };
    let fda = Subject::Access(Access::FullDiskAccess);
    let mut needs: Vec<Subject> = managed_with(Standing::NeedsYou).collect();
    if fda_regressed && !needs.contains(&fda) {
        needs.push(fda);
    }
    needs.dedup();
    // A failure the run still retries by itself is the page's alone (§3.5).
    let mut failed: Vec<Subject> = managed_with(Standing::Failed)
        .filter(|subject| status.failures_signal() && !needs.contains(subject))
        .collect();
    failed.dedup();
    let mut failed: Vec<Option<Subject>> = failed.into_iter().map(Some).collect();
    // A step the last run could not finish is a failure too, though no reading says so (a join
    // the vault refused reads Not joined): one a row names counts as that row, any other is
    // counted but not named, since the page's note names it.
    for row in status.failed_steps(status.plan.as_ref()) {
        match row {
            Some(row) if needs.contains(&row) || failed.contains(&Some(row)) => {}
            row => failed.push(row),
        }
    }
    let label = match (needs.as_slice(), failed.as_slice()) {
        ([], []) => return None,
        ([one], []) => format!("System State: {} needs you", one.name()),
        ([], [Some(one)]) => format!("System State: {} failed", one.name()),
        ([], [None]) => "System State: 1 step failed".to_owned(),
        (needs, []) => format!("System State: {} need you", needs.len()),
        ([], failed) => format!("System State: {} failed", failed.len()),
        ([_], failed) => format!("System State: 1 needs you, {} failed", failed.len()),
        (needs, failed) => format!(
            "System State: {} need you, {} failed",
            needs.len(),
            failed.len()
        ),
    };
    let note = status
        .observations_for(status.plan.as_ref().map(|plan| plan.vault_url.as_str()))
        .find(|observation| {
            observation.subject == Subject::Setting(Setting::Secrets)
                && observation.standing == Standing::NeedsYou
                && selection.contains(Setting::Secrets)
        })
        .and_then(|observation| observation.note.clone())
        .filter(|note| !note.trim().is_empty());
    Some(SystemStateLine { label, note })
}

impl App {
    /// The approved setup revision whose access steps may run: the admitted
    /// plan's revision while the permission gate allows it, else none.
    pub(crate) fn access_revision(&self) -> Option<u64> {
        self.workstation_setup.access_revision()
    }

    /// The `[privacy]` switches leave the steps on: `enabled`, `check` and
    /// `notice`.
    pub(crate) fn access_steps_allowed(&self) -> bool {
        self.config.privacy_probe_gate().permits() && self.config.privacy_notice()
    }

    /// Which Permissions rows the person opted out of, in [`OPT_OUT_ROWS`] order: every row
    /// while a `[privacy]` switch is off ([`OptOut::Always`]), and the debugger wherever its off
    /// reading is their choice ([`crate::debugger_access::off_is_chosen`], read from the
    /// debugger record, so it holds with or without the window's own admission). The record
    /// says what an off reading means, so the debugger's is [`OptOut::WhileOff`]: a debugger
    /// that reads on, or could not be read, keeps its own words.
    pub(crate) fn access_opt_outs(&self) -> [Option<OptOut>; 4] {
        let off = (!self.access_steps_allowed()).then_some(OptOut::Always(PRIVACY_OFF));
        OPT_OUT_ROWS.map(|access| match access {
            Access::Debugger => off.or(self
                .debugger_change
                .off_is_chosen()
                .then_some(OptOut::WhileOff(DEBUGGER_OFF))),
            _ => off,
        })
    }

    /// Tell the setup worker which Permissions rows the person opted out of
    /// ([`Self::access_opt_outs`]), so each reads as their choice. Called on each System State
    /// tick; `true` when what the worker was told changed, so the page's rows are read again
    /// (`App::tick_system_state`).
    pub(crate) fn publish_access_opt_outs(&mut self) -> bool {
        use aterm_types::macos_setup::opt_out;
        let opt_outs = self.access_opt_outs();
        for (access, why) in OPT_OUT_ROWS.into_iter().zip(opt_outs) {
            opt_out::set(access, why);
        }
        self.access_steps.opt_outs.replace(opt_outs) != Some(opt_outs)
    }

    /// The marker words the hand-over pages write and read: bound to the
    /// approved revision (`r7`), or the running version while none is approved.
    pub(crate) fn access_marks(&self) -> devtools_helper::Marks {
        let revision = self.access_revision();
        devtools_helper::Marks {
            devtools: revision.map_or_else(
                || aterm_types::version::app_version().to_owned(),
                atpkg::devtools::revision_key,
            ),
            fda: Self::fda_opened_key(revision),
            revision,
        }
    }

    /// Whether one of the window's own access gestures is up: a hand-over panel,
    /// a folder warm-up walking, the Local Network alert's hold, or a pane's.
    fn access_gesture_up(&self, now: Instant) -> bool {
        self.devtools_helper.panel().is_some()
            || self.consent_warmup.is_live()
            || self.access_steps.local_network_alert_up(now)
            || self.access_steps.pane_up(now)
    }

    /// Reserve the worker's dialog slot before a pane, panel or folder worker
    /// can start. A moment check alone cannot keep a worker from winning it.
    pub(crate) fn reserve_access_dialog(&mut self) -> bool {
        if !self.access_steps.reach.holds_dialog || self.access_steps.dialog.is_some() {
            return true;
        }
        self.access_steps.dialog = MacosDialog::when_none_up(Duration::ZERO);
        self.access_steps.dialog.is_some()
    }

    /// A person's own press brings their chosen page forward immediately;
    /// automatic steps and every control client must instead win the slot.
    pub(crate) fn reserve_hand_over_dialog(&mut self, origin: devtools_helper::Origin) -> bool {
        if origin.owner_press()
            && self.access_steps.reach.holds_dialog
            && self.access_steps.dialog.is_none()
        {
            self.access_steps.dialog = Some(MacosDialog::up());
            return true;
        }
        self.reserve_access_dialog()
    }

    /// A failed start releases its reservation; an already visible gesture
    /// keeps the same guard, including when another helper replaces its panel.
    pub(crate) fn release_unused_access_dialog(&mut self, now: Instant) {
        if !self.access_gesture_up(now) {
            self.access_steps.dialog = None;
        }
    }

    /// A pane without a helper gives no close notification. Keep the bounded
    /// hold until a helper takes over or the normal deadline ends it.
    pub(crate) fn hold_access_pane(&mut self, now: Instant) {
        self.access_steps.pane_until = Some(now + PANE_HOLD);
    }

    pub(crate) fn access_helper_shown(&mut self) {
        self.access_steps.pane_until = None;
    }

    /// The person's direct Settings gesture already owns this guard. Transfer
    /// it before recording the hand-off, without opening a gap for a worker.
    pub(crate) fn adopt_access_dialog(&mut self, dialog: Option<MacosDialog>, now: Instant) {
        if let Some(dialog) = dialog {
            if self.access_steps.dialog.is_none() {
                self.access_steps.dialog = Some(dialog);
            }
            self.hold_access_pane(now);
        }
    }

    /// The moment the first step (Local Network) may start: an approved
    /// revision, the switches on, System State frontmost, no macOS gesture up and
    /// the quiet-moment rules ([`StepsReach::quiet`]).
    fn local_network_moment(&self, now: Instant) -> bool {
        self.access_revision().is_some()
            && self.access_steps_allowed()
            && (self.access_steps.reach.frontmost)(self)
            && !self.access_gesture_up(now)
            && !(self.access_steps.reach.dialog_up)()
            && (self.access_steps.reach.quiet)(self, now)
    }

    /// Whether the Local Network step of the approved revision is still to run
    /// in this process.
    fn local_network_pending(&self) -> bool {
        self.access_revision()
            .is_some_and(|revision| self.access_steps.local_network_done != Some(revision))
    }

    /// The step moment: an access step after Local Network may put its page, panel or folder
    /// dialog on screen now ([`Self::local_network_moment`], with Local Network done).
    pub(crate) fn access_step_moment(&self, now: Instant) -> bool {
        !self.local_network_pending() && self.local_network_moment(now)
    }

    /// The Local Network step, first in the run: once per approved revision, a UDP connect
    /// (nothing sent) to a unicast LAN address, which makes macOS ask its Local Network
    /// question; recorded `triggered`. A revision already recorded is done. With no LAN to name
    /// the other steps go on, and it looks again every [`LOCAL_NETWORK_RETRY`] at its moment
    /// only: a retry due away from it is moved on. A retry is dropped only for another approved
    /// revision; while none is approved it is kept but arms no deadline
    /// ([`Self::next_access_step_deadline`]), so the same revision coming back looks again.
    pub(crate) fn tick_local_network_step(&mut self, now: Instant) {
        if self
            .access_steps
            .local_network_until
            .is_some_and(|until| now >= until)
        {
            self.access_steps.local_network_until = None;
        }
        if self
            .access_steps
            .pane_until
            .is_some_and(|until| now >= until)
        {
            self.access_steps.pane_until = None;
        }
        let Some(revision) = self.access_revision() else {
            return;
        };
        if self
            .access_steps
            .local_network_retry
            .is_some_and(|(failed, _)| failed != revision)
        {
            self.access_steps.local_network_retry = None;
        }
        if self.access_steps.local_network_done == Some(revision) {
            // A trigger that found no LAN is tried again: the network may have
            // come up since, and the run's Permissions stage waits for it.
            let Some((_, at)) = self.access_steps.local_network_retry else {
                return;
            };
            if now < at {
                return;
            }
            if !self.local_network_moment(now) {
                // Due, but not the step's moment (the person is in a terminal
                // tab, typing, or away): look again a retry later, never at once.
                self.access_steps.local_network_retry = Some((revision, now + LOCAL_NETWORK_RETRY));
                return;
            }
        }
        if self.access_steps.local_network_read != Some(revision) {
            self.access_steps.local_network_read = Some(revision);
            if (self.access_steps.reach.recorded)(revision, Access::LocalNetwork).is_some() {
                self.access_steps.local_network_done = Some(revision);
                return;
            }
        }
        if !self.local_network_moment(now) {
            return;
        }
        if self.access_steps.reach.holds_dialog {
            // Own the worker's dialog slot before either the trigger or its
            // record wakes it. The quiet-moment check cannot reserve the slot.
            let Some(dialog) = MacosDialog::when_none_up(Duration::ZERO) else {
                // A worker won the slot after the moment was checked. A due
                // retry waits again rather than leave a deadline due now.
                if let Some((_, at)) = &mut self.access_steps.local_network_retry {
                    *at = now + LOCAL_NETWORK_RETRY;
                }
                return;
            };
            self.access_steps.dialog = Some(dialog);
        }
        self.access_steps.local_network_done = Some(revision);
        if (self.access_steps.reach.trigger_local_network)() {
            self.access_steps.local_network_retry = None;
            self.access_steps.local_network_until = Some(now + LOCAL_NETWORK_ALERT_HOLD);
            self.record_access_step(Access::LocalNetwork, AccessOutcome::Triggered);
            aterm_log::info!("system state: Local Network asked for (revision {revision})");
        } else {
            self.access_steps.dialog = None;
            self.access_steps.local_network_retry = Some((revision, now + LOCAL_NETWORK_RETRY));
            aterm_log::info!(
                "system state: nothing to ask Local Network with: no LAN to name, or no \
                 socket (revision {revision}); trying again in {}s",
                LOCAL_NETWORK_RETRY.as_secs()
            );
        }
    }

    /// The end of the consent lanes' turn: park or release a walking launch folder pass, hold
    /// or release the window's dialog guard, record the panels' Not now, spend the access
    /// card's ([`Self::spend_card_not_now`]), bring the pages back for a revision approved in
    /// this process ([`Self::rearm_access_steps`]), and end the retired digest entries once.
    pub(crate) fn settle_access_steps(&mut self, now: Instant) {
        if self.access_steps.warmup_gate.is_some() && !self.consent_warmup.is_live() {
            self.note_launch_warmup_progress();
            self.access_steps.warmup_gate = None;
        }
        if let Some(gate) = &self.access_steps.warmup_gate {
            // Losing admission retires this pass. A privacy switch off or
            // leaving System State only pauses it between folders.
            if self.access_revision().is_none() {
                gate.retire();
            } else {
                gate.set(
                    (self.access_steps.reach.frontmost)(self)
                        && self.access_steps_allowed()
                        && self.config.privacy_warmup()
                            == crate::app_config::PrivacyWarmup::Automatic,
                );
            }
        }
        let up = self.access_steps.reach.holds_dialog && self.access_gesture_up(now);
        if up != self.access_steps.dialog.is_some() {
            self.access_steps.dialog = up.then(MacosDialog::up);
        }
        for (revision, access) in self.devtools_helper.take_declines() {
            self.record_access_at(revision, access, AccessOutcome::Declined);
        }
        self.spend_card_not_now();
        self.rearm_access_steps(now);
        if !self.access_steps.retired {
            self.access_steps.retired = true;
            for key in RETIRED_KEYS {
                self.except_withdraw(key);
            }
        }
    }

    /// At Commit of a seamless update the parent's log can hold entries an older
    /// parent raised after this process launched, and the successor adopts them
    /// then: one under a retired key ends at the next settle as one adopted at
    /// launch does, and the vault join's ends now if a status has read the
    /// Secrets row.
    #[cfg(unix)]
    pub(crate) fn rearm_retired_keys(&mut self) {
        self.access_steps.retired = false;
        self.access_steps.join_pending_retired = false;
        self.refresh_system_state_menu_line();
    }

    /// Whether the approved revision's own Full Disk Access step is declined (its panel's Not
    /// now under the revision's key, `devtools_helper::declined_for`): that holds the window's
    /// page back and keeps a regressed grant off the menu line for the revision. Read once per
    /// key and again after a decline this process notes; never without an approved revision.
    pub(crate) fn fda_declined_in_revision(&mut self) -> bool {
        if self.access_revision().is_none() {
            return false;
        }
        let key = self.access_marks().fda;
        if let Some((read, declined)) = &self.access_steps.fda_declined
            && *read == key
        {
            return *declined;
        }
        let last = (self.devtools_helper.fda_reach().last_declined)();
        let declined = devtools_helper::declined_for(
            devtools_helper::Origin::Automatic,
            &key,
            last.as_ref(),
            false,
        );
        self.access_steps.fda_declined = Some((key, declined));
        declined
    }

    /// A Full Disk Access decline was noted in this process: the menu line reads
    /// the marker again.
    fn fda_decline_noted(&mut self) {
        self.access_steps.fda_declined = None;
        self.refresh_system_state_menu_line();
    }

    /// Compose the menu line again from the latest status and the regression flag (dark while
    /// the revision's Full Disk Access step is declined): called where either changes, never
    /// per park. An older aterm's vault-join entry ends once the selected Secrets row has a
    /// usable reading for this vault; a waiting row counts only once it carries its guidance,
    /// which replaces the entry's approval code.
    pub(crate) fn refresh_system_state_menu_line(&mut self) {
        let regressed = self.access_steps.fda_regressed && !self.fda_declined_in_revision();
        let status = self.system_state_host.latest_status();
        let line = menu_line(status, regressed);
        let secrets_read = status.plan.as_ref().is_some_and(|plan| {
            plan.selection.contains(Setting::Secrets)
                && status
                    .observations_for(Some(&plan.vault_url))
                    .find(|observation| observation.subject == Subject::Setting(Setting::Secrets))
                    .is_some_and(|observation| match observation.standing {
                        Standing::Unknown | Standing::Failed => false,
                        Standing::NeedsYou => observation
                            .note
                            .as_deref()
                            .or(observation.detail.as_deref())
                            .is_some_and(|note| !note.trim().is_empty()),
                        Standing::Waiting => observation
                            .note
                            .as_deref()
                            .is_some_and(|note| !note.trim().is_empty()),
                        _ => true,
                    })
        });
        if secrets_read && !self.access_steps.join_pending_retired {
            self.access_steps.join_pending_retired = true;
            self.except_withdraw(JOIN_PENDING_KEY);
        }
        if line != self.access_steps.line {
            self.access_steps.line = line;
            self.mark_status_dirty();
        }
    }

    /// The access steps' next look: a hold ending; while the Local Network step or a client's
    /// pane act waits with an aterm window focused, the next look for the moment; and the
    /// approved revision's Local Network retry while its moment can come (the switches on and a
    /// window focused; a focus gain wakes the loop). The step's tick moves a retry that fell due
    /// away from its moment, so this never arms a deadline that is due again at once.
    pub(crate) fn next_access_step_deadline(&self, now: Instant) -> Option<Instant> {
        let hold = self
            .access_steps
            .local_network_until
            .into_iter()
            .chain(self.access_steps.pane_until);
        let waits = (self.local_network_pending() && self.access_steps.reach.holds_dialog)
            || self.access_steps.pane_act.is_some();
        let retry = self
            .access_steps
            .local_network_retry
            .filter(|(failed, _)| Some(*failed) == self.access_revision());
        // Focus is read only when something waits for the moment.
        let focused = (waits || retry.is_some())
            && self.access_steps_allowed()
            && self.any_os_window_focused();
        let look = (focused && waits).then(|| now + devtools_helper::RECHECK_EVERY);
        let retry = retry.filter(|_| focused).map(|(_, at)| at.max(now));
        hold.chain(look).chain(retry).min()
    }

    /// Record `outcome` for `access` under the approved revision; nothing
    /// without one, and one line per outcome per process.
    pub(crate) fn record_access_step(&mut self, access: Access, outcome: AccessOutcome) {
        if let Some(revision) = self.access_revision() {
            self.record_access_at(revision, access, outcome);
        }
    }

    fn record_access_at(&mut self, revision: u64, access: Access, outcome: AccessOutcome) {
        let line = (revision, access, outcome);
        if self.access_steps.recorded.contains(&line) {
            return;
        }
        self.access_steps.recorded.push(line);
        (self.access_steps.reach.record)(revision, access, outcome);
        if (access, outcome) == (Access::FullDiskAccess, AccessOutcome::Declined) {
            self.fda_decline_noted();
        }
    }

    /// The launch folder pass started with `gate`, which parks it between folders from now on
    /// ([`Self::settle_access_steps`]).
    pub(crate) fn park_launch_warmup_with(&mut self, gate: crate::consent_warmup::WarmupGate) {
        self.access_steps.warmup_gate = Some(gate);
    }

    /// Retire only the automatic launch pass. A person's explicit folder
    /// pass has no gate and keeps its own intent.
    pub(crate) fn retire_launch_warmup(&self) {
        if let Some(gate) = &self.access_steps.warmup_gate {
            gate.retire();
        }
    }

    pub(crate) fn launch_warmup_retired(&self) -> bool {
        self.access_steps
            .warmup_gate
            .as_ref()
            .is_some_and(|gate| gate.is_retired())
    }

    /// A verified Full Disk Access grant reads off (`true`), or that ended: the
    /// menu line follows. It never opens a page by itself (§3.6).
    pub(crate) fn note_fda_regression(&mut self, regressed: bool) {
        if self.access_steps.fda_regressed != regressed {
            self.access_steps.fda_regressed = regressed;
            aterm_log::info!(
                "system state: a verified Full Disk Access grant {}",
                if regressed { "reads off" } else { "is settled" }
            );
            self.refresh_system_state_menu_line();
        }
    }

    /// The menu line the status item shows, as last composed.
    pub(crate) fn system_state_menu_line(&self) -> Option<SystemStateLine> {
        self.access_steps.line.clone()
    }

    /// The status item's menu line press: System State in the frontmost window (else the
    /// first), raised. With every window closed, a new window (Cmd-N's) shows it once made
    /// ([`Self::system_state_once_landed`]). `true` when System State is up now; a window on its
    /// way answers `false`.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn open_system_state_from_status_item(&mut self) -> bool {
        let window = self
            .frontmost_window
            .filter(|wid| self.windows.contains_key(wid))
            .or_else(|| self.windows.keys().next().copied());
        if let Some(wid) = window {
            let opened = self.open_system_state_tab_in_window(wid);
            self.raise_window(wid);
            return opened;
        }
        if self.headless {
            return false;
        }
        aterm_log::info!("status item: no window; System State opens in a new one");
        self.open_window_off_loop(
            crate::tab_spawn::SpawnAsk::default(),
            None,
            Self::system_state_once_landed(),
        );
        false
    }

    /// What a new window the status item asked for does once its first shell
    /// lands: System State shown in the window that shell is in, raised. A
    /// window that never came says why in its own message row.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn system_state_once_landed() -> crate::tab_spawn::TabSpawnThen {
        crate::tab_spawn::TabSpawnThen::Run(Box::new(|app, landed| {
            let local = landed.ok().and_then(|sid| {
                let store = app.store.read().unwrap_or_else(|p| p.into_inner());
                store
                    .by_sid(&aterm_session::SessionId::new(sid))
                    .map(|handle| handle.local_id)
            });
            let Some(wid) = local.and_then(|id| app.window_of_session(id)) else {
                aterm_log::info!("status item: the new window for System State did not come");
                return;
            };
            if app.open_system_state_tab_in_window(wid) {
                app.raise_window(wid);
            }
        }))
    }

    /// The access step whose macOS gesture is up, or about to start, for the
    /// page's headline and row ("Enter your password for Debugger access").
    pub(crate) fn system_state_access_step(&self) -> Option<Access> {
        if let Some(debugger) = self.system_state_debugger_step() {
            return Some(debugger);
        }
        if let Some(panel) = self.devtools_helper.panel() {
            return Some(panel.page().access());
        }
        if self.access_steps.local_network_alert_up(Instant::now()) {
            return Some(Access::LocalNetwork);
        }
        let revision = self.access_revision()?;
        if !self.access_steps_allowed() || !(self.access_steps.reach.frontmost)(self) {
            return None;
        }
        if self.access_steps.local_network_done != Some(revision) {
            Some(Access::LocalNetwork)
        } else if self.devtools_helper.fda_waiting() {
            Some(Access::FullDiskAccess)
        } else if self.devtools_helper.devtools_waiting() {
            Some(Access::DeveloperTools)
        } else {
            None
        }
    }

    /// A press on a Needs-you row's button (`Open Full Disk Access`, `Turn On…`); `true` when
    /// System Settings (or the debugger change) is up for it now. A person's press opens the
    /// page past any Not now. A control client's act (`by_person` false) never mints the
    /// standing yes: it does nothing without an approved revision or with a `[privacy]` switch
    /// off, its Full Disk Access and Developer Tools pages pass the window's own hand-over rules
    /// as [`devtools_helper::Origin::SystemStateAct`], and its Local Network and Accessibility
    /// panes are queued for the step moment ([`Self::tick_queued_pane`]), answering `false`.
    pub(crate) fn open_access_from_system_state(
        &mut self,
        access: Access,
        by_person: bool,
    ) -> bool {
        let (opened, how) =
            if !by_person && (self.access_revision().is_none() || !self.access_steps_allowed()) {
                (false, "no approved revision, or a [privacy] switch off")
            } else {
                match access {
                    Access::FullDiskAccess => {
                        let answer = self.open_fda_from_system_state(by_person);
                        (answer.brings_owner(), answer.as_str())
                    }
                    Access::DeveloperTools => {
                        let origin = if by_person {
                            devtools_helper::Origin::Settings
                        } else {
                            devtools_helper::Origin::SystemStateAct
                        };
                        let answer = self.hand_devtools_to_owner(origin);
                        (answer.brings_owner(), answer.as_str())
                    }
                    Access::LocalNetwork | Access::Accessibility if by_person => {
                        let opened = self.open_access_pane(
                            access,
                            devtools_helper::Origin::Settings,
                            Instant::now(),
                        );
                        (opened, if opened { "opened" } else { "failed" })
                    }
                    Access::LocalNetwork | Access::Accessibility if self.headless => {
                        (false, "headless")
                    }
                    Access::LocalNetwork | Access::Accessibility => {
                        self.access_steps.pane_act = Some((access, Instant::now()));
                        (false, "queued")
                    }
                    Access::Debugger => {
                        let started = self.turn_on_debugger_from_system_state(by_person);
                        (started, if started { "started" } else { "not started" })
                    }
                }
            };
        aterm_log::info!(
            "system state: {} {how} ({})",
            access.name(),
            if by_person {
                "a person's press"
            } else {
                "a client's act"
            }
        );
        opened
    }

    /// Full Disk Access from System State's row, through the one hand-over
    /// ([`Self::fda_hand_over`]): a person's press opens the page past a Not now; a client's act
    /// is queued for the step moment and judged there as the window's own is. A probe still
    /// pending reads as not granted, so the act queues and its moment reads it again.
    fn open_fda_from_system_state(&mut self, by_person: bool) -> devtools_helper::HandOver {
        use aterm_containment::consent::{FdaState, ProbeLabel};
        let origin = if by_person {
            devtools_helper::Origin::Settings
        } else {
            devtools_helper::Origin::SystemStateAct
        };
        let read = self.consent_panel_facts();
        let fda = if read.probe == ProbeLabel::Pending {
            FdaState::Denied
        } else {
            read.fda
        };
        let answer = self.fda_hand_over(origin, devtools_helper::Moment::Asked, fda);
        if answer.brings_owner() {
            self.fda_page_up_for_launch();
            if by_person {
                self.note_macos_access_settings_opened(Instant::now());
            }
        }
        answer
    }

    /// A control client's act on the Local Network or Accessibility row: its pane opens at the
    /// step moment ([`Self::access_step_moment`]), and an act still waiting after
    /// [`devtools_helper::ASKED_LAPSE`] lapses.
    pub(crate) fn tick_queued_pane(&mut self, now: Instant) {
        let Some((access, asked)) = self.access_steps.pane_act else {
            return;
        };
        if now.saturating_duration_since(asked) >= devtools_helper::ASKED_LAPSE {
            self.access_steps.pane_act = None;
            aterm_log::info!(
                "system state: {} lapsed (a client's act; System State never came to the front)",
                access.name()
            );
            return;
        }
        if !self.access_step_moment(now) {
            return;
        }
        if !self.reserve_access_dialog() {
            return;
        }
        self.access_steps.pane_act = None;
        let opened = self.open_access_pane(access, devtools_helper::Origin::SystemStateAct, now);
        aterm_log::info!(
            "system state: {} {} at the step moment (a client's act)",
            access.name(),
            if opened { "opened" } else { "failed" }
        );
    }

    /// A new access revision approved while aterm runs (Permissions off and on, or Set up)
    /// brings back the window's own Full Disk Access and Developer Tools pages, as the next
    /// launch would ([`Self::rearm_fda_step`], [`Self::rearm_devtools_step`]), each queued for
    /// the step moment (design §6). The debugger step comes back by the window's own admission
    /// (`DebuggerStepQueue::unpark`), and Local Network is per revision already. The first
    /// revision this process sees is the launch path's, and the same revision coming back
    /// changes nothing. While a `[privacy]` switch is off nothing is queued and the revision is
    /// not taken as seen, so one approved meanwhile asks once every switch is on.
    pub(crate) fn rearm_access_steps(&mut self, now: Instant) {
        let Some(revision) = self.access_revision() else {
            return;
        };
        if !self.access_steps_allowed() {
            return;
        }
        let Some(seen) = self.access_steps.seen_revision.replace(revision) else {
            return;
        };
        if seen == revision {
            return;
        }
        aterm_log::info!(
            "system state: access revision {revision} approved in this process; its pages ask again"
        );
        self.rearm_fda_step(now);
        self.rearm_devtools_step(now);
    }

    /// A `[privacy]` switch on again runs what it held at launch (design §4): a switch off is a
    /// pause, never an answer. The first park with the needed switches on reruns the access
    /// card's held launch check (`consent_card::CardState::rearm_launch`, with Full Disk Access
    /// undecided again so the later steps wait behind its verdict), and makes the Developer
    /// Tools read held while the probe was off (`devtools_watch::DevToolsWatch::read_held`),
    /// forgetting a reading that only said the probe was off. The folder warm-up, the debugger
    /// step (`DebuggerStepQueue::resume`), Local Network and queued acts resume by themselves.
    pub(crate) fn resume_held_launch_steps(&mut self, now: Instant) {
        if !cfg!(target_os = "macos") || self.headless {
            return;
        }
        if self.config.privacy_probe_gate().permits() {
            // A read whose completion landed after a switch went off was spent on
            // nothing (the launch read among them): it is owed again.
            if self.devtools_helper.forget_switched_off_reading() {
                self.devtools_watch.hold_read();
            }
            if self.devtools_watch.read_held() && self.any_os_window_focused() {
                self.read_devtools_list(now);
            }
        }
        if self.consent_card_host.launch_held
            && self.access_steps_allowed()
            && self.consent_card.rearm_launch()
        {
            self.consent_card_host.launch_held = false;
            aterm_log::info!(
                "system state: the [privacy] switches are on again; the access check runs"
            );
            if self.consent_launch.live()
                && self.consent_launch.fda() == crate::consent_launch::FdaLaunch::NotAsked
            {
                self.consent_launch
                    .set_fda(crate::consent_launch::FdaLaunch::Undecided);
            }
        }
    }

    /// The access card's launch check found Full Disk Access off on a Mac where aterm never
    /// verified it (`unverified`, per `consent_launch::hands_over`), or a grant was verified
    /// here since (`false`).
    pub(crate) fn note_fda_unverified_here(&mut self, unverified: bool) {
        self.access_steps.fda_unverified_here = unverified;
    }

    /// The window's own Full Disk Access page for a new access revision, only where the launch
    /// check would bring it ([`Self::note_fda_unverified_here`]) and the grant does not read
    /// on. It is judged whole at the step moment (`App::fda_hand_over`); a page aterm brought
    /// up within [`atpkg::devtools::REOPEN_AFTER_S`] defers it rather than refusing it.
    fn rearm_fda_step(&mut self, now: Instant) {
        if !cfg!(target_os = "macos")
            || !self.devtools_helper.windowed()
            || !self.access_steps.fda_unverified_here
            || self.consent_panel_facts().fda == aterm_containment::consent::FdaState::Granted
        {
            return;
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "a new access revision in this process: the window's own Full Disk Access step asks again, judged whole at its moment"
        )]
        self.devtools_helper
            .queue_fda(devtools_helper::Origin::Automatic, now);
        let window = atpkg::devtools::REOPEN_AFTER_S;
        if let Some(ago) = (self.devtools_helper.fda_reach().opened_within)(window) {
            self.devtools_helper
                .defer_fda(now + Duration::from_secs(window.saturating_sub(ago).max(1)));
        }
        // The folders wait behind it again, as at launch.
        self.consent_launch
            .set_fda(crate::consent_launch::FdaLaunch::Waiting);
    }

    /// The window's own Developer Tools page for a new access revision: the watch's
    /// once-per-process latch is let go, and a reading that already says `not_granted` queues
    /// the page now unless aterm opened it for this revision. It is judged whole at the step
    /// moment (`App::devtools_hand_over`), behind any Full Disk Access page, and a page aterm
    /// brought up within [`atpkg::devtools::REOPEN_AFTER_S`] defers it.
    fn rearm_devtools_step(&mut self, now: Instant) {
        if !cfg!(target_os = "macos") || !self.devtools_helper.windowed() {
            return;
        }
        self.devtools_watch.forget_hand_over();
        if self.devtools_reading().grant != aterm_containment::DevToolGrant::NotGranted {
            return;
        }
        let reach = self.devtools_helper.reach();
        let key = self.access_marks().devtools;
        if (reach.last_opened)().is_some_and(|(word, _)| word == key) {
            return;
        }
        self.devtools_helper
            .queue(devtools_helper::Origin::Automatic, now);
        let window = atpkg::devtools::REOPEN_AFTER_S;
        if let Some(ago) = (reach.opened_within)(window) {
            self.devtools_helper
                .defer(now + Duration::from_secs(window.saturating_sub(ago).max(1)));
        }
    }

    /// The access card's Not now, an older aterm's answer (`consent_card::Marker::NotNow`), is
    /// the Not now of the first approved revision it meets (design §6). Under an approved
    /// revision with the steps allowed it is noted as that revision's panel Not now
    /// (`FdaReach::note_declined`, unless one is noted already) and recorded `declined` unless
    /// the row has a record in it, so the row reads as the person's choice; then it is retired
    /// ([`Self::retire_card_not_now`]). Until spent it holds the window's own page in every
    /// revision (`App::fda_admit_facts`).
    fn spend_card_not_now(&mut self) {
        if self.consent_card_host.marker != Some(crate::consent_card::Marker::NotNow) {
            return;
        }
        let Some(revision) = self.access_revision() else {
            return;
        };
        if !self.access_steps_allowed() {
            return;
        }
        let key = self.access_marks().fda;
        let fda = self.devtools_helper.fda_reach();
        if !(fda.last_declined)().is_some_and(|(word, _)| atpkg::devtools::same_step(&word, &key)) {
            (fda.note_declined)(&key);
        }
        if (self.access_steps.reach.recorded)(revision, Access::FullDiskAccess).is_none() {
            self.record_access_at(revision, Access::FullDiskAccess, AccessOutcome::Declined);
        }
        self.retire_card_not_now();
        self.fda_decline_noted();
        aterm_log::info!(
            "system state: the access card's Not now answers Full Disk Access for revision \
             {revision}, and is retired"
        );
    }

    /// Retire the access card's `not-now` once spent: the cache at once, the file beside
    /// `aterm.toml` off the UI thread (`consent_card::retire_not_now`, which leaves anything but
    /// a `not-now` as found). Best effort: a file that survives is spent again by the next
    /// launch.
    fn retire_card_not_now(&mut self) {
        self.consent_card_host.marker = None;
        let Some(config_path) = self.consent_card_config.clone() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("aterm-macos-access-answer".into())
            .spawn(move || {
                if let Err(error) = crate::consent_card::retire_not_now(Some(&config_path)) {
                    aterm_log::warn!(
                        "macOS access card: the spent not-now was not retired: {error}"
                    );
                }
            });
        if let Err(error) = spawned {
            aterm_log::warn!("macOS access card: the spent not-now was not retired: {error}");
        }
    }

    /// The System Settings pane of a grant aterm has no page of its own for:
    /// Local Network and Accessibility. Once open it counts as up for
    /// [`PANE_HOLD`], so no other step's gesture comes over it.
    fn open_access_pane(
        &mut self,
        access: Access,
        origin: devtools_helper::Origin,
        now: Instant,
    ) -> bool {
        if self.headless || !self.reserve_hand_over_dialog(origin) {
            return false;
        }
        let opened = (self.access_steps.reach.open_pane)(self, access);
        if opened {
            self.hold_access_pane(now);
        } else {
            self.release_unused_access_dialog(now);
        }
        opened
    }
}

#[cfg(target_os = "macos")]
fn live_open_access_pane(app: &App, access: Access) -> bool {
    use crate::platform::AppRt as _;
    const ACCESSIBILITY_PANE: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
    app.apprt.open_url(if access == Access::Accessibility {
        ACCESSIBILITY_PANE
    } else {
        crate::fleet_glance::LOCAL_NETWORK_PANE
    })
}

#[cfg(not(target_os = "macos"))]
fn live_open_access_pane(_: &App, _: Access) -> bool {
    false
}

/// The live trigger: name a LAN peer from this Mac's interfaces and connect a
/// UDP socket to it. Nothing is sent. `false` when nothing was attempted (no
/// LAN to name, or no socket), so nothing is recorded and no hold is taken.
#[cfg(target_os = "macos")]
fn live_trigger_local_network() -> bool {
    let Some(target) = lan_target(&interface_addresses()) else {
        return false;
    };
    let socket = match std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(socket) => socket,
        Err(error) => {
            aterm_log::info!("system state: no socket for the Local Network trigger: {error}");
            return false;
        }
    };
    if let Err(error) = socket.connect((target, LOCAL_NETWORK_PORT)) {
        // A connect macOS refused is still the question asked: it asks on the
        // attempt, and a refusal is how an earlier Don't Allow reads.
        aterm_log::info!("system state: the Local Network connect to {target} failed: {error}");
    }
    true
}

#[cfg(not(target_os = "macos"))]
fn live_trigger_local_network() -> bool {
    let _ = LOCAL_NETWORK_PORT;
    false
}

/// This Mac's IPv4 interfaces that are up and not loopback.
#[cfg(target_os = "macos")]
fn interface_addresses() -> Vec<LanInterface> {
    /// The IPv4 address in a `sockaddr` of `AF_INET`, reading no byte past its
    /// `sa_len` (a netmask's may be cut short; the missing bytes are zero).
    ///
    /// # Safety
    /// `at` is non-null and points at a `sockaddr` whose `sa_len` bytes are
    /// readable.
    unsafe fn ipv4(at: *const libc::sockaddr) -> Ipv4Addr {
        // SAFETY: the caller's contract: `at` points at a readable sockaddr header.
        let len = usize::from(unsafe { (*at).sa_len });
        // SAFETY: `len` bytes from `at` are readable (the caller's contract).
        let bytes = unsafe { std::slice::from_raw_parts(at.cast::<u8>(), len) };
        // `sin_addr` sits after `sin_len`, `sin_family` and `sin_port`.
        let octet = |i: usize| bytes.get(4 + i).copied().unwrap_or(0);
        Ipv4Addr::new(octet(0), octet(1), octet(2), octet(3))
    }
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs writes the head of a list it allocated into `head`.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut at = head;
    while !at.is_null() {
        // SAFETY: `at` is a node of the list getifaddrs returned, freed only below.
        let node = unsafe { &*at };
        at = node.ifa_next;
        let flags = node.ifa_flags;
        if flags & libc::IFF_UP as u32 == 0
            || flags & libc::IFF_LOOPBACK as u32 != 0
            || node.ifa_addr.is_null()
            || node.ifa_netmask.is_null()
        {
            continue;
        }
        // SAFETY: a non-null `ifa_addr` points at a sockaddr header.
        if i32::from(unsafe { (*node.ifa_addr).sa_family }) != libc::AF_INET {
            continue;
        }
        // SAFETY: both are non-null sockaddrs of the node, `sa_len` bytes long.
        let (address, mask) = unsafe { (ipv4(node.ifa_addr), ipv4(node.ifa_netmask)) };
        out.push(LanInterface {
            address,
            mask,
            broadcast: flags & libc::IFF_BROADCAST as u32 != 0
                && flags & libc::IFF_POINTOPOINT as u32 == 0,
        });
    }
    // SAFETY: `head` came from a successful getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(head) };
    out
}

fn live_record(revision: u64, access: Access, outcome: AccessOutcome) {
    let Some(state) = aterm_types::dirs::state_dir() else {
        return;
    };
    if let Err(error) =
        aterm_types::macos_setup::access_record::append(&state, revision, access, outcome)
    {
        aterm_log::warn!("system state: the access record was not written: {error}");
    }
}

fn live_recorded(revision: u64, access: Access) -> Option<AccessOutcome> {
    let state = aterm_types::dirs::state_dir()?;
    aterm_types::macos_setup::access_record::latest(&state, revision, access)
}

#[cfg(test)]
#[path = "system_state_access_tests.rs"]
mod tests;
