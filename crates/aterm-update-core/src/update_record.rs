// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The update record: the one place an updater copy keeps its verdict on a release.
//!
//! One [`UpdateRecord`] per updater copy says what runs ([`UpdateRecord::running`], whether its
//! trial was [`proven`](UpdateRecord::proven)), the newest build a check saw
//! ([`offered`](UpdateRecord::offered)), the boot trial under way ([`TrialNote`]), and at most
//! one [`Hold`]: an artifact (build and SHA-256) the updater does not stage, apply or trial
//! again until the hold's `until`. Every hold expires: its span comes from [`hold_until`]'s
//! schedule and never exceeds [`HOLD_MAX`], and a hold that reaches further ahead of the
//! clock than that, a stamp from a clock that ran ahead, holds nothing. A hold also ends
//! when the build starts cleanly ([`UpdateRecord::confirm`]), when a newer build is offered
//! ([`UpdateRecord::offer`]), when a person asks ([`UpdateRecord::end_hold`]), and once per
//! artifact when another Mac of the same platform runs it proven
//! ([`UpdateRecord::peer_lift`]). A hold that expired, or that a person or a peer cut short,
//! stays in the slot as history, so a repeat failure of the same artifact escalates its
//! schedule. Beside the hold, the record keeps the artifact a window here saw fail to start
//! ([`UpdateRecord::start_failure`]), which holds only the windowless lanes and expires on the
//! same terms.
//!
//! The record is published to the lab's journal ring (aterm-db's `upd` collection) by a hook
//! the executable installs; [`install_notify`] is how a write asks that publisher to run
//! soon. The file this module reads and writes ([`UpdateRecord::edit`]) is that row's local
//! copy: the crash-loop revert and the boot gate run before any network exists, and an
//! install with no lab has nowhere else to keep it. An unreadable file reads as no record,
//! so it holds nothing: the boot sentinel already bounds a bad build to three launches.
//!
//! `NativeUpdateHold` (aterm-models-update) is the model these functions refine.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

/// The record's format; a record of any other version reads as unreadable.
pub const VERSION: u8 = 1;
/// The record's file name in an updater copy's own folder (macOS `Updates/`, a Linux copy's
/// updater directory).
pub const FILE: &str = "record.json";
/// The longest any hold lasts, in seconds: a day.
pub const HOLD_MAX: u64 = 24 * HOUR;
/// The longest [`Hold::why`] kept, in bytes.
pub const WHY_MAX: usize = 192;
/// The longest [`TrialNote::why`] kept, in bytes.
pub const TRIAL_WHY_MAX: usize = 160;
/// How long [`UpdateRecord::edit`] waits for another writer's lock.
pub const LOCK_WAIT: Duration = Duration::from_secs(5);
/// [`Hold::why`] of a crash-loop revert whose trial recorded no reason.
pub const CRASH_WHY: &str = "it did not start cleanly 3 times";

const HOUR: u64 = 3600;
/// A crash loop, an unproven trial or a Linux rollback that no window saw: an hour, then six,
/// then a day.
pub const CRASH_SPANS: [u64; 3] = [HOUR, 6 * HOUR, 24 * HOUR];
/// The same failure a window launched: a person saw it, so four hours, then a day.
pub const CRASH_WINDOW_SPANS: [u64; 2] = [4 * HOUR, 24 * HOUR];
/// A download, verification or manifest failure of the artifact: 15 minutes, an hour, four
/// hours, then a day.
pub const STAGE_SPANS: [u64; 4] = [15 * 60, HOUR, 4 * HOUR, 24 * HOUR];

/// A SHA-256 digest, written as 64 lowercase hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha(pub [u8; 32]);

impl Sha {
    /// The digest `hex` spells (64 hex digits, either case), or `None`.
    #[must_use]
    pub fn parse(hex: &str) -> Option<Self> {
        let bytes = hex.as_bytes();
        if bytes.len() != 64 {
            return None;
        }
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        };
        let mut out = [0u8; 32];
        for (i, [high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
            out[i] = (digit(*high)? << 4) | digit(*low)?;
        }
        Some(Self(out))
    }

    /// The 64 lowercase hex digits.
    #[must_use]
    pub fn hex(&self) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for byte in self.0 {
            out.push(char::from(DIGITS[usize::from(byte >> 4)]));
            out.push(char::from(DIGITS[usize::from(byte & 0xf)]));
        }
        out
    }
}

impl Serialize for Sha {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Sha {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).ok_or_else(|| serde::de::Error::custom("a SHA-256 is 64 hex digits"))
    }
}

/// The operating system an artifact is for: a peer's evidence counts only for the same one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Os {
    Macos,
    Linux,
    Windows,
    /// Any other: never matches a peer.
    Other,
}

