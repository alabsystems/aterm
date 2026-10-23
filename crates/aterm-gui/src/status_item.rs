// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
    /// Overview… (and "and k more — Overview…"): the card of every session (`App::open_overview`).
    Overview,
    /// Read This Mac: `aterm ctl survey` into a document tab.
    ReadThisMac,
    /// Notify Me When I'm Needed: flip `desktop_alerts`.
    ToggleNotify,
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
    /// Read the Fleet: `aterm ctl survey --fleet` into a document tab.
    ReadFleet,
    /// Read the Fleet Now: a fresh fleet glance, whatever the last one's age.
    ReadFleetNow,
    /// The degrade line's click: the Local Network privacy pane.
    OpenLocalNetwork,
    /// A fleet need's Read This Session ([`Glance::peers`]).
    PeerRead(Pick),
    /// A fleet need's Screen Sharing to its Mac ([`Glance::peers`]).
    PeerScreen(Pick),
    /// A fleet need's Copy "ssh <its Mac>" ([`Glance::peers`]).
    PeerSsh(Pick),
    /// A fleet need's Take Over on its Mac — `aterm ctl --host <it> hold <sid> on`, the step-in — where that Mac lets this one drive it ([`PeerRef::drive`]).
    PeerTakeOver(Pick),
    /// A fleet need's Hand Back on its Mac: `aterm ctl --host <it> hold <sid> off`.
    PeerHandBack(Pick),
    /// A Fleet submenu Mac's Read in Full: the pick's generation is the fleet snapshot's ([`crate::fleet_glance::FleetSlot::generation`]), its index the Mac's.
    MacRead(Pick),
    /// A Fleet submenu Mac's Screen Sharing.
    MacScreen(Pick),
    /// A Fleet submenu Mac's Copy "ssh <it>".
    MacSsh(Pick),
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
            Self::ToggleNotify => 8,
            Self::HandBackAll => 9,
            Self::ReadFleet => 10,
            Self::ReadFleetNow => 11,
            Self::OpenLocalNetwork => 12,
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
        }
    }

    /// Inverse of [`tag`](Self::tag); unknown and retired tags are `None` (an inert item).
    pub(crate) fn from_tag(tag: isize) -> Option<Self> {
        match tag {
            5 => Some(Self::DismissExceptions),
            6 => Some(Self::Overview),
            7 => Some(Self::ReadThisMac),
            8 => Some(Self::ToggleNotify),
            9 => Some(Self::HandBackAll),
            10 => Some(Self::ReadFleet),
            11 => Some(Self::ReadFleetNow),
            12 => Some(Self::OpenLocalNetwork),
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
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// One session as the glance reads it, copied under the leaf locks by `App::status_session_row`: its tab label, its published verdict ([`crate::needs::Verdict`]), its agent word and its task record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionRow {
    /// Process-local session id.
    pub id: u64,
    /// The session's global sid (a task's `from=` names it).
    pub sid: String,
    /// The tab's label as every Agents surface names it (`App::tab_label_of_session`): its role, else its title pin, else the strip's own rungs (the live OSC title without the status glyph an agent leads it with, then the cwd), else the tab's position, else the short sid.
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
    /// The task record, for a worker.
    pub task: Option<TaskRow>,
    /// When the need was first published (unix seconds, [`Herald::observe`]).
    pub need_since: Option<i64>,
    /// When the hold was first seen standing (unix seconds).
    pub held_since: Option<i64>,
    /// How many writes aimed at the tab the person's hold refused since it was set (`SessionFabric::person_refusals`); `0` with no person hold.
    pub person_refusals: u64,
    /// The Mac that holds the tab over the fleet (the step-in's remote slot), by the name this Mac's book gives it.
    pub held_by_host: Option<String>,
    /// Its supervised agent's task end, while one stands on the harness's glass (`harness_glass::EndGlass`).
    pub end: Option<EndRow>,
    /// Its Autopilot chip's face is LANDED (`ChipFace::Landed`: under Land, all of the work on the remote main branch), seen or not, whether or not an end was told: This Mac counts it `landed`, a worker or a plain Land tab.
    pub landed: bool,
    /// It is LANDING (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.4.3 item 1): its Autopilot is Land, its chip wears `⤓ land` (the task open or unknown), and a hold stands — Land's instruction has been typed to it (`App::shown_hold`).
    pub landing: bool,
    /// What holds the landing, where a land act's yield did not land or nudges were typed: its worker row's tip, in the hold's words with no clock (`not landed yet: 2 unpushed commits · 3 nudges`, `harness_glass::hold_words`' tip form) — the menu keeps ages, composed at paint, and never a clock that would go stale.
    pub landing_tip: Option<String>,
    /// Where its tab is (`in tab 3`, `in tab 3, left pane`; `App::session_place`), filled by the glance's first pass only where another row shares its label: every row that names the session says it after the label ([`Self::named`]).
    pub place: Option<String>,
}

impl SessionRow {
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
}

/// The stall half of a session's need facts — `input_stall::menu_row`'s words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputStallRow {
    /// The server's own attention words for the stall (`<prog> is frozen: not reading input since <HH:MM> (…) — restart it: …`), as its `meta attention=` entry carries them.
    pub text: String,
    /// The episode's identity (a hash of when its oldest unread byte was accepted): one notification per stall, however its text moves.
    pub key: u64,
    /// The job is stopped with input queued (`signal cont` resumes it), not frozen: the notification's title says which.
    pub stopped: bool,
}

/// What an escalation row is about: the need's word in the herald's coarser vocabulary (its log word and key spelling), most severe first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum EscalationKind {
    /// The program has left its input unread, or its job is stopped with input queued ([`NeedWord::Frozen`], [`NeedWord::Stopped`]).
    Unresponsive,
    /// The worker's task is stalled ([`NeedWord::Stalled`]).
    Stalled,
    /// Typed attention of kind ask ([`NeedWord::Ask`]).
    Attention,
    /// An approval box nobody presses ([`NeedWord::Approval`]).
    Prompt,
    /// A question nobody answers ([`NeedWord::Question`]).
    Question,
    /// A wall nobody answers: a lost login, the memory banner nobody restarts, any other wall ([`NeedWord::Login`], [`NeedWord::Memory`], [`NeedWord::Wall`]).
    Wall,
}

impl EscalationKind {
    /// The herald's kind for a need word.
    pub(crate) const fn of(word: NeedWord) -> Self {
        match word {
            NeedWord::Frozen | NeedWord::Stopped => Self::Unresponsive,
            NeedWord::Stalled => Self::Stalled,
            NeedWord::Ask => Self::Attention,
            NeedWord::Approval => Self::Prompt,
            NeedWord::Question => Self::Question,
            NeedWord::Login | NeedWord::Memory | NeedWord::Wall => Self::Wall,
        }
    }

    const fn log_word(self) -> &'static str {
        match self {
            Self::Unresponsive => "unresponsive",
            Self::Stalled => "stalled",
            Self::Attention => "attention",
            Self::Prompt => "prompt",
            Self::Question => "question",
            Self::Wall => "wall",
        }
    }
}

/// The notification's title for a need — aterm's own words, the word's: a stopped job is not frozen, and an agent under the memory banner has not stopped (Claude Code draws it while the agent still runs).
pub(crate) const fn headline(word: NeedWord) -> &'static str {
    match word {
        NeedWord::Frozen => "aterm \u{00b7} program frozen",
        NeedWord::Stopped => "aterm \u{00b7} program stopped",
        NeedWord::Stalled => "aterm \u{00b7} worker stalled",
        NeedWord::Login | NeedWord::Wall => "aterm \u{00b7} agent stopped",
        NeedWord::Memory => "aterm \u{00b7} agent memory critical",
        NeedWord::Ask => "aterm \u{00b7} needs you",
        NeedWord::Approval => "aterm \u{00b7} approval waiting",
        NeedWord::Question => "aterm \u{00b7} question waiting",
    }
}

/// One session's escalation: its published need, as the menu row and the herald read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Escalation {
    /// Process-local session id, or a fleet need's key (`fleet_glance::herald_key`, which no local id reaches).
    pub session: u64,
    /// What it is about.
    pub kind: EscalationKind,
    /// The need it is: what a seamless update's carry is matched by ([`Herald::note`]) — a kind holds several words (a login, a memory wall and any other wall are all [`EscalationKind::Wall`]), and each is its own need.
    pub word: NeedWord,
    /// The transition's identity within `(session, kind)` ([`crate::needs::Need::key`]): the same need re-read is the same key; a new one is a new key.
    pub key: u64,
    /// The need row's words without its age: `<tab label> — <what>`.
    pub label: String,
    /// The notification body: `<tab title>: <what>`.
    pub body: String,
    /// The notification title ([`headline`]).
    pub headline: &'static str,
}

const ROW_TITLE_MAX: usize = 32;

pub(crate) use crate::needs::{fold_clip, wall_words};

/// A banner body about one tab: `<tab title>: <what>`, the title stripped of status glyphs and clipped like a menu row; `what` alone when the title is empty.
pub(crate) fn tab_body(title: &str, what: &str) -> String {
    let title = fold_clip(stripped_title(title), ROW_TITLE_MAX);
    if title.is_empty() {
        what.to_string()
    } else {
        format!("{title}: {what}")
    }
}

fn body_words(need: &crate::needs::Need) -> String {
    need.harness.as_ref().map_or_else(
        || need.what.clone(),
        |ask| ask.line(crate::needs::NEED_WHAT_MAX + ask.more_words().len()),
    )
}

/// Adapter from a row's published need to the herald's escalation: every notice names its tab, attention included.
pub(crate) fn escalation(row: &SessionRow) -> Option<Escalation> {
    let need = row.need.as_ref()?;
    let body = tab_body(&row.title, &body_words(need));
    let label = need_label(&row.title, None, &need.what);
    Some(Escalation {
        session: row.id,
        kind: EscalationKind::of(need.word),
        word: need.word,
        key: need.key,
        label,
        body,
        headline: headline(need.word),
    })
}

/// The shortest spacing between two notifications for one session: a flapping verdict costs one notification per floor, never one per flap.
pub(crate) const NOTIFY_SESSION_FLOOR: std::time::Duration = std::time::Duration::from_secs(20);
/// At most [`NOTIFY_BURST`] notifications per [`NOTIFY_BURST_WINDOW`] across the instance: ten agents hitting one usage limit page a few times, not ten.
pub(crate) const NOTIFY_BURST: usize = 3;
/// The window [`NOTIFY_BURST`] is counted over.
pub(crate) const NOTIFY_BURST_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a session whose notice `notify.rs`'s queue refused ([`Herald::refused`]) waits before that notice is tried again; no other session waits on it.
pub(crate) const NOTIFY_RETRY: std::time::Duration = std::time::Duration::from_secs(5);

/// One native notification the herald decided to post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeraldNotice {
    /// The session it is about (the delivery thread's focus suppression).
    pub session: u64,
    /// aterm's own headline for the row ([`Escalation::headline`]).
    pub title: &'static str,
    /// `<tab title>: <what>`.
    pub body: String,
    decided: (),
}

/// What one [`Herald::note`] decided.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HeraldOutcome {
    /// The session's menu row appeared, changed or went away — re-render.
    pub row_moved: bool,
    /// Post exactly this notification.
    pub notice: Option<HeraldNotice>,
    /// This note met a new `(kind, key)`: not a re-read of the one shown, and not the timer re-heralding an owed notice ([`Herald::due`]).
    pub transition: bool,
    /// The transitions awaiting a notice when this note decided, its own included: what a post pays (`aterm.log`'s `(+n-1 more)`), what [`HeraldQuiet::Limited`] still owes (`1` on the FIRST hold), what [`HeraldQuiet::Looking`] or [`HeraldQuiet::Disabled`] spent.
    pub owed: u32,
    /// This quiet decision — [`HeraldQuiet::Looking`], [`HeraldQuiet::Disabled`], or [`HeraldQuiet::Limited`]'s FIRST hold (a transition leaving one notice owed) — is its session's first since the later of its last post and a [`NOTIFY_SESSION_FLOOR`] ago: the one `aterm.log` records at `info` ([`herald_log_line`]).
    pub first_quiet: bool,
}

/// Why a transition into an escalation posted nothing (test-visible).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeraldQuiet {
    /// The same `(kind, key)` as the last note — the same box re-read.
    Same,
    /// The human is looking at the tab in the active app.
    Looking,
    /// Desktop alerts are off.
    Disabled,
    /// [`NOTIFY_SESSION_FLOOR`] or [`NOTIFY_BURST`] held it back, or the notifier's queue refused this session's notice less than [`NOTIFY_RETRY`] ago (another session's refusal holds nothing here): it is owed, and posted (coalesced) when the limit allows ([`Herald::due`]).
    Limited,
}

#[derive(Clone, Debug, Default)]
struct HeraldSlot {
    shown: Option<(EscalationKind, u64)>,
    label: Option<String>,
    notified_at: Option<std::time::Instant>,
    notified_before: Option<std::time::Instant>,
    retry_at: Option<std::time::Instant>,
    owed: u32,
    quiet_logged_at: Option<std::time::Instant>,
}

impl HeraldSlot {
    const fn suppress_desktop_notice(&mut self) {
        self.owed = 0;
        self.retry_at = None;
    }

    fn first_quiet(&mut self, now: std::time::Instant) -> bool {
        let open = self
            .quiet_logged_at
            .is_none_or(|at| now.saturating_duration_since(at) >= NOTIFY_SESSION_FLOOR);
        if open {
            self.quiet_logged_at = Some(now);
        }
        open
    }
}

/// The escalation herald: per session, folds the current [`escalation`] into "did the menu row move" and "post one notification".
#[derive(Debug, Default)]
pub(crate) struct Herald {
    slots: std::collections::HashMap<u64, HeraldSlot>,
    recent: std::collections::VecDeque<std::time::Instant>,
    last_quiet: Option<HeraldQuiet>,
    since: std::collections::HashMap<u64, Since>,
    adopted: std::collections::HashMap<u64, Adopted>,
}

/// How long a seamless successor takes what its outgoing process had told the owner as already told ([`Herald::adopt`], and `App::herald_fleet`'s first fresh read): room for every standing need to be published again — the stall watch's re-detection (`input_backlog::STALL_AFTER`, stretched under load, then `input_stall::RECHECK`) and an issuer's first fleet read (`fleet_glance::POLL_EVERY` and its timeout).
pub(crate) const ADOPT_SETTLE: std::time::Duration = std::time::Duration::from_secs(120);

/// What the status item knew of one session, carried across a seamless update as one row (`SessionRecord::status_seen`): the published need and since when it stood, whether the herald had told it (posted, or spent it on a looking owner or alerts off: nothing owed), the hold and since when, and the done the owner looked at, by when its task entered it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusCarry {
    /// The need's word and since when it stood (unix seconds).
    pub need: Option<(NeedWord, i64)>,
    /// The herald told that need: nothing is owed for it.
    pub told: bool,
    /// The hold's word and since when it stood (unix seconds).
    pub held: Option<(HoldWord, i64)>,
    /// The done the owner looked at: its task's `since_unix_ms`, the wall-clock instant the record entered it (carried exactly, unlike its entry rev).
    pub done: Option<u64>,
}

impl StatusCarry {
    /// The row: `need=<word>@<unix s>`, `told=1`, `hold=<word>@<unix s>` and `done=<unix ms>`, each only when it holds; `None` for nothing known.
    #[cfg(any(unix, test))]
    pub(crate) fn row(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some((word, at)) = self.need {
            parts.push(format!("need={}@{at}", word.word()));
            if self.told {
                parts.push("told=1".to_string());
            }
        }
        if let Some((word, at)) = self.held {
            parts.push(format!("hold={}@{at}", word.word()));
        }
        if let Some(ms) = self.done {
            parts.push(format!("done={ms}"));
        }
        (!parts.is_empty()).then(|| parts.join(" "))
    }

    /// Read a carried row back.
    pub(crate) fn parse(row: &str) -> Self {
        let stamped = |value: &str| {
            let (word, at) = value.split_once('@')?;
            Some((word.to_string(), at.parse::<i64>().ok()?))
        };
        let mut carry = Self::default();
        for (key, value) in row.split_whitespace().filter_map(|t| t.split_once('=')) {
            match key {
                "need" => {
                    carry.need =
                        stamped(value).and_then(|(w, at)| Some((NeedWord::parse(&w)?, at)));
                }
                "told" => carry.told = value == "1",
                "hold" => {
                    carry.held =
                        stamped(value).and_then(|(w, at)| Some((HoldWord::parse(&w)?, at)));
                }
                "done" => carry.done = value.parse().ok(),
                _ => {}
            }
        }
        carry.told &= carry.need.is_some();
        carry
    }
}

