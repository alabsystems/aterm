// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The menu-bar status item: the caret, the lock beside it, and the menu a right click opens.
//!
//! The item is about this Mac. A left click rotates its fleet access ([`LockFace`]); the menu
//! ([`compose_status_menu`]) names this Mac, its fleet and the network it is on, offers the
//! three levels, and sums up the agent requests between this Mac and the others over the last
//! hour, from the local record (`aterm_types::fleet_activity`), with the ones a lock refused
//! marked. No row asks the person to do anything, and none describes a session.
//!
//! The file also keeps what a session's published need and hold are aged by ([`NeedAges`],
//! over a [`SessionRow`]): `await needs`, the take-over order and a seamless update's carry
//! read it.

use aterm_agent::fleet_access::Basis;
use aterm_types::fleet_access::FleetAccess;
use aterm_types::fleet_activity::Summary;
use aterm_types::needs::{HoldWord, NeedWord};

use crate::task_record::TaskState;

/// One user action from the status item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum StatusAction {
    /// A left click on the lock: this Mac's fleet access one step on ([`LockFace::click`]).
    /// `repeated` is the second or a later click of a multi-click, which lowers or cancels
    /// and never raises.
    RotateFleetAccess {
        /// AppKit counted the release as 2 or more.
        repeated: bool,
    },
    /// A level picked in the menu: this Mac's fleet access, set at once ([`LockFace::pick`]).
    SetFleetAccess(FleetAccess),
    /// All activity…: `aterm fleet activity` into a document tab, the record the menu's
    /// traffic rows are summed from.
    AllActivity,
}

#[cfg(any(target_os = "macos", test))]
const TAG_BAND: isize = 1_000_000_000_000;

/// The band a picked level's tag is in: its place in the rising order rides below it.
#[cfg(any(target_os = "macos", test))]
const SET_ACCESS_BAND: isize = 16;

#[cfg(any(target_os = "macos", test))]
impl StatusAction {
    /// The `NSMenuItem.tag` this action rides in. A number an earlier menu's row used is not
    /// used again: 1 to 13 and 16, and the bands 1 to 15.
    pub(crate) fn tag(self) -> isize {
        match self {
            Self::RotateFleetAccess { repeated: false } => 14,
            Self::RotateFleetAccess { repeated: true } => 15,
            Self::AllActivity => 17,
            // The level's place in the rising order: 0, 1 or 2.
            Self::SetFleetAccess(level) => SET_ACCESS_BAND * TAG_BAND + level as isize,
        }
    }

    /// Inverse of [`tag`](Self::tag); unknown and retired tags are `None` (an inert item).
    pub(crate) fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            14 => Some(Self::RotateFleetAccess { repeated: false }),
            15 => Some(Self::RotateFleetAccess { repeated: true }),
            17 => Some(Self::AllActivity),
            t if t / TAG_BAND == SET_ACCESS_BAND => usize::try_from(t % TAG_BAND)
                .ok()
                .and_then(|at| FleetAccess::ALL.get(at).copied())
                .map(Self::SetFleetAccess),
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
/// no event or a stale one. A level is then one pick away in the menu,
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

/// One session as the need-age keeper reads it ([`NeedAges::observe`]), copied under the leaf
/// locks by `App::status_session_row`: its tab label, its published verdict
/// ([`crate::needs::Verdict`]), its agent word and its task record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionRow {
    /// The session's global sid (a seamless update's carry is keyed by it).
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
}

impl SessionRow {
    /// The word the row is classed by: the reader's own word, or, where it names no state,
    /// the program's (`program`).
    #[must_use]
    pub(crate) fn glance_word(&self) -> &'static str {
        self.program.unwrap_or(self.agent)
    }
}

/// A worker's task record as the need-age keeper reads it.
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
    /// The owner looked at this done (`App.done_seen` holds its rev).
    pub seen: bool,
}

/// The stall half of a session's need facts — `input_stall::stall_row`'s words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputStallRow {
    /// The server's own attention words for the stall (`<prog> is frozen: not reading input since <HH:MM> (…) — restart it: …`), as its `meta attention=` entry carries them.
    pub text: String,
    /// The episode's identity (a hash of when its oldest unread byte was accepted): one need per stall, however its text moves.
    pub key: u64,
    /// The job is stopped with input queued (`signal cont` resumes it), not frozen: the need's word says which.
    pub stopped: bool,
}

