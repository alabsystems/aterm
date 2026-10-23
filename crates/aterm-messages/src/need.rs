// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rule at the door.
//!
//! # The locks (each a `compile_fail` doctest with its rustc error code)
//!
//! A persistent hold cannot be written by hand: the bare spelling is a type error, a
//! forged token has no constructor, and the decision row's seal is not optional:
//!
//! ```compile_fail,E0308
//! use aterm_messages::{Hold, Message, Severity, tags};
//! let _ = Message::new(tags::UPDATE, Severity::Warn, "x").hold(Hold::Standing);
//! ```
//!
//! ```compile_fail,E0423
//! use aterm_messages::{Hold, need::Persist};
//! let _ = Hold::Standing(Persist(()));
//! ```
//!
//! ```compile_fail,E0063
//! use aterm_messages::{Duration, Hold};
//! let _ = Hold::Ask { for_: Duration::from_secs(600) };
//! ```
//!
//! A message's class is not a public field:
//!
//! ```compile_fail,E0616
//! use aterm_messages::{Message, Severity, need::Need, tags};
//! let mut m = Message::new(tags::UPDATE, Severity::Warn, "x");
//! m.need = Need::Record;
//! ```
//!
//! The vocabulary has no word for "waiting":
//!
//! ```compile_fail,E0599
//! use aterm_messages::need::{Reason, Why};
//! let _ = Why::Waiting(Reason::new("it is waiting for its work"));
//! ```
//!
//! A snooze is not "the one action" — there is no constructor for it:
//!
//! ```compile_fail,E0599
//! use aterm_messages::need::Mover;
//! let _ = Mover::not_today("s-1", "2.1.284");
//! ```
//!
//! ...and a raw [`crate::Intent`] is not a [`Mover`]:
//!
//! ```compile_fail,E0308
//! use aterm_messages::{Intent, UpgradeWord, need::Then};
//! let skip = Intent::AgentUpgrade { tab: "s-1".into(), to: "2.1.284".into(), word: UpgradeWord::Skip };
//! let _ = Then::press(skip, "Skip this build");
//! ```
//!
//! ...whose intent field cannot be written around the whitelist:
//!
//! ```compile_fail,E0423
//! use aterm_messages::{Intent, UpgradeWord, need::Mover};
//! let _ = Mover(Intent::AgentUpgrade { tab: "s-1".into(), to: "2.1.284".into(), word: UpgradeWord::Skip });
//! ```
//!
//! A reason that may only be a flash cannot be made standing:
//!
//! ```compile_fail,E0308
//! use aterm_messages::need::{Ask, Reason, Then, Why};
//! let _ = Ask::standing(
//!     Why::YourAction(Reason::new("you typed a bad key")),
//!     Then::step("fix aterm.toml"),
//! );
//! ```
//!
//! An exception can never be promoted to an interrupt:
//!
//! ```compile_fail,E0277
//! use aterm_messages::need::{Ask, LaterAsk};
//! fn promote(later: LaterAsk) -> Ask { later.into() }
//! ```
//!
//! ...and a horizon cannot be invented at a call site:
//!
//! ```compile_fail,E0423
//! use aterm_messages::{Duration, need::Horizon};
//! let _ = Horizon(Duration::ZERO);
//! ```

use crate::model::{Hold, Intent};
use crate::text::clip;
use crate::{Duration, HOLD_ASK, HOLD_GESTURE, HOLD_STAGED_MANUAL, HOLD_WARN, KEY_CAP};

