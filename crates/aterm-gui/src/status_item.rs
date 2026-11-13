// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use aterm_agent::fleet_access::Basis;
use aterm_types::fleet_access::FleetAccess;
use aterm_types::needs::{HoldWord, NeedWord, Severity, WaitWord};

use crate::task_record::TaskState;

/// A click's place in the glance it was drawn from: the glance's generation ([`Glance::generation`]) and an index into one of its target tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Pick {
    /// The drawn glance's generation.
    pub generation: u16,
    /// The index into the table the action names.
    pub index: u16,
}

impl Pick {
    #[cfg(any(target_os = "macos", test))]
    fn packed(self) -> u64 {
        (u64::from(self.generation) << 16) | u64::from(self.index)
    }

    #[cfg(any(target_os = "macos", test))]
    fn unpacked(payload: u64) -> Option<Self> {
        let payload = u32::try_from(payload).ok()?;
        Some(Self {
            generation: u16::try_from(payload >> 16).ok()?,
            index: u16::try_from(payload & 0xffff).ok()?,
        })
    }
}

/// One user action from the status-item menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum StatusAction {
    /// File the exceptions digest: every fact it stands on goes to the log as one dismissal — what `Dismiss all` does on Settings ▸ Messages, the same press.
    DismissExceptions,
    /// Overview…: the card of every session (`App::open_overview`).
    Overview,
    /// Read This Mac: `aterm ctl survey` into a document tab.
    ReadThisMac,
    /// Hand Back All: lift every person hold.
    HandBackAll,
    /// Open Settings ▸ Messages at one digest entry (payload: its log id) — what a click on a "To look at" line does.
    ShowException(u64),
    /// Go to a tab ([`Glance::go`]): a need row, a worker, a manager, a waiting tab, a held tab, a finished worker with no result.
    Go(Pick),
    /// Read a finished worker's result in a document tab ([`Glance::reads`]), which marks it seen.
    ReadResult(Pick),
    /// A sibling aterm's need row ([`Glance::raises`]): raise its tab there.
    RaiseSibling(Pick),
    /// The Fleet submenu's Read the Fleet in Full: `aterm ctl survey --fleet` into a document tab.
    ReadFleet,
    /// The Fleet submenu's Refresh Now: a fresh fleet glance, whatever the last one's age.
    ReadFleetNow,
    /// A fleet need's Read This Session ([`Glance::peers`]).
    PeerRead(Pick),
    /// A fleet need's Screen Sharing to its Mac ([`Glance::peers`]).
    PeerScreen(Pick),
    /// A fleet need's Copy "`ssh <its Mac>`" ([`Glance::peers`]).
    PeerSsh(Pick),
    /// A fleet need's Take Over on its Mac — `aterm ctl --host <it> hold <sid> on`, the step-in — where that Mac lets this one drive it ([`PeerRef::drive`]).
    PeerTakeOver(Pick),
    /// A fleet need's Hand Back on its Mac: `aterm ctl --host <it> hold <sid> off`.
    PeerHandBack(Pick),
    /// A Fleet submenu Mac's Read in Full: the pick's generation is the fleet snapshot's ([`crate::fleet_glance::FleetSlot::generation`]), its index the Mac's.
    MacRead(Pick),
    /// A Fleet submenu Mac's Screen Sharing.
    MacScreen(Pick),
    /// A Fleet submenu Mac's Copy "`ssh <it>`".
    MacSsh(Pick),
    /// The System State line's press ([`Glance::system_state`]): the System
    /// State tab in the frontmost window.
    OpenSystemState,
    /// A left click on the lock: this Mac's fleet access one step on ([`LockFace::click`]).
    /// `repeated` is the second or a later click of a multi-click, which lowers or cancels
    /// and never raises.
    RotateFleetAccess {
        /// AppKit counted the release as 2 or more.
        repeated: bool,
    },
    /// A level picked in the Fleet Access submenu: this Mac's fleet access, set at once
    /// ([`LockFace::pick`]).
    SetFleetAccess(FleetAccess),
}

#[cfg(any(target_os = "macos", test))]
const TAG_BAND: isize = 1_000_000_000_000;

#[cfg(any(target_os = "macos", test))]
impl StatusAction {
    /// The `NSMenuItem.tag` this action rides in.
    pub(crate) fn tag(self) -> isize {
        let packed = |kind: isize, payload: u64| -> isize {
            isize::try_from(payload)
                .ok()
                .filter(|p| *p < TAG_BAND)
                .map_or(0, |p| kind * TAG_BAND + p)
        };
        match self {
            Self::DismissExceptions => 5,
            Self::Overview => 6,
            Self::ReadThisMac => 7,
            Self::HandBackAll => 9,
            Self::ReadFleet => 10,
            Self::ReadFleetNow => 11,
            Self::OpenSystemState => 13,
            Self::RotateFleetAccess { repeated: false } => 14,
            Self::RotateFleetAccess { repeated: true } => 15,
            Self::ShowException(id) => packed(4, id),
            Self::Go(pick) => packed(5, pick.packed()),
            Self::ReadResult(pick) => packed(6, pick.packed()),
            Self::RaiseSibling(pick) => packed(7, pick.packed()),
            Self::PeerRead(pick) => packed(8, pick.packed()),
            Self::PeerScreen(pick) => packed(9, pick.packed()),
            Self::PeerSsh(pick) => packed(10, pick.packed()),
            Self::MacRead(pick) => packed(11, pick.packed()),
            Self::MacScreen(pick) => packed(12, pick.packed()),
            Self::MacSsh(pick) => packed(13, pick.packed()),
            Self::PeerTakeOver(pick) => packed(14, pick.packed()),
            Self::PeerHandBack(pick) => packed(15, pick.packed()),
            // The level's place in the rising order: 0, 1 or 2.
            Self::SetFleetAccess(level) => packed(16, level as u64),
        }
    }

    /// Inverse of [`tag`](Self::tag); unknown and retired tags are `None` (an inert item).
    pub(crate) fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            5 => Some(Self::DismissExceptions),
            6 => Some(Self::Overview),
            7 => Some(Self::ReadThisMac),
            9 => Some(Self::HandBackAll),
            10 => Some(Self::ReadFleet),
            11 => Some(Self::ReadFleetNow),
            13 => Some(Self::OpenSystemState),
            14 => Some(Self::RotateFleetAccess { repeated: false }),
            15 => Some(Self::RotateFleetAccess { repeated: true }),
            t if t >= TAG_BAND => {
                let payload = u64::try_from(t % TAG_BAND).ok()?;
                let pick = || Pick::unpacked(payload);
                match t / TAG_BAND {
                    4 => Some(Self::ShowException(payload)),
                    5 => pick().map(Self::Go),
                    6 => pick().map(Self::ReadResult),
                    7 => pick().map(Self::RaiseSibling),
                    8 => pick().map(Self::PeerRead),
                    9 => pick().map(Self::PeerScreen),
                    10 => pick().map(Self::PeerSsh),
                    11 => pick().map(Self::MacRead),
                    12 => pick().map(Self::MacScreen),
                    13 => pick().map(Self::MacSsh),
                    14 => pick().map(Self::PeerTakeOver),
                    15 => pick().map(Self::PeerHandBack),
                    16 => usize::try_from(payload)
                        .ok()
                        .and_then(|at| FleetAccess::ALL.get(at).copied())
                        .map(Self::SetFleetAccess),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// How long the icon rests on a higher level before that level takes effect, and how long a
/// click's level stays named in the title (`docs/DESIGN-fleet-access-lock-2026-11-10.md` §6.3).
#[cfg(any(target_os = "macos", test))]
pub(crate) const SETTLE: std::time::Duration = aterm_types::fleet_access::SETTLE;

/// THE LOCK'S FACE (§6.3 of the fleet access design): the level on disk, the higher level a
/// click asked for that has not taken effect yet, and how long the title names the level a
/// click chose. Pure, with the clock handed in; the App owns one and writes what it is told to.
///
/// A raise waits [`SETTLE`] with the icon resting on it and a lower is written at once, so one
/// accidental click admits nothing, a click on round to Read Only inside the settle writes
/// nothing, and passing over Full Access on the way back down never opens it. A raise is asked
/// over the state as it stood at its click ([`Basis`]): a level another writer stated since,
/// this Mac leaving its fleet and its moving to another each drop it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LockFace {
    /// The level in force, as the state file last read.
    on_disk: FleetAccess,
    /// What the state held when it was read: the level file's stamp and the fleet held.
    basis: Basis,
    /// A raise that has not taken effect: the level, and when it is written.
    pending: Option<(FleetAccess, std::time::Instant)>,
    /// Until when the title names the shown level: a click is that fresh.
    flash: Option<std::time::Instant>,
}

impl LockFace {
    /// The face of a Mac whose stored level reads `on_disk`.
    pub(crate) const fn new(on_disk: FleetAccess) -> Self {
        Self {
            on_disk,
            basis: Basis {
                stored: None,
                fleet: None,
            },
            pending: None,
            flash: None,
        }
    }

    /// The level in force (the menu's checkmark).
    pub(crate) const fn on_disk(&self) -> FleetAccess {
        self.on_disk
    }

    /// What a raise written now is asked over: the state as this face last read it.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn basis(&self) -> Basis {
        self.basis
    }

    /// Whether a raise waits its settle: a level at Read and Write or above that this Mac's
    /// person already asked for. The fleet listener, unix's, is told.
    #[cfg(any(unix, test))]
    pub(crate) const fn raising(&self) -> bool {
        self.pending.is_some()
    }

    /// The level the icon shows: a pending raise's, else the one in force. It can only overstate
    /// how open this Mac is.
    pub(crate) fn shown(&self) -> FleetAccess {
        self.pending.map_or(self.on_disk, |(level, _)| level)
    }

    /// The level the title names at `now`, while a click is fresh.
    pub(crate) fn flash(&self, now: std::time::Instant) -> Option<FleetAccess> {
        self.flash
            .is_some_and(|until| now < until)
            .then(|| self.shown())
    }

    /// A left click at `now`: the shown level one step on. A level no higher than the one in
    /// force clears a pending raise and is the level to write now (`None` where it is the one in
    /// force already); a higher one becomes the pending raise, written by [`Self::due`] once the
    /// icon has rested on it for [`SETTLE`].
    ///
    /// A `repeated` click (the second or a later one of a multi-click) is honoured only where
    /// its step lowers or cancels: a double-click never raises two steps, and a quick second
    /// click on round to Read Only is never lost, which would leave the first click's raise to
    /// fall due.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn click(&mut self, now: std::time::Instant, repeated: bool) -> Option<FleetAccess> {
        let next = self.shown().rotated();
        if next <= self.on_disk {
            self.flash = Some(now + SETTLE);
            self.pending = None;
            (next != self.on_disk).then_some(next)
        } else {
            if !repeated {
                self.flash = Some(now + SETTLE);
                self.pending = Some((next, now + SETTLE));
            }
            None
        }
    }

    /// Whether the pending raise's time has come at `now`: the App reads the state again
    /// ([`Self::seen`]) before it takes it ([`Self::due`]).
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn raise_due(&self, now: std::time::Instant) -> bool {
        self.pending.is_some_and(|(_, at)| at <= now)
    }

    /// The pending raise to write at `now`, once its time has come; a flash that ran out is
    /// cleared with it.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn due(&mut self, now: std::time::Instant) -> Option<FleetAccess> {
        if self.flash.is_some_and(|until| until <= now) {
            self.flash = None;
        }
        let (level, at) = self.pending?;
        (at <= now).then(|| {
            self.pending = None;
            level
        })
    }

    /// A level picked in the menu: no raise is pending any more, and it is the level to write
    /// now, `None` where it is the one in force.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn pick(&mut self, level: FleetAccess) -> Option<FleetAccess> {
        self.pending = None;
        self.flash = None;
        (level != self.on_disk).then_some(level)
    }

    /// The menu opened: a pending raise is cancelled.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn menu_opened(&mut self) {
        self.pending = None;
        self.flash = None;
    }

    /// This App wrote `level`, and the state reads `basis` after it: it is the level in force.
    /// A click's flash stays.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) const fn wrote(&mut self, level: FleetAccess, basis: Basis) {
        self.on_disk = level;
        self.basis = basis;
    }

    /// The state reads `level`, over `basis`. Another writer's level (`aterm fleet access`,
    /// another window's lock) replaces the one in force and cancels a pending raise; so does a
    /// state that moved under a pending raise with the level as it was: the same level stated
    /// again (`aterm fleet access read` at Read Only), the fleet left, another fleet joined.
    /// The state this face already holds, which the notification of its own write reads,
    /// changes nothing. Answers whether the face changed.
    pub(crate) fn seen(&mut self, level: FleetAccess, basis: Basis) -> bool {
        if level == self.on_disk && (basis == self.basis || self.pending.is_none()) {
            self.basis = basis;
            return false;
        }
        *self = Self {
            basis,
            ..Self::new(level)
        };
        true
    }

    /// When the face next changes by itself: a pending raise falls due, or the flash runs out.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn next_deadline(&self) -> Option<std::time::Instant> {
        let due = self.pending.map(|(_, at)| at);
        match (due, self.flash) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// What a release of the menu-bar button does.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClickAct {
    /// Open the menu.
    Menu,
    /// Rotate this Mac's fleet access ([`StatusAction::RotateFleetAccess`]); `repeated` for the
    /// second or a later click of a multi-click.
    Rotate {
        /// AppKit counted the release as 2 or more.
        repeated: bool,
    },
}

/// What a release of the button means, read from the app's current `event` and the one already
/// read (`stale`), on a Mac that is `in_fleet` or not (§6.2). Only a pointer's plain left
/// release in a fleet rotates the level (`aterm_types::status_click::pointed`), and it says
/// whether it is a multi-click's second or later release, which [`LockFace::click`] lets lower
/// and never raise. Every other press opens the menu: a right release, a Control-click, every
/// click out of a fleet (where no lock shows), and a press that is no pointer's own, such as a
/// key's on the focused item (Control-F8, then Space) or an accessibility press, which finds
/// no event or a stale one. A level is then one pick away in the menu's Fleet Access submenu,
/// written at once, and no press that cannot see the icon rest changes a security level. The
/// event reading is acaffeine's (`aterm_types::status_click`); these two rules are this item's
/// alone.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn click_act(
    event: Option<aterm_types::status_click::EventKey>,
    stale: Option<aterm_types::status_click::EventKey>,
    in_fleet: bool,
) -> ClickAct {
    use aterm_types::status_click::{pointed, repeated};
    if in_fleet && pointed(event, stale) {
        ClickAct::Rotate {
            repeated: repeated(event, stale),
        }
    } else {
        ClickAct::Menu
    }
}

/// One session as the glance reads it, copied under the leaf locks by `App::status_session_row`: its tab label, its published verdict ([`crate::needs::Verdict`]), its agent word and its task record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionRow {
    /// Process-local session id.
    pub id: u64,
    /// The session's global sid (a task's `from=` names it).
    pub sid: String,
    /// The tab's label as every Agents surface names it (`App::tab_label_of_session`): its role, else its title pin, else the strip's own rungs (the live OSC title without the status glyph an agent leads it with, what a parked worker runs instead while nothing it runs has titled its tab (its agent, or a program worker's program), then the cwd), else the tab's position, else the short sid.
    pub title: String,
    /// What the session needs of its owner now, as published.
    pub need: Option<crate::needs::Need>,
    /// What aterm is waiting out for it, as published.
    pub waiting: Option<crate::needs::Waiting>,
    /// The wait is an unsupervised limit before its reset (the verdict's recheck): nobody continues it but the person, at the reset.
    pub unsupervised: bool,
    /// Whose hold stands, as published.
    pub held: Option<HoldWord>,
    /// The published agent word (`busy`, `idle`, `prompt`, …); empty or `-` for no identified agent.
    pub agent: &'static str,
    /// Where the reader names no state (`agent=unknown`, codex's), the survey's word for what the
    /// program in front says of itself (`program_state::survey_word` of its shown OSC 7501
    /// report), as `survey` reads it at render; `None` under any other word or with no report.
    pub program: Option<&'static str>,
    /// The task record, for a worker.
    pub task: Option<TaskRow>,
    /// When the need was first published (unix seconds, [`NeedAges::observe`]).
    pub need_since: Option<i64>,
    /// When the hold was first seen standing (unix seconds).
    pub held_since: Option<i64>,
    /// How many writes aimed at the tab the person's hold refused since it was set (`SessionFabric::person_refusals`); `0` with no person hold.
    pub person_refusals: u64,
    /// The Mac that holds the tab over the fleet (the step-in's remote slot), by the name this Mac's book gives it.
    pub held_by_host: Option<String>,
    /// Its supervised agent's task end, while one stands on the harness's glass (`harness_glass::EndGlass`).
    pub end: Option<EndRow>,
    /// Its Autopilot chip's face is LANDED (`ChipFace::Landed`: under Land, a clean `Nothing left:` defense, and no repository check found work left to land), seen or not, whether or not an end was told: This Mac counts it `landed`, a worker or a plain Land tab.
    pub landed: bool,
    /// It is LANDING (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.4.3 item 1): its Autopilot is Land, its chip wears Land's `⤓` (the task open or unknown), and a hold stands — Land's instruction has been typed to it, by aterm or by a codex-alab under its own proven claim (`App::shown_hold`, §14.3.3).
    pub landing: bool,
    /// What holds the landing, where a land act's yield did not land or nudges were typed, led by the stop where whoever drives it stopped typing into it in any mode (aterm, or a codex-alab under its own proven claim: `App::shown_stop`): its worker row's tip, in the hold's and the stop's words with no clock (`stopped typing: the same reply twice, no work · not landed yet: 2 unpushed commits · 3 nudges`, `ShownStop::words`' and `ShownHold::words`' tip forms; a codex-alab's stop in its name, `codex-alab stopped typing: …`, and its hold too where no stop named it first, `codex-alab: not landed yet: …`; a stopped Cruise tab's is the stop alone) — the menu keeps ages, composed at paint, and never a clock that would go stale.
    pub landing_tip: Option<String>,
    /// What aterm checked at its landing (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9, slice 9c), as its loop told it, read under the timeline's lock beside a landed face only (`SessionTimeline::land_receipt`): its Done for you row's tip and its worker row's say it (`mode_words::landed_tip`, `mode_words::checked_words`); none for a landing with no receipt (codex-alab's own), whose landed rows' tips say aterm checked nothing.
    pub receipt: Option<std::sync::Arc<aterm_agent::harness::acts::LandReceipt>>,
    /// Its need is all that holds its landing (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7): it is [`Self::landing`], its hold's yield was read, and the hold's one leg is its needs (`HoldLeg::Needs`, [`crate::harness_glass::HeldGlass::need_alone`]) — no repository work, no open words, no missing defense, no cut. Its report row's tip then says `<Program>'s landing waits on this alone` where any other says `<Program> carries on meanwhile`.
    pub need_alone: bool,
    /// Where its tab is (`in tab 3`, `in tab 3, left pane`; `App::session_place`), filled by the glance's first pass only where another row shares its label: every row that names the session says it after the label ([`Self::named`]).
    pub place: Option<String>,
}