/// THE NEED-AGE KEEPER: per session, since when its published need and its hold have stood ([`Self::observe`]), which the take-over order reads, and what a seamless update carried of them ([`Self::adopt`]).
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

    /// Forget a retired session.
    pub(crate) fn retire(&mut self, session: u64) {
        self.since.remove(&session);
        self.adopted.remove(&session);
    }
}

/// The glance: everything the status item shows, as pure data (`App::status_glance`), painted
/// by [`compose_status_menu`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Glance {
    /// This machine's title (`m8 Heron`, [`crate::machine_title::machine_menu_bar_title`]),
    /// which leads the menu's first line and the tooltip
    /// (`docs/DESIGN-machine-names-2026-11-04.md` §5.5); `None` until the title's reader has
    /// read it, and off macOS.
    pub machine: Option<String>,
    /// This Mac is in a fleet (`fleet_glance::read_book`'s answer): the lock shows, and the
    /// menu has its levels and the traffic.
    pub in_fleet: bool,
    /// Where this Mac is on the network, as the first line ends
    /// (`crate::net_place::identity_words`); `None` until a place was read.
    pub network: Option<String>,
    /// This Mac's fleet access in force, as its state file last read ([`LockFace::on_disk`]):
    /// the menu's checkmark.
    pub access: FleetAccess,
    /// The level the lock shows ([`LockFace::shown`]): a raise that has not taken effect yet,
    /// else [`Self::access`].
    pub access_shown: FleetAccess,
    /// The level the title names while a click is fresh ([`LockFace::flash`]).
    pub access_flash: Option<FleetAccess>,
    /// The agent requests between this Mac and the others over the last hour, as the
    /// background reader last added the local record up (`crate::fleet_traffic`); `None` until
    /// its first look, and where no reader runs.
    pub traffic: Option<Summary>,
    /// This Mac's lock refused another Mac's request since the person last opened the menu
    /// ([`blocked_unseen`]): the lock on the bar wears the blocked mark.
    pub blocked: bool,
}

/// The menu-bar button's face: its title, and the lock beside it where one shows.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) struct ButtonFace {
    /// The title: the caret, and the level a fresh click chose.
    pub title: String,
    /// The lock's level; `None` out of a fleet, where the face is the caret alone.
    pub lock: Option<FleetAccess>,
    /// The lock wears the blocked mark ([`Glance::blocked`]).
    pub blocked: bool,
}

/// WHETHER THE BAR'S LOCK WEARS THE BLOCKED MARK at `now_ms`: this Mac's fleet access refused
/// another Mac's request (`traffic`'s latest refusal of an incoming one) later than the latest
/// one a menu the person opened showed (`seen_ms`, [`Glance::blocked_shown_ms`]; `0` where
/// none did), and that refusal is still within the summary's window. The mark so clears when
/// a menu that shows the refusal opens and when the refusal ages out, whichever is first; a
/// refusal that came after the open menu was painted keeps it. A request another Mac's lock
/// refused of this one never sets it: that is a row's mark in the menu alone.
#[must_use]
pub(crate) fn blocked_unseen(traffic: Option<&Summary>, seen_ms: u64, now_ms: u64) -> bool {
    traffic.is_some_and(|summary| {
        summary
            .last_blocked_in_ms
            .is_some_and(|at| at > seen_ms && now_ms.saturating_sub(at) < summary.window_ms)
    })
}

