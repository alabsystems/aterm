// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's side of System State's Permissions group
//! (`docs/DESIGN-system-state-2026-10-21.md` §3.6, §6): the access steps of an
//! approved setup revision, which macOS gesture is up, a press on a Needs-you
//! row, and the one quiet signal in aterm's menu-bar menu.
//!
//! * The admitted plan's access revision ([`App::access_revision`]; a change to another group
//!   keeps it) is the standing yes. It runs, once per revision, the Local Network trigger, the
//!   Full Disk Access page, the Developer Tools page (once Full Disk Access reads on), and the
//!   folders (only while Full Disk Access is off). Each brings up only macOS's own question, so
//!   any admitted plan runs it. Debugger access is no step of its own: setup's one early
//!   elevation turns developer mode on (`aterm_machines::root_steps`).
//! * A step puts something on screen only at [`App::access_step_moment`], one gesture at a
//!   time; while one is up the window holds a [`MacosDialog`], so
//!   the setup worker's own prompts wait too. The Local Network trigger goes first, at a moment of
//!   its own that waits for no walk still to come, and that walk waits for it and its alert's hold
//!   ([`App::local_network_step_to_come`]) until the step alone has held a ready walk for
//!   `permission_walk::LOCAL_NETWORK_WAIT` of unbroken looks (`App::waited_out_local_network`):
//!   the walk then goes first, and the step asks once it is over. The alert's hold ends once the
//!   LAN re-probe hears the answer, once a worker reading the window list, whose first read the
//!   trigger waits for, saw a new window of the alert's owner come and then, at a read with the
//!   person at the Mac, no window of that owner left on screen or off
//!   ([`App::on_local_network_alert_answered`]), or once the person has been at the Mac for
//!   [`LOCAL_NETWORK_ALERT_HOLD`] ([`App::look_at_local_network_alert`]). A question a first LAN
//!   use asked through the door (`aterm_types::macos_setup::lan_first_use`), or the Commons
//!   gate's first use asked by itself once the wait for the step ran out, is the door's to hold,
//!   for the person's time too: the door's hold starts the same watch before that use asks, where
//!   the window installed it ([`door_question_watch`]), which ends that hold once it sees the
//!   question answered, and the LAN re-probe's answer ends it as well
//!   ([`StepsReach::end_door_hold`]).
//! * A panel's Not now holds the window's page, and a control client's act on its row, for
//!   the rest of its revision (`devtools_helper::declined_for`). A new revision, even one
//!   approved while aterm runs ([`App::rearm_access_steps`]), asks again.
//! * A Needs-you row's press opens its page past any Not now. A control client's act never
//!   mints the yes, and its pages wait for the step moment
//!   ([`App::open_access_from_system_state`]).
//! * Each step appends `<revision> <row> <outcome>` to `<state>/system-state-access`
//!   ([`aterm_types::macos_setup::access_record`]), which System State reads. Local Network
//!   carries the loaded-code subject when known; its readers accept only matching known
//!   subjects.
//! * The menu line ([`menu_line`]) is the only signal posted; the digest entries it replaced
//!   are withdrawn ([`RETIRED_KEYS`]).
//! * A `[privacy]` switch off, and a debugger the person turned off, are told to the setup
//!   worker as the row's wanted state ([`App::publish_access_opt_outs`]).

#[cfg(any(target_os = "macos", test))]
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use aterm_types::macos_setup::opt_out::OptOut;
use aterm_types::macos_setup::{
    Access, AccessOutcome, MacosDialog, Selection, Setting, Standing, Subject, SyncKind,
};

use crate::status_item::SystemStateLine;
use crate::system_state::ToolsWait;
use crate::{App, SetupStatus, devtools_helper};

/// How long the person must have been at the Mac, counted from their first input after the
/// step's trigger, for macOS's Local Network alert to count as seen and answered
/// ([`aterm_types::macos_setup::LOCAL_NETWORK_QUESTION_HOLD`]) when neither the LAN re-probe
/// heard an answer nor the alert watch saw the alert go ([`App::on_local_network_alert_answered`]):
/// macOS reports neither the alert nor its answer, and the alert stands, unanswered, while nobody
/// is there ([`App::look_at_local_network_alert`]).
pub(crate) const LOCAL_NETWORK_ALERT_HOLD: Duration =
    aterm_types::macos_setup::LOCAL_NETWORK_QUESTION_HOLD;

/// How long the Local Network alert watch's worker waits, once its trigger armed it, between the
/// end of one read of the window list and the next while its last read found the person at the
/// Mac, and whatever the person for [`LOCAL_NETWORK_ALERT_FIRST_LOOKS`] after the arm
/// ([`watch_alert`]). A read takes its own time on top (WindowServer's: a median of about
/// 1 ms on a 27.0.1 Mac, `docs/measured/unc-alert-window-list-2026-11-03.md`; reads on a
/// 0.25 s cadence came up to 1.007 s apart in a VM,
/// `docs/measured/fda-quit-reopen-sheet-window-list-2026-10-28.md`).
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOCAL_NETWORK_ALERT_LOOK: Duration = Duration::from_millis(250);

/// [`LOCAL_NETWORK_ALERT_LOOK`] once the watch's first looks are over
/// ([`LOCAL_NETWORK_ALERT_FIRST_LOOKS`]) while its last read found the person away (no touch of
/// the keyboard or pointer within the last minute, or the screen locked or switched away for
/// another login): such a read never ends the hold ([`watch_alert`]), which may stand for hours
/// while nobody is there, so the watch reads only this often then. An alert already seen that
/// the person answers then ends the hold at most this long after its window leaves the list,
/// while the press that answered it still counts as their touch.
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOCAL_NETWORK_ALERT_AWAY_LOOK: Duration = Duration::from_secs(5);

/// How long after its trigger armed it the watch's worker reads every
/// [`LOCAL_NETWORK_ALERT_LOOK`] whatever the person, before it paces its reads by the person
/// ([`LOCAL_NETWORK_ALERT_AWAY_LOOK`] while away). The alert must be seen on screen before its
/// going counts ([`watch_alert`]), and the step's moment ([`App::step_moment_is_quiet`]) admits
/// a person whose last touch was up to 5 minutes before it ([`devtools_helper::PRESENT_WITHIN`]),
/// whom the watch's own minute reads as away: at the away pace, an alert drawn after the trigger
/// and answered within one away look would never be seen. This is meant to cover the alert's
/// coming up: a CFUserNotification's window was listed within 0.12 s of its request
/// (`docs/measured/unc-alert-window-list-2026-11-03.md`), while the Local Network alert's own
/// delay after its trigger was not measured. Past it, an alert drawn while the last read found
/// the person away and answered within one away look is not seen, and the hold ends by the
/// person's time ([`LOCAL_NETWORK_ALERT_HOLD`]). It costs about 120 reads of about 1 ms each.
#[cfg(any(target_os = "macos", test))]
pub(crate) const LOCAL_NETWORK_ALERT_FIRST_LOOKS: Duration = Duration::from_secs(30);

/// How long the Local Network trigger waits for its alert watch's first read, the baseline
/// ([`StepsReach::local_network_alert_baseline_in`]). Past it the trigger goes without the watch,
/// which is stopped, and its hold ends only by the re-probe's answer or the person's time: the
/// wait is one window-list read, whose time WindowServer's load decides (see
/// [`LOCAL_NETWORK_ALERT_LOOK`]), and the trigger goes at most this long after its moment.
pub(crate) const LOCAL_NETWORK_ALERT_BASELINE_WAIT: Duration = Duration::from_secs(1);

/// How long the live alert watch's worker waits, after its first read, for its trigger to arm
/// it or stop it: a hang detector, since the trigger does one or the other within
/// [`LOCAL_NETWORK_ALERT_BASELINE_WAIT`] and a turn of the window.
#[cfg(any(target_os = "macos", test))]
const LOCAL_NETWORK_ALERT_ARM_WAIT: Duration = Duration::from_secs(60);

/// The process that owns macOS's Local Network alert window: UserNotificationCenter on Tahoe
/// 26.6.2 and on Golden Gate 27.0 (`docs/measured/fda-quit-reopen-sheet-window-list-2026-10-28.md`,
/// its windows table: layer 8, 260 points wide, read without a Screen Recording grant), and the
/// 27.0 rows' dialog census (`docs/measured/commons-ln-first-use-2026-10-28.md`) counts it as
/// UserNotificationCenter's, whose Allow the person pressed. UserNotificationCenter draws every
/// CFUserNotification, tccd's consent prompts among them, so a window of it is not told apart
/// from the alert: the watch ends a hold only at a read that lists no window of it at all, on
/// screen or off ([`watch_alert`]). The watch starts only on a macOS whose alert host was
/// measured ([`alert_host_measured`]); on any other, the hold ends only by the re-probe's answer
/// or the person's time.
#[cfg(any(target_os = "macos", test))]
const LOCAL_NETWORK_ALERT_OWNERS: [&str; 1] = ["UserNotificationCenter"];

/// Whether macOS `product_version` (`SystemVersion.plist`'s `ProductVersion`) is one whose Local
/// Network alert was measured as a window of [`LOCAL_NETWORK_ALERT_OWNERS`]: Tahoe 26 and Golden
/// Gate 27. An unknown version is not.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn alert_host_measured(product_version: Option<&str>) -> bool {
    product_version
        .and_then(|version| version.split('.').next())
        .is_some_and(|major| matches!(major, "26" | "27"))
}

/// Within how long a touch of the keyboard or pointer reads the person present while the Local
/// Network alert's hold counts their time ([`App::look_at_local_network_alert`]): the permission
/// walk's own presence ([`crate::permission_walk::START_PRESENT_WITHIN`]). Time between two looks
/// counts only with the person present at both, and never more than this of it.
const ALERT_PRESENT_WITHIN: Duration = crate::permission_walk::START_PRESENT_WITHIN;

/// How long a Local Network trigger that named a LAN but could not make its socket waits before
/// it looks again. One that found no LAN to name looks again only once this Mac's network moves
/// ([`StepsReach::watch_network`]): the Wi-Fi may join after launch, and nothing else changes
/// what it finds.
const LOCAL_NETWORK_RETRY: Duration = Duration::from_secs(60);

/// The Local Network row's words while this Mac is on no network at all ([`LanHere::Offline`]):
/// the row reads calm, not waiting ([`App::access_opt_outs`]), and the step asks once a network
/// move brings one. Every network has something to ask with ([`lan_ask`]).
pub(crate) const LOCAL_NETWORK_NOTHING_TO_ASK: &str = "Nothing to ask yet: this Mac is on no \
    network. aterm asks macOS's Local Network question once it joins one.";

/// What this Mac's interfaces give the Local Network step to ask with ([`StepsReach::lan_here`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LanHere {
    /// A network: a subnet to name ([`lan_target`]) or a Bonjour browse to make ([`lan_ask`]).
    Lan,
    /// No network at all.
    Offline,
}

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

/// The Local Network row's description while `[swarm] enabled = false` holds System State's Local
/// Network step ([`App::local_network_pending`]): setup asks nothing of the local network then, so
/// the row settles for the Permissions stage, said calmly. It claims no answer to macOS's
/// question, which a session's own first use of the local network (an `ssh` to a Mac here) still
/// raises, as it does for any app.
pub(crate) const SHARING_OFF_LAN: &str = "Setup asks for no Local Network while sharing with your \
     other Macs is off in aterm.toml ([swarm]).";

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