impl SessionRow {
    /// The word the glance buckets the row by: the reader's own word, or, where it names no state,
    /// the program's (`program`).
    #[must_use]
    pub(crate) fn glance_word(&self) -> &'static str {
        self.program.unwrap_or(self.agent)
    }

    /// The session as a row names it: its label, and where it is when another row shares the label (`Claude Code in tab 2`).
    #[must_use]
    pub(crate) fn named(&self) -> String {
        match &self.place {
            Some(place) => format!("{} {place}", self.title),
            None => self.title.clone(),
        }
    }
}

/// A supervised agent's task end as the glance reads it: under Cruise / Land the one end is Land's landing (Cruise never ends), so an instant and whether it was seen, nothing that moves while it stands — no trail words, no live state words, no clock words — so the Done for you row it makes cannot go stale; its age is composed when the menu is painted ([`done_line`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct EndRow {
    /// The end's session-local serial: a captured result acknowledges this end.
    pub serial: u32,
    /// When the task's face moved there (unix milliseconds); `0` where no time is known, and the row then says no age.
    pub since_unix_ms: u64,
    /// Someone looked at it (the one looking rule, or the menu bar's Go).
    pub seen: bool,
}

/// A worker's task record as the glance reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskRow {
    /// The state.
    pub state: TaskState,
    /// What moved it there (`why=`).
    pub why: Option<&'static str>,
    /// Who started it: a global sid, or `ctl`.
    pub from: String,
    /// The rev the record entered its state at: one done, one key.
    pub entered: u64,
    /// When the state was entered (unix milliseconds).
    pub since_unix_ms: u64,
    /// The size of its result.
    pub result_bytes: u64,
    /// The file its result is in.
    pub result_path: Option<std::path::PathBuf>,
    /// The owner looked at this done (`App.done_seen` holds its rev).
    pub seen: bool,
    /// An `exited` task that had finished first ([`crate::task_record::TaskRecord::finished`]).
    pub finished: bool,
    /// A worker with its own report of how it ended
    /// ([`crate::task_record::reports_its_own_end`]): no end of it is listed for the owner.
    pub own_report: bool,
}

impl TaskRow {
    /// How it ended ([`TaskState::conclusion`]).
    #[must_use]
    pub(crate) const fn conclusion(&self) -> Option<crate::task_record::Conclusion> {
        self.state.conclusion(self.finished)
    }

    /// Whether it ended and nothing else says so: a stall keeps its need, and a working task or a
    /// worker with its own report has no end to list.
    #[must_use]
    const fn ended_quietly(&self) -> bool {
        self.state.has_an_end_to_read() && !self.own_report
    }
}

/// One same-Mac sibling aterm, from the last sibling scan (`sibling_scan`, its `survey facts`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SiblingInstance {
    /// Its pid.
    pub pid: u32,
    /// Its control socket.
    pub sock: String,
    /// Its live sessions.
    pub sessions: usize,
    /// Its sessions that need the owner, by their published `needs=`.
    pub needs: Vec<SiblingNeed>,
    /// When the scan read it (unix ms): its needs count only while this is younger than [`SIBLING_FRESH`], and their ages are measured from it.
    pub scanned_ms: u64,
}

/// A sibling's needs count on the badge only from a scan this young.
pub(crate) const SIBLING_FRESH: std::time::Duration = std::time::Duration::from_secs(45);

/// One sibling session's need, from its `survey facts` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SiblingNeed {
    /// The session's sid there (what `raise <sid>` names).
    pub sid: String,
    /// Its published need.
    pub word: NeedWord,
    /// Its role, else its program (a facts row carries no title).
    pub label: String,
    /// How long ago the fact the need stands on moved, when the row says.
    pub age_ms: Option<u64>,
    /// The need's words where the row says it is the program's own report (`needs_by=program`,
    /// [`crate::needs::claim_words_of_row`]).
    pub claim: Option<&'static str>,
    /// The row says the need is a harness's `NEEDS YOU:` report its agent carries on past
    /// (`needs_by=harness`), so it lists after the needs that stopped their agents ([`NeedRow::carries_on`]).
    pub carries_on: bool,
}

/// The stall half of a session's need facts — `input_stall::menu_row`'s words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputStallRow {
    /// The server's own attention words for the stall (`<prog> is frozen: not reading input since <HH:MM> (…) — restart it: …`), as its `meta attention=` entry carries them.
    pub text: String,
    /// The episode's identity (a hash of when its oldest unread byte was accepted): one need per stall, however its text moves.
    pub key: u64,
    /// The job is stopped with input queued (`signal cont` resumes it), not frozen: the need's word says which.
    pub stopped: bool,
}

pub(crate) use crate::needs::{fold_clip, wall_words};

/// THE NEED-AGE KEEPER: per session, since when its published need and its hold have stood ([`Self::observe`]), which the glance's ages read, and what a seamless update carried of them ([`Self::adopt`]).
#[derive(Debug, Default)]
pub(crate) struct NeedAges {
    since: std::collections::HashMap<u64, Since>,
    adopted: std::collections::HashMap<u64, Adopted>,
}

/// How long a seamless successor's first sighting of a need or a hold takes the age its outgoing process carried ([`NeedAges::adopt`]): room for every standing need to be published again, which the stall watch's re-detection (`input_backlog::STALL_AFTER`, stretched under load, then `input_stall::RECHECK`) bounds.
pub(crate) const ADOPT_SETTLE: std::time::Duration = std::time::Duration::from_secs(120);

/// What the status item knew of one session, carried across a seamless update as one row (`SessionRecord::status_seen`): the published need and since when it stood, the hold and since when, and the done the owner looked at, by when its task entered it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusCarry {
    /// The need's word and since when it stood (unix seconds).
    pub need: Option<(NeedWord, i64)>,
    /// The hold's word and since when it stood (unix seconds).
    pub held: Option<(HoldWord, i64)>,
    /// The done the owner looked at: its task's `since_unix_ms`, the wall-clock instant the record entered it (carried exactly, unlike its entry rev).
    pub done: Option<u64>,
}

impl StatusCarry {
    /// The row: `need=<word>@<unix s>`, `hold=<word>@<unix s>` and `done=<unix ms>`, each only
    /// when it holds; `None` for nothing known.
    #[cfg(any(unix, test))]
    pub(crate) fn row(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some((word, at)) = self.need {
            parts.push(format!("need={}@{at}", word.word()));
        }
        if let Some((word, at)) = self.held {
            parts.push(format!("hold={}@{at}", word.word()));
        }
        if let Some(ms) = self.done {
            parts.push(format!("done={ms}"));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// Read a carried row back. A part it does not know (an older parent's `told=`, `told_ids=` or
    /// `box=`) is read past.
    pub(crate) fn parse(row: &str) -> Self {
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let stamped = |value: &str| {
            let (word, at) = value.split_once('@')?;
            if !digits(at) {
                return None;
            }
            Some((word.to_string(), at.parse::<i64>().ok()?))
        };
        let mut carry = Self::default();
        for (key, value) in row.split_whitespace().filter_map(|t| t.split_once('=')) {
            match key {
                "need" => {
                    carry.need =
                        stamped(value).and_then(|(w, at)| Some((NeedWord::parse(&w)?, at)));
                }
                "hold" => {
                    carry.held =
                        stamped(value).and_then(|(w, at)| Some((HoldWord::parse(&w)?, at)));
                }
                "done" => carry.done = digits(value).then(|| value.parse().ok()).flatten(),
                _ => {}
            }
        }
        carry
    }
}

#[derive(Clone, Copy, Debug)]
struct Adopted {
    need: Option<(NeedWord, i64)>,
    held: Option<(HoldWord, i64)>,
    until_unix: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Since {
    need: Option<(NeedWord, u64, i64)>,
    held: Option<(HoldWord, i64)>,
    shown: u64,
}

fn shown_key(row: &SessionRow) -> u64 {
    let class = match row.glance_word() {
        "" | "-" => 0_u8,
        "idle" => 1,
        _ => 2,
    };
    let task = row
        .task
        .as_ref()
        .map(|t| (t.state, t.why, &t.from, t.entered, t.result_bytes, t.seen));
    let fact = (
        &row.title,
        row.need
            .as_ref()
            .map(|n| (n.word, n.key, &n.what, &n.harness)),
        &row.waiting,
        row.unsupervised,
        row.held,
        class,
        task,
    );
    aterm_hash::fnv1a_64(format!("{fact:?}").as_bytes())
}

impl NeedAges {
    /// Fold session `session`'s row at `now_unix` (`None`: it is gone): the first sight of a need (a new word or key) or of a hold (a new word) starts its age; the same one re-read keeps it.
    pub(crate) fn observe(
        &mut self,
        session: u64,
        row: Option<&SessionRow>,
        now_unix: i64,
    ) -> bool {
        let Some(row) = row else {
            return self.since.remove(&session).is_some();
        };
        let shown = shown_key(row);
        let since = self.since.entry(session).or_default();
        let moved = since.shown != shown;
        since.shown = shown;
        let mut adopted = self
            .adopted
            .get_mut(&session)
            .filter(|a| now_unix < a.until_unix);
        let need = row.need.as_ref().map(|n| (n.word, n.key));
        since.need = need.map(|(word, key)| match since.need {
            Some((w, k, at)) if (w, k) == (word, key) => (w, k, at),
            _ => {
                let carried = adopted.as_mut().and_then(|a| a.need.take());
                let at = carried
                    .filter(|(w, _)| *w == word)
                    .map_or(now_unix, |(_, at)| at);
                (word, key, at)
            }
        });
        since.held = row.held.map(|word| match since.held {
            Some((w, at)) if w == word => (w, at),
            _ => {
                let carried = adopted.as_mut().and_then(|a| a.held.take());
                let at = carried
                    .filter(|(w, _)| *w == word)
                    .map_or(now_unix, |(_, at)| at);
                (word, at)
            }
        });
        moved
    }

    /// A seamless update adopted `session` with what its outgoing process knew of it (`carry`, `SessionRecord::status_seen`), at `now_unix`: for [`ADOPT_SETTLE`], its first need published here takes the carried age when it is the carried word, and its first hold the carried hold's when it is the carried word ([`Self::observe`]). An update is no new need.
    pub(crate) fn adopt(&mut self, session: u64, carry: StatusCarry, now_unix: i64) {
        let settle = i64::try_from(ADOPT_SETTLE.as_secs()).unwrap_or(i64::MAX);
        self.adopted.insert(
            session,
            Adopted {
                need: carry.need,
                held: carry.held,
                until_unix: now_unix.saturating_add(settle),
            },
        );
    }

    /// What the keeper knows of `session` for the next process ([`StatusCarry`]; the done is the App's): the need and hold it last observed and since when.
    #[cfg(any(unix, test))]
    pub(crate) fn carry_of(&self, session: u64) -> StatusCarry {
        let since = self.since.get(&session).copied().unwrap_or_default();
        StatusCarry {
            need: since.need.map(|(word, _, at)| (word, at)),
            held: since.held,
            done: None,
        }
    }

    /// Since when `session`'s need has stood ([`Self::observe`]).
    pub(crate) fn need_since(&self, session: u64) -> Option<i64> {
        self.since.get(&session)?.need.map(|(_, _, at)| at)
    }

    /// Since when `session`'s hold has stood ([`Self::observe`]).
    pub(crate) fn held_since(&self, session: u64) -> Option<i64> {
        self.since.get(&session)?.held.map(|(_, at)| at)
    }

    /// Forget a retired session.
    pub(crate) fn retire(&mut self, session: u64) {
        self.since.remove(&session);
        self.adopted.remove(&session);
    }
}

/// The most need rows the menu spells out; the rest are counted on one `and k more in Overview` line.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) const NEEDS_SHOWN: usize = 10;
/// The most rows one level in a session's further needs get under its own row (§12.2.1); the needs past them are counted on that row as ` (+N more)`.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) const NEED_MORE_ROWS: usize = 2;
/// The most rows one level in the whole Needs you section draws, the sessions in the menu's order taking them first.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) const NEED_MORE_ALL: usize = 8;
/// The most manager rows the menu spells out.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) const MANAGERS_SHOWN: usize = 8;
/// The most Done for you rows the menu spells out.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) const DONE_SHOWN: usize = 5;
#[cfg(any(target_os = "macos", test))]
const EXCEPTION_LINES_SHOWN: usize = 4;
const LINE_MAX: usize = 120;

/// Where a need stands, in the order the menu lists them: this aterm's, then a same-Mac sibling's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Origin {
    /// This aterm.
    Local,
    /// A sibling aterm on this Mac (its pid).
    Sibling(u32),
    /// Another Mac of the fleet.
    Fleet,
}

/// What a click on a need row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum NeedAt {
    /// Go to the tab ([`Glance::go`]).
    Go(u16),
    /// Raise the sibling's tab ([`Glance::raises`]).
    Raise(u16),
    /// A fleet need: its submenu's acts ([`Glance::peers`]).
    Peer(u16),
}

/// One row under "Needs you".
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NeedRow {
    /// The need.
    pub word: NeedWord,
    /// Where it stands.
    pub origin: Origin,
    /// The tab's label.
    pub label: String,
    /// "(worker of lab-sweep)": the manager's label where the task's `from=` resolves here.
    pub worker_of: Option<String>,
    /// The need's words.
    pub what: String,
    /// A harness's need in its own words ([`crate::needs::Need::harness`]): the row says it clipped to the room its label and age leave ([`need_line`]). `Some` only for this aterm's `NEEDS YOU:` report, and its row's tip says its agent carries on ([`report_tip`]).
    pub harness: Option<crate::needs::HarnessAsk>,
    /// A harness's `NEEDS YOU:` report, which its agent carries on past: this aterm's ([`Self::harness`] is `Some`), a sibling's or a fleet peer's (its facts row's `needs_by=harness`). The list puts it after every need of its severity that has stopped an agent (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7, [`need_order`]).
    pub carries_on: bool,
    /// The report is all that holds its session's landing ([`SessionRow::need_alone`]): its tip says the landing waits on it alone. `false` for every need but a local report.
    pub alone: bool,
    /// Since when it has stood (unix seconds), for its age.
    pub since_unix: Option<i64>,
    /// What a click does.
    pub at: NeedAt,
    /// It counts on the badge: always for this Mac's; for a fleet need, only from a read younger than `fleet_glance::COUNT_FRESH`.
    pub counted: bool,
    /// The fleet read it comes from (unix ms): "(read 20 s ago)".
    pub read_at_ms: Option<u64>,
}

/// "This Mac": every live session of this aterm by what it is doing, and how many more a sibling aterm holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Here {
    /// Agents at work (a box a harness presses is work too).
    pub working: usize,
    /// Sessions whose published verdict needs the owner (stuck or a decision): never counted as working.
    pub need_you: usize,
    /// Sessions aterm is waiting something out for (a limit, the API, a writer's `kind=wait`): never counted as working.
    pub waiting: usize,
    /// Land sessions that hold ([`SessionRow::landing`]: Land's instruction typed, a hold standing), whatever their agent is doing, past a need or a wait, which count first: `docs/DESIGN-harness-autopilot-2026-10-15.md` §14.6. A Cruise session at rest is resting.
    pub landing: usize,
    /// Agents at rest.
    pub resting: usize,
    /// Sessions that LANDED: every session whose chip's face is landed (a plain Land tab included), and every worker whose record reads `done why=landed`.
    pub landed: usize,
    /// Workers whose task is done by an older build's rule (`why=marker|done-check`), which no harness writes any more, or by a hand's `aterm ctl task mark done` with no `why=landed`.
    pub done: usize,
    /// Workers whose task ended unfinished with their agent still there ([`TaskState::Failed`]).
    pub failed: usize,
    /// Workers whose agent exited.
    pub exited: usize,
    /// Sessions no identified agent reads.
    pub shells: usize,
    /// Live sessions of this Mac's sibling aterms.
    pub elsewhere: usize,
}

/// One worker under its manager.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerRow {
    /// Its tab's label.
    pub label: String,
    /// Its task's state.
    pub state: TaskState,
    /// Why it moved there.
    pub why: Option<&'static str>,
    /// An `exited` task that had finished first ([`crate::task_record::TaskRecord::finished`]).
    pub finished: bool,
    /// Since when (unix milliseconds).
    pub since_unix_ms: u64,
    /// Its result's size.
    pub result_bytes: u64,
    /// Go to it ([`Glance::go`]).
    pub go: u16,
    /// Read its result ([`Glance::reads`]), for a worker that ended with one, either way.
    pub read: Option<u16>,
    /// Its session is landing ([`SessionRow::landing`]): Land's instruction was typed to it and a hold stands.
    pub landing: bool,
    /// Its row's tip while it is landing or stopped ([`SessionRow::landing_tip`]); once it landed (`done why=landed`, its chip's face landed), what aterm checked there (`mode_words::checked_words`, §14.9 slice 9c).
    pub tip: Option<String>,
}