impl Os {
    /// The system this build is for.
    #[must_use]
    pub const fn here() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// The processor an artifact is for: a peer's evidence counts only for the same one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arch {
    #[serde(rename = "aarch64")]
    Aarch64,
    #[serde(rename = "x86_64")]
    X86_64,
    /// Any other: never matches a peer.
    #[serde(rename = "other")]
    Other,
}

impl Arch {
    /// The processor this build is for.
    #[must_use]
    pub const fn here() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::Aarch64
        } else if cfg!(target_arch = "x86_64") {
            Self::X86_64
        } else {
            Self::Other
        }
    }
}

/// What failed, which picks the hold's schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The trial did not start cleanly within its launches, and the launch reverted it.
    CrashLoop,
    /// The trial never proved itself, and was settled without a revert.
    Unproven,
    /// The artifact did not download, verify or match its manifest.
    Stage,
    /// A Linux copy rolled the build back.
    LinuxRollback,
    /// A window here launched the build's bytes as its successor and saw them fail to start.
    /// Only [`UpdateRecord::start_failure`] has this cause: it holds the windowless lanes (a
    /// headless boot, the node job's look, the supersede chain over a failed activation), and a
    /// window is the witness that may try the bytes again.
    StartFailed,
}

impl Cause {
    /// The cause as one word, for a machine-read field (`aterm ctl update`'s `cause=`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::CrashLoop => "crash-loop",
            Self::Unproven => "unproven",
            Self::Stage => "stage",
            Self::LinuxRollback => "linux-rollback",
            Self::StartFailed => "start-failed",
        }
    }

    /// The cause as a clause a person reads after a build's name ("… is held: it did not start
    /// cleanly").
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::CrashLoop => "it did not start cleanly",
            Self::Unproven => "it never showed it started",
            Self::Stage => "its download could not be prepared",
            Self::LinuxRollback => "it was rolled back on this copy",
            Self::StartFailed => "a window here saw it fail to start",
        }
    }
}

/// Who saw the failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Witness {
    /// A `--headless` launch: nobody was looking.
    Headless,
    /// A launch that opens windows: a person saw it.
    Window,
    /// A Linux copy's updater.
    Linux,
}

/// The boot trial under way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrialNote {
    pub build: u64,
    pub sha: Sha,
    /// When it was armed (unix seconds).
    pub since: u64,
    pub witness: Witness,
    /// How many of its launches failed to prove themselves.
    pub misses: u8,
    /// The latest miss's reason, at most [`TRIAL_WHY_MAX`] bytes.
    pub why: String,
}

/// The one hold: the artifact `(build, sha)` is not staged, applied or trialled while it lasts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hold {
    pub build: u64,
    /// The build's version, for people.
    pub version: String,
    pub sha: Sha,
    pub cause: Cause,
    pub witness: Witness,
    /// Why, at most [`WHY_MAX`] bytes.
    pub why: String,
    /// When the latest failure set it (unix seconds).
    pub at: u64,
    /// How many failures this artifact has had in a row: 1 for the first.
    pub tries: u8,
    /// The span its schedule asked for, never more than [`HOLD_MAX`] and never shorter than an
    /// earlier failure's of the same artifact.
    pub span: u64,
    /// When it lapses: `at + span`, or earlier once a person or peer evidence cut it short.
    pub until: u64,
    /// Whether peer evidence has cut this artifact's hold short once already.
    pub peer_lifted: bool,
    /// The build that ran where the failure was seen, when the writer named one (0 otherwise):
    /// a [`Cause::StartFailed`] hold names the window's build.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub by: u64,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// How the latest hold ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndHow {
    /// The build started cleanly.
    Started,
    /// A newer build was offered.
    Newer,
    /// Its time ran out; it stays in the slot as history.
    Expired,
    /// Another Mac runs it proven; it stays in the slot as history.
    Peer,
    /// A person or an agent asked to try again; it stays in the slot as history.
    Person,
}

/// The endings a caller may declare with [`UpdateRecord::end_hold`]. A peer ends a hold only
/// through [`UpdateRecord::peer_lift`], with the peer's record as the evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Started,
    Newer,
    Expired,
    Person,
}

impl From<Ending> for EndHow {
    fn from(ending: Ending) -> Self {
        match ending {
            Ending::Started => Self::Started,
            Ending::Newer => Self::Newer,
            Ending::Expired => Self::Expired,
            Ending::Person => Self::Person,
        }
    }
}

/// The latest ending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    pub build: u64,
    pub how: EndHow,
    pub at: u64,
}

/// A failure to hold an artifact for ([`UpdateRecord::set_hold`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub build: u64,
    pub version: String,
    pub sha: Sha,
    pub cause: Cause,
    pub witness: Witness,
    pub why: String,
}