/// Whether the LAN re-probe heard macOS's Local Network answer (pure): a LAN server answered, or
/// macOS refused a `.local` lookup; one that ran out heard none.
const fn probe_heard_an_answer(probe: LocalNetworkProbe) -> bool {
    match probe {
        LocalNetworkProbe::Granted => true,
        #[cfg(any(target_os = "macos", test))]
        LocalNetworkProbe::Refused => true,
        #[cfg(any(target_os = "macos", test))]
        LocalNetworkProbe::Unanswered => false,
    }
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
    /// `triggered`, which the access record keeps as asked), the alert watch saw no window of the
    /// alert's owner left with the person at the Mac ([`App::on_local_network_alert_answered`]),
    /// or the re-probe after it ended, whatever it heard. Each LAN reader looks again now, the
    /// converge pass's `sync.peers` among them ([`crate::network_moved::local_network_moved`]).
    pub(crate) lan_answer_moved: fn(),
    /// Start watching for macOS's Local Network alert that the trigger to come for `revision`
    /// raises, in place of any watch before it: the watch takes its first read, the baseline,
    /// and the trigger waits for it ([`Self::local_network_alert_baseline_in`]). `false` when no
    /// watch starts (INERT; live, a macOS whose alert host was not measured,
    /// [`alert_host_measured`], or no worker), so the trigger goes at once and its hold ends only
    /// by the re-probe's answer or the person's time. Live ([`start_alert_watch`]): a lane worker
    /// in the window process runs [`watch_alert`] on the window list, never on the main thread,
    /// and wakes the window ([`crate::Wake::LocalNetworkAlertWatch`]) once its first read is in
    /// and once it saw the alert answered.
    pub(crate) start_local_network_alert_watch: fn(u64) -> bool,
    /// Whether the alert watch's first read has come back (read, or found unreadable), so the
    /// trigger waiting for it may go.
    pub(crate) local_network_alert_baseline_in: fn() -> bool,
    /// The trigger went: the alert watch reads the window list from now on, until it is stopped
    /// ([`Self::cancel_local_network_alert_watch`]), as the hold it may end has no end of its own
    /// on the clock.
    pub(crate) arm_local_network_alert_watch: fn(),
    /// Drain the revision whose alert the watch saw answered, if any.
    pub(crate) take_local_network_alert_answer: fn() -> Option<u64>,
    /// Stop the alert watch and drop an answer not yet drained: its hold ended, or its trigger
    /// went without it or named no LAN.
    pub(crate) cancel_local_network_alert_watch: fn(),
    /// Open a named access pane; injected so headless fixtures reach no OS API.
    pub(crate) open_pane: fn(&App, Access) -> bool,
    /// Append one step's outcome, reporting any failure to the retry owner.
    pub(crate) record: fn(u64, Access, AccessOutcome) -> std::io::Result<()>,
    /// The latest outcome recorded for a row in a revision.
    pub(crate) recorded: fn(u64, Access) -> Option<AccessOutcome>,
    /// This loaded signing identity's latest Local Network step across revisions.
    /// A prior ask permits a silent recheck without opening System State or asking again.
    pub(crate) local_network_recorded_before: fn() -> Option<AccessOutcome>,
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
    /// Whether a first LAN use asked macOS past the wait for the step
    /// ([`aterm_types::macos_setup::lan_first_use::asked`]): one of this process's through the
    /// door or the Commons gate's own, or the vault subscriber's. The step reads that ask as its
    /// question, and the door's hold, where one holds it, as its alert's.
    pub(crate) door_asked: fn() -> bool,
    /// The LAN re-probe heard macOS's answer: end the door's hold of the question a first LAN use
    /// asked (through the door, or the Commons gate's own ask past the wait), if one stands
    /// ([`aterm_types::macos_setup::lan_first_use::answer_heard`]). Whether one stood.
    pub(crate) end_door_hold: fn() -> bool,
    /// Whether the window holds a [`MacosDialog`] while its own gesture is up
    /// (a live windowed instance only: the count is process-wide).
    pub(crate) holds_dialog: bool,
    /// Whether aterm.log names what holds the Local Network step
    /// ([`App::note_local_network_hold`]): only where the step can run, a live windowed macOS
    /// instance, never on an instance whose arms reach nothing.
    pub(crate) names_holds: bool,
    /// What this Mac's interfaces give the Local Network step to name now (`getifaddrs`: no
    /// socket, so no question), read once its trigger named nothing.
    pub(crate) lan_here: fn() -> LanHere,
    /// Subscribe, request one fresh App reading, then request another after every non-timeout
    /// network wake on a process-lifetime thread. Its latest source may hide a route move;
    /// the App rechecks local state at the step's moment, including an Offline change.
    pub(crate) watch_network: fn(devtools_helper::Wake),
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
        start_local_network_alert_watch: |_| false,
        local_network_alert_baseline_in: || false,
        arm_local_network_alert_watch: || {},
        take_local_network_alert_answer: || None,
        cancel_local_network_alert_watch: || {},
        open_pane: |_, _| false,
        record: |_, _, _| Ok(()),
        recorded: |_, _| None,
        local_network_recorded_before: || None,
        handed: |_, _| false,
        frontmost: |_| false,
        quiet: |_, _| false,
        dialog_up: || false,
        hand_lan_over: |_| {},
        door_asked: || false,
        end_door_hold: || false,
        holds_dialog: false,
        names_holds: false,
        lan_here: || LanHere::Offline,
        watch_network: |_| {},
    };

    /// The live arms of a windowed macOS instance.
    pub(crate) fn live() -> Self {
        Self {
            trigger_local_network: live_trigger_local_network,
            start_local_network_probe: live_start_local_network_probe,
            take_local_network_probe: live_take_local_network_probe,
            lan_answer_moved: crate::network_moved::local_network_moved,
            start_local_network_alert_watch: live_start_local_network_alert_watch,
            local_network_alert_baseline_in: live_local_network_alert_baseline_in,
            arm_local_network_alert_watch: live_arm_local_network_alert_watch,
            take_local_network_alert_answer: live_take_local_network_alert_answer,
            cancel_local_network_alert_watch: live_cancel_local_network_alert_watch,
            open_pane: live_open_access_pane,
            record: live_record,
            recorded: live_recorded,
            local_network_recorded_before: live_local_network_recorded_before,
            handed: live_handed,
            frontmost: App::system_state_frontmost,
            quiet: App::step_moment_is_quiet,
            dialog_up: aterm_types::macos_setup::macos_dialog_up,
            hand_lan_over: aterm_types::macos_setup::lan_first_use::hand_over,
            door_asked: aterm_types::macos_setup::lan_first_use::asked,
            end_door_hold: aterm_types::macos_setup::lan_first_use::answer_heard,
            holds_dialog: true,
            names_holds: true,
            lan_here: live_lan_here,
            watch_network: live_watch_network,
        }
    }
}

/// The steps' own state in the App.
pub(crate) struct AccessSteps {
    reach: StepsReach,
    /// The revision whose Local Network step this process finished (triggered, its trigger
    /// waiting for the alert watch's first read, found recorded, or found no LAN to name).
    local_network_done: Option<u64>,
    /// The revision whose record this process read for Local Network.
    local_network_read: Option<u64>,
    /// The revision whose Local Network trigger waits for its alert watch's first read, and the
    /// last moment it waits ([`LOCAL_NETWORK_ALERT_BASELINE_WAIT`]). The alert counts as up
    /// meanwhile for the window's gestures, so the dialog slot stays held and every later step
    /// waits ([`AccessSteps::local_network_alert_up`]).
    local_network_baseline: Option<(u64, Instant)>,
    /// macOS's Local Network alert the step raised, while it counts as up
    /// ([`App::look_at_local_network_alert`]).
    local_network_alert: Option<LocalNetworkAlert>,
    /// The revision whose trigger went with the alert watch armed, which may still end its hold
    /// ([`App::on_local_network_alert_answered`]); `None` while no such hold stands.
    local_network_watch: Option<u64>,
    /// The revision whose Local Network trigger found nothing to connect to, and when to try it
    /// again: a minute after one that named a LAN but had no socket, `None` once this Mac's
    /// network moves ([`Self::network_moved`]) after one that found no LAN to name.
    local_network_retry: Option<(u64, Option<Instant>)>,
    /// A silent check heard no answer. Retry independently of window or tab focus;
    /// this never repeats the OS permission trigger.
    local_network_recheck: Option<(u64, Instant)>,
    /// This Mac's network moved since the step last looked for a LAN to name, as the watch
    /// started at its first such look tells it ([`StepsReach::watch_network`]).
    network_moved: Arc<AtomicBool>,
    /// That watch was started (once per process).
    watching_network: bool,
    /// The step's last look found this Mac on no network at all ([`LanHere::Offline`]): the
    /// Local Network row reads calm ([`LOCAL_NETWORK_NOTHING_TO_ASK`]).
    nothing_to_ask: bool,
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

/// macOS's Local Network alert the step raised, and the person's time with it
/// ([`App::look_at_local_network_alert`]).
#[derive(Clone, Copy, Debug)]
struct LocalNetworkAlert {
    /// When the step's trigger asked.
    asked: Instant,
    /// The person's time at the Mac counted since a look first found them present with an input
    /// after the ask; `None` until a look does.
    present_for: Option<Duration>,
    /// The last look that read the person.
    looked: Instant,
    /// Whether that look read them present.
    present: bool,
}

impl LocalNetworkAlert {
    /// The alert the trigger raised at `now`.
    const fn asked(now: Instant) -> Self {
        Self {
            asked: now,
            present_for: None,
            looked: now,
            present: false,
        }
    }
}

struct RecordedAccess {
    revision: u64,
    access: Access,
    outcome: AccessOutcome,
    retry_at: Option<Instant>,
}

/// One window of the Local Network alert's owner as one read of the window list lists it
/// ([`watch_alert`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AlertWindow {
    /// Its window number, which WindowServer gives each window anew.
    pub(crate) number: u32,
    /// Whether it is on screen. A window ordered out stays in the list off screen: a
    /// CFUserNotification taken down by `CFUserNotificationCancel` was listed off screen for
    /// about 8 s before it left the list (`docs/measured/unc-alert-window-list-2026-11-03.md`).
    pub(crate) on_screen: bool,
}

/// How a watch of macOS's Local Network alert ended ([`watch_alert`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlertWatchEnd {
    /// A window that the first read did not list on screen was seen on screen, and a later read,
    /// with the person at the Mac, listed no window of the alert's owner at all, on screen or
    /// off: whatever the person answered, no alert of that owner is up, the one the trigger
    /// raised included.
    Answered,
    /// Such a window was seen, and the last read that could read the list listed a window of the
    /// alert's owner (that one, or one the first read listed, which may be the alert itself), on
    /// screen or off, when the watch stopped.
    StillUp,
    /// Such a window was seen, and the last read that could read the list listed no window of
    /// the alert's owner, but found the person away, which ends nothing.
    GoneWhileAway,
    /// No such window was seen before the watch stopped: macOS raised no alert, drew it only
    /// after the watch's last read, drew and took it down between two reads, or had it up before
    /// the first read (an earlier LAN use raised it, and macOS does not draw it twice), which
    /// then listed it.
    NeverSeen,
    /// The first read could not read the window list, so no window can be told to be the alert.
    Blind,
}

/// Watch for macOS's Local Network alert (pure, run on the alert watch's worker): `read` is one
/// read of the window list's windows of the alert's owner, on screen or off (`None` when the
/// list could not be read); `present` says whether the person is at the Mac now, read after
/// each read of the list; and `look_again` waits for the next look, given whether the last
/// such reading found the person present, and says whether there is one (`false` once the
/// watch was stopped). The first read, which the trigger waits for, is the baseline: no window
/// it lists on screen is one the trigger raised. A window a later read lists on screen that the
/// baseline does not is seen, and the watch ends [`AlertWatchEnd::Answered`] at the first read
/// that lists no window of the owner at all, on screen or off, with the person present. The
/// owner draws other alerts too, a baseline window still up may be the alert itself, and a
/// window ordered out (off screen) is still up, so a read that lists any window of it ends
/// nothing. A window's going is not known to mean an answer while nobody is at the Mac (an
/// alert stands, unanswered, while nobody is there), so a read that finds the person away ends
/// nothing either, and a later read with them present decides. A read that failed changes
/// nothing, and a baseline window going away, with nothing seen, ends nothing.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn watch_alert(
    mut read: impl FnMut() -> Option<Vec<AlertWindow>>,
    mut present: impl FnMut() -> bool,
    mut look_again: impl FnMut(bool) -> bool,
) -> AlertWatchEnd {
    let Some(baseline) = read() else {
        return AlertWatchEnd::Blind;
    };
    let baseline: Vec<u32> = baseline
        .iter()
        .filter(|window| window.on_screen)
        .map(|window| window.number)
        .collect();
    let mut here = present();
    let mut seen: Vec<u32> = Vec::new();
    let mut listed_none = false;
    while look_again(here) {
        let listed = read();
        here = present();
        let Some(listed) = listed else {
            continue;
        };
        for window in &listed {
            if window.on_screen
                && !baseline.contains(&window.number)
                && !seen.contains(&window.number)
            {
                seen.push(window.number);
            }
        }
        listed_none = listed.is_empty();
        if !seen.is_empty() && listed_none && here {
            return AlertWatchEnd::Answered;
        }
    }
    match (seen.is_empty(), listed_none) {
        (true, _) => AlertWatchEnd::NeverSeen,
        (false, true) => AlertWatchEnd::GoneWhileAway,
        (false, false) => AlertWatchEnd::StillUp,
    }
}

/// Whose question an alert watch follows: its log record names it, and an answer it saw is left
/// as it ([`AlertWatchSlot::take_answer`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Watched {
    /// The alert System State's Local Network step's own trigger raises for this approved
    /// revision ([`StepsReach::start_local_network_alert_watch`]).
    Step(u64),
    /// The question a first LAN use raises through `lan_first_use`'s door, or the Commons gate's
    /// first use raises by itself past the wait for the step, held by the door's hold under this
    /// door pass ([`start_door_alert_watch`]).
    Door(u64),
}