const WORKER_WORDS: [&str; 7] = [
    "landing", "working", "landed", "done", "failed", "exited", "stalled",
];

impl WorkerRow {
    /// What its row and its manager's count say of it: `landing` for a working task whose session is landing (Land's instruction typed, a hold standing: [`SessionRow::landing`]), `landed` for a record that reads `done why=landed` (Land landed its work), else its state's own word — a worker on its first task, before Land's instruction, says `working`, and an older build's `done why=marker|done-check` keeps `done`.
    #[must_use]
    pub(crate) fn word(&self) -> &'static str {
        if self.landing && self.state == TaskState::Working {
            "landing"
        } else if self.state == TaskState::Done && self.why == Some(crate::task_record::WHY_LANDED)
        {
            "landed"
        } else {
            self.state.as_str()
        }
    }
}

/// A manager: a session a live task's `from=` names, or the workers of a sid not on this instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagerRow {
    /// The manager's label; for a sid not here, its short sid.
    pub label: String,
    /// Go to the manager ([`Glance::go`]); `None` when it is not here.
    pub go: Option<u16>,
    /// Its workers, in id order.
    pub workers: Vec<WorkerRow>,
}

/// What a click on a Done for you row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DoneAt {
    /// Read its result ([`Glance::reads`]).
    Read(u16),
    /// Go to its tab ([`Glance::go`]): it has no result.
    Go(u16),
}

/// What came down that nobody here waits on and nobody has looked at: a finished worker started from `ctl` or whose manager is gone, or a supervised agent's task end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DoneRow {
    /// Its tab's label.
    pub label: String,
    /// When it finished (unix milliseconds); `0`: not known, no age.
    pub since_unix_ms: u64,
    /// Its result's size.
    pub result_bytes: u64,
    /// What a click does.
    pub at: DoneAt,
    /// A landing (`landed`), seen or not: a supervised agent's end, or a worker whose record reads `done why=landed` or whose end or face is a landing; else an older build's `done`.
    pub landed: bool,
    /// A landing's tip (§14.9, slice 9c) while its chip's face is landed ([`SessionRow::landed`]): what aterm checked there and what the agent said (`mode_words::landed_tip`); `None` for an older build's `done` and once the face moved off the landing, when no receipt is read.
    pub tip: Option<String>,
    /// How it ended unfinished, in a person's words ([`crate::task_record::failed_words`]); `None`
    /// for a row that finished.
    pub failed: Option<String>,
}

/// One Waiting cause: every tab aterm waits out for the same reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitRow {
    /// The cause.
    pub word: WaitWord,
    /// The wall's kind, where a wall is the cause.
    pub wall: Option<&'static str>,
    /// An unsupervised limit, waited out until the person continues it at its reset.
    pub unsupervised: bool,
    /// The reset the first of them names.
    pub reset: Option<String>,
    /// Since when the API has been out of reach (unix seconds).
    pub since_unix: Option<i64>,
    /// The `kind=wait` entry they wait on ([`WaitWord::Entry`]): its writer and words, which the row prints.
    pub entry: Option<crate::needs::WaitEntry>,
    /// Its tabs: label and where a click goes ([`Glance::go`]).
    pub tabs: Vec<(String, u16)>,
}

/// One held tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldRow {
    /// Its tab's label.
    pub label: String,
    /// "(worker of lab-sweep)": the manager's label where the task's `from=` resolves here — who waits while the tab is held.
    pub worker_of: Option<String>,
    /// Whose hold.
    pub hold: HoldWord,
    /// The Mac whose node holds it over the fleet ([`HoldWord::Remote`]).
    pub by_host: Option<String>,
    /// How many writes aimed at the tab the person's hold refused ([`SessionRow::person_refusals`]); `0` for a client's or the bus's.
    pub refused: u64,
    /// Since when (unix seconds).
    pub since_unix: Option<i64>,
    /// Go to it ([`Glance::go`]).
    pub go: u16,
}

/// A session on another Mac that this Mac holds over the fleet (the step-in's remote hold, its row's `held_from=` this Mac's node): listed whatever its need, since a hold is never one, so its Hand Back stays in reach for as long as the hold stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeldThereRow {
    /// "mac-17 · lab-sweep".
    pub label: String,
    /// Its session's acts ([`Glance::peers`]).
    pub at: u16,
}

/// A finished worker's result, as Read Result opens it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResultRef {
    /// The worker's local id.
    pub session: u64,
    /// The done it is (its task's entry rev): what reading it marks seen.
    pub entered: u64,
    /// The unseen end this result shared when drawn; no later end is acknowledged.
    pub end_serial: Option<u32>,
    /// The result file.
    pub path: std::path::PathBuf,
    /// The worker's label, for the tab's title.
    pub label: String,
}

/// A fleet need's session, for its submenu's acts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerRef {
    /// The name the book knows the Mac by: what `survey --host` takes (`fleet_glance::dial_of`).
    pub dial: String,
    /// How rows name the Mac (`fleet_glance::short_name`).
    pub name: String,
    /// Where Screen Sharing and ssh go, when the Mac named a host.
    pub host: Option<String>,
    /// The session there.
    pub sid: String,
    /// That Mac lets this one drive it (its survey said `drive=1`: its person ran `aterm fleet allow drive --from <this Mac>` or `--all` there): Take Over and Hand Back on it, over the fleet, where its fleet access admits them ([`Self::writes`]).
    pub drive: bool,
    /// That Mac's fleet access, as its survey said (`fleet_glance::FleetMac::access`); `None` where it was not read.
    pub access: Option<FleetAccess>,
}

impl PeerRef {
    /// Whether this Mac may act on that Mac's sessions (Take Over, Hand Back): it is a driver
    /// there, and that Mac's fleet access is Read and Write or higher. A level that was not
    /// read holds nothing back: a Mac that says none enforces none.
    #[cfg(any(target_os = "macos", test))]
    fn writes(&self) -> bool {
        self.drive
            && self
                .access
                .is_none_or(|level| level >= FleetAccess::ReadWrite)
    }
}

/// The Fleet row's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum FleetLine {
    /// No fleet glance runs (a headless instance, or before it starts).
    #[default]
    Off,
    /// This Mac is in no fleet.
    NotInFleet,
    /// In a fleet.
    InFleet {
        /// The last good read: how many Macs answered of how many asked, how many agents they run (where their rows count them), and when it was read (unix ms).
        read: Option<(usize, usize, Option<usize>, u64)>,
        /// A read is in flight.
        reading: bool,
        /// Why the last read failed, when it did.
        failed: Option<String>,
        /// The issuer a member's book names: what it alone reaches.
        member_of: Option<String>,
        /// The issuer a member's book binds no key to: no Mac's key is proven here until this Mac joins again (`fleet_glance::BookFacts::unbound_issuer`).
        unbound_issuer: Option<String>,
    },
}

/// A sibling session to raise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SiblingRef {
    /// The sibling's pid (the fallback raise).
    pub pid: u32,
    /// Its control socket.
    pub sock: String,
    /// The session there.
    pub sid: String,
}

/// The glance: everything the status item shows, as pure data ([`Glance::build`]), painted by [`compose_status_menu`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Glance {
    /// The generation the App drew it as ([`Pick::generation`]); `0` until handed over.
    pub generation: u16,
    /// Needs you: stuck before decide, this aterm's before a sibling's, oldest first.
    pub needs: Vec<NeedRow>,
    /// This Mac.
    pub here: Here,
    /// The managers, this aterm's in id order, then the workers of sids not here.
    pub managers: Vec<ManagerRow>,
    /// Done for you, newest first: what ended, finished or failed, with nobody here waiting on it.
    pub done: Vec<DoneRow>,
    /// Waiting, one row per cause.
    pub waiting: Vec<WaitRow>,
    /// The held tabs.
    pub held: Vec<HeldRow>,
    /// The sessions on other Macs this Mac holds over the fleet.
    pub held_there: Vec<HeldThereRow>,
    /// The digest's entries — what needs a look, not now — `(log id, subject)` per entry, worst first.
    pub exceptions: Vec<(u64, String)>,
    /// The sessions a [`StatusAction::Go`] goes to.
    pub go: Vec<u64>,
    /// The results a [`StatusAction::ReadResult`] reads.
    pub reads: Vec<ResultRef>,
    /// The sibling sessions a [`StatusAction::RaiseSibling`] raises.
    pub raises: Vec<SiblingRef>,
    /// The fleet sessions a fleet need row's acts name.
    pub peers: Vec<PeerRef>,
    /// The Fleet row.
    pub fleet: FleetLine,
    /// The Network row above it: where this Mac is on the network, in the place's words
    /// (`crate::net_place::menu_line`); `None` until a place was read.
    pub network: Option<crate::net_place::MenuLine>,
    /// System State's one quiet signal (`docs/DESIGN-system-state-2026-10-21.md`
    /// §3.6): a row it manages needs a person or failed.
    pub system_state: Option<SystemStateLine>,
    /// This machine's title (`m8 Heron`, [`crate::machine_title::machine_menu_bar_title`]), which
    /// leads the tooltip and This Mac's row (`docs/DESIGN-machine-names-2026-11-04.md` §5.5);
    /// `None` until the title's reader has read it, and off macOS.
    pub machine: Option<String>,
    /// This Mac's fleet access in force, as its state file last read ([`LockFace::on_disk`]):
    /// the Fleet Access submenu's checkmark.
    pub access: FleetAccess,
    /// The level the lock shows ([`LockFace::shown`]): a raise that has not taken effect yet,
    /// else [`Self::access`].
    pub access_shown: FleetAccess,
    /// The level the title names while a click is fresh ([`LockFace::flash`]).
    pub access_flash: Option<FleetAccess>,
}

/// The menu-bar button's face: its title, and the lock beside it where one shows.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) struct ButtonFace {
    /// The title: the caret, and the level a fresh click chose.
    pub title: String,
    /// The lock's level; `None` out of a fleet, where the face is the caret alone.
    pub lock: Option<FleetAccess>,
}

/// The status menu's System State line: `System State: Needs you: <row>` (or
/// `<row> failed`, `N need you`, `N failed`), which opens the tab, and a waiting
/// vault approval code under it (`system_state_access::menu_line`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SystemStateLine {
    /// The line's words.
    pub label: String,
    /// A note under it: the vault approval code a join waits on, or that an update needs Full Disk
    /// Access re-allowed.
    pub note: Option<String>,
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
#[must_use]
fn report_tip(need: &NeedRow) -> Option<String> {
    let ask = need.harness.as_ref()?;
    Some(aterm_agent::supervise::mode_words::report_words(
        ask.program.product(),
        ask.tail,
        need.alone,
    ))
}

/// Sort diagnostics by severity, stalled work, origin and age.
fn need_order(n: &NeedRow) -> (bool, Severity, bool, Origin, i64, &str) {
    (
        !n.counted,
        n.word.severity(),
        n.carries_on,
        n.origin,
        n.since_unix.unwrap_or(i64::MAX),
        &n.label,
    )
}

fn slot<T>(table: &mut Vec<T>, item: T) -> Option<u16> {
    let at = u16::try_from(table.len()).ok()?;
    table.push(item);
    Some(at)
}

impl Glance {
    /// The glance of `rows` (this aterm's live sessions, in id order), `siblings` (the last sibling scan) and `fleet` (the fleet glance's slot, `None` where none runs), at `now_ms` (unix milliseconds: a read's freshness is judged to the millisecond its reader wakes at); `api_since` is when the first tab lost the API (`App::api_outage`).
    pub(crate) fn build(
        rows: &[SessionRow],
        siblings: &[SiblingInstance],
        fleet: Option<&crate::fleet_glance::FleetSlot>,
        now_ms: u64,
        api_since: Option<i64>,
    ) -> Self {
        let mut g = Self::default();
        let label_of = |sid: &str| rows.iter().find(|r| r.sid == sid).map(SessionRow::named);
        let mut go_of: std::collections::HashMap<u64, u16> = std::collections::HashMap::new();
        let mut go = |g: &mut Self, session: u64| -> Option<u16> {
            if let Some(at) = go_of.get(&session) {
                return Some(*at);
            }
            let at = slot(&mut g.go, session)?;
            go_of.insert(session, at);
            Some(at)
        };
        // A machine's need (Grok Build signed out) stands in every session it reaches, and is
        // listed once, on the first.
        let mut machine: Vec<&str> = Vec::new();
        for row in rows {
            let Some(need) = &row.need else { continue };
            if crate::needs::machine_wide(need) {
                if machine.contains(&need.what.as_str()) {
                    continue;
                }
                machine.push(need.what.as_str());
            }
            let Some(at) = go(&mut g, row.id) else {
                continue;
            };
            let worker_of = row
                .task
                .as_ref()
                .filter(|t| t.from != crate::task_record::FROM_CTL)
                .and_then(|t| label_of(&t.from));
            g.needs.push(NeedRow {
                word: need.word,
                origin: Origin::Local,
                label: row.named(),
                worker_of,
                what: need.what.clone(),
                harness: need.harness.clone(),
                carries_on: need.harness.is_some(),
                alone: need.harness.is_some() && row.need_alone,
                since_unix: row.need_since,
                at: NeedAt::Go(at),
                counted: true,
                read_at_ms: None,
            });
        }
        let sibling_fresh = u64::try_from(SIBLING_FRESH.as_millis()).unwrap_or(u64::MAX);
        for sibling in siblings {
            let fresh = now_ms.saturating_sub(sibling.scanned_ms) <= sibling_fresh;
            let scanned = i64::try_from(sibling.scanned_ms / 1000).unwrap_or(i64::MAX);
            for need in &sibling.needs {
                let Some(at) = slot(
                    &mut g.raises,
                    SiblingRef {
                        pid: sibling.pid,
                        sock: sibling.sock.clone(),
                        sid: need.sid.clone(),
                    },
                ) else {
                    continue;
                };
                g.needs.push(NeedRow {
                    word: need.word,
                    origin: Origin::Sibling(sibling.pid),
                    label: need.label.clone(),
                    worker_of: None,
                    what: need
                        .claim
                        .unwrap_or_else(|| need_words(need.word))
                        .to_string(),
                    harness: None,
                    carries_on: need.carries_on,
                    alone: false,
                    since_unix: need.age_ms.map(|ms| {
                        scanned.saturating_sub(i64::try_from(ms / 1000).unwrap_or(i64::MAX))
                    }),
                    at: NeedAt::Raise(at),
                    counted: fresh,
                    read_at_ms: (!fresh).then_some(sibling.scanned_ms),
                });
            }
        }
        g.fleet = fleet.map_or(FleetLine::Off, |slot| g.fleet_needs(slot, now_ms));
        g.needs.sort_by(|a, b| need_order(a).cmp(&need_order(b)));
        for row in rows {
            let landed = |t: &TaskRow| {
                t.state == TaskState::Done && t.why == Some(crate::task_record::WHY_LANDED)
            };
            match (&row.task, row.glance_word()) {
                _ if row.landed => g.here.landed += 1,
                (Some(t), _) if landed(t) => g.here.landed += 1,
                (Some(t), _) if t.state == TaskState::Done => g.here.done += 1,
                (Some(t), _) if t.state == TaskState::Failed => g.here.failed += 1,
                (Some(t), _) if t.state == TaskState::Exited => g.here.exited += 1,
                _ if row.need.is_some() => g.here.need_you += 1,
                _ if row.waiting.is_some() => g.here.waiting += 1,
                _ if row.landing => g.here.landing += 1,
                (_, "" | "-") => g.here.shells += 1,
                (_, "idle") => g.here.resting += 1,
                _ => g.here.working += 1,
            }
        }
        g.here.elsewhere = siblings.iter().map(|s| s.sessions).sum();
        let mut managers: Vec<(String, ManagerRow)> = Vec::new();
        for row in rows {
            let Some(task) = row
                .task
                .as_ref()
                .filter(|t| t.from != crate::task_record::FROM_CTL)
            else {
                continue;
            };
            let Some(worker_go) = go(&mut g, row.id) else {
                continue;
            };
            let read = (task.state.has_an_end_to_read() && task.result_bytes > 0)
                .then(|| task.result_path.clone())
                .flatten()
                .and_then(|path| {
                    slot(
                        &mut g.reads,
                        ResultRef {
                            session: row.id,
                            entered: task.entered,
                            end_serial: row.end.filter(|end| !end.seen).map(|end| end.serial),
                            path,
                            label: row.named(),
                        },
                    )
                });
            let landed_task = row.landed
                && task.state == TaskState::Done
                && task.why == Some(crate::task_record::WHY_LANDED);
            let worker = WorkerRow {
                label: row.named(),
                state: task.state,
                why: task.why,
                finished: task.finished,
                since_unix_ms: task.since_unix_ms,
                result_bytes: task.result_bytes,
                go: worker_go,
                read,
                landing: row.landing,
                tip: row.landing_tip.clone().or_else(|| {
                    landed_task.then(|| {
                        aterm_agent::supervise::mode_words::checked_words(row.receipt.as_deref())
                    })
                }),
            };
            match managers.iter_mut().find(|(sid, _)| *sid == task.from) {
                Some((_, manager)) => manager.workers.push(worker),
                None => {
                    let here = rows.iter().find(|r| r.sid == task.from);
                    let manager = ManagerRow {
                        label: here.map_or_else(
                            || {
                                format!(
                                    "workers of {} (not here)",
                                    crate::presence::short_sid(&task.from)
                                )
                            },
                            SessionRow::named,
                        ),
                        go: here.and_then(|m| go(&mut g, m.id)),
                        workers: vec![worker],
                    };
                    managers.push((task.from.clone(), manager));
                }
            }
        }
        managers.sort_by_key(|(sid, m)| {
            (
                m.go.is_none(),
                rows.iter()
                    .find(|r| r.sid == *sid)
                    .map_or(u64::MAX, |r| r.id),
                sid.clone(),
            )
        });
        g.managers = managers.into_iter().map(|(_, m)| m).collect();
        let mut done: Vec<DoneRow> = Vec::new();
        let mut listed: Vec<u64> = Vec::new();
        for row in rows {
            let Some(task) = row.task.as_ref().filter(|t| {
                t.ended_quietly()
                    && !t.seen
                    && (t.from == crate::task_record::FROM_CTL
                        || !rows.iter().any(|r| r.sid == t.from))
            }) else {
                continue;
            };
            let read = (task.result_bytes > 0)
                .then(|| task.result_path.clone())
                .flatten()
                .and_then(|path| {
                    slot(
                        &mut g.reads,
                        ResultRef {
                            session: row.id,
                            entered: task.entered,
                            end_serial: row.end.filter(|end| !end.seen).map(|end| end.serial),
                            path,
                            label: row.named(),
                        },
                    )
                });
            let at = match read {
                Some(read) => DoneAt::Read(read),
                None => match go(&mut g, row.id) {
                    Some(at) => DoneAt::Go(at),
                    None => continue,
                },
            };
            let failed = (task.conclusion() == Some(crate::task_record::Conclusion::Failed))
                .then(|| crate::task_record::failed_words(task.state, task.why));
            let landed = failed.is_none()
                && (task.why == Some(crate::task_record::WHY_LANDED)
                    || row.end.is_some()
                    || row.landed);
            done.push(DoneRow {
                label: row.named(),
                since_unix_ms: task.since_unix_ms,
                result_bytes: task.result_bytes,
                at,
                landed,
                tip: (landed && row.landed).then(|| {
                    aterm_agent::supervise::mode_words::landed_tip(row.receipt.as_deref())
                }),
                failed,
            });
            listed.push(row.id);
        }
        for row in rows {
            let Some(end) = row.end.filter(|end| !end.seen) else {
                continue;
            };
            let waited_on = row.task.as_ref().is_some_and(|t| {
                t.from != crate::task_record::FROM_CTL && rows.iter().any(|r| r.sid == t.from)
            });
            // A worker with its own report of its end has only that one, a landing too.
            if waited_on || row.task.as_ref().is_some_and(|t| t.own_report) {
                continue;
            }
            if listed.contains(&row.id) {
                continue;
            }
            let Some(at) = go(&mut g, row.id) else {
                continue;
            };
            done.push(DoneRow {
                label: row.named(),
                since_unix_ms: end.since_unix_ms,
                result_bytes: 0,
                at: DoneAt::Go(at),
                landed: true,
                tip: row.landed.then(|| {
                    aterm_agent::supervise::mode_words::landed_tip(row.receipt.as_deref())
                }),
                failed: None,
            });
        }
        done.sort_by_key(|d| std::cmp::Reverse(d.since_unix_ms));
        g.done = done;
        for row in rows {
            let Some(wait) = &row.waiting else { continue };
            let Some(at) = go(&mut g, row.id) else {
                continue;
            };
            let unsupervised = row.unsupervised;
            let cause = g.waiting.iter_mut().find(|w| {
                (w.word, w.wall, w.unsupervised, &w.entry)
                    == (wait.word, wait.wall, unsupervised, &wait.entry)
            });
            match cause {
                Some(cause) => {
                    cause.tabs.push((row.named(), at));
                    if cause.reset.is_none() {
                        cause.reset.clone_from(&wait.reset);
                    }
                }
                None => g.waiting.push(WaitRow {
                    word: wait.word,
                    wall: wait.wall,
                    unsupervised,
                    reset: wait.reset.clone(),
                    since_unix: (wait.word == WaitWord::Api).then_some(api_since).flatten(),
                    entry: wait.entry.clone(),
                    tabs: vec![(row.named(), at)],
                }),
            }
        }
        for row in rows {
            let Some(hold) = row.held else { continue };
            let Some(at) = go(&mut g, row.id) else {
                continue;
            };
            let worker_of = row
                .task
                .as_ref()
                .filter(|t| t.from != crate::task_record::FROM_CTL)
                .and_then(|t| label_of(&t.from));
            g.held.push(HeldRow {
                label: row.named(),
                worker_of,
                hold,
                by_host: row.held_by_host.clone(),
                refused: if hold == HoldWord::Person {
                    row.person_refusals
                } else {
                    0
                },
                since_unix: row.held_since,
                go: at,
            });
        }
        g
    }