/// Chars of a [`Reason`].
pub const REASON_CAP: usize = 90;
/// Chars of a [`Step`].
pub const STEP_CAP: usize = 90;
/// The longest a transient row may hold.
pub const MAX_TRANSIENT: Duration = Duration::from_mins(2);
/// An open exception files itself (`Retired::Unseen`) after this long.
pub const DIGEST_EXPIRY: Duration = Duration::from_hours(14 * 24);
/// Open exceptions kept.
pub const DIGEST_CAP: usize = 12;
/// How long an omitted family entry stays open before resolving at the next look or `MessageCenter::settle_digest`.
pub const RESOLVE_AFTER: Duration = Duration::from_mins(10);
/// How long a filed exception (`Dismissed`, `Answered`, `Unseen`, `Evicted`) stays in the log's ring after its filing, whatever else is posted.
pub const FILED_KEEP: Duration = Duration::from_hours(180 * 24);
/// [`FILED_KEEP`] in milliseconds.
pub(crate) const FILED_KEEP_MS: u64 = 180 * 86_400_000;
const _: () = assert!(FILED_KEEP.as_secs() * 1000 == FILED_KEEP_MS);
const _: () = assert!(FILED_KEEP_MS >= 2 * Resurface::MAX.millis());
/// The least gap between two restatements of one entry that change only its words (holder counts and ages move every look).
pub const RESTATE_QUIET: Duration = Duration::from_mins(10);
/// A standing row the owner has read (it folded) or answered is not raised again for this long, unless its key changes.
pub const STANDING_QUIET: Duration = Duration::from_hours(24);

/// Stems a reason may not lean on: it says why a person is needed, not that something
/// is pending.
const WAITING_STEMS: [&str; 10] = [
    "waiting",
    "retry",
    "retries",
    "retrying",
    "self-heal",
    "heals",
    "healing",
    "soon",
    "eventually",
    "automatically",
];
/// Leading words of a step that are a snooze, not an action.
const SNOOZE_LEADS: [&str; 8] = [
    "skip", "snooze", "dismiss", "ignore", "wait", "defer", "postpone", "not",
];

/// One clause: why a person, and not the machine, is needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reason(String);

impl Reason {
    /// The clause, sanitized to one line and capped.
    #[must_use]
    pub fn new(words: impl AsRef<str>) -> Self {
        Self(clip(words.as_ref(), REASON_CAP))
    }

    /// The words.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Why these words are not a reason a person is needed, or `None`.
    #[must_use]
    pub fn fault(&self) -> Option<&'static str> {
        let words = self.0.to_lowercase();
        if words.trim().is_empty() {
            Some("a reason that says nothing")
        } else if WAITING_STEMS.iter().any(|stem| words.contains(stem)) {
            Some("a reason that only says something is pending")
        } else {
            None
        }
    }
}

/// One imperative sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step(String);

impl Step {
    /// The sentence, sanitized to one line and capped.
    #[must_use]
    pub fn new(words: impl AsRef<str>) -> Self {
        Self(clip(words.as_ref(), STEP_CAP))
    }

    /// The words.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Why these words are not one action, or `None`.
    #[must_use]
    pub fn fault(&self) -> Option<&'static str> {
        let words = self.0.to_lowercase();
        let lead = words.split_whitespace().next().unwrap_or("");
        if lead.is_empty() {
            Some("an action that says nothing")
        } else if SNOOZE_LEADS.contains(&lead) {
            Some("a snooze is not an action")
        } else if words.contains("; ") || words.contains(" or ") {
            Some("two actions in one sentence")
        } else {
            None
        }
    }
}

/// Why a person is needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    /// A step no software can take.
    OnlyYou(Reason),
    /// Something waits on a yes or a no.
    Question(Reason),
    /// What you just did did not work.
    YourAction(Reason),
    /// Your own setting removed the automatic answer.
    Configured {
        /// The setting (`resume_limits`).
        key: Reason,
        /// What it removed.
        reason: Reason,
    },
    /// aterm tried, and the automatic answer is spent.
    Spent {
        /// What aterm did (`asked it to resume`).
        tried: Reason,
        /// Why that is not enough (`it did not come back`).
        reason: Reason,
    },
}

impl Why {
    /// The clause the band shows as the row's reason.
    #[must_use]
    pub fn words(&self) -> String {
        match self {
            Self::OnlyYou(r) | Self::Question(r) | Self::YourAction(r) => r.0.clone(),
            Self::Configured { key, reason } => {
                clip(&format!("{} ({} is set)", reason.0, key.0), REASON_CAP + 24)
            }
            Self::Spent { tried, reason } => {
                clip(&format!("aterm {}; {}", tried.0, reason.0), REASON_CAP + 40)
            }
        }
    }
}

/// The reasons that may outlast a look, a subset of [`Why`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StandingWhy {
    /// See [`Why::OnlyYou`].
    OnlyYou(Reason),
    /// See [`Why::Configured`].
    Configured {
        /// The setting.
        key: Reason,
        /// What it removed.
        reason: Reason,
    },
    /// See [`Why::Spent`].
    Spent {
        /// What aterm did.
        tried: Reason,
        /// Why that is not enough.
        reason: Reason,
    },
}