#[cfg(any(target_os = "macos", test))]
impl Watched {
    /// The revision of a step's watch; `None` for the door's.
    pub(crate) const fn revision(self) -> Option<u64> {
        match self {
            Self::Step(revision) => Some(revision),
            Self::Door(_) => None,
        }
    }

    /// Who the watch's aterm.log lines are of.
    const fn logger(self) -> &'static str {
        match self {
            Self::Step(_) => "system state",
            Self::Door(_) => "lan first use",
        }
    }
}

#[cfg(any(target_os = "macos", test))]
impl std::fmt::Display for Watched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Step(revision) => write!(f, "revision {revision}"),
            Self::Door(pass) => write!(f, "door pass {pass}"),
        }
    }
}

/// What an alert watch's first read, the baseline, found: how long after the watch's start it
/// came back, and how many windows of the alert's owner it listed, on screen or off (`None`:
/// the window list could not be read).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BaselineRead {
    pub(crate) after: Duration,
    pub(crate) windows: Option<usize>,
}

/// What an alert watch's worker did after its first read, besides what [`watch_alert`] saw.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlertWatchLooks {
    /// Its trigger armed it, and it read the list again at least once.
    Read,
    /// It was stopped or replaced before it read the list again: its trigger went without it
    /// (its first read came back too late) or named no LAN, or a stop or a later start came
    /// first.
    Stopped,
    /// Its trigger neither armed nor stopped it within the arm's wait, a hang detector.
    ArmWaitRanOut,
}

/// How an alert watch ended, as its one log record names it ([`AlertWatchReport`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlertWatchEnded {
    /// [`AlertWatchEnd::Answered`] while the watch was still the window's: it left the answer.
    Answered,
    /// [`AlertWatchEnd::Answered`] once a stop or a later start had overtaken it: it left none.
    AnsweredOnceStopped,
    /// [`AlertWatchEnd::StillUp`].
    StillUp,
    /// [`AlertWatchEnd::GoneWhileAway`].
    GoneWhileAway,
    /// [`AlertWatchEnd::NeverSeen`], after it read the list again ([`AlertWatchLooks::Read`]).
    NeverSeen,
    /// [`AlertWatchEnd::NeverSeen`] with no read after its first ([`AlertWatchLooks::Stopped`]).
    StoppedUnarmed,
    /// [`AlertWatchEnd::NeverSeen`] with no read after its first
    /// ([`AlertWatchLooks::ArmWaitRanOut`]).
    ArmWaitRanOut,
    /// [`AlertWatchEnd::Blind`].
    Blind,
}

/// The one log record an alert watch's end makes ([`finish_alert_watch`]); its `Display` is
/// the aterm.log line.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AlertWatchReport {
    pub(crate) watched: Watched,
    pub(crate) ended: AlertWatchEnded,
    pub(crate) baseline: Option<BaselineRead>,
}

#[cfg(any(target_os = "macos", test))]
impl std::fmt::Display for AlertWatchReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let owner = LOCAL_NETWORK_ALERT_OWNERS[0];
        let (watch, asked, unarmed) = match self.watched {
            Watched::Step(_) => (
                "system state: the Local Network alert watch",
                "its trigger",
                "its trigger went without it or named no LAN, or a stop or a later start came \
                 first",
            ),
            Watched::Door(_) => (
                "lan first use: the door's Local Network alert watch",
                "its use asked",
                "its use asked without it, its hold was over first, or a later start came \
                 first",
            ),
        };
        match self.ended {
            AlertWatchEnded::Answered => write!(
                f,
                "{watch} saw a new window of {owner} come and then, with the person at the Mac, \
                 no window of {owner} left"
            ),
            AlertWatchEnded::AnsweredOnceStopped => write!(
                f,
                "{watch} saw a new window of {owner} come and then, with the person at the Mac, \
                 no window of {owner} left, after it was stopped"
            ),
            AlertWatchEnded::StillUp => write!(
                f,
                "{watch} stopped with a window of {owner} still listed, after a new one came"
            ),
            AlertWatchEnded::GoneWhileAway => write!(
                f,
                "{watch} stopped after a new window of {owner} came and its last read listed \
                 none, with the person away, which it does not take for an answer"
            ),
            AlertWatchEnded::NeverSeen => write!(
                f,
                "{watch} stopped with no new window of {owner} on screen after its first read: \
                 macOS raised no alert, had it up before that read, or drew and took it down \
                 between two reads"
            ),
            AlertWatchEnded::StoppedUnarmed => write!(
                f,
                "{watch} was stopped before it read the list again after its first read: \
                 {unarmed}"
            ),
            AlertWatchEnded::ArmWaitRanOut => write!(
                f,
                "{watch} was neither armed nor stopped within {} s of its first read, and \
                 stopped reading",
                LOCAL_NETWORK_ALERT_ARM_WAIT.as_secs()
            ),
            AlertWatchEnded::Blind => write!(
                f,
                "{watch} could not read the window list before {asked}: its hold ends only by \
                 the re-probe's answer or the person's time"
            ),
        }?;
        write!(f, " ({}; ", self.watched)?;
        match self.baseline {
            Some(BaselineRead {
                after,
                windows: Some(windows),
            }) => write!(
                f,
                "its first read listed {windows} window(s) of {owner}, {} ms after its start)",
                after.as_millis()
            ),
            Some(BaselineRead {
                after,
                windows: None,
            }) => write!(
                f,
                "its first read found the window list unreadable, {} ms after its start)",
                after.as_millis()
            ),
            None => f.write_str("it made no first read)"),
        }
    }
}

/// The end of an alert watch's worker once [`watch_alert`] returned `end` after `looks`.
/// `settle` is given whether the watch answered; it leaves `watched` for the window (or the
/// door's hold) when it did and the watch is still its slot's, and says whether the watch is
/// still its slot's. Only such an answer wakes the window (`wake`). Every end then makes one log
/// record (`log`), a watch a stop overtook included (its hold's end, the re-probe's answer, a
/// later start), with what its first read found, so a run's log says how each watch ended and
/// when its baseline came back.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn finish_alert_watch(
    end: AlertWatchEnd,
    looks: AlertWatchLooks,
    watched: Watched,
    baseline: Option<BaselineRead>,
    settle: impl FnOnce(bool) -> bool,
    wake: impl FnOnce(),
    log: impl FnOnce(AlertWatchReport),
) {
    let answered = end == AlertWatchEnd::Answered;
    let live = settle(answered);
    if answered && live {
        wake();
    }
    let ended = match end {
        AlertWatchEnd::Answered if live => AlertWatchEnded::Answered,
        AlertWatchEnd::Answered => AlertWatchEnded::AnsweredOnceStopped,
        AlertWatchEnd::StillUp => AlertWatchEnded::StillUp,
        AlertWatchEnd::GoneWhileAway => AlertWatchEnded::GoneWhileAway,
        AlertWatchEnd::NeverSeen => match looks {
            AlertWatchLooks::Read => AlertWatchEnded::NeverSeen,
            AlertWatchLooks::Stopped => AlertWatchEnded::StoppedUnarmed,
            AlertWatchLooks::ArmWaitRanOut => AlertWatchEnded::ArmWaitRanOut,
        },
        AlertWatchEnd::Blind => AlertWatchEnded::Blind,
    };
    log(AlertWatchReport {
        watched,
        ended,
        baseline,
    });
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
            local_network_baseline: None,
            local_network_alert: None,
            local_network_watch: None,
            local_network_retry: None,
            local_network_recheck: None,
            network_moved: Arc::new(AtomicBool::new(false)),
            watching_network: false,
            nothing_to_ask: false,
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

    /// Raise the Local Network alert's hold as the step's trigger at `asked` does, the person
    /// touching the Mac at once (tests): it ends once they have had [`LOCAL_NETWORK_ALERT_HOLD`]
    /// with it, or the re-probe hears an answer.
    #[cfg(test)]
    pub(crate) const fn alert_seen_at_for_test(&mut self, asked: Instant) {
        self.local_network_alert = Some(LocalNetworkAlert {
            asked,
            present_for: Some(Duration::ZERO),
            looked: asked,
            present: true,
        });
    }

    /// Whether the menu line names a regressed Full Disk Access grant (tests).
    #[cfg(test)]
    pub(crate) const fn fda_regressed(&self) -> bool {
        self.fda_regressed
    }

    /// Whether macOS's Local Network alert counts as up for the window's gestures: the alert the
    /// step raised, until the LAN re-probe hears its answer, the alert watch sees it go
    /// ([`App::on_local_network_alert_answered`]) or the person has had
    /// [`LOCAL_NETWORK_ALERT_HOLD`] with it ([`App::look_at_local_network_alert`]); or the step's
    /// trigger waiting for its alert watch's first read, which goes within
    /// [`LOCAL_NETWORK_ALERT_BASELINE_WAIT`]. Only a raised alert names Local Network as the
    /// page's headline step (`App::system_state_access_step`).
    const fn local_network_alert_up(&self) -> bool {
        self.local_network_baseline.is_some() || self.local_network_alert.is_some()
    }

    /// End the Local Network alert's hold, and stop its alert watch: an answer the watch hears
    /// from here on has no hold to end. A trigger still waiting for the watch's first read never
    /// goes, so its step is not done: it runs again at its next moment. Every end of the hold
    /// goes through here: the re-probe's answer and `[swarm] enabled = false`
    /// ([`App::on_local_network_probe_ready`]), the person's time
    /// ([`App::look_at_local_network_alert`]), the watch's answer
    /// ([`App::on_local_network_alert_answered`]) and a wait dropped without its trigger
    /// (`App::local_network_trigger_waits`). A wait that ends in its trigger does not come here:
    /// `App::local_network_trigger_waits` clears it, and stops the watch itself when the first
    /// read did not come back in time.
    fn end_local_network_hold(&mut self) {
        self.local_network_alert = None;
        let waiting = self.local_network_baseline.take();
        if let Some((revision, _)) = waiting
            && self.local_network_done == Some(revision)
        {
            self.local_network_done = None;
        }
        if self.local_network_watch.take().is_some() || waiting.is_some() {
            (self.reach.cancel_local_network_alert_watch)();
        }
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
/// then link-local (169.254/16), then any other directly attached subnet (a
/// campus network may hand out public addresses, and TN3179 counts any host on
/// an attached subnet as local network); the subnet's first host, or its second
/// when the first is this Mac. `None` when no such subnet has a mask from /8 to
/// /30 (a /31, a /32, an empty mask, a mask with holes, or none at all).
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
    pick(|address| address.is_private())
        .or_else(|| pick(|address| address.is_link_local()))
        .or_else(|| pick(|_| true))
}

/// How the Local Network step makes macOS ask its question on this Mac's network
/// ([`lan_ask`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LanAsk {
    /// A UDP connect (nothing sent) to a host on a directly attached subnet ([`lan_target`]).
    Connect(Ipv4Addr),
    /// A short Bonjour browse of the step's own ([`LOCAL_NETWORK_BROWSE_FOR`]): a network with
    /// no IPv4 subnet to name (IPv6 alone, a /31 or /32 link), where a browse is still a Local
    /// Network use macOS asks about.
    Browse,
}

/// How the Local Network step asks macOS on this Mac's network now (pure): a connect to the
/// subnet [`lan_target`] names, else a Bonjour browse on any network (a LAN segment among
/// `interfaces`, or `ipv6_network`), else `None`: a Mac on no network at all has nothing to ask
/// with yet.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn lan_ask(interfaces: &[LanInterface], ipv6_network: bool) -> Option<LanAsk> {
    lan_target(interfaces).map(LanAsk::Connect).or_else(|| {
        (ipv6_network || interfaces.iter().any(|interface| interface.broadcast))
            .then_some(LanAsk::Browse)
    })
}