    fn fleet_needs(&mut self, fleet: &crate::fleet_glance::FleetSlot, now_ms: u64) -> FleetLine {
        if !fleet.book.in_fleet {
            return FleetLine::NotInFleet;
        }
        let read = fleet.snapshot.as_ref().map(|snap| {
            for mac in snap.macs.iter().filter(|m| m.counts()) {
                let fresh = mac.fresh(now_ms);
                for need in &mac.needs {
                    let Some(at) = slot(
                        &mut self.peers,
                        PeerRef {
                            dial: mac.dial.clone(),
                            name: mac.name.clone(),
                            host: mac.host.clone(),
                            sid: need.sid.clone(),
                            drive: mac.drive,
                            access: mac.access,
                        },
                    ) else {
                        continue;
                    };
                    let worker = need
                        .worker_of
                        .as_deref()
                        .map_or_else(String::new, |m| format!(" worker of {m}"));
                    let read_at = i64::try_from(mac.read_at_ms / 1000).unwrap_or(0);
                    self.needs.push(NeedRow {
                        word: need.word,
                        origin: Origin::Fleet,
                        label: format!("{} \u{b7} {}{worker}", mac.name, need.who),
                        worker_of: None,
                        what: need
                            .what
                            .clone()
                            .unwrap_or_else(|| need_words(need.word).to_string()),
                        harness: None,
                        carries_on: need.carries_on,
                        alone: false,
                        since_unix: need.age_ms.map(|ms| {
                            read_at.saturating_sub(i64::try_from(ms / 1000).unwrap_or(i64::MAX))
                        }),
                        at: NeedAt::Peer(at),
                        counted: fresh,
                        read_at_ms: Some(mac.read_at_ms),
                    });
                }
                for held in &mac.held_here {
                    let Some(at) = slot(
                        &mut self.peers,
                        PeerRef {
                            dial: mac.dial.clone(),
                            name: mac.name.clone(),
                            host: mac.host.clone(),
                            sid: held.sid.clone(),
                            drive: mac.drive,
                            access: mac.access,
                        },
                    ) else {
                        continue;
                    };
                    self.held_there.push(HeldThereRow {
                        label: format!("{} \u{b7} {}", mac.name, held.who),
                        at,
                    });
                }
            }
            (
                snap.answered(),
                snap.answered() + snap.unanswered.len(),
                snap.agents(),
                snap.read_at_ms,
            )
        });
        FleetLine::InFleet {
            read,
            reading: fleet.reading,
            failed: fleet.error.clone(),
            member_of: fleet.book.issuer_label.clone(),
            unbound_issuer: fleet.book.unbound_issuer.clone(),
        }
    }

    /// How many needs the badge counts: this Mac's, and the fleet's from a fresh read.
    pub(crate) fn need_count(&self) -> usize {
        self.needs.iter().filter(|n| n.counted).count()
    }

    /// Whether any recorded need is stuck.
    #[cfg(test)]
    pub(crate) fn stuck(&self) -> bool {
        self.stuck_count() > 0
    }

    /// How many counted needs are stuck.
    pub(crate) fn stuck_count(&self) -> usize {
        self.needs
            .iter()
            .filter(|n| n.counted && n.word.severity() == Severity::Stuck)
            .count()
    }

    /// Whether this Mac is in a fleet.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) const fn in_fleet(&self) -> bool {
        matches!(self.fleet, FleetLine::InFleet { .. })
    }

    /// The menu-bar button's title is a stable entry to diagnostics: the caret, whatever the
    /// sessions do, never a count or a mark that asks the person to act. The lock beside it
    /// ([`Self::button_face`]) is a state its person set, not an alert.
    pub(crate) fn button_title(&self) -> String {
        "\u{276f}".into()
    }

    /// The button's face: the caret, in a fleet the lock at the level it shows
    /// ([`Self::access_shown`]), and while a click is fresh that level's short name after the
    /// caret (`❯ Read and Write`). Out of a fleet the level means nothing and no lock shows.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn button_face(&self) -> ButtonFace {
        let caret = self.button_title();
        if !self.in_fleet() {
            return ButtonFace {
                title: caret,
                lock: None,
            };
        }
        ButtonFace {
            title: match self.access_flash {
                Some(level) => format!("{caret} {}", level.short()),
                None => caret,
            },
            lock: Some(self.access_shown),
        }
    }

    /// The button's hover: this Mac, and in a fleet its fleet access and what each click does.
    /// The button opens diagnostics on demand; it never asks the person to intervene.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn tooltip(&self) -> String {
        let name = match &self.machine {
            Some(machine) => format!("aterm \u{b7} {machine}"),
            None => "aterm".into(),
        };
        if !self.in_fleet() {
            return name;
        }
        let pending = if self.access_shown == self.access {
            String::new()
        } else {
            format!(", becoming {}", self.access_shown.short())
        };
        format!(
            "{name} \u{2014} fleet access: {}{pending}. Click to change it; right-click for the \
             menu.",
            self.access.short()
        )
    }

    /// What a screen reader says the button is: aterm, and in a fleet its fleet access.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn accessibility_label(&self) -> String {
        if self.in_fleet() {
            format!("aterm, fleet access {}", self.access_shown.short())
        } else {
            "aterm".into()
        }
    }

    /// The local sessions needing the owner, in the order the menu lists them: what a test reads off a real App.
    #[cfg(test)]
    pub(crate) fn local_need_sessions(&self) -> Vec<u64> {
        self.needs
            .iter()
            .filter_map(|n| match n.at {
                NeedAt::Go(at) => self.go.get(usize::from(at)).copied(),
                NeedAt::Raise(_) | NeedAt::Peer(_) => None,
            })
            .collect()
    }

    /// A cheap change fingerprint: the App hands the status item a new glance only when this differs from the last one handed over.
    pub(crate) fn fingerprint(&self) -> u64 {
        let mut plain = self.clone();
        plain.generation = 0;
        for need in &mut plain.needs {
            need.since_unix = None;
        }
        for manager in &mut plain.managers {
            for worker in &mut manager.workers {
                worker.since_unix_ms = 0;
            }
        }
        for done in &mut plain.done {
            done.since_unix_ms = 0;
        }
        for wait in &mut plain.waiting {
            wait.since_unix = None;
        }
        for held in &mut plain.held {
            held.since_unix = None;
        }
        aterm_hash::fnv1a_64(format!("{plain:?}").as_bytes())
    }
}

const fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A sibling's or a fleet peer's row names its session by what that row carries: its typed role, else the title its person gave it, else its OSC title without the status glyph an agent leads it with ([`crate::app_take_over::title_without_agent_glyph`], the one strip of a spinner frame or Claude Code's idle `✳`, so the animation moves neither a row nor [`Glance::fingerprint`]); `None` when all three are empty, and the row falls back to the peer's program (`fleet_glance::mac_of`).
pub(crate) fn tab_name(
    role: Option<&str>,
    user_title: Option<&str>,
    osc_title: &str,
) -> Option<String> {
    let osc = crate::app_take_over::title_without_agent_glyph(osc_title);
    [role, user_title, Some(osc)]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(|s| fold_clip(s, 40))
}

/// A need word in a few words, where a row has no words of its own (a sibling's or a fleet peer's facts row carries the word, never its text).
pub(crate) const fn need_words(word: NeedWord) -> &'static str {
    match word {
        NeedWord::Frozen => "frozen: not reading input",
        NeedWord::Stopped => "stopped with input queued",
        NeedWord::Stalled => "stalled",
        NeedWord::Login => "logged out",
        NeedWord::Memory => "memory critical",
        NeedWord::Wall => "stopped at a wall",
        NeedWord::Ask => "asks for you",
        NeedWord::Approval => "approval waiting",
        NeedWord::Question => "question waiting",
    }
}

/// The mark a need row leads with: `◆` stuck, `◇` decide.
pub(crate) const fn need_mark(word: NeedWord) -> char {
    match word.severity() {
        Severity::Stuck => '\u{25c6}',
        Severity::Decide => '\u{25c7}',
    }
}

/// A need's words after its tab's label: `<label> (worker of m) — <what>`.
pub(crate) fn need_label(label: &str, worker_of: Option<&str>, what: &str) -> String {
    let worker = worker_of.map_or_else(String::new, |m| format!(" (worker of {m})"));
    format!("{label}{worker} \u{2014} {what}")
}

/// A span in a person's words: `12s`, `3m`, `2h`, `4d`.
pub(crate) fn age_words(secs: i64) -> String {
    let secs = secs.max(0);
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A result's size: `512 B`, `2.1 KB`, `3.4 MB`.
pub(crate) fn bytes_words(bytes: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "a size printed to one decimal")]
    let tenth = |unit: u64| (bytes as f64) / (unit as f64);
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KB", tenth(1024)),
        _ => format!("{:.1} MB", tenth(1_048_576)),
    }
}

/// `HH:MM` of `unix` on this Mac's clock, where its zone is known.
pub(crate) fn clock_words(unix: i64) -> Option<String> {
    clock_words_local(unix, &crate::presence::local_offset_at)
}

pub(crate) fn clock_words_local(
    unix: i64,
    offset_at: &dyn Fn(i64) -> Option<i64>,
) -> Option<String> {
    let offset = offset_at(unix)?;
    let local = (unix + offset).rem_euclid(86_400);
    Some(format!("{:02}:{:02}", local / 3600, (local % 3600) / 60))
}

fn age_suffix(since: Option<i64>, now_unix: i64) -> String {
    since.map_or_else(String::new, |at| {
        format!(" \u{b7} {}", age_words(now_unix - at))
    })
}

fn ms_age(since_unix_ms: u64, now_unix: i64) -> i64 {
    now_unix.saturating_sub(i64::try_from(since_unix_ms / 1000).unwrap_or(i64::MAX))
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn need_line(row: &NeedRow, now_unix: i64, shown: usize) -> String {
    let mut end = age_suffix(row.since_unix, now_unix);
    if let Origin::Sibling(pid) = row.origin {
        end.push_str(&format!(" \u{b7} other aterm (pid {pid})"));
    }
    if let Some(read) = row.read_at_ms {
        end.push_str(&format!(
            " (read {} ago)",
            age_words(ms_age(read, now_unix))
        ));
    }
    if let Some(ask) = &row.harness {
        return harness_need_line(row, ask, shown, &end);
    }
    let line = format!(
        "{} {}{end}",
        need_mark(row.word),
        need_label(&row.label, row.worker_of.as_deref(), &row.what),
    );
    fold_clip(&line, LINE_MAX)
}

#[cfg(any(target_os = "macos", test))]
fn further_need_rows(need: &NeedRow, left: &mut usize, go: StatusAction) -> Vec<StatusRow> {
    let Some(ask) = need.harness.as_ref() else {
        return Vec::new();
    };
    let drawn = ask.others.len().min(NEED_MORE_ROWS).min(*left);
    *left -= drawn;
    let past = ask.more_past(drawn);
    let product = ask.program.product();
    let label = need.label.trim();
    let tip = fold_clip(
        &if label.is_empty() || label.eq_ignore_ascii_case(product) {
            format!("{product} needs this too")
        } else {
            format!("{product} in {label} needs this too")
        },
        LINE_MAX,
    );
    ask.others
        .iter()
        .take(drawn)
        .enumerate()
        .map(|(i, other)| {
            let tail = if i + 1 == drawn { past.as_str() } else { "" };
            StatusRow::Action {
                label: format!("{}{tail}", fold_clip(other, LINE_MAX - tail.len())),
                action: go,
                enabled: true,
                checked: false,
                tip: Some(tip.clone()),
                indent: 1,
            }
        })
        .collect()
}

const NEED_ROOM_MIN: usize = 24;

fn harness_need_line(
    row: &NeedRow,
    ask: &crate::needs::HarnessAsk,
    shown: usize,
    end: &str,
) -> String {
    const SEP: &str = " \u{2014} ";
    let more = if shown == 0 {
        ask.more_words()
    } else {
        String::new()
    };
    let end = format!("{more}{end}");
    let worker = row
        .worker_of
        .as_deref()
        .map_or_else(String::new, |m| format!(" (worker of {m})"));
    let lead = fold_clip(
        &format!("{} {}{worker}", need_mark(row.word), row.label),
        LINE_MAX.saturating_sub(end.len() + SEP.len() + NEED_ROOM_MIN),
    );
    let room = LINE_MAX.saturating_sub(lead.len() + SEP.len() + end.len());
    format!("{lead}{SEP}{}{end}", fold_clip(&ask.first, room))
}

/// This Mac's row: this machine's title (`m8 Heron`, `machine`), else `This Mac`, then ` — ` and
/// each nonzero count of `here` in its order, `·` between.
#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
pub(crate) fn here_line(here: &Here, machine: Option<&str>) -> String {
    let lead = machine.unwrap_or("This Mac");
    let parts: Vec<String> = [
        (here.working, "working"),
        (here.need_you, "need you"),
        (here.waiting, "waiting"),
        (here.landing, "landing"),
        (here.resting, "resting"),
        (here.landed, "landed"),
        (here.done, "done"),
        (here.failed, "failed"),
        (here.exited, "exited"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, word)| format!("{n} {word}"))
    .chain((here.shells > 0).then(|| format!("{} shell{}", here.shells, plural(here.shells))))
    .collect();
    let mut line = if parts.is_empty() {
        format!("{lead} \u{2014} no sessions")
    } else {
        format!("{lead} \u{2014} {}", parts.join(" \u{b7} "))
    };
    if here.elsewhere > 0 {
        line.push_str(&format!(" (+{} in another aterm)", here.elsewhere));
    }
    line
}

/// A Land desk at rest like the forever lab's (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.6): six Land sessions that hold, one more holding with a need, one Off session at rest, five landed and three shells. `objc_toolbar_drive` puts it on a real status item and reads This Mac back off AppKit; the unit tests read the same rows.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn a_land_desk_at_rest() -> Vec<SessionRow> {
    let idle = |id: u64, title: &str| SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: title.to_string(),
        agent: "idle",
        ..SessionRow::default()
    };
    let held = |id: u64, title: &str| SessionRow {
        landing: true,
        landing_tip: Some("not landed yet: 1 changed file \u{b7} 2 nudges".to_string()),
        ..idle(id, title)
    };
    let ask = crate::needs::NeedFacts {
        attention: vec![crate::needs::AttentionFact {
            owner: "-".to_string(),
            text: "rotate the deploy key".to_string(),
            kind: Some(aterm_types::needs::AttentionKind::Ask),
        }],
        ..crate::needs::NeedFacts::default()
    };
    let mut rows: Vec<SessionRow> = ["land-b", "land-c", "land-f", "land-g", "land-h", "land-i"]
        .into_iter()
        .zip(1..)
        .map(|(title, id)| held(id, title))
        .collect();
    rows.push(SessionRow {
        need: crate::needs::of(&ask).need,
        ..held(7, "land-d")
    });
    rows.push(idle(8, "off"));
    rows.extend(
        ["land-a", "land-e", "instant-land", "land-j", "land-k"]
            .into_iter()
            .zip(9..)
            .map(|(title, id)| SessionRow {
                landed: true,
                ..idle(id, title)
            }),
    );
    rows.extend((14..17).map(|id| SessionRow {
        agent: "-",
        ..idle(id, "zsh")
    }));
    rows
}