#[derive(Clone, Copy, Debug)]
struct Adopted {
    need: Option<(NeedWord, i64)>,
    told: Option<NeedWord>,
    held: Option<(HoldWord, i64)>,
    until_unix: i64,
    until: std::time::Instant,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Since {
    need: Option<(NeedWord, u64, i64)>,
    held: Option<(HoldWord, i64)>,
    shown: u64,
}

fn shown_key(row: &SessionRow) -> u64 {
    let class = match row.agent {
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

impl Herald {
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

    /// A seamless update adopted `session` with what its outgoing process knew of it (`carry`, `SessionRecord::status_seen`), at `now_unix` / `now`: for [`ADOPT_SETTLE`], its first need published here takes the carried age when it is the carried word ([`Self::observe`]), and is no transition when it is the need the herald had told ([`Self::note`]) — an update is no new need, and the owner was told once.
    pub(crate) fn adopt(
        &mut self,
        session: u64,
        carry: StatusCarry,
        now_unix: i64,
        now: std::time::Instant,
    ) {
        let settle = i64::try_from(ADOPT_SETTLE.as_secs()).unwrap_or(i64::MAX);
        self.adopted.insert(
            session,
            Adopted {
                need: carry.need,
                told: carry
                    .told
                    .then_some(carry.need.map(|(word, _)| word))
                    .flatten(),
                held: carry.held,
                until_unix: now_unix.saturating_add(settle),
                until: now + ADOPT_SETTLE,
            },
        );
    }

    /// What this herald knows of `session` for the next process ([`StatusCarry`]; the done is the App's): the need and hold it last observed and since when, and whether the need it shows is told — noted and nothing owed for it.
    #[cfg(any(unix, test))]
    pub(crate) fn carry_of(&self, session: u64) -> StatusCarry {
        let since = self.since.get(&session).copied().unwrap_or_default();
        let told = since
            .need
            .is_some_and(|(word, key, _)| self.told(session, EscalationKind::of(word), key));
        StatusCarry {
            need: since.need.map(|(word, _, at)| (word, at)),
            told,
            held: since.held,
            done: None,
        }
    }

    /// Whether `session`'s notice for the need `(kind, key)` is shown and owed nothing: told (or looked at, or held back by the setting, which owe nothing either) — what a seamless update's carry calls told.
    #[cfg(any(unix, test))]
    pub(crate) fn told(&self, session: u64, kind: EscalationKind, key: u64) -> bool {
        self.slots
            .get(&session)
            .is_some_and(|slot| slot.shown == Some((kind, key)) && slot.owed == 0)
    }

    /// Since when `session`'s need has stood ([`Self::observe`]).
    pub(crate) fn need_since(&self, session: u64) -> Option<i64> {
        self.since.get(&session)?.need.map(|(_, _, at)| at)
    }

    /// Since when `session`'s hold has stood ([`Self::observe`]).
    pub(crate) fn held_since(&self, session: u64) -> Option<i64> {
        self.since.get(&session)?.held.map(|(_, at)| at)
    }

    /// Fold `session`'s current escalation.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DesktopAlertDebt",
            action = "ObserveNewLimited",
            project = "aterm_gui::status_item::desktop_alert_debt_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DesktopAlertDebt",
            action = "ObserveNewFree",
            project = "aterm_gui::status_item::desktop_alert_debt_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DesktopAlertDebt",
            action = "Reobserve",
            project = "aterm_gui::status_item::desktop_alert_debt_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DesktopAlertDebt",
            action = "Tick",
            project = "aterm_gui::status_item::desktop_alert_debt_conformance::Rig::project"
        )
    )]
    pub(crate) fn note(
        &mut self,
        session: u64,
        current: Option<&Escalation>,
        looking: bool,
        desktop_alerts: bool,
        now: std::time::Instant,
    ) -> HeraldOutcome {
        self.last_quiet = None;
        if current.is_none() && !self.slots.contains_key(&session) {
            // Nothing escalates and nothing did: keep no slot, so a retired session's late
            // refresh cannot re-grow the map.
            return HeraldOutcome::default();
        }
        let slot = self.slots.entry(session).or_default();
        let ident = current.map(|e| (e.kind, e.key));
        let label = current.map(|e| e.label.clone());
        let row_moved = slot.label != label;
        slot.label = label;
        let Some(esc) = current else {
            slot.shown = None;
            slot.owed = 0;
            return HeraldOutcome {
                row_moved,
                ..HeraldOutcome::default()
            };
        };
        if let Some(told) = self
            .adopted
            .get_mut(&session)
            .filter(|a| now < a.until)
            .and_then(|a| a.told.take())
            && told == esc.word
        {
            slot.shown = ident;
        }
        let transition = slot.shown != ident;
        if transition {
            slot.shown = ident;
            slot.owed += 1;
        }
        let owed = slot.owed;
        let quiet = |herald: &mut Self, why, first_quiet| {
            herald.last_quiet = Some(why);
            HeraldOutcome {
                row_moved,
                notice: None,
                transition,
                owed,
                first_quiet,
            }
        };
        if owed == 0 {
            // The same box re-read, and nothing owed for it.
            return quiet(self, HeraldQuiet::Same, false);
        }
        if looking {
            slot.owed = 0;
            let first_quiet = slot.first_quiet(now);
            return quiet(self, HeraldQuiet::Looking, first_quiet);
        }
        if !desktop_alerts {
            slot.suppress_desktop_notice();
            let first_quiet = slot.first_quiet(now);
            return quiet(self, HeraldQuiet::Disabled, first_quiet);
        }
        let (notified_at, retry_at) = (slot.notified_at, slot.retry_at);
        while self
            .recent
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= NOTIFY_BURST_WINDOW)
        {
            self.recent.pop_front();
        }
        if Self::free_at(&self.recent, notified_at, retry_at, now) > now {
            let first_quiet = transition && owed == 1 && slot.first_quiet(now);
            return quiet(self, HeraldQuiet::Limited, first_quiet);
        }
        let Some(slot) = self.slots.get_mut(&session) else {
            return HeraldOutcome::default();
        };
        slot.owed = 0;
        slot.notified_before = slot.notified_at;
        slot.notified_at = Some(now);
        slot.retry_at = None;
        slot.quiet_logged_at = None;
        self.recent.push_back(now);
        HeraldOutcome {
            row_moved,
            notice: Some(HeraldNotice {
                session,
                title: esc.headline,
                body: esc.body.clone(),
                decided: (),
            }),
            transition,
            owed,
            first_quiet: false,
        }
    }

    /// `notify.rs`'s queue refused the notice [`Self::note`] just posted for `session`, which paid `owed` transitions ([`HeraldOutcome::owed`]): it was full — it is shared with program notifications — or its thread was gone (`App::post_herald_notice`).
    pub(crate) fn refused(&mut self, session: u64, owed: u32, now: std::time::Instant) {
        let Some(slot) = self.slots.get_mut(&session) else {
            return;
        };
        slot.retry_at = Some(now + NOTIFY_RETRY);
        let Some(posted) = slot.notified_at else {
            return;
        };
        if let Some(at) = self.recent.iter().rposition(|t| *t == posted) {
            self.recent.remove(at);
        }
        slot.notified_at = slot.notified_before.take();
        slot.owed += owed;
    }

    /// Turning desktop alerts off cancels pending rate-limit and refusal retries, keeping the labels and transition keys the menu uses.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DesktopAlertDebt",
            action = "Disable",
            project = "aterm_gui::status_item::desktop_alert_debt_conformance::Rig::project"
        )
    )]
    pub(crate) fn suppress_pending_desktop_notices(&mut self) {
        for slot in self.slots.values_mut() {
            slot.suppress_desktop_notice();
        }
    }

    fn free_at(
        recent: &std::collections::VecDeque<std::time::Instant>,
        notified_at: Option<std::time::Instant>,
        retry_at: Option<std::time::Instant>,
        now: std::time::Instant,
    ) -> std::time::Instant {
        let live: Vec<std::time::Instant> = recent
            .iter()
            .copied()
            .filter(|t| now.saturating_duration_since(*t) < NOTIFY_BURST_WINDOW)
            .collect();
        let floor = notified_at.map_or(now, |t| t + NOTIFY_SESSION_FLOOR);
        let burst = if live.len() >= NOTIFY_BURST {
            live[live.len() - NOTIFY_BURST] + NOTIFY_BURST_WINDOW
        } else {
            now
        };
        let retry = retry_at.unwrap_or(now);
        floor.max(burst).max(retry).max(now)
    }

    /// The sessions whose owed notice the limit now allows — the App's timer re-heralds each ([`Self::note`] with its current escalation), and the earliest instant any other owed notice becomes allowed (its next wake).
    pub fn due(&self, now: std::time::Instant) -> (Vec<u64>, Option<std::time::Instant>) {
        let mut ready = Vec::new();
        let mut next: Option<std::time::Instant> = None;
        for (session, slot) in self.slots.iter().filter(|(_, slot)| slot.owed > 0) {
            let (session, notified_at) = (*session, slot.notified_at);
            let at = Self::free_at(&self.recent, notified_at, slot.retry_at, now);
            if at <= now {
                ready.push(session);
            } else if next.is_none_or(|n| at < n) {
                next = Some(at);
            }
        }
        ready.sort_unstable();
        (ready, next)
    }

    /// Why the last [`Self::note`] posted nothing, when it had a transition or a re-read to judge; `None` after a post or a cleared escalation.
    pub(crate) const fn last_quiet(&self) -> Option<HeraldQuiet> {
        self.last_quiet
    }

    /// `session` cannot be told of now — a fleet need whose Mac did not answer, or whose last read stopped counting (`App::herald_fleet`): a notice it owes is dropped, since no read can pay it and the App's timer ([`Self::due`]) would ask again at once, and what it shows is kept, so the same need read again is no new transition.
    pub(crate) fn forgo(&mut self, session: u64) {
        if let Some(slot) = self.slots.get_mut(&session) {
            slot.suppress_desktop_notice();
        }
    }

    /// Forget a retired session.
    pub(crate) fn retire(&mut self, session: u64) {
        self.slots.remove(&session);
        self.since.remove(&session);
        self.adopted.remove(&session);
    }
}

/// What became of a [`Herald::note`]'s notice ([`herald_log_line`]'s `post`), decided before its line is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeraldPost {
    /// `notify.rs`'s queue took it — or the note owed no notice at all.
    Queued,
    /// The queue refused it, and why (`App::post_herald_notice`'s `Err`).
    Refused(&'static str),
    /// `desktop_alerts` is off: nothing was offered to the queue.
    Suppressed,
}

/// The one `aterm.log` line a [`Herald::note`] earns, as `(level, text)`, or `None` when it decided nothing (nothing escalates, or the escalation cleared).
pub(crate) fn herald_log_line(
    session: u64,
    current: Option<&Escalation>,
    outcome: &HeraldOutcome,
    quiet: Option<HeraldQuiet>,
    post: HeraldPost,
) -> Option<(aterm_log::Level, String)> {
    use aterm_log::Level;
    let esc = current?;
    let (level, decision) = if outcome.notice.is_some() {
        let held = if outcome.transition {
            ""
        } else {
            " after the rate limit"
        };
        let more = match outcome.owed.saturating_sub(1) {
            0 => String::new(),
            more => format!(" (+{more} more)"),
        };
        match post {
            HeraldPost::Queued => (Level::Info, format!("queued{held}{more}")),
            HeraldPost::Refused(why) => (Level::Warn, format!("not queued{held}{more}: {why}")),
            HeraldPost::Suppressed => (
                Level::Info,
                format!("not queued{held}{more}: suppressed by desktop_alerts = false"),
            ),
        }
    } else {
        let first = if outcome.first_quiet {
            Level::Info
        } else {
            Level::Debug
        };
        match quiet? {
            HeraldQuiet::Looking => (first, "not queued: looking".to_string()),
            HeraldQuiet::Disabled => (
                first,
                "not queued: suppressed by desktop_alerts = false".to_string(),
            ),
            HeraldQuiet::Limited if outcome.first_quiet => {
                (Level::Info, "not queued: limited".to_string())
            }
            HeraldQuiet::Limited => (
                Level::Debug,
                format!("not queued: limited, {} owed", outcome.owed),
            ),
            HeraldQuiet::Same => (Level::Debug, "not queued: same".to_string()),
        }
    };
    Some((
        level,
        format!(
            "escalation notification for session {session} ({} {}): {decision}",
            esc.kind.log_word(),
            herald_key_words(esc.kind, esc.key)
        ),
    ))
}

fn herald_key_words(kind: EscalationKind, key: u64) -> String {
    match kind {
        EscalationKind::Prompt | EscalationKind::Question | EscalationKind::Wall => {
            format!("agent_rev={key}")
        }
        EscalationKind::Attention => format!("text_key={key:#018x}"),
        EscalationKind::Unresponsive => format!("episode_key={key:#018x}"),
        EscalationKind::Stalled => format!("task_rev={key}"),
    }
}

/// The most need rows the menu spells out; the rest are counted on one "and k more — Overview…" row.
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
    /// A harness's need in its own words ([`crate::needs::Need::harness`]): the row says it clipped to the room its label and age leave ([`need_line`]).
    pub harness: Option<crate::needs::HarnessAsk>,
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
    /// Agents at rest.
    pub resting: usize,
    /// Sessions that LANDED: every session whose chip's face is landed (a plain Land tab included), and every worker whose record reads `done why=landed`.
    pub landed: usize,
    /// Workers whose task is done by an older build's rule (`why=marker|done-check`), which no harness writes any more, or by a hand's `aterm ctl task mark done` with no `why=landed`.
    pub done: usize,
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
    /// Since when (unix milliseconds).
    pub since_unix_ms: u64,
    /// Its result's size.
    pub result_bytes: u64,
    /// Go to it ([`Glance::go`]).
    pub go: u16,
    /// Read its result ([`Glance::reads`]), for a done worker with one.
    pub read: Option<u16>,
    /// Its session is landing ([`SessionRow::landing`]): Land's instruction was typed to it and a hold stands.
    pub landing: bool,
    /// Its row's tip while it is landing ([`SessionRow::landing_tip`]).
    pub tip: Option<String>,
}

const WORKER_WORDS: [&str; 6] = ["landing", "working", "landed", "done", "exited", "stalled"];

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
    /// That Mac lets this one drive it (its survey said `drive=1`: its person ran `aterm fleet allow drive --from <this Mac>` there): Take Over and Hand Back on it, over the fleet.
    pub drive: bool,
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
        /// The last good read: how many Macs answered of how many asked, how many agents they run, and when it was read (unix ms).
        read: Option<(usize, usize, usize, u64)>,
        /// A read is in flight.
        reading: bool,
        /// Why the last read failed, when it did.
        failed: Option<String>,
        /// Every peer refused with no route.
        no_route: bool,
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
    /// Done for you, newest first.
    pub done: Vec<DoneRow>,
    /// Waiting, one row per cause.
    pub waiting: Vec<WaitRow>,
    /// The held tabs.
    pub held: Vec<HeldRow>,
    /// The sessions on other Macs this Mac holds over the fleet.
    pub held_there: Vec<HeldThereRow>,
    /// The digest's entries — what needs a look, not now — `(log id, subject)` per entry, worst first.
    pub exceptions: Vec<(u64, String)>,
    /// `desktop_alerts`: the Notify Me checkmark.
    pub alerts: bool,
    /// Whose banner those notices arrive as here (`notify::own_voice`): the Notify Me row's hover.
    pub voice: Option<NoticeVoice>,
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
}

/// Whose banner aterm's own notices arrive as on this Mac (`notify::own_voice`, macOS): what the Notify Me row's hover says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NoticeVoice {
    /// What macOS has said about aterm posting as itself (`notice_center`): granted, a notice is aterm's own and a click goes to the tab it is about; otherwise a bundle posts none.
    pub center: crate::notice_center::Center,
    /// What a notice is where there is no center to post through (`center` is `Absent`): none from a bundle, a subprocess's banner from a bare binary.
    pub fallback: NoticeFallback,
}