/// One updater copy's record. Every field is this copy's own statement about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRecord {
    /// [`VERSION`].
    pub v: u8,
    /// Rises with every write that changed the record, to at least the writer's clock (unix
    /// seconds), so a record that started over still ranks above its earlier rows: the
    /// publisher publishes when it moved.
    pub n: u64,
    pub os: Os,
    pub arch: Arch,
    /// The build that runs.
    pub running: u64,
    pub running_sha: Option<Sha>,
    /// Whether `running` passed its boot trial here.
    pub proven: bool,
    /// The newest build the check lane saw.
    pub offered: Option<u64>,
    pub trial: Option<TrialNote>,
    pub hold: Option<Hold>,
    pub last: Option<Ended>,
    /// The artifact a window here launched as its successor and saw fail to start
    /// ([`Cause::StartFailed`], [`UpdateRecord::start_failed`]), live or lapsed. It is evidence
    /// about bytes a window judged, kept beside the one hold rather than in its slot: it holds
    /// only the windowless lanes ([`UpdateRecord::holds_windowless`]), and nothing that ends or
    /// replaces the hold (a newer offer, a running build, a failure of other bytes, a peer's
    /// proof from another Mac) says anything about why those bytes failed to start here. It
    /// expires on the window's crash schedule; a window of those bytes that starts or settles
    /// their trial ends it ([`UpdateRecord::end_start_failure`]), a person's retry cuts it short,
    /// and a window-start failure of other bytes replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_failure: Option<Hold>,
}

impl Default for UpdateRecord {
    fn default() -> Self {
        Self::fresh()
    }
}

/// The span `cause` and `witness` schedule for a hold's `tries`th failure (1 for the first;
/// past the end of its schedule, the schedule's last span, which is [`HOLD_MAX`]).
#[must_use]
pub fn schedule_span(cause: Cause, witness: Witness, tries: u8) -> u64 {
    let spans: &[u64] = match cause {
        Cause::Stage => &STAGE_SPANS,
        // A person saw a window fail to start it, as a window witness of a crash loop.
        Cause::StartFailed => &CRASH_WINDOW_SPANS,
        Cause::CrashLoop | Cause::Unproven | Cause::LinuxRollback => {
            if witness == Witness::Window {
                &CRASH_WINDOW_SPANS
            } else {
                &CRASH_SPANS
            }
        }
    };
    let at = usize::from(tries.saturating_sub(1)).min(spans.len() - 1);
    spans[at].min(HOLD_MAX)
}

/// When a hold set at `at` for the `tries`th failure lapses: `at` plus [`schedule_span`].
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateHold",
        action = "Revert",
        project = "aterm_update_core::update_record::hold_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateHold",
        action = "Unproven",
        project = "aterm_update_core::update_record::hold_projection"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "NativeUpdateHold",
        action = "StageFail",
        project = "aterm_update_core::update_record::hold_projection"
    )
)]
#[must_use]
pub fn hold_until(cause: Cause, witness: Witness, tries: u8, at: u64) -> u64 {
    at.saturating_add(schedule_span(cause, witness, tries))
}

/// `text` cut to at most `max` bytes on a character boundary.
fn clip(text: &str, max: usize) -> String {
    text[..text.floor_char_boundary(max)].to_owned()
}

/// When `until` comes, read from `now` as a person reads a clock: the local time ("18:40"),
/// "tomorrow 09:15" on the next local day, and with "UTC" named where the local zone cannot be
/// read. A hold never reaches further than a day ([`HOLD_MAX`]), so no later day is named.
#[must_use]
pub fn clock_words(until: u64, now: u64) -> String {
    clock_words_in(until, now, &aterm_uds::zone::local_offset_at)
}

/// [`clock_words`] with the zone's offset from UTC (seconds east) given by `offset_at`.
#[must_use]
pub fn clock_words_in(until: u64, now: u64, offset_at: &dyn Fn(i64) -> Option<i64>) -> String {
    let signed = |t: u64| i64::try_from(t).unwrap_or(i64::MAX);
    let (until, now) = (signed(until), signed(now));
    let (offset, zone) = offset_at(until).map_or((0, " UTC"), |offset| (offset, ""));
    let local = until.saturating_add(offset);
    let day = |t: i64| t.div_euclid(86_400);
    let of_day = local.rem_euclid(86_400);
    let next_day = day(local) > day(now.saturating_add(offset_at(now).unwrap_or(offset)));
    format!(
        "{}{:02}:{:02}{zone}",
        if next_day { "tomorrow " } else { "" },
        of_day / 3600,
        (of_day % 3600) / 60
    )
}

/// `secs` of time left in plain words: "under a minute", "40 min", "3 h". One copy, shared with
/// the fleet's readers of a peer's hold ([`aterm_types::update_standing`]).
pub use aterm_types::update_standing::left_words;

impl Hold {
    /// The held build as a person names it: its version, or "build N" where none was recorded.
    #[must_use]
    pub fn name(&self) -> String {
        if self.version.is_empty() {
            format!("build {}", self.build)
        } else {
            self.version.clone()
        }
    }