/// A LANDING'S RECEIPT ON THE BAR (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.9, slice
/// 9c), at `now_ms`: `proj-api`, a worker started from `ctl` that landed three minutes ago with
/// `3f9a1c2e4b7d` on `origin/main` (its Done for you row), and `parser`, a worker of `lead` that
/// landed a minute ago in a repository with no remote (its row under its manager). Its receipts
/// are [`landing_receipts`]'. `objc_toolbar_drive` puts them on a real status item and reads both
/// rows' tips back off AppKit; the unit tests read the same rows.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn a_landing_with_its_receipt(now_ms: u64) -> Vec<SessionRow> {
    let (on_main, no_remote) = landing_receipts();
    let landed = |from: &str, ago_ms: u64| TaskRow {
        state: TaskState::Done,
        why: Some(crate::task_record::WHY_LANDED),
        from: from.to_string(),
        entered: 3,
        since_unix_ms: now_ms.saturating_sub(ago_ms),
        result_bytes: 2100,
        result_path: None,
        seen: false,
        finished: false,
        own_report: false,
    };
    let row = |id: u64, title: &str| SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: title.to_string(),
        agent: "idle",
        ..SessionRow::default()
    };
    let lead = row(1, "lead");
    let lead_sid = lead.sid.clone();
    vec![
        lead,
        SessionRow {
            task: Some(landed(crate::task_record::FROM_CTL, 180_000)),
            landed: true,
            receipt: Some(std::sync::Arc::new(on_main)),
            ..row(2, "proj-api")
        },
        SessionRow {
            task: Some(landed(&lead_sid, 60_000)),
            landed: true,
            receipt: Some(std::sync::Arc::new(no_remote)),
            ..row(3, "parser")
        },
    ]
}

/// [`a_landing_with_its_receipt`]'s two receipts: `3f9a1c2e4b7d` on `origin/main` in
/// `/Users//me/src/proj-api` on `main`, and `0c4d2e9f1a7b` committed in `/Users//me/src/parser`,
/// which has no remote.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn landing_receipts() -> (
    aterm_agent::harness::acts::LandReceipt,
    aterm_agent::harness::acts::LandReceipt,
) {
    use aterm_agent::harness::acts::{LandReceipt, ReceiptLeg};
    (
        LandReceipt {
            head: "3f9a1c2e4b7d".to_string(),
            main_ref: "origin/main".to_string(),
            repo: "/Users//me/src/proj-api".to_string(),
            branch: "main".to_string(),
            leg: ReceiptLeg::OnMain,
            check: false,
            said: "Nothing left: the parser is committed and pushed to origin/main.".to_string(),
            at_ms: 0,
            statement: aterm_agent::harness::land_receipt::Statement::Unread,
        },
        LandReceipt {
            head: "0c4d2e9f1a7b".to_string(),
            repo: "/Users//me/src/parser".to_string(),
            leg: ReceiptLeg::NoRemote,
            said: "Nothing left: committed here; this folder has no remote.".to_string(),
            ..LandReceipt::default()
        },
    )
}

/// A box beside an older report (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7), at `now_unix`: tab 3's three-hour-old `NEEDS YOU:` report of Claude Code's (`proj-api`), which its agent carries on past, and tab 5's two-minute-old approval box (`build`), which has stopped its agent. Both are decide, both `◇`, and the box lists first. `objc_toolbar_drive` puts them on a real status item and reads the rows and their tips back off AppKit; the unit tests read the same rows.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn a_box_beside_an_older_report(now_unix: i64) -> Vec<SessionRow> {
    let report = crate::needs::NeedFacts {
        attention: vec![crate::needs::AttentionFact {
            owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
            text: "claude needs you: the release credential to sign 0.48.0 (+1 more) \u{2014} carrying on"
                .to_string(),
            kind: Some(aterm_types::needs::AttentionKind::Ask),
        }],
        ..crate::needs::NeedFacts::default()
    };
    let approval = crate::needs::NeedFacts {
        agent: Some(crate::needs::AgentFact {
            word: "prompt",
            detail: Some("bash:not-read-only".to_string()),
            rev: 1,
            subject: Some("rm -rf build".to_string()),
        }),
        ..crate::needs::NeedFacts::default()
    };
    [
        (3, "proj-api", report, now_unix - 3 * 3600),
        (5, "build", approval, now_unix - 120),
    ]
    .into_iter()
    .map(|(id, title, facts, since)| SessionRow {
        id,
        sid: format!("s-{id:020}"),
        title: title.to_string(),
        need: crate::needs::of(&facts).need,
        need_since: Some(since),
        ..SessionRow::default()
    })
    .collect()
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn manager_line(m: &ManagerRow) -> String {
    let count = |word| m.workers.iter().filter(|w| w.word() == word).count();
    let states: Vec<String> = WORKER_WORDS
        .into_iter()
        .map(|word| (count(word), word))
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect();
    let n = m.workers.len();
    fold_clip(
        &format!(
            "{} \u{2014} {n} worker{}: {}",
            m.label,
            plural(n),
            states.join(", ")
        ),
        LINE_MAX,
    )
}

/// The mark of a worker that ended unfinished, in the menu's rows and the tab menu's.
pub(crate) const FAILED_GLYPH: &str = "\u{2715}";

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn worker_line(w: &WorkerRow, now_unix: i64) -> String {
    let age = age_words(ms_age(w.since_unix_ms, now_unix));
    let line = match w.state {
        TaskState::Working => format!("\u{25cf} {} \u{2014} {} {age}", w.label, w.word()),
        TaskState::Stalled => format!(
            "\u{25c6} {} \u{2014} stalled: {} \u{b7} {age}",
            w.label,
            crate::task_record::stall_words(w.why)
        ),
        TaskState::Done if w.result_bytes > 0 => format!(
            "\u{2713} {} \u{2014} {} {age} \u{b7} {}",
            w.label,
            w.word(),
            bytes_words(w.result_bytes)
        ),
        TaskState::Done => format!("\u{2713} {} \u{2014} {} {age}", w.label, w.word()),
        TaskState::Failed => format!(
            "{FAILED_GLYPH} {} \u{2014} failed: {} \u{b7} {age}",
            w.label,
            crate::task_record::failed_words(w.state, w.why)
        ),
        TaskState::Exited if w.finished => {
            let word = if w.why == Some(crate::task_record::WHY_LANDED) {
                "landed"
            } else {
                "done"
            };
            format!("\u{2713} {} \u{2014} {word}, then exited {age}", w.label)
        }
        TaskState::Exited => format!(
            "{FAILED_GLYPH} {} \u{2014} exited: {} \u{b7} {age}",
            w.label,
            crate::task_record::failed_words(w.state, w.why)
        ),
    };
    fold_clip(&line, LINE_MAX)
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn done_line(d: &DoneRow, now_unix: i64) -> String {
    let word = if d.landed { "landed" } else { "done" };
    let glyph = if d.failed.is_some() {
        FAILED_GLYPH
    } else {
        "\u{2713}"
    };
    let word = d
        .failed
        .as_ref()
        .map_or_else(|| word.to_string(), |words| format!("failed: {words}"));
    let age = if d.since_unix_ms == 0 {
        String::new()
    } else {
        format!(" {}", age_words(ms_age(d.since_unix_ms, now_unix)))
    };
    let size = if d.result_bytes > 0 {
        format!(" \u{b7} {}", bytes_words(d.result_bytes))
    } else {
        String::new()
    };
    fold_clip(
        &format!("{glyph} {} \u{2014} {word}{age}{size}", d.label),
        LINE_MAX,
    )
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn wait_line(w: &WaitRow) -> String {
    let n = w.tabs.len();
    let reset = w
        .reset
        .as_deref()
        .map_or_else(String::new, |r| format!(" \u{2014} resets {r}"));
    let one = crate::needs::Waiting {
        word: w.word,
        wall: w.wall,
        reset: None,
        entry: w.entry.clone(),
    };
    let words = crate::needs::wait_words(&one, w.unsupervised);
    let body = match w.word {
        WaitWord::Api => {
            let since = w
                .since_unix
                .and_then(clock_words)
                .map_or_else(String::new, |at| format!(" since {at}"));
            format!("{n} {words}{since}")
        }
        WaitWord::MemoryRestart => {
            format!(
                "{n} memory restart{} queued for the next idle point",
                plural(n)
            )
        }
        WaitWord::Entry => match w.tabs.as_slice() {
            [(label, _)] => format!("{label} \u{2014} {words}"),
            _ => format!("{n} tabs \u{2014} {words}"),
        },
        WaitWord::Limit => format!("{n} {words}{reset}"),
        WaitWord::Overloaded | WaitWord::Login => format!("{n} {words}"),
    };
    fold_clip(&format!("\u{29d7} {body}"), LINE_MAX)
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn held_line(h: &HeldRow, now_unix: i64) -> String {
    let age = h
        .since_unix
        .map_or_else(String::new, |at| format!(" {}", age_words(now_unix - at)));
    let remote;
    let whose = match h.hold {
        HoldWord::Person => "yours",
        HoldWord::Remote => {
            remote = format!(
                "held by {}",
                crate::typed_by::fleet_credit(h.by_host.as_deref().unwrap_or("another Mac"))
            );
            remote.as_str()
        }
        HoldWord::Local => "held by a client",
        HoldWord::Bridge => "held over the message bus",
    };
    let refused = match h.refused {
        0 => String::new(),
        n => format!(
            " \u{b7} refused {n} write{}",
            plural(usize::try_from(n).unwrap_or(2))
        ),
    };
    let label = need_label(
        &h.label,
        h.worker_of.as_deref(),
        &format!("{whose}{age}{refused}"),
    );
    fold_clip(&format!("\u{270b} {label}"), LINE_MAX)
}

/// One row of the rendered status menu — the pure model the AppKit half only paints (the tab context menu's "the description IS the menu" discipline).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum StatusRow {
    /// A disabled information line.
    Info(String),
    /// A separator.
    Separator,
    /// A clickable row carrying a [`StatusAction`].
    Action {
        /// The visible label.
        label: String,
        /// What a click posts.
        action: StatusAction,
        /// Rendered-but-inert when `false`.
        enabled: bool,
        /// Drawn with a checkmark.
        checked: bool,
        /// Its hover, when it has one.
        tip: Option<String>,
        /// How many levels in it is drawn (`NSMenuItem`'s `indentationLevel`): 1 for a harness's further need under its session's row (§12.2.1), with no mark; 0 for every other row.
        indent: u8,
    },
    /// A row that opens a submenu of `rows`, painted with its parent.
    Submenu {
        /// The visible label.
        label: String,
        /// The submenu's rows.
        rows: Vec<StatusRow>,
    },
    /// The Fleet row: a submenu composed at its OWN `menuNeedsUpdate:` from the fleet glance's slot ([`compose_fleet_menu`]), which the reader writes directly, so a read that lands while the menu is open shows the next time the submenu opens.
    FleetSubmenu {
        /// The visible label.
        label: String,
    },
}

#[cfg(any(target_os = "macos", test))]
fn act(label: impl Into<String>, action: StatusAction) -> StatusRow {
    StatusRow::Action {
        label: label.into(),
        action,
        enabled: true,
        checked: false,
        tip: None,
        indent: 0,
    }
}

/// What each level means, as its row's hover says it.
#[cfg(any(target_os = "macos", test))]
const fn access_tip(level: FleetAccess) -> &'static str {
    match level {
        FleetAccess::ReadOnly => {
            "Other Macs in the fleet read this one. None starts a job, sends a message or types \
             into a session here."
        }
        FleetAccess::ReadWrite => {
            "Other Macs also message, type into, take over and close the sessions already here. \
             None starts a new one."
        }
        FleetAccess::Full => "Other Macs also start new jobs and tabs here.",
    }
}

/// The Fleet Access row: a submenu named for the level in force, holding the three levels, the
/// one in force checked, each set by its pick ([`StatusAction::SetFleetAccess`]).
#[cfg(any(target_os = "macos", test))]
fn access_rows(access: FleetAccess) -> StatusRow {
    StatusRow::Submenu {
        label: format!("Fleet Access: {}", access.label()),
        rows: FleetAccess::ALL
            .into_iter()
            .map(|level| StatusRow::Action {
                label: level.label().to_string(),
                action: StatusAction::SetFleetAccess(level),
                enabled: true,
                checked: level == access,
                tip: Some(access_tip(level).to_string()),
                indent: 0,
            })
            .collect(),
    }
}

/// `and k more in Overview`: the rows a section did not show, which the footer's Overview… does.
#[cfg(any(target_os = "macos", test))]
fn more_in_overview(more: usize) -> StatusRow {
    StatusRow::Info(format!("and {more} more in Overview"))
}

/// Compose the whole status menu from a glance, its ages as of `now_unix` — pure, deterministic, and unit-testable off macOS. In order (`docs/DESIGN-fleet-access-lock-2026-11-10.md` §7): Fleet Access (in a fleet), Needs you, System State, This Mac, the managers, Done for you, Waiting, the held tabs, the holds on other Macs, the fleet, To look at, and the footer, which is always Overview… and Read This Mac.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn compose_status_menu(g: &Glance, now_unix: i64) -> Vec<StatusRow> {
    let pick = |index: u16| Pick {
        generation: g.generation,
        index,
    };
    let mut rows = Vec::new();
    if g.in_fleet() {
        rows.push(access_rows(g.access));
        rows.push(StatusRow::Separator);
    }
    if !g.needs.is_empty() {
        let counted = g.need_count();
        let stale = g.needs.len() - counted;
        rows.push(StatusRow::Info(if stale == 0 {
            format!("Needs you ({counted})")
        } else {
            format!("Needs you ({counted}, and {stale} from an old read)")
        }));
        let mut further_left = NEED_MORE_ALL;
        for need in g.needs.iter().take(NEEDS_SHOWN) {
            match need.at {
                NeedAt::Go(at) => {
                    let go = StatusAction::Go(pick(at));
                    let further = further_need_rows(need, &mut further_left, go);
                    rows.push(StatusRow::Action {
                        label: need_line(need, now_unix, further.len()),
                        action: go,
                        enabled: true,
                        checked: false,
                        tip: report_tip(need),
                        indent: 0,
                    });
                    rows.extend(further);
                }
                NeedAt::Raise(at) => rows.push(act(
                    need_line(need, now_unix, 0),
                    StatusAction::RaiseSibling(pick(at)),
                )),
                NeedAt::Peer(at) => {
                    let Some(peer) = g.peers.get(usize::from(at)) else {
                        continue;
                    };
                    rows.push(StatusRow::Submenu {
                        label: need_line(need, now_unix, 0),
                        rows: peer_rows(peer, pick(at)),
                    });
                }
            }
        }
        if g.needs.len() > NEEDS_SHOWN {
            rows.push(more_in_overview(g.needs.len() - NEEDS_SHOWN));
        }
        rows.push(StatusRow::Separator);
    }
    // System State's one quiet signal: a pull surface, so it interrupts nothing.
    if let Some(line) = &g.system_state {
        rows.push(act(line.label.clone(), StatusAction::OpenSystemState));
        if let Some(note) = &line.note {
            rows.push(StatusRow::Info(fold_clip(note, LINE_MAX)));
        }
        rows.push(StatusRow::Separator);
    }
    rows.push(StatusRow::Info(here_line(&g.here, g.machine.as_deref())));
    for manager in g.managers.iter().take(MANAGERS_SHOWN) {
        let mut sub = Vec::new();
        if let Some(at) = manager.go {
            sub.push(act(
                format!("Go to {}", manager.label),
                StatusAction::Go(pick(at)),
            ));
            sub.push(StatusRow::Separator);
        }
        for worker in &manager.workers {
            let mut row = act(
                worker_line(worker, now_unix),
                StatusAction::Go(pick(worker.go)),
            );
            if let StatusRow::Action { tip, .. } = &mut row {
                tip.clone_from(&worker.tip);
            }
            sub.push(row);
        }
        let reads: Vec<StatusRow> = manager
            .workers
            .iter()
            .filter_map(|w| {
                w.read.map(|at| {
                    act(
                        format!("Read {}\u{2019}s Result", w.label),
                        StatusAction::ReadResult(pick(at)),
                    )
                })
            })
            .collect();
        if !reads.is_empty() {
            sub.push(StatusRow::Separator);
            sub.extend(reads);
        }
        rows.push(StatusRow::Submenu {
            label: manager_line(manager),
            rows: sub,
        });
    }
    if g.managers.len() > MANAGERS_SHOWN {
        rows.push(more_in_overview(g.managers.len() - MANAGERS_SHOWN));
    }
    if !g.done.is_empty() {
        rows.push(StatusRow::Separator);
        rows.push(StatusRow::Info(format!("Done for you ({})", g.done.len())));
        for done in g.done.iter().take(DONE_SHOWN) {
            let action = match done.at {
                DoneAt::Read(at) => StatusAction::ReadResult(pick(at)),
                DoneAt::Go(at) => StatusAction::Go(pick(at)),
            };
            let mut row = act(done_line(done, now_unix), action);
            if let StatusRow::Action { tip, .. } = &mut row {
                tip.clone_from(&done.tip);
            }
            rows.push(row);
        }
    }
    if !g.waiting.is_empty() {
        rows.push(StatusRow::Separator);
        rows.push(StatusRow::Info("Waiting".to_string()));
        for wait in &g.waiting {
            rows.push(StatusRow::Submenu {
                label: wait_line(wait),
                rows: wait
                    .tabs
                    .iter()
                    .map(|(label, at)| act(label.clone(), StatusAction::Go(pick(*at))))
                    .collect(),
            });
        }
    }
    if !g.held.is_empty() {
        rows.push(StatusRow::Separator);
        let yours = g.held.iter().any(|h| h.hold == HoldWord::Person);
        let all_yours = g.held.iter().all(|h| h.hold == HoldWord::Person);
        rows.push(StatusRow::Info(
            if all_yours { "You hold" } else { "Held" }.to_string(),
        ));
        // What a hold costs is each held row's hover, and Hand Back All's: prose is no menu row.
        let cost = || Some(aterm_types::control_verbs::HOLD_COST.to_string());
        for held in &g.held {
            rows.push(StatusRow::Action {
                label: held_line(held, now_unix),
                action: StatusAction::Go(pick(held.go)),
                enabled: true,
                checked: false,
                tip: cost(),
                indent: 0,
            });
        }
        if yours {
            rows.push(StatusRow::Action {
                label: "Hand Back All".to_string(),
                action: StatusAction::HandBackAll,
                enabled: true,
                checked: false,
                tip: cost(),
                indent: 0,
            });
        }
    }
    if !g.held_there.is_empty() {
        rows.push(StatusRow::Separator);
        rows.push(StatusRow::Info("Held from here".to_string()));
        for held in &g.held_there {
            let Some(peer) = g.peers.get(usize::from(held.at)) else {
                continue;
            };
            rows.push(StatusRow::Submenu {
                label: fold_clip(&held.label, LINE_MAX),
                rows: held_there_rows(peer, pick(held.at)),
            });
        }
    }
    let network_row = |rows: &mut Vec<StatusRow>| {
        if let Some(network) = &g.network {
            rows.push(StatusRow::Info(fold_clip(&network.label, LINE_MAX)));
        }
    };
    match &g.fleet {
        FleetLine::Off => {}
        FleetLine::NotInFleet => {
            rows.push(StatusRow::Separator);
            network_row(&mut rows);
            rows.push(StatusRow::Info(
                "Fleet \u{2014} not in a fleet (aterm fleet init or join)".to_string(),
            ));
        }
        FleetLine::InFleet {
            read,
            reading,
            failed,
            member_of,
            unbound_issuer,
        } => {
            rows.push(StatusRow::Separator);
            network_row(&mut rows);
            rows.push(StatusRow::FleetSubmenu {
                label: fleet_line(read.as_ref(), *reading, failed.is_some(), now_unix),
            });
            rows.extend(degrade_rows(
                (member_of.as_deref(), unbound_issuer.as_deref()),
                failed.as_deref(),
            ));
        }
    }
    if !g.exceptions.is_empty() {
        rows.push(StatusRow::Separator);
        rows.push(StatusRow::Info(format!(
            "To look at ({})",
            g.exceptions.len()
        )));
        for (id, line) in g.exceptions.iter().take(EXCEPTION_LINES_SHOWN) {
            rows.push(act(line.clone(), StatusAction::ShowException(*id)));
        }
        if g.exceptions.len() > EXCEPTION_LINES_SHOWN {
            rows.push(StatusRow::Info(format!(
                "and {} more",
                g.exceptions.len() - EXCEPTION_LINES_SHOWN
            )));
        }
        rows.push(act(
            match g.exceptions.len() {
                1 => "Dismiss \u{2014} kept in Messages".to_string(),
                n => format!("Dismiss all {n} \u{2014} kept in Messages"),
            },
            StatusAction::DismissExceptions,
        ));
    }
    rows.push(StatusRow::Separator);
    rows.push(act("Overview\u{2026}", StatusAction::Overview));
    rows.push(act("Read This Mac", StatusAction::ReadThisMac));

    rows
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn fleet_line(
    read: Option<&(usize, usize, Option<usize>, u64)>,
    reading: bool,
    failed: bool,
    now_unix: i64,
) -> String {
    let mut line = match read {
        Some((answered, asked, agents, at)) => format!(
            "Fleet \u{2014} {answered} of {asked} Mac{} answered{} \u{b7} read {} ago",
            plural(*asked),
            agents.map_or_else(String::new, |agents| format!(
                " \u{b7} {agents} agent{}",
                plural(agents)
            )),
            age_words(ms_age(*at, now_unix))
        ),
        None if reading => return "Fleet \u{2014} reading\u{2026}".to_string(),
        None if failed => return "Fleet \u{2014} last read failed".to_string(),
        None => return "Fleet \u{2014} not read yet".to_string(),
    };
    if reading {
        line.push_str(" \u{b7} reading\u{2026}");
    }
    line
}

#[cfg(any(target_os = "macos", test))]
fn degrade_rows(
    (member_of, unbound): (Option<&str>, Option<&str>),
    failed: Option<&str>,
) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    if let Some(issuer) = member_of {
        rows.push(StatusRow::Info(fold_clip(
            &format!("This Mac knows only its issuer {issuer}: update aterm on {issuer} to share its book"),
            LINE_MAX,
        )));
    }
    if let Some(issuer) = unbound {
        rows.push(unbound_issuer_row(issuer));
    }
    if let Some(why) = failed {
        rows.push(StatusRow::Info(fold_clip(
            &format!("Last read failed: {why}"),
            LINE_MAX,
        )));
    }
    rows
}