/// What a notice is where there is no center to post through (`notify::route`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeFallback {
    /// None: aterm runs from an app bundle, which posts only as itself, and it has no center.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "UNUserNotificationCenter is macOS's")
    )]
    Withheld {
        /// Whether the running bundle's id was read.
        named: bool,
    },
    /// terminal-notifier, speaking as itself: a bare binary has no bundle id to speak for.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "terminal-notifier is macOS's")
    )]
    Notifier,
    /// `osascript`: Script Editor's banner, and a click opens Script Editor — a bare binary's, without terminal-notifier.
    ScriptEditor,
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
        for row in rows {
            let Some(need) = &row.need else { continue };
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
                    what: need_words(need.word).to_string(),
                    harness: None,
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
        g.needs.sort_by(|a, b| {
            (
                !a.counted,
                a.word.severity(),
                a.origin,
                a.since_unix.unwrap_or(i64::MAX),
                &a.label,
            )
                .cmp(&(
                    !b.counted,
                    b.word.severity(),
                    b.origin,
                    b.since_unix.unwrap_or(i64::MAX),
                    &b.label,
                ))
        });
        for row in rows {
            let landed = |t: &TaskRow| {
                t.state == TaskState::Done && t.why == Some(crate::task_record::WHY_LANDED)
            };
            match (&row.task, row.agent) {
                _ if row.landed => g.here.landed += 1,
                (Some(t), _) if landed(t) => g.here.landed += 1,
                (Some(t), _) if t.state == TaskState::Done => g.here.done += 1,
                (Some(t), _) if t.state == TaskState::Exited => g.here.exited += 1,
                _ if row.need.is_some() => g.here.need_you += 1,
                _ if row.waiting.is_some() => g.here.waiting += 1,
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
            let read = (task.state == TaskState::Done && task.result_bytes > 0)
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
            let worker = WorkerRow {
                label: row.named(),
                state: task.state,
                why: task.why,
                since_unix_ms: task.since_unix_ms,
                result_bytes: task.result_bytes,
                go: worker_go,
                read,
                landing: row.landing,
                tip: row.landing.then(|| row.landing_tip.clone()).flatten(),
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
                t.state == TaskState::Done
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
            done.push(DoneRow {
                label: row.named(),
                since_unix_ms: task.since_unix_ms,
                result_bytes: task.result_bytes,
                at,
                landed: task.why == Some(crate::task_record::WHY_LANDED)
                    || row.end.is_some()
                    || row.landed,
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
            if waited_on {
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
            no_route: fleet.snapshot.as_ref().is_some_and(|s| s.no_route),
            member_of: fleet.book.issuer_label.clone(),
            unbound_issuer: fleet.book.unbound_issuer.clone(),
        }
    }

    /// How many needs the badge counts: this Mac's, and the fleet's from a fresh read.
    pub(crate) fn need_count(&self) -> usize {
        self.needs.iter().filter(|n| n.counted).count()
    }

    /// Whether any counted need is stuck: the title is red.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
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

    /// The menu-bar button title: `❯N` while N needs stand, else `❯•` while the digest holds entries or a row stands under Done for you (a worker's done, a supervised agent's end nobody has looked at), else `❯`.
    pub(crate) fn button_title(&self) -> String {
        match self.need_count() {
            0 if !self.exceptions.is_empty() || !self.done.is_empty() => "\u{276f}\u{2022}".into(),
            0 => "\u{276f}".into(),
            n => format!("\u{276f}{n}"),
        }
    }

    /// The button's hover: how many need you and where, or that nothing does, and how much of the fleet the last read reached.
    #[cfg_attr(
        not(target_os = "macos"),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn tooltip(&self) -> String {
        let count = |origin: fn(&Origin) -> bool| {
            self.needs
                .iter()
                .filter(|n| n.counted && origin(&n.origin))
                .count()
        };
        let here = count(|o| *o == Origin::Local);
        let sibling = count(|o| matches!(o, Origin::Sibling(_)));
        let fleet = count(|o| *o == Origin::Fleet);
        let mut fleet_macs: Vec<&str> = self
            .needs
            .iter()
            .filter(|n| n.counted)
            .filter_map(|n| match n.at {
                NeedAt::Peer(at) => self.peers.get(usize::from(at)).map(|p| p.name.as_str()),
                NeedAt::Go(_) | NeedAt::Raise(_) => None,
            })
            .collect();
        fleet_macs.sort_unstable();
        fleet_macs.dedup();
        let on_fleet = match fleet_macs.as_slice() {
            [mac] => format!("on {mac}"),
            _ => "on the fleet".to_string(),
        };
        let parts: Vec<String> = [
            (here, "here"),
            (sibling, "in another aterm"),
            (fleet, on_fleet.as_str()),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, place)| format!("{n} {place}"))
        .collect();
        let mut tip = match self.need_count() {
            0 => "Nothing needs you".to_string(),
            1 => format!("1 needs you: {}", parts.join(", ")),
            n => format!("{n} need you: {}", parts.join(", ")),
        };
        if let FleetLine::InFleet {
            read: Some((answered, asked, _, _)),
            ..
        } = &self.fleet
        {
            tip.push_str(&format!(" \u{b7} fleet: {answered} of {asked} answered"));
        }
        tip
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

fn stripped_title(title: &str) -> &str {
    title
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '⚠')
        .trim_start()
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
    let offset = crate::presence::local_offset_s()?;
    let local = (unix + offset).rem_euclid(86_400);
    Some(format!("{:02}:{:02}", local / 3600, (local % 3600) / 60))
}

fn stall_words(why: Option<&str>) -> String {
    match why {
        Some("held") => "held at a screen aterm does not know".to_string(),
        Some("startup" | "shell") => "never started".to_string(),
        Some(why) => format!("stalled ({why})"),
        None => "stalled".to_string(),
    }
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

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn here_line(here: &Here) -> String {
    let parts: Vec<String> = [
        (here.working, "working"),
        (here.need_you, "need you"),
        (here.waiting, "waiting"),
        (here.resting, "resting"),
        (here.landed, "landed"),
        (here.done, "done"),
        (here.exited, "exited"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, word)| format!("{n} {word}"))
    .chain((here.shells > 0).then(|| format!("{} shell{}", here.shells, plural(here.shells))))
    .collect();
    let mut line = if parts.is_empty() {
        "This Mac \u{2014} no sessions".to_string()
    } else {
        format!("This Mac \u{2014} {}", parts.join(" \u{b7} "))
    };
    if here.elsewhere > 0 {
        line.push_str(&format!(" (+{} in another aterm)", here.elsewhere));
    }
    line
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
            stall_words(w.why)
        ),
        TaskState::Done if w.result_bytes > 0 => format!(
            "\u{2713} {} \u{2014} {} {age} \u{b7} {}",
            w.label,
            w.word(),
            bytes_words(w.result_bytes)
        ),
        TaskState::Done => format!("\u{2713} {} \u{2014} {} {age}", w.label, w.word()),
        TaskState::Exited => format!("\u{25cb} {} \u{2014} exited {age}", w.label),
    };
    fold_clip(&line, LINE_MAX)
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn done_line(d: &DoneRow, now_unix: i64) -> String {
    let word = if d.landed { "landed" } else { "done" };
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
        &format!("\u{2713} {} \u{2014} {word}{age}{size}", d.label),
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

#[cfg(any(target_os = "macos", test))]
const COST_LINE: usize = 72;

#[cfg(any(target_os = "macos", test))]
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for piece in aterm_grapheme::pieces(text, aterm_grapheme::BreakKind::is_word_boundary) {
        let space = usize::from(piece.spaced);
        match lines.last_mut() {
            Some(line)
                if aterm_grapheme::str_width(line)
                    + space
                    + aterm_grapheme::str_width(piece.text)
                    <= width =>
            {
                if piece.spaced {
                    line.push(' ');
                }
                line.push_str(piece.text);
            }
            _ => lines.push(piece.text.to_string()),
        }
    }
    lines
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

#[cfg(any(target_os = "macos", test))]
fn notify_tip(voice: Option<NoticeVoice>, on: bool) -> String {
    use crate::notice_center::Center;
    let what = "One notice when something needs you, never for a wait";
    let Some(voice) = voice else {
        return format!("{what}.");
    };
    let elsewhere = "the menu bar, the message band and messages.log still say what needs you";
    match (voice.center, on) {
        (Center::Granted, _) => format!(
            "{what}. They are aterm's own: a click on one about a tab on this Mac goes to that \
             tab, and any other brings aterm forward."
        ),
        (Center::Unasked, false) => format!(
            "{what}. Turning this on asks macOS to let aterm show them as its own, and a click \
             on one about a tab here then goes to that tab; if macOS says no, none reach the \
             desktop."
        ),
        (Center::Unasked, true) => format!(
            "{what}. macOS has not said yet whether aterm may show them as its own, so none \
             reach the desktop until it does: the first one asks macOS (turning this off and on \
             asks again), and after macOS's yes a click on one about a tab here goes to that tab."
        ),
        (Center::Refused, _) => format!(
            "{what}. aterm's notifications are off in System Settings \u{25B8} Notifications, \
             so none reach the desktop until aterm is allowed there; {elsewhere}."
        ),
        (Center::Absent, _) => match voice.fallback {
            NoticeFallback::Withheld { named: false } => format!(
                "{what}. aterm does not read this aterm.app's identity where it sits (in \
                 Desktop, Documents, Downloads or another folder macOS guards) or when it is \
                 started through a link, so it has none to show them as and none reach the \
                 desktop: move aterm.app to /Applications (or ~/Applications) and open it from \
                 there, and they are aterm's own once macOS allows it; {elsewhere}."
            ),
            NoticeFallback::Withheld { named: true } => format!(
                "{what}. macOS gave this aterm.app no notification center (aterm.log says why), \
                 and aterm shows them only as its own, so none reach the desktop; aterm's next \
                 start tries again, and {elsewhere}."
            ),
            NoticeFallback::Notifier => format!(
                "{what}. This aterm is not running from aterm.app, so they are \
                 terminal-notifier's banners; run from aterm.app, they are aterm's own once \
                 macOS allows it."
            ),
            NoticeFallback::ScriptEditor => format!(
                "{what}. This aterm is not running from aterm.app, and without terminal-notifier \
                 macOS shows them as Script Editor's banners, and a click opens Script Editor. \
                 With terminal-notifier installed, they are terminal-notifier's banners from \
                 aterm's next start; run from aterm.app, they are aterm's own once macOS \
                 allows it."
            ),
        },
    }
}

/// Compose the whole status menu from a glance, its ages as of `now_unix` — pure, deterministic, and unit-testable off macOS.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn compose_status_menu(g: &Glance, now_unix: i64) -> Vec<StatusRow> {
    let pick = |index: u16| Pick {
        generation: g.generation,
        index,
    };
    let mut rows = Vec::new();
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
                    rows.push(act(need_line(need, now_unix, further.len()), go));
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
            rows.push(act(
                format!(
                    "and {} more \u{2014} Overview\u{2026}",
                    g.needs.len() - NEEDS_SHOWN
                ),
                StatusAction::Overview,
            ));
        }
        rows.push(StatusRow::Separator);
    }
    rows.push(StatusRow::Info(here_line(&g.here)));
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
        rows.push(act(
            format!(
                "and {} more \u{2014} Overview\u{2026}",
                g.managers.len() - MANAGERS_SHOWN
            ),
            StatusAction::Overview,
        ));
    }
    if !g.done.is_empty() {
        rows.push(StatusRow::Separator);
        rows.push(StatusRow::Info(format!("Done for you ({})", g.done.len())));
        for done in g.done.iter().take(DONE_SHOWN) {
            let action = match done.at {
                DoneAt::Read(at) => StatusAction::ReadResult(pick(at)),
                DoneAt::Go(at) => StatusAction::Go(pick(at)),
            };
            rows.push(act(done_line(done, now_unix), action));
        }
    }
    if !g.waiting.is_empty() {
        rows.push(StatusRow::Separator);
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
        for held in &g.held {
            rows.push(act(
                held_line(held, now_unix),
                StatusAction::Go(pick(held.go)),
            ));
        }
        rows.extend(
            wrap_words(aterm_types::control_verbs::HOLD_COST, COST_LINE)
                .into_iter()
                .map(StatusRow::Info),
        );
        if yours {
            rows.push(act("Hand Back All", StatusAction::HandBackAll));
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
    match &g.fleet {
        FleetLine::Off => {}
        FleetLine::NotInFleet => {
            rows.push(StatusRow::Separator);
            rows.push(StatusRow::Info(
                "Fleet \u{2014} not in a fleet (aterm fleet init or join)".to_string(),
            ));
        }
        FleetLine::InFleet {
            read,
            reading,
            failed,
            no_route,
            member_of,
            unbound_issuer,
        } => {
            rows.push(StatusRow::Separator);
            rows.push(StatusRow::FleetSubmenu {
                label: fleet_line(read.as_ref(), *reading, failed.is_some(), now_unix),
            });
            rows.extend(degrade_rows(
                *no_route,
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
    rows.push(if g.in_fleet() {
        act("Read the Fleet", StatusAction::ReadFleet)
    } else {
        act("Read This Mac", StatusAction::ReadThisMac)
    });
    rows.push(StatusRow::Action {
        label: "Notify Me When I\u{2019}m Needed".to_string(),
        action: StatusAction::ToggleNotify,
        enabled: true,
        checked: g.alerts,
        tip: Some(notify_tip(g.voice, g.alerts)),
        indent: 0,
    });
    rows
}

#[cfg_attr(
    all(not(target_os = "macos"), not(test)),
    expect(dead_code, reason = "the menu-bar status item is macOS's")
)]
fn fleet_line(
    read: Option<&(usize, usize, usize, u64)>,
    reading: bool,
    failed: bool,
    now_unix: i64,
) -> String {
    let mut line = match read {
        Some((answered, asked, agents, at)) => format!(
            "Fleet \u{2014} {answered} of {asked} Mac{} answered \u{b7} {agents} agent{} \u{b7} read {} ago",
            plural(*asked),
            plural(*agents),
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
    no_route: bool,
    (member_of, unbound): (Option<&str>, Option<&str>),
    failed: Option<&str>,
) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    if no_route {
        rows.push(act(
            "No route to any fleet Mac: allow aterm in System Settings \u{25b8} Privacy & \
             Security \u{25b8} Local Network",
            StatusAction::OpenLocalNetwork,
        ));
    }
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

#[cfg(any(target_os = "macos", test))]
fn peer_rows(peer: &PeerRef, pick: Pick) -> Vec<StatusRow> {
    let mut rows = vec![act("Read This Session", StatusAction::PeerRead(pick))];
    if peer.drive {
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
    let line = if peer.drive {
        format!(
            "{} lets this Mac drive it; its person's Take Over wins",
            peer.name
        )
    } else {
        format!(
            "Read-only from here: {} does not let this Mac drive it (`aterm fleet allow drive \
             --from <this Mac>` there)",
            peer.name
        )
    };
    rows.push(StatusRow::Info(fold_clip(&line, LINE_MAX)));
    rows
}

#[cfg(any(target_os = "macos", test))]
fn held_there_rows(peer: &PeerRef, pick: Pick) -> Vec<StatusRow> {
    let mut rows = Vec::new();
    if peer.drive {
        rows.push(act(
            format!("Hand Back on {}", peer.name),
            StatusAction::PeerHandBack(pick),
        ));
    }
    rows.push(act("Read This Session", StatusAction::PeerRead(pick)));
    let line = if peer.drive {
        format!(
            "Until it is handed back, {}'s harness leaves it alone and its app verbs refuse",
            peer.name
        )
    } else {
        format!(
            "{} no longer lets this Mac drive it: its person hands it back there",
            peer.name
        )
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
                need_words(need.word),
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
    rows.push(StatusRow::Info(format!(
        "{} session{} \u{b7} {} agent{}, {} working",
        mac.sessions,
        plural(mac.sessions),
        mac.agents,
        plural(mac.agents),
        mac.working
    )));
    if mac.left_out > 0 {
        rows.push(StatusRow::Info(format!(
            "{} more not sent (its reply cap)",
            mac.left_out
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
    let what = match (mac.needs.len(), mac.working) {
        (0, 0) => "quiet".to_string(),
        (0, w) => format!("{w} working"),
        (n, w) if mac.unproven => format!("{n} need{} \u{b7} {w} working", plural(n)),
        (n, w) => format!(
            "{n} need{} you \u{b7} {w} working",
            if n == 1 { "s" } else { "" }
        ),
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
    StatusRow::Submenu {
        label: fold_clip(
            &format!("{} \u{2014} {mark}{what}{carried}", mac.name),
            LINE_MAX,
        ),
        rows,
    }
}

/// Compose the Fleet submenu from the fleet glance's slot, ages as of `now_unix` — pure, at the submenu's own `menuNeedsUpdate:`: the degrade lines; the Macs that need you or work, needs first; the quiet ones, the ones that did not answer, the ones whose key is not proven (an older one among them stands with the older) and the older ones each folded into one row; and Read the Fleet Now.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn compose_fleet_menu(
    slot: &crate::fleet_glance::FleetSlot,
    now_unix: i64,
) -> Vec<StatusRow> {
    let unbound = slot.book.unbound_issuer.as_deref();
    let mut rows = degrade_rows(
        slot.snapshot.as_ref().is_some_and(|s| s.no_route),
        (slot.book.issuer_label.as_deref(), unbound),
        slot.error.as_deref(),
    );
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
                .filter(|(_, m)| m.counts() && (!m.needs.is_empty() || m.working > 0))
                .collect();
            active.sort_by_key(|(i, m)| {
                (
                    std::cmp::Reverse(m.needs.len()),
                    std::cmp::Reverse(m.working),
                    *i,
                )
            });
            rows.extend(active.into_iter().filter_map(|(i, m)| row(i, m)));
            let quiet: Vec<StatusRow> = snap
                .macs
                .iter()
                .enumerate()
                .filter(|(_, m)| m.counts() && m.needs.is_empty() && m.working == 0)
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
    rows.push(act("Read the Fleet Now", StatusAction::ReadFleetNow));
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

/// The bar button's face — its title, and whether it is drawn red — to set for `glance`, or `None` when the button already shows it.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn button_face_change(
    shown: Option<&(String, bool)>,
    glance: &Glance,
) -> Option<(String, bool)> {
    let next = (glance.button_title(), glance.stuck());
    (shown != Some(&next)).then_some(next)
}

#[cfg(target_os = "macos")]
pub(crate) use macos::{StatusItemHandle, attach_fleet, install, update};

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

    use aterm_objc::{Id, Obj, Retained, Sel, autoreleasepool, class, sel};
    use aterm_window::event_loop::EventLoopProxy;

    use super::{Glance, StatusAction};
    use crate::Wake;
    use crate::appkit::consts::NS_VARIABLE_STATUS_ITEM_LENGTH;
    use crate::appkit::{self, MainThread};

    const NS_CONTROL_STATE_VALUE_ON: isize = 1;

    pub(crate) struct StatusItemHandle {
        target: Retained<StatusTarget>,
        item: Obj,
        shown_face: RefCell<Option<(String, bool)>>,
        shown_tip: RefCell<Option<String>>,
    }

    pub(crate) struct StatusIvars {
        proxy: EventLoopProxy<Wake>,
        glance: RefCell<Glance>,
        painted: RefCell<Option<Glance>>,
        fleet: RefCell<Option<Arc<Mutex<crate::fleet_glance::FleetSlot>>>>,
        fleet_menu: Cell<usize>,
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

            @sel(menuNeedsUpdate:)
            fn menu_needs_update(&self, menu: Id) {
                if menu.is_null() || MainThread::new().is_none() {
                    return;
                }
                if menu.addr() == self.ivars().fleet_menu.get() {
                    paint_fleet(menu, self);
                    return;
                }
                let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
                let (rows, painted) = match self.ivars().glance.try_borrow() {
                    Ok(glance) => (super::compose_status_menu(&glance, now), glance.clone()),
                    Err(_) => return,
                };
                autoreleasepool(|_| paint_menu(menu, self, rows));
                if let Ok(mut slot) = self.ivars().painted.try_borrow_mut() {
                    *slot = Some(painted);
                }
            }

            @sel(menuWillOpen:)
            fn menu_will_open(&self, menu: Id) {
                // The root's open only: the Fleet submenu's is the same visit.
                if menu.addr() == self.ivars().fleet_menu.get() {
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
            // SAFETY: `-setDelegate:` and `-setMenu:` are `-(void)(id)` on live
            // receivers, on the main thread. The delegate is the retained
            // target, which outlives the menu because the handle owns it; the
            // item retains the menu, so `menu` may drop at the end of this
            // scope, inside the pool.
            unsafe {
                appkit::send_v_id(menu.id(), sel!(setDelegate:), target.as_id());
                appkit::send_v_id(item.id(), sel!(setMenu:), menu.id());
            }
            Some(StatusItemHandle {
                target,
                item,
                shown_face: RefCell::new(None),
                shown_tip: RefCell::new(None),
            })
        })?;
        show_face(&handle, (glance.button_title(), glance.stuck()));
        show_tip(&handle, glance.tooltip());
        Some(handle)
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

    fn attribute_keys() -> (Id, Id) {
        #[link(name = "AppKit", kind = "framework")]
        unsafe extern "C" {
            static NSForegroundColorAttributeName: *mut std::ffi::c_void;
            static NSFontAttributeName: *mut std::ffi::c_void;
        }
        // SAFETY: immortal `NSString *` constants AppKit exports; reading a symbol is a load.
        unsafe {
            (
                Id::from_ptr(NSForegroundColorAttributeName),
                Id::from_ptr(NSFontAttributeName),
            )
        }
    }

    fn red_title(title: &str, font: Id) -> Option<Obj> {
        let text = appkit::nsstring(title)?;
        // SAFETY: `+systemRedColor` is `-(NSColor *)`, an autoreleased shared colour;
        // `+menuBarFontOfSize:` is `-(NSFont *)(CGFloat)`, 0 meaning the menu bar's own size;
        // `+dictionaryWithObjects:forKeys:count:` is `-(id)(const id *, const id *, NSUInteger)`,
        // which copies the two arrays (live on this frame) and retains their elements;
        // `-initWithString:attributes:` is NSAttributedString's `-(id)(NSString *, NSDictionary *)`
        // initializer, consuming the +1 `+alloc`. Every argument is live for the call, inside the
        // caller's pool.
        unsafe {
            let red = appkit::send_id(class(c"NSColor").as_id(), sel!(systemRedColor));
            let font = if font.is_null() {
                appkit::send_id_f64(class(c"NSFont").as_id(), sel!(menuBarFontOfSize:), 0.0)
            } else {
                font
            };
            if red.is_null() || font.is_null() {
                return None;
            }
            let (color_key, font_key) = attribute_keys();
            let values = [red, font];
            let keys = [color_key, font_key];
            let dict: unsafe extern "C-unwind" fn(Id, Sel, *const Id, *const Id, usize) -> Id =
                aterm_objc::checked_msg();
            let attrs = dict(
                class(c"NSDictionary").as_id(),
                sel!(dictionaryWithObjects:forKeys:count:),
                values.as_ptr(),
                keys.as_ptr(),
                values.len(),
            );
            if attrs.is_null() {
                return None;
            }
            let init: unsafe extern "C-unwind" fn(Id, Sel, Id, Id) -> Id =
                aterm_objc::checked_msg();
            Obj::from_owned(init(
                appkit::alloc(class(c"NSAttributedString")),
                sel!(initWithString:attributes:),
                text.id(),
                attrs,
            ))
        }
    }

    fn show_face(handle: &StatusItemHandle, face: (String, bool)) {
        let set = autoreleasepool(|_| {
            // SAFETY: plain AppKit sends on live receivers, on the main thread
            // (both callers checked `MainThread::new()`). `-button` is `-(id)`
            // and returns the bar button for an item created with a variable
            // length; `-setTitle:` and `-setAttributedTitle:` are
            // `-(void)(id)`, the plain title replacing any attributed one.
            unsafe {
                let button = appkit::send_id(handle.item.id(), sel!(button));
                if button.is_null() {
                    return false;
                }
                if face.1 {
                    let font = appkit::send_id(button, sel!(font));
                    let Some(text) = red_title(&face.0, font) else {
                        return false;
                    };
                    appkit::send_v_id(button, sel!(setAttributedTitle:), text.id());
                } else {
                    let Some(text) = appkit::nsstring(&face.0) else {
                        return false;
                    };
                    appkit::send_v_id(button, sel!(setTitle:), text.id());
                }
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
    mod tests {
        use aterm_objc::{ClassType, Sel, autoreleasepool, class, class_name, method_types, sel};

        use super::{StatusTarget, new_item};
        use crate::appkit;

        #[test]
        fn the_registered_class_is_what_the_runtime_reports() {
            let cls = StatusTarget::class();
            assert!(!cls.is_null());
            assert_eq!(class(c"ATermStatusTarget"), cls);
            // SAFETY: `cls` is the class this module registered, so it and its superclass are live, immortal class objects.
            unsafe {
                assert_eq!(class_name(cls), c"ATermStatusTarget");
                assert_eq!(class_name(aterm_objc::superclass_of(cls)), c"NSObject");
            }
            let proto = aterm_objc::protocol(c"NSMenuDelegate");
            assert!(
                !proto.is_null()
                    && !aterm_objc::protocols_registered_by_aterm().contains(&c"NSMenuDelegate"),
                "this host's AppKit registers NSMenuDelegate — not one aterm had to supply"
            );
            // SAFETY: `+conformsToProtocol:` and `+instancesRespondToSelector:` are side-effect-free NSObject queries on a live class object.
            unsafe {
                assert!(
                    appkit::send_bool_id(
                        cls.as_id(),
                        sel!(conformsToProtocol:),
                        aterm_objc::Id::from_ptr(proto.as_ptr()),
                    ),
                    "the declared class does not answer NSMenuDelegate"
                );
                assert!(appkit::send_bool_sel(
                    cls.as_id(),
                    sel!(instancesRespondToSelector:),
                    sel!(statusAction:)
                ));
                assert!(appkit::send_bool_sel(
                    cls.as_id(),
                    sel!(instancesRespondToSelector:),
                    sel!(menuNeedsUpdate:)
                ));
                assert!(appkit::send_bool_sel(
                    cls.as_id(),
                    sel!(instancesRespondToSelector:),
                    sel!(menuWillOpen:)
                ));
            }
        }

        #[test]
        fn the_runtime_reports_the_encodings_the_table_says() {
            let cls = StatusTarget::class();
            // SAFETY: `cls` is the live registered class.
            unsafe {
                assert_eq!(
                    method_types(cls, sel!(statusAction:)).as_deref(),
                    Some("v@:@")
                );
                assert_eq!(
                    method_types(cls, sel!(menuNeedsUpdate:)).as_deref(),
                    Some("v@:@")
                );
                assert_eq!(
                    method_types(cls, sel!(menuWillOpen:)).as_deref(),
                    Some("v@:@")
                );
                assert_eq!(method_types(cls, sel!(dealloc)).as_deref(), Some("v@:"));
            }
        }

        #[test]
        fn an_information_row_has_a_nil_action() {
            autoreleasepool(|_| {
                let item = new_item("info", Sel::NULL).expect("item");
                let wired = new_item("cmd", sel!(statusAction:)).expect("item");
                // SAFETY: `-action` is `-(SEL)` on a live NSMenuItem, and `-title` is `-(NSString *)`.
                unsafe {
                    let action: unsafe extern "C-unwind" fn(aterm_objc::Id, Sel) -> Sel =
                        aterm_objc::checked_msg();
                    assert!(action(item.id(), sel!(action)).is_null());
                    assert_eq!(action(wired.id(), sel!(action)), sel!(statusAction:));
                    assert_eq!(
                        appkit::nsstring_to_rust(appkit::send_id(item.id(), sel!(title))),
                        "info"
                    );
                }
            });
        }
    }
}

#[cfg(test)]
#[path = "desktop_alert_debt_conformance.rs"]
mod desktop_alert_debt_conformance;

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_agent::harness::resume::AfterRestart;

    use crate::needs::{AgentFact, NeedFacts};

    fn local_glance(
        rows: &[SessionRow],
        siblings: &[SiblingInstance],
        now_unix: i64,
        api_since: Option<i64>,
    ) -> Glance {
        Glance::build(rows, siblings, None, unix_ms(now_unix), api_since)
    }

    fn unix_ms(secs: i64) -> u64 {
        u64::try_from(secs).unwrap_or(0) * 1000
    }

    fn row_of(id: u64, title: &str, facts: &NeedFacts) -> SessionRow {
        SessionRow {
            id,
            title: title.to_string(),
            need: crate::needs::of(facts).need,
            ..SessionRow::default()
        }
    }

    fn agent_facts(word: &'static str, rev: u64) -> NeedFacts {
        NeedFacts {
            agent: Some(AgentFact {
                word,
                detail: (word == "prompt").then(|| "bash:not-read-only".to_string()),
                rev,
                subject: (word == "prompt").then(|| "rm -rf build".to_string()),
            }),
            ..NeedFacts::default()
        }
    }

    fn agent_row(id: u64, title: &str, word: &'static str, rev: u64) -> SessionRow {
        row_of(id, title, &agent_facts(word, rev))
    }

    const OWN: &str = "claude --model opus --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a1b2c3d4e5f";

    fn stall_row(
        fact: &crate::input_stall::InputStallFact,
        program: Option<&str>,
        after: &AfterRestart,
        clock: Option<&str>,
        waited: std::time::Duration,
    ) -> InputStallRow {
        InputStallRow {
            text: crate::input_stall::attention_text(
                fact,
                program,
                after,
                "s-b7cf523445a1b0d8658e",
                clock,
                waited,
            ),
            key: crate::input_stall::episode_key(fact),
            stopped: fact.stopped,
        }
    }

    fn incident_stall() -> crate::input_stall::InputStallFact {
        crate::input_stall::InputStallFact {
            word: aterm_session::input_backlog::InputWord::Stalled,
            since: std::time::Instant::now(),
            since_unix: None,
            bytes: 1,
            stopped: false,
            rss_mb: Some(39_731),
            restart: crate::input_stall::Restart::default(),
            act: None,
        }
    }

    fn stalled_agent(
        fact: &crate::input_stall::InputStallFact,
        row: InputStallRow,
        rev: u64,
    ) -> NeedFacts {
        NeedFacts {
            stall: Some(crate::needs::StallFact {
                text: row.text,
                key: row.key,
                stopped: row.stopped,
                act: fact.act.is_some(),
                hand_back_frees: false,
            }),
            agent: Some(AgentFact {
                word: "wall:unresponsive",
                detail: fact.stopped.then(|| "stopped".to_string()),
                rev,
                subject: None,
            }),
            ..NeedFacts::default()
        }
    }

    #[test]
    fn a_stall_notifies_once_per_episode_while_its_text_moves() {
        let fact = incident_stall();
        let row_at = |fact: &crate::input_stall::InputStallFact, secs: u64| {
            let row = stall_row(
                fact,
                Some("claude"),
                &AfterRestart::Command(OWN.to_string()),
                None,
                std::time::Duration::from_secs(secs),
            );
            row_of(5, "worker", &stalled_agent(fact, row, secs))
        };
        let first = escalation(&row_at(&fact, 10)).unwrap();
        let later = escalation(&row_at(
            &crate::input_stall::InputStallFact {
                bytes: 7,
                ..fact.clone()
            },
            95,
        ))
        .unwrap();
        assert_ne!(first.label, later.label, "the text moved");
        assert_eq!(first.key, later.key, "the episode did not");
        let turned = crate::input_stall::InputStallFact {
            word: aterm_session::input_backlog::InputWord::Stopped,
            stopped: true,
            ..fact.clone()
        };
        let stopped = escalation(&row_at(&turned, 120)).unwrap();
        assert_eq!(stopped.key, first.key);
        assert_eq!(stopped.headline, headline(NeedWord::Stopped));

        let now = std::time::Instant::now();
        let mut h = Herald::default();
        let posted = h.note(5, Some(&first), false, true, now);
        assert!(posted.notice.is_some());
        let moved = h.note(5, Some(&later), false, true, now + NOTIFY_SESSION_FLOOR * 2);
        assert!(moved.row_moved && moved.notice.is_none());
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Same));
        let turned_note = h.note(
            5,
            Some(&stopped),
            false,
            true,
            now + NOTIFY_SESSION_FLOOR * 5 / 2,
        );
        assert!(turned_note.row_moved && turned_note.notice.is_none());
        let _ = h.note(5, None, false, true, now + NOTIFY_SESSION_FLOOR * 3);
        let next = crate::input_stall::InputStallFact {
            since: fact.since + std::time::Duration::from_secs(600),
            ..fact.clone()
        };
        let again = escalation(&row_at(&next, 10)).unwrap();
        assert_ne!(again.key, first.key);
        assert!(
            h.note(5, Some(&again), false, true, now + NOTIFY_SESSION_FLOOR * 4)
                .notice
                .is_some()
        );
        assert_ne!(
            aterm_hash::fnv1a_64(first.label.as_bytes()),
            aterm_hash::fnv1a_64(later.label.as_bytes())
        );
    }

    #[test]
    fn a_stall_on_an_agent_is_its_row_frozen_or_stopped() {
        use crate::input_stall::{InputStallFact, StallAct};
        let fact = incident_stall();
        let worker = |fact: &InputStallFact, after: &AfterRestart, rev: u64| {
            let row = stall_row(
                fact,
                Some("claude"),
                after,
                Some("14:02"),
                std::time::Duration::from_secs(9660),
            );
            NeedFacts {
                supervised: true,
                attention: vec![crate::needs::AttentionFact {
                    owner: aterm_agent::supervise::ATTENTION_OWNER.to_string(),
                    text: "answer this box: 4. Chat about this".to_string(),
                    kind: None,
                }],
                ..stalled_agent(fact, row, rev)
            }
        };
        let hosted = InputStallFact {
            act: Some(StallAct {
                cmd: aterm_agent::harness::resume::restart_command(
                    Some("s-b7cf523445a1b0d8658e"),
                    None,
                ),
                promised: true,
            }),
            ..fact.clone()
        };
        let frozen_row = row_of(
            3,
            "\u{2733} worker",
            &worker(&hosted, &AfterRestart::Relaunch, 9),
        );
        assert_eq!(
            frozen_row.need.as_ref().map(|n| n.word),
            Some(NeedWord::Frozen)
        );
        let frozen = escalation(&frozen_row).expect("a stall row");
        assert_eq!(frozen.kind, EscalationKind::Unresponsive);
        assert_eq!(frozen.headline, headline(NeedWord::Frozen));
        assert_eq!(frozen.key, crate::input_stall::episode_key(&fact));
        assert!(
            frozen.body.ends_with(
                "claude is frozen: not reading input since 14:02 (1 B queued, rss 38.8 GB)"
            ),
            "{}",
            frozen.body
        );
        let entry = worker(&hosted, &AfterRestart::Relaunch, 9)
            .stall
            .expect("a published stall")
            .text;
        assert!(
            entry.contains("restart if-stalled"),
            "the entry keeps it: {entry}"
        );
        assert!(!frozen.body.contains("aterm ctl"), "{}", frozen.body);
        let g = local_glance(
            &[
                row(1, "zsh", Some("deploy needs a human")),
                agent_row(2, "builder", "prompt", 5),
                frozen_row.clone(),
            ],
            &[],
            0,
            None,
        );
        assert_eq!(g.local_need_sessions()[0], 3, "{:?}", g.needs);
        let stopped_fact = InputStallFact {
            word: aterm_session::input_backlog::InputWord::Stopped,
            stopped: true,
            ..fact.clone()
        };
        for supervised in [true, false] {
            let facts = NeedFacts {
                supervised,
                ..worker(&stopped_fact, &AfterRestart::Command(OWN.to_string()), 10)
            };
            let stopped_row = row_of(3, "\u{2733} worker", &facts);
            assert_eq!(
                stopped_row.need.as_ref().map(|n| n.word),
                Some(NeedWord::Stopped),
                "supervised={supervised}"
            );
            let stopped = escalation(&stopped_row).expect("a stopped row");
            assert_eq!(stopped.kind, EscalationKind::Unresponsive);
            assert_eq!(stopped.headline, headline(NeedWord::Stopped));
            assert_eq!(stopped.key, frozen.key, "one episode");
            assert!(
                stopped
                    .body
                    .ends_with("claude is stopped with input queued since 14:02"),
                "{}",
                stopped.body
            );
            assert!(!stopped.label.contains("signal cont"), "{}", stopped.label);
        }
        let mut facts = worker(&fact, &AfterRestart::Relaunch, 11);
        facts.stall = None;
        facts.agent.as_mut().unwrap().word = "prompt";
        assert_eq!(
            escalation(&row_of(3, "worker", &facts)).unwrap().kind,
            EscalationKind::Attention
        );
    }

    #[test]
    fn a_refused_notice_spends_no_floor_and_no_burst_place() {
        let t0 = std::time::Instant::now();
        let t1 = t0 + NOTIFY_SESSION_FLOOR;
        let retry = t1 + NOTIFY_RETRY;
        let coalesced = Some("a: bash rm -rf build".to_string());
        let run = |refuse: bool| {
            let mut h = Herald::default();
            for id in [1, 2] {
                let row = agent_row(id, "a", "prompt", 1);
                assert!(herald_note(&mut h, &row, false, t0).notice.is_some());
            }
            for rev in [3, 5] {
                let row = agent_row(1, "a", "prompt", rev);
                assert!(herald_note(&mut h, &row, false, t0).notice.is_none());
            }
            assert_eq!(h.due(t1).0, vec![1]);
            let paid = herald_note(&mut h, &agent_row(1, "a", "prompt", 5), false, t1);
            assert_eq!(paid.owed, 2, "one notice pays both held boxes");
            assert_eq!(paid.notice.map(|n| n.body), coalesced);
            if refuse {
                h.refused(1, paid.owed, t1);
            }
            h
        };

        let mut h = run(true);
        assert_eq!(h.slots[&1].notified_at, Some(t0));
        assert_eq!(h.due(t1), (vec![], Some(retry)));
        assert_eq!(h.due(retry).0, vec![1]);
        let three = agent_row(3, "c", "prompt", 1);
        assert!(herald_note(&mut h, &three, false, retry).notice.is_some());
        let freed = t0 + NOTIFY_BURST_WINDOW;
        assert_eq!(h.due(retry), (vec![], Some(freed)));
        let paid = herald_note(&mut h, &agent_row(1, "a", "prompt", 5), false, freed);
        assert_eq!(paid.owed, 2, "coalesced as it was");
        assert_eq!(paid.notice.map(|n| n.body), coalesced);
        assert_eq!(h.due(freed), (vec![], None));

        let mut h = run(false);
        assert_eq!(h.slots[&1].notified_at, Some(t1));
        assert_eq!(h.due(retry), (vec![], None));
        assert!(herald_note(&mut h, &three, false, retry).notice.is_none());
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Limited));
    }

    #[test]
    fn a_refusal_holds_only_the_refused_session() {
        let t = std::time::Instant::now();
        let t1 = t + std::time::Duration::from_secs(1);
        let (retry, retry_two) = (t + NOTIFY_RETRY, t1 + NOTIFY_RETRY);
        let mut h = Herald::default();
        let out = herald_note(&mut h, &agent_row(1, "a", "prompt", 1), false, t);
        assert!(out.notice.is_some());
        h.refused(1, out.owed, t);

        let two = agent_row(2, "b", "prompt", 1);
        let posted = herald_note(&mut h, &two, false, t1);
        assert_eq!(h.last_quiet(), None, "held by another session's refusal");
        assert_eq!(posted.notice.as_ref().map(|n| n.session), Some(2));
        let line = herald_log_line(
            2,
            escalation(&two).as_ref(),
            &posted,
            None,
            HeraldPost::Queued,
        );
        assert_eq!(line.map(|(_, text)| text.ends_with(": queued")), Some(true));

        assert_eq!(h.due(t1), (vec![], Some(retry)));
        let held = herald_note(&mut h, &agent_row(1, "a", "prompt", 3), false, t1);
        assert!(held.notice.is_none());
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Limited));

        h.refused(2, posted.owed, t1);
        assert_eq!(h.due(t1), (vec![], Some(retry)));
        assert_eq!(h.due(retry), (vec![1], Some(retry_two)));
        let paid = herald_note(&mut h, &agent_row(1, "a", "prompt", 3), false, retry);
        assert_eq!(paid.owed, 2, "the retry pays both, coalesced");
        assert_eq!(
            paid.notice.map(|n| n.body),
            Some("a: bash rm -rf build".to_string())
        );
        assert_eq!(h.slots[&1].retry_at, None, "a post ends the wait");
        assert_eq!(h.due(retry), (vec![], Some(retry_two)));
        assert_eq!(h.due(retry_two), (vec![2], None));
    }

    #[test]
    fn row_text_is_one_clipped_line() {
        assert_eq!(fold_clip("rm -rf\n  build\t/x", 64), "rm -rf build /x");
        assert_eq!(fold_clip("  lead", 64), "lead");
        let long = "x".repeat(100);
        let clipped = fold_clip(&long, 20);
        assert!(
            clipped.len() <= 20 && clipped.ends_with('\u{2026}'),
            "{clipped}"
        );
        let wide = "\u{00e9}".repeat(30);
        assert!(fold_clip(&wide, 11).ends_with('\u{2026}'));
        assert_eq!(
            fold_clip("rm -rf /tmp/\u{202e}gol.txt\u{2066}x\u{2069}\u{200b}y", 64),
            "rm -rf /tmp/gol.txtxy"
        );
        let mut spoof = agent_facts("prompt", 1);
        spoof.agent.as_mut().unwrap().subject = Some("ls \u{202e}fr- mr".to_string());
        let esc = escalation(&row_of(1, "t", &spoof)).unwrap();
        assert!(!esc.label.contains('\u{202e}') && !esc.body.contains('\u{202e}'));
        assert_eq!(
            fold_clip("caf\u{00e9} \u{1f469}\u{200d}\u{1f4bb}", 64),
            "caf\u{00e9} \u{1f469}\u{200d}\u{1f4bb}"
        );
        let mut bare = agent_facts("prompt", 1);
        bare.agent.as_mut().unwrap().subject = None;
        assert_eq!(
            escalation(&row_of(1, "t", &bare)).unwrap().label,
            "t \u{2014} bash approval"
        );
    }

    #[test]
    fn presence_band_and_menu_bar_say_a_box_and_a_wall_alike() {
        let mut bare = agent_facts("prompt", 1);
        bare.agent.as_mut().unwrap().subject = None;
        for (detail, kind, label) in [
            ("question", EscalationKind::Question, "question"),
            ("other", EscalationKind::Prompt, "approval"),
            ("plan-exit", EscalationKind::Prompt, "plan approval"),
        ] {
            bare.agent.as_mut().unwrap().detail = Some(detail.into());
            let esc = escalation(&row_of(1, "t", &bare)).unwrap();
            assert_eq!(esc.kind, kind, "{detail}");
            assert_eq!(esc.label, format!("t \u{2014} {label}"), "{detail}");
            assert_eq!(
                crate::presence::prompt_band_word(Some(detail)),
                label,
                "{detail}"
            );
        }
        let overloaded = crate::presence::AgentPhase::Wall {
            kind: aterm_phase::WallKind::Overloaded,
            reset: None,
            until: None,
        };
        assert_eq!(
            escalation(&agent_row(2, "w", "wall:overloaded", 1))
                .unwrap()
                .label,
            format!("w \u{2014} {}", overloaded.band_word())
        );
    }

    fn herald_note(
        h: &mut Herald,
        row: &SessionRow,
        looking: bool,
        now: std::time::Instant,
    ) -> HeraldOutcome {
        h.note(row.id, escalation(row).as_ref(), looking, true, now)
    }

    #[test]
    fn the_herald_posts_once_per_transition() {
        let t0 = std::time::Instant::now();
        let mut h = Herald::default();
        let boxed = agent_row(4, "builder", "prompt", 5);
        let first = herald_note(&mut h, &boxed, false, t0);
        assert!(first.row_moved);
        let notice = first.notice.expect("the first box posts");
        assert_eq!(notice.session, 4);
        let again = herald_note(&mut h, &boxed, false, t0);
        assert_eq!(again, HeraldOutcome::default(), "same box, nothing");
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Same));
        let busy = agent_row(4, "builder", "busy", 6);
        assert!(herald_note(&mut h, &busy, false, t0).row_moved);
        let later = t0 + NOTIFY_SESSION_FLOOR;
        let next = herald_note(&mut h, &agent_row(4, "builder", "prompt", 7), false, later);
        assert!(next.notice.is_some(), "a new box is a new transition");

        let mut h = Herald::default();
        let seen = herald_note(&mut h, &boxed, true, t0);
        assert!(seen.row_moved && seen.notice.is_none());
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Looking));
        assert!(herald_note(&mut h, &boxed, false, t0).notice.is_none());

        let mut h = Herald::default();
        let mut early = agent_facts("prompt", 5);
        early.agent.as_mut().unwrap().subject = None;
        assert!(
            herald_note(&mut h, &row_of(4, "builder", &early), false, t0)
                .notice
                .is_some()
        );
        let late = herald_note(&mut h, &boxed, false, t0);
        assert!(late.row_moved && late.notice.is_none());
    }

    #[test]
    fn the_herald_is_rate_limited_per_session_and_per_instance() {
        let t0 = std::time::Instant::now();
        let mut h = Herald::default();
        assert!(
            herald_note(&mut h, &agent_row(1, "a", "prompt", 1), false, t0)
                .notice
                .is_some()
        );
        let _ = herald_note(&mut h, &agent_row(1, "a", "busy", 2), false, t0);
        let flap = herald_note(&mut h, &agent_row(1, "a", "prompt", 3), false, t0);
        assert!(flap.notice.is_none(), "inside the session floor");
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Limited));
        let second = herald_note(&mut h, &agent_row(1, "a", "prompt", 4), false, t0);
        assert!(second.notice.is_none());
        assert_eq!(h.due(t0), (vec![], Some(t0 + NOTIFY_SESSION_FLOOR)));
        let after = t0 + NOTIFY_SESSION_FLOOR;
        assert_eq!(h.due(after), (vec![1], None));
        let paid = herald_note(&mut h, &agent_row(1, "a", "prompt", 4), false, after);
        assert_eq!(paid.owed, 2, "one notice pays both held boxes");
        assert_eq!(
            paid.notice.map(|n| n.body),
            Some("a: bash rm -rf build".to_string()),
            "it names the box shown now, never a count of boxes it replaced"
        );
        assert_eq!(h.due(after), (vec![], None));
        assert!(
            herald_note(&mut h, &agent_row(1, "a", "prompt", 4), false, after)
                .notice
                .is_none()
        );
        let _ = herald_note(&mut h, &agent_row(1, "a", "prompt", 5), false, after);
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Limited));
        let _ = herald_note(&mut h, &agent_row(1, "a", "busy", 6), false, after);
        assert_eq!(h.due(after + NOTIFY_SESSION_FLOOR), (vec![], None));

        let mut h = Herald::default();
        let posted = (10..20)
            .filter(|id| {
                herald_note(
                    &mut h,
                    &agent_row(*id, "w", "wall:usage-session", 1),
                    false,
                    t0,
                )
                .notice
                .is_some()
            })
            .count();
        assert_eq!(posted, NOTIFY_BURST);
        let rows: Vec<SessionRow> = (10..20)
            .map(|id| agent_row(id, "w", "wall:usage-session", 1))
            .collect();
        assert_eq!(local_glance(&rows, &[], 0, None).needs.len(), 10);
        let later = t0 + NOTIFY_BURST_WINDOW;
        assert_eq!(h.due(t0).1, Some(later));
        let (ready, _) = h.due(later);
        assert_eq!(ready.len(), 10 - NOTIFY_BURST);

        h.retire(30);
        assert_eq!(h.note(99, None, false, true, t0), HeraldOutcome::default());
        assert!(!h.slots.contains_key(&99));
    }

    #[test]
    fn each_herald_decision_is_one_log_line_without_its_text() {
        use aterm_log::Level::{Debug, Info, Warn};
        let t0 = std::time::Instant::now();
        let s = |secs| t0 + std::time::Duration::from_secs(secs);
        let level = |line: &Option<(aterm_log::Level, String)>| line.as_ref().map(|(l, _)| *l);
        let note = |h: &mut Herald, row: &SessionRow, looking, now| {
            let esc = escalation(row);
            let out = h.note(row.id, esc.as_ref(), looking, true, now);
            let line = herald_log_line(
                row.id,
                esc.as_ref(),
                &out,
                h.last_quiet(),
                HeraldPost::Queued,
            );
            (esc, out, line)
        };
        let leaks = |line: &str, esc: &Escalation, secrets: &[&str]| {
            [esc.body.as_str(), esc.label.as_str()]
                .into_iter()
                .chain(secrets.iter().copied())
                .find(|s| line.contains(*s))
                .map(str::to_string)
        };
        let secrets = ["builder", "rm -rf build", "hunter2", "approve me"];
        let full = HeraldPost::Refused("the notifier's queue is full");
        let mut h = Herald::default();
        let mut log = Vec::new();
        let mut step = |h: &mut Herald, word: &'static str, rev: u64, looking, at| {
            let (_, out, line) = note(h, &agent_row(4, "builder", word, rev), looking, s(at));
            log.extend(line.clone());
            (out, line)
        };

        let (out, line) = step(&mut h, "prompt", 7, false, 0);
        assert!(out.transition && out.owed == 1);
        assert_eq!(level(&line), Some(Info));
        let said = line.as_ref().map(|(_, l)| l.as_str()).unwrap_or_default();
        assert!(
            said.contains("session 4 ") && said.contains("agent_rev=7)"),
            "{said}"
        );
        let esc = escalation(&agent_row(4, "builder", "prompt", 7)).expect("a row");
        assert_eq!(
            level(&herald_log_line(4, Some(&esc), &out, None, full)),
            Some(Warn)
        );
        assert_eq!(
            level(&herald_log_line(
                4,
                Some(&esc),
                &out,
                None,
                HeraldPost::Suppressed
            )),
            Some(Info)
        );
        assert!(out.notice.unwrap().body.contains("rm -rf build"));

        let (out, line) = step(&mut h, "prompt", 7, false, 0);
        assert_eq!(out, HeraldOutcome::default());
        assert_eq!(level(&line), Some(Debug));

        assert_eq!(step(&mut h, "busy", 8, false, 15).1, None);
        let (out, line) = step(&mut h, "prompt", 9, false, 15);
        assert_eq!((out.transition, out.owed, out.first_quiet), (true, 1, true));
        assert_eq!(level(&line), Some(Info));
        assert_eq!(level(&step(&mut h, "prompt", 9, false, 15).1), Some(Debug));
        for (answered, redrawn, at) in [(10, 11, 16), (12, 13, 17), (14, 15, 18)] {
            assert_eq!(step(&mut h, "busy", answered, false, at).1, None);
            let (out, line) = step(&mut h, "prompt", redrawn, false, at);
            assert_eq!(h.last_quiet(), Some(HeraldQuiet::Limited));
            assert_eq!(
                (out.transition, out.owed, out.first_quiet),
                (true, 1, false)
            );
            assert_eq!(level(&line), Some(Debug));
        }
        let (out, line) = step(&mut h, "prompt", 16, false, 19);
        assert_eq!((out.transition, out.owed), (true, 2));
        assert_eq!(level(&line), Some(Debug));
        assert_eq!(h.due(s(20)).0, vec![4]);
        let (out, line) = step(&mut h, "prompt", 16, false, 20);
        assert!(!out.transition && out.notice.is_some());
        assert_eq!(level(&line), Some(Info));

        assert_eq!(step(&mut h, "busy", 17, false, 21).1, None);
        let (out, line) = step(&mut h, "prompt", 18, true, 21);
        assert_eq!(h.last_quiet(), Some(HeraldQuiet::Looking));
        assert!(out.transition && out.first_quiet && out.notice.is_none());
        assert_eq!(level(&line), Some(Info));
        assert_eq!(step(&mut h, "busy", 19, true, 22).1, None);
        assert_eq!(level(&step(&mut h, "prompt", 20, true, 22).1), Some(Debug));
        assert_eq!(step(&mut h, "busy", 21, true, 41).1, None);
        let (out, line) = step(&mut h, "prompt", 22, true, 41);
        assert!(out.first_quiet);
        assert_eq!(level(&line), Some(Info));

        assert_eq!(log.iter().filter(|(l, _)| *l == Info).count(), 5);
        let boxed = escalation(&agent_row(4, "builder", "prompt", 7)).expect("a row");
        for (_, line) in &log {
            assert_eq!(leaks(line, &boxed, &secrets), None, "{line}");
        }

        let mut h = Herald::default();
        let asked = row(5, "zsh", Some("deploy token hunter2 needs a human"));
        let (esc, _, line) = note(&mut h, &asked, false, t0);
        let esc = esc.expect("a row");
        assert_eq!(
            esc.key,
            aterm_hash::fnv1a_64(b"deploy token hunter2 needs a human")
        );
        let (level, line) = line.expect("a post is logged");
        assert_eq!(level, Info);
        assert_eq!(
            line,
            format!(
                "escalation notification for session 5 (attention text_key={:#018x}): queued",
                esc.key
            )
        );
        assert_eq!(leaks(&line, &esc, &secrets), None, "{line}");
        let frozen = row_of(
            6,
            "builder",
            &NeedFacts {
                stall: Some(crate::needs::StallFact {
                    text: "claude is frozen: not reading input".to_string(),
                    key: 0xbeef,
                    stopped: false,
                    act: true,
                    hand_back_frees: false,
                }),
                ..NeedFacts::default()
            },
        );
        let (esc, _, line) = note(&mut h, &frozen, false, t0);
        let (level_was, line) = line.expect("a post is logged");
        assert_eq!(level_was, Info);
        assert!(line.contains("0x000000000000beef"), "its episode: {line}");
        assert_eq!(leaks(&line, &esc.unwrap(), &["frozen"]), None, "{line}");

        let old = format!("escalation notification not delivered: {}", boxed.body);
        assert_eq!(
            leaks(&old, &boxed, &secrets).as_deref(),
            Some("builder: bash rm -rf build")
        );
    }

    fn row(id: u64, title: &str, attention: Option<&str>) -> SessionRow {
        let facts = NeedFacts {
            attention: attention
                .map(|text| crate::needs::AttentionFact {
                    owner: "-".to_string(),
                    text: text.to_string(),
                    kind: Some(aterm_types::needs::AttentionKind::Ask),
                })
                .into_iter()
                .collect(),
            ..NeedFacts::default()
        };
        row_of(id, title, &facts)
    }

    fn worker(id: u64, title: &str, from: &str, state: TaskState, result: u64) -> SessionRow {
        SessionRow {
            id,
            sid: format!("s-{id:020}"),
            title: title.to_string(),
            agent: "busy",
            task: Some(TaskRow {
                state,
                why: (state == TaskState::Stalled).then_some("startup"),
                from: from.to_string(),
                entered: 7,
                since_unix_ms: 1_000_000,
                result_bytes: result,
                result_path: (result > 0).then(|| std::path::PathBuf::from("/tmp/r.md")),
                seen: false,
            }),
            ..SessionRow::default()
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Shape {
        Info,
        Sep,
        Act(StatusAction),
        Sub(Vec<Shape>),
        Fleet,
    }

    fn shape(rows: &[StatusRow]) -> Vec<Shape> {
        rows.iter()
            .map(|row| match row {
                StatusRow::Info(_) => Shape::Info,
                StatusRow::Separator => Shape::Sep,
                StatusRow::Action { action, .. } => Shape::Act(*action),
                StatusRow::Submenu { rows, .. } => Shape::Sub(shape(rows)),
                StatusRow::FleetSubmenu { .. } => Shape::Fleet,
            })
            .collect()
    }

    #[test]
    fn a_fleet_need_on_a_mac_that_lets_this_one_drive_it_offers_take_over() {
        let driven = PeerRef {
            dial: "lab-mini.local".to_string(),
            name: "lab-mini".to_string(),
            host: Some("lab-mini.local".to_string()),
            sid: "s-ab12".to_string(),
            drive: true,
        };
        let rows = peer_rows(&driven, pick(0));
        assert_eq!(
            shape(&rows),
            vec![
                Shape::Act(StatusAction::PeerRead(pick(0))),
                Shape::Act(StatusAction::PeerTakeOver(pick(0))),
                Shape::Act(StatusAction::PeerHandBack(pick(0))),
                Shape::Act(StatusAction::PeerScreen(pick(0))),
                Shape::Act(StatusAction::PeerSsh(pick(0))),
                Shape::Info,
            ]
        );
        let labels: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                StatusRow::Action { label, .. } => Some(label.as_str()),
                StatusRow::Info(line) => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"Take Over on lab-mini"), "{labels:?}");
        assert!(labels.contains(&"Hand Back on lab-mini"), "{labels:?}");
        assert!(
            labels
                .last()
                .is_some_and(|l| l.contains("person's Take Over wins")),
            "{labels:?}"
        );
        let read_only = peer_rows(
            &PeerRef {
                drive: false,
                ..driven
            },
            pick(0),
        );
        assert_eq!(
            shape(&read_only),
            vec![
                Shape::Act(StatusAction::PeerRead(pick(0))),
                Shape::Act(StatusAction::PeerScreen(pick(0))),
                Shape::Act(StatusAction::PeerSsh(pick(0))),
                Shape::Info,
            ]
        );
        assert!(
            matches!(read_only.last(), Some(StatusRow::Info(line))
                if line.starts_with("Read-only from here") && line.contains("allow drive")),
            "{read_only:?}"
        );
    }

    #[test]
    fn a_hold_this_mac_placed_elsewhere_keeps_its_hand_back() {
        let now = 10_000_i64;
        let read_ms = u64::try_from((now - 20) * 1000).unwrap();
        let held = crate::fleet_glance::FleetMac {
            drive: true,
            held_here: vec![crate::fleet_glance::FleetHeld {
                sid: "s-ab12".to_string(),
                who: "review".to_string(),
            }],
            ..mac("lab-mini", 0, 0, false)
        };
        let g = Glance::build(
            &[],
            &[],
            Some(&fleet_slot(vec![held.clone()], read_ms)),
            unix_ms(now),
            None,
        );
        assert_eq!((g.needs.len(), g.need_count()), (0, 0), "a hold is no need");
        assert_eq!(g.held_there.len(), 1);
        let peer = &g.peers[usize::from(g.held_there[0].at)];
        assert_eq!(
            (peer.dial.as_str(), peer.sid.as_str()),
            ("lab-mini", "s-ab12")
        );
        let menu = compose_status_menu(&g, now);
        let at = menu
            .iter()
            .position(|r| matches!(r, StatusRow::Info(line) if line == "Held from here"))
            .expect("a Held from here group");
        let pick = Pick {
            generation: g.generation,
            index: g.held_there[0].at,
        };
        assert_eq!(
            shape(&menu[at + 1..=at + 1]),
            vec![Shape::Sub(vec![
                Shape::Act(StatusAction::PeerHandBack(pick)),
                Shape::Act(StatusAction::PeerRead(pick)),
                Shape::Info,
            ])]
        );
        let revoked = crate::fleet_glance::FleetMac {
            drive: false,
            ..held
        };
        let g = Glance::build(
            &[],
            &[],
            Some(&fleet_slot(vec![revoked], read_ms)),
            unix_ms(now),
            None,
        );
        let menu = compose_status_menu(&g, now);
        assert!(
            !menu.iter().any(|r| matches!(r, StatusRow::Submenu { rows, .. }
                if rows.iter().any(|r| matches!(r, StatusRow::Action { action: StatusAction::PeerHandBack(_), .. })))),
            "{menu:?}"
        );
        let g = Glance::build(
            &[],
            &[],
            Some(&fleet_slot(vec![mac("lab-mini", 0, 0, false)], read_ms)),
            unix_ms(now),
            None,
        );
        assert!(g.held_there.is_empty());
        assert!(
            !compose_status_menu(&g, now)
                .iter()
                .any(|r| matches!(r, StatusRow::Info(line) if line == "Held from here"))
        );
    }

    fn flat(rows: &[StatusRow], fleet: &[StatusRow], depth: usize) -> Vec<(usize, Option<String>)> {
        let mut out = Vec::new();
        for row in rows {
            match row {
                StatusRow::Separator => out.push((depth, None)),
                StatusRow::Info(label) | StatusRow::Action { label, .. } => {
                    out.push((depth, Some(label.clone())));
                }
                StatusRow::Submenu { label, rows } => {
                    out.push((depth, Some(label.clone())));
                    out.extend(flat(rows, fleet, depth + 1));
                }
                StatusRow::FleetSubmenu { label } => {
                    out.push((depth, Some(label.clone())));
                    out.extend(flat(fleet, &[], depth + 1));
                }
            }
        }
        out
    }

    fn worded(lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| match line.rsplit_once(" at=") {
                Some((words, _)) => words.to_string(),
                None => line,
            })
            .collect()
    }

    #[test]
    fn every_row_path_chrome_prints_names_its_row() {
        let now = 10_000_i64;
        let read_ms = u64::try_from((now - 20) * 1000).unwrap();
        let slot = fleet_slot(
            vec![crate::fleet_glance::FleetMac {
                drive: true,
                ..mac("lab-mini", 1, 1, false)
            }],
            read_ms,
        );
        let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
        let (rows, fleet) = (compose_status_menu(&g, now), compose_fleet_menu(&slot, now));
        let lines = menu_lines(&rows, &fleet);
        let flat = flat(&rows, &fleet, 0);
        assert_eq!(lines.len(), flat.len(), "{lines:#?}");
        let mut depths = std::collections::BTreeSet::new();
        for (line, (depth, label)) in lines.iter().zip(&flat) {
            let (words, at) = line
                .rsplit_once(" at=")
                .unwrap_or_else(|| panic!("no path: {line}"));
            assert!(!at.contains(' '), "the path is the last word: {line}");
            let path = MenuAt::parse(at).unwrap_or_else(|| panic!("{at} is no path"));
            assert_eq!(path.to_string(), at, "printed as it reads");
            assert_eq!(path.0.len(), depth + 1, "a level a path index: {line}");
            let row = row_at(&rows, &fleet, &path).unwrap_or_else(|| panic!("{line}: no row"));
            let named = match row {
                StatusRow::Separator => None,
                StatusRow::Info(label)
                | StatusRow::Action { label, .. }
                | StatusRow::Submenu { label, .. }
                | StatusRow::FleetSubmenu { label } => Some(label.clone()),
            };
            assert_eq!(&named, label, "{line}");
            assert!(label.is_some() || words.ends_with("---"), "{line}");
            depths.insert(*depth);
        }
        assert!(
            depths.contains(&1),
            "PRECONDITION: a submenu's rows were read: {lines:#?}"
        );
        let fleet_child = lines
            .iter()
            .find(|l| l.contains("\"Read the Fleet Now\""))
            .expect("the Fleet row's own rows");
        assert!(
            fleet_child.starts_with("statusmenu   "),
            "PRECONDITION: one level in: {fleet_child}"
        );
        assert_eq!(row_at(&rows, &fleet, &MenuAt(vec![rows.len()])), None);
        let leaf = rows
            .iter()
            .position(|r| matches!(r, StatusRow::Action { .. }))
            .expect("an action row");
        assert_eq!(row_at(&rows, &fleet, &MenuAt(vec![leaf, 0])), None);
        for junk in ["", ".", "1.", "a", "-1", "1.x", "0.0.0.0.0", "1 2"] {
            assert_eq!(MenuAt::parse(junk), None, "{junk:?}");
        }
        assert_eq!(MenuAt::parse("3.0"), Some(MenuAt(vec![3, 0])));
    }

    #[test]
    fn the_menu_dump_is_the_builders_rows_in_order() {
        let now = 10_000_i64;
        let read_ms = u64::try_from((now - 20) * 1000).unwrap();
        let driven = crate::fleet_glance::FleetMac {
            drive: true,
            held_here: vec![crate::fleet_glance::FleetHeld {
                sid: "s-ab12".to_string(),
                who: "review".to_string(),
            }],
            ..mac("lab-mini", 1, 1, false)
        };
        let dump = |mac: crate::fleet_glance::FleetMac| {
            let slot = fleet_slot(vec![mac], read_ms);
            let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
            let (rows, fleet) = (compose_status_menu(&g, now), compose_fleet_menu(&slot, now));
            (worded(menu_lines(&rows, &fleet)), flat(&rows, &fleet, 0))
        };
        let (lines, rows) = dump(driven.clone());
        assert_eq!(lines.len(), rows.len(), "{lines:#?}");
        for (line, (depth, label)) in lines.iter().zip(&rows) {
            let at = format!("statusmenu {}", "  ".repeat(*depth));
            let row = line.strip_prefix(&at).unwrap_or_else(|| panic!("{line}"));
            assert!(!row.starts_with(' '), "{line}");
            match label {
                None => assert_eq!(row, "---"),
                Some(label) => assert!(row.starts_with(&format!("{label:?} ")), "{line}"),
            }
        }
        assert!(
            lines.contains(&format!(
                "statusmenu \"Notify Me When I\u{2019}m Needed\" enabled=yes checked=no tip={:?}",
                notify_tip(None, false)
            )),
            "{lines:#?}"
        );
        assert!(
            lines.contains(
                &"statusmenu   \"Take Over on lab-mini\" enabled=yes checked=no".to_string()
            ),
            "{lines:#?}"
        );
        assert!(
            lines.contains(&"statusmenu \"Held from here\" enabled=no".to_string()),
            "{lines:#?}"
        );
        assert!(
            lines.contains(
                &"statusmenu   \"Read the Fleet Now\" enabled=yes checked=no".to_string()
            ),
            "the Fleet row's own rows: {lines:#?}"
        );
        let (lines, _) = dump(crate::fleet_glance::FleetMac {
            drive: false,
            ..driven
        });
        assert!(
            !lines.iter().any(|line| line.contains("Take Over on")),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("\"Read-only from here: lab-mini")),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_mac_whose_key_is_not_proven_is_listed_and_never_counted_or_driven() {
        let now = 10_000_i64;
        let read_ms = u64::try_from((now - 20) * 1000).unwrap();
        let studio = crate::fleet_glance::FleetMac {
            drive: true,
            unproven: true,
            held_here: vec![crate::fleet_glance::FleetHeld {
                sid: "s-ab12".to_string(),
                who: "review".to_string(),
            }],
            ..mac("studio", 1, 1, false)
        };
        let read = |mac: crate::fleet_glance::FleetMac| {
            let slot = fleet_slot(vec![mac], read_ms);
            let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
            let fleet = compose_fleet_menu(&slot, now);
            let lines = worded(menu_lines(&compose_status_menu(&g, now), &fleet));
            (g, fleet, lines)
        };
        let (g, fleet, lines) = read(studio.clone());
        assert_eq!((g.needs.len(), g.need_count()), (0, 0), "{:?}", g.needs);
        assert!(g.held_there.is_empty() && g.peers.is_empty(), "{g:?}");
        assert_eq!(g.button_title(), "\u{276f}");
        let fold = fleet
            .iter()
            .find_map(|row| match row {
                StatusRow::Submenu { label, rows }
                    if label == "1 Mac's key not proven (not counted: rejoin it)" =>
                {
                    Some(rows)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("its fold: {fleet:?}"));
        let [StatusRow::Submenu { label, rows }] = fold.as_slice() else {
            panic!("one Mac in it: {fold:?}");
        };
        assert_eq!(
            label,
            "studio \u{2014} key not proven \u{b7} 1 need \u{b7} 1 working"
        );
        let infos: Vec<&str> = rows
            .iter()
            .filter_map(|row| match row {
                StatusRow::Info(line) => Some(line.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            infos.contains(
                &"\u{270b} review \u{2014} held from here: its person hands it back there"
            ),
            "{infos:?}"
        );
        assert!(
            infos.last().is_some_and(|line| line
                == &"Update aterm on studio and join it again (`aterm fleet join`); \
                     `aterm fleet peers` shows its KEY"),
            "{infos:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.contains("Take Over on") || line.contains("Hand Back on")),
            "{lines:#?}"
        );
        assert!(
            lines.contains(
                &"statusmenu   \"1 Mac's key not proven (not counted: rejoin it)\" enabled=yes \
                  submenu"
                    .to_string()
            ),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("studio's key is not proven")),
            "{lines:#?}"
        );
        let (g, fleet, lines) = read(crate::fleet_glance::FleetMac {
            unproven: false,
            ..studio
        });
        assert_eq!((g.needs.len(), g.need_count()), (1, 1));
        assert_eq!(g.held_there.len(), 1);
        assert!(
            lines.contains(
                &"statusmenu   \"Take Over on studio\" enabled=yes checked=no".to_string()
            ),
            "{lines:#?}"
        );
        assert!(
            fleet
                .iter()
                .any(|row| matches!(row, StatusRow::Submenu { label, .. }
                if label == "studio \u{2014} 1 needs you \u{b7} 1 working")),
            "{fleet:?}"
        );
        assert!(
            !lines.iter().any(|line| line.contains("not proven")),
            "{lines:#?}"
        );
    }

    #[test]
    fn where_this_mac_binds_no_key_to_its_issuer_its_own_join_proves_them() {
        let now = 10_000_i64;
        let read_at = u64::try_from((now - 20) * 1000).unwrap();
        let unproven = |name| crate::fleet_glance::FleetMac {
            unproven: true,
            ..mac(name, 1, 0, false)
        };
        let read = |unbound: Option<&str>| {
            let mut slot = fleet_slot(vec![unproven("lab-mini"), unproven("studio")], read_at);
            slot.book.issuer = false;
            slot.book.unbound_issuer = unbound.map(str::to_string);
            let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
            assert_eq!(g.need_count(), 0, "nothing of theirs counts either way");
            worded(menu_lines(
                &compose_status_menu(&g, now),
                &compose_fleet_menu(&slot, now),
            ))
        };
        let lines = read(Some("lab-mini"));
        let says = |lines: &[String], text: &str| lines.iter().filter(|l| l.contains(text)).count();
        let join_here = "This Mac binds no key to its issuer lab-mini: join again here once it has \
                         one (`aterm fleet init` there)";
        assert_eq!(
            says(&lines, join_here),
            4,
            "the menu, the Fleet submenu and both rows: {lines:#?}"
        );
        assert_eq!(
            says(
                &lines,
                "\"2 Macs' keys not proven (not counted: join again here)\" enabled=yes submenu"
            ),
            1,
            "{lines:#?}"
        );
        assert_eq!(says(&lines, "Update aterm on"), 0, "{lines:#?}");
        let lines = read(None);
        assert_eq!(says(&lines, "binds no key to its issuer"), 0, "{lines:#?}");
        assert_eq!(
            says(
                &lines,
                "\"2 Macs' keys not proven (not counted: rejoin them)\" enabled=yes submenu"
            ),
            1,
            "{lines:#?}"
        );
        for name in ["lab-mini", "studio"] {
            assert_eq!(
                says(
                    &lines,
                    &format!(
                        "Update aterm on {name} and join it again (`aterm fleet join`); `aterm \
                         fleet peers` shows its KEY"
                    )
                ),
                1,
                "{name}: {lines:#?}"
            );
        }
    }

    fn acts() -> [Shape; 4] {
        [
            Shape::Sep,
            Shape::Act(StatusAction::Overview),
            Shape::Act(StatusAction::ReadThisMac),
            Shape::Act(StatusAction::ToggleNotify),
        ]
    }

    fn pick(index: u16) -> Pick {
        Pick {
            generation: 0,
            index,
        }
    }

    #[test]
    fn needs_are_listed_stuck_first_then_oldest_and_each_goes_to_its_tab() {
        let mut limits = agent_row(1, "limits", "wall:usage-session", 3);
        limits.need_since = Some(500);
        let mut asker = agent_row(3, "asker", "question", 2);
        asker.need_since = Some(100);
        let mut builder = agent_row(4, "builder", "prompt", 5);
        builder.need_since = Some(50);
        let mut zsh = row(5, "zsh", Some("deploy needs a human"));
        zsh.need_since = Some(200);
        let g = local_glance(
            &[
                limits,
                agent_row(2, "tests", "busy", 1),
                asker,
                builder,
                zsh,
                agent_row(6, "idle one", "idle", 1),
            ],
            &[],
            1000,
            None,
        );
        assert_eq!(g.local_need_sessions(), vec![1, 4, 3, 5]);
        assert_eq!(g.need_count(), 4);
        assert_eq!(g.button_title(), "\u{276f}4");
        assert!(g.stuck(), "the wall is stuck");
        let menu = compose_status_menu(&g, 1000);
        let go = |session: u64| {
            let at =
                g.go.iter()
                    .position(|s| *s == session)
                    .expect("a go target");
            Shape::Act(StatusAction::Go(pick(u16::try_from(at).unwrap())))
        };
        assert_eq!(
            shape(&menu[..6]),
            vec![Shape::Info, go(1), go(4), go(3), go(5), Shape::Sep]
        );
        let calm = local_glance(&[agent_row(2, "tests", "busy", 1)], &[], 1000, None);
        assert!(calm.needs.is_empty());
        assert_eq!(calm.button_title(), "\u{276f}");
    }

    #[test]
    fn the_badge_counts_local_and_sibling_needs_and_is_red_only_when_stuck() {
        let sibling = SiblingInstance {
            pid: 900,
            sock: "/tmp/aterm-900.sock".to_string(),
            sessions: 2,
            needs: vec![SiblingNeed {
                sid: "s-sib".to_string(),
                word: NeedWord::Frozen,
                label: "claude".to_string(),
                age_ms: Some(60_000),
            }],
            scanned_ms: unix_ms(990),
        };
        let decide = local_glance(&[row(1, "zsh", Some("look"))], &[], 1000, None);
        assert_eq!((decide.need_count(), decide.stuck()), (1, false));
        let both = local_glance(
            &[row(1, "zsh", Some("look"))],
            std::slice::from_ref(&sibling),
            1000,
            None,
        );
        assert_eq!(both.button_title(), "\u{276f}2");
        assert!(both.stuck(), "the sibling's frozen agent is stuck");
        assert_eq!(both.stuck_count(), 1);
        assert_eq!(both.needs[0].origin, Origin::Sibling(900));
        assert_eq!(both.needs[0].at, NeedAt::Raise(0));
        assert_eq!(both.raises[0].sid, "s-sib");
        assert_eq!(both.here.elsewhere, 2);
        let quiet = SiblingInstance {
            needs: Vec::new(),
            ..sibling
        };
        let g = local_glance(&[], &[quiet], 1000, None);
        assert_eq!((g.need_count(), g.here.elsewhere), (0, 2));
    }

    #[test]
    fn a_sibling_need_counts_only_from_a_young_scan() {
        let scanned_ms = 1_000_000_000_500;
        let fresh = u64::try_from(SIBLING_FRESH.as_millis()).unwrap();
        let sibling = SiblingInstance {
            pid: 900,
            sock: "/tmp/aterm-900.sock".to_string(),
            sessions: 1,
            needs: vec![SiblingNeed {
                sid: "s-sib".to_string(),
                word: NeedWord::Stalled,
                label: "claude".to_string(),
                age_ms: Some(60_000),
            }],
            scanned_ms,
        };
        let at =
            |now_ms: u64| Glance::build(&[], std::slice::from_ref(&sibling), None, now_ms, None);
        let young = at(scanned_ms + fresh);
        assert_eq!(
            (young.need_count(), young.stuck()),
            (1, true),
            "the control"
        );
        assert_eq!(young.needs[0].read_at_ms, None);
        let born = i64::try_from(scanned_ms / 1000).unwrap() - 60;
        assert_eq!(young.needs[0].since_unix, Some(born));
        let old = at(scanned_ms + fresh + 1);
        assert_eq!((old.needs.len(), old.need_count()), (1, 0));
        assert_eq!(old.needs[0].read_at_ms, Some(scanned_ms));
        assert_eq!(old.needs[0].since_unix, Some(born), "aged from the scan");
        assert_eq!(old.button_title(), "\u{276f}");
        assert_eq!(young.fingerprint(), at(scanned_ms + 1).fingerprint());
        assert_ne!(young.fingerprint(), old.fingerprint(), "uncounted moves it");
    }

    #[test]
    fn the_dot_means_something_for_later_and_never_hides_a_need() {
        let mut g = local_glance(&[], &[], 0, None);
        assert_eq!(g.button_title(), "\u{276f}");
        g.exceptions = vec![(11, "macOS keeps asking for access".into())];
        assert_eq!(g.button_title(), "\u{276f}\u{2022}");
        let done = local_glance(
            &[worker(
                2,
                "summary",
                crate::task_record::FROM_CTL,
                TaskState::Done,
                10,
            )],
            &[],
            0,
            None,
        );
        assert_eq!(done.button_title(), "\u{276f}\u{2022}", "a result for you");
        let mut asked = local_glance(&[row(3, "zsh", Some("look"))], &[], 0, None);
        asked.exceptions = g.exceptions.clone();
        assert_eq!(asked.button_title(), "\u{276f}1", "a need wins the title");
    }

    #[test]
    fn sections_appear_only_with_data_in_order() {
        let empty = compose_status_menu(&local_glance(&[], &[], 0, None), 0);
        let mut want = vec![Shape::Info];
        want.extend(acts());
        assert_eq!(shape(&empty), want);

        let manager = SessionRow {
            id: 1,
            sid: "s-manager".to_string(),
            title: "lab-sweep".to_string(),
            agent: "busy",
            ..SessionRow::default()
        };
        let mut waiting = agent_row(5, "limited", "wall:usage-session", 1);
        waiting.need = None;
        waiting.waiting = Some(crate::needs::Waiting {
            word: WaitWord::Limit,
            wall: Some("usage-session"),
            reset: Some("7pm".to_string()),
            entry: None,
        });
        let mut held = agent_row(6, "bench", "busy", 1);
        held.held = Some(HoldWord::Person);
        let rows = [
            manager,
            worker(2, "bench", "s-manager", TaskState::Working, 0),
            worker(3, "lint", "s-manager", TaskState::Done, 2100),
            worker(
                4,
                "summary",
                crate::task_record::FROM_CTL,
                TaskState::Done,
                0,
            ),
            waiting,
            held,
            row(7, "zsh", Some("look at the build")),
        ];
        let mut g = local_glance(&rows, &[], 2000, None);
        g.exceptions = vec![(41, "a fact for later".into())];
        let menu = compose_status_menu(&g, 2000);
        let go = |session: u64| {
            let at =
                g.go.iter()
                    .position(|s| *s == session)
                    .expect("a go target");
            Shape::Act(StatusAction::Go(pick(u16::try_from(at).unwrap())))
        };
        let mut want = vec![
            Shape::Info,
            go(7),
            Shape::Sep,
            Shape::Info,
            Shape::Sub(vec![
                go(1),
                Shape::Sep,
                go(2),
                go(3),
                Shape::Sep,
                Shape::Act(StatusAction::ReadResult(pick(0))),
            ]),
            Shape::Sep,
            Shape::Info,
            go(4),
            Shape::Sep,
            Shape::Sub(vec![go(5)]),
            Shape::Sep,
            Shape::Info,
            go(6),
        ];
        want.extend(
            wrap_words(aterm_types::control_verbs::HOLD_COST, COST_LINE)
                .iter()
                .map(|_| Shape::Info),
        );
        want.extend([
            Shape::Act(StatusAction::HandBackAll),
            Shape::Sep,
            Shape::Info,
            Shape::Act(StatusAction::ShowException(41)),
            Shape::Act(StatusAction::DismissExceptions),
        ]);
        want.extend(acts());
        assert_eq!(shape(&menu), want);
        assert_eq!(g.reads[0].session, 3, "lint's result");
        assert_eq!(
            (g.here.working, g.here.done, g.here.shells),
            (2, 2, 1),
            "the manager and bench work; lint and the summary are done"
        );
        assert_eq!(
            (g.here.need_you, g.here.waiting),
            (1, 1),
            "zsh's ask needs you; the limited tab waits"
        );
        assert_eq!(
            here_line(&g.here),
            "This Mac \u{2014} 2 working \u{b7} 1 need you \u{b7} 1 waiting \u{b7} 2 done \u{b7} 1 shell"
        );
    }

    #[test]
    fn the_you_hold_row_counts_the_refused_writes_and_names_who_waits() {
        let manager = SessionRow {
            id: 1,
            sid: "s-manager".to_string(),
            title: "lab-sweep".to_string(),
            agent: "busy",
            ..SessionRow::default()
        };
        let held = |refused: u64, hold: HoldWord| {
            let mut bench = worker(2, "bench", "s-manager", TaskState::Working, 0);
            bench.held = Some(hold);
            bench.held_since = Some(2000 - 240);
            bench.person_refusals = refused;
            bench
        };
        let glance = |bench: SessionRow| local_glance(&[manager.clone(), bench], &[], 2000, None);
        let line = |g: &Glance| held_line(&g.held[0], 2000);

        let quiet = glance(held(0, HoldWord::Person));
        assert_eq!(quiet.held[0].refused, 0);
        assert_eq!(quiet.held[0].worker_of.as_deref(), Some("lab-sweep"));
        assert_eq!(
            line(&quiet),
            "\u{270b} bench (worker of lab-sweep) \u{2014} yours 4m"
        );
        let refused = glance(held(1, HoldWord::Person));
        assert_eq!(refused.held[0].refused, 1);
        assert_eq!(
            line(&refused),
            "\u{270b} bench (worker of lab-sweep) \u{2014} yours 4m \u{b7} refused 1 write"
        );
        assert_ne!(refused.fingerprint(), quiet.fingerprint());
        assert!(line(&glance(held(3, HoldWord::Person))).ends_with("refused 3 writes"));
        assert_eq!(
            glance(held(0, HoldWord::Person)).fingerprint(),
            quiet.fingerprint()
        );
        let client = glance(held(5, HoldWord::Local));
        assert_eq!(client.held[0].refused, 0);
        assert!(!line(&client).contains("refused"), "{}", line(&client));

        let menu = compose_status_menu(&refused, 2000);
        let infos: Vec<&str> = menu
            .iter()
            .filter_map(|r| match r {
                StatusRow::Info(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let cost = wrap_words(aterm_types::control_verbs::HOLD_COST, COST_LINE);
        assert!(cost.len() > 1, "a paragraph, not one wide row");
        assert!(cost.iter().all(|l| l.chars().count() <= COST_LINE));
        assert_eq!(cost.join(" "), aterm_types::control_verbs::HOLD_COST);
        assert!(
            infos.windows(cost.len()).any(|w| w == cost.as_slice()),
            "{infos:?}"
        );
    }

    #[test]
    fn more_needs_and_managers_than_fit_are_one_overview_row() {
        let mut rows: Vec<SessionRow> = (0..13).map(|id| row(id, "zsh", Some("look"))).collect();
        for id in 0..9 {
            let sid = format!("s-m{id}");
            rows.push(SessionRow {
                id: 100 + id,
                sid: sid.clone(),
                title: format!("m{id}"),
                ..SessionRow::default()
            });
            rows.push(worker(200 + id, "w", &sid, TaskState::Working, 0));
        }
        let g = local_glance(&rows, &[], 0, None);
        let menu = compose_status_menu(&g, 0);
        let overviews = menu
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    StatusRow::Action {
                        action: StatusAction::Overview,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(overviews, 3, "needs, managers, and the act");
        let need_rows = menu
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    StatusRow::Action {
                        action: StatusAction::Go(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(need_rows, NEEDS_SHOWN);
        let managers = menu
            .iter()
            .filter(|r| matches!(r, StatusRow::Submenu { .. }))
            .count();
        assert_eq!(managers, MANAGERS_SHOWN);
    }

    #[test]
    fn done_for_you_lists_what_nobody_here_waits_on() {
        let manager = SessionRow {
            id: 1,
            sid: "s-manager".to_string(),
            title: "lab-sweep".to_string(),
            ..SessionRow::default()
        };
        let mut seen = worker(5, "seen", crate::task_record::FROM_CTL, TaskState::Done, 5);
        seen.task.as_mut().unwrap().seen = true;
        let g = local_glance(
            &[
                manager,
                worker(2, "managed", "s-manager", TaskState::Done, 5),
                worker(3, "orphan", "s-gone", TaskState::Done, 5),
                worker(4, "bare", crate::task_record::FROM_CTL, TaskState::Done, 0),
                seen,
                worker(
                    6,
                    "busy",
                    crate::task_record::FROM_CTL,
                    TaskState::Working,
                    0,
                ),
            ],
            &[],
            0,
            None,
        );
        let labels: Vec<&str> = g.done.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(labels, vec!["orphan", "bare"]);
        assert!(matches!(g.done[0].at, DoneAt::Read(_)));
        assert!(matches!(g.done[1].at, DoneAt::Go(_)));
        assert!(g.managers.iter().any(|m| m.go.is_none()));
    }

    fn ended(id: u64, title: &str, at_unix: i64, seen: bool) -> SessionRow {
        SessionRow {
            id,
            sid: format!("s-{id:020}"),
            title: title.to_string(),
            agent: "idle",
            end: Some(EndRow {
                serial: 1,
                since_unix_ms: unix_ms(at_unix),
                seen,
            }),
            ..SessionRow::default()
        }
    }

    fn done_lines(g: &Glance, now_unix: i64) -> Vec<String> {
        worded(menu_lines(&compose_status_menu(g, now_unix), &[]))
            .into_iter()
            .filter(|l| l.starts_with("statusmenu \"Done for you") || l.contains("\u{2713} "))
            .collect()
    }

    #[test]
    fn an_unseen_end_is_one_go_row_worded_landed() {
        let now = 1_000_000;
        let rows = [
            ended(1, "Calc.py small tested module", now - 41 * 60, false),
            ended(2, "proj-api refactor", now - 3 * 60, false),
            ended(3, "looked at", now - 60, true),
        ];
        let g = local_glance(&rows, &[], now, None);
        let labels: Vec<&str> = g.done.iter().map(|d| d.label.as_str()).collect();
        assert_eq!(
            labels,
            ["proj-api refactor", "Calc.py small tested module"],
            "newest first; the seen end has none"
        );
        let goes: Vec<u64> = g
            .done
            .iter()
            .map(|d| match d.at {
                DoneAt::Go(at) => g.go[usize::from(at)],
                DoneAt::Read(_) => panic!("an end has no result: {d:?}"),
            })
            .collect();
        assert_eq!(goes, [2, 1], "a click goes to the session");
        assert!(g.done.iter().all(|d| d.result_bytes == 0));
        assert_eq!(
            done_lines(&g, now),
            [
                "statusmenu \"Done for you (2)\" enabled=no",
                "statusmenu \"\u{2713} proj-api refactor \u{2014} landed 3m\" enabled=yes checked=no",
                "statusmenu \"\u{2713} Calc.py small tested module \u{2014} landed 41m\" enabled=yes checked=no",
            ]
        );
        assert_eq!(g.button_title(), "\u{276f}\u{2022}");
        let mut unseen = rows.clone();
        unseen[2].end.as_mut().expect("an end").seen = false;
        assert_eq!(local_glance(&unseen, &[], now, None).done.len(), 3);
    }

    #[test]
    fn a_worker_whose_task_row_stands_gets_one_row() {
        let end = Some(EndRow {
            serial: 1,
            since_unix_ms: 2_000_000,
            seen: false,
        });
        let manager = SessionRow {
            id: 1,
            sid: "s-manager".to_string(),
            title: "lab-sweep".to_string(),
            ..SessionRow::default()
        };
        let mut read = worker(
            2,
            "fleet summary",
            crate::task_record::FROM_CTL,
            TaskState::Done,
            1434,
        );
        read.end = end;
        let mut bare = worker(3, "bare", crate::task_record::FROM_CTL, TaskState::Done, 0);
        bare.end = end;
        let mut managed = worker(4, "managed", "s-manager", TaskState::Working, 0);
        managed.end = end;
        let g = local_glance(&[manager, read, bare, managed.clone()], &[], 2_000, None);
        let rows: Vec<(&str, bool)> = g
            .done
            .iter()
            .map(|d| (d.label.as_str(), matches!(d.at, DoneAt::Read(_))))
            .collect();
        assert_eq!(rows, [("fleet summary", true), ("bare", false)], "one each");
        assert_eq!(
            done_lines(&g, 2_000)[1],
            "statusmenu \"\u{2713} fleet summary \u{2014} landed 16m \u{b7} 1.4 KB\" enabled=yes checked=no",
            "the task's own row, worded as its landing"
        );
        assert!(
            g.managers
                .iter()
                .any(|m| m.workers.iter().any(|w| w.label == "managed")),
            "the managed worker's row is under its manager"
        );
        let g = local_glance(&[managed], &[], 2_000, None);
        assert_eq!(g.done.len(), 1);
        assert!(matches!(g.done[0].at, DoneAt::Go(_)));
    }

    #[test]
    fn a_landed_workers_one_row_says_landed() {
        let with_end = |end: Option<bool>| {
            let mut read = worker(
                2,
                "fleet summary",
                crate::task_record::FROM_CTL,
                TaskState::Done,
                1434,
            );
            read.end = end.map(|seen| EndRow {
                serial: 1,
                since_unix_ms: 2_000_000,
                seen,
            });
            local_glance(&[read], &[], 2_000, None)
        };
        for seen in [false, true] {
            let g = with_end(Some(seen));
            assert_eq!(g.done.len(), 1, "one row: {:?}", g.done);
            assert!(matches!(g.done[0].at, DoneAt::Read(_)), "the task's row");
            assert_eq!(
                done_lines(&g, 2_000)[1],
                "statusmenu \"\u{2713} fleet summary \u{2014} landed 16m \u{b7} 1.4 KB\" enabled=yes checked=no",
                "seen={seen}",
            );
        }
        let g = with_end(None);
        assert_eq!(g.done.len(), 1, "the task's row stands: {:?}", g.done);
        assert!(!g.done[0].landed, "no end: done");
    }

    #[test]
    fn the_heading_counts_every_end_and_the_newest_are_shown() {
        let now = 1_000_000;
        let rows: Vec<SessionRow> = (1..=7)
            .map(|id| {
                ended(
                    id,
                    &format!("t{id}"),
                    now - i64::try_from(id).unwrap() * 60,
                    false,
                )
            })
            .collect();
        let g = local_glance(&rows, &[], now, None);
        let lines = done_lines(&g, now);
        assert_eq!(lines[0], "statusmenu \"Done for you (7)\" enabled=no");
        let shown: Vec<&str> = lines[1..]
            .iter()
            .map(|l| {
                l.split('\u{2713}')
                    .nth(1)
                    .unwrap_or_default()
                    .split(' ')
                    .nth(1)
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(shown, ["t1", "t2", "t3", "t4", "t5"]);
        let mut fewer = rows.clone();
        fewer[0].end.as_mut().expect("an end").seen = true;
        let g = local_glance(&fewer, &[], now, None);
        assert_eq!(
            done_lines(&g, now).last().map(|l| l.contains("t6")),
            Some(true)
        );
    }

    #[test]
    fn the_title_lights_for_an_end_row_while_nothing_needs_you() {
        let now = 1_000_000;
        let end = ended(1, "proj-api refactor", now - 60, false);
        let g = local_glance(
            &[end.clone(), agent_row(2, "busy", "busy", 1)],
            &[],
            now,
            None,
        );
        assert_eq!(g.button_title(), "\u{276f}\u{2022}");
        assert_eq!(g.need_count(), 0);
        let g = local_glance(
            &[end.clone(), agent_row(2, "rm", "prompt", 1)],
            &[],
            now,
            None,
        );
        assert_eq!(g.button_title(), "\u{276f}1", "a need wins the title");
        let mut seen = end;
        seen.end.as_mut().expect("an end").seen = true;
        let g = local_glance(&[seen, agent_row(2, "busy", "busy", 1)], &[], now, None);
        assert_eq!(g.button_title(), "\u{276f}", "CONTROL: seen");
    }

    #[test]
    fn an_end_rows_age_is_composed_at_paint() {
        let now = 1_000_000;
        let rows = [ended(1, "proj-api refactor", now - 3 * 60, false)];
        let g = local_glance(&rows, &[], now, None);
        assert!(done_lines(&g, now)[1].contains("landed 3m"));
        assert!(done_lines(&g, now + 2 * 3600)[1].contains("landed 2h"));
        assert_eq!(
            local_glance(&rows, &[], now + 600, None).fingerprint(),
            g.fingerprint()
        );
        let mut untimed = rows.clone();
        untimed[0].end.as_mut().expect("an end").since_unix_ms = 0;
        let g = local_glance(&untimed, &[], now, None);
        assert_eq!(
            done_lines(&g, now)[1],
            "statusmenu \"\u{2713} proj-api refactor \u{2014} landed\" enabled=yes checked=no"
        );
    }

    #[test]
    fn waiting_is_one_row_per_cause() {
        let wait = |id: u64, word: WaitWord, wall: Option<&'static str>| SessionRow {
            id,
            title: format!("t{id}"),
            agent: "busy",
            waiting: Some(crate::needs::Waiting {
                word,
                wall,
                reset: None,
                entry: None,
            }),
            ..SessionRow::default()
        };
        let mut rows: Vec<SessionRow> = (0..8)
            .map(|id| wait(id, WaitWord::Limit, Some("usage-session")))
            .collect();
        rows.push(wait(8, WaitWord::Api, Some("api-error")));
        rows.push(wait(9, WaitWord::Api, Some("api-error")));
        let g = local_glance(&rows, &[], 0, Some(100));
        assert_eq!(g.waiting.len(), 2);
        assert_eq!(g.waiting[0].tabs.len(), 8);
        assert_eq!(g.waiting[1].since_unix, Some(100));
        assert!(g.needs.is_empty(), "a wait is never a need");
        assert_eq!(g.button_title(), "\u{276f}");
    }

    #[test]
    fn a_writers_wait_is_shown_in_its_words() {
        let wait = |id: u64, owner: &str, text: &str| SessionRow {
            id,
            title: format!("tab {id}"),
            agent: "idle",
            waiting: Some(crate::needs::Waiting {
                word: WaitWord::Entry,
                wall: None,
                reset: None,
                entry: Some(crate::needs::WaitEntry {
                    owner: owner.to_string(),
                    text: text.to_string(),
                }),
            }),
            ..SessionRow::default()
        };
        let mut limited = wait(9, "-", "");
        limited.waiting = Some(crate::needs::Waiting {
            word: WaitWord::Limit,
            wall: Some("usage-session"),
            reset: Some("7pm".to_string()),
            entry: None,
        });
        let rows = [
            wait(4, "ci", "waiting for CI on PR 412"),
            wait(5, "ci", "waiting for CI on PR 413"),
            wait(6, "ci", "waiting for CI on PR 413"),
            wait(7, "-", "back after lunch"),
            limited,
        ];
        let g = local_glance(&rows, &[], 0, None);
        let lines: Vec<String> = g.waiting.iter().map(wait_line).collect();
        assert_eq!(
            lines,
            [
                "\u{29d7} tab 4 \u{2014} waiting for CI on PR 412 (ci)",
                "\u{29d7} 2 tabs \u{2014} waiting for CI on PR 413 (ci)",
                "\u{29d7} tab 7 \u{2014} back after lunch",
                "\u{29d7} 1 waiting out the session usage limit \u{2014} resets 7pm",
            ]
        );
        assert_eq!(g.here.waiting, 5, "a wait is never work");
    }

    #[test]
    fn tags_round_trip_and_retired_tags_are_inert() {
        let p = Pick {
            generation: 513,
            index: 65_535,
        };
        let every = [
            StatusAction::DismissExceptions,
            StatusAction::Overview,
            StatusAction::ReadThisMac,
            StatusAction::ToggleNotify,
            StatusAction::HandBackAll,
            StatusAction::ShowException(41),
            StatusAction::Go(p),
            StatusAction::ReadResult(p),
            StatusAction::RaiseSibling(p),
            StatusAction::ReadFleet,
            StatusAction::ReadFleetNow,
            StatusAction::OpenLocalNetwork,
            StatusAction::PeerRead(p),
            StatusAction::PeerScreen(p),
            StatusAction::PeerSsh(p),
            StatusAction::PeerTakeOver(p),
            StatusAction::PeerHandBack(p),
            StatusAction::MacRead(p),
            StatusAction::MacScreen(p),
            StatusAction::MacSsh(p),
        ];
        for a in every {
            assert_ne!(a.tag(), 0, "{a:?}");
            assert_eq!(StatusAction::from_tag(a.tag()), Some(a));
        }
        let tags: std::collections::HashSet<isize> = every.iter().map(|a| a.tag()).collect();
        assert_eq!(tags.len(), every.len(), "no two actions share a tag");
        for retired in [
            1,
            2,
            3,
            4,
            TAG_BAND + 7,
            2 * TAG_BAND + 3,
            3 * TAG_BAND + 4242,
        ] {
            assert_eq!(StatusAction::from_tag(retired), None, "{retired}");
        }
        assert_eq!(StatusAction::from_tag(0), None);
        assert_eq!(StatusAction::from_tag(99), None);
        assert_eq!(StatusAction::from_tag(99 * TAG_BAND + 5), None);
        assert_eq!(StatusAction::ShowException(u64::MAX).tag(), 0);
        assert_eq!(StatusAction::from_tag(5 * TAG_BAND + (1 << 33)), None);
    }

    #[test]
    fn the_fingerprint_is_unmoved_by_ages_alone() {
        let rows = |since: i64, ms: u64| {
            let mut asked = row(1, "zsh", Some("look"));
            asked.need_since = Some(since);
            let mut held = agent_row(2, "bench", "busy", 1);
            held.held = Some(HoldWord::Local);
            held.held_since = Some(since);
            let mut w = worker(3, "lint", "s-gone", TaskState::Done, 9);
            w.task.as_mut().unwrap().since_unix_ms = ms;
            vec![asked, held, w]
        };
        let a = local_glance(&rows(100, 1_000), &[], 200, None);
        let b = local_glance(&rows(150, 90_000), &[], 900, None);
        assert_ne!(a, b, "the ages differ");
        assert_eq!(a.fingerprint(), b.fingerprint());
        let mut more = rows(100, 1_000);
        more.push(row(4, "vim", Some("another")));
        assert_ne!(
            a.fingerprint(),
            local_glance(&more, &[], 200, None).fingerprint()
        );
        let mut regen = a.clone();
        regen.generation = 9;
        assert_eq!(a.fingerprint(), regen.fingerprint());
    }

    #[test]
    fn the_bar_button_is_refaced_only_when_its_face_changes() {
        let calm = local_glance(&[row(1, "zsh", None)], &[], 0, None);
        let asked = local_glance(&[row(1, "zsh", Some("look"))], &[], 0, None);
        let asked_more = local_glance(&[row(1, "zsh", Some("look again"))], &[], 0, None);
        assert_ne!(asked.fingerprint(), asked_more.fingerprint());
        let face = |g: &Glance| (g.button_title(), g.stuck());
        assert_eq!(button_face_change(None, &calm), Some(face(&calm)));
        assert_eq!(button_face_change(Some(&face(&calm)), &calm), None);
        assert_eq!(
            button_face_change(Some(&face(&calm)), &asked),
            Some(face(&asked))
        );
        assert_eq!(button_face_change(Some(&face(&asked)), &asked_more), None);
        let stuck = local_glance(&[agent_row(1, "w", "wall:auth", 1)], &[], 0, None);
        assert_eq!(stuck.button_title(), asked.button_title(), "one need each");
        assert_eq!(
            button_face_change(Some(&face(&asked)), &stuck),
            Some(face(&stuck)),
            "the colour alone re-faces it"
        );
    }

    #[test]
    fn the_tab_name_drops_status_glyphs_and_a_title_raises_nothing() {
        for frame in [
            "\u{25d0} Summarise the fleet",
            "\u{25d1} Summarise the fleet",
            "\u{2733} Summarise the fleet",
            "\u{280b} Summarise the fleet",
            "  \u{2819} Summarise the fleet ",
        ] {
            assert_eq!(
                tab_name(None, None, frame).as_deref(),
                Some("Summarise the fleet")
            );
        }
        assert_eq!(
            tab_name(Some("lab-sweep"), Some("mine"), "osc").as_deref(),
            Some("lab-sweep")
        );
        assert_eq!(
            tab_name(Some("  "), Some("mine"), "osc").as_deref(),
            Some("mine")
        );
        assert_eq!(
            tab_name(None, None, "\u{2733}fleet").as_deref(),
            Some("\u{2733}fleet")
        );
        assert_eq!(tab_name(None, Some(" "), " \u{2733} ").as_deref(), None);
        let g = local_glance(
            &[
                row(9, "\u{26a0} needs human: approval", None),
                row(8, "Operator: fleet idle", None),
            ],
            &[],
            0,
            None,
        );
        assert!(g.needs.is_empty());
        let g = local_glance(
            &[row(9, "\u{26a0} needs human", Some("approval"))],
            &[],
            0,
            None,
        );
        assert_eq!(g.local_need_sessions(), vec![9]);
    }

    #[test]
    fn the_herald_remembers_since_when_a_need_and_a_hold_stand() {
        let asked = |key: u64, hold: Option<HoldWord>| SessionRow {
            id: 1,
            title: "zsh".to_string(),
            agent: "busy",
            need: Some(crate::needs::Need {
                word: NeedWord::Ask,
                key,
                what: "look".to_string(),
                harness: None,
            }),
            held: hold,
            ..SessionRow::default()
        };
        let mut h = Herald::default();
        assert!(h.observe(1, Some(&asked(7, None)), 100));
        assert!(!h.observe(1, Some(&asked(7, None)), 130), "the same row");
        assert!(h.observe(1, Some(&asked(7, Some(HoldWord::Local))), 160));
        assert_eq!((h.need_since(1), h.held_since(1)), (Some(100), Some(160)));
        assert!(h.observe(1, Some(&asked(8, Some(HoldWord::Local))), 200));
        assert_eq!((h.need_since(1), h.held_since(1)), (Some(200), Some(160)));
        assert!(h.observe(1, Some(&asked(8, Some(HoldWord::Person))), 300));
        assert_eq!(h.held_since(1), Some(300), "another hold is a new one");
        let mut answered = asked(8, Some(HoldWord::Person));
        answered.agent = "prompt";
        assert!(!h.observe(1, Some(&answered), 310), "one class");
        assert!(h.observe(1, None, 400));
        assert_eq!((h.need_since(1), h.held_since(1)), (None, None));
        assert!(h.since.is_empty());
    }

    #[test]
    fn the_status_carry_row_reads_back_a_part_at_a_time() {
        let full = StatusCarry {
            need: Some((NeedWord::Frozen, 100)),
            told: true,
            held: Some((HoldWord::Local, 90)),
            done: Some(1_000_000),
        };
        assert_eq!(StatusCarry::parse(&full.row().expect("a row")), full);
        let untold = StatusCarry {
            told: false,
            held: None,
            ..full
        };
        assert_eq!(StatusCarry::parse(&untold.row().expect("a row")), untold);
        assert_eq!(StatusCarry::default().row(), None);
        assert_eq!(
            StatusCarry::parse("told=1 done=5"),
            StatusCarry {
                done: Some(5),
                ..StatusCarry::default()
            }
        );
        assert_eq!(
            StatusCarry::parse("need=zapped@100 told=1 hold=local@x later=1 done=-3"),
            StatusCarry::default()
        );
    }

    #[test]
    fn an_adopted_standing_need_posts_nothing_and_a_new_one_posts() {
        let t0 = std::time::Instant::now();
        let mut before = Herald::default();
        let boxed = agent_row(4, "builder", "prompt", 5);
        let _ = before.observe(4, Some(&boxed), 100);
        assert!(herald_note(&mut before, &boxed, false, t0).notice.is_some());
        let told = before.carry_of(4);
        assert!(told.told, "PRECONDITION: told and nothing owed");
        let told = StatusCarry::parse(&told.row().expect("a row"));
        let reboxed = agent_row(9, "builder", "prompt", 1);
        let successor = |carry: StatusCarry, at: std::time::Instant| {
            let mut h = Herald::default();
            h.adopt(9, carry, 1_000, t0);
            let _ = h.observe(9, Some(&reboxed), 1_010);
            let out = herald_note(&mut h, &reboxed, false, at);
            (h, out)
        };
        let (mut h, adopted) = successor(told, t0);
        assert!(
            adopted.notice.is_none() && !adopted.transition,
            "{adopted:?}"
        );
        assert_eq!(h.need_since(9), Some(100), "its age carried");
        let next = herald_note(
            &mut h,
            &agent_row(9, "builder", "prompt", 2),
            false,
            t0 + NOTIFY_SESSION_FLOOR,
        );
        assert!(next.notice.is_some(), "a new box after it posts");

        let untold = StatusCarry {
            told: false,
            ..told
        };
        assert!(successor(untold, t0).1.notice.is_some(), "owed: told now");
        let other = StatusCarry {
            need: Some((NeedWord::Ask, 100)),
            ..told
        };
        assert!(successor(other, t0).1.notice.is_some(), "another need");
        assert!(
            successor(told, t0 + ADOPT_SETTLE).1.notice.is_some(),
            "past the settle"
        );
    }

    #[test]
    fn a_carried_need_is_matched_by_its_word_not_its_kind() {
        let t0 = std::time::Instant::now();
        let carried = |word: NeedWord| StatusCarry {
            need: Some((word, 100)),
            told: true,
            ..StatusCarry::default()
        };
        let first_note = |carry: StatusCarry, verdict: &'static str| {
            let mut h = Herald::default();
            h.adopt(9, carry, 1_000, t0);
            let row = agent_row(9, "builder", verdict, 1);
            let _ = h.observe(9, Some(&row), 1_010);
            herald_note(&mut h, &row, false, t0)
        };
        assert_eq!(
            agent_row(9, "b", "wall:auth", 1).need.map(|n| n.word),
            Some(NeedWord::Login),
            "PRECONDITION"
        );
        assert_eq!(
            agent_row(9, "b", "wall:memory", 1).need.map(|n| n.word),
            Some(NeedWord::Memory),
            "PRECONDITION"
        );
        assert!(
            first_note(carried(NeedWord::Login), "wall:auth")
                .notice
                .is_none(),
            "the carried word: told already"
        );
        assert!(
            first_note(carried(NeedWord::Login), "wall:memory")
                .notice
                .is_some(),
            "the same kind, another word: told"
        );
        assert!(
            first_note(carried(NeedWord::Wall), "wall:auth")
                .notice
                .is_some(),
            "a told wall says nothing of a login"
        );
    }

    #[test]
    fn the_menu_lists_the_digest_and_offers_to_file_it() {
        let mut g = local_glance(&[], &[], 0, None);
        g.exceptions = (0..7).map(|n| (n, format!("thing {n}"))).collect();
        let menu = compose_status_menu(&g, 0);
        let shown = menu
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    StatusRow::Action {
                        action: StatusAction::ShowException(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(shown, EXCEPTION_LINES_SHOWN);
        assert_eq!(
            menu.iter()
                .filter(|r| matches!(
                    r,
                    StatusRow::Action {
                        action: StatusAction::DismissExceptions,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_ne!(
            g.fingerprint(),
            local_glance(&[], &[], 0, None).fingerprint()
        );
    }

    #[test]
    fn notify_me_says_whose_banner_a_notice_is() {
        use crate::notice_center::Center;
        let tip = |voice: Option<NoticeVoice>, on: bool| {
            let mut g = local_glance(&[], &[], 0, None);
            g.voice = voice;
            g.alerts = on;
            let menu = compose_status_menu(&g, 0);
            assert_eq!(
                menu.iter()
                    .filter(|r| matches!(r, StatusRow::Action { tip: Some(_), .. }))
                    .count(),
                1
            );
            menu.into_iter()
                .find_map(|r| match r {
                    StatusRow::Action {
                        action: StatusAction::ToggleNotify,
                        tip,
                        ..
                    } => tip,
                    _ => None,
                })
                .expect("a hover")
        };
        let voice = |center, fallback| Some(NoticeVoice { center, fallback });
        let fallbacks = [
            NoticeFallback::Withheld { named: true },
            NoticeFallback::Withheld { named: false },
            NoticeFallback::Notifier,
            NoticeFallback::ScriptEditor,
        ];
        let mut cases = vec![(None, false)];
        for fallback in fallbacks {
            cases.push((voice(Center::Absent, fallback), false));
        }
        let without = tip(voice(Center::Absent, NoticeFallback::ScriptEditor), false);
        let found = tip(voice(Center::Absent, NoticeFallback::Notifier), false);
        assert!(without.contains("Script Editor's banners"), "{without}");
        for bare in [&without, &found] {
            assert!(
                bare.contains("they are terminal-notifier's banners"),
                "{bare}"
            );
            assert!(bare.contains("not running from aterm.app"), "{bare}");
        }
        let unread = tip(
            voice(Center::Absent, NoticeFallback::Withheld { named: false }),
            false,
        );
        assert!(unread.contains("none reach the desktop"), "{unread}");
        assert!(
            unread.contains("move aterm.app to /Applications (or ~/Applications)"),
            "{unread}"
        );
        assert!(unread.contains("they are aterm's own"), "{unread}");
        let unopened = tip(
            voice(Center::Absent, NoticeFallback::Withheld { named: true }),
            true,
        );
        assert!(unopened.contains("none reach the desktop"), "{unopened}");
        assert!(unopened.contains("aterm.log says why"), "{unopened}");
        let bundled = NoticeFallback::Withheld { named: true };
        cases.push((voice(Center::Unasked, bundled), true));
        cases.push((voice(Center::Unasked, bundled), false));
        cases.push((voice(Center::Refused, bundled), false));
        cases.push((voice(Center::Granted, bundled), false));
        let tips: std::collections::HashSet<String> =
            cases.iter().map(|&(v, on)| tip(v, on)).collect();
        assert_eq!(tips.len(), cases.len(), "{tips:#?}");
        let granted = tip(voice(Center::Granted, bundled), true);
        assert!(
            granted.contains("about a tab on this Mac goes to that tab"),
            "{granted}"
        );
        assert!(
            granted.contains("any other brings aterm forward"),
            "{granted}"
        );
        let unasked = tip(voice(Center::Unasked, bundled), false);
        assert!(unasked.contains("Turning this on asks macOS"), "{unasked}");
        assert!(
            unasked.contains("if macOS says no, none reach the desktop"),
            "{unasked}"
        );
        let asking = tip(voice(Center::Unasked, bundled), true);
        assert!(asking.contains("the first one asks macOS"), "{asking}");
        assert!(asking.contains("none reach the desktop"), "{asking}");
        let refused = tip(voice(Center::Refused, bundled), false);
        assert!(refused.contains("none reach the desktop"), "{refused}");
        for fallback in fallbacks {
            for on in [true, false] {
                assert_eq!(granted, tip(voice(Center::Granted, fallback), on));
                assert_eq!(refused, tip(voice(Center::Refused, fallback), on));
            }
            assert_eq!(unasked, tip(voice(Center::Unasked, fallback), false));
            assert_eq!(asking, tip(voice(Center::Unasked, fallback), true));
        }
        for bundled in [&granted, &refused, &unasked, &asking, &unread, &unopened] {
            for banner in ["Script Editor", "terminal-notifier"] {
                assert!(!bundled.contains(banner), "{bundled}");
            }
        }
    }

    #[test]
    fn notify_me_is_checked_with_desktop_alerts() {
        for alerts in [true, false] {
            let mut g = local_glance(&[], &[], 0, None);
            g.alerts = alerts;
            let checked = compose_status_menu(&g, 0).iter().any(|r| {
                matches!(
                    r,
                    StatusRow::Action {
                        action: StatusAction::ToggleNotify,
                        checked: true,
                        ..
                    }
                )
            });
            assert_eq!(checked, alerts);
        }
    }

    fn fleet_slot(
        macs: Vec<crate::fleet_glance::FleetMac>,
        read_at_ms: u64,
    ) -> crate::fleet_glance::FleetSlot {
        let macs = macs
            .into_iter()
            .map(|mac| crate::fleet_glance::FleetMac { read_at_ms, ..mac })
            .collect();
        crate::fleet_glance::FleetSlot {
            generation: 3,
            snapshot: Some(std::sync::Arc::new(crate::fleet_glance::FleetSnapshot {
                read_at_ms,
                macs,
                unanswered: vec![crate::fleet_glance::Unanswered {
                    name: "mac-22".to_string(),
                    dial: "mac-22".to_string(),
                    why: "no connection in 3 s".to_string(),
                }],
                no_route: false,
            })),
            book: crate::fleet_glance::BookFacts {
                in_fleet: true,
                issuer: true,
                issuer_label: None,
                unbound_issuer: None,
            },
            ..crate::fleet_glance::FleetSlot::default()
        }
    }

    fn mac(
        label: &str,
        needs: usize,
        working: usize,
        older: bool,
    ) -> crate::fleet_glance::FleetMac {
        crate::fleet_glance::FleetMac {
            name: label.to_string(),
            dial: label.to_string(),
            host: crate::fleet_glance::host_word(label),
            older,
            sessions: needs + working,
            agents: needs + working,
            working,
            needs: (0..needs)
                .map(|n| crate::fleet_glance::FleetNeed {
                    sid: format!("s-{n}"),
                    word: NeedWord::Stalled,
                    who: "claude".to_string(),
                    worker_of: Some("s-1e91".to_string()),
                    age_ms: Some(60_000),
                    what: None,
                })
                .collect(),
            ..crate::fleet_glance::FleetMac::default()
        }
    }

    #[test]
    fn counted_needs_are_shown_before_an_old_reads() {
        let now = 10_000_i64;
        let read_ms = |ago: i64| u64::try_from((now - ago) * 1000).unwrap();
        let mut slot = fleet_slot(
            vec![mac("mac-old", 3, 0, false), mac("mac-17", 1, 0, false)],
            read_ms(20),
        );
        let mut snapshot = (**slot.snapshot.as_ref().expect("a read")).clone();
        snapshot.macs[0].read_at_ms = read_ms(301);
        slot.snapshot = Some(std::sync::Arc::new(snapshot));
        let rows: Vec<SessionRow> = (0..9).map(|id| row(id, "zsh", Some("look"))).collect();
        let g = Glance::build(&rows, &[], Some(&slot), unix_ms(now), None);
        assert_eq!((g.needs.len(), g.need_count()), (13, 10));
        assert!(g.needs[..NEEDS_SHOWN].iter().all(|n| n.counted));
        let menu = compose_status_menu(&g, now);
        let kinds: Vec<&str> = menu[1..=NEEDS_SHOWN + 1]
            .iter()
            .map(|r| match r {
                StatusRow::Submenu { .. } => "fleet",
                StatusRow::Action {
                    action: StatusAction::Go(_),
                    ..
                } => "go",
                StatusRow::Action {
                    action: StatusAction::Overview,
                    ..
                } => "more",
                _ => "other",
            })
            .collect();
        let mut expected = vec!["fleet"];
        expected.extend(["go"; 9]);
        expected.push("more");
        assert_eq!(kinds, expected);
    }

    #[test]
    fn fleet_needs_count_only_from_a_fresh_read() {
        let now = 10_000_i64;
        let read_ms = |ago: i64| u64::try_from((now - ago) * 1000).unwrap();
        let slot = fleet_slot(
            vec![mac("mac-17", 1, 0, false), mac("mac-old", 0, 1, true)],
            read_ms(20),
        );
        let g = Glance::build(&[], &[], Some(&slot), unix_ms(now), None);
        assert_eq!(g.needs.len(), 1);
        assert_eq!((g.need_count(), g.stuck()), (1, true));
        assert_eq!(g.button_title(), "\u{276f}1");
        assert!(g.in_fleet());
        let stale = fleet_slot(vec![mac("mac-17", 1, 0, false)], read_ms(301));
        let g_stale = Glance::build(&[], &[], Some(&stale), unix_ms(now), None);
        assert_eq!((g_stale.needs.len(), g_stale.need_count()), (1, 0));
        assert_eq!(g_stale.button_title(), "\u{276f}");
        let mut failed = slot.clone();
        failed.error = Some("no answer in 10 s: killed".to_string());
        let g_failed = Glance::build(&[], &[], Some(&failed), unix_ms(now), None);
        assert_eq!(
            g_failed.button_title(),
            g.button_title(),
            "a failed read never moves it"
        );
        let menu = compose_status_menu(&g, now);
        let sub = menu
            .iter()
            .find_map(|r| match r {
                StatusRow::Submenu { rows, .. } => Some(shape(rows)),
                _ => None,
            })
            .expect("the fleet need's submenu");
        assert_eq!(
            sub,
            vec![
                Shape::Act(StatusAction::PeerRead(pick(0))),
                Shape::Act(StatusAction::PeerScreen(pick(0))),
                Shape::Act(StatusAction::PeerSsh(pick(0))),
                Shape::Info,
            ]
        );
        assert!(
            menu.iter()
                .any(|r| matches!(r, StatusRow::FleetSubmenu { .. }))
        );
        assert!(menu.iter().any(|r| matches!(
            r,
            StatusRow::Action {
                action: StatusAction::ReadFleet,
                ..
            }
        )));
        assert!(!menu.iter().any(|r| matches!(
            r,
            StatusRow::Action {
                action: StatusAction::ReadThisMac,
                ..
            }
        )));
        let mut alone = slot.clone();
        alone.book.in_fleet = false;
        let menu = compose_status_menu(
            &Glance::build(&[], &[], Some(&alone), unix_ms(now), None),
            now,
        );
        assert!(
            !menu
                .iter()
                .any(|r| matches!(r, StatusRow::FleetSubmenu { .. }))
        );
        assert!(menu.iter().any(|r| matches!(
            r,
            StatusRow::Action {
                action: StatusAction::ReadThisMac,
                ..
            }
        )));
    }

    #[test]
    fn a_fleet_need_counts_by_its_own_read_to_the_millisecond() {
        let fresh = u64::try_from(crate::fleet_glance::COUNT_FRESH.as_millis()).unwrap();
        let read_at = 1_000_000_123;
        let slot = fleet_slot(vec![mac("mac-17", 1, 0, false)], read_at);
        let count = |slot: &crate::fleet_glance::FleetSlot, now_ms| {
            Glance::build(&[], &[], Some(slot), now_ms, None).need_count()
        };
        assert_eq!(count(&slot, read_at + fresh), 1, "the control");
        assert_eq!(count(&slot, read_at + fresh + 1), 0);
        let mut carried = fleet_slot(vec![mac("mac-busy", 0, 2, false)], read_at + 60_000);
        if let Some(snap) = carried.snapshot.as_mut() {
            std::sync::Arc::make_mut(snap)
                .macs
                .push(crate::fleet_glance::FleetMac {
                    read_at_ms: read_at,
                    carried: true,
                    ..mac("mac-22", 1, 0, false)
                });
        }
        assert_eq!(count(&carried, read_at + fresh), 1);
        assert_eq!(
            count(&carried, read_at + fresh + 1),
            0,
            "its own read lapsed"
        );
        let g = Glance::build(&[], &[], Some(&carried), read_at + 60_000, None);
        assert!(
            matches!(
                g.fleet,
                FleetLine::InFleet {
                    read: Some((1, 2, 2, _)),
                    ..
                }
            ),
            "mac-busy answered (2 agents) of 2 asked: {:?}",
            g.fleet
        );
    }

    #[test]
    fn the_fleet_submenu_lists_macs_needs_first_and_folds_the_rest() {
        let mut hostless = mac("x y", 0, 2, false);
        hostless.host = None;
        let unproven = |mac| crate::fleet_glance::FleetMac {
            unproven: true,
            ..mac
        };
        let slot = fleet_slot(
            vec![
                mac("mac-busy", 0, 3, false),
                mac("mac-17", 2, 0, false),
                mac("mac-q1", 0, 0, false),
                mac("mac-q2", 0, 0, false),
                unproven(mac("mac-old", 0, 1, true)),
                hostless,
                unproven(mac("mac-key", 3, 0, false)),
            ],
            1000,
        );
        let at = |index: u16| Pick {
            generation: 3,
            index,
        };
        let menu = compose_fleet_menu(&slot, 10);
        let mac_acts = |index: u16, host: bool| {
            let mut acts = vec![Shape::Act(StatusAction::MacRead(at(index)))];
            if host {
                acts.push(Shape::Act(StatusAction::MacScreen(at(index))));
                acts.push(Shape::Act(StatusAction::MacSsh(at(index))));
            }
            acts
        };
        let tail = |rows: &[StatusRow]| match rows {
            [.., StatusRow::Separator, StatusRow::Action { action, .. }] => Some(*action),
            _ => None,
        };
        let shapes = shape(&menu);
        let firsts: Vec<Vec<Shape>> = shapes
            .iter()
            .take(3)
            .map(|s| match s {
                Shape::Sub(rows) => rows
                    .iter()
                    .skip_while(|r| !matches!(r, Shape::Sep))
                    .skip(1)
                    .map(|r| match r {
                        Shape::Act(a) => Shape::Act(*a),
                        _ => Shape::Info,
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(
            firsts,
            vec![mac_acts(1, true), mac_acts(0, true), mac_acts(5, false)]
        );
        assert!(
            matches!(&menu[3], StatusRow::Submenu { rows, .. } if rows.len() == 2),
            "2 quiet"
        );
        assert!(
            matches!(&menu[4], StatusRow::Submenu { rows, .. } if rows.len() == 1),
            "1 not answering"
        );
        assert!(
            matches!(&menu[5], StatusRow::Submenu { label, rows }
                if rows.len() == 1 && label.contains("key not proven")),
            "1 unproven, though it has the most needs"
        );
        assert!(
            matches!(&menu[6], StatusRow::Submenu { label, rows }
                if rows.len() == 1 && label.contains("older aterm")),
            "1 older"
        );
        assert_eq!(tail(&menu), Some(StatusAction::ReadFleetNow));
        let mut dark = slot.clone();
        if let Some(snap) = dark.snapshot.as_mut() {
            std::sync::Arc::make_mut(snap).no_route = true;
        }
        assert!(matches!(
            compose_fleet_menu(&dark, 10).first(),
            Some(StatusRow::Action {
                action: StatusAction::OpenLocalNetwork,
                ..
            })
        ));
        let mut unread = slot.clone();
        unread.snapshot = None;
        assert_eq!(
            shape(&compose_fleet_menu(&unread, 10)),
            vec![
                Shape::Info,
                Shape::Sep,
                Shape::Act(StatusAction::ReadFleetNow)
            ]
        );
    }

    const NEED_A_CUT: &str = "grant Screen Recording to com.aterm.aterm in System Settings \u{25b8} \
                              Priv\u{2026}";

    fn harness_row(id: u64, title: &str, first: &str, since: i64) -> SessionRow {
        let facts = NeedFacts {
            attention: vec![crate::needs::AttentionFact {
                owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
                text: format!("claude needs you: {first} (+1 more) \u{2014} carrying on"),
                kind: Some(aterm_types::needs::AttentionKind::Ask),
            }],
            ..NeedFacts::default()
        };
        SessionRow {
            need_since: Some(since),
            ..row_of(id, title, &facts)
        }
    }

    #[test]
    fn a_need_row_keeps_its_age_and_clips_the_need() {
        let now = 10_000_i64;
        let label = "proj-api refactor: the gateway, the store and the CLI (v2.1)";
        assert_eq!(label.len(), 60);
        let g = local_glance(
            &[harness_row(1, label, NEED_A_CUT, now - 180)],
            &[],
            now,
            None,
        );
        assert!(g.needs[0].harness.is_some(), "the row carries the words");
        let line = need_line(&g.needs[0], now, 0);
        assert!(line.len() <= LINE_MAX, "{} bytes: {line:?}", line.len());
        assert!(line.ends_with(" (+1 more) \u{b7} 3m"), "{line:?}");
        assert!(
            line.starts_with(&format!("\u{25c7} {label} \u{2014} grant Screen Recording")),
            "{line:?}"
        );
        assert!(!line.contains("needs you"), "{line:?}");
        let long = "x".repeat(150);
        let g = local_glance(
            &[harness_row(1, &long, NEED_A_CUT, now - 180)],
            &[],
            now,
            None,
        );
        let line = need_line(&g.needs[0], now, 0);
        assert!(
            line.len() <= LINE_MAX && line.ends_with(" \u{b7} 3m"),
            "{line:?}"
        );
        assert!(line.contains("\u{2014} grant Screen"), "{line:?}");
    }

    #[test]
    fn a_harness_need_reads_whole_on_the_banner() {
        let row = harness_row(1, "proj-api refactor", NEED_A_CUT, 0);
        let body = escalation(&row).expect("a need").body;
        assert_eq!(body, format!("proj-api refactor: {NEED_A_CUT} (+1 more)"));
    }

    #[test]
    fn the_shown_key_folds_the_harness_words() {
        let a = harness_row(1, "t", NEED_A_CUT, 0);
        let mut b = a.clone();
        if let Some(need) = b.need.as_mut()
            && let Some(ask) = need.harness.as_mut()
        {
            ask.more = 7;
        }
        assert_ne!(shown_key(&a), shown_key(&b));
    }

    fn told_facts(needs: &[&str]) -> NeedFacts {
        let more = needs.len() - 1;
        let tail = if more == 0 {
            String::new()
        } else {
            format!(" (+{more} more)")
        };
        let entry = format!("claude needs you: {}{tail} \u{2014} carrying on", needs[0]);
        let set: Vec<String> = needs.iter().map(|need| (*need).to_string()).collect();
        let mut told = crate::needs::ToldNeeds::default();
        told.tell(
            crate::needs::HarnessNeeds::told(&set, &entry).map(std::sync::Arc::new),
            Some(&entry),
        );
        NeedFacts {
            attention: vec![crate::needs::AttentionFact {
                owner: crate::harness_host::NEEDS_ATTENTION_OWNER.to_string(),
                text: entry,
                kind: Some(aterm_types::needs::AttentionKind::Ask),
            }],
            told,
            ..NeedFacts::default()
        }
    }

    fn told_row(id: u64, title: &str, needs: &[&str], since: i64) -> SessionRow {
        SessionRow {
            need_since: Some(since),
            ..row_of(id, title, &told_facts(needs))
        }
    }

    fn named_needs(title: &str, n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("{title} {i}")).collect()
    }

    fn needs_section(menu: &[StatusRow]) -> &[StatusRow] {
        let end = menu
            .iter()
            .position(|r| matches!(r, StatusRow::Separator))
            .unwrap_or(menu.len());
        &menu[1..end]
    }

    fn further_labels(rows: &[StatusRow]) -> Vec<&str> {
        rows.iter()
            .filter_map(|r| match r {
                StatusRow::Action {
                    label, indent: 1, ..
                } => Some(label.as_str()),
                _ => None,
            })
            .collect()
    }

    fn own_row<'a>(rows: &'a [StatusRow], title: &str) -> &'a str {
        rows.iter()
            .find_map(|r| match r {
                StatusRow::Action {
                    label, indent: 0, ..
                } if label.starts_with(&format!("\u{25c7} {title} \u{2014} ")) => {
                    Some(label.as_str())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no row of {title}: {rows:?}"))
    }

    #[test]
    fn every_further_need_reads_on_a_row_of_its_own_capped() {
        let now = 10_000_i64;
        let three = named_needs("three", 3);
        let five = named_needs("five", 5);
        fn refs(v: &[String]) -> Vec<&str> {
            v.iter().map(String::as_str).collect()
        }
        let g = local_glance(
            &[
                told_row(1, "one", &["one 1"], now - 300),
                told_row(2, "three", &refs(&three), now - 200),
                told_row(3, "five", &refs(&five), now - 100),
            ],
            &[],
            now,
            None,
        );
        let menu = compose_status_menu(&g, now);
        let section = needs_section(&menu);
        assert_eq!(
            further_labels(section),
            vec!["three 2", "three 3", "five 2", "five 3 (+2 more)"],
            "{section:#?}"
        );
        let order: Vec<&str> = section
            .iter()
            .filter_map(|r| match r {
                StatusRow::Action { label, .. } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(order.len(), 7, "{order:#?}");
        assert!(
            order[0].starts_with("\u{25c7} one \u{2014} one 1"),
            "{order:#?}"
        );
        assert!(
            order[1].starts_with("\u{25c7} three \u{2014} three 1"),
            "{order:#?}"
        );
        assert_eq!(&order[2..4], ["three 2", "three 3"]);
        assert!(
            order[4].starts_with("\u{25c7} five \u{2014} five 1"),
            "{order:#?}"
        );
        assert_eq!(&order[5..7], ["five 2", "five 3 (+2 more)"]);
        assert!(!own_row(section, "one").contains("more)"));
        assert!(
            !own_row(section, "three").contains("more)"),
            "every need has its row"
        );
        assert!(
            own_row(section, "five").ends_with("five 1 \u{b7} 1m"),
            "the count rides the last row under it: {}",
            own_row(section, "five")
        );
        assert!(
            !section.iter().any(|r| matches!(
                r,
                StatusRow::Action {
                    action: StatusAction::Overview,
                    ..
                }
            )),
            "no overflow row"
        );

        let fives: Vec<Vec<String>> = (1..=5).map(|i| named_needs(&format!("s{i}"), 5)).collect();
        let rows: Vec<SessionRow> = fives
            .iter()
            .enumerate()
            .map(|(i, needs)| {
                let id = u64::try_from(i).unwrap() + 1;
                told_row(
                    id,
                    &format!("s{id}"),
                    &refs(needs),
                    now - 600 + 10 * i64::try_from(i).unwrap(),
                )
            })
            .collect();
        let g = local_glance(&rows, &[], now, None);
        let menu = compose_status_menu(&g, now);
        let section = needs_section(&menu);
        assert_eq!(further_labels(section).len(), NEED_MORE_ALL, "{section:#?}");
        for title in ["s1", "s2", "s3", "s4"] {
            assert!(!own_row(section, title).contains("more)"), "{title}");
            assert!(
                further_labels(section).contains(&format!("{title} 3 (+2 more)").as_str()),
                "{title}: {section:#?}"
            );
        }
        assert!(
            own_row(section, "s5").contains(" (+4 more) "),
            "the menu's rows spent: {}",
            own_row(section, "s5")
        );

        let rows: Vec<SessionRow> = (1..=12_u64)
            .map(|id| {
                let first = format!("t{id} 1");
                let second = format!("t{id} 2");
                told_row(
                    id,
                    &format!("t{id}"),
                    &[&first, &second],
                    now - 1000 + i64::try_from(id).unwrap(),
                )
            })
            .collect();
        let g = local_glance(&rows, &[], now, None);
        let menu = compose_status_menu(&g, now);
        let section = needs_section(&menu);
        assert_eq!(further_labels(section).len(), NEED_MORE_ALL);
        let overflow: Vec<&str> = section
            .iter()
            .filter_map(|r| match r {
                StatusRow::Action {
                    label,
                    action: StatusAction::Overview,
                    ..
                } => Some(label.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(overflow, ["and 2 more \u{2014} Overview\u{2026}"]);
        assert!(
            own_row(section, "t8").contains("t8 1 \u{b7} "),
            "{}",
            own_row(section, "t8")
        );
        assert!(
            own_row(section, "t9").contains(" (+1 more) "),
            "{}",
            own_row(section, "t9")
        );
        assert!(own_row(section, "t10").contains(" (+1 more) "));
    }

    #[test]
    fn a_further_needs_row_goes_where_its_session_goes() {
        let now = 10_000_i64;
        let g = local_glance(
            &[
                told_row(1, "alpha", &["a 1", "a 2", "a 3"], now - 200),
                told_row(2, "beta", &["b 1", "b 2"], now - 100),
            ],
            &[],
            now,
            None,
        );
        let menu = compose_status_menu(&g, now);
        let mut owner: Option<StatusAction> = None;
        let mut further = 0;
        for (i, row) in menu.iter().enumerate() {
            match row {
                StatusRow::Action {
                    action, indent: 0, ..
                } if matches!(action, StatusAction::Go(_)) => owner = Some(*action),
                StatusRow::Action {
                    action,
                    indent: 1,
                    enabled,
                    ..
                } => {
                    further += 1;
                    assert!(*enabled);
                    assert_eq!(Some(*action), owner, "row {i} goes where its session goes");
                    let at = MenuAt::parse(&i.to_string()).expect("a path");
                    assert_eq!(row_at(&menu, &[], &at), Some(row));
                }
                _ => {}
            }
        }
        assert_eq!(further, 3, "{menu:#?}");
        let go_of = |label: &str| {
            menu.iter().find_map(|r| match r {
                StatusRow::Action {
                    label: l,
                    action: StatusAction::Go(pick),
                    ..
                } if l == label => Some(g.go[usize::from(pick.index)]),
                _ => None,
            })
        };
        assert_eq!(
            (go_of("a 2"), go_of("a 3"), go_of("b 2")),
            (Some(1), Some(1), Some(2))
        );
    }

    #[test]
    fn menu_lines_marks_a_row_one_level_in() {
        let now = 10_000_i64;
        let g = local_glance(
            &[told_row(1, "beta", &["b 1", "b 2"], now - 100)],
            &[],
            now,
            None,
        );
        let lines = menu_lines(&compose_status_menu(&g, now), &[]);
        assert_eq!(
            lines.get(2).map(String::as_str),
            Some(
                "statusmenu \"b 2\" enabled=yes checked=no \
                 tip=\"Claude Code in beta needs this too\" indent=1 at=2"
            ),
            "{lines:#?}"
        );
        let own = lines.get(1).expect("the session's row");
        assert!(
            own.starts_with("statusmenu \"\u{25c7} beta \u{2014} b 1 "),
            "{own}"
        );
        assert!(!own.contains("indent="), "{own}");
        assert!(
            lines
                .iter()
                .all(|l| !l.contains("indent=") || l.starts_with("statusmenu \"")),
            "{lines:#?}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_further_needs_item_is_drawn_one_level_in() {
        let now = 10_000_i64;
        let g = local_glance(
            &[
                told_row(1, "alpha", &["a 1", "a 2", "a 3"], now - 200),
                told_row(2, "beta", &["b 1"], now - 100),
            ],
            &[],
            now,
            None,
        );
        let menu = compose_status_menu(&g, now);
        let mut drawn: Vec<(String, isize)> = Vec::new();
        aterm_objc::autoreleasepool(|_| {
            for row in &menu {
                let StatusRow::Action {
                    label,
                    action,
                    enabled,
                    checked,
                    indent,
                    ..
                } = row
                else {
                    continue;
                };
                let item = super::macos::action_item(
                    aterm_objc::Id::NIL,
                    label,
                    *action,
                    (*enabled, *checked, *indent),
                )
                .expect("an item");
                // SAFETY: `-indentationLevel` is `-(NSInteger)` with no side effects, on a live NSMenuItem.
                let level = unsafe {
                    crate::appkit::send_isize(item.id(), aterm_objc::sel!(indentationLevel))
                };
                drawn.push((label.clone(), level));
            }
        });
        let level_of = |label: &str| {
            drawn
                .iter()
                .find(|(l, _)| l == label)
                .map(|(_, level)| *level)
        };
        assert_eq!(
            (level_of("a 2"), level_of("a 3")),
            (Some(1), Some(1)),
            "{drawn:#?}"
        );
        assert!(
            drawn
                .iter()
                .filter(|(l, _)| l != "a 2" && l != "a 3")
                .all(|(_, level)| *level == 0),
            "every other row flush: {drawn:#?}"
        );
        assert!(
            drawn.len() > 4,
            "the session rows and the menu's own: {drawn:#?}"
        );
    }

    #[test]
    fn a_further_needs_row_says_whose_it_is_and_the_count_follows_the_rows() {
        let now = 10_000_i64;
        let alpha: Vec<String> = (1..=5).map(|i| format!("a {i}")).collect();
        let refs: Vec<&str> = alpha.iter().map(String::as_str).collect();
        let g = local_glance(&[told_row(1, "alpha", &refs, now - 60)], &[], now, None);
        let menu = compose_status_menu(&g, now);
        let section = needs_section(&menu);
        assert_eq!(further_labels(section), vec!["a 2", "a 3 (+2 more)"]);
        let own = own_row(section, "alpha");
        assert!(own.ends_with("a 1 \u{b7} 1m"), "{own}");
        let tips: Vec<Option<&str>> = section
            .iter()
            .filter_map(|r| match r {
                StatusRow::Action { tip, .. } => Some(tip.as_deref()),
                _ => None,
            })
            .collect();
        let whose = Some("Claude Code in alpha needs this too");
        assert_eq!(tips, vec![None, whose, whose], "{section:#?}");
        let lines = menu_lines(&menu, &[]);
        assert_eq!(
            lines.get(3).map(String::as_str),
            Some(
                "statusmenu \"a 3 (+2 more)\" enabled=yes checked=no \
                 tip=\"Claude Code in alpha needs this too\" indent=1 at=3"
            ),
            "{lines:#?}"
        );
        let g = local_glance(
            &[told_row(1, "Claude Code", &["c 1", "c 2"], now - 60)],
            &[],
            now,
            None,
        );
        let menu = compose_status_menu(&g, now);
        let tips: Vec<&str> = needs_section(&menu)
            .iter()
            .filter_map(|r| match r {
                StatusRow::Action {
                    tip: Some(tip),
                    indent: 1,
                    ..
                } => Some(tip.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(tips, ["Claude Code needs this too"]);
    }

    #[test]
    fn a_told_need_reads_whole_on_the_banner_and_the_menu() {
        const NEED_A: &str = "grant Screen Recording to com.aterm.aterm in System Settings \
                              \u{25b8} Privacy & Security \u{25b8} Screen Recording, so the \
                              windowed check can capture the menu";
        const NEED_B: &str = "the release credential (~/.aterm/release-credentials.toml) to \
                              sign 0.48.0";
        let now = 10_000_i64;
        let mut facts = told_facts(&[NEED_A, NEED_B]);
        let cut: String = NEED_A.chars().take(63).chain(['\u{2026}']).collect();
        let entry = format!("claude needs you: {cut} (+1 more) \u{2014} carrying on");
        facts.attention[0].text.clone_from(&entry);
        let set = vec![NEED_A.to_string(), NEED_B.to_string()];
        let mut told = crate::needs::ToldNeeds::default();
        told.tell(
            crate::needs::HarnessNeeds::told(&set, &entry).map(std::sync::Arc::new),
            Some(&entry),
        );
        facts.told = told;
        let row = SessionRow {
            need_since: Some(now - 180),
            ..row_of(1, "proj-api refactor", &facts)
        };
        let body = escalation(&row).expect("a need").body;
        assert_eq!(body, format!("proj-api refactor: {NEED_A} (+1 more)"));
        let g = local_glance(&[row], &[], now, None);
        let menu = compose_status_menu(&g, now);
        let section = needs_section(&menu);
        assert_eq!(further_labels(section), [NEED_B]);
        let own = own_row(section, "proj-api refactor");
        assert!(own.len() <= LINE_MAX, "{own}");
        assert!(own.ends_with("\u{2026} \u{b7} 3m"), "{own}");
        assert!(own.contains("\u{2014} grant Screen Recording"), "{own}");
    }
}