    /// Whether this hold holds `now`: before its `until`, and not further ahead than
    /// [`HOLD_MAX`] (a stamp from a clock that ran ahead holds nothing).
    #[must_use]
    pub const fn live(&self, now: u64) -> bool {
        now < self.until && self.until - now <= HOLD_MAX
    }

    /// Seconds until it lapses, 0 when it does not hold `now`.
    #[must_use]
    pub const fn left(&self, now: u64) -> u64 {
        if self.live(now) { self.until - now } else { 0 }
    }

    /// The laws a read enforces whatever was written: the span within [`HOLD_MAX`], `until`
    /// within `at + span`, `why` within [`WHY_MAX`].
    fn settle(&mut self) {
        self.span = self.span.min(HOLD_MAX);
        self.until = self.until.min(self.at.saturating_add(self.span));
        if self.why.len() > WHY_MAX {
            self.why = clip(&self.why, WHY_MAX);
        }
    }
}

impl UpdateRecord {
    /// An empty record for this copy: nothing known to run, nothing held.
    #[must_use]
    pub const fn fresh() -> Self {
        Self {
            v: VERSION,
            n: 0,
            os: Os::here(),
            arch: Arch::here(),
            running: 0,
            running_sha: None,
            proven: false,
            offered: None,
            trial: None,
            hold: None,
            last: None,
            start_failure: None,
        }
    }

    /// The hold, when it holds `now`.
    #[must_use]
    pub fn live_hold(&self, now: u64) -> Option<&Hold> {
        self.hold.as_ref().filter(|hold| hold.live(now))
    }

    /// The gate of every lane: whether the artifact `(build, sha)` is held `now` by the hold. A
    /// windowless lane asks [`Self::holds_windowless`].
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "Probe",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "ProbeOther",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    #[must_use]
    pub fn holds(&self, build: u64, sha: &Sha, now: u64) -> bool {
        self.live_hold(now)
            .is_some_and(|hold| hold.build == build && hold.sha == *sha)
    }

    /// The window-start failure, when it holds `now`.
    #[must_use]
    pub fn live_start_failure(&self, now: u64) -> Option<&Hold> {
        self.start_failure.as_ref().filter(|hold| hold.live(now))
    }

    /// The gate of a windowless lane: whether the artifact `(build, sha)` is held `now` by the
    /// hold or by the window-start failure ([`Self::start_failure`]).
    #[must_use]
    pub fn holds_windowless(&self, build: u64, sha: &Sha, now: u64) -> bool {
        self.holds(build, sha, now)
            || self
                .live_start_failure(now)
                .is_some_and(|hold| hold.build == build && hold.sha == *sha)
    }