impl From<StandingWhy> for Why {
    fn from(w: StandingWhy) -> Self {
        match w {
            StandingWhy::OnlyYou(r) => Why::OnlyYou(r),
            StandingWhy::Configured { key, reason } => Why::Configured { key, reason },
            StandingWhy::Spent { tried, reason } => Why::Spent { tried, reason },
        }
    }
}

/// A button that moves the outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mover(Intent);

impl Mover {
    /// Open the Settings page where the fix is made (`/updates`, `/packages`).
    #[must_use]
    pub fn open_settings(route: &str) -> Self {
        Self(Intent::OpenSettings {
            route: route.to_string(),
        })
    }

    /// Open the macOS Settings pane where the switch is (`full-disk-access`).
    #[must_use]
    pub fn open_system_pane(pane: &str) -> Self {
        Self(Intent::OpenSystemPane {
            pane: pane.to_string(),
        })
    }

    /// Open `aterm.toml` at the line that is wrong.
    #[must_use]
    pub const fn open_config_editor(line: Option<u32>) -> Self {
        Self(Intent::OpenConfigEditor { line })
    }

    /// Open a new window (the GPU-lost remedy).
    #[must_use]
    pub const fn new_window() -> Self {
        Self(Intent::NewWindow)
    }

    /// Open the web page where the fix is made (`notice later … url=`).
    #[must_use]
    pub(crate) fn open_link(url: &str) -> Option<Self> {
        crate::model::link_host(url)?;
        Some(Self(Intent::OpenLink {
            url: url.to_string(),
        }))
    }

    /// The intent the capsule carries.
    #[must_use]
    pub const fn intent(&self) -> &Intent {
        &self.0
    }
}

/// The one action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Then {
    step: Step,
    mover: Option<Mover>,
    short: Option<Step>,
}

impl Then {
    /// One button that changes the outcome, and the sentence that says what it is for (`Turn on aterm in Full Disk Access`).
    #[must_use]
    pub fn press(mover: Mover, step: impl AsRef<str>) -> Self {
        Self {
            step: Step::new(step),
            mover: Some(mover),
            short: None,
        }
    }

    /// One step outside aterm, in words (`Stop that work in the tab`).
    #[must_use]
    pub fn step(step: impl AsRef<str>) -> Self {
        Self {
            step: Step::new(step),
            mover: None,
            short: None,
        }
    }

    /// The sentence.
    #[must_use]
    pub fn words(&self) -> &str {
        self.step.as_str()
    }

    /// The button, when one press does it.
    #[must_use]
    pub const fn mover(&self) -> Option<&Mover> {
        self.mover.as_ref()
    }

    /// THE WORDS THE BAND SHOWS IN THE STEP'S PLACE where the step does not stand whole beside the title (`Run the command under Details.`).
    #[must_use]
    pub fn or_short(mut self, short: impl AsRef<str>) -> Self {
        self.short = Some(Step::new(short));
        self
    }

    /// The short form ([`Self::or_short`]), if the step has one.
    #[must_use]
    pub fn short(&self) -> Option<&str> {
        self.short.as_ref().map(Step::as_str)
    }

    /// Why the sentence, or its short form, is not one action ([`Step::fault`]).
    #[must_use]
    pub fn fault(&self) -> Option<&'static str> {
        self.step
            .fault()
            .or_else(|| self.short.as_ref().and_then(Step::fault))
    }
}

/// How long a transient ask stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    /// A failed gesture (12 s).
    Gesture,
    /// A warning (the severity's 45 s).
    Warn,
    /// A staged decision (60 s).
    Minute,
    /// A launch-time fault (two minutes: [`MAX_TRANSIENT`]).
    Launch,
    /// A decision row (ten minutes).
    Ten,
}

impl Span {
    /// The span's duration.
    #[inline]
    #[must_use]
    pub const fn duration(self) -> Duration {
        match self {
            Self::Gesture => HOLD_GESTURE,
            Self::Warn => HOLD_WARN,
            Self::Minute => HOLD_STAGED_MANUAL,
            Self::Launch => MAX_TRANSIENT,
            Self::Ten => HOLD_ASK,
        }
    }
}