impl Glance {
    /// The latest refusal by this Mac's lock that the menu composed from this glance shows
    /// ([`compose_status_menu`]: its headline counts it), in Unix milliseconds: what a person
    /// who opens that menu has seen. `None` where the menu shows no traffic (out of a fleet,
    /// the record not read yet) or no such refusal.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn blocked_shown_ms(&self) -> Option<u64> {
        self.traffic
            .as_ref()
            .filter(|_| self.in_fleet)
            .and_then(|traffic| traffic.last_blocked_in_ms)
    }

    /// The menu-bar button's title is the caret, whatever happens: never a count or a mark
    /// that asks the person to act. The lock beside it ([`Self::button_face`]) is a state its
    /// person set.
    pub(crate) fn button_title(&self) -> String {
        "\u{276f}".into()
    }

    /// The button's face: the caret, in a fleet the lock at the level it shows
    /// ([`Self::access_shown`]) with the blocked mark where [`Self::blocked`], and while a
    /// click is fresh that level's short name after the caret (`❯ Read and Write`). Out of a
    /// fleet the level means nothing and no lock shows.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn button_face(&self) -> ButtonFace {
        let caret = self.button_title();
        if !self.in_fleet {
            return ButtonFace {
                title: caret,
                lock: None,
                blocked: false,
            };
        }
        ButtonFace {
            title: match self.access_flash {
                Some(level) => format!("{caret} {}", level.short()),
                None => caret,
            },
            lock: Some(self.access_shown),
            blocked: self.blocked,
        }
    }

    /// This Mac as the menu names it: its title, else `This Mac`.
    #[cfg(any(target_os = "macos", test))]
    fn name(&self) -> &str {
        self.machine.as_deref().unwrap_or("This Mac")
    }

    /// The button's hover: this Mac, and in a fleet its fleet access, that it blocked a
    /// request while the mark shows, and what each click does.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn tooltip(&self) -> String {
        let name = match &self.machine {
            Some(machine) => format!("aterm \u{b7} {machine}"),
            None => "aterm".into(),
        };
        if !self.in_fleet {
            return name;
        }
        let pending = if self.access_shown == self.access {
            String::new()
        } else {
            format!(", becoming {}", self.access_shown.short())
        };
        let blocked = if self.blocked {
            " It blocked a request from another Mac since this menu was last opened."
        } else {
            ""
        };
        format!(
            "{name} \u{2014} fleet access: {}{pending}.{blocked} Click to change it; right-click \
             for the menu.",
            self.access.short()
        )
    }

    /// What a screen reader says the button is: aterm, in a fleet its fleet access, and that a
    /// request was blocked while the mark shows.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn accessibility_label(&self) -> String {
        if !self.in_fleet {
            return "aterm".into();
        }
        let blocked = if self.blocked {
            ", a request blocked"
        } else {
            ""
        };
        format!("aterm, fleet access {}{blocked}", self.access_shown.short())
    }

    /// A cheap change fingerprint: the App hands the status item a new glance only when this
    /// differs from the last one handed over. It covers every field, the traffic included.
    pub(crate) fn fingerprint(&self) -> u64 {
        aterm_hash::fnv1a_64(format!("{self:?}").as_bytes())
    }
}

/// A fleet peer's row names its session by what that row carries: its typed role, else the title its person gave it, else its OSC title without the status glyph an agent leads it with ([`crate::app_take_over::title_without_agent_glyph`], the one strip of a spinner frame or Claude Code's idle `✳`, so the animation moves no name); `None` when all three are empty, and the row falls back to the peer's program (`fleet_glance::mac_of`).
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
        .map(|s| crate::needs::fold_clip(s, 40))
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

/// What stands in a grey line's image place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum Mark {
    /// No image: the line starts at the menu's edge.
    None,
    /// The image's room, empty: the line's words start where a marked line's do.
    Room,
    /// The blocked mark, the lock with a bar through it (`crate::status_lock_icon`): a fleet
    /// access refused a request this line counts.
    Blocked,
}

/// One row of the rendered status menu: the pure model the AppKit half only paints.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "macos", test))]
pub(crate) enum StatusRow {
    /// A grey line that cannot be clicked.
    Info {
        /// Its words.
        text: String,
        /// What stands before them.
        mark: Mark,
    },
    /// A separator.
    Separator,
    /// A clickable row carrying a [`StatusAction`].
    Action {
        /// The visible label.
        label: String,
        /// What a click posts.
        action: StatusAction,
        /// Drawn with a checkmark.
        checked: bool,
        /// Its hover, when it has one.
        tip: Option<String>,
    },
}