#[cfg(any(target_os = "macos", test))]
fn unbound_issuer_row(issuer: &str) -> StatusRow {
    StatusRow::Info(fold_clip(
        &format!(
            "This Mac binds no key to its issuer {issuer}: join again here once it has one \
             (`aterm fleet init` there)"
        ),
        LINE_MAX,
    ))
}

/// A fleet need's acts. Take Over and Hand Back show only where the other Mac would take them
/// ([`PeerRef::writes`]): a row it would refuse is not offered. The closing line says what this
/// Mac may do there and who changes that.
#[cfg(any(target_os = "macos", test))]
fn peer_rows(peer: &PeerRef, pick: Pick) -> Vec<StatusRow> {
    let mut rows = vec![act("Read This Session", StatusAction::PeerRead(pick))];
    if peer.writes() {
        rows.push(act(
            format!("Take Over on {}", peer.name),
            StatusAction::PeerTakeOver(pick),
        ));
        rows.push(act(
            format!("Hand Back on {}", peer.name),
            StatusAction::PeerHandBack(pick),
        ));
    }
    if let Some(host) = &peer.host {
        rows.push(act(
            format!("Screen Sharing to {host}"),
            StatusAction::PeerScreen(pick),
        ));
        rows.push(act(
            format!("Copy \u{201c}ssh {host}\u{201d}"),
            StatusAction::PeerSsh(pick),
        ));
    }
    let name = &peer.name;
    let line = match (peer.drive, peer.access) {
        (false, _) => format!(
            "Read-only from here: {name} does not let this Mac drive it (`aterm fleet allow drive \
             --from <this Mac>` there)"
        ),
        (true, Some(FleetAccess::ReadOnly)) => {
            format!("{name} is Read Only: this Mac reads it; its person raises that at its lock")
        }
        (true, Some(FleetAccess::ReadWrite)) => {
            format!("{name} is Read and Write: this Mac drives its sessions and starts none there")
        }
        (true, Some(FleetAccess::Full) | None) => {
            format!("{name} lets this Mac drive it; its person's Take Over wins")
        }
    };
    rows.push(StatusRow::Info(fold_clip(&line, LINE_MAX)));
    rows
}

/// A session on another Mac this Mac holds: Hand Back where that Mac would take it
/// ([`PeerRef::writes`]), and who lifts the hold where it would not.
#[cfg(any(target_os = "macos", test))]
fn held_there_rows(peer: &PeerRef, pick: Pick) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    if peer.writes() {
        rows.push(act(
            format!("Hand Back on {}", peer.name),
            StatusAction::PeerHandBack(pick),
        ));
    }
    rows.push(act("Read This Session", StatusAction::PeerRead(pick)));
    let name = &peer.name;
    let line = if peer.writes() {
        format!(
            "Until it is handed back, {name}'s harness leaves it alone and its app verbs refuse"
        )
    } else if peer.drive {
        format!("{name} is Read Only now: its person hands it back there")
    } else {
        format!("{name} no longer lets this Mac drive it: its person hands it back there")
    };
    rows.push(StatusRow::Info(fold_clip(&line, LINE_MAX)));
    rows
}

#[cfg(any(target_os = "macos", test))]
fn mac_row(
    mac: &crate::fleet_glance::FleetMac,
    pick: Pick,
    unbound: Option<&str>,
    now_unix: i64,
) -> StatusRow {
    let mut rows: Vec<StatusRow> = Vec::new();
    let read_at = i64::try_from(mac.read_at_ms / 1000).unwrap_or(0);
    for need in &mac.needs {
        let since = need
            .age_ms
            .map(|ms| read_at.saturating_sub(i64::try_from(ms / 1000).unwrap_or(i64::MAX)));
        rows.push(StatusRow::Info(fold_clip(
            &format!(
                "{} {} \u{2014} {}{}",
                need_mark(need.word),
                need.who,
                need.claim.unwrap_or_else(|| need_words(need.word)),
                age_suffix(since, now_unix)
            ),
            LINE_MAX,
        )));
    }
    for manager in &mac.managers {
        rows.push(StatusRow::Info(fold_clip(
            &format!(
                "{} ({}) \u{2014} {} worker{}",
                manager.label,
                crate::presence::short_sid(&manager.sid),
                manager.workers,
                plural(manager.workers)
            ),
            LINE_MAX,
        )));
    }
    if mac.unproven {
        for held in &mac.held_here {
            rows.push(StatusRow::Info(fold_clip(
                &format!(
                    "\u{270b} {} \u{2014} held from here: its person hands it back there",
                    held.who
                ),
                LINE_MAX,
            )));
        }
    }
    // Each count is absent, never zero, where it was not read: this Mac's copy reads no session
    // count (`sessions: None`), and an older aterm's rows, or a copy's Mac that publishes no fresh
    // activity row, no agents.
    let sessions = mac
        .sessions
        .filter(|&n| n > 0 || mac.activity.is_some())
        .map(|n| format!("{n} session{} \u{b7} ", plural(n)));
    let sessions = sessions.as_deref().unwrap_or("");
    rows.push(StatusRow::Info(match mac.activity {
        Some(t) => format!(
            "{sessions}{} agent{}: {} busy, {} waiting, {} walled",
            t.agents,
            plural(usize::from(t.agents)),
            t.busy,
            t.waiting,
            t.walled
        ),
        None => format!("{sessions}agents not read"),
    }));
    if let Some(build) = &mac.build {
        rows.push(StatusRow::Info(format!("at {build}")));
    }
    if mac.left_out > 0 {
        rows.push(StatusRow::Info(format!(
            "{} more not sent (its reply cap)",
            mac.left_out
        )));
    }
    if mac.instances_unread > 0 {
        rows.push(StatusRow::Info(format!(
            "{} of {} aterm instances read",
            mac.instances_read,
            mac.instances_read.saturating_add(mac.instances_unread)
        )));
    }
    rows.push(StatusRow::Separator);
    rows.push(act(
        format!("Read {} in Full", mac.name),
        StatusAction::MacRead(pick),
    ));
    if let Some(host) = &mac.host {
        rows.push(act(
            format!("Screen Sharing to {host}"),
            StatusAction::MacScreen(pick),
        ));
        rows.push(act(
            format!("Copy \u{201c}ssh {host}\u{201d}"),
            StatusAction::MacSsh(pick),
        ));
    }
    if mac.unproven {
        rows.push(StatusRow::Info(fold_clip(
            &format!(
                "{}'s key is not proven: what it says is its word alone, never counted, told or \
                 driven",
                mac.name
            ),
            LINE_MAX,
        )));
        rows.push(unbound.map_or_else(
            || {
                StatusRow::Info(fold_clip(
                    &format!(
                        "Update aterm on {} and join it again (`aterm fleet join`); `aterm fleet \
                         peers` shows its KEY",
                        mac.name
                    ),
                    LINE_MAX,
                ))
            },
            unbound_issuer_row,
        ));
    }
    // Where its activity was not read (an older aterm, this Mac's copy) the busy count is
    // absent, never 0.
    let busy = mac.activity.map(|t| t.busy);
    // The agents' words from the tally [`quiet_mac`] reads, so a Mac listed among the active is
    // never labelled quiet.
    let t = mac.activity.unwrap_or_default();
    let agents: Vec<String> = [
        (t.busy, "busy"),
        (t.waiting, "waiting"),
        (t.walled, "at a limit"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, word)| format!("{n} {word}"))
    .collect();
    let what = match mac.needs.len() {
        0 if agents.is_empty() && mac.instances_unread > 0 => "other instances unread".to_string(),
        0 if mac.activity.is_none() => "agents not read".to_string(),
        0 if agents.is_empty() => "quiet".to_string(),
        0 => agents.join(" \u{b7} "),
        n => {
            let needs = if mac.unproven {
                format!("{n} need{}", plural(n))
            } else {
                format!("{n} need{} you", if n == 1 { "s" } else { "" })
            };
            busy.map_or(needs.clone(), |b| format!("{needs} \u{b7} {b} busy"))
        }
    };
    let mark = if mac.unproven {
        "key not proven \u{b7} "
    } else {
        ""
    };
    let carried = if mac.carried {
        format!(
            " (read {} ago)",
            age_words(ms_age(mac.read_at_ms, now_unix))
        )
    } else {
        String::new()
    };
    let partial = if mac.instances_unread > 0 && (!mac.needs.is_empty() || !agents.is_empty()) {
        " \u{b7} partial read"
    } else {
        ""
    };
    // A Mac below Full Access says so, since this Mac may not do there what the level holds back.
    let access = match mac.access {
        Some(FleetAccess::ReadOnly) => " \u{b7} read only",
        Some(FleetAccess::ReadWrite) => " \u{b7} no new tabs",
        Some(FleetAccess::Full) | None => "",
    };
    StatusRow::Submenu {
        label: fold_clip(
            &format!(
                "{} \u{2014} {mark}{what}{partial}{access}{carried}",
                mac.name
            ),
            LINE_MAX,
        ),
        rows,
    }
}

/// Whether a fleet Mac is quiet: its agents read, no need, no agent busy, waiting or at a limit,
/// and no other instance reported unread. A Mac whose agents were not read is not quiet: absent,
/// never zero.
#[cfg(any(target_os = "macos", test))]
fn quiet_mac(mac: &crate::fleet_glance::FleetMac) -> bool {
    let Some(t) = mac.activity else {
        return false;
    };
    mac.needs.is_empty()
        && t.busy == 0
        && t.waiting == 0
        && t.walled == 0
        && mac.instances_unread == 0
}

/// Compose the Fleet submenu from the fleet glance's slot, ages as of `now_unix` — pure, at the submenu's own `menuNeedsUpdate:`: the Macs that need you, work or were only partly read, needs first; the quiet ones with no other instances reported, the ones that did not answer, the ones whose key is not proven (an older one among them stands with the older) and the older ones each folded into one row; then Refresh Now and Read the Fleet in Full. The degrade lines are the root menu's, under the Fleet row, where a person sees them without opening this.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn compose_fleet_menu(
    slot: &crate::fleet_glance::FleetSlot,
    now_unix: i64,
) -> Vec<StatusRow> {
    let unbound = slot.book.unbound_issuer.as_deref();
    let mut rows = Vec::new();
    match &slot.snapshot {
        None if slot.reading => rows.push(StatusRow::Info("Reading\u{2026}".to_string())),
        None => rows.push(StatusRow::Info("Not read yet".to_string())),
        Some(snap) => {
            let pick = |index: usize| {
                u16::try_from(index).ok().map(|index| Pick {
                    generation: slot.generation,
                    index,
                })
            };
            let row = |index: usize, mac| pick(index).map(|p| mac_row(mac, p, unbound, now_unix));
            let mut active: Vec<(usize, &crate::fleet_glance::FleetMac)> = snap
                .macs
                .iter()
                .enumerate()
                .filter(|(_, m)| m.counts() && !quiet_mac(m))
                .collect();
            // Needs first, then the agents that wait on a person or sit at a limit, then the
            // busy ones.
            active.sort_by_key(|(i, m)| {
                let t = m.activity.unwrap_or_default();
                (
                    std::cmp::Reverse(m.needs.len()),
                    std::cmp::Reverse(t.waiting.saturating_add(t.walled)),
                    std::cmp::Reverse(t.busy),
                    *i,
                )
            });
            rows.extend(active.into_iter().filter_map(|(i, m)| row(i, m)));
            let quiet: Vec<StatusRow> = snap
                .macs
                .iter()
                .enumerate()
                .filter(|(_, m)| m.counts() && quiet_mac(m))
                .filter_map(|(i, m)| row(i, m))
                .collect();
            if !quiet.is_empty() {
                rows.push(StatusRow::Submenu {
                    label: format!("{} quiet Mac{}", quiet.len(), plural(quiet.len())),
                    rows: quiet,
                });
            }
            if !snap.unanswered.is_empty() {
                rows.push(StatusRow::Submenu {
                    label: format!("{} not answering", snap.unanswered.len()),
                    rows: snap
                        .unanswered
                        .iter()
                        .map(|peer| {
                            StatusRow::Info(fold_clip(
                                &format!("{} \u{2014} {}", peer.name, peer.why),
                                LINE_MAX,
                            ))
                        })
                        .collect(),
                });
            }
            let unproven: Vec<StatusRow> = snap
                .macs
                .iter()
                .enumerate()
                .filter(|(_, m)| m.unproven && !m.older)
                .filter_map(|(i, m)| row(i, m))
                .collect();
            if !unproven.is_empty() {
                let one = unproven.len() == 1;
                rows.push(StatusRow::Submenu {
                    label: format!(
                        "{} {} not proven (not counted: {})",
                        unproven.len(),
                        if one { "Mac's key" } else { "Macs' keys" },
                        match (unbound, one) {
                            (Some(_), _) => "join again here",
                            (None, true) => "rejoin it",
                            (None, false) => "rejoin them",
                        }
                    ),
                    rows: unproven,
                });
            }
            let older: Vec<StatusRow> = snap
                .macs
                .iter()
                .enumerate()
                .filter(|(_, m)| m.older)
                .filter_map(|(i, m)| row(i, m))
                .collect();
            if !older.is_empty() {
                rows.push(StatusRow::Submenu {
                    label: format!(
                        "{} run{} an older aterm (not counted: update them)",
                        older.len(),
                        if older.len() == 1 { "s" } else { "" }
                    ),
                    rows: older,
                });
            }
        }
    }
    rows.push(StatusRow::Separator);
    rows.push(act("Refresh Now", StatusAction::ReadFleetNow));
    rows.push(act("Read the Fleet in Full", StatusAction::ReadFleet));
    rows
}