/// The identity of a fact (not of its moving parts).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fingerprint(String);

impl Fingerprint {
    /// `family.subject.cause`.
    #[must_use]
    pub fn new(family: &str, subject: &str, cause: &str) -> Self {
        let family = clip(family, 24);
        let cause = clip(cause, 20);
        let subject = clip(subject, usize::MAX);
        if subject.is_empty() {
            return Self(format!("{family}.{cause}"));
        }
        let fixed = family.chars().count() + cause.chars().count() + 2;
        let room = KEY_CAP.saturating_sub(fixed);
        let subject = if subject.chars().count() <= room {
            subject
        } else {
            let hash = subject.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
            });
            let keep = room.saturating_sub(5);
            let head: String = subject.chars().take(keep.min(12)).collect();
            format!("{head}~{:04x}", hash & 0xffff)
        };
        Self(format!("{family}.{subject}.{cause}"))
    }

    /// A wire key (`wire.<key>`, which the wire's rule already holds to [`KEY_CAP`]) kept whole.
    pub(crate) fn wire(full: &str) -> Self {
        Self(clip(full, KEY_CAP))
    }

    /// The key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// How long a dismissal of an entry holds before the same fact may be shown again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Resurface(u16);

impl Resurface {
    /// A wait that outlasted its horizon.
    pub const WAIT: Self = Self(7);
    /// A fact about the owner's own choice (tmux, a wrapper, Full Disk Access declined).
    pub const CHOICE: Self = Self(30);
    /// The longest a fact stays put away, however often it is dismissed.
    pub const MAX: Self = Self(90);

    /// The quiet span in milliseconds, for a first dismissal.
    #[inline]
    #[must_use]
    pub const fn millis(self) -> u64 {
        self.0 as u64 * 86_400_000
    }

    /// The quiet span after the `n`th dismissal of the same fact inside [`FILED_KEEP`] (`n` counts this one).
    #[inline]
    #[must_use]
    pub const fn backed_off(self, n: u32) -> u64 {
        let mut ms = self.millis();
        let mut i = 1;
        while i < n && ms < Self::MAX.millis() {
            ms = ms.saturating_mul(4);
            i += 1;
        }
        if ms > Self::MAX.millis() {
            Self::MAX.millis()
        } else {
            ms
        }
    }
}

/// A named horizon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Horizon(Duration);

impl Horizon {
    /// The agent's own running work holds an upgrade (24 h of the same cause).
    pub const OWN_WORK: Self = Self(Duration::from_hours(24));
    /// A suspended (`^Z`) agent or a tab with no shell integration (1 h).
    pub const NOT_FOREGROUND: Self = Self(Duration::from_hours(1));
    /// Updates failing (24 h since the first failure of a kind).
    pub const UPDATES_FAILING: Self = Self(Duration::from_hours(24));
    /// A supervisor restarting past its budget (1 h).
    pub const SUPERVISOR_RESTARTS: Self = Self(Duration::from_hours(1));

    /// The span.
    #[inline]
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.0
    }
}

/// The seal on a persistent hold ([`Hold::Standing`], [`Hold::Ask`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Persist(());

impl Persist {
    /// The token.
    #[inline]
    pub(crate) const fn mint() -> Self {
        Self(())
    }
}

/// The owner is needed now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    why: Why,
    then: Then,
    stay: Stay,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Stay {
    /// A transient row.
    For(Span),
    /// A decision row: a question that waits ten minutes for a yes or a no
    /// ([`Hold::Ask`]).
    Question,
    /// Until resolved or read. The fact's identity is the row's key.
    Standing,
    /// Adopted from an older parent's carry: it decided, not this process.
    Adopted,
}

impl Ask {
    /// A transient ask.
    #[must_use]
    pub const fn flash(why: Why, then: Then, span: Span) -> Self {
        Self {
            why,
            then,
            stay: Stay::For(span),
        }
    }

    /// A standing ask.
    #[must_use]
    pub fn standing(why: StandingWhy, then: Then) -> Self {
        Self {
            why: why.into(),
            then,
            stay: Stay::Standing,
        }
    }