#[cfg(any(target_os = "macos", test))]
fn info(text: impl Into<String>, mark: Mark) -> StatusRow {
    StatusRow::Info {
        text: text.into(),
        mark,
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

/// The menu's first line: this Mac's name, its fleet and the network it is on
/// (`m9 Yellow Seahorse · ALab fleet · ALab LAN`), or out of a fleet the name and
/// `not in a fleet`.
#[cfg(any(target_os = "macos", test))]
fn identity_line(g: &Glance) -> String {
    const DOT: &str = " \u{b7} ";
    if !g.in_fleet {
        return format!("{}{DOT}not in a fleet", g.name());
    }
    let mut line = format!("{}{DOT}{} fleet", g.name(), crate::build_info::COMPANY);
    if let Some(network) = &g.network {
        line.push_str(DOT);
        line.push_str(network);
    }
    line
}

/// One Mac's requests one way, as its line says them: the Mac, the way, how many, what they
/// mostly were with anything blocked, and how long ago the latest was
/// (`m85 → 12 · 3 new jobs, 1 blocked · 20m`).
#[cfg(any(target_os = "macos", test))]
fn pair_line(pair: &aterm_types::fleet_activity::Pair, now_ms: u64) -> String {
    format!(
        "{} {} {} \u{b7} {} \u{b7} {}",
        pair.name(),
        pair.way.arrow(),
        pair.count(),
        pair.phrase(),
        aterm_types::fleet_activity::ago(now_ms, pair.last_ms)
    )
}

/// What `All activity…` is called, and what its hover says it does.
#[cfg(any(target_os = "macos", test))]
const ALL_ACTIVITY: (&str, &str) = (
    "All activity\u{2026}",
    "Opens this Mac's whole record of requests between Macs in a tab: what `aterm fleet \
     activity` prints.",
);

/// COMPOSE THE WHOLE STATUS MENU from a glance, its ages as of `now_ms` (Unix milliseconds):
/// pure, deterministic, and unit-testable off macOS
/// (`docs/DESIGN-fleet-access-lock-2026-11-10.md` §7). In order:
///
/// 1. the identity line ([`identity_line`]);
/// 2. in a fleet, the three levels, the one in force checked, each set by its pick
///    ([`StatusAction::SetFleetAccess`]);
/// 3. in a fleet, once the record was read, the last hour's traffic: its headline
///    (`Summary::headline`), marked where a lock refused anything; a line for each of the
///    busiest Macs and ways (`Summary::shown`), marked where a lock refused one of its
///    requests; and `All activity…`. An hour with no request is the headline alone
///    (`Quiet for 3h`).
///
/// Nothing else: no row asks the person to do anything, and none describes a session.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn compose_status_menu(g: &Glance, now_ms: u64) -> Vec<StatusRow> {
    let mut rows = vec![info(identity_line(g), Mark::None)];
    if !g.in_fleet {
        return rows;
    }
    rows.push(StatusRow::Separator);
    rows.extend(FleetAccess::ALL.into_iter().map(|level| StatusRow::Action {
        label: level.label().to_string(),
        action: StatusAction::SetFleetAccess(level),
        checked: level == g.access,
        tip: Some(access_tip(level).to_string()),
    }));
    let Some(traffic) = &g.traffic else {
        return rows;
    };
    rows.push(StatusRow::Separator);
    // The headline stands in the column of the lines under it: where any follow it keeps the
    // mark's room, so its words start where theirs do.
    let headline = if traffic.blocked() > 0 {
        Mark::Blocked
    } else if traffic.shown().next().is_some() {
        Mark::Room
    } else {
        Mark::None
    };
    rows.push(info(traffic.headline(now_ms), headline));
    if traffic.count() == 0 {
        return rows;
    }
    rows.extend(traffic.shown().map(|pair| {
        let mark = if pair.blocked.total() > 0 {
            Mark::Blocked
        } else {
            Mark::Room
        };
        info(pair_line(pair, now_ms), mark)
    }));
    rows.push(StatusRow::Action {
        label: ALL_ACTIVITY.0.to_string(),
        action: StatusAction::AllActivity,
        checked: false,
        tip: Some(ALL_ACTIVITY.1.to_string()),
    });
    rows
}

/// AN HOUR IN WHICH A LOCK REFUSED A REQUEST, ending at `now_ms`, as a summary: `m86` typed
/// into this Mac 3 times a minute ago, and `m3` asked it for 2 new jobs 35 minutes ago, both of
/// which this Mac's lock refused at Read Only. What a test and `objc_toolbar_drive` paint the
/// traffic rows and the blocked mark from.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn a_blocked_hour(now_ms: u64) -> Summary {
    use aterm_types::fleet_activity::{Outcome, Plain, Row, WINDOW_MS, Way, summarize};
    let row = |ago_min: u64, node: &str, host: &str, what, outcome, n| Row {
        at_ms: now_ms.saturating_sub(ago_min * 60_000),
        way: Way::In,
        node: node.to_owned(),
        host: Some(host.to_owned()),
        what,
        outcome,
        by: None,
        n,
    };
    summarize(
        &[
            row(
                35,
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "m3",
                Plain::NewJob,
                Outcome::Blocked {
                    level: FleetAccess::ReadOnly,
                    needs: FleetAccess::Full,
                },
                2,
            ),
            row(
                1,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "m86",
                Plain::Typed,
                Outcome::Admitted,
                3,
            ),
        ],
        now_ms,
        WINDOW_MS,
    )
}