    /// Hold `failure`'s artifact from `now`. A failure of an artifact older than the newest
    /// offered or held build changes nothing: the newer build supersedes it. Nor does a
    /// failure of the held artifact while its hold still holds: reports of one failure must
    /// not escalate it. The same artifact again after its hold lapsed (an expired hold stays as
    /// history) is the next try: `tries` rises, the span never shortens, and `peer_lifted`
    /// carries over. Any other artifact replaces the slot, the old hold ending as
    /// [`EndHow::Newer`]. Raises `offered` to the build. The window-start failure
    /// ([`Self::start_failure`]) is not this slot and is left as it is. Returns whether the
    /// record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "StageFail",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn set_hold(&mut self, failure: Failure, now: u64) -> bool {
        let newest = self
            .offered
            .into_iter()
            .chain(self.hold.as_ref().map(|hold| hold.build))
            .max()
            .unwrap_or(0);
        if failure.build < newest {
            return false;
        }
        let before = self.clone();
        let build = failure.build;
        if !self.place(failure, now) {
            return false;
        }
        self.offered = Some(self.offered.map_or(build, |o| o.max(build)));
        *self != before
    }

    /// A window running `by` saw `failure`'s artifact fail to start ([`Cause::StartFailed`],
    /// [`Witness::Window`] whatever `failure` says): book it as [`Self::start_failure`], which
    /// holds the windowless lanes on the window's crash schedule. The hold slot, `offered` and
    /// `last` are left as they are: the window names bytes it launched, not an offer, and a hold
    /// on any artifact, newer or older, keeps its own lineage. The same artifact while its
    /// window-start failure still holds changes nothing; after it lapsed it is the next try
    /// (`tries` rises, the span never shortens); other bytes replace it. Returns whether the
    /// record changed.
    pub fn start_failed(&mut self, failure: Failure, by: u64, now: u64) -> bool {
        let same = self
            .start_failure
            .as_ref()
            .filter(|hold| hold.build == failure.build && hold.sha == failure.sha);
        let (tries, floor) = match same {
            Some(hold) if hold.live(now) => return false,
            Some(hold) => (hold.tries.saturating_add(1), hold.span),
            None => (1, 0),
        };
        self.start_failure = Some(Self::hold_of(
            Failure {
                cause: Cause::StartFailed,
                witness: Witness::Window,
                ..failure
            },
            tries,
            floor,
            false,
            by,
            now,
        ));
        true
    }

    /// End the window-start failure if it names `(build, sha)`, live or lapsed: a window of
    /// those bytes started, or a window settled their trial. Returns whether it did.
    pub fn end_start_failure(&mut self, build: u64, sha: &Sha) -> bool {
        if self
            .start_failure
            .as_ref()
            .is_some_and(|hold| hold.build == build && hold.sha == *sha)
        {
            self.start_failure = None;
            true
        } else {
            false
        }
    }

    /// The hold `failure` takes as its `tries`th failure at `now`, its span never below `floor`.
    fn hold_of(
        failure: Failure,
        tries: u8,
        floor: u64,
        peer_lifted: bool,
        by: u64,
        now: u64,
    ) -> Hold {
        let scheduled = hold_until(failure.cause, failure.witness, tries, now).saturating_sub(now);
        let span = scheduled.max(floor).min(HOLD_MAX);
        Hold {
            build: failure.build,
            version: failure.version,
            sha: failure.sha,
            cause: failure.cause,
            witness: failure.witness,
            why: clip(&failure.why, WHY_MAX),
            at: now,
            tries,
            span,
            until: now.saturating_add(span),
            peer_lifted,
            by,
        }
    }

    /// Put `failure`'s hold in the slot at `now`, as [`Self::set_hold`] says. Returns false,
    /// changing nothing, where the held artifact's hold still holds.
    fn place(&mut self, failure: Failure, now: u64) -> bool {
        let same = self
            .hold
            .as_ref()
            .filter(|hold| hold.build == failure.build && hold.sha == failure.sha);
        let (tries, floor, peer_lifted) = match same {
            Some(hold) if hold.live(now) => return false,
            Some(hold) => (hold.tries.saturating_add(1), hold.span, hold.peer_lifted),
            None => (1, 0, false),
        };
        if same.is_none()
            && let Some(old) = &self.hold
        {
            self.last = Some(Ended {
                build: old.build,
                how: EndHow::Newer,
                at: now,
            });
        }
        self.hold = Some(Self::hold_of(failure, tries, floor, peer_lifted, 0, now));
        true
    }

    /// End the hold `how` declares at `now`. [`Ending::Started`] and [`Ending::Newer`] empty
    /// the slot. [`Ending::Person`] cuts a hold that still holds short, keeping it in the slot
    /// as history (as a peer's lift does), so a repeat failure of the same artifact still
    /// escalates its schedule and a peer cannot lift it twice; a hold that already lapsed has
    /// nothing to end; a person's retry cuts a live window-start failure short the same way
    /// ([`Self::start_failure`]), which no other [`Ending`] touches. [`Ending::Expired`] only
    /// records, once, that a hold which no longer holds has lapsed, and keeps it as history.
    /// Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "PersonRetry",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn end_hold(&mut self, how: Ending, now: u64) -> bool {
        if how == Ending::Person
            && let Some(failure) = self.start_failure.as_mut().filter(|hold| hold.live(now))
        {
            failure.until = now;
            let failed = failure.build;
            if !self.hold.as_ref().is_some_and(|hold| hold.live(now)) {
                self.last = Some(Ended {
                    build: failed,
                    how: EndHow::Person,
                    at: now,
                });
                return true;
            }
        }
        let Some(hold) = &mut self.hold else {
            return false;
        };
        let ended = Ended {
            build: hold.build,
            how: how.into(),
            at: now,
        };
        match how {
            Ending::Expired => {
                let noted = self.last.is_some_and(|last| {
                    last.build == hold.build && last.how == EndHow::Expired && last.at >= hold.at
                });
                if hold.live(now) || noted {
                    return false;
                }
                self.last = Some(ended);
            }
            Ending::Person => {
                if !hold.live(now) {
                    return false;
                }
                hold.until = now;
                self.last = Some(ended);
            }
            Ending::Started | Ending::Newer => {
                self.last = Some(ended);
                self.hold = None;
            }
        }
        true
    }

    /// Cut the hold short on `peer`'s evidence: another copy for the same system and processor
    /// runs the held artifact, proven. Once per artifact: a hold already cut short by a peer
    /// (`peer_lifted`, which a repeat failure of the same artifact carries) lasts its schedule.
    /// Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "PeerProven",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn peer_lift(&mut self, peer: &UpdateRecord, now: u64) -> bool {
        let Some(hold) = &mut self.hold else {
            return false;
        };
        let evidence = self.os != Os::Other
            && self.arch != Arch::Other
            && peer.os == self.os
            && peer.arch == self.arch
            && peer.proven
            && peer.running == hold.build
            && peer.running_sha == Some(hold.sha);
        if !evidence || hold.peer_lifted || !hold.live(now) {
            return false;
        }
        hold.until = now;
        hold.peer_lifted = true;
        self.last = Some(Ended {
            build: hold.build,
            how: EndHow::Peer,
            at: now,
        });
        true
    }

    /// The check lane saw `build` as the newest: record it, and end a hold on any older build
    /// ([`EndHow::Newer`]). Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "OfferNewer",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn offer(&mut self, build: u64, now: u64) -> bool {
        let before = self.offered;
        self.offered = Some(build);
        let ended = self.hold.as_ref().is_some_and(|hold| hold.build < build)
            && self.end_hold(Ending::Newer, now);
        ended || before != self.offered
    }

    /// A boot trial of `(build, sha)` begins, launched as `witness` saw it. Arming the trial
    /// already under way keeps its misses, and a window's witness is never lowered to a
    /// headless one. Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "Stage",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "WindowLaunch",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn arm_trial(&mut self, build: u64, sha: Sha, witness: Witness, now: u64) -> bool {
        let before = self.trial.clone();
        match &mut self.trial {
            Some(trial) if trial.build == build && trial.sha == sha => {
                if witness == Witness::Window {
                    trial.witness = Witness::Window;
                }
            }
            _ => {
                self.trial = Some(TrialNote {
                    build,
                    sha,
                    since: now,
                    witness,
                    misses: 0,
                    why: String::new(),
                });
            }
        }
        before != self.trial
    }

    /// One launch of the trial failed to prove itself, for `why`: kept before the launch
    /// restarts, so the revert can say why. Returns whether the record changed (false with
    /// no trial).
    pub fn note_miss(&mut self, why: &str) -> bool {
        let Some(trial) = &mut self.trial else {
            return false;
        };
        trial.misses = trial.misses.saturating_add(1);
        trial.why = clip(why, TRIAL_WHY_MAX);
        true
    }

    /// The trial crash-looped and the launch reverted it: hold its artifact
    /// ([`Cause::CrashLoop`], the trial's witness and reason) and end the trial. Where that hold
    /// now names the artifact, a window-start failure of the same bytes ends: the crash-loop
    /// hold holds every lane. `running` stays the build the revert restored, which the trial
    /// never replaced. Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "Revert",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn revert(&mut self, version: &str, now: u64) -> bool {
        let artifact = self.trial.as_ref().map(|trial| (trial.build, trial.sha));
        let changed = self.settle_trial(version, Cause::CrashLoop, now);
        if let Some((build, sha)) = artifact
            && self
                .hold
                .as_ref()
                .is_some_and(|hold| hold.build == build && hold.sha == sha)
        {
            self.end_start_failure(build, &sha);
        }
        changed
    }

    /// The trial never proved itself and was settled keeping the build: it runs, unproven,
    /// and its artifact is held ([`Cause::Unproven`]). Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "Unproven",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn unproven(&mut self, version: &str, now: u64) -> bool {
        let Some(trial) = &self.trial else {
            return false;
        };
        self.running = trial.build;
        self.running_sha = Some(trial.sha);
        self.proven = false;
        self.settle_trial(version, Cause::Unproven, now);
        true
    }

    fn settle_trial(&mut self, version: &str, cause: Cause, now: u64) -> bool {
        let Some(trial) = self.trial.take() else {
            return false;
        };
        let why = if trial.why.is_empty() {
            CRASH_WHY.to_owned()
        } else {
            trial.why
        };
        self.set_hold(
            Failure {
                build: trial.build,
                version: version.to_owned(),
                sha: trial.sha,
                cause,
                witness: trial.witness,
                why,
            },
            now,
        );
        true
    }

    /// `(build, sha)` passed its boot trial here, as a person runs it: it runs, proven, the
    /// trial is over, a hold on it or an older build ends ([`EndHow::Started`] for it,
    /// [`EndHow::Newer`] for an older one), and a window-start failure of exactly these bytes
    /// ends (a window started them). Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "Confirm",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn confirm(&mut self, build: u64, sha: Sha, now: u64) -> bool {
        let before = self.clone();
        self.running = build;
        self.running_sha = Some(sha);
        self.proven = true;
        if self
            .trial
            .as_ref()
            .is_some_and(|trial| trial.build <= build)
        {
            self.trial = None;
        }
        if let Some(held) = self.hold.as_ref().map(|hold| hold.build)
            && held <= build
        {
            let how = if held == build {
                Ending::Started
            } else {
                Ending::Newer
            };
            self.end_hold(how, now);
        }
        self.end_start_failure(build, &sha);
        *self != before
    }

    /// The trial of `build` was settled keeping the build without proving it: a headless
    /// start that says nothing about its windows, or a trial settled at its refusal bound. It
    /// runs, unproven, the trial is over, and the hold slot is left as it is: an expired hold of
    /// this artifact stays as history, so the next failure of it still escalates. Returns
    /// whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "KeepUnproven",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn keep_unproven(&mut self, build: u64, sha: Option<Sha>) -> bool {
        let before = self.clone();
        self.running = build;
        self.running_sha = sha.or_else(|| {
            self.trial
                .as_ref()
                .filter(|trial| trial.build == build)
                .map(|trial| trial.sha)
        });
        self.proven = false;
        if self
            .trial
            .as_ref()
            .is_some_and(|trial| trial.build <= build)
        {
            self.trial = None;
        }
        *self != before
    }

    /// The check lane found `(build, sha)` running where the record names another build and
    /// saw no trial of it settle: it was put in place some other way (by hand, by an earlier
    /// updater, or the record started over). It runs, unproven, since nothing here proved it;
    /// a hold on an older build ends ([`EndHow::Newer`]), and a hold on this one stays, as it
    /// would for a build kept unproven. Returns whether the record changed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateHold",
            action = "Observe",
            project = "aterm_update_core::update_record::hold_projection"
        )
    )]
    pub fn observe_running(&mut self, build: u64, sha: Option<Sha>, now: u64) -> bool {
        if self.running == build && (sha.is_none() || self.running_sha == sha) {
            return false;
        }
        self.running = build;
        self.running_sha = sha;
        self.proven = false;
        if self.hold.as_ref().is_some_and(|hold| hold.build < build) {
            self.end_hold(Ending::Newer, now);
        }
        true
    }

    /// The record as the file and the journal row spell it.
    #[must_use]
    pub fn encode(&self) -> String {
        aterm_json::to_string(self).unwrap_or_default()
    }

    /// The record `text` spells, with the laws [`Hold`] keeps enforced whatever was written.
    ///
    /// # Errors
    /// Not a record, or a record of another [`VERSION`].
    pub fn decode(text: &str) -> Result<Self, String> {
        let mut record: Self = aterm_json::from_str(text).map_err(|e| e.to_string())?;
        if record.v != VERSION {
            return Err(format!("record version {} is not {VERSION}", record.v));
        }
        for hold in [&mut record.hold, &mut record.start_failure]
            .into_iter()
            .flatten()
        {
            hold.settle();
        }
        if let Some(trial) = &mut record.trial
            && trial.why.len() > TRIAL_WHY_MAX
        {
            trial.why = clip(&trial.why, TRIAL_WHY_MAX);
        }
        Ok(record)
    }

    /// The record at `path`; `None` when there is none.
    ///
    /// # Errors
    /// It could not be read, or it is not a record ([`io::ErrorKind::InvalidData`]).
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::decode(&text)
                .map(Some)
                .map_err(|why| io::Error::new(io::ErrorKind::InvalidData, why)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The record at `path`, or a [fresh](Self::fresh) one when there is none or it cannot be
    /// read: an unreadable record holds nothing.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        match Self::read(path) {
            Ok(record) => record.unwrap_or_default(),
            Err(error) => {
                crate::warn(&format!(
                    "update record {} is unreadable, so it holds nothing: {error}",
                    path.display()
                ));
                Self::default()
            }
        }
    }

    /// Change the record at `path` with `change`, under its lock (`<path>.lock`, waited for
    /// [`LOCK_WAIT`]). A change that changed something raises `n` and is written durably
    /// (temp file, flush, rename); one that set, ended or cut a hold short or noted a trial
    /// miss also calls the [`install_notify`] hook. An unreadable record is replaced even
    /// when `change` changed nothing.
    /// Returns the record as it now is and what `change` returned.
    ///
    /// # Errors
    /// The lock was not free within [`LOCK_WAIT`], or the write failed: the file is as it was.
    pub fn edit<R>(path: &Path, change: impl FnOnce(&mut Self) -> R) -> io::Result<(Self, R)> {
        let mut lock_path = path.as_os_str().to_owned();
        lock_path.push(".lock");
        let _lock = crate::FileLock::acquire_within(Path::new(&lock_path), LOCK_WAIT)?;
        let (before, unreadable) = match Self::read(path) {
            Ok(record) => (record.unwrap_or_default(), false),
            Err(error) => {
                crate::warn(&format!(
                    "update record {} is unreadable, so it holds nothing and is replaced: \
                     {error}",
                    path.display()
                ));
                (Self::default(), true)
            }
        };
        let mut after = before.clone();
        let out = change(&mut after);
        after.n = before.n;
        after.v = VERSION;
        // The family this build is for: a record carried to another kind of machine (a
        // migrated Updates folder) must not claim the old one, or a peer of that family could
        // lift a hold on this one's evidence.
        after.os = Os::here();
        after.arch = Arch::here();
        if after == before && !unreadable {
            return Ok((after, out));
        }
        // Readers rank a copy's rows by `n`: a record that starts over (missing or unreadable)
        // starts from the clock, so its first write still ranks above every earlier one.
        after.n = before
            .n
            .saturating_add(1)
            .max(aterm_time::unix_secs().unwrap_or(0));
        write_durable(path, after.encode().as_bytes())?;
        let misses = |r: &Self| r.trial.as_ref().map_or(0, |trial| trial.misses);
        if (before.hold != after.hold
            || before.start_failure != after.start_failure
            || before.last != after.last
            || misses(&after) > misses(&before))
            && let Some(notify) = NOTIFY.get()
        {
            notify();
        }
        Ok((after, out))
    }
}