    /// A decision row.
    #[must_use]
    pub const fn question(why: Reason, then: Then) -> Self {
        Self {
            why: Why::Question(why),
            then,
            stay: Stay::Question,
        }
    }

    /// A row from an older parent's carry, which had no need.
    pub(crate) fn adopted(then_words: &str) -> Self {
        Self {
            why: Why::OnlyYou(Reason::new("carried across the update")),
            then: Then::step(then_words),
            stay: Stay::Adopted,
        }
    }

    /// The hold the ask earns.
    #[must_use]
    pub const fn hold(&self) -> Hold {
        match &self.stay {
            Stay::For(span) => Hold::For(span.duration()),
            Stay::Question => Hold::Ask {
                for_: HOLD_ASK,
                seal: Persist::mint(),
            },
            Stay::Standing | Stay::Adopted => Hold::Standing(Persist::mint()),
        }
    }

    /// The reason.
    #[must_use]
    pub const fn why(&self) -> &Why {
        &self.why
    }

    /// The one action.
    #[must_use]
    pub const fn then(&self) -> &Then {
        &self.then
    }

    /// `true` for an ask adopted from an older parent's carry (or lent to a test fixture).
    #[must_use]
    pub(crate) const fn is_adopted(&self) -> bool {
        matches!(self.stay, Stay::Adopted)
    }

    /// `true` when the ask outlasts a look.
    #[must_use]
    pub const fn is_standing(&self) -> bool {
        matches!(self.stay, Stay::Standing | Stay::Adopted)
    }

    /// `true` when the reason and the action both say something.
    #[must_use]
    pub(crate) fn is_complete(&self) -> bool {
        self.fault().is_none()
    }

    /// Why this ask cannot stand as it is, or `None`.
    #[must_use]
    pub fn fault(&self) -> Option<&'static str> {
        why_fault(&self.why).or_else(|| self.then.fault())
    }
}

fn why_fault(why: &Why) -> Option<&'static str> {
    match why {
        Why::OnlyYou(r) | Why::Question(r) | Why::YourAction(r) => r.fault(),
        Why::Configured { key, reason } => key.fault().or_else(|| reason.fault()),
        Why::Spent { tried, reason } => tried.fault().or_else(|| reason.fault()),
    }
}

/// Why an exception is in the digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaterWhy {
    /// A person is needed for something that can wait a day.
    Person(Why),
    /// A fact that heals itself has outlasted its horizon.
    Outlasted {
        /// What has gone on (`its own background work holds the move`).
        what: Reason,
        /// The named horizon it outlasted.
        after: Horizon,
    },
}

/// The owner is needed, but not now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaterAsk {
    why: LaterWhy,
    then: Then,
    fingerprint: Fingerprint,
    resurface: Resurface,
    /// Its words move only when its fact does, never every look
    /// ([`Self::restated_at_once`]).
    at_once: bool,
}

impl LaterAsk {
    /// A person is needed for something that can wait.
    #[must_use]
    pub const fn person(
        why: Why,
        then: Then,
        fingerprint: Fingerprint,
        resurface: Resurface,
    ) -> Self {
        Self {
            why: LaterWhy::Person(why),
            then,
            fingerprint,
            resurface,
            at_once: false,
        }
    }

    /// Words that move only when the fact itself does (the tabs an outage holds, counted as they join and leave it) are news each time.
    #[must_use]
    pub const fn restated_at_once(mut self) -> Self {
        self.at_once = true;
        self
    }

    /// Whether a restatement of its words is shown at once ([`Self::restated_at_once`]).
    #[must_use]
    pub const fn restates_at_once(&self) -> bool {
        self.at_once
    }

    /// A self-healing fact, promoted only once it has gone on for `horizon`.
    #[must_use]
    pub fn outlasted(
        what: Reason,
        horizon: Horizon,
        age: Duration,
        then: Then,
        fingerprint: Fingerprint,
        resurface: Resurface,
    ) -> Option<Self> {
        (age >= horizon.duration()).then_some(Self {
            why: LaterWhy::Outlasted {
                what,
                after: horizon,
            },
            then,
            fingerprint,
            resurface,
            at_once: false,
        })
    }

