// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE SESSION WORDS (`docs/DESIGN-comprehension-2026-11-14.md` §5.5, §5.6): the closed words a
//! session's row is read by, on every platform. Two are what today's survey row says: the glance
//! word ([`State`]) and the task word ([`Task`]). Three are the comprehension design's: the track
//! word a session's own Mac judges from its records ([`Track`]), the reason that word stands
//! ([`TrackWhy`]), and the activity word, what the session is doing this minute and never a
//! judgement ([`Activity`]).
//!
//! [`activity`] is the activity a reader derives for a session whose row sends none: from the
//! words the fold already reads the row by ([`Seen`]) and the track rule's verdict. The fleet
//! fold calls it for a peer, and aterm-gui for this Mac's own sessions until a row-state
//! function of its own gives them their word, so one rule reads both.
//!
//! Every word list is in the order a schema lists it: a typed column's `Enum` reads these
//! `WORDS`, and each `ALL` holds the same values in the same order.

/// What an agent is doing, by its row's own words: its glance word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    /// At work.
    Busy,
    /// At rest.
    Idle,
    /// Waiting on a person.
    Waiting,
    /// At a limit.
    Walled,
    /// Its task is stalled.
    Stuck,
    /// Any other word, or none.
    Unknown,
}

impl State {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] =
        &["busy", "idle", "waiting", "walled", "stuck", "unknown"];

    /// Every state, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 6] = [
        Self::Busy,
        Self::Idle,
        Self::Waiting,
        Self::Walled,
        Self::Stuck,
        Self::Unknown,
    ];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Idle => "idle",
            Self::Waiting => "waiting",
            Self::Walled => "walled",
            Self::Stuck => "stuck",
            Self::Unknown => "unknown",
        }
    }
}

/// A session's task, as its row's `task=` says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Task {
    Working,
    Done,
    Failed,
    Exited,
    Stalled,
}

impl Task {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &["working", "done", "failed", "exited", "stalled"];

    /// Every task word, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 5] = [
        Self::Working,
        Self::Done,
        Self::Failed,
        Self::Exited,
        Self::Stalled,
    ];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Exited => "exited",
            Self::Stalled => "stalled",
        }
    }
}

/// The track word (§5.6): a judgement the session's own Mac derives from its records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Track {
    OnTrack,
    AtRisk,
    OffTrack,
    Done,
    /// Nothing to judge: a plain shell, an idle session with no task. Its word is `none`.
    Untracked,
}

impl Track {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] =
        &["on-track", "at-risk", "off-track", "done", "none"];

    /// Every track word, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 5] = [
        Self::OnTrack,
        Self::AtRisk,
        Self::OffTrack,
        Self::Done,
        Self::Untracked,
    ];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::OnTrack => "on-track",
            Self::AtRisk => "at-risk",
            Self::OffTrack => "off-track",
            Self::Done => "done",
            Self::Untracked => "none",
        }
    }

    /// The track `raw` names: exactly one of [`Self::WORDS`]; any other word names none.
    pub fn of(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|track| track.word() == raw)
    }
}

/// Why the track word stands (`track_why`): each reason names exactly one track word
/// ([`Self::track`]), so one value carries both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackWhy {
    Failed,
    CheckRed,
    Stalled,
    Blocked,
    Repeat,
    NoProgress,
    Walled,
    Flagged,
    Progress,
    Landed,
    /// The reason of [`Track::Untracked`]. Its word is `none`.
    Untracked,
}

impl TrackWhy {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &[
        "failed",
        "check-red",
        "stalled",
        "blocked",
        "repeat",
        "no-progress",
        "walled",
        "flagged",
        "progress",
        "landed",
        "none",
    ];

    /// Every reason, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 11] = [
        Self::Failed,
        Self::CheckRed,
        Self::Stalled,
        Self::Blocked,
        Self::Repeat,
        Self::NoProgress,
        Self::Walled,
        Self::Flagged,
        Self::Progress,
        Self::Landed,
        Self::Untracked,
    ];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::CheckRed => "check-red",
            Self::Stalled => "stalled",
            Self::Blocked => "blocked",
            Self::Repeat => "repeat",
            Self::NoProgress => "no-progress",
            Self::Walled => "walled",
            Self::Flagged => "flagged",
            Self::Progress => "progress",
            Self::Landed => "landed",
            Self::Untracked => "none",
        }
    }

    /// The reason `raw` names: exactly one of [`Self::WORDS`]; any other word names none.
    pub fn of(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|why| why.word() == raw)
    }

    /// The track word this reason stands for (§5.6's track table): a failed task, a check that
    /// says done against the records, a stall, a block on a person and a run of errors are off
    /// track; a wall, no progress and a flag at risk; a landing done; progress on track.
    pub const fn track(self) -> Track {
        match self {
            Self::Failed | Self::CheckRed | Self::Stalled | Self::Blocked | Self::Repeat => {
                Track::OffTrack
            }
            Self::Walled | Self::NoProgress | Self::Flagged => Track::AtRisk,
            Self::Landed => Track::Done,
            Self::Progress => Track::OnTrack,
            Self::Untracked => Track::Untracked,
        }
    }
}