/// A ROW'S PLACE IN THE COMPOSED MENU: its index among the menu's rows, separators and grey
/// lines counted (`aterm ctl chrome`'s `at=`, `aterm ctl person menubar press <at>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MenuAt(usize);

impl MenuAt {
    /// `at` read: plain decimal digits; `None` for anything else.
    pub(crate) fn parse(at: &str) -> Option<Self> {
        (!at.is_empty() && at.bytes().all(|b| b.is_ascii_digit()))
            .then(|| at.parse().ok())
            .flatten()
            .map(Self)
    }
}

impl std::fmt::Display for MenuAt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(any(target_os = "macos", test))]
const ROW_PATHS: bool = cfg!(any(test, debug_assertions));

/// The row `at` names in the menu `rows` composes, as [`menu_lines`] numbers them; `None`
/// where no row stands there.
#[cfg(any(test, all(target_os = "macos", debug_assertions)))]
pub(crate) fn row_at<'a>(rows: &'a [StatusRow], at: &MenuAt) -> Option<&'a StatusRow> {
    rows.get(at.0)
}

/// The menu as text (`aterm ctl chrome`'s `statusmenu` lines): `rows`, as
/// [`compose_status_menu`] composed them for the menu itself, one line per row in the order the
/// menu shows them. A grey line is `enabled=no`, with `blocked=yes` where it wears the blocked
/// mark; a row that can be clicked says whether it is checked, and its hover.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn menu_lines(rows: &[StatusRow]) -> Vec<String> {
    let yes = |on: bool| if on { "yes" } else { "no" };
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let at = if ROW_PATHS {
                format!(" at={}", MenuAt(i))
            } else {
                String::new()
            };
            match row {
                StatusRow::Info { text, mark } => format!(
                    "statusmenu {text:?} enabled=no{}{at}",
                    if *mark == Mark::Blocked {
                        " blocked=yes"
                    } else {
                        ""
                    }
                ),
                StatusRow::Separator => format!("statusmenu ---{at}"),
                StatusRow::Action {
                    label,
                    checked,
                    tip,
                    ..
                } => format!(
                    "statusmenu {label:?} enabled=yes checked={}{}{at}",
                    yes(*checked),
                    tip.as_ref()
                        .map_or_else(String::new, |tip| format!(" tip={tip:?}")),
                ),
            }
        })
        .collect()
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
    BarItem, StatusItemHandle, button_on_the_bar, face_on_the_bar, install, items_on_the_bar,
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

    // The loading lookup: a class of AppKit's is asked for by name whether or not anything
    // has loaded AppKit yet.
    use aterm_macos_ui::appkit::class;
    use aterm_macos_ui::status_button;
    use aterm_objc::{Bool, CGSize, Id, Obj, Retained, Sel, autoreleasepool, sel};
    use aterm_types::fleet_access::FleetAccess;
    use aterm_types::status_click::EventKey;
    use aterm_window::event_loop::EventLoopProxy;

    use super::{ButtonFace, ClickAct, Glance, Mark, StatusAction, StatusRow};
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
        /// The lock at each level, lowest first, plain and with the blocked mark, built once;
        /// `None` where one did not build, and the face then shows the caret alone.
        locks: [[Option<Obj>; 2]; 3],
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
        /// What a grey line holds before its words: the blocked mark, and the mark's room left
        /// empty ([`Mark`]). `None` where one did not build, and the line then holds no image.
        row_blocked: Option<Obj>,
        row_room: Option<Obj>,
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
        /// The latest refusal by this Mac's lock that the menu's last paint shows
        /// ([`Glance::blocked_shown_ms`]): what the open announces the person has seen.
        shown_blocked_ms: Cell<Option<u64>>,
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
                    .is_ok_and(|glance| glance.in_fleet);
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
                if self.ivars().popping.get() {
                    return;
                }
                self.paint_root(menu);
            }

            @sel(menuWillOpen:)
            fn menu_will_open(&self, _menu: Id) {
                if self.ivars().popping.get() {
                    return;
                }
                self.announce_open();
            }
        }
    }

    impl StatusTarget {
        /// Compose the menu from the item's glance and paint it into `menu`: it reads the
        /// glance the App last handed over and no file. Where the glance is borrowed nothing is
        /// painted, and the menu shows its last paint.
        fn paint_root(&self, menu: Id) {
            let now_ms = aterm_time::unix_millis().unwrap_or(0);
            let Ok((rows, shown_blocked_ms)) = self.ivars().glance.try_borrow().map(|glance| {
                (
                    super::compose_status_menu(&glance, now_ms),
                    glance.blocked_shown_ms(),
                )
            }) else {
                return;
            };
            self.ivars().shown_blocked_ms.set(shown_blocked_ms);
            autoreleasepool(|_| paint_menu(menu, self, rows));
        }

        /// Tell the App the menu is opening, with what its last paint showed the person.
        fn announce_open(&self) {
            let _ = self.ivars().proxy.send_event(Wake::StatusMenuOpening {
                seen_blocked_ms: self.ivars().shown_blocked_ms.get(),
            });
        }

        /// A menu click: paint the menu, announce its open (as `menuWillOpen:` does for a menu
        /// AppKit opens), and pop it up below the button, outside every borrow. Returns when
        /// the menu closes.
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
            self.paint_root(menu.id());
            self.announce_open();
            self.ivars().popping.set(true);
            // SAFETY: on the main thread (`main`), with the live menu and button retained here
            // across the call, and no borrow of the target's cells held: the menu's items send
            // `statusAction:`, which borrows them.
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
                row_blocked: row_image(true),
                row_room: row_image(false),
                bar: RefCell::new(None),
                stale: Cell::new(None),
                popping: Cell::new(false),
                shown_blocked_ms: Cell::new(None),
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
            let now_ms = aterm_time::unix_millis().unwrap_or(0);
            paint_menu(
                menu.id(),
                &target,
                super::compose_status_menu(glance, now_ms),
            );
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
                locks: FleetAccess::ALL
                    .map(|level| [lock_image(level, false), lock_image(level, true)]),
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

    /// `level`'s lock, with the blocked mark where `blocked`, as a template image holding a
    /// bitmap per scale (`crate::status_lock_icon`).
    fn lock_image(level: FleetAccess, blocked: bool) -> Option<Obj> {
        use crate::status_lock_icon as icon;
        let (width, height) = icon::SIZE_POINTS;
        let pngs = icon::SCALES.map(|scale| icon::png(level, blocked, scale));
        status_button::template_image(
            &pngs.each_ref().map(Vec::as_slice),
            CGSize { width, height },
            icon::description(level, blocked),
        )
    }

    /// What a grey line holds before its words, as a template image: the blocked mark
    /// (`status_lock_icon::ROW_MARK` with its bar) where `blocked`, else the mark's room with
    /// nothing drawn, which accessibility is told nothing of.
    fn row_image(blocked: bool) -> Option<Obj> {
        use crate::status_lock_icon as icon;
        let (width, height) = icon::SIZE_POINTS;
        let pngs = icon::SCALES.map(|scale| {
            if blocked {
                icon::png(icon::ROW_MARK, true, scale)
            } else {
                icon::blank_png(scale)
            }
        });
        status_button::template_image(
            &pngs.each_ref().map(Vec::as_slice),
            CGSize { width, height },
            if blocked {
                icon::ROW_MARK_DESCRIPTION
            } else {
                ""
            },
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

    fn show_face(handle: &StatusItemHandle, face: ButtonFace) {
        let image = face
            .lock
            .and_then(|level| handle.locks[level as usize][usize::from(face.blocked)].as_ref())
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

    fn paint_menu(menu: Id, target: &StatusTarget, rows: Vec<StatusRow>) {
        // SAFETY: `-removeAllItems` is `-(void)` on the live menu, on the main thread.
        unsafe { appkit::send_v(menu, sel!(removeAllItems)) };
        for row in rows {
            match row {
                StatusRow::Info { text, mark } => {
                    let image = match mark {
                        Mark::None => None,
                        Mark::Room => target.ivars().row_room.as_ref(),
                        Mark::Blocked => target.ivars().row_blocked.as_ref(),
                    };
                    add_info(menu, &text, image);
                }
                StatusRow::Separator => add_separator(menu),
                StatusRow::Action {
                    label,
                    action,
                    checked,
                    tip,
                } => add_action(menu, target, &label, action, checked, tip.as_deref()),
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
        checked: bool,
        tip: Option<&str>,
    ) {
        let Some(item) = action_item(target.as_id(), title, action, checked) else {
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
        checked: bool,
    ) -> Option<Obj> {
        let item = new_item(title, sel!(statusAction:))?;
        // SAFETY: plain setters on a fresh NSMenuItem — `-setTarget:` is `-(void)(id)` (AppKit holds the target WEAKLY, which is why the handle retains it; nil is "no target"), `-setTag:` and `-setState:` are `-(void)(NSInteger)` and `-setEnabled:` is `-(void)(BOOL)`.
        unsafe {
            appkit::send_v_id(item.id(), sel!(setTarget:), target);
            appkit::send_v_isize(item.id(), sel!(setTag:), action.tag());
            appkit::send_v_bool(item.id(), sel!(setEnabled:), true);
            if checked {
                appkit::send_v_isize(item.id(), sel!(setState:), NS_CONTROL_STATE_VALUE_ON);
            }
        }
        Some(item)
    }

    fn add_info(menu: Id, title: &str, image: Option<&Obj>) {
        let Some(item) = new_item(title, Sel::NULL) else {
            return;
        };
        // SAFETY: as `add_action`, on an item with no action; `-setImage:` is
        // `-(void)(NSImage *)` on the fresh item, handed a live image the target holds, which
        // the item retains.
        unsafe {
            appkit::send_v_bool(item.id(), sel!(setEnabled:), false);
            if let Some(image) = image {
                appkit::send_v_id(item.id(), sel!(setImage:), image.id());
            }
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
        /// Its `-image`'s `-accessibilityDescription` (`blocked` on a marked line, empty for
        /// the mark's room), `None` where it holds no image.
        pub(crate) image: Option<String>,
    }

    /// What the menu bar's real item holds as a person opens it: the menu the click handler pops
    /// up (the handle's, since the item has no `-menu`), repainted by its own delegate's
    /// `menuNeedsUpdate:` from the item's glance, each item in order. `objc_toolbar_drive` reads
    /// the first line, the levels and the traffic off it; empty off the main thread.
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
    /// it is checked and what its image is.
    fn items_of(menu: Id) -> Vec<BarItem> {
        // SAFETY: `menu` is a live NSMenu; `-numberOfItems` is `-(NSInteger)`, `-itemAtIndex:`
        // `-(NSMenuItem *)(NSInteger)` within its count, `-title` and `-toolTip`
        // `-(NSString *)` (nil where none was set), `-image` `-(NSImage *)` (nil where none)
        // and its `-accessibilityDescription` `-(NSString *)`, `-isEnabled` `-(BOOL)` and
        // `-state` `-(NSControlStateValue)`, an `NSInteger`, all without side effects on the
        // menu and its items.
        unsafe {
            let count = appkit::send_isize(menu, sel!(numberOfItems));
            let item_at: unsafe extern "C-unwind" fn(Id, Sel, isize) -> Id =
                aterm_objc::checked_msg();
            let string = |s: Id| (!s.is_null()).then(|| appkit::nsstring_to_rust(s));
            (0..count)
                .map(|i| {
                    let item = item_at(menu, sel!(itemAtIndex:), i);
                    let image = appkit::send_id(item, sel!(image));
                    BarItem {
                        title: appkit::nsstring_to_rust(appkit::send_id(item, sel!(title))),
                        enabled: appkit::send_bool(item, sel!(isEnabled)),
                        tip: string(appkit::send_id(item, sel!(toolTip))),
                        checked: appkit::send_isize(item, sel!(state)) == NS_CONTROL_STATE_VALUE_ON,
                        image: (!image.is_null()).then(|| {
                            string(appkit::send_id(image, sel!(accessibilityDescription)))
                                .unwrap_or_default()
                        }),
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