/// A ROW'S PATH IN THE COMPOSED MENU (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.2): its index among its menu's rows, separators and information lines counted, then its index in each submenu down to it — the Fleet row's rows being the ones its own open composes ([`compose_fleet_menu`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuAt(Vec<usize>);

impl MenuAt {
    const MAX_DEPTH: usize = 4;

    /// `at` read: one to [`Self::MAX_DEPTH`] indices joined by `.`, each plain decimal digits; `None` for anything else.
    pub(crate) fn parse(at: &str) -> Option<Self> {
        let path: Vec<usize> = at
            .split('.')
            .map(|i| {
                (!i.is_empty() && i.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| i.parse().ok())
                    .flatten()
            })
            .collect::<Option<_>>()?;
        (path.len() <= Self::MAX_DEPTH).then_some(Self(path))
    }
}

impl std::fmt::Display for MenuAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (n, i) in self.0.iter().enumerate() {
            if n > 0 {
                f.write_str(".")?;
            }
            write!(f, "{i}")?;
        }
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
const ROW_PATHS: bool = cfg!(any(test, debug_assertions));

/// The row `at` names in the menu `rows` composes (the Fleet row's own rows `fleet`), as [`menu_lines`] numbers them; `None` where no row stands there, or the path goes on below a row that opens no submenu.
#[cfg(any(test, all(target_os = "macos", debug_assertions)))]
pub(crate) fn row_at<'a>(
    rows: &'a [StatusRow],
    fleet: &'a [StatusRow],
    at: &MenuAt,
) -> Option<&'a StatusRow> {
    let (last, above) = at.0.split_last()?;
    let (mut level, mut fleet) = (rows, fleet);
    for &i in above {
        (level, fleet) = match level.get(i)? {
            StatusRow::Submenu { rows, .. } => (rows.as_slice(), fleet),
            StatusRow::FleetSubmenu { .. } => (fleet, &[][..]),
            StatusRow::Info(_) | StatusRow::Separator | StatusRow::Action { .. } => return None,
        };
    }
    level.get(*last)
}

/// The menu as text (`aterm ctl chrome`'s `statusmenu` lines): `rows`, as [`compose_status_menu`] composed them for the menu itself, one line per row in the order the menu shows them — each submenu's rows after it, indented two spaces a level, the Fleet row's being `fleet` (what its own open composes, [`compose_fleet_menu`]).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn menu_lines(rows: &[StatusRow], fleet: &[StatusRow]) -> Vec<String> {
    fn push(lines: &mut Vec<String>, rows: &[StatusRow], fleet: &[StatusRow], at: &mut MenuAt) {
        let indent = "  ".repeat(at.0.len());
        let yes = |on: bool| if on { "yes" } else { "no" };
        for (i, row) in rows.iter().enumerate() {
            at.0.push(i);
            let path = if ROW_PATHS {
                format!(" at={at}")
            } else {
                String::new()
            };
            match row {
                StatusRow::Info(text) => {
                    lines.push(format!("statusmenu {indent}{text:?} enabled=no{path}"))
                }
                StatusRow::Separator => lines.push(format!("statusmenu {indent}---{path}")),
                StatusRow::Action {
                    label,
                    enabled,
                    checked,
                    tip,
                    indent: level,
                    ..
                } => lines.push(format!(
                    "statusmenu {indent}{label:?} enabled={} checked={}{}{}{path}",
                    yes(*enabled),
                    yes(*checked),
                    tip.as_ref()
                        .map_or_else(String::new, |tip| format!(" tip={tip:?}")),
                    if *level > 0 {
                        format!(" indent={level}")
                    } else {
                        String::new()
                    }
                )),
                StatusRow::Submenu { label, rows } => {
                    lines.push(format!(
                        "statusmenu {indent}{label:?} enabled=yes submenu{path}"
                    ));
                    push(lines, rows, fleet, at);
                }
                StatusRow::FleetSubmenu { label } => {
                    lines.push(format!(
                        "statusmenu {indent}{label:?} enabled=yes submenu{path}"
                    ));
                    push(lines, fleet, &[], at);
                }
            }
            at.0.pop();
        }
    }
    let mut lines = Vec::new();
    push(&mut lines, rows, fleet, &mut MenuAt(Vec::new()));
    lines
}