static NOTIFY: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Install the hook [`UpdateRecord::edit`] calls after a write that set, ended or cut short a
/// hold or noted a trial miss: the lab publisher's nudge. The first install wins; returns
/// whether this one did.
pub fn install_notify(notify: Box<dyn Fn() + Send + Sync>) -> bool {
    NOTIFY.set(notify).is_ok()
}

/// Write `bytes` to `path` durably: a `0600` temp file beside it, flushed, renamed over it,
/// and the folder flushed (a folder flush that fails after the rename is logged: the record
/// is in place, though a power loss may take it back).
#[cfg_attr(trust_verify, trust::skip)]
fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let parent = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = path
        .file_name()
        .map_or_else(|| "record".into(), |n| n.to_string_lossy());
    let tmp = parent.join(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = opts.open(&tmp)?;
    let wrote = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        aterm_durable::refusal_accepted(crate::durable::finish(
            &file,
            aterm_durable::Flush::Durable,
        ))?;
        std::fs::rename(&tmp, path)
    })();
    drop(file);
    if let Err(error) = wrote {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = aterm_durable::Flush::Durable.dir_best_effort(parent) {
        crate::warn(&format!(
            "update record {}: written, but flushing its folder failed, so a power loss may \
             take it back: {error}",
            path.display()
        ));
    }
    Ok(())
}