/// The activity word (tracker decision ST-D23's five): what the session is doing this minute,
/// never a judgement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Activity {
    Working,
    Quiet,
    Idle,
    Ended,
    /// It cannot go on by itself: read only where its track is off track, for a failed or
    /// stalled task or a block on a person.
    Stuck,
}

impl Activity {
    /// Every word, in the order a schema lists them.
    pub const WORDS: &'static [&'static str] = &["working", "quiet", "idle", "ended", "stuck"];

    /// Every activity word, in [`Self::WORDS`]' order.
    pub const ALL: [Self; 5] = [
        Self::Working,
        Self::Quiet,
        Self::Idle,
        Self::Ended,
        Self::Stuck,
    ];

    /// Its word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Quiet => "quiet",
            Self::Idle => "idle",
            Self::Ended => "ended",
            Self::Stuck => "stuck",
        }
    }

    /// The activity `raw` names: exactly one of [`Self::WORDS`]; any other word names none.
    pub fn of(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|activity| activity.word() == raw)
    }
}

/// What one session's row says, in the words the fold already reads it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    /// Its glance word (the agents table's `state`).
    pub glance: State,
    /// The glance word the same row reads with its need set aside, as a row whose `needs=` is
    /// `-` is read: `busy`, `walled`, `idle`, `unknown`, or `stuck` where an agent that is not
    /// walled has a stalled task; never `waiting`. [`activity`] reads it only for a `waiting`
    /// row whose task is neither failed nor stalled, where it is never `stuck`.
    pub aside: State,
    /// Its task word, where it has a task.
    pub task: Option<Task>,
    /// It has ended: its row's lifecycle word (`state=`) is `exited`, or it is a row of the
    /// machine's table of ended sessions.
    pub ended: bool,
}

/// The activity word of a session whose row sends none (§5.6's activity table, its third
/// column), under the track rule's `verdict`: `None` where the verdict is unread (a peer that
/// sends no `track=`, or an origin that derives none yet). `None` is the unread cell, `?`.
///
/// The first rule that holds: an ended session is `ended`, whatever its last words were. A
/// failed or stalled task is `stuck`: those are the records the track table's `failed` and
/// `stalled` rows read, so no verdict is needed to read them. A `blocked` or `stalled` verdict is
/// `stuck` whatever the glance word: the needs those rows read without a stalled task (`frozen`,
/// `stopped`, `login`, `memory`, `wall`) are ones the glance leaves under the agent's own word
/// (`busy`, `idle`, `walled`), since only a need a person decides makes it `waiting`, so only the
/// verdict reads them. Then the glance word: `stuck` is `stuck`, `busy` `working`, `walled`
/// `quiet`, `idle` `idle`, and `unknown` unread. A `waiting` row reads `quiet` where the verdict
/// is unread, and otherwise the word the row reads with its need set aside, since that need
/// blocks nothing. So `stuck` is read only where the track table's `failed`, `stalled` or
/// `blocked` row holds, and `stuck` implies `off-track`.
///
/// A stalled task outranks a wall and a wait here, where the glance ranks those over it: the
/// track table's `stalled` row (off track) stands above its `walled` row (at risk). The glance
/// word itself is unchanged.
pub fn activity(seen: &Seen, verdict: Option<TrackWhy>) -> Option<Activity> {
    if seen.ended {
        return Some(Activity::Ended);
    }
    if matches!(seen.task, Some(Task::Failed | Task::Stalled))
        || matches!(verdict, Some(TrackWhy::Blocked | TrackWhy::Stalled))
    {
        return Some(Activity::Stuck);
    }
    match (seen.glance, verdict) {
        (State::Waiting, None) => Some(Activity::Quiet),
        (State::Waiting, Some(_)) => of_glance(seen.aside),
        (glance, _) => of_glance(glance),
    }
}

/// The activity a glance word reads as by itself: a wait with nothing to say it is blocked is
/// `quiet`, as a wall is.
const fn of_glance(glance: State) -> Option<Activity> {
    match glance {
        State::Busy => Some(Activity::Working),
        State::Walled | State::Waiting => Some(Activity::Quiet),
        State::Idle => Some(Activity::Idle),
        State::Stuck => Some(Activity::Stuck),
        State::Unknown => None,
    }
}

#[cfg(test)]
#[path = "words_tests.rs"]
mod tests;