/// `status`'s `setup=` (pure): where this Mac's setup stands, from the last status the worker
/// published (its progress record and its prompt-free readings), so a `status` costs no probe.
/// `pending:<row,...>` names each managed row that needs a person, waits, failed, is unread or
/// still differs without a recorded personal change (`fda`, `devtools`, `vault`, … [`setup_word`]), then `;needs:<row,...>`
/// for those of them that wait on a person's press (their reading is Needs you), so a reader
/// tells a press from a wait without the page; `done` once System State's current headline
/// says this Mac is set up ([`crate::system_state::Headline::set_up`]), including before its run finished,
/// nothing left for anyone to do now; then `;asking:<row,...>` names each stage the run's last
/// pass parked to ask again by itself (an ALab vault that did not answer or is absent off the
/// ALab LAN, `vault`; a step an engine put off), so `done` is never read as a finished record;
/// `pending` while a run goes with none read; `-` before the worker said anything (no setup in
/// this process). Grants still on offer remain named as pending personal steps
/// ([`crate::system_state::offered_access`]); their stage record alone claims no grant.
pub(crate) fn setup_field(status: &SetupStatus) -> String {
    let selection = status
        .plan
        .as_ref()
        .map_or_else(Selection::none, |plan| plan.selection);
    let rows = crate::system_state::rows(status, selection);
    let mut pending: Vec<&str> = Vec::new();
    let mut needs: Vec<&str> = Vec::new();
    let offered_access: Vec<_> = crate::system_state::offered_access(status)
        .into_iter()
        .filter(|access| crate::system_state::manages(selection, Subject::Access(*access)))
        .collect();
    for access in &offered_access {
        let word = setup_word(Subject::Access(*access));
        if !pending.contains(&word) {
            pending.push(word);
            needs.push(word);
        }
    }
    for row in rows.iter().filter(|row| row.managed) {
        let left = match row.standing {
            None
            | Some(Standing::Unknown | Standing::NeedsYou | Standing::Waiting | Standing::Failed) => {
                true
            }
            // The probe's personal-change record is evidence of a choice; a finished stage
            // alone says nothing about why a current reading differs.
            Some(Standing::Differs) => !row.changed_by_you,
            _ => false,
        };
        let word = setup_word(row.subject);
        if left {
            if !pending.contains(&word) {
                pending.push(word);
            }
            if row.standing == Some(Standing::NeedsYou) && !needs.contains(&word) {
                needs.push(word);
            }
        }
    }
    // What the page says: a run going, a step that waits or failed, or a row not read yet keeps
    // its headline off set up.
    let set_up = || {
        status.plan.as_ref().is_some_and(|plan| {
            let headline = crate::system_state::headline(status, &rows, Some(plan));
            headline.set_up
                // A completed explicit opt-out has no managed work to prove. Keep the
                // readable idle terminal case without reviving finished-only success.
                || (plan.selection.is_empty()
                    && (status.finished || status.cancelled)
                    && !status.busy
                    && !status.changing
                    && !status.queued
                    && status.running.is_empty()
                    && headline.left_as_is)
        })
    };
    let offered = || {
        if offered_access.is_empty() {
            String::new()
        } else {
            let words: Vec<&str> = offered_access
                .iter()
                .map(|access| setup_word(Subject::Access(*access)))
                .collect();
            format!(";offered:{}", words.join(","))
        }
    };
    if !pending.is_empty() {
        let needs = if needs.is_empty() {
            String::new()
        } else {
            format!(";needs:{}", needs.join(","))
        };
        format!("pending:{}{needs}", pending.join(","))
    } else if set_up() {
        // The stages the run's last pass parked to ask again by itself, in the rows' words: a
        // vault nobody answered (`unreached`) or none here (`waiting`), a step put off.
        let mut asking: Vec<&str> = Vec::new();
        for stage in status
            .rows
            .iter()
            .filter(|row| matches!(row.status.as_str(), "waiting" | "unreached" | "deferred"))
            .filter_map(|row| aterm_types::macos_setup::StageRow::named(&row.name))
        {
            for setting in stage.settings.iter().filter(|s| selection.contains(**s)) {
                let word = setup_word(Subject::Setting(*setting));
                if !asking.contains(&word) {
                    asking.push(word);
                }
            }
        }
        let asking = if asking.is_empty() {
            String::new()
        } else {
            format!(";asking:{}", asking.join(","))
        };
        format!("done{asking}{}", offered())
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
/// `None` when nothing does.
pub(crate) fn menu_line(status: &SetupStatus, fda_regressed: bool) -> Option<SystemStateLine> {
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
    // Apple's Command Line Tools installing, or a press of Install… waiting for a dialog, is a
    // wait that ends by itself (§3.3): no line.
    // The vault's join asking quietly is a wait that usually ends by itself too
    // ([`SetupStatus::joins_quietly`]).
    let secrets = Subject::Setting(Setting::Secrets);
    let mut needs: Vec<Subject> = managed_with(Standing::NeedsYou)
        .filter(|subject| *subject != repositories || !tools.is_some_and(ToolsWait::calm))
        .filter(|subject| *subject != secrets || !status.joins_quietly())
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
    // Their tab asking for the password or restored, or Apple's installer window, says where the
    // person is needed, as the page does.
    let note = note_of(Setting::Secrets).or_else(|| {
        note_of(Setting::Repositories).map(|note| match tools {
            Some(ToolsWait::Password) => crate::system_state::TOOLS_PASSWORD_HEADLINE.to_owned(),
            Some(ToolsWait::Recovered) => crate::system_state::TOOLS_RECOVERED_HEADLINE.to_owned(),
            Some(ToolsWait::AppleWindow) => crate::system_state::APPLE_WINDOW_NOTE.to_owned(),
            _ => note,
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
    /// while a `[privacy]` switch is off ([`OptOut::Always`]), the debugger wherever its off
    /// reading is their choice ([`crate::debugger_access::off_is_chosen`], read from the
    /// debugger record, so it holds with or without the window's own admission), and Local
    /// Network while `[swarm] enabled = false` holds its step ([`SHARING_OFF_LAN`]), which then
    /// never runs, so the Permissions stage would wait on it for good, or while this Mac is on no
    /// network at all, so the step has nothing to ask macOS with yet
    /// ([`LOCAL_NETWORK_NOTHING_TO_ASK`]: settled calm, not the person's choice, but no step waits
    /// on it). The record says what an off
    /// reading means, so the debugger's is [`OptOut::WhileOff`]: a debugger that reads on, or
    /// could not be read, keeps its own words.
    pub(crate) fn access_opt_outs(&self) -> [Option<OptOut>; 4] {
        let off = (!self.access_steps_allowed()).then_some(OptOut::Always(PRIVACY_OFF));
        OPT_OUT_ROWS.map(|access| match access {
            Access::Debugger => off.or(self
                .debugger_change
                .off_is_chosen()
                .then_some(OptOut::WhileOff(DEBUGGER_OFF))),
            Access::LocalNetwork => off
                .or((!self.config.swarm_enabled()).then_some(OptOut::Always(SHARING_OFF_LAN)))
                .or(self
                    .access_steps
                    .nothing_to_ask
                    .then_some(OptOut::Always(LOCAL_NETWORK_NOTHING_TO_ASK))),
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
            || self.access_steps.local_network_alert_up()
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
    /// this process is up, while a person's Install… press waits for the slot or its tab is
    /// on its way ([`Self::install_press_waits`]): a person's press never waits behind a
    /// control client's walk, and while System Settings asks something or may still
    /// ([`Self::system_settings_may_ask`], read last since it may read the window list): a page
    /// opened under its own sheet is lost, its panel left beside that question. A guard the
    /// window holds then is the walk's own, taken at a turn's end while it waited
    /// ([`Self::settle_access_steps`]): it is given up and the slot won only when none is up, so a
    /// setup-worker prompt that was up first is waited out, not stacked on.
    pub(crate) fn reserve_walk_dialog(&mut self, now: Instant) -> bool {
        if self.access_gesture_up_beside_walk(now)
            || self.install_press_waits()
            || self.system_settings_may_ask()
        {
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
    /// standing, and macOS's Local Network alert, a later step's page or dialog and the walk that
    /// starts by itself
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
    /// part of it that fails. The window's gestures and dialogs, and the quiet moment's parts
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
        if self.access_steps.local_network_alert.is_some() {
            hold("the Local Network alert's hold");
        }
        if self.access_steps.local_network_baseline.is_some() {
            hold("the Local Network trigger's wait for its alert watch's first read");
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
    /// has up, a pane's hold (a bound, not a reading: macOS does not say when a pane closes) and
    /// the Local Network alert's hold (the person's counted time, ended sooner by the re-probe's
    /// answer or once the alert watch sees no window of the alert's owner left,
    /// [`Self::on_local_network_alert_answered`]).
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

    /// Whether the approved revision's Local Network step is still to run in this process
    /// ([`Self::local_network_pending`]), its record read first
    /// ([`Self::local_network_on_record`]), so a revision the record already holds is done at this
    /// look. The first permission walk to start by itself waits for it, as every step after
    /// it does (`App::look_for_walk_start`): its alert goes first in the run, and its hold
    /// ([`LOCAL_NETWORK_ALERT_HOLD`] of the person's time, ended early by the LAN re-probe's
    /// answer, [`Self::on_local_network_probe_ready`], or once the alert watch sees no window of
    /// the alert's owner left, [`Self::on_local_network_alert_answered`]) is one of the window's
    /// gestures
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

    /// A fresh worker reading may have observed a grant or another process's access record.
    /// Invalidate a pending negative read on that wake, never on every rendered frame.
    pub(crate) fn refresh_pending_local_network_record(&mut self) {
        if self.local_network_pending() {
            self.access_steps.local_network_read = None;
        }
    }

    /// Read Local Network's record once per revision or fresh worker wake. A recorded step,
    /// prior ask of this signing identity, or an observed grant lets its silent check run
    /// without bringing System State forward. Whether that settles this revision's trigger.
    fn local_network_on_record(&mut self, revision: u64) -> bool {
        if self.access_steps.local_network_read == Some(revision) {
            return false;
        }
        self.access_steps.local_network_read = Some(revision);
        let recorded = (self.access_steps.reach.recorded)(revision, Access::LocalNetwork);
        let previously_asked = matches!(
            recorded.or_else(self.access_steps.reach.local_network_recorded_before),
            Some(AccessOutcome::Triggered | AccessOutcome::Granted)
        ) || (recorded.is_none()
            && self
                .system_state_host
                .latest_status()
                .observed
                .iter()
                .any(|row| {
                    row.subject == Subject::Access(Access::LocalNetwork)
                        && row.standing == Standing::InPlace
                }));
        if recorded.is_some() || previously_asked {
            self.access_steps.local_network_done = Some(revision);
            self.access_steps.local_network_retry = None;
            self.access_steps.nothing_to_ask = false;
            if previously_asked {
                // This is a check of an existing ask, not another OS gesture. It must run
                // before the frontmost/quiet gates, and owns no dialog slot. Only the probe's
                // actual answer may record Granted for this revision.
                if recorded.is_none() && self.access_steps_allowed() {
                    self.record_access_at(revision, Access::LocalNetwork, AccessOutcome::Triggered);
                }
                if self.access_steps_allowed() {
                    (self.access_steps.reach.start_local_network_probe)(revision);
                } else {
                    // Keep the check due while an explicit privacy hold suppresses its timer.
                    // Re-enabling it wakes the loop and resumes without requiring tab focus.
                    self.access_steps.local_network_recheck = Some((revision, Instant::now()));
                }
            }
            return true;
        }
        false
    }

    /// The step moment: an access step after Local Network may put its page, panel or folder
    /// dialog on screen now ([`Self::local_network_moment`], with Local Network done, and System
    /// Settings asking nothing, nor a question seen asking lapsed off screen:
    /// [`Self::system_settings_may_ask`], since a step's page opened under it is lost).
    pub(crate) fn access_step_moment(&self, now: Instant) -> bool {
        !self.local_network_pending()
            && self.local_network_moment_but_settings(now)
            && !self.system_settings_may_ask()
    }

    /// Drain a completed 1 s × 30 s LAN grant re-probe: on an answer heard (a LAN server
    /// answered, or macOS refused a `.local` lookup) release the Local Network alert hold and its
    /// dialog slot, and the door's hold of a question a first LAN use asked (through the door, or
    /// the Commons gate's own ask past the wait) ([`StepsReach::end_door_hold`]; the step started
    /// this re-probe for that question,
    /// [`Self::local_network_asked_through_the_door`]), first so the worker's Secrets lane sees no
    /// dialog up when [`AccessOutcome::Granted`] wakes it, then record `Granted` when a LAN server
    /// answered. A probe that heard nothing (no ALab vault on this network, an answer after its
    /// 30 s, or nobody there to answer) leaves the hold to the person's time with the alert
    /// ([`Self::look_at_local_network_alert`]). Whatever it
    /// heard, the LAN readers look again now ([`StepsReach::lan_answer_moved`]): a grant or a
    /// Don't Allow moved the answer, and a probe that heard nothing still ends with the question
    /// asked, which a look before it did not know.
    pub(crate) fn on_local_network_probe_ready(&mut self, now: Instant) {
        if !self.config.swarm_enabled() {
            cancel_local_network_probe();
            self.access_steps.end_local_network_hold();
            self.access_steps.local_network_recheck = None;
            self.release_unused_access_dialog(now);
            return;
        }
        let Some((revision, probe)) = (self.access_steps.reach.take_local_network_probe)() else {
            return;
        };
        if self.access_steps.local_network_done != Some(revision) {
            return;
        }
        self.access_steps.local_network_recheck = match probe {
            #[cfg(any(target_os = "macos", test))]
            LocalNetworkProbe::Unanswered => Some((revision, now + LOCAL_NETWORK_RETRY)),
            LocalNetworkProbe::Granted => None,
            #[cfg(any(target_os = "macos", test))]
            LocalNetworkProbe::Refused => None,
        };
        // An answer heard ends the alert's hold; a re-probe that heard none leaves it to the
        // person's time with the alert ([`Self::look_at_local_network_alert`]): one raised while
        // they were away still stands, unanswered, when they come back.
        if probe_heard_an_answer(probe) {
            self.access_steps.end_local_network_hold();
            self.release_unused_access_dialog(now);
            if (self.access_steps.reach.end_door_hold)() {
                aterm_log::info!(
                    "system state: the LAN re-probe heard macOS's Local Network answer (revision \
                     {revision}); the door's hold of the question a first LAN use asked ends"
                );
            }
        }
        if probe == LocalNetworkProbe::Granted {
            self.record_access_at(revision, Access::LocalNetwork, AccessOutcome::Granted);
            aterm_log::info!(
                "system state: Local Network granted (LAN answered, revision {revision})"
            );
        }
        (self.access_steps.reach.lan_answer_moved)();
    }

    /// Count the person's time with macOS's Local Network alert the step raised, and end its hold
    /// once they have had [`LOCAL_NETWORK_ALERT_HOLD`] with it. macOS says neither whether the
    /// alert is still up nor how it was answered: the re-probe hears an answer only where a LAN
    /// server answers or macOS refuses (`on_local_network_probe_ready` ends the hold then), and
    /// the alert watch sees the alert go only on a macOS whose alert host was measured, once a
    /// new window of its owner came and, at a read with the person at the Mac, no window of that
    /// owner is left (`on_local_network_alert_answered` ends the hold then); an alert raised
    /// while the person was away (present within the steps' five minutes, the step's
    /// moment, but not at the screen) stands, unanswered, until they come back. So the count
    /// starts at the first look that reads them present ([`ALERT_PRESENT_WITHIN`], the screen not
    /// locked or switched away) with a touch of the keyboard or pointer since the ask
    /// ([`Self::person_present_within`]), and adds the time between two looks only with them
    /// present at both, at most [`ALERT_PRESENT_WITHIN`] of it: time away does not count. While
    /// the hold stands no page or dialog of the window's comes up beside the alert (it is one of
    /// the window's gestures, [`Self::access_gesture_up`]), the permission walk's start among
    /// them. Called at each turn's first look at the steps (`App::tick_permission_walk`) and at
    /// the step's own.
    pub(crate) fn look_at_local_network_alert(&mut self, now: Instant) {
        let Some(mut alert) = self.access_steps.local_network_alert else {
            return;
        };
        let present =
            !(self.screen_away)() && self.person_present_within(ALERT_PRESENT_WITHIN, now);
        alert.present_for = match alert.present_for {
            None => (present
                && self.person_present_within(now.saturating_duration_since(alert.asked), now))
            .then_some(Duration::ZERO),
            Some(counted) if present && alert.present => Some(
                counted
                    + now
                        .saturating_duration_since(alert.looked)
                        .min(ALERT_PRESENT_WITHIN),
            ),
            Some(counted) => Some(counted),
        };
        alert.looked = now;
        alert.present = present;
        if alert
            .present_for
            .is_some_and(|counted| counted >= LOCAL_NETWORK_ALERT_HOLD)
        {
            self.access_steps.end_local_network_hold();
            aterm_log::info!(
                "system state: the Local Network alert's hold is over: the person has been at \
                 the Mac {} s since their first input after the ask",
                LOCAL_NETWORK_ALERT_HOLD.as_secs()
            );
        } else {
            self.access_steps.local_network_alert = Some(alert);
        }
    }

    /// Drain the alert watch's answer ([`StepsReach::take_local_network_alert_answer`]; live, a
    /// worker's [`watch_alert`] read the window list, and [`crate::Wake::LocalNetworkAlertWatch`]
    /// brings it): a window of the alert's owner that its first read did not list on screen came,
    /// and a later read, with the person at the Mac, listed no window of that owner at all, on
    /// screen or off, so no alert of it is up, the one the step's trigger raised included,
    /// whatever the person answered. The hold ends now: the
    /// dialog slot goes as the LAN re-probe's answer lets it go
    /// ([`Self::on_local_network_probe_ready`]), and the LAN readers look again
    /// ([`StepsReach::lan_answer_moved`]). Nothing is recorded: the answer is not known, and the
    /// re-probe still records a grant it hears later. An answer ends only the hold whose trigger
    /// armed its watch, while that hold stands: one heard once the hold ended (the person's time,
    /// the re-probe's answer, the swarm switched off) ends nothing. With no answer (no new window
    /// on screen at any read after the first, so also an alert drawn and taken down between two
    /// reads; a window list the watch could not read before the trigger; a window of the owner
    /// still listed, on screen or off; the person away at every read that listed none; or no
    /// watch on this macOS) the hold ends only by the re-probe's answer or the person's time
    /// ([`Self::look_at_local_network_alert`]). The consent lanes' turn drains it before the
    /// permission walk looks for its start, so a walk waiting on the hold starts in the same
    /// turn. The door's watch of a question a first LAN use asked ([`door_question_watch`])
    /// leaves nothing here: it ends the door's hold itself, and its wake brings a turn only so a
    /// walk waiting on the dialog slot looks at once.
    pub(crate) fn on_local_network_alert_answered(&mut self, now: Instant) {
        let Some(revision) = (self.access_steps.reach.take_local_network_alert_answer)() else {
            return;
        };
        if !self.config.swarm_enabled()
            || self.access_steps.local_network_watch != Some(revision)
            || self.access_steps.local_network_alert.is_none()
        {
            return;
        }
        self.access_steps.end_local_network_hold();
        self.release_unused_access_dialog(now);
        (self.access_steps.reach.lan_answer_moved)();
        aterm_log::info!(
            "system state: no window of macOS's Local Network alert's owner is left, with the \
             person at the Mac (revision {revision}); its hold ends"
        );
    }

    /// When the next look at the Local Network alert's hold may end it
    /// ([`Self::look_at_local_network_alert`]): the rest of [`LOCAL_NETWORK_ALERT_HOLD`] while the
    /// person is being counted present, else a [`devtools_helper::RECHECK_EVERY`], to see them
    /// touch the Mac or come back.
    fn local_network_alert_deadline(&self, now: Instant) -> Option<Instant> {
        let alert = self.access_steps.local_network_alert?;
        Some(match alert.present_for {
            Some(counted) if alert.present => {
                now + LOCAL_NETWORK_ALERT_HOLD.saturating_sub(counted)
            }
            _ => now + devtools_helper::RECHECK_EVERY,
        })
    }

    /// The Local Network step, first in the run: once per approved revision, a UDP connect
    /// (nothing sent) to a host on an attached subnet, or a short Bonjour browse of its own on a
    /// network with no IPv4 subnet to name ([`lan_ask`]), which makes macOS ask its Local Network
    /// question; recorded `triggered`. At its moment the step takes the dialog slot and starts an
    /// alert watch, and the trigger waits for the watch's first read
    /// ([`Self::local_network_trigger_waits`]), so the baseline is read before the connect can
    /// raise the alert. Its alert's hold starts at the trigger, which arms the watch, and the
    /// watch ends it sooner once, with the person at the Mac, no window of the alert's owner is
    /// left ([`Self::on_local_network_alert_answered`]). Where no watch starts, the trigger goes
    /// at once; a trigger that named no LAN stops the watch. A question asked through a first LAN
    /// use's door ([`StepsReach::door_asked`]) starts no watch of the step's: the door keeps its
    /// own ([`door_question_watch`]). A revision already recorded is done. On no network at all
    /// the other steps go on, and it looks again, at its moment only,
    /// after its watch subscribes and when this Mac's network moves
    /// ([`StepsReach::watch_network`]), not on a timer: nothing else changes what it finds, and
    /// the Local Network row reads calm meanwhile ([`LOCAL_NETWORK_NOTHING_TO_ASK`]). aterm.log
    /// says so once per such look. A trigger that had a network to ask on but no socket or
    /// browse looks again [`LOCAL_NETWORK_RETRY`] later, at its moment only: one due away from it is
    /// moved on. A retry is dropped only for another approved revision; while none is approved
    /// it is kept but arms no deadline ([`Self::next_access_step_deadline`]), so the same
    /// revision coming back looks again.
    pub(crate) fn tick_local_network_step(&mut self, now: Instant) {
        self.on_local_network_probe_ready(now);
        if !self.config.swarm_enabled() {
            return;
        }
        self.look_at_local_network_alert(now);
        if self
            .access_steps
            .pane_until
            .is_some_and(|until| now >= until)
        {
            self.access_steps.pane_until = None;
        }
        // A trigger waiting for its alert watch's first read goes, or is dropped, before the
        // step's recheck, retry or record look in this turn.
        if self.local_network_trigger_waits(now) {
            return;
        }
        let Some(revision) = self.access_revision() else {
            if self.access_steps.local_network_done.is_none() {
                let _ = self.note_local_network_hold(now, None);
            }
            return;
        };
        if let Some((checked, at)) = self.access_steps.local_network_recheck {
            if checked != revision {
                self.access_steps.local_network_recheck = None;
            } else if now >= at && self.access_steps_allowed() {
                self.access_steps.local_network_recheck = None;
                (self.access_steps.reach.start_local_network_probe)(revision);
                return;
            }
        }
        if self
            .access_steps
            .local_network_retry
            .is_some_and(|(failed, _)| failed != revision)
        {
            self.access_steps.local_network_retry = None;
            self.access_steps.nothing_to_ask = false;
        }
        if self.access_steps.local_network_done == Some(revision) {
            // A trigger that found nothing is tried again: once the network moved after one that
            // found no LAN, a minute on after one with no socket.
            let Some((_, at)) = self.access_steps.local_network_retry else {
                return;
            };
            match at {
                Some(at) if now < at => return,
                Some(_) => {
                    if !self.local_network_moment(now) {
                        // Due, but not the step's moment (the person is in a terminal tab,
                        // typing, or away): look again a retry later, never at once.
                        self.access_steps.local_network_retry =
                            Some((revision, Some(now + LOCAL_NETWORK_RETRY)));
                        return;
                    }
                }
                // The move stays told until the step's moment comes.
                None if !self
                    .access_steps
                    .network_moved
                    .load(std::sync::atomic::Ordering::Acquire)
                    || !self.local_network_moment(now) =>
                {
                    return;
                }
                None => {}
            }
        }
        if self.local_network_on_record(revision) {
            return;
        }
        if (self.access_steps.reach.door_asked)() {
            self.local_network_asked_through_the_door(revision);
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
                // timed retry waits again rather than leave a deadline due now;
                // a network move stays told for the next look.
                if let Some((_, Some(at))) = &mut self.access_steps.local_network_retry {
                    *at = now + LOCAL_NETWORK_RETRY;
                }
                return;
            };
            self.access_steps.dialog = Some(dialog);
        }
        self.access_steps.local_network_done = Some(revision);
        if (self.access_steps.reach.start_local_network_alert_watch)(revision) {
            // The trigger waits for the watch's first read, the windows up before it, so no
            // alert it raises can be in that read; one already in goes in this turn.
            self.access_steps.local_network_baseline =
                Some((revision, now + LOCAL_NETWORK_ALERT_BASELINE_WAIT));
            self.local_network_trigger_waits(now);
        } else {
            self.fire_local_network_trigger(revision, now, false);
        }
    }

    /// A Local Network trigger waiting for its alert watch's first read
    /// ([`StepsReach::local_network_alert_baseline_in`]) goes once that read is in, the watch
    /// armed; or, once [`LOCAL_NETWORK_ALERT_BASELINE_WAIT`] has passed without it, goes with the
    /// watch stopped, so its hold ends only by the re-probe's answer or the person's time. The
    /// moment the step took stands for it: the wait is one window-list read. A wait whose
    /// revision is no longer the approved one, or whose `[privacy]` switches went off, is
    /// dropped ([`AccessSteps::end_local_network_hold`]): its trigger never goes, nothing is
    /// recorded, a network move told before it stays told, and the step runs again at its next
    /// moment. Whether a trigger waited in this turn (still waiting, or gone now), so the step
    /// does nothing more in it.
    fn local_network_trigger_waits(&mut self, now: Instant) -> bool {
        let Some((revision, by)) = self.access_steps.local_network_baseline else {
            return false;
        };
        if self.access_revision() != Some(revision) || !self.access_steps_allowed() {
            self.access_steps.end_local_network_hold();
            self.release_unused_access_dialog(now);
            return false;
        }
        let baseline_in = (self.access_steps.reach.local_network_alert_baseline_in)();
        if !baseline_in && now < by {
            return true;
        }
        self.access_steps.local_network_baseline = None;
        if !baseline_in {
            (self.access_steps.reach.cancel_local_network_alert_watch)();
            aterm_log::info!(
                "system state: the Local Network alert watch's first read did not come back \
                 within {} ms (revision {revision}): the trigger goes without the watch, and its \
                 hold ends only by the re-probe's answer or the person's time",
                LOCAL_NETWORK_ALERT_BASELINE_WAIT.as_millis()
            );
        }
        self.fire_local_network_trigger(revision, now, baseline_in);
        true
    }

    /// The Local Network step's trigger for `revision`, its dialog slot held: a trigger that
    /// named a LAN hands the LAN over, raises the alert's hold, arms the alert watch when
    /// `watched` (its first read is in), records `triggered` and starts the LAN re-probe; one
    /// that named none stops the watch when `watched`, lets the slot go and looks again
    /// ([`Self::local_network_named_nothing`]).
    fn fire_local_network_trigger(&mut self, revision: u64, now: Instant, watched: bool) {
        // This look reads the network as it is now: a move told before it is spent.
        self.access_steps
            .network_moved
            .store(false, std::sync::atomic::Ordering::Release);
        if (self.access_steps.reach.trigger_local_network)() {
            // macOS asked under this step's slot: the Commons runtime may use the LAN now.
            (self.access_steps.reach.hand_lan_over)(revision);
            self.access_steps.local_network_retry = None;
            self.access_steps.nothing_to_ask = false;
            self.access_steps.local_network_alert = Some(LocalNetworkAlert::asked(now));
            if watched {
                // The hold has no end on the clock, so the watch reads until the hold's end
                // stops it ([`AccessSteps::end_local_network_hold`]).
                (self.access_steps.reach.arm_local_network_alert_watch)();
                self.access_steps.local_network_watch = Some(revision);
            }
            self.record_access_step(Access::LocalNetwork, AccessOutcome::Triggered);
            // The record now says asked, so a LAN look made before it (`sync.peers`' "macOS has
            // not asked") is stale from here: the readers look again now.
            (self.access_steps.reach.lan_answer_moved)();
            (self.access_steps.reach.start_local_network_probe)(revision);
            aterm_log::info!("system state: Local Network asked for (revision {revision})");
        } else {
            if watched {
                (self.access_steps.reach.cancel_local_network_alert_watch)();
            }
            self.access_steps.dialog = None;
            self.local_network_named_nothing(revision, now);
        }
    }

    /// A first LAN use already asked macOS's Local Network question past the wait for the step
    /// (`lan_first_use::asked`): that ask is the step's question for `revision`, recorded
    /// `triggered` as the step's own trigger is. Where a use of this process asked it (a use
    /// through the door, or the Commons gate's own ask past the wait), the door's hold holds this
    /// process's dialog slot for the question, counting the person's time as the step's alert
    /// hold does; where the vault subscriber, another process, asked it, nothing here holds it.
    /// So the step raises nothing, holds no alert of its own and hands nothing over: one
    /// question, at most one hold. The re-probe listens for the answer as after the step's own,
    /// and an answer it hears ends the door's hold, if one stands
    /// ([`Self::on_local_network_probe_ready`]). The step starts no watch of its own for that
    /// question: the door's hold started one before its use asked, where the window installed it
    /// ([`door_question_watch`]) and that watch's first read came back, and that watch ends the
    /// door's hold itself ([`Self::on_local_network_alert_answered`] ends only the step's own).
    fn local_network_asked_through_the_door(&mut self, revision: u64) {
        self.access_steps.local_network_held = None;
        self.access_steps.local_network_done = Some(revision);
        self.access_steps.local_network_retry = None;
        self.access_steps.nothing_to_ask = false;
        self.record_access_step(Access::LocalNetwork, AccessOutcome::Triggered);
        (self.access_steps.reach.lan_answer_moved)();
        (self.access_steps.reach.start_local_network_probe)(revision);
        aterm_log::info!(
            "system state: Local Network was asked through the door by a first LAN use (revision \
             {revision}); the step reads it as its question"
        );
    }

    /// The Local Network trigger for `revision` asked nothing: on no network at all the step
    /// looks again once this Mac's network moves (the watch started now, once per process) and
    /// the row reads calm; on a network with no socket or browse to ask with, a
    /// [`LOCAL_NETWORK_RETRY`] later. aterm.log says which, once per look.
    fn local_network_named_nothing(&mut self, revision: u64, now: Instant) {
        let here = (self.access_steps.reach.lan_here)();
        self.access_steps.nothing_to_ask = here == LanHere::Offline;
        if here == LanHere::Lan {
            self.access_steps.local_network_retry =
                Some((revision, Some(now + LOCAL_NETWORK_RETRY)));
            aterm_log::info!(
                "system state: no socket or browse for the Local Network trigger (revision \
                 {revision}); trying again in {}s",
                LOCAL_NETWORK_RETRY.as_secs()
            );
            return;
        }
        self.access_steps.local_network_retry = Some((revision, None));
        if !self.access_steps.watching_network {
            self.access_steps.watching_network = true;
            let moved = Arc::clone(&self.access_steps.network_moved);
            let wake = self.devtools_hand_over_wake();
            (self.access_steps.reach.watch_network)(Arc::new(move || {
                moved.store(true, std::sync::atomic::Ordering::Release);
                wake();
            }));
        }
        aterm_log::info!(
            "system state: this Mac is on no network, so the Local Network step has nothing to \
             ask macOS with yet (revision {revision}); the Local Network row reads calm, and the \
             step looks again when the network changes"
        );
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
        // While the launch's offer waits for the page's answer at its first tick, its headline
        // is the one signal: no menu line, as no diamond (§3.3, §3.6).
        let line = if self.system_state_launch_offer_waits() {
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

    /// The access steps' next look: a hold ending, or the end of a Local Network trigger's wait
    /// for its alert watch's first read; while the Local Network step or a client's
    /// pane act waits with an aterm window focused, the next look for the moment; and the
    /// approved revision's Local Network retry while its moment can come (the switches on and a
    /// window focused; a focus gain wakes the loop). The step's tick moves a retry that fell due
    /// away from its moment, so this never arms a deadline that is due again at once.
    pub(crate) fn next_access_step_deadline(&self, now: Instant) -> Option<Instant> {
        // A trigger waiting for its alert watch's first read goes, at the latest, at the wait's
        // end; the watch's answer wakes the loop itself, so no look of it is armed here.
        let hold = self
            .local_network_alert_deadline(now)
            .into_iter()
            .chain(self.access_steps.local_network_baseline.map(|(_, by)| by))
            .chain(self.access_steps.pane_until);
        let waits = (self.local_network_pending() && self.access_steps.reach.holds_dialog)
            || self.access_steps.pane_act.is_some();
        // A retry that waits for the network to move arms nothing until it has (the watch wakes
        // the loop then), and then looks for the step's moment.
        let retry = self
            .access_steps
            .local_network_retry
            .filter(|(failed, _)| Some(*failed) == self.access_revision())
            .and_then(|(_, at)| match at {
                Some(at) => Some(at.max(now)),
                None => self
                    .access_steps
                    .network_moved
                    .load(std::sync::atomic::Ordering::Acquire)
                    .then(|| now + devtools_helper::RECHECK_EVERY),
            });
        // Automatic steps keep their retry timer even when aterm is in the background.
        let focused = (waits || retry.is_some()) && self.access_steps_allowed();
        let look = (focused && waits).then(|| now + devtools_helper::RECHECK_EVERY);
        let retry = retry.filter(|_| focused);
        let saves = self
            .access_steps
            .recorded
            .iter()
            .filter_map(|record| record.retry_at)
            .map(|at| at.max(now));
        hold.chain(look)
            .chain(retry)
            .chain(
                self.access_steps
                    .local_network_recheck
                    .and_then(|(revision, at)| {
                        (self.config.swarm_enabled()
                            && self.access_steps_allowed()
                            && self.access_revision() == Some(revision))
                        .then_some(at.max(now))
                    }),
            )
            .chain(saves)
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
    /// page's headline and row.
    pub(crate) fn system_state_access_step(&self) -> Option<Access> {
        if let Some(panel) = self.devtools_helper.panel() {
            return Some(panel.page().access());
        }
        // Only an alert the step raised names Local Network: a trigger still waiting for its
        // alert watch's first read has raised none.
        if self.access_steps.local_network_alert.is_some() {
            return Some(Access::LocalNetwork);
        }
        let revision = self.access_revision()?;
        if !self.access_steps_allowed() || !(self.access_steps.reach.frontmost)(self) {
            return None;
        }
        if self.access_steps.local_network_done != Some(revision)
            || self.access_steps.local_network_baseline.is_some()
        {
            // A check waiting to start, or a trigger waiting for its alert watch's first read,
            // is not evidence that macOS needs an answer, and no later step starts beside it.
            // The actual alert above, or a refused observation in the row, names that need.
            None
        } else if self.devtools_helper.fda_waiting() {
            Some(Access::FullDiskAccess)
        } else if self.devtools_helper.devtools_waiting() {
            Some(Access::DeveloperTools)
        } else {
            None
        }
    }

    /// A press on a Needs-you row's button (`Open Full Disk Access`); `true` when System
    /// Settings is up for it now. A person's press opens the
    /// page past any Not now, and never waits behind a helper panel they cannot see
    /// ([`Self::press_meets_unseen_panel`]): the panel's own row brings its page forward, and
    /// any other row's press has the panel give way first. A control client's act (`by_person`
    /// false) does nothing without an approved revision or with
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
                // Setup's one early elevation turns developer mode on; the row offers no
                // press of its own.
                Access::Debugger => (false, "setup's own elevation's"),
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
    /// the step moment (design §6). Local Network is per revision already. The first
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
    /// forgetting a reading that only said the probe was off. The folder warm-up, Local Network
    /// and queued acts resume by themselves.
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

/// How long the Local Network step's own Bonjour browse runs ([`LanAsk::Browse`]): macOS asks
/// as the browse starts, and its answer outlives the browse.
#[cfg(target_os = "macos")]
const LOCAL_NETWORK_BROWSE_FOR: Duration = Duration::from_secs(5);

/// The live trigger: from this Mac's interfaces ([`lan_ask`]), connect a UDP socket to a host
/// on an attached subnet (nothing is sent), or, on a network with no IPv4 subnet to name, start
/// a short `_alab._tcp` browse on a thread of its own. `false` when nothing was attempted (no
/// network, no socket, no thread), so nothing is recorded and no hold is taken.
#[cfg(target_os = "macos")]
fn live_trigger_local_network() -> bool {
    match lan_ask(&interface_addresses(), ipv6_network_up()) {
        None => false,
        Some(LanAsk::Connect(target)) => live_connect_local_network(target),
        Some(LanAsk::Browse) => live_browse_local_network(),
    }
}

#[cfg(target_os = "macos")]
fn live_connect_local_network(target: Ipv4Addr) -> bool {
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

/// The step's own Bonjour browse ([`LanAsk::Browse`]): `_alab._tcp` on `local.`, the service
/// the bundle declares, for [`LOCAL_NETWORK_BROWSE_FOR`], hearing nothing it keeps.
#[cfg(target_os = "macos")]
fn live_browse_local_network() -> bool {
    if aterm_agent::node_service::DiscoveryPolicy::process_discovery().mesh {
        // The node job owns the only browse and asks macOS when this step permits it.
        return true;
    }
    let Ok(service) = std::ffi::CString::new(aterm_agent::node_service::ALAB_TCP_SERVICE) else {
        return false;
    };
    let spawned = std::thread::Builder::new()
        .name("aterm-local-network-ask".into())
        .spawn(move || {
            let until = Instant::now() + LOCAL_NETWORK_BROWSE_FOR;
            let browsed = aterm_net::bonjour::browse_while(
                &service,
                c"local.",
                Duration::from_millis(500),
                || Instant::now() < until,
                |_| std::ops::ControlFlow::Continue(()),
            );
            if let Err(error) = browsed {
                // A browse macOS refused is still the question asked, as a connect's is.
                aterm_log::info!("system state: the Local Network browse ended: {error}");
            }
        });
    match spawned {
        Ok(_) => {
            aterm_log::info!(
                "system state: this Mac's network has no IPv4 subnet to name, so the Local \
                 Network step asks through a short Bonjour browse"
            );
            true
        }
        Err(error) => {
            aterm_log::info!("system state: no thread for the Local Network browse: {error}");
            false
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn live_trigger_local_network() -> bool {
    let _ = LOCAL_NETWORK_PORT;
    false
}

/// What this Mac's interfaces give the Local Network step to ask with now ([`LanHere`]).
#[cfg(target_os = "macos")]
fn live_lan_here() -> LanHere {
    if lan_ask(&interface_addresses(), ipv6_network_up()).is_some() {
        LanHere::Lan
    } else {
        LanHere::Offline
    }
}

#[cfg(not(target_os = "macos"))]
const fn live_lan_here() -> LanHere {
    LanHere::Offline
}

/// Request an App reading after subscription and every network wake, on a thread of its own
/// for the life of the process. The signal's latest cause can hide a coalesced route change;
/// the App reads current local state at its permitted moment, including an Offline change.
#[cfg(target_os = "macos")]
fn live_watch_network(moved: devtools_helper::Wake) {
    let spawned = std::thread::Builder::new()
        .name("aterm-local-network-moves".into())
        .spawn(move || {
            let mut watch = crate::network_moved::watch();
            watch_network_moves(
                std::iter::repeat_with(|| watch.wait(crate::network_moved::LONG)),
                || moved(),
            );
        });
    if let Err(error) = spawned {
        aterm_log::warn!(
            "system state: the Local Network step's network watch could not start: {error}"
        );
    }
}

#[cfg(not(target_os = "macos"))]
fn live_watch_network(_moved: devtools_helper::Wake) {}

/// Request initial reconciliation only after the source is subscribed, then after every
/// non-timeout move. This level callback asks the App for its authoritative fresh reading;
/// filtering with a worker-local LAN snapshot could retain a stale Offline status while
/// its callback waits. The live source is infinite; tests use finite moves without a worker.
#[cfg(any(target_os = "macos", test))]
fn watch_network_moves(
    moves: impl IntoIterator<Item = crate::network_moved::Woke>,
    mut moved: impl FnMut(),
) {
    moved();
    for wake in moves {
        if wake != crate::network_moved::Woke::Timer {
            moved();
        }
    }
}

/// What a Local Network alert watch's window and worker share under the watch's lock.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AlertWatchState {
    /// The live watch, by the id its start took; 0 for none. A start or a stop moves it on, so a
    /// worker an earlier start began stops at once and leaves no answer.
    id: u64,
    /// The last id a start took.
    last_id: u64,
    /// Whose question the live watch follows (`None` for none).
    watched: Option<Watched>,
    /// Whether the live watch's first read came back (read, or found unreadable).
    baseline_in: bool,
    /// Whether that first read found the window list unreadable, so the watch's worker ended at
    /// it ([`AlertWatchEnd::Blind`]).
    blind: bool,
    /// When the live watch's trigger went (`None` until it does), so it reads until it is
    /// stopped.
    armed: Option<Instant>,
    /// Whose alert the live watch saw answered, until the window (or the door's watch) drains
    /// it.
    answer: Option<Watched>,
}

/// A process's Local Network alert watch, which the window (through [`StepsReach`]'s arms) or
/// the door's hold ([`door_question_watch`]) and the watch's worker ([`run_alert_watch`])
/// share: the window process's own are [`LIVE_ALERT_WATCH`], the step's, and
/// [`DOOR_ALERT_WATCH`], the door's, and a test makes its own.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct AlertWatchSlot {
    state: std::sync::Mutex<AlertWatchState>,
    /// Wakes the worker when the window arms, stops or replaces its watch.
    moved: std::sync::Condvar,
    /// Wakes a wait for the first read ([`Self::await_baseline`]) when it comes back, or when
    /// the watch is stopped or replaced.
    baseline: std::sync::Condvar,
    /// How many waits on `moved` a worker began, each counted under the lock it then gives up
    /// to wait: a test that reads a new count and then takes the lock knows the worker sleeps.
    #[cfg(test)]
    waits: std::sync::atomic::AtomicUsize,
}

/// What a wait for an alert watch's first read found ([`AlertWatchSlot::await_baseline`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BaselineWait {
    /// It came back with the window list read: once armed, the watch reads on.
    Read,
    /// It came back unable to read the window list: the watch's worker ended at it
    /// ([`AlertWatchEnd::Blind`]), and nothing reads on.
    Blind,
    /// It did not come back within the wait, or the watch was stopped or replaced first.
    NotIn,
}

/// What a worker's wait for its next look came to ([`AlertWatchSlot::next_look`]).
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    /// Its trigger armed it and its wait is over: it reads again.
    Again,
    /// It was stopped or replaced.
    Stopped,
    /// Its trigger neither armed nor stopped it within the arm's wait.
    ArmWaitRanOut,
}

#[cfg(any(target_os = "macos", test))]
impl AlertWatchSlot {
    pub(crate) const fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(AlertWatchState {
                id: 0,
                last_id: 0,
                watched: None,
                baseline_in: false,
                blind: false,
                armed: None,
                answer: None,
            }),
            moved: std::sync::Condvar::new(),
            baseline: std::sync::Condvar::new(),
            #[cfg(test)]
            waits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Take a new watch's id, of `watched`'s question, in place of any watch before it, whose
    /// worker this wakes, so it stops at once and leaves no answer; an answer not yet drained is
    /// dropped. `None` when the lock was poisoned.
    fn begin(&self, watched: Watched) -> Option<u64> {
        let id = {
            let mut state = self.state.lock().ok()?;
            let id = state.last_id + 1;
            *state = AlertWatchState {
                id,
                last_id: id,
                watched: Some(watched),
                baseline_in: false,
                blind: false,
                armed: None,
                answer: None,
            };
            id
        };
        self.moved.notify_all();
        self.baseline.notify_all();
        Some(id)
    }

    /// Whether the live watch's first read came back (read, or found unreadable).
    pub(crate) fn baseline_in(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.id != 0 && state.baseline_in)
    }

    /// The watch `id`'s first read came back, having `read` the window list or not: the trigger
    /// waiting for it may go, if `id` is still the live watch.
    fn mark_baseline_in(&self, id: u64, read: bool) {
        if let Ok(mut state) = self.state.lock()
            && state.id == id
        {
            state.baseline_in = true;
            state.blind = !read;
        }
        self.baseline.notify_all();
    }

    /// Wait, at most `wait`, for the first read of the live watch of `watched`'s question to come
    /// back: what it found. [`BaselineWait::NotIn`] at once once that watch was stopped or
    /// replaced (each wakes this wait).
    pub(crate) fn await_baseline(&self, watched: Watched, wait: Duration) -> BaselineWait {
        let Ok(mut state) = self.state.lock() else {
            return BaselineWait::NotIn;
        };
        let asked = Instant::now();
        loop {
            if state.id == 0 || state.watched != Some(watched) {
                return BaselineWait::NotIn;
            }
            if state.baseline_in {
                return if state.blind {
                    BaselineWait::Blind
                } else {
                    BaselineWait::Read
                };
            }
            let left = wait.saturating_sub(asked.elapsed());
            if left.is_zero() {
                return BaselineWait::NotIn;
            }
            state = match self.baseline.wait_timeout(state, left) {
                Ok((state, _)) => state,
                Err(_) => return BaselineWait::NotIn,
            };
        }
    }

    /// Arm the live watch: its trigger went now, so its worker, woken now, reads until it is
    /// stopped. A second arm of the same watch keeps the first's time.
    pub(crate) fn arm(&self) {
        self.arm_where(None);
    }

    /// [`Self::arm`], only where the live watch follows `watched`'s question.
    pub(crate) fn arm_of(&self, watched: Watched) {
        self.arm_where(Some(watched));
    }

    /// Arm the live watch, where it follows `watched`'s question (`None`: whichever it follows).
    fn arm_where(&self, watched: Option<Watched>) {
        if let Ok(mut state) = self.state.lock()
            && state.id != 0
            && watched.is_none_or(|watched| state.watched == Some(watched))
        {
            state.armed.get_or_insert_with(Instant::now);
        }
        self.moved.notify_all();
    }

    /// Drain whose alert the live watch saw answered, if any.
    pub(crate) fn take_answer(&self) -> Option<Watched> {
        self.state
            .lock()
            .ok()
            .and_then(|mut state| state.answer.take())
    }

    /// Stop the live watch: its worker, woken now, leaves no answer from here on and stops at
    /// once, and an answer it left is dropped.
    pub(crate) fn cancel(&self) {
        self.cancel_where(None);
    }

    /// [`Self::cancel`], only where the live watch follows `watched`'s question: a later watch's
    /// is left alone.
    pub(crate) fn cancel_of(&self, watched: Watched) {
        self.cancel_where(Some(watched));
    }

    /// Stop the live watch, where it follows `watched`'s question (`None`: whichever it follows).
    fn cancel_where(&self, watched: Option<Watched>) {
        if let Ok(mut state) = self.state.lock()
            && watched.is_none_or(|watched| state.watched == Some(watched))
        {
            state.id = 0;
            state.watched = None;
            state.baseline_in = false;
            state.blind = false;
            state.armed = None;
            state.answer = None;
        }
        self.moved.notify_all();
        self.baseline.notify_all();
    }

    /// The watch `id`'s end, under the lock, given whether it `answered`: a stop, which empties
    /// the answer under that lock, either came first and this leaves nothing, or comes after and
    /// empties what this left. Leaves `watched` iff it answered and `id` is still the live
    /// watch, and says whether it is.
    fn settle(&self, id: u64, watched: Watched, answered: bool) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let live = state.id == id;
        if live && answered {
            state.answer = Some(watched);
        }
        live
    }

    /// The watch `id`'s next look: before its trigger has armed it, it waits for that, at most
    /// `arm_wait` (a hang detector for a window that never comes back to it); then it waits
    /// `period(armed)`, given when the trigger armed it. [`Look::Stopped`] at once, without
    /// waiting out either, once the watch was stopped or replaced (each wakes it). It holds the
    /// lock only while it waits on it, never across a read.
    fn next_look(
        &self,
        id: u64,
        period: impl FnOnce(Instant) -> Duration,
        arm_wait: Duration,
    ) -> Look {
        let Ok(mut state) = self.state.lock() else {
            return Look::Stopped;
        };
        let asked = Instant::now();
        while state.id == id && state.armed.is_none() {
            let left = (asked + arm_wait).saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Look::ArmWaitRanOut;
            }
            #[cfg(test)]
            self.waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            state = match self.moved.wait_timeout(state, left) {
                Ok((state, _)) => state,
                Err(_) => return Look::Stopped,
            };
        }
        let armed = match state.armed {
            Some(armed) if state.id == id => armed,
            _ => return Look::Stopped,
        };
        let next = Instant::now() + period(armed);
        loop {
            if state.id != id {
                return Look::Stopped;
            }
            let left = next.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Look::Again;
            }
            #[cfg(test)]
            self.waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            state = match self.moved.wait_timeout(state, left) {
                Ok((state, _)) => state,
                Err(_) => return Look::Stopped,
            };
        }
    }

    /// How many waits on the window's moves a worker began ([`Self::next_look`]).
    #[cfg(test)]
    fn waits(&self) -> usize {
        self.waits.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// What the lock holds now, for a test to read.
    #[cfg(test)]
    fn state(&self) -> AlertWatchState {
        *self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// How often an alert watch's worker reads ([`run_alert_watch`]): every `look` for `first`
/// after its trigger armed it, whatever the person; past that, every `look` while its last read
/// found the person at the Mac and every `away` while it found them away; and at most
/// `arm_wait` for its trigger to arm or stop it.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct AlertWatchPace {
    pub(crate) look: Duration,
    pub(crate) away: Duration,
    pub(crate) first: Duration,
    pub(crate) arm_wait: Duration,
}

/// What an alert watch's worker reaches ([`run_alert_watch`]), injected so a test drives the
/// worker with a scripted window list and reads what it wakes and logs.
#[cfg(any(target_os = "macos", test))]
pub(crate) struct AlertWatchIo {
    /// One read of the window list's windows of the alert's owner ([`watch_alert`]'s `read`).
    pub(crate) read: Box<dyn FnMut() -> Option<Vec<AlertWindow>> + Send>,
    /// Whether the person is at the Mac now ([`watch_alert`]'s `present`).
    pub(crate) present: Box<dyn FnMut() -> bool + Send>,
    /// Wake the window: the first read came back, or the watch left its answer.
    pub(crate) wake: Box<dyn Fn() + Send>,
    /// Log the watch's end ([`finish_alert_watch`]).
    pub(crate) log: Box<dyn FnOnce(AlertWatchReport) + Send>,
    pub(crate) pace: AlertWatchPace,
}

/// Start a Local Network alert watch of `watched`'s question on `slot` in place of any watch
/// before it, on a macOS whose alert host was measured (`product_version`,
/// [`alert_host_measured`]): a [`crate::qos::Role::Responsive`] lane worker runs
/// [`run_alert_watch`] over `io`. `false`, with no worker and `slot` untouched, on any other macOS
/// (logged), and `false`, with the watch stopped, when the worker did not start.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn start_alert_watch(
    slot: &'static AlertWatchSlot,
    watched: Watched,
    product_version: Option<&str>,
    io: AlertWatchIo,
) -> bool {
    if !alert_host_measured(product_version) {
        aterm_log::info!(
            "{}: no Local Network alert watch on macOS {} ({watched}): its alert's window was \
             measured on 26 and 27 only, so its hold ends only by the re-probe's answer or the \
             person's time",
            watched.logger(),
            product_version.unwrap_or("of an unknown version")
        );
        return false;
    }
    let started = Instant::now();
    let Some(id) = slot.begin(watched) else {
        return false;
    };
    let spawned = crate::qos::spawn_lane_thread(
        "aterm-local-network-alert",
        crate::qos::Role::Responsive,
        move || run_alert_watch(slot, id, watched, started, io),
    );
    if let Err(error) = spawned {
        aterm_log::warn!(
            "{}: the Local Network alert watch did not start ({watched}): {error}",
            watched.logger()
        );
        slot.cancel_of(watched);
        return false;
    }
    true
}

/// An alert watch's worker, for the watch `id` on `slot` of `watched`'s question, started at
/// `started`: [`watch_alert`] over `io`'s reads. Its first read goes at once; once it is back the
/// worker marks it in ([`AlertWatchSlot::baseline_in`], which wakes a door's wait for it,
/// [`AlertWatchSlot::await_baseline`]) and wakes the window, whose trigger waits for it. It then
/// waits for the trigger, or the door, to arm it ([`AlertWatchSlot::arm`]; at most
/// `pace.arm_wait`, a hang detector) and reads until it is stopped ([`AlertWatchSlot::cancel`],
/// which every end of the hold calls) or replaced: every `pace.look` for `pace.first` after the
/// arm, whatever the person, so an alert drawn then is seen unless it goes within one
/// `pace.look`; past that, every `pace.look` while its last read found the person at the Mac and
/// every `pace.away` while it found them away. It never holds the slot's lock across a read. An
/// answer is left on the slot for the window (or the door's watch), which a wake brings to drain
/// it, and every end makes one log record ([`finish_alert_watch`]).
#[cfg(any(target_os = "macos", test))]
fn run_alert_watch(
    slot: &AlertWatchSlot,
    id: u64,
    watched: Watched,
    started: Instant,
    io: AlertWatchIo,
) {
    let AlertWatchIo {
        mut read,
        present,
        wake,
        log,
        pace,
    } = io;
    let mut baseline = None;
    let mut looks = AlertWatchLooks::Stopped;
    let end = watch_alert(
        || {
            let listed = read();
            if baseline.is_none() {
                baseline = Some(BaselineRead {
                    after: started.elapsed(),
                    windows: listed.as_ref().map(Vec::len),
                });
                slot.mark_baseline_in(id, listed.is_some());
                // The step's trigger waiting for this read goes in the window's next turn (the
                // door's wait was woken by the mark).
                wake();
            }
            listed
        },
        present,
        |here| {
            let period = |armed: Instant| {
                if here || armed.elapsed() < pace.first {
                    pace.look
                } else {
                    pace.away
                }
            };
            match slot.next_look(id, period, pace.arm_wait) {
                Look::Again => {
                    looks = AlertWatchLooks::Read;
                    true
                }
                Look::Stopped => false,
                Look::ArmWaitRanOut => {
                    looks = AlertWatchLooks::ArmWaitRanOut;
                    false
                }
            }
        },
    );
    finish_alert_watch(
        end,
        looks,
        watched,
        baseline,
        |answered| slot.settle(id, watched, answered),
        wake,
        log,
    );
}

/// The window process's Local Network alert watch ([`StepsReach::live`]'s arms).
#[cfg(target_os = "macos")]
static LIVE_ALERT_WATCH: AlertWatchSlot = AlertWatchSlot::new();

// The live watch reads the person as `crate::person_at_this_screen` does (a touch within
// `PERSON_PRESENT_WITHIN`, the screen neither locked nor switched away), which is how
// `App::look_at_local_network_alert` counts the person's time with the alert.
#[cfg(target_os = "macos")]
const _: () = assert!(
    ALERT_PRESENT_WITHIN.as_millis() == crate::system_state_host::PERSON_PRESENT_WITHIN.as_millis()
);

/// What a live alert watch's worker reaches ([`AlertWatchIo`]): the window list's windows of
/// [`LOCAL_NETWORK_ALERT_OWNERS`], on screen or off (`devtools_helper::live::windows_owned_by`:
/// owner names, window numbers and whether each is on screen, no title, so no Screen Recording
/// grant), the person read as `crate::person_at_this_screen` reads them from any thread, `wake`
/// and aterm.log, reading every [`LOCAL_NETWORK_ALERT_LOOK`] for
/// [`LOCAL_NETWORK_ALERT_FIRST_LOOKS`] after the arm and then with the person at the Mac, and
/// every [`LOCAL_NETWORK_ALERT_AWAY_LOOK`] past those first looks with them away. A window-list
/// read waits on WindowServer, so none runs on the main thread.
#[cfg(target_os = "macos")]
fn live_alert_watch_io(wake: Box<dyn Fn() + Send>) -> AlertWatchIo {
    AlertWatchIo {
        read: Box::new(|| {
            devtools_helper::live::windows_owned_by(&LOCAL_NETWORK_ALERT_OWNERS).map(|windows| {
                windows
                    .into_iter()
                    .map(|(number, on_screen)| AlertWindow { number, on_screen })
                    .collect()
            })
        }),
        present: Box::new(crate::person_at_this_screen),
        wake,
        log: Box::new(|report| aterm_log::info!("{report}")),
        pace: AlertWatchPace {
            look: LOCAL_NETWORK_ALERT_LOOK,
            away: LOCAL_NETWORK_ALERT_AWAY_LOOK,
            first: LOCAL_NETWORK_ALERT_FIRST_LOOKS,
            arm_wait: LOCAL_NETWORK_ALERT_ARM_WAIT,
        },
    }
}

/// Wake the window: [`crate::Wake::LocalNetworkAlertWatch`], whose turn runs the consent lanes.
#[cfg(target_os = "macos")]
fn wake_for_alert_watch() {
    let _ = crate::workstation_setup::send_setup_wake(crate::Wake::LocalNetworkAlertWatch);
}

/// The live alert watch of the step's own alert: [`start_alert_watch`] on [`LIVE_ALERT_WATCH`]
/// over this macOS's version and [`live_alert_watch_io`], waking the window with
/// [`crate::Wake::LocalNetworkAlertWatch`]. Only [`StepsReach::live`] reaches it, which a
/// windowed macOS instance alone uses: a headless instance starts no watch.
#[cfg(target_os = "macos")]
fn live_start_local_network_alert_watch(revision: u64) -> bool {
    start_alert_watch(
        &LIVE_ALERT_WATCH,
        Watched::Step(revision),
        crate::signing_identity::os_version(),
        live_alert_watch_io(Box::new(wake_for_alert_watch)),
    )
}

#[cfg(not(target_os = "macos"))]
const fn live_start_local_network_alert_watch(_revision: u64) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn live_local_network_alert_baseline_in() -> bool {
    LIVE_ALERT_WATCH.baseline_in()
}

#[cfg(not(target_os = "macos"))]
const fn live_local_network_alert_baseline_in() -> bool {
    false
}

#[cfg(target_os = "macos")]
fn live_arm_local_network_alert_watch() {
    LIVE_ALERT_WATCH.arm();
}

#[cfg(not(target_os = "macos"))]
const fn live_arm_local_network_alert_watch() {}

#[cfg(target_os = "macos")]
fn live_take_local_network_alert_answer() -> Option<u64> {
    LIVE_ALERT_WATCH.take_answer().and_then(Watched::revision)
}

#[cfg(not(target_os = "macos"))]
const fn live_take_local_network_alert_answer() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn live_cancel_local_network_alert_watch() {
    LIVE_ALERT_WATCH.cancel();
}

#[cfg(not(target_os = "macos"))]
const fn live_cancel_local_network_alert_watch() {}

/// Start the alert watch of the question door pass `pass` is about to raise through
/// `lan_first_use`'s door (a use through it, or the Commons gate's first use past the wait for
/// the step), on `slot` and over `io`, and wait, at most `baseline_wait`, for its first read,
/// which the use waits for before it asks
/// (`aterm_types::macos_setup::lan_first_use::QuestionWatch::start`): `true` once that read is in
/// with the window list read and the watch armed, so it reads on until stopped
/// ([`AlertWatchSlot::cancel_of`]). Its answer is not left for the window: the worker's wake
/// drains it and runs `answered(pass)`, which ends the door's hold of that pass, and only then
/// `io`'s wake; the first read's wake runs nothing, since the use's own wait was woken.
/// `false`, the watch stopped, where none starts ([`start_alert_watch`]: a macOS whose alert host
/// was not measured, a worker that did not start), its first read was not back in time
/// (logged), or that read could not read the window list (its worker ended there and logged
/// it): the use then asks unwatched, and the door's hold ends only by the re-probe's answer or
/// the person's time.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn start_door_alert_watch(
    slot: &'static AlertWatchSlot,
    pass: u64,
    product_version: Option<&str>,
    io: AlertWatchIo,
    answered: impl Fn(u64) + Send + 'static,
    baseline_wait: Duration,
) -> bool {
    let watched = Watched::Door(pass);
    let AlertWatchIo {
        read,
        present,
        wake,
        log,
        pace,
    } = io;
    let io = AlertWatchIo {
        read,
        present,
        wake: Box::new(move || {
            if let Some(Watched::Door(pass)) = slot.take_answer() {
                answered(pass);
                wake();
            }
        }),
        log,
        pace,
    };
    if !start_alert_watch(slot, watched, product_version, io) {
        return false;
    }
    match slot.await_baseline(watched, baseline_wait) {
        BaselineWait::Read => {
            slot.arm_of(watched);
            return true;
        }
        BaselineWait::Blind => {
            slot.cancel_of(watched);
            return false;
        }
        BaselineWait::NotIn => slot.cancel_of(watched),
    }
    aterm_log::info!(
        "lan first use: the door's Local Network alert watch's first read did not come back \
         within {} ms ({watched}): the use asks without the watch, and the door's hold ends only \
         by the re-probe's answer or the person's time",
        baseline_wait.as_millis()
    );
    false
}

/// The window process's watch of the question the door's hold holds ([`door_question_watch`]).
#[cfg(target_os = "macos")]
static DOOR_ALERT_WATCH: AlertWatchSlot = AlertWatchSlot::new();

/// The window's watch of macOS's Local Network question a first LAN use raises through
/// `lan_first_use`'s door, or the Commons gate's first use raises by itself past the wait for
/// the step (`aterm_types::macos_setup::lan_first_use::install_question_watch`):
/// [`start_door_alert_watch`] on [`DOOR_ALERT_WATCH`] over this macOS's version and
/// [`live_alert_watch_io`], its first read waited for at most
/// [`LOCAL_NETWORK_ALERT_BASELINE_WAIT`] on the use's own thread (a window's door uses are made
/// from its setup worker and its network readers' worker threads, the gate's from its Commons
/// runtime's owner thread), and an answer that ends the
/// door's hold waking the window ([`crate::Wake::LocalNetworkAlertWatch`]), so a walk waiting on
/// the dialog slot looks at once; the hold's end stops it by its pass
/// ([`AlertWatchSlot::cancel_of`]). The window process alone installs it: a headless or private
/// instance installs no door.
#[cfg(target_os = "macos")]
pub(crate) fn door_question_watch() -> aterm_types::macos_setup::lan_first_use::QuestionWatch {
    aterm_types::macos_setup::lan_first_use::QuestionWatch {
        start: |pass, answered| {
            start_door_alert_watch(
                &DOOR_ALERT_WATCH,
                pass,
                crate::signing_identity::os_version(),
                live_alert_watch_io(Box::new(wake_for_alert_watch)),
                answered,
                LOCAL_NETWORK_ALERT_BASELINE_WAIT,
            )
        },
        stop: |pass| DOOR_ALERT_WATCH.cancel_of(Watched::Door(pass)),
    }
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

fn live_local_network_recorded_before() -> Option<AccessOutcome> {
    let state = aterm_types::dirs::state_dir()?;
    aterm_types::macos_setup::access_record::latest_of_any_revision(&state, Access::LocalNetwork)
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