/// The clock crossing a boundary of a hold's time left (`Tick`), which [`Hold::live`] reads:
/// the anchor host of the model's environment.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateHold",
        action = "Tick",
        reason = "Environment: the wall clock advancing; Hold::live and Hold::left read it"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "NativeUpdateHold",
        action = "Rearm",
        reason = "Negative control only: a hold whose time is set past HOLD_MAX again, which Hold::settle and Hold::live refuse"
    )
)]
#[doc(hidden)]
pub fn update_hold_environment() {}

/// A duration class of the model: 0 none, 1 up to an hour, 2 up to six hours, 3 under a
/// day, 4 a day or more. Monotone in `secs`.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub const fn duration_class(secs: u64) -> i64 {
    match secs {
        0 => 0,
        s if s <= HOUR => 1,
        s if s <= 6 * HOUR => 2,
        s if s < HOLD_MAX => 3,
        _ => 4,
    }
}

/// Project `record` at `now` onto `NativeUpdateHold`. `base` is the build the model calls 1:
/// an older one is 0, a newer one 2. `probe` is a gate question just answered, as `(model
/// build asked about, answer)`, with 3 for an artifact the hold does not name.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
#[must_use]
pub fn hold_projection(
    record: &UpdateRecord,
    base: u64,
    now: u64,
    probe: Option<(i64, bool)>,
) -> std::collections::BTreeMap<&'static str, i64> {
    let class = |build: u64| -> i64 {
        match build.cmp(&base) {
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Equal => 1,
            std::cmp::Ordering::Greater => 2,
        }
    };
    let hold = record.hold.as_ref();
    let tries = hold.map_or(0, |h| i64::from(h.tries.min(4)));
    let span = hold.map_or(0, |h| duration_class(h.span));
    // `peak` is what the schedule asks for at the stored count, independent of the stored
    // span, so `RetryEscalates` (`peak <= span`) fails on a record whose span is shorter than
    // its schedule. `fails` shadows `tries` itself: on code, `tries == fails` holds by
    // construction and the count's escalation is checked by the step comparison alone.
    let peak = hold.map_or(0, |h| {
        duration_class(schedule_span(h.cause, h.witness, h.tries))
    });
    let (pb, sup) = probe.map_or((0, 0), |(asked, answer)| (asked, i64::from(answer)));
    std::collections::BTreeMap::from([
        ("offered", record.offered.map_or(1, class)),
        ("running", class(record.running)),
        ("proven", i64::from(record.proven)),
        ("trial", record.trial.as_ref().map_or(0, |t| class(t.build))),
        (
            "tw",
            i64::from(
                record
                    .trial
                    .as_ref()
                    .is_some_and(|t| t.witness == Witness::Window),
            ),
        ),
        ("hb", hold.map_or(0, |h| class(h.build))),
        ("tries", tries),
        ("fails", tries),
        ("span", span),
        ("peak", peak),
        ("left", hold.map_or(0, |h| duration_class(h.left(now)))),
        ("lifts", hold.map_or(0, |h| i64::from(h.peer_lifted))),
        ("pb", pb),
        ("sup", sup),
    ])
}

#[cfg(test)]
#[path = "update_record_tests.rs"]
mod tests;
