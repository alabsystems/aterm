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
//!   time, and never while a first permission walk may still start and waits only for the person
//!   ([`App::walk_waits_for_the_person`]); while one is up the window holds a [`MacosDialog`], so
//!   the setup worker's own prompts wait too. The Local Network trigger goes first, at a moment of
//!   its own that waits for no walk still to come, and that walk waits for it and its alert's hold
//!   ([`App::local_network_step_to_come`]) until the step alone has held a ready walk for
//!   `permission_walk::LOCAL_NETWORK_WAIT` of unbroken looks (`App::waited_out_local_network`):
//!   the walk then goes first, and the step asks once it is over.
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
    Access, AccessOutcome, MacosDialog, Selection, Setting, Standing, Subject, SyncKind,
};

use crate::status_item::SystemStateLine;
use crate::system_state::ToolsWait;
use crate::{App, SetupStatus, devtools_helper};

/// How long the Local Network alert counts as up after the trigger
/// ([`aterm_types::macos_setup::LOCAL_NETWORK_QUESTION_HOLD`], a bound, not a reading).
pub(crate) const LOCAL_NETWORK_ALERT_HOLD: Duration =
    aterm_types::macos_setup::LOCAL_NETWORK_QUESTION_HOLD;

/// How long a Local Network trigger that found no LAN to name waits before it
/// looks again (the Wi-Fi may join after launch).
const LOCAL_NETWORK_RETRY: Duration = Duration::from_secs(60);

/// A failed access-record save retries independently of the step that ended.
const ACCESS_RECORD_RETRY: Duration = Duration::from_secs(1);

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

/// The menu line's note under a verified Full Disk Access grant an update left off
/// ([`App::note_fda_off_after_update`]).
pub(crate) const FDA_AFTER_UPDATE: &str = "The update needs Full Disk Access re-allowed.";

/// The vault join's waiting entry, which carries the approval code an admin Mac needs: it ends
/// only once a status has read the Secrets row.
pub(crate) const JOIN_PENDING_KEY: &str = "privacy.secrets.join-pending";

/// What a person's press on System State found of the window's own helper panel
/// ([`App::press_meets_unseen_panel`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnseenPanel {
    /// No panel held the press back unseen: it goes on as it would.
    Clear,
    /// The press asked for the panel's own page: System Settings came up at it, and the panel,
    /// which shows only beside System Settings, with it.
    BroughtForward,
    /// The panel gave way to the press: it closed (no Not now, no act on its page), its walk
    /// stepped aside, and the window's dialog guard went with it, so the press goes on now.
    GaveWay,
    /// No panel stood, but a pane aterm opened with none beside it held the guard for its bounded
    /// time ([`PANE_HOLD`]): a press back in aterm is past that pane, and the guard went.
    PaneLeft,
}

/// What the 1 s × 30 s LAN re-probe after [`AccessOutcome::Triggered`] observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalNetworkProbe {
    /// A LAN server answered: the person allowed Local Network.
    Granted,
    /// macOS refused a `.local` lookup (`-65570`): the person answered Don't Allow.
    #[cfg(any(target_os = "macos", test))]
    Refused,
    /// The 30 s window elapsed with no definite answer.
    #[cfg(any(target_os = "macos", test))]
    Unanswered,
}

/// What the access steps reach outside the App, injected so the steps are
/// tested without a LAN, a state directory or the process-wide dialog count.
#[derive(Clone, Copy)]
pub(crate) struct StepsReach {
    /// Raise macOS's Local Network question: `true` when a LAN peer was named.
    pub(crate) trigger_local_network: fn() -> bool,
    /// Start the 1 s × 30 s LAN grant re-probe for `revision` after [`AccessOutcome::Triggered`].
    pub(crate) start_local_network_probe: fn(u64),
    /// Drain a completed LAN grant re-probe (`(revision, outcome)`), if any.
    pub(crate) take_local_network_probe: fn() -> Option<(u64, LocalNetworkProbe)>,
    /// Local Network's answer moved as the window heard it: its question went up (recorded
    /// `triggered`, which the access record keeps as asked), or the re-probe after it ended,
    /// whatever it heard. Each LAN reader looks again now, the converge pass's `sync.peers` among
    /// them ([`crate::network_moved::local_network_moved`]).
    pub(crate) lan_answer_moved: fn(),
    /// Open a named access pane; injected so headless fixtures reach no OS API.
    pub(crate) open_pane: fn(&App, Access) -> bool,
    /// Append one step's outcome, reporting any failure to the retry owner.
    pub(crate) record: fn(u64, Access, AccessOutcome) -> std::io::Result<()>,
    /// The latest outcome recorded for a row in a revision.
    pub(crate) recorded: fn(u64, Access) -> Option<AccessOutcome>,
    /// Whether the record holds a row's `handed` in a revision: its macOS page came up then.
    pub(crate) handed: fn(u64, Access) -> bool,
    /// Whether System State is the visible view of a focused window.
    pub(crate) frontmost: fn(&App) -> bool,
    /// The steps' quiet-moment rules ([`App::step_moment_is_quiet`]).
    pub(crate) quiet: fn(&App, Instant) -> bool,
    /// Whether any macOS dialog this process started is up
    /// ([`aterm_types::macos_setup::macos_dialog_up`]).
    pub(crate) dialog_up: fn() -> bool,
    /// Hand this process's LAN to the Commons runtime
    /// ([`aterm_types::macos_setup::lan_first_use::hand_over`]) once the step's trigger has asked.
    pub(crate) hand_lan_over: fn(u64),
    /// Whether the window holds a [`MacosDialog`] while its own gesture is up
    /// (a live windowed instance only: the count is process-wide).
    pub(crate) holds_dialog: bool,
    /// Whether aterm.log names what holds the Local Network step
    /// ([`App::note_local_network_hold`]): only where the step can run, a live windowed macOS
    /// instance, never on an instance whose arms reach nothing.
    pub(crate) names_holds: bool,
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
        start_local_network_probe: |_| {},
        take_local_network_probe: || None,
        lan_answer_moved: || {},
        open_pane: |_, _| false,
        record: |_, _, _| Ok(()),
        recorded: |_, _| None,
        handed: |_, _| false,
        frontmost: |_| false,
        quiet: |_, _| false,
        dialog_up: || false,
        hand_lan_over: |_| {},
        holds_dialog: false,
        names_holds: false,
    };

    /// The live arms of a windowed macOS instance.
    pub(crate) fn live() -> Self {
        Self {
            trigger_local_network: live_trigger_local_network,
            start_local_network_probe: live_start_local_network_probe,
            take_local_network_probe: live_take_local_network_probe,
            lan_answer_moved: crate::network_moved::local_network_moved,
            open_pane: live_open_access_pane,
            record: live_record,
            recorded: live_recorded,
            handed: live_handed,
            frontmost: App::system_state_frontmost,
            quiet: App::step_moment_is_quiet,
            dialog_up: aterm_types::macos_setup::macos_dialog_up,
            hand_lan_over: aterm_types::macos_setup::lan_first_use::hand_over,
            holds_dialog: true,
            names_holds: true,
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
    /// What held the Local Network step from its moment at the last look that found it held
    /// ([`App::local_network_holds`]), as aterm.log last said it: a look that finds the same
    /// says nothing.
    local_network_held: Option<Vec<String>>,
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
    /// The regressed grant reads off in a build newer than the one that verified it
    /// ([`App::note_fda_off_after_update`]): the menu line says the update needs it re-allowed.
    fda_after_update: bool,
    /// Whether the window's own Full Disk Access step is declined under the
    /// marker key it was read for (the approved revision's): read once per key,
    /// and again after a decline this process notes.
    fda_declined: Option<(String, bool)>,
    /// Held while one of the window's own macOS gestures is up.
    dialog: Option<MacosDialog>,
    /// The menu line as last composed.
    line: Option<SystemStateLine>,
    /// The latest completed outcome for each revision and row, including a
    /// failed save owed another try. A later outcome replaces the earlier one.
    recorded: Vec<RecordedAccess>,
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
    /// No revision stood at a look since the last one that saw it: a revision standing again
    /// admits a Developer Tools step queued meanwhile ([`App::rearm_access_steps`]).
    revision_lapsed: bool,
    /// The launch check found Full Disk Access off on a Mac where aterm never
    /// verified it, and no grant was verified here since
    /// ([`App::note_fda_unverified_here`]): a new approval brings the window's
    /// own page back.
    fda_unverified_here: bool,
}

struct RecordedAccess {
    revision: u64,
    access: Access,
    outcome: AccessOutcome,
    retry_at: Option<Instant>,
}

impl RecordedAccess {
    fn save(
        &mut self,
        record: fn(u64, Access, AccessOutcome) -> std::io::Result<()>,
        now: Instant,
    ) {
        match record(self.revision, self.access, self.outcome) {
            Ok(()) => self.retry_at = None,
            Err(error) => {
                if self.retry_at.is_none() {
                    aterm_log::warn!(
                        "system state: the {} access record for revision {} was not written: {error}",
                        self.access.name(),
                        self.revision
                    );
                }
                self.retry_at = Some(now + ACCESS_RECORD_RETRY);
            }
        }
    }
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
            local_network_held: None,
            opt_outs: None,
            pane_act: None,
            pane_until: None,
            fda_regressed: false,
            fda_after_update: false,
            fda_declined: None,
            dialog: None,
            line: None,
            recorded: Vec::new(),
            retired: false,
            join_pending_retired: false,
            warmup_gate: None,
            seen_revision: None,
            revision_lapsed: false,
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

/// Why System State's Local Network step will not ask macOS on a network it has no subnet to
/// name on ([`lan_target`]): one with only public IPv4 addresses (a campus network may hand those
/// out), or an IPv6-only one.
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOCAL_NETWORK_STEP_NO_LAN: &str = "this Mac's network has no private or \
    link-local IPv4 subnet, so System State's Local Network step has nothing to ask macOS with";

/// Why System State's Local Network step will not run in a window here (pure): `config_off`,
/// the reason `aterm.toml` gives ([`crate::app_config::local_network_step_off_setting`]), else
/// [`LOCAL_NETWORK_STEP_NO_LAN`] where this Mac is on a network (an IPv4 LAN segment among
/// `interfaces`, or `ipv6_network`) but its `interfaces` give the step no LAN to name, so its
/// trigger asks nothing ([`App::tick_local_network_step`]); `None` while it may, a Mac on no
/// network yet included (the step asks once it joins one it can name). Uses that wait for the
/// step go where it will not run.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn local_network_step_off_with(
    config_off: Option<&'static str>,
    interfaces: &[LanInterface],
    ipv6_network: bool,
) -> Option<&'static str> {
    config_off.or_else(|| {
        (lan_target(interfaces).is_none()
            && (ipv6_network || interfaces.iter().any(|interface| interface.broadcast)))
        .then_some(LOCAL_NETWORK_STEP_NO_LAN)
    })
}