/// The face to set on the bar for `glance` (its title and its lock), or `None` when it is the
/// one already shown.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn button_face_change(
    shown: Option<&ButtonFace>,
    glance: &Glance,
) -> Option<ButtonFace> {
    let next = glance.button_face();
    (shown != Some(&next)).then_some(next)
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{
    StatusItemHandle, attach_fleet, button_on_the_bar, face_on_the_bar, install, items_on_the_bar,
    update,
};

/// Non-macOS no-op handle: there is no menu-bar status item off macOS.
#[cfg(not(target_os = "macos"))]
pub(crate) type StatusItemHandle = ();

/// Non-macOS stub: nothing to refresh.
#[cfg(not(target_os = "macos"))]
pub(crate) fn update(_handle: &StatusItemHandle, _glance: Glance) {}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::sync::{Arc, Mutex};

    // The loading lookup: a class of AppKit's is asked for by name whether or not anything
    // has loaded AppKit yet.
    use aterm_macos_ui::appkit::class;
    use aterm_macos_ui::status_button;
    use aterm_objc::{Bool, CGSize, Id, Obj, Retained, Sel, autoreleasepool, sel};
    use aterm_types::fleet_access::FleetAccess;
    use aterm_types::status_click::EventKey;
    use aterm_window::event_loop::EventLoopProxy;

    use super::{ButtonFace, ClickAct, Glance, StatusAction};
    use crate::Wake;
    use crate::appkit::consts::NS_VARIABLE_STATUS_ITEM_LENGTH;
    use crate::appkit::{self, MainThread};

    const NS_CONTROL_STATE_VALUE_ON: isize = 1;
    /// `NSImageLeft`: the lock at the title's leading side.
    const NS_IMAGE_LEFT: usize = 2;

    pub(crate) struct StatusItemHandle {
        target: Retained<StatusTarget>,
        item: Obj,
        /// The menu: the item has no `-menu` of its own (AppKit would open that on every
        /// press), so the handle keeps it for the click handler to pop up.
        menu: Obj,
        /// The lock at each level, lowest first, built once; `None` where one did not build,
        /// and the face then shows the caret alone.
        locks: [Option<Obj>; 3],
        shown_face: RefCell<Option<ButtonFace>>,
        shown_tip: RefCell<Option<String>>,
        shown_label: RefCell<Option<String>>,
        /// The watch on this Mac's stored fleet access (`AppRtMacOS::install_status_item`),
        /// which lives as long as the item.
        _access_watch: Option<aterm_macos_ui::darwin_notify::Observer>,
    }

    impl StatusItemHandle {
        /// Keep `watch`, the observer of the stored fleet access's change notification, for as
        /// long as the item is on the bar.
        pub(crate) fn keep_access_watch(
            &mut self,
            watch: Option<aterm_macos_ui::darwin_notify::Observer>,
        ) {
            self._access_watch = watch;
        }
    }

    pub(crate) struct StatusIvars {
        proxy: EventLoopProxy<Wake>,
        glance: RefCell<Glance>,
        painted: RefCell<Option<Glance>>,
        fleet: RefCell<Option<Arc<Mutex<crate::fleet_glance::FleetSlot>>>>,
        fleet_menu: Cell<usize>,
        /// The menu and the button it drops from, set once the item is installed
        /// ([`StatusTarget::show_menu`]).
        bar: RefCell<Option<(Obj, Obj)>>,
        /// The app's current event when the last press was read or the menu closed
        /// (`aterm_types::status_click::classify`).
        stale: Cell<Option<EventKey>>,
        /// The click handler is popping the menu up, having painted it and announced its open
        /// itself: the root's `menuNeedsUpdate:` and `menuWillOpen:` do neither again, whether
        /// or not AppKit sends them for a hand-popped menu.
        popping: Cell<bool>,
    }

    aterm_objc::declare_class! {
        pub(crate) struct StatusTarget: NSObject {
            const NAME: &str = "ATermStatusTarget";
            type Ivars = StatusIvars;
            protocols: [NSObject, NSMenuDelegate];

            @sel(statusAction:)
            fn status_action(&self, sender: Id) {
                if sender.is_null() {
                    return;
                }
                // SAFETY: `sender` is the live NSMenuItem AppKit passed as the action sender; `-tag` is `-(NSInteger)` with no side effects.
                let tag = unsafe { appkit::send_isize(sender, sel!(tag)) };
                if let Some(action) = StatusAction::from_tag(tag) {
                    // Fire-and-forget: a closed loop (app shutting down) just
                    // drops the event — mirrors menu.rs.
                    let _ = self.ivars().proxy.send_event(Wake::StatusAction { action });
                }
            }

            /// The button was released: the lock rotates, or the menu opens
            /// ([`super::click_act`]).
            @sel(statusClicked:)
            fn status_clicked(&self, _sender: Id) {
                let Some(main) = MainThread::new() else {
                    return;
                };
                let event = status_button::current_event(main);
                let stale = self.ivars().stale.replace(event);
                let in_fleet = self
                    .ivars()
                    .glance
                    .try_borrow()
                    .is_ok_and(|glance| glance.in_fleet());
                match super::click_act(event, stale, in_fleet) {
                    ClickAct::Menu => self.show_menu(main),
                    ClickAct::Rotate { repeated } => {
                        let _ = self.ivars().proxy.send_event(Wake::StatusAction {
                            action: StatusAction::RotateFleetAccess { repeated },
                        });
                    }
                }
            }

            /// The button's Show Menu accessibility action (`status_button::SHOW_MENU`):
            /// VoiceOver cannot right-click. The menu opens from the next turn of the run loop,
            /// once the action has answered.
            @sel(showMenuAction:)
            fn show_menu_action(&self, _action: Id) -> Bool {
                if MainThread::new().is_none() {
                    return Bool::NO;
                }
                if !self.ivars().popping.get() {
                    // SAFETY: `-performSelector:withObject:afterDelay:` is `v40@0:8:16@24d32` on
                    // this live object, which the run loop retains until the perform; `openMenu:`
                    // is a `v@:@` method this class declares.
                    unsafe {
                        appkit::send_v_sel_id_f64(
                            self.as_id(),
                            sel!(performSelector:withObject:afterDelay:),
                            sel!(openMenu:),
                            Id::NIL,
                            0.0,
                        );
                    }
                }
                Bool::YES
            }

            /// The delayed perform [`Self::show_menu_action`] scheduled.
            @sel(openMenu:)
            fn open_menu(&self, _arg: Id) {
                if let Some(main) = MainThread::new() {
                    self.show_menu(main);
                }
            }

            @sel(menuNeedsUpdate:)
            fn menu_needs_update(&self, menu: Id) {
                if menu.is_null() || MainThread::new().is_none() {
                    return;
                }
                if menu.addr() == self.ivars().fleet_menu.get() {
                    paint_fleet(menu, self);
                    return;
                }
                if self.ivars().popping.get() {
                    return;
                }
                if let Some(painted) = self.paint_root(menu)
                    && let Ok(mut slot) = self.ivars().painted.try_borrow_mut()
                {
                    *slot = Some(painted);
                }
            }

            @sel(menuWillOpen:)
            fn menu_will_open(&self, menu: Id) {
                // The root's open only: the Fleet submenu's is the same visit.
                if menu.addr() == self.ivars().fleet_menu.get() || self.ivars().popping.get() {
                    return;
                }
                let painted = self
                    .ivars()
                    .painted
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut slot| slot.take())
                    .map(Box::new);
                let _ = self
                    .ivars()
                    .proxy
                    .send_event(Wake::StatusMenuOpening { painted });
            }
        }
    }

    impl StatusTarget {
        /// Compose the root menu from the item's glance and paint it into `menu`, answering the
        /// glance it was painted from, which a row's pick resolves against (`App::status_pick`);
        /// `None` where the glance is borrowed and nothing was painted.
        fn paint_root(&self, menu: Id) -> Option<Glance> {
            let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
            let (rows, painted) = match self.ivars().glance.try_borrow() {
                Ok(glance) => (super::compose_status_menu(&glance, now), glance.clone()),
                Err(_) => return None,
            };
            autoreleasepool(|_| paint_menu(menu, self, rows));
            Some(painted)
        }

        /// A menu click: paint the menu, announce its open with the glance it was painted from
        /// (as `menuWillOpen:` does for a menu AppKit opens), and pop it up below the button,
        /// outside every borrow. Returns when the menu closes.
        fn show_menu(&self, main: MainThread) {
            if self.ivars().popping.get() {
                return;
            }
            let bar = self.ivars().bar.try_borrow().ok().and_then(|bar| {
                bar.as_ref()
                    .map(|(menu, button)| (menu.clone_retained(), button.clone_retained()))
            });
            let Some((menu, button)) = bar else {
                return;
            };
            let painted = self.paint_root(menu.id()).map(Box::new);
            let _ = self
                .ivars()
                .proxy
                .send_event(Wake::StatusMenuOpening { painted });
            self.ivars().popping.set(true);
            // SAFETY: on the main thread (`main`), with the live menu and button retained here
            // across the call, and no borrow of the target's cells held: the menu's items send
            // `statusAction:` and its Fleet submenu `menuNeedsUpdate:`, which borrow them.
            unsafe { status_button::pop_up(menu.id(), button.id()) };
            self.ivars().popping.set(false);
            // The event that closed the menu is no press of the button.
            self.ivars().stale.set(status_button::current_event(main));
            autoreleasepool(|_| drop((menu, button)));
        }
    }

    pub(crate) fn install(
        proxy: &EventLoopProxy<Wake>,
        glance: &Glance,
    ) -> Option<StatusItemHandle> {
        let main = MainThread::new()?;
        let target = StatusTarget::alloc_init(
            main,
            StatusIvars {
                proxy: proxy.clone(),
                glance: RefCell::new(glance.clone()),
                painted: RefCell::new(Some(glance.clone())),
                fleet: RefCell::new(None),
                fleet_menu: Cell::new(0),
                bar: RefCell::new(None),
                stale: Cell::new(None),
                popping: Cell::new(false),
            },
        )?;
        let handle = autoreleasepool(|_| -> Option<StatusItemHandle> {
            // SAFETY: `+systemStatusBar` is a `-(id)` singleton accessor and
            // `-statusItemWithLength:` is `-(id)(CGFloat)`; both are main-thread
            // AppKit calls (proved by `main`). The item comes back AUTORELEASED
            // (+0), so it is retained into the handle here — releasing an
            // `NSStatusItem` removes it from the menu bar, which is why the
            // handle owns one.
            let item = unsafe {
                let bar = appkit::send_id(class(c"NSStatusBar").as_id(), sel!(systemStatusBar));
                if bar.is_null() {
                    return None;
                }
                let raw = appkit::send_id_f64(
                    bar,
                    sel!(statusItemWithLength:),
                    NS_VARIABLE_STATUS_ITEM_LENGTH,
                );
                Obj::retain(raw)
            }?;
            let menu = new_menu()?;
            let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
            paint_menu(menu.id(), &target, super::compose_status_menu(glance, now));
            // SAFETY: main-thread AppKit sends on live receivers. `-setDelegate:` is
            // `-(void)(id)`: the delegate is the retained target, which outlives the menu
            // because the handle owns both. `-button` is `-(id)` and answers the item's bar
            // button at +0, retained here. The button holds its target weakly (the handle keeps
            // it alive), and the target answers `statusClicked:` as `v@:@` and
            // `showMenuAction:` as `B@:@` (`status_button::wire_clicks`,
            // `add_show_menu_action`). `-setImagePosition:` is `-(void)(NSCellImagePosition)`.
            // The item is given no `-menu`: AppKit would open that on every press, and a left
            // click is the lock's.
            let button = unsafe {
                appkit::send_v_id(menu.id(), sel!(setDelegate:), target.as_id());
                let button = Obj::retain(appkit::send_id(item.id(), sel!(button)))?;
                status_button::wire_clicks(button.id(), target.as_id(), sel!(statusClicked:));
                status_button::add_show_menu_action(
                    button.id(),
                    target.as_id(),
                    sel!(showMenuAction:),
                );
                appkit::send_v_usize(button.id(), sel!(setImagePosition:), NS_IMAGE_LEFT);
                button
            };
            *target.ivars().bar.borrow_mut() = Some((menu.clone_retained(), button));
            Some(StatusItemHandle {
                target,
                item,
                menu,
                locks: FleetAccess::ALL.map(lock_image),
                shown_face: RefCell::new(None),
                shown_tip: RefCell::new(None),
                shown_label: RefCell::new(None),
                _access_watch: None,
            })
        })?;
        show_face(&handle, glance.button_face());
        show_tip(&handle, glance.tooltip());
        show_label(&handle, glance.accessibility_label());
        Some(handle)
    }

    /// `level`'s lock as a template image holding a bitmap per scale
    /// (`crate::status_lock_icon`).
    fn lock_image(level: FleetAccess) -> Option<Obj> {
        use crate::status_lock_icon as icon;
        let (width, height) = icon::SIZE_POINTS;
        let pngs = icon::SCALES.map(|scale| icon::png(level, scale));
        status_button::template_image(
            &pngs.each_ref().map(Vec::as_slice),
            CGSize { width, height },
            icon::description(level),
        )
    }

    pub(crate) fn update(handle: &StatusItemHandle, glance: Glance) {
        let Some(_main) = MainThread::new() else {
            return;
        };
        let change = super::button_face_change(handle.shown_face.borrow().as_ref(), &glance);
        if let Some(face) = change {
            show_face(handle, face);
        }
        let tip = glance.tooltip();
        if handle.shown_tip.borrow().as_deref() != Some(tip.as_str()) {
            show_tip(handle, tip);
        }
        let label = glance.accessibility_label();
        if handle.shown_label.borrow().as_deref() != Some(label.as_str()) {
            show_label(handle, label);
        }
        if let Ok(mut stored) = handle.target.ivars().glance.try_borrow_mut() {
            *stored = glance;
        }
    }

    pub(crate) fn attach_fleet(
        handle: &StatusItemHandle,
        slot: Arc<Mutex<crate::fleet_glance::FleetSlot>>,
    ) {
        *handle.target.ivars().fleet.borrow_mut() = Some(slot);
    }

    fn paint_fleet(menu: Id, target: &StatusTarget) {
        let slot = target
            .ivars()
            .fleet
            .try_borrow()
            .ok()
            .and_then(|fleet| fleet.as_ref().map(Arc::clone));
        let Some(slot) = slot else {
            return;
        };
        let slot = slot.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
        let rows = super::compose_fleet_menu(&slot, now);
        autoreleasepool(|_| paint_menu(menu, target, rows));
    }

    fn show_face(handle: &StatusItemHandle, face: ButtonFace) {
        let image = face
            .lock
            .and_then(|level| handle.locks[level as usize].as_ref())
            .map_or(Id::NIL, Obj::id);
        let set = autoreleasepool(|_| {
            // SAFETY: plain AppKit sends on live receivers, on the main thread
            // (both callers checked `MainThread::new()`). `-button` is `-(id)`
            // and returns the bar button for an item created with a variable
            // length; `-setTitle:` and `-setAttributedTitle:` are
            // `-(void)(id)`, the plain title replacing any attributed one.
            // `-setImage:` is `-(void)(NSImage *)`, handed a live image the handle
            // holds (the button retains it too) or nil, which shows none.
            unsafe {
                let button = appkit::send_id(handle.item.id(), sel!(button));
                if button.is_null() {
                    return false;
                }
                let Some(text) = appkit::nsstring(&face.title) else {
                    return false;
                };
                appkit::send_v_id(button, sel!(setTitle:), text.id());
                appkit::send_v_id(button, sel!(setImage:), image);
                true
            }
        });
        if set {
            *handle.shown_face.borrow_mut() = Some(face);
        }
    }

    fn show_tip(handle: &StatusItemHandle, tip: String) {
        let set = autoreleasepool(|_| {
            // SAFETY: as `show_face`; `-setToolTip:` is `-(void)(NSString *)`.
            unsafe {
                let button = appkit::send_id(handle.item.id(), sel!(button));
                if button.is_null() {
                    return false;
                }
                let Some(text) = appkit::nsstring(&tip) else {
                    return false;
                };
                appkit::send_v_id(button, sel!(setToolTip:), text.id());
                true
            }
        });
        if set {
            *handle.shown_tip.borrow_mut() = Some(tip);
        }
    }

    fn show_label(handle: &StatusItemHandle, label: String) {
        let set = autoreleasepool(|_| {
            // SAFETY: as `show_face`; `-setAccessibilityLabel:` is `-(void)(NSString *)`.
            unsafe {
                let button = appkit::send_id(handle.item.id(), sel!(button));
                if button.is_null() {
                    return false;
                }
                let Some(text) = appkit::nsstring(&label) else {
                    return false;
                };
                appkit::send_v_id(button, sel!(setAccessibilityLabel:), text.id());
                true
            }
        });
        if set {
            *handle.shown_label.borrow_mut() = Some(label);
        }
    }

    fn new_menu() -> Option<Obj> {
        // SAFETY: `+alloc` on `NSMenu` gives a +1 uninitialised instance and `-init` consumes it, so `Obj::from_owned` adopts exactly one +1; `-setAutoenablesItems:` is `-(void)(BOOL)` on the fresh menu.
        unsafe {
            let raw = appkit::send_id(appkit::alloc(class(c"NSMenu")), sel!(init));
            let menu = Obj::from_owned(raw)?;
            appkit::send_v_bool(menu.id(), sel!(setAutoenablesItems:), false);
            Some(menu)
        }
    }

    fn paint_menu(menu: Id, target: &StatusTarget, rows: Vec<super::StatusRow>) {
        // SAFETY: `-removeAllItems` is `-(void)` on the live menu, on the main thread.
        unsafe { appkit::send_v(menu, sel!(removeAllItems)) };
        for row in rows {
            match row {
                super::StatusRow::Info(text) => add_info(menu, &text),
                super::StatusRow::Separator => add_separator(menu),
                super::StatusRow::Action {
                    label,
                    action,
                    enabled,
                    checked,
                    tip,
                    indent,
                } => add_action(
                    menu,
                    target,
                    &label,
                    action,
                    (enabled, checked, indent),
                    tip.as_deref(),
                ),
                super::StatusRow::Submenu { label, rows } => {
                    add_submenu(menu, target, &label, rows);
                }
                super::StatusRow::FleetSubmenu { label } => add_fleet_submenu(menu, target, &label),
            }
        }
    }

    fn new_item(title: &str, action: Sel) -> Option<Obj> {
        let title = appkit::nsstring(title)?;
        let empty = appkit::nsstring("")?;
        // SAFETY: `initWithTitle:action:keyEquivalent:` is NSMenuItem's
        // designated initializer, `-(id)(NSString *, SEL, NSString *)`, and a
        // nil SEL is its documented "no action" value. `+alloc` is +1 and the
        // initializer consumes it, so `Obj::from_owned` adopts one +1. Both
        // strings are live +1 NSStrings the initializer copies.
        unsafe {
            let raw = appkit::send_id_id_sel_id(
                appkit::alloc(class(c"NSMenuItem")),
                sel!(initWithTitle:action:keyEquivalent:),
                title.id(),
                action,
                empty.id(),
            );
            Obj::from_owned(raw)
        }
    }

    fn add_action(
        menu: Id,
        target: &StatusTarget,
        title: &str,
        action: StatusAction,
        looks: (bool, bool, u8),
        tip: Option<&str>,
    ) {
        let Some(item) = action_item(target.as_id(), title, action, looks) else {
            return;
        };
        if let Some(tip) = tip.and_then(appkit::nsstring) {
            // SAFETY: `-setToolTip:` is `-(void)(NSString *)` on the fresh item, handed a live string it copies.
            unsafe { appkit::send_v_id(item.id(), sel!(setToolTip:), tip.id()) };
        }
        // SAFETY: `-addItem:` is `-(void)(NSMenuItem *)` on the live menu, handed the fresh item, which it retains.
        unsafe { appkit::send_v_id(menu, sel!(addItem:), item.id()) };
    }

    pub(super) fn action_item(
        target: Id,
        title: &str,
        action: StatusAction,
        (enabled, checked, indent): (bool, bool, u8),
    ) -> Option<Obj> {
        let item = new_item(title, sel!(statusAction:))?;
        // SAFETY: plain setters on a fresh NSMenuItem — `-setTarget:` is `-(void)(id)` (AppKit holds the target WEAKLY, which is why the handle retains it; nil is "no target"), `-setTag:`, `-setState:` and `-setIndentationLevel:` are `-(void)(NSInteger)` (the level within AppKit's 0-15) and `-setEnabled:` is `-(void)(BOOL)`.
        unsafe {
            appkit::send_v_id(item.id(), sel!(setTarget:), target);
            appkit::send_v_isize(item.id(), sel!(setTag:), action.tag());
            appkit::send_v_bool(item.id(), sel!(setEnabled:), enabled);
            if checked {
                appkit::send_v_isize(item.id(), sel!(setState:), NS_CONTROL_STATE_VALUE_ON);
            }
            if indent > 0 {
                appkit::send_v_isize(item.id(), sel!(setIndentationLevel:), isize::from(indent));
            }
        }
        Some(item)
    }

    fn add_submenu(menu: Id, target: &StatusTarget, title: &str, rows: Vec<super::StatusRow>) {
        let Some(item) = new_item(title, Sel::NULL) else {
            return;
        };
        let Some(submenu) = new_menu() else {
            return;
        };
        paint_menu(submenu.id(), target, rows);
        // SAFETY: `-setSubmenu:` is `-(void)(NSMenu *)` and `-setEnabled:` `-(void)(BOOL)` on the fresh item; the item retains the submenu, and `-addItem:` retains the item into the live menu.
        unsafe {
            appkit::send_v_id(item.id(), sel!(setSubmenu:), submenu.id());
            appkit::send_v_bool(item.id(), sel!(setEnabled:), true);
            appkit::send_v_id(menu, sel!(addItem:), item.id());
        }
    }

    fn add_fleet_submenu(menu: Id, target: &StatusTarget, title: &str) {
        let Some(item) = new_item(title, Sel::NULL) else {
            return;
        };
        let Some(submenu) = new_menu() else {
            return;
        };
        target.ivars().fleet_menu.set(submenu.id().addr());
        paint_fleet(submenu.id(), target);
        // SAFETY: as `add_submenu`; `-setDelegate:` is `-(void)(id)` on the fresh submenu, its delegate the retained target that outlives it.
        unsafe {
            appkit::send_v_id(submenu.id(), sel!(setDelegate:), target.as_id());
            appkit::send_v_id(item.id(), sel!(setSubmenu:), submenu.id());
            appkit::send_v_bool(item.id(), sel!(setEnabled:), true);
            appkit::send_v_id(menu, sel!(addItem:), item.id());
        }
    }

    fn add_info(menu: Id, title: &str) {
        let Some(item) = new_item(title, Sel::NULL) else {
            return;
        };
        // SAFETY: as `add_action`, on an item with no action.
        unsafe {
            appkit::send_v_bool(item.id(), sel!(setEnabled:), false);
            appkit::send_v_id(menu, sel!(addItem:), item.id());
        }
    }

    /// One item of the real menu as AppKit holds it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct BarItem {
        /// `-title`.
        pub(crate) title: String,
        /// `-isEnabled`.
        pub(crate) enabled: bool,
        /// `-toolTip`, `None` where it is nil.
        pub(crate) tip: Option<String>,
        /// `-state` is on: the item carries a checkmark.
        pub(crate) checked: bool,
        /// Its `-submenu`'s items, in order; empty where it has none.
        pub(crate) under: Vec<BarItem>,
    }

    /// What the menu bar's real item holds as a person opens it: the menu the click handler pops
    /// up (the handle's, since the item has no `-menu`), repainted by its own delegate's
    /// `menuNeedsUpdate:` from the item's glance, each item in order. `objc_toolbar_drive` reads
    /// This Mac, Needs you and Fleet Access off it; empty off the main thread.
    pub(crate) fn items_on_the_bar(handle: &StatusItemHandle) -> Vec<BarItem> {
        if MainThread::new().is_none() {
            return Vec::new();
        }
        autoreleasepool(|_| {
            // SAFETY: on the main thread (checked above); `-delegate` is a `-(id)` accessor on
            // the live menu the handle owns, and `menuNeedsUpdate:` is `-(void)(id)` on the
            // retained target the handle owns, sent with the menu it delegates for, as AppKit
            // sends it.
            unsafe {
                let menu = handle.menu.id();
                let delegate = appkit::send_id(menu, sel!(delegate));
                if !delegate.is_null() {
                    appkit::send_v_id(delegate, sel!(menuNeedsUpdate:), menu);
                }
                items_of(menu)
            }
        })
    }

    /// The button's face as AppKit holds it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct BarFace {
        /// The button's `-title`.
        pub(crate) title: String,
        /// Its `-image`'s `-accessibilityDescription` (`lock closed`), `None` where it shows no
        /// image.
        pub(crate) image: Option<String>,
        /// The image is a template, which AppKit tints for the bar.
        pub(crate) template: bool,
        /// Its `-toolTip`.
        pub(crate) tip: Option<String>,
        /// Its `-accessibilityLabel`.
        pub(crate) label: Option<String>,
        /// The item's own `-menu` is nil: a press is the click handler's, never AppKit's menu.
        pub(crate) no_menu: bool,
        /// The button's `-action` is `statusClicked:`.
        pub(crate) clicks: bool,
    }

    /// The real item's face, read back off AppKit: what `objc_toolbar_drive` holds the lock to;
    /// `None` off the main thread or where the item has no button.
    pub(crate) fn face_on_the_bar(handle: &StatusItemHandle) -> Option<BarFace> {
        MainThread::new()?;
        autoreleasepool(|_| {
            // SAFETY: on the main thread (checked above); `-button`, `-image`, `-menu`, `-title`,
            // `-toolTip`, `-accessibilityLabel` and `-accessibilityDescription` are `-(id)`
            // accessors (nil where none is set), `-isTemplate` `-(BOOL)` and `-action` `-(SEL)`,
            // on the live item, its button and its image, all without side effects.
            unsafe {
                let button = appkit::send_id(handle.item.id(), sel!(button));
                if button.is_null() {
                    return None;
                }
                let string = |s: Id| (!s.is_null()).then(|| appkit::nsstring_to_rust(s));
                let image = appkit::send_id(button, sel!(image));
                let action: unsafe extern "C-unwind" fn(Id, Sel) -> Sel = aterm_objc::checked_msg();
                Some(BarFace {
                    title: appkit::nsstring_to_rust(appkit::send_id(button, sel!(title))),
                    image: (!image.is_null())
                        .then(|| string(appkit::send_id(image, sel!(accessibilityDescription))))
                        .flatten(),
                    template: !image.is_null() && appkit::send_bool(image, sel!(isTemplate)),
                    tip: string(appkit::send_id(button, sel!(toolTip))),
                    label: string(appkit::send_id(button, sel!(accessibilityLabel))),
                    no_menu: appkit::send_id(handle.item.id(), sel!(menu)).is_null(),
                    clicks: action(button, sel!(action)) == sel!(statusClicked:),
                })
            }
        })
    }

    /// The real item's bar button, retained: what `objc_toolbar_drive` has AppKit draw, to see
    /// the lock on it. `None` off the main thread or where the item has no button.
    pub(crate) fn button_on_the_bar(handle: &StatusItemHandle) -> Option<Obj> {
        MainThread::new()?;
        // SAFETY: on the main thread (checked above); `-button` is a `-(id)` accessor on the
        // live item, answering its button at +0, retained here.
        unsafe { Obj::retain(appkit::send_id(handle.item.id(), sel!(button))) }
    }

    /// Each item of the live `menu`, in order: its `-title`, `-isEnabled`, `-toolTip`, whether
    /// it is checked and its `-submenu`'s items.
    fn items_of(menu: Id) -> Vec<BarItem> {
        // SAFETY: `menu` is a live NSMenu; `-numberOfItems` is `-(NSInteger)`, `-itemAtIndex:`
        // `-(NSMenuItem *)(NSInteger)` within its count, `-title` and `-toolTip`
        // `-(NSString *)` (nil where none was set), `-submenu` `-(NSMenu *)` (nil where none),
        // `-isEnabled` `-(BOOL)` and `-state` `-(NSControlStateValue)`, an `NSInteger`, all
        // without side effects on the menu and its items.
        unsafe {
            let count = appkit::send_isize(menu, sel!(numberOfItems));
            let item_at: unsafe extern "C-unwind" fn(Id, Sel, isize) -> Id =
                aterm_objc::checked_msg();
            let string = |s: Id| (!s.is_null()).then(|| appkit::nsstring_to_rust(s));
            (0..count)
                .map(|i| {
                    let item = item_at(menu, sel!(itemAtIndex:), i);
                    let sub = appkit::send_id(item, sel!(submenu));
                    BarItem {
                        title: appkit::nsstring_to_rust(appkit::send_id(item, sel!(title))),
                        enabled: appkit::send_bool(item, sel!(isEnabled)),
                        tip: string(appkit::send_id(item, sel!(toolTip))),
                        checked: appkit::send_isize(item, sel!(state)) == NS_CONTROL_STATE_VALUE_ON,
                        under: if sub.is_null() {
                            Vec::new()
                        } else {
                            items_of(sub)
                        },
                    }
                })
                .collect()
        }
    }

    fn add_separator(menu: Id) {
        // SAFETY: `+separatorItem` is `-(id)` and returns a shared, AUTORELEASED item — borrowed here for the length of `-addItem:`, which retains it into the menu, inside the caller's pool.
        unsafe {
            let sep = appkit::send_id(class(c"NSMenuItem").as_id(), sel!(separatorItem));
            if !sep.is_null() {
                appkit::send_v_id(menu, sel!(addItem:), sep);
            }
        }
    }

    #[cfg(test)]
    mod tests;
}

#[cfg(test)]
#[path = "status_item_tests.rs"]
mod tests;