    /// An exception the replayed log left open, rebuilt from its words (the reporter re-asserts the real one at its first look).
    pub(crate) fn adopted(why: &str, then: &str, key: &str, first: Option<&Intent>) -> Self {
        let link = match first {
            Some(Intent::OpenLink { url }) => Mover::open_link(url),
            _ => None,
        };
        Self {
            why: LaterWhy::Person(Why::OnlyYou(Reason::new(why))),
            then: link.map_or_else(|| Then::step(then), |m| Then::press(m, then)),
            fingerprint: Fingerprint(clip(key, KEY_CAP)),
            resurface: Resurface::WAIT,
            at_once: false,
        }
    }

    /// Take the action half of `other` (what a press does, how long a dismissal holds), keeping this ask's reason.
    pub(crate) fn take_action_of(&mut self, other: &LaterAsk) {
        if self.then != other.then {
            self.then = other.then.clone();
        }
        self.resurface = other.resurface;
    }

    /// The reason.
    #[must_use]
    pub const fn why(&self) -> &LaterWhy {
        &self.why
    }

    /// The one action.
    #[must_use]
    pub const fn then(&self) -> &Then {
        &self.then
    }

    /// The fact's identity.
    #[must_use]
    pub const fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    /// How long a dismissal holds.
    #[must_use]
    pub const fn resurface(&self) -> Resurface {
        self.resurface
    }

    /// The clause the digest shows as the reason.
    #[must_use]
    pub fn why_words(&self) -> String {
        match &self.why {
            LaterWhy::Person(w) => w.words(),
            LaterWhy::Outlasted { what, after } => clip(
                &format!("{} (for over {})", what.0, hours_words(after.duration())),
                REASON_CAP + 24,
            ),
        }
    }

    /// `true` when the reason and the action both say something.
    #[must_use]
    pub(crate) fn is_complete(&self) -> bool {
        self.fault().is_none()
    }

    /// Why this ask cannot stand as it is, or `None` (see [`Ask::fault`]).
    #[must_use]
    pub fn fault(&self) -> Option<&'static str> {
        let why = match &self.why {
            LaterWhy::Person(w) => why_fault(w),
            LaterWhy::Outlasted { what, .. } => what.fault(),
        };
        why.or_else(|| self.then.fault())
    }
}

fn hours_words(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 86_400 && secs.is_multiple_of(86_400) {
        format!("{} d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{} h", secs / 3_600)
    } else {
        format!("{} min", secs / 60)
    }
}

/// The closed list of transient rows that ask nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// The crash-last-time rows.
    LastRun,
    /// A script's warn or error over the socket (`notice post`).
    Script,
    /// The answer to the owner's own press (`Upgrade now`).
    Answer,
}

impl NoticeKind {
    /// The hold the kind earns.
    #[must_use]
    pub const fn span(self) -> Duration {
        match self {
            Self::LastRun | Self::Script | Self::Answer => MAX_TRANSIENT,
        }
    }
}

/// What a message is to the person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Need {
    /// Not yet classified.
    Unclassified,
    /// The log only.
    Record,
    /// Work in flight, ending with the work.
    Working,
    /// A closed list of transient rows that ask nothing.
    Notice(NoticeKind),
    /// The owner is needed now.
    Now(Ask),
    /// The owner is needed, but not now.
    Later(LaterAsk),
}

/// The class a message earned, as a plain word for introspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// See [`Need::Unclassified`].
    Unclassified,
    /// See [`Need::Record`].
    Record,
    /// See [`Need::Working`].
    Working,
    /// See [`Need::Notice`].
    Notice,
    /// See [`Need::Now`].
    Now,
    /// See [`Need::Later`].
    Later,
}

impl Class {
    /// The word `aterm ctl messages` prints (`need=<word>`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unclassified => "unclassified",
            Self::Record => "record",
            Self::Working => "working",
            Self::Notice => "notice",
            Self::Now => "now",
            Self::Later => "later",
        }
    }
}

impl Need {
    /// The plain class.
    #[must_use]
    pub const fn class(&self) -> Class {
        match self {
            Self::Unclassified => Class::Unclassified,
            Self::Record => Class::Record,
            Self::Working => Class::Working,
            Self::Notice(_) => Class::Notice,
            Self::Now(_) => Class::Now,
            Self::Later(_) => Class::Later,
        }
    }
}