/// [`local_network_step_off_with`] of `aterm.toml` and this Mac's interfaces as they read now
/// (`getifaddrs`: no socket, so no question).
#[cfg(target_os = "macos")]
pub(crate) fn local_network_step_off_here() -> Option<&'static str> {
    local_network_step_off_with(
        crate::app_config::local_network_step_off_setting(),
        &interface_addresses(),
        ipv6_network_up(),
    )
}

/// `status`'s `setup=` (pure): where this Mac's setup stands, from the last status the worker
/// published (its progress record and its prompt-free readings), so a `status` costs no probe.
/// `pending:<row,...>` names each managed row that needs a person, waits, failed or, while the
/// run goes, still differs (`fda`, `devtools`, `vault`, … [`setup_word`]); `done` once the plan
/// finished or a person cancelled it with none of those left, or once System State's headline
/// says this Mac is set up ([`crate::system_state::Headline::set_up`]) before its run finished,
/// as while its Secrets stage parks to ask again by itself for an ALab vault that did not answer,
/// which the page does not count; `pending` while a run goes with none read; `-` before the
/// worker said anything (no setup in this process).
pub(crate) fn setup_field(status: &SetupStatus) -> String {
    let selection = status
        .plan
        .as_ref()
        .map_or_else(Selection::none, |plan| plan.selection);
    let ended = status.finished || status.cancelled;
    let mut pending: Vec<&str> = Vec::new();
    for observation in
        status.observations_for(status.plan.as_ref().map(|plan| plan.vault_url.as_str()))
    {
        let left = match observation.standing {
            Standing::NeedsYou | Standing::Waiting | Standing::Failed => true,
            // A row a finished setup set once and a person changed since is theirs.
            Standing::Differs => !ended,
            _ => false,
        };
        let word = setup_word(observation.subject);
        if left
            && crate::system_state::manages(selection, observation.subject)
            && !pending.contains(&word)
        {
            pending.push(word);
        }
    }
    // What the page says: a run going, a step that waits or failed, or a row not read yet keeps
    // its headline off set up.
    let set_up = || {
        status.plan.as_ref().is_some_and(|plan| {
            let rows = crate::system_state::rows(status, plan.selection);
            crate::system_state::headline(status, &rows, Some(plan)).set_up
        })
    };
    if !pending.is_empty() {
        format!("pending:{}", pending.join(","))
    } else if ended || set_up() {
        "done".to_owned()
    } else if status.plan.is_some() || status.run.is_some() {
        "pending".to_owned()
    } else {
        "-".to_owned()
    }
}

/// A System State row's word in `setup=` ([`setup_field`]).
const fn setup_word(subject: Subject) -> &'static str {
    match subject {
        Subject::Access(Access::FullDiskAccess) => "fda",
        Subject::Access(Access::DeveloperTools) => "devtools",
        Subject::Access(Access::Debugger) => "debugger",
        Subject::Access(Access::LocalNetwork) => "lan",
        Subject::Access(Access::Accessibility) => "ax",
        Subject::Setting(Setting::CapsLock) => "capslock",
        Subject::Setting(Setting::NaturalScrolling) => "scrolling",
        Subject::Setting(Setting::TrackpadScrolling) => "trackpad",
        Subject::Setting(Setting::Desktop) => "desktop",
        Subject::Setting(Setting::Widgets) => "widgets",
        Subject::Setting(Setting::Dock) => "dock",
        Subject::Setting(Setting::Messages) => "messages",
        Subject::Setting(Setting::FaceTime) => "facetime",
        Subject::Setting(Setting::Spotlight) => "spotlight",
        Subject::Setting(Setting::Permissions) => "permissions",
        Subject::Setting(Setting::Secrets) => "vault",
        Subject::Setting(Setting::Repositories) => "repos",
        Subject::Sync(SyncKind::Peers) => "sync-peers",
        Subject::Sync(SyncKind::Identity) => "sync-identity",
        Subject::Sync(SyncKind::Fleet) => "sync-fleet",
        Subject::Sync(SyncKind::Accounts) => "sync-accounts",
        Subject::Sync(SyncKind::SharedData) => "sync-shared-data",
        Subject::Sync(SyncKind::Name) => "sync-name",
    }
}

/// The one quiet signal (§3.6, pure): `System State: <row> needs you`, from the managed rows
/// that wait for a person plus a verified Full Disk Access grant that reads off. A failed row,
/// or a step the last run could not finish, reads `System State: <row> failed` (`1 step
/// failed`) once nothing tries it again by itself (§3.5); a Check's result never does, since a
/// Check only reads and pressing it again is its remedy ([`SetupStatus::failed_steps`]).
/// Several are counted (`2 need you`, `1 needs you, 1 failed`). A row Restore could not put
/// back, alone, reads `System State: Restore failed for <row>`. The note is the Secrets row's
/// while it needs you (a waiting vault approval code, or a move to Applications), else the
/// GitHub repositories row's while it needs you (its token's remedy, in the page's words).
/// `None` when nothing does, and while a review closed unanswered waits for an answer: nobody
/// approved its permission steps, so none of them needs anyone yet (§3.3).
pub(crate) fn menu_line(status: &SetupStatus, fda_regressed: bool) -> Option<SystemStateLine> {
    if status.unanswered {
        return None;
    }
    let selection = status
        .plan
        .as_ref()
        .map_or_else(Selection::none, |plan| plan.selection);
    let managed_with = |standing: Standing| {
        status
            .observations_for(status.plan.as_ref().map(|plan| plan.vault_url.as_str()))
            .filter(move |observation| {
                observation.standing == standing
                    && crate::system_state::manages(selection, observation.subject)
            })
            .map(|observation| observation.subject)
    };
    let fda = Subject::Access(Access::FullDiskAccess);
    let repositories = Subject::Setting(Setting::Repositories);
    let tools = status.repositories_tools_wait();
    // Apple's Command Line Tools installing is a wait that ends by itself (§3.3): no line.
    let mut needs: Vec<Subject> = managed_with(Standing::NeedsYou)
        .filter(|subject| {
            *subject != repositories
                || !matches!(tools, Some(ToolsWait::Installing | ToolsWait::Queued))
        })
        .collect();
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
    // counted but not named, since the page's note names it. A row Restore could not put back
    // is said as Restore's (§3.4).
    let mut restore: Vec<Option<Subject>> = Vec::new();
    for step in status.failed_steps(status.plan.as_ref()) {
        match step.row {
            _ if step.restore => restore.push(step.row),
            Some(row) if needs.contains(&row) || failed.contains(&Some(row)) => {}
            row => failed.push(row),
        }
    }
    if needs.is_empty() && failed.is_empty() {
        let label = match restore.as_slice() {
            [] => return None,
            [Some(row)] => format!("System State: Restore failed for {}", row.name()),
            [None] => "System State: Restore failed".to_owned(),
            rows => format!("System State: Restore failed for {} settings", rows.len()),
        };
        return Some(SystemStateLine { label, note: None });
    }
    failed.extend(restore);
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
    let note_of = |setting: Setting| {
        status
            .observations_for(status.plan.as_ref().map(|plan| plan.vault_url.as_str()))
            .find(|observation| {
                observation.subject == Subject::Setting(setting)
                    && observation.standing == Standing::NeedsYou
                    && selection.contains(setting)
            })
            .and_then(|observation| observation.note.clone())
            .filter(|note| !note.trim().is_empty())
    };
    // Their tab asking for the password says where the person is needed, as the headline does.
    let note = note_of(Setting::Secrets).or_else(|| {
        note_of(Setting::Repositories).map(|note| {
            if tools == Some(ToolsWait::Password) {
                crate::system_state::TOOLS_PASSWORD_HEADLINE.to_owned()
            } else if tools == Some(ToolsWait::Recovered) {
                crate::system_state::TOOLS_RECOVERED_HEADLINE.to_owned()
            } else {
                note
            }
        })
    });
    Some(SystemStateLine { label, note })
}

impl App {
    /// The approved setup revision whose access steps may run: the admitted
    /// plan's revision while the permission gate allows it, else none.
    pub(crate) fn access_revision(&self) -> Option<u64> {
        self.workstation_setup.access_revision()
    }

    /// Whether the latest answer taken here for this approval and row is Not
    /// now, including an answer whose history write is still owed.
    pub(crate) fn access_declined_here(&self, access: Access) -> bool {
        self.access_revision().is_some_and(|revision| {
            self.access_steps.recorded.iter().any(|record| {
                record.revision == revision
                    && record.access == access
                    && record.outcome == AccessOutcome::Declined
            })
        })
    }

    /// Whether the access record's latest line for this approval and row is `declined`: a Not
    /// now an earlier process took, which [`Self::access_declined_here`] does not hold. Never
    /// without an approved revision.
    pub(crate) fn access_declined_on_record(&self, access: Access) -> bool {
        self.access_revision().is_some_and(|revision| {
            (self.access_steps.reach.recorded)(revision, access) == Some(AccessOutcome::Declined)
        })
    }

    /// The `[privacy]` switches leave the steps on: `enabled`, `check` and
    /// `notice` ([`crate::app_config::Config::access_steps_allowed`]).
    pub(crate) fn access_steps_allowed(&self) -> bool {
        self.config.access_steps_allowed()
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

    /// Whether the access record holds the access revision's `handed` for `access`: the row's
    /// macOS page came up in the revision, whichever door brought it. The record names no TCC
    /// identity and no Mac.
    pub(crate) fn handed_this_revision(&self, access: Access) -> bool {
        self.access_revision()
            .is_some_and(|revision| (self.access_steps.reach.handed)(revision, access))
    }

    /// Whether the window's own Developer Tools page came up in the access revision: its marker
    /// names the revision's step, or the record holds its `handed`. `aterm doctor` rewrites
    /// the one-line marker under the running version, so the marker alone would bring the page
    /// up a second time in the revision. That marker names no bundle or Mac, nor does the
    /// record. (Full Disk Access's marker names both, so a page brought up while no revision
    /// stands keeps the revision's step there instead: `devtools_helper::fda_opened_word`.)
    /// While no revision stands no page came up in one, whatever the marker's version says: a
    /// reading then asks, and the window's own step waits queued for the revision and is judged
    /// against it there (`App::devtools_hand_over_carrying`).
    pub(crate) fn devtools_shown_this_revision(&self) -> bool {
        let Some(revision) = self.access_revision() else {
            return false;
        };
        let key = atpkg::devtools::revision_key(revision);
        (self.devtools_helper.reach().last_opened)().is_some_and(|(word, _)| word == key)
            || self.handed_this_revision(Access::DeveloperTools)
    }

    /// Whether this bundle's and Mac's newest Full Disk Access opening
    /// (`atpkg::devtools::PageMarkers::opened_as`) names the approved revision's step
    /// (`atpkg::devtools::same_step`): the window's own page came up under it. Each bundle's and
    /// Mac's opening is kept beside the others' (`atpkg::devtools::PageMarkers::note_opened_beside`),
    /// so another one's page under the revision leaves this one's.
    pub(crate) fn fda_shown_this_revision(&self) -> bool {
        let key = self.access_marks().fda;
        (self.devtools_helper.fda_reach().opened_as)(&key)
            .is_some_and(|(word, _)| atpkg::devtools::same_step(&word, &key))
    }

    /// Whether System State is the visible view of a focused window ([`StepsReach::frontmost`]).
    pub(crate) fn system_state_in_front(&self) -> bool {
        (self.access_steps.reach.frontmost)(self)
    }

    /// Whether System State's row for `access` offers its button: the setup run reads it as
    /// needing the person.
    pub(crate) fn system_state_offers(&self, access: Access) -> bool {
        self.native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .is_some_and(|instance| {
                matches!(self.native_runtime.app(instance),
                    Some(crate::native_app::NativeApp::SystemState(state)) if state.can_open_access(access))
            })
    }

    /// Whether one of the window's own access gestures is up: a hand-over panel (or the
    /// permission walk between its pages), a folder warm-up walking, an installer terminal, or
    /// an alert or pane's hold.
    pub(crate) fn access_gesture_up(&self, now: Instant) -> bool {
        self.devtools_helper.panel().is_some()
            || self.permission_walk.active()
            || self.access_gesture_up_beside_walk(now)
    }

    /// [`Self::access_gesture_up`] leaving out the permission walk and its own panel: a
    /// hand-over panel the walk did not bring up, a folder warm-up walking, an installer
    /// terminal, or an alert or pane's hold.
    pub(crate) fn access_gesture_up_beside_walk(&self, now: Instant) -> bool {
        self.developer_tools_install_active()
            || self
                .devtools_helper
                .panel()
                .is_some_and(|panel| panel.origin() != devtools_helper::Origin::Walk)
            || self.consent_warmup.is_live()
            || self.access_steps.local_network_alert_up(now)
            || self.access_steps.pane_up(now)
    }

    /// Whether another macOS gesture owns the dialog slot (inert in headless tests).
    pub(crate) fn access_dialog_up(&self) -> bool {
        (self.access_steps.reach.dialog_up)()
    }

    /// Reserve the worker's dialog slot before a pane, panel or folder worker
    /// can start. A moment check alone cannot keep a worker from winning it. A slot the window
    /// holds already is taken as won: one gesture going on from its pane to its panel. Its
    /// moment keeps a second gesture from starting beside the first
    /// ([`Self::access_step_moment`], [`Self::control_ask_moment`]).
    pub(crate) fn reserve_access_dialog(&mut self) -> bool {
        if !self.access_steps.reach.holds_dialog || self.access_steps.dialog.is_some() {
            return true;
        }
        self.access_steps.dialog = MacosDialog::when_none_up(Duration::ZERO);
        self.access_steps.dialog.is_some()
    }

    /// Restore custody of an existing gesture without opening another prompt.
    pub(crate) fn recover_access_dialog(&mut self) {
        if self.access_steps.reach.holds_dialog && self.access_steps.dialog.is_none() {
            self.access_steps.dialog = Some(MacosDialog::up());
        }
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

    /// Win the dialog slot for a page of the control verb's permission walk while the walk holds
    /// none (its first page), under the steps' one-gesture rule: `false` while another of the
    /// window's gestures ([`Self::access_gesture_up_beside_walk`]) or another macOS dialog of
    /// this process is up, and while a person's Install… press waits for the slot or its tab is
    /// on its way ([`Self::install_press_waits`]): a person's press never waits behind a
    /// control client's walk. A guard the window holds then is the walk's own, taken at a turn's
    /// end while it waited ([`Self::settle_access_steps`]): it is given up and the slot won only
    /// when none is up, so a setup-worker prompt that was up first is waited out, not stacked on.
    pub(crate) fn reserve_walk_dialog(&mut self, now: Instant) -> bool {
        if self.access_gesture_up_beside_walk(now) || self.install_press_waits() {
            return false;
        }
        self.drop_access_dialog();
        !self.access_dialog_up() && self.reserve_access_dialog()
    }

    /// Whether a person's press of the Repositories row's Install… waits for the dialog slot
    /// (`Install::PressedQueued`) or its tab is on its way to landing (`Install::Pressed`), where
    /// it takes the slot for its installer.
    pub(crate) const fn install_press_waits(&self) -> bool {
        matches!(
            self.workstation_setup.developer_tools,
            crate::clt_install::Install::PressedQueued { .. }
                | crate::clt_install::Install::Pressed
        )
    }

    /// Give up the window's dialog slot outright: the permission walk hands it to the debugger
    /// change, whose administrator dialog takes a slot of its own.
    pub(crate) fn drop_access_dialog(&mut self) {
        self.access_steps.dialog = None;
    }

    /// A failed start releases its reservation; an already visible gesture
    /// keeps the same guard, including when another helper replaces its panel.
    pub(crate) fn release_unused_access_dialog(&mut self, now: Instant) {
        if !self.access_gesture_up(now) && self.access_steps.dialog.take().is_some() {
            self.tick_developer_tools_install();
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
    /// revision, the switches on, System State frontmost, no macOS gesture up,
    /// the quiet-moment rules ([`StepsReach::quiet`]), and System Settings asking
    /// nothing ([`Self::system_settings_asks`]), read last since it may read the
    /// window list.
    pub(crate) fn local_network_moment(&self, now: Instant) -> bool {
        self.local_network_moment_but_settings(now) && !self.system_settings_asks()
    }

    /// [`Self::local_network_moment`] but for System Settings asking, for a reader that reads that
    /// itself in this look, after (`App::waited_out_local_network`, whose look reads it last), or
    /// in its own order (`App::access_step_moment`), so one look reads it once.
    pub(crate) fn local_network_moment_but_settings(&self, now: Instant) -> bool {
        self.access_revision().is_some()
            && self.access_steps_allowed()
            && (self.access_steps.reach.frontmost)(self)
            && self.no_gesture_up(now)
            && (self.access_steps.reach.quiet)(self, now)
    }

    /// Whether System Settings asks something of the person now, what the permission walk's Full
    /// Disk Access done words wait for (`App::turn_after_fda`): one of its own windows over its
    /// main one (its password window, or macOS's Quit & Reopen sheet after a Full Disk Access
    /// switch: `devtools_helper::Lane::settings_asks`, which also counts one seen standing over a
    /// main window since moved to another Space, hidden or minimized), or a quit asked of this
    /// process (`WalkReach::quit_asked`: Quit & Reopen asks for one before its sheet goes). A walk
    /// the close box ended, or a page a step opened, leaves System Settings up with that question
    /// standing, and macOS's Local Network alert, a later step's page or dialog (the debugger's
    /// among them, `App::debugger_step_moment`) and the walk that starts by itself
    /// (`App::look_for_walk_start`) are not put beside it.
    pub(crate) fn system_settings_asks(&self) -> bool {
        (self.permission_walk.reach().quit_asked)() || self.devtools_helper.settings_asks()
    }

    /// [`Self::system_settings_asks`], or the question System Settings was seen asking still
    /// listed over its main window off screen past `devtools_helper::ASKING_KEPT_FOR` after the
    /// person left it (`devtools_helper::Lane::settings_may_ask`): what a gesture that opens one
    /// of System Settings' pages waits for (the walk's start and its next page, the access steps
    /// after Local Network), since an `open` under a standing sheet is lost
    /// (`docs/measured/first-launch-permission-walk-0.145.0-2026-10-28.md`, finding 3). Only a
    /// reading with its main window on screen ends it. The Local Network alert and the debugger's
    /// dialog wait for [`Self::system_settings_asks`] alone.
    pub(crate) fn system_settings_may_ask(&self) -> bool {
        (self.permission_walk.reach().quit_asked)() || self.devtools_helper.settings_may_ask()
    }

    /// What holds the Local Network step from its moment ([`Self::local_network_moment`]) now,
    /// in aterm.log's words: each unmet condition once, in the moment's order, and empty when
    /// none is. A lasting hold (no approved revision, a `[privacy]` switch off) is said alone, so
    /// the words change only when it does. System State not frontmost is said alone too, as the
    /// part of it that fails ([`Self::system_state_front_gap`], read only when the step's own arm
    /// says it fails), so keys typed, windows or apps switched to and the window's gestures and
    /// dialogs coming and going while the person is elsewhere change nothing said. With System
    /// State in front come the window's gestures and dialogs, the quiet moment's parts
    /// ([`Self::step_quiet_gaps`], read only when the step's own arm says it fails; a part both
    /// name is said once), and System Settings asking (read last, as the moment reads it, only
    /// when nothing else holds the step: it may read the window list).
    pub(crate) fn local_network_holds(&self, now: Instant) -> Vec<String> {
        self.local_network_holds_read(now, None)
    }

    /// [`Self::local_network_holds`], with System Settings asking taken from `settings_asked`
    /// when the caller read it already in this look (`App::start_walk_by_itself`).
    pub(crate) fn local_network_holds_read(
        &self,
        now: Instant,
        settings_asked: Option<bool>,
    ) -> Vec<String> {
        let lasting: Vec<String> = [
            self.access_revision()
                .is_none()
                .then_some("no approved setup revision admits the Permissions group"),
            (!self.access_steps_allowed())
                .then_some("a [privacy] switch (enabled, check or notice) is off"),
        ]
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .collect();
        if !lasting.is_empty() {
            return lasting;
        }
        let mut holds: Vec<String> = Vec::new();
        let mut hold = |words: &str| {
            if !holds.iter().any(|held| held == words) {
                holds.push(words.to_owned());
            }
        };
        if !(self.access_steps.reach.frontmost)(self) {
            return vec![
                self.system_state_front_gap()
                    .unwrap_or("System State is not the visible view of a focused window")
                    .to_owned(),
            ];
        }
        if let Some(panel) = self.devtools_helper.panel() {
            hold(if panel.origin() == devtools_helper::Origin::Walk {
                "the permission walk's helper panel is up"
            } else {
                "a System Settings helper panel is up"
            });
        }
        if self.permission_walk.active() {
            hold("a permission walk is under way");
        }
        if self.developer_tools_install_active() {
            hold("the Developer Tools installer is up");
        }
        if self.consent_warmup.is_live() {
            hold("the launch folder pass is walking");
        }
        if self.access_steps.local_network_alert_up(now) {
            hold("the Local Network alert's hold");
        }
        if self.access_steps.pane_up(now) {
            hold("the hold of a System Settings pane aterm opened");
        }
        if self.access_dialog_up() {
            hold("a macOS dialog or page this process started counts as up");
        }
        if !(self.access_steps.reach.quiet)(self, now) {
            let gaps = self.step_quiet_gaps(now);
            if gaps.is_empty() {
                hold("the quiet moment");
            }
            for gap in &gaps {
                hold(gap);
            }
        }
        if holds.is_empty() {
            if settings_asked.unwrap_or_else(|| self.devtools_helper.settings_asks()) {
                holds.push(format!(
                    "System Settings shows a window of its own over its main one (its password \
                     window, or Quit & Reopen's sheet), or, with its main window off screen, the \
                     one last seen there is still listed over it (for at most {} min after the \
                     person left it)",
                    devtools_helper::ASKING_KEPT_FOR.as_secs() / 60
                ));
            }
            if (self.permission_walk.reach().quit_asked)() {
                holds.push(
                    "a quit is asked of this process (System Settings' Quit & Reopen)".to_owned(),
                );
            }
        }
        holds
    }

    /// Say in aterm.log what holds the Local Network step ([`Self::local_network_holds`]) when it
    /// differs from what was last said, so a step that waits names its hold once per change,
    /// never once per look; never on an instance whose step cannot run
    /// ([`StepsReach::names_holds`]). The line it wrote, if it wrote one.
    fn note_local_network_hold(&mut self, now: Instant, revision: Option<u64>) -> Option<String> {
        if !self.access_steps.reach.names_holds {
            return None;
        }
        let holds = self.local_network_holds(now);
        if holds.is_empty() || self.access_steps.local_network_held.as_ref() == Some(&holds) {
            return None;
        }
        let of = revision.map_or_else(String::new, |revision| format!(" (revision {revision})"));
        let line = format!(
            "system state: the Local Network step waits{of}: {}",
            holds.join("; ")
        );
        aterm_log::info!("{line}");
        self.access_steps.local_network_held = Some(holds);
        Some(line)
    }

    /// The one-gesture rule: none of the window's own access gestures up
    /// ([`Self::access_gesture_up`]) and no macOS dialog of this process up.
    fn no_gesture_up(&self, now: Instant) -> bool {
        !self.access_gesture_up(now) && !self.access_dialog_up()
    }

    /// A control verb's moment (`devtools_helper::Origin::Ctl`): its own quiet moment, with no
    /// approved revision and no System State in front needed (`App::devtools_moment_is_quiet`),
    /// held to the steps' one-gesture rule, so its page waits out a panel or installer the window
    /// has up and the Local Network alert's or a pane's hold (bounds, not readings: macOS does not
    /// say when either closes).
    pub(crate) fn control_ask_moment(&self, now: Instant) -> bool {
        self.no_gesture_up(now) && self.devtools_moment_is_quiet(now)
    }

    /// Whether the Local Network step of the approved revision is still to run
    /// in this process.
    pub(crate) fn local_network_pending(&self) -> bool {
        self.config.swarm_enabled()
            && self
                .access_revision()
                .is_some_and(|revision| self.access_steps.local_network_done != Some(revision))
    }

    /// The Local Network step of `revision` ran in this process, as its trigger leaves it.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn local_network_ran_for_test(&mut self, revision: u64) {
        self.access_steps.local_network_done = Some(revision);
    }

    /// Whether the approved revision's Local Network step is still to run in this process
    /// ([`Self::local_network_pending`]), its record read first
    /// ([`Self::local_network_on_record`]), so a revision the record already holds is done at this
    /// look. The first permission walk to start by itself waits for it, as every step after
    /// it does (`App::look_for_walk_start`): its alert goes first in the run, and its hold
    /// ([`LOCAL_NETWORK_ALERT_HOLD`], ended early by the LAN re-probe's answer,
    /// [`Self::on_local_network_probe_ready`]) is one of the window's gestures
    /// ([`Self::access_gesture_up`]). The walk's wait ends once the step alone has held it ready
    /// for `permission_walk::LOCAL_NETWORK_WAIT` of unbroken looks
    /// (`App::waited_out_local_network`): the walk then goes first, and the step asks once it is
    /// over.
    pub(crate) fn local_network_step_to_come(&mut self) -> bool {
        if let Some(revision) = self.access_revision() {
            self.local_network_on_record(revision);
        }
        self.local_network_pending()
    }

    /// Read the access record's Local Network row for `revision`, once per revision: a row it
    /// already holds (an earlier process's step) makes the step done in this one. Whether this read
    /// found it.
    fn local_network_on_record(&mut self, revision: u64) -> bool {
        if self.access_steps.local_network_read == Some(revision) {
            return false;
        }
        self.access_steps.local_network_read = Some(revision);
        let recorded = (self.access_steps.reach.recorded)(revision, Access::LocalNetwork).is_some();
        if recorded {
            self.access_steps.local_network_done = Some(revision);
        }
        recorded
    }

    /// The step moment: an access step after Local Network may put its page, panel or folder
    /// dialog on screen now ([`Self::local_network_moment`], with Local Network done, and System
    /// Settings asking nothing, nor a question seen asking lapsed off screen:
    /// [`Self::system_settings_may_ask`], since a step's page opened under it is lost), and no
    /// first walk that may still start by itself waits for the
    /// person to come back ([`Self::walk_waits_for_the_person`]), so no step takes its first page.
    /// The debugger step, judged by its own admission, waits on the same run order
    /// (`App::debugger_step_moment`: Local Network still to run, System Settings asking).
    pub(crate) fn access_step_moment(&self, now: Instant) -> bool {
        !self.local_network_pending()
            && self.local_network_moment_but_settings(now)
            && !self.system_settings_may_ask()
            && !self.walk_waits_for_the_person(now)
    }

    /// Drain a completed 1 s × 30 s LAN grant re-probe: release the Local Network alert hold and
    /// its dialog slot first so the worker's Secrets lane sees no dialog up when [`AccessOutcome::Granted`]
    /// wakes it, then record `Granted` when a LAN server answered. Whatever it heard, the LAN
    /// readers look again now ([`StepsReach::lan_answer_moved`]): a grant or a Don't Allow moved
    /// the answer, and a probe that heard nothing (no ALab vault on this network, or an answer
    /// after its 30 s) still ends with the question asked, which a look before it did not know.
    pub(crate) fn on_local_network_probe_ready(&mut self, now: Instant) {
        if !self.config.swarm_enabled() {
            cancel_local_network_probe();
            self.access_steps.local_network_until = None;
            self.release_unused_access_dialog(now);
            return;
        }
        let Some((revision, probe)) = (self.access_steps.reach.take_local_network_probe)() else {
            return;
        };
        if self.access_steps.local_network_done != Some(revision) {
            return;
        }
        self.access_steps.local_network_until = None;
        self.release_unused_access_dialog(now);
        if probe == LocalNetworkProbe::Granted {
            self.record_access_at(revision, Access::LocalNetwork, AccessOutcome::Granted);
            aterm_log::info!(
                "system state: Local Network granted (LAN answered, revision {revision})"
            );
        }
        (self.access_steps.reach.lan_answer_moved)();
    }

    /// The Local Network step, first in the run: once per approved revision, a UDP connect
    /// (nothing sent) to a unicast LAN address, which makes macOS ask its Local Network
    /// question; recorded `triggered`. A revision already recorded is done. With no LAN to name
    /// the other steps go on, and it looks again every [`LOCAL_NETWORK_RETRY`] at its moment
    /// only: a retry due away from it is moved on. A retry is dropped only for another approved
    /// revision; while none is approved it is kept but arms no deadline
    /// ([`Self::next_access_step_deadline`]), so the same revision coming back looks again.
    pub(crate) fn tick_local_network_step(&mut self, now: Instant) {
        self.on_local_network_probe_ready(now);
        if !self.config.swarm_enabled() {
            return;
        }
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
            if self.access_steps.local_network_done.is_none() {
                let _ = self.note_local_network_hold(now, None);
            }
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
        if self.local_network_on_record(revision) {
            return;
        }
        if !self.local_network_moment(now) {
            let _ = self.note_local_network_hold(now, Some(revision));
            return;
        }
        self.access_steps.local_network_held = None;
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
            // macOS asked under this step's slot: the Commons runtime may use the LAN now.
            (self.access_steps.reach.hand_lan_over)(revision);
            self.access_steps.local_network_retry = None;
            self.access_steps.local_network_until = Some(now + LOCAL_NETWORK_ALERT_HOLD);
            self.record_access_step(Access::LocalNetwork, AccessOutcome::Triggered);
            // The record now says asked, so a LAN look made before it (`sync.peers`' "macOS has
            // not asked") is stale from here: the readers look again now.
            (self.access_steps.reach.lan_answer_moved)();
            (self.access_steps.reach.start_local_network_probe)(revision);
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
    /// or release the window's dialog guard (held while one of the window's gestures is up, the
    /// permission walk among them, except a control verb's walk that has not won the slot while a
    /// person's Install… press waits for it), record the panels' Not now, spend the access
    /// card's ([`Self::spend_card_not_now`]), bring the pages back for a revision approved in
    /// this process ([`Self::rearm_access_steps`]), and end the retired digest entries once.
    pub(crate) fn settle_access_steps(&mut self, now: Instant) {
        self.on_local_network_probe_ready(now);
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
        // The control verb's walk that has not won the slot takes no guard while a person's
        // Install… press waits for it, so the press's tab can open and land first.
        let walk_gives_way = self.permission_walk.waits_for_slot() && self.install_press_waits();
        let up = self.access_steps.reach.holds_dialog
            && (self.devtools_helper.panel().is_some()
                || self.access_gesture_up_beside_walk(now)
                || (self.permission_walk.active() && !walk_gives_way));
        if up != self.access_steps.dialog.is_some() {
            self.access_steps.dialog = up.then(MacosDialog::up);
        }
        for (revision, access) in self.devtools_helper.take_declines() {
            self.record_access_at(revision, access, AccessOutcome::Declined);
        }
        self.spend_card_not_now();
        // Drain new outcomes first: a later answer replaces a failed save,
        // never the other way around. Saving a completed step raises no prompt.
        self.retry_access_records(now);
        self.debugger_launch.retry_records(now);
        self.release_kept_steps(now);
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
    /// Local answers precede that marker cache, so a failed save still holds
    /// and a later local outcome replaces only that local hold.
    pub(crate) fn fda_declined_in_revision(&mut self) -> bool {
        if self.access_declined_here(Access::FullDiskAccess) {
            return true;
        }
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
        // While the first launch's review is up its headline is the one signal, on a tab in
        // front: no menu line, as no diamond (§3.3, §3.6).
        let line = if self.system_state_review_pending() {
            None
        } else {
            menu_line(status, regressed).map(|mut line| {
                if regressed && self.access_steps.fda_after_update && line.note.is_none() {
                    line.note = Some(FDA_AFTER_UPDATE.to_owned());
                }
                line
            })
        };
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
        let saves = self
            .access_steps
            .recorded
            .iter()
            .filter_map(|record| record.retry_at)
            .map(|at| at.max(now));
        hold.chain(look)
            .chain(retry)
            .chain(saves)
            .chain(self.debugger_launch.record_deadline(now))
            .min()
    }

    /// Record `outcome` for `access` under the approved revision; nothing
    /// without one, and no repeated line while that row's outcome is unchanged.
    /// Returns whether that outcome is durable.
    pub(crate) fn record_access_step(&mut self, access: Access, outcome: AccessOutcome) -> bool {
        self.access_revision()
            .is_some_and(|revision| self.record_access_at(revision, access, outcome))
    }

    fn record_access_at(&mut self, revision: u64, access: Access, outcome: AccessOutcome) -> bool {
        let index = self
            .access_steps
            .recorded
            .iter()
            .position(|record| record.revision == revision && record.access == access);
        let index = if let Some(index) = index {
            let last = &mut self.access_steps.recorded[index];
            if last.outcome == outcome {
                return last.retry_at.is_none();
            }
            last.outcome = outcome;
            last.retry_at = None;
            index
        } else {
            let index = self.access_steps.recorded.len();
            self.access_steps.recorded.push(RecordedAccess {
                revision,
                access,
                outcome,
                retry_at: None,
            });
            index
        };
        self.access_steps.recorded[index].save(self.access_steps.reach.record, Instant::now());
        if (access, outcome) == (Access::FullDiskAccess, AccessOutcome::Declined) {
            self.fda_decline_noted();
        }
        self.access_steps.recorded[index].retry_at.is_none()
    }

    fn retry_access_records(&mut self, now: Instant) {
        for record in &mut self.access_steps.recorded {
            if record.retry_at.is_some_and(|retry| retry <= now) {
                record.save(self.access_steps.reach.record, now);
            }
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
        if !regressed {
            self.access_steps.fda_after_update = false;
        }
        if self.access_steps.fda_regressed != regressed {
            self.access_steps.fda_regressed = regressed;
            aterm_log::info!(
                "system state: a verified Full Disk Access grant {}",
                if regressed { "reads off" } else { "is settled" }
            );
            self.refresh_system_state_menu_line();
        }
    }

    /// A verified Full Disk Access grant still reads off once this process settled, in a build
    /// newer than the one that verified it: a regression the menu line names says, under it,
    /// that the update needs it re-allowed ([`FDA_AFTER_UPDATE`]), until the regression ends.
    pub(crate) fn note_fda_off_after_update(&mut self) {
        if !self.access_steps.fda_after_update {
            self.access_steps.fda_after_update = true;
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
    /// page past any Not now, and never waits behind a helper panel they cannot see
    /// ([`Self::press_meets_unseen_panel`]): the panel's own row brings its page forward, and
    /// any other row's press has the panel give way first. A control client's act (`by_person`
    /// false) never mints the standing yes: it does nothing without an approved revision or with
    /// a `[privacy]` switch off, its Full Disk Access and Developer Tools pages pass the window's
    /// own hand-over rules as [`devtools_helper::Origin::SystemStateAct`], and its Local Network
    /// and Accessibility panes are queued for the step moment ([`Self::tick_queued_pane`]),
    /// answering `false`.
    pub(crate) fn open_access_from_system_state(
        &mut self,
        access: Access,
        by_person: bool,
    ) -> bool {
        let unseen = if by_person {
            self.press_meets_unseen_panel(Some(access), Instant::now())
        } else {
            UnseenPanel::Clear
        };
        let walked = (by_person && unseen != UnseenPanel::BroughtForward)
            .then(|| self.walk_from_row(access))
            .flatten();
        let (opened, how) = if unseen == UnseenPanel::BroughtForward {
            (true, "brought forward")
        } else if let Some(walked) = walked {
            walked
        } else if !by_person && (self.access_revision().is_none() || !self.access_steps_allowed()) {
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

    /// A person's press on System State meets the window's own helper panel (a walk's page, or a
    /// hand-over's), which holds the window's one macOS gesture while it stands
    /// ([`Self::access_gesture_up`]), so every press that needs it (Turn On…, Install…) would
    /// wait for the panel's answer. The panel stands beside System Settings and leaves the screen
    /// whenever another app is active, so with aterm in front, where the press was made, the
    /// person cannot see what waits: a press never waits behind it. One for the panel's own row
    /// (`own`) brings its page forward ([`Self::reopen_panel_page`]); any other has the panel give
    /// way ([`devtools_helper::Closed::GaveWay`]: no Not now, no act on its page, nothing the walk
    /// queued opens after it) and the walk step aside, its pages still to come taken up by the
    /// next walk that starts by itself (`WalkLane::give_way`), and lets the dialog guard go. A
    /// walk between pages, with no panel up, steps aside too, except for a press on the row of
    /// the page it is at, which takes that walk over (`WalkLane::pressed`, in
    /// `Self::walk_from_row`). A panel the person can see (System Settings the active app)
    /// stands. A pane aterm opened with no panel beside it (Local
    /// Network's) holds the guard only for its bounded time, since macOS says nothing when the
    /// person leaves it: a press made back in aterm is past it, so the guard goes then too.
    pub(crate) fn press_meets_unseen_panel(
        &mut self,
        own: Option<Access>,
        now: Instant,
    ) -> UnseenPanel {
        match self
            .devtools_helper
            .panel()
            .map(devtools_helper::Panel::page)
        {
            None if !self.permission_walk.active() => {
                if !self
                    .access_steps
                    .pane_until
                    .take()
                    .is_some_and(|until| now < until)
                {
                    return UnseenPanel::Clear;
                }
                self.release_unused_access_dialog(now);
                aterm_log::info!(
                    "system state: a person's press{} is past the pane aterm opened; its hold \
                     on the dialog guard went",
                    own.map_or_else(String::new, |access| format!(" on {}", access.name()))
                );
                return UnseenPanel::PaneLeft;
            }
            None if own.is_some()
                && self
                    .permission_walk
                    .walk()
                    .and_then(crate::permission_walk::Walk::current)
                    .map(crate::permission_walk::Subject::access)
                    == own =>
            {
                return UnseenPanel::Clear;
            }
            Some(_) if self.devtools_helper.panel_seen() => return UnseenPanel::Clear,
            Some(page) if own == Some(page.access()) => {
                let opened = self.reopen_panel_page(page);
                aterm_log::info!(
                    "system state: {} brought forward for a person's press ({})",
                    page.as_str(),
                    if opened {
                        "System Settings is up at it"
                    } else {
                        "System Settings did not open"
                    }
                );
                return UnseenPanel::BroughtForward;
            }
            _ => {}
        }
        self.permission_walk
            .give_way("a person's press elsewhere on System State, with its page out of sight");
        self.access_steps.pane_until = None;
        self.devtools_helper.give_way();
        self.release_unused_access_dialog(now);
        aterm_log::info!(
            "system state: the helper panel out of sight gave way to a person's press{}",
            own.map_or_else(String::new, |access| format!(" on {}", access.name()))
        );
        UnseenPanel::GaveWay
    }

    /// Bring the page the helper panel stands beside up again in System Settings, through the
    /// opener its hand-over used (`devtools_helper::Reach::open_pane` and `FdaReach::open_pane`,
    /// `menu::open_privacy_settings`; the Accessibility pane's [`StepsReach::open_pane`]): System
    /// Settings comes to the front at the page, and the panel, which shows only beside it, with
    /// it. The panel's own Open ([`devtools_helper::Button::OpenPage`]) and a person's press on
    /// its row ask it, both the owner's, which every hand-over admits whatever aterm read; the
    /// page was brought up and recorded already. A headless instance's openers are inert.
    /// `true` when System Settings opened.
    pub(crate) fn reopen_panel_page(&mut self, page: devtools_helper::Page) -> bool {
        use devtools_helper::Page;
        let opened = match page {
            Page::DeveloperTools => (self.devtools_helper.reach().open_pane)(),
            Page::FullDiskAccess => (self.devtools_helper.fda_reach().open_pane)(),
            Page::Accessibility => (self.access_steps.reach.open_pane)(self, Access::Accessibility),
        };
        aterm_log::info!(
            "{}: the page again{}",
            page.as_str(),
            if opened {
                ""
            } else {
                " (System Settings did not open)"
            }
        );
        opened
    }

    /// A person's press on the Full Disk Access, Developer Tools or Accessibility row starts the
    /// permission walk with that row's page first, going on to every other grant missing
    /// (`crate::permission_walk`); a walk at another page gives way to it, and one at its page is
    /// the person's from there on, so a control verb's walk waiting there opens it now
    /// (`WalkLane::pressed`). `None` leaves the
    /// press to its own page: another row, a headless instance, or a grant the window reads held,
    /// whose page an owner's press opens anyway.
    fn walk_from_row(&mut self, access: Access) -> Option<(bool, &'static str)> {
        use crate::permission_walk::{Started, Subject};
        let subject = match access {
            Access::FullDiskAccess => Subject::FullDiskAccess,
            Access::DeveloperTools => Subject::DeveloperTools,
            Access::Accessibility => Subject::Accessibility,
            Access::LocalNetwork | Access::Debugger => return None,
        };
        let now = Instant::now();
        self.take_walk_close(now);
        match self.permission_walk.walk().map(|walk| walk.current()) {
            Some(current) if current == Some(subject) => {
                self.permission_walk.pressed();
                self.drive_permission_walk(now);
                return Some((true, "walking"));
            }
            Some(_) => self.permission_walk.end("a person's press on another row"),
            None => {}
        }
        if !self.walk_missing(subject, true) {
            self.drive_permission_walk(now);
            return None;
        }
        match self.start_permission_walk(&[subject], "a person's press", now) {
            Started::Walking { .. } => Some((true, "walking")),
            Started::Headless | Started::Granted | Started::Recent | Started::Declined => None,
        }
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
        let eligible = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .is_some_and(|instance| {
                matches!(self.native_runtime.app(instance),
                    Some(crate::native_app::NativeApp::SystemState(state)) if state.can_open_access(access))
            });
        if !eligible {
            self.access_steps.pane_act = None;
            aterm_log::info!(
                "system state: {} no longer needs its queued pane (a client's act)",
                access.name()
            );
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

    /// While System State offers the Debugger row's `Turn On…`, keep this window's own reading
    /// of debugger access in hand. The page offers the row from the setup worker's reading; the
    /// window reads its own, and aterm's hardening with it, only when asked, and a press that
    /// finds none (the first after a launch, such as Full Disk Access's Quit & Reopen) is
    /// refused: only a reading tells a press that the build is hardened and the right is off.
    /// A reading in hand is kept, however old (a press reads the rule again past its minute); a
    /// read that brought no reading is retired and read again once it is
    /// [`devtools_helper::RECHECK_EVERY`] old, so one read is out at a time and the next waits
    /// that long after the last fails. Neither while a read is out (retiring the row would void
    /// it) nor while the launch step, which retired one, reads the rule again itself at that pace.
    pub(crate) fn warm_debugger_reading(&mut self) {
        if self.headless || !cfg!(target_os = "macos") || self.debugger_launch.rereads_unread() {
            return;
        }
        let gate = self.config.privacy_probe_gate();
        if !gate.permits() {
            return;
        }
        let retire = match self.consent.debugger_cached(gate) {
            None => false,
            Some(row)
                if row
                    .unread_age()
                    .is_some_and(|age| age >= devtools_helper::RECHECK_EVERY) =>
            {
                !self.consent.debugger_in_flight()
            }
            Some(_) => return,
        };
        let offered = self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::SystemState)
            .is_some_and(|instance| {
                matches!(self.native_runtime.app(instance),
                    Some(crate::native_app::NativeApp::SystemState(state)) if state.can_open_access(Access::Debugger))
            });
        if !offered {
            return;
        }
        if retire {
            self.consent.invalidate_debugger();
        }
        // Dispatches the reading unless one is out (or ends one that never started); its wake
        // observes it ([`App::on_debugger_probe_ready`]).
        let _ = self.consent.debugger(gate);
    }

    /// A revision stands: the window's own steps kept waiting through a page brought up while
    /// none stood (`devtools_helper::Lane::page_came_up`) look at once where this revision brought
    /// their page up already, so a step that can only be refused as shown holds nothing behind it
    /// for the rest of that page's ten minutes (`devtools_helper::Lane::revision_stands`).
    fn release_kept_steps(&mut self, now: Instant) {
        if self.access_revision().is_none() {
            return;
        }
        let kept = self.devtools_helper.kept();
        if kept == [false, false] {
            return;
        }
        let shown = [
            kept[0] && self.devtools_shown_this_revision(),
            kept[1] && self.fda_shown_this_revision(),
        ];
        self.devtools_helper.revision_stands(now, shown);
    }

    /// A new access revision approved while aterm runs (Permissions off and on, or Set up)
    /// brings back the window's own Full Disk Access and Developer Tools pages, as the next
    /// launch would ([`Self::rearm_fda_step`], [`Self::rearm_devtools_step`]), each queued for
    /// the step moment (design §6). The debugger step comes back by the window's own admission
    /// (`DebuggerStepQueue::unpark`), and Local Network is per revision already. The first
    /// revision this process sees is the launch path's, and the same revision coming back
    /// changes nothing, except that a Developer Tools step queued while no revision stood starts
    /// its `spctl` run then ([`Self::list_ahead_for_queued_devtools_step`]). While a `[privacy]`
    /// switch is off nothing is queued and the revision is not taken as seen, so one approved
    /// meanwhile asks once every switch is on.
    pub(crate) fn rearm_access_steps(&mut self, now: Instant) {
        let Some(revision) = self.access_revision() else {
            self.access_steps.revision_lapsed = true;
            return;
        };
        if !self.access_steps_allowed() {
            return;
        }
        let lapsed = std::mem::take(&mut self.access_steps.revision_lapsed);
        let Some(seen) = self.access_steps.seen_revision.replace(revision) else {
            // A step asked before any revision stood waits for this one at its moment.
            self.list_ahead_for_queued_devtools_step();
            return;
        };
        if seen == revision {
            if lapsed {
                // The same revision back: a step asked while none stood waits for it.
                self.list_ahead_for_queued_devtools_step();
            }
            return;
        }
        aterm_log::info!(
            "system state: access revision {revision} approved in this process; its pages ask again"
        );
        self.rearm_fda_step(now);
        self.rearm_devtools_step(now);
        self.list_ahead_for_queued_devtools_step();
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
        if self.config.privacy_probe_gate().permits() && !self.config.privacy_observer_off() {
            // A read whose completion landed after a switch went off was spent on
            // nothing (the launch read among them): it is owed again.
            if self.devtools_helper.forget_switched_off_reading() {
                self.devtools_watch.hold_read();
            }
            if self.devtools_watch.owes_read() && self.any_os_window_focused() {
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
                .fda_wait_out_page(now + Duration::from_secs(window.saturating_sub(ago).max(1)));
        }
        // The folders wait behind it again, as at launch.
        self.consent_launch
            .set_fda(crate::consent_launch::FdaLaunch::Waiting);
    }

    /// A new access approval owes a fresh Developer Tools reading even when
    /// the preceding approval read Granted. Its current completion queues
    /// the page through [`Self::queue_rearmed_devtools_step`].
    fn rearm_devtools_step(&mut self, now: Instant) {
        if !cfg!(target_os = "macos") || !self.devtools_helper.windowed() {
            return;
        }
        if let Some(revision) = self.access_revision() {
            self.devtools_watch.read_for_revision(revision);
            self.consent.refresh_devtools();
            if self.any_os_window_focused() {
                self.read_devtools_list(now);
            }
        }
    }

    /// A fresh new-approval read said NotGranted: preserve the revision's
    /// once-only page and defer behind a page already brought up recently.
    pub(crate) fn queue_rearmed_devtools_step(&mut self, now: Instant) {
        if self.devtools_shown_this_revision() {
            return;
        }
        let reach = self.devtools_helper.reach();
        self.devtools_helper
            .queue(devtools_helper::Origin::Automatic, now);
        let window = atpkg::devtools::REOPEN_AFTER_S;
        if let Some(ago) = (reach.opened_within)(window) {
            self.devtools_helper
                .wait_out_page(now + Duration::from_secs(window.saturating_sub(ago).max(1)));
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
    pub(crate) fn open_access_pane(
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

#[cfg(target_os = "macos")]
static ACTIVE_LOCAL_NETWORK_PROBE_REVISION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(target_os = "macos")]
static READY_LOCAL_NETWORK_PROBE: std::sync::Mutex<Option<(u64, LocalNetworkProbe)>> =
    std::sync::Mutex::new(None);

/// A master-Off change cancels the worker and discards its pending grant without another
/// probe, permission record or join. An in-flight network call observes cancellation next.
pub(crate) fn cancel_local_network_probe() {
    #[cfg(target_os = "macos")]
    {
        ACTIVE_LOCAL_NETWORK_PROBE_REVISION.store(0, std::sync::atomic::Ordering::Release);
        if let Ok(mut ready) = READY_LOCAL_NETWORK_PROBE.lock() {
            *ready = None;
        }
    }
}

#[cfg(target_os = "macos")]
fn live_start_local_network_probe(revision: u64) {
    use std::sync::atomic::Ordering;
    ACTIVE_LOCAL_NETWORK_PROBE_REVISION.store(revision, Ordering::Release);
    if let Ok(mut ready) = READY_LOCAL_NETWORK_PROBE.lock() {
        *ready = None;
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-local-network-probe".into())
        .spawn(move || {
            let outcome = aterm_secrets::client::probe_lan_for_grant(|| {
                ACTIVE_LOCAL_NETWORK_PROBE_REVISION.load(Ordering::Acquire) != revision
            });
            if ACTIVE_LOCAL_NETWORK_PROBE_REVISION.load(Ordering::Acquire) != revision {
                return;
            }
            if let aterm_secrets::client::LanGrantProbe::Granted(ref probed) = outcome
                && let Ok(paths) = aterm_secrets::client::Paths::live()
                && let Some(network) = aterm_secrets::heard::network_key()
            {
                let heard = match probed {
                    aterm_secrets::client::ProbeOutcome::Found(found) => {
                        Some(aterm_secrets::heard::Heard::Found {
                            url: found.url.clone(),
                            signer: found.fingerprint.clone(),
                        })
                    }
                    aterm_secrets::client::ProbeOutcome::AnotherVault(_) => {
                        Some(aterm_secrets::heard::Heard::Absent)
                    }
                    aterm_secrets::client::ProbeOutcome::Absent
                    | aterm_secrets::client::ProbeOutcome::NotThisVault(_) => None,
                };
                if let Some(heard) = heard {
                    let at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |since| since.as_secs());
                    let _ = aterm_secrets::heard::write(
                        &paths,
                        &aterm_secrets::heard::Verdict { at, heard, network },
                    );
                }
            }
            let probe = match outcome {
                aterm_secrets::client::LanGrantProbe::Granted(_) => LocalNetworkProbe::Granted,
                aterm_secrets::client::LanGrantProbe::Refused => LocalNetworkProbe::Refused,
                aterm_secrets::client::LanGrantProbe::Unanswered => LocalNetworkProbe::Unanswered,
            };
            if let Ok(mut ready) = READY_LOCAL_NETWORK_PROBE.lock() {
                *ready = Some((revision, probe));
            }
            let _ = crate::workstation_setup::send_setup_wake(crate::Wake::LocalNetworkProbeReady);
        });
    if let Err(error) = spawned {
        aterm_log::warn!("system state: Local Network grant probe did not start: {error}");
    }
}

#[cfg(not(target_os = "macos"))]
fn live_start_local_network_probe(_revision: u64) {}

#[cfg(target_os = "macos")]
fn live_take_local_network_probe() -> Option<(u64, LocalNetworkProbe)> {
    READY_LOCAL_NETWORK_PROBE
        .lock()
        .ok()
        .and_then(|mut ready| ready.take())
}

#[cfg(not(target_os = "macos"))]
fn live_take_local_network_probe() -> Option<(u64, LocalNetworkProbe)> {
    None
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

/// Whether one of this Mac's interfaces that is up, not loopback and a LAN segment (broadcast,
/// not point-to-point: no VPN's `utun`) has an IPv6 address beyond link-local: this Mac is on an
/// IPv6 network, which the Local Network step cannot name. A link-local address says nothing of
/// a network: `awdl0` has one with none joined.
#[cfg(target_os = "macos")]
fn ipv6_network_up() -> bool {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs writes the head of a list it allocated into `head`.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return false;
    }
    let mut up = false;
    let mut at = head;
    while !at.is_null() && !up {
        // SAFETY: `at` is a node of the list getifaddrs returned, freed only below.
        let node = unsafe { &*at };
        at = node.ifa_next;
        let flags = node.ifa_flags;
        if flags & libc::IFF_UP as u32 == 0
            || flags & libc::IFF_LOOPBACK as u32 != 0
            || flags & libc::IFF_BROADCAST as u32 == 0
            || flags & libc::IFF_POINTOPOINT as u32 != 0
            || node.ifa_addr.is_null()
        {
            continue;
        }
        // SAFETY: a non-null `ifa_addr` points at a sockaddr header.
        let (family, len) = unsafe { ((*node.ifa_addr).sa_family, (*node.ifa_addr).sa_len) };
        if i32::from(family) != libc::AF_INET6 {
            continue;
        }
        // SAFETY: a non-null `ifa_addr` of the node is a sockaddr whose `sa_len` bytes are
        // readable.
        let bytes =
            unsafe { std::slice::from_raw_parts(node.ifa_addr.cast::<u8>(), usize::from(len)) };
        // `sin6_addr` sits after `sin6_len`, `sin6_family`, `sin6_port` and `sin6_flowinfo`.
        let Some(Ok(octets)) = bytes.get(8..24).map(<[u8; 16]>::try_from) else {
            continue;
        };
        let address = std::net::Ipv6Addr::from(octets);
        up = !address.is_unspecified()
            && !address.is_loopback()
            && !address.is_multicast()
            && !address.is_unicast_link_local();
    }
    // SAFETY: `head` came from a successful getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(head) };
    up
}

fn live_record(revision: u64, access: Access, outcome: AccessOutcome) -> std::io::Result<()> {
    let state = aterm_types::dirs::state_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "aterm's state directory is unavailable",
        )
    })?;
    aterm_types::macos_setup::access_record::append(&state, revision, access, outcome)
}

fn live_recorded(revision: u64, access: Access) -> Option<AccessOutcome> {
    let state = aterm_types::dirs::state_dir()?;
    aterm_types::macos_setup::access_record::latest(&state, revision, access)
}

fn live_handed(revision: u64, access: Access) -> bool {
    aterm_types::dirs::state_dir().is_some_and(|state| {
        aterm_types::macos_setup::access_record::read(&state, revision)
            .contains(&(access, AccessOutcome::Handed))
    })
}

#[cfg(test)]
#[path = "system_state_access_tests.rs"]
mod tests;
