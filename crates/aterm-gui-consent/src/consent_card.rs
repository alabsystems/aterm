// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE macOS ACCESS CARD — the one prompt that stands in for the many
//! (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md` §3.4, as amended 2026-09-07).
//!
//! # What this is, and what it replaces
//!
//! macOS asks for consent one folder at a time, in a system modal, at the
//! moment a program first touches the folder — and it asks in the name of the
//! RESPONSIBLE app, which for everything run inside a session is aterm. An
//! agent started in a home directory walks Documents, Desktop, Downloads,
//! other apps' data and the media library within seconds of starting, and the
//! owner takes five dialogs in a row (measured on this tree 2026-09-07:
//! five `AUTHREQ_PROMPTING` lines in six seconds, all `subject=com.aterm.aterm`,
//! all raised by one freshly started coding agent).
//!
//! The single thing macOS offers for that class is ONE Full Disk Access grant
//! for the app, made by a human in System Settings — an app cannot prompt for
//! it and aterm never tries (design §6). The design's §3.4 specified a
//! non-clickable pill to say so once; that pill was authored
//! (in the retired `notice.rs`, `privacy_notice_once`) but never wired, so nothing at launch ever
//! told the owner there was one switch to flip, and the per-folder dialogs
//! stayed the only surface. The owner's direction (2026-09-07) is that aterm
//! consolidate this into one prompt. This module is that prompt: ONE entry of
//! the exceptions digest, "To look at" (owner rule, 2026-09-28: a thing that can
//! wait is one dismissable line, filed in the log, never a row that persists on
//! the glass), with one button — *Open Settings*, which deep-links to the Full
//! Disk Access pane. It began as a decision row on the message band
//! (docs/DESIGN-unified-messages-2026-09-21.md §6 R15–R17, with a *Not now*
//! that recorded a marker; the floating card before it was the same question,
//! 2026-09-07 → 2026-09-22). The entry is `message_reporters::file_access_entry`,
//! its words are [`FDA_WHAT`], [`FDA_WHY`] and [`FDA_THEN`] with three lines behind
//! `Details ›`; the old `Not now` marker still ends the question for good.
//!
//! # What it is not
//!
//! Not a consent dialog: aterm renders nothing macOS would render, answers
//! nothing, and the grant is still made by the human, in Settings (§0.2, §6).
//! Not an auto-open of System Settings: the card's pane opens on a press and
//! never on detection (§6). What brings the Full Disk Access page up unasked is
//! the window's own launch hand-over (owner ask 2026-10-16, a recorded exception
//! beside the Developer Tools page): when [`decide_launch`] answers
//! [`NotDue::NoNeedYet`] with the probe reading `denied`,
//! `App::hand_fda_to_owner` opens the page at a quiet moment with aterm's panel
//! beside it (aterm-gui's `consent_launch` and `devtools_helper::admit_fda`).
//! Not the warm-up: pressing nothing here touches a protected folder, so no
//! dialog is raised by the card itself (§3.5 is the Security panel's gesture and
//! the launch pass in aterm-gui's `consent_launch`, never the card's). And not a
//! coverage claim: the copy names the access check
//! and the Full Disk Access setting without promising fewer interruptions.
//! Which services a held grant silences is §7 S4's measurement and S4 is unrun.
//! A denied access probe is not proof that the System Settings switch is off:
//! the owner may already have enabled it. The card names unconfirmed access,
//! and its Settings follow-up asks the owner to leave an enabled switch on.
//!
//! # When it shows — once per real need, never at launch (the card)
//!
//! The card used to be a per-process machine that offered itself at every
//! start: eight posts in 3.8 days, six of them within half a second of an
//! update, none tied to any dialog (`docs/DESIGN-macos-tcc-prompts-2026-08-30.md`
//! §3.4, as amended 2026-09-28). It is now NEED-driven. The question is about
//! ONE thing — the "access data from other apps" dialog, whose Allow is
//! forgotten as soon as the responsible instance ends — and it is raised only
//! when that dialog has actually been shown for a program of this aterm, or
//! aterm's own protected read was refused ([`NeedCause`]), and at most once per
//! EPOCH: an epoch ends when a verified grant later reads as off, or
//! [`RENEW_AFTER`] passes.
//!
//! [`decide_need`] is the decision and it is pure; [`NotDue`] names the rule
//! that said no, so the log says why. In order, the first that applies wins:
//! the `[privacy]` switches; a headless instance; a prompt for a service the
//! card is not about; *Not now* on record; a grant-UNSTABLE build (an ad-hoc
//! cdhash grant dies on the next build); the grant already held; the owner
//! already acted on a window older than the act ([`NotDue::OwnerActed`]); the
//! probe not having OBSERVED `denied`; an ask already made this epoch.
//!
//! [`decide_launch`] runs once per process at the first park and NEVER posts
//! anything: it only acknowledges a grant (the ✓ record) and logs. The card
//! still never asks at launch; a `NoNeedYet` verdict beside a `denied` probe is
//! what the window's own Full Disk Access hand-over follows
//! (`App::note_launch_fda`).
//!
//! # The three files, and what each records
//!
//! * [`MARKER`] (`privacy-access-card-answered`) — `not-now` (the owner's
//!   answer, permanent) or `opened` (the owner pressed *Open Settings* and the
//!   outcome is not yet known). The NEXT process, finding `opened` beside a
//!   probe that now reads `granted`, shows the ✓ record once and clears it.
//! * [`RECORD`] (`privacy-fda-record`) — four unix-second facts (`asked_at`,
//!   `opened_at`, `verified_at`, `verified_build`), each written as the MAX of
//!   the old and new value so two processes cannot move a field backwards.
//!   This is what makes "once" survive an update: the successor is a new
//!   process with a fresh [`CardState`] and finds the same record.
//!
//! # Lifetime, in one process
//!
//! [`CardState`] is the per-process machine the event loop drives from its
//! park point. `Idle` runs the launch check once (a worker warms the signing
//! identity and reads the marker and the record OFF the event loop). A need
//! event ([`CardState::on_need`]) re-arms a `Settled` card into `Deciding`;
//! the main thread decides against a completed CACHED probe (a pending probe
//! is waited on with the watch's own patience, [`WATCH_FOR`]); a due card posts
//! ONE question through the host seam (`App::raise_fda_need`) and WATCHES the
//! probe for the grant, for at most [`WATCH_FOR`], and the question itself
//! through `center.live(id)`. The prompt-free access probe runs on a detached
//! worker; the event loop never joins it or calls its `open()`. This observes
//! whether access became effective; it does not read the System Settings
//! switch. `Settled` means "not currently active", not "done for this
//! process": a later need may re-arm it, and [`decide_need`] alone decides
//! whether that asks anything.
//!
//! # The words: one source
//!
//! Every string the owner reads about this question is a const here, and the
//! exceptions digest imports them rather than retyping them
//! ([`FDA_WHAT`], [`FDA_WHY`], [`FDA_THEN`], [`FDA_GRANTS`], [`FDA_AFTER`],
//! [`FDA_DETAIL`], [`FDA_TITLE`]). None claims anything S1 (when a running
//! process sees a new grant) or S4 (which services a held grant silences)
//! decides.
//!
//! # No protected-folder literal lives here
//!
//! Nothing in this module names a folder or touches one (the aterm-census guard
//! `protected-folder`).
//! The card's whole contact with the filesystem is the marker and the record
//! beside `aterm.toml`, through [`crate::config_marker`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_containment::consent::{DrClass, FdaState, Folder};

/// The marker beside `aterm.toml` (the config-dir latch idiom of
/// `connections`' first-use notice, [`crate::config_marker`]).
pub const MARKER: &str = "privacy-access-card-answered";

/// A marker holds one token; anything bigger reads as "never answered", so the
/// card shows (erring toward disclosure).
pub const MAX_MARKER_BYTES: usize = 64;

/// The record beside `aterm.toml`: four `key=value` lines. A second file, so
/// an older build reading the marker pair behaves exactly as it did.
pub const RECORD: &str = "privacy-fda-record";

/// A record is four short lines; anything bigger is something else wearing
/// the name and reads as an empty record.
pub const MAX_RECORD_BYTES: usize = 160;

/// How long, after the question is raised, the instance keeps re-checking the
/// probe for the grant — and how long a due card waits on a probe that is
/// still pending before it is retired unraised. Bounded so a process that
/// outlives an unanswered card does not carry a per-park check forever; fresh
/// observations are made off the event loop at the bounded
/// `[privacy] probe_interval_ms` cadence.
pub const WATCH_FOR: Duration = Duration::from_secs(30 * 60);

/// How long the launch one-shot waits for its worker's verdict before asking
/// again. A verdict can be lost: an uncommitted incoming handoff drops every
/// wake but Commit.
const DECIDE_TIMEOUT: Duration = Duration::from_secs(20);

/// How many verdicts the one-shot will wait for before giving up for the life
/// of the process.
pub const DECIDE_ATTEMPTS: u8 = 3;

/// How long after an ask the next one may be made in the same epoch (the row
/// host; the digest host's entry stands instead, with its own `resurface`).
/// The dialogs the question explains recur, so a weekly pointer is not
/// nagging; an ask is never repeated inside the week.
pub const RENEW_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// How long an owner who pressed *Open Settings* is presumed to have acted
/// ([`NotDue::OwnerActed`]): a window that STARTED BEFORE the press cannot be
/// expected to see the switch, and asking again about it would be the
/// non-actionable alert the owner ruled out.
pub const OWNER_ACTED_HOLD: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// How long a fresh process gives the window's own probe before a denied read
/// beside a VERIFIED grant is believed. An update leaves the new build's first
/// seconds inside the `.rollback` window, where a held grant can read off; the
/// need is kept and decided again after this, and only a second denied read
/// (the one this delay buys) marks the grant regressed.
pub const LAUNCH_SETTLE: Duration = Duration::from_secs(90);

/// Needs closer together than this are one need (a `find /` raises a burst of
/// dialogs' worth of log lines in a second).
pub const NEED_DEBOUNCE: Duration = Duration::from_secs(60);

/// At most one `aterm.log` line per this interval says that macOS asked.
pub const NEED_LOG_EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The pre-digest ROW's title (R15): the ASK — a question, so it claims nothing.
/// The digest entry leads with [`FDA_WHAT`] instead; this const is the retired
/// row's (`message_reporters::file_access_question`, test-only since the seam
/// hands the need to the digest), and the words of the doc below are why it
/// stayed short.
///
/// [`FDA_DETAIL`], behind Details, acknowledges that Full Disk Access may
/// already be enabled (the probe does not read the Settings toggle). The copy
/// names no folder, no coverage and no scope, and never promises the
/// interruptions go away: it mitigates, never eliminates.
///
/// The title names the one action, a switch in Settings. The verb is
/// deliberately not the button of the dialog the owner has just met ("Allow"),
/// because the action asked for is a different one. The first detail line
/// ([`FDA_WHY`]) says why, and [`FDA_GRANTS`] says what the switch grants (it
/// grants a lot: every program run under aterm, agents included). It does NOT
/// name a folder, does NOT say which services the grant covers, does NOT say
/// when a running process sees it and does NOT promise the interruptions go
/// away: coverage is §7 S4's claim to make, timing is S1's, and the owner's
/// ruling is that mitigating this annoyance is acceptable, so the row must
/// never read as elimination.
///
/// # Why it is short
///
/// The band's width law keeps a decision row's capsules whole and shortens
/// the words, so on an ordinary window the question must fit beside "Open
/// Settings" and "Not now" or the owner sees two buttons and no ask. Title and
/// first detail together are held under ~75 characters.
#[cfg_attr(not(test), allow(dead_code))]
pub const FDA_TITLE: &str = "Turn on Full Disk Access?";

/// The entry's `what` in the exceptions digest (glass-title form, cap 48).
/// Consumed by `message_reporters::file_access_entry` (design-exceptions §8.2),
/// which imports it instead of retyping it.
pub const FDA_WHAT: &str = "macOS keeps asking for access";

/// The entry's `what` when no prompt was seen: macOS refused aterm's own read
/// under a protected root ([`NeedCause::ProtectedRead`]). Same cap as
/// [`FDA_WHAT`].
pub const FDA_WHAT_DENIED: &str = "macOS refused aterm a protected read";

/// The entry's `what` when a grant aterm had verified reads off again
/// (`NeedWords::regressed`). Same cap as [`FDA_WHAT`].
pub const FDA_WHAT_REGRESSED: &str = "Full Disk Access reads off again";

/// WHY the owner is needed, first: an Allow is forgotten when aterm updates.
/// True of both kinds of tab — for a live tab the Allow is scoped to the
/// window and ends when the window is replaced; for a tab that survived an
/// update it ends earlier, with that program — so "when aterm updates" is an
/// upper bound. Row `detail[0]`; the digest's `Need.why` (cap 90).
pub const FDA_WHY: &str = "macOS forgets an Allow when aterm updates";

/// The ONE action, as an imperative: the entry's `Then::press` sentence (cap 90:
/// no `; `, no ` or `, no newline, no restart phrase). Imported by the digest
/// host like [`FDA_WHAT`].
pub const FDA_THEN: &str = "turn on aterm in Full Disk Access";

/// What the switch GRANTS, said where the ask is. A disclosure of a
/// consequence Apple's own pane states ("data like Mail, Messages, Safari"),
/// not a promise of a benefit: it says nothing about which prompts the switch
/// silences or when a running process sees it. Row `detail[2]`; a `more` line
/// of the digest entry.
pub const FDA_GRANTS: &str = "Full Disk Access lets programs you run in aterm, agents included, read protected data such as Mail and Messages";

/// What aterm DOES next, true whatever S1 answers. Row `detail[4]`.
pub const FDA_AFTER: &str = "aterm checks now and after each update, and says so in \
                                    aterm's Messages once Full Disk Access is on";

/// The hedge, always shown: a failed probe does not read the switch, and a
/// window may not yet see a switch that is on. Row `detail[5]`.
pub const FDA_DETAIL: &str = "Full Disk Access may already be enabled";

/// The ✓ row that supersedes the question once the probe observes the grant
/// (R17; `message_reporters::file_access_granted`). It names the fact and
/// stops: which services the grant covers and how far it reaches are §7 S4's
/// and S1's measurements. The success glyph is the row's own; the words carry
/// no marker. The question's own noun, one clause (audit 2026-09-24): it is a
/// record's title.
pub const GRANTED_CAPTION: &str = "Full Disk Access granted";

/// The words the row takes after *Open Settings*. `openURL:` reports only that
/// System Settings took the URL, never that it scrolled to the row, so the
/// route in words (`route`, from `menu::privacy_settings_path_words`) goes out
/// on every outcome, first. Behind it: how to find aterm there (the `+` adds
/// an unlisted app), what to do if it is already on, and that aterm re-checks
/// its probe; nothing the B10/B12 phrase fence forbids. One sentence in the
/// `"<title> — <detail>"` shape: `messages_host::route_words` splits it at the
/// dash into title and at `; ` into detail lines, the route first
/// (owner, 2026-09-23).
pub fn opened_settings_caption(opened: bool, route: &str) -> String {
    if opened {
        format!(
            "Opened System Settings \u{2014} {route}; find aterm there (+ adds a missing app); \
             if already enabled, leave it on; aterm keeps checking access"
        )
    } else {
        format!(
            "System Settings did not open \u{2014} {route}; find aterm there (+ adds a missing \
             app); if already enabled, leave it on"
        )
    }
}

/// What made the card think the owner is needed. Data only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedCause {
    /// The sentinel saw `AUTHREQ_PROMPTING` for our own subject. `service` is
    /// `Some(folder)` when it is one of the four the roster knows and `None`
    /// for any other service; `session` is the tab the prompting program
    /// belongs to, when it resolved (best effort, never a gate).
    PromptObserved {
        service: Option<Folder>,
        session: Option<u64>,
    },
    /// aterm's own read under a protected root took `EPERM`. `session` is
    /// `None` for aterm's own instance-level work (the warm-up).
    ProtectedRead { session: Option<u64> },
}

impl NeedCause {
    /// The tab's session, when the cause names one.
    pub const fn session(self) -> Option<u64> {
        match self {
            Self::PromptObserved { session, .. } => session,
            Self::ProtectedRead { session } => session,
        }
    }
}

/// The data the words are built from, so the reporter stays pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NeedWords {
    pub cause: NeedCause,
    /// The 1-based tab number the prompting program belongs to, when it
    /// resolved. Never a path, never a program name.
    pub tab_number: Option<usize>,
    /// A verified grant later read as off: the epoch is a new one.
    pub regressed: bool,
}

impl NeedWords {
    /// The event line (the retired row's `detail[1]`): what macOS did, in one
    /// sentence, from the cause. The digest entry holds three `more` lines and
    /// they are the consts', so this one goes to `aterm.log` (`raise_fda_need`).
    pub fn event_line(&self) -> String {
        if self.regressed {
            return "Full Disk Access was on for aterm and macOS now reports it off".to_owned();
        }
        let in_tab = self
            .tab_number
            .map_or_else(String::new, |n| format!(" in tab {n}"));
        match self.cause {
            NeedCause::PromptObserved { .. } => {
                let mut what = Folder::AppData.label().to_owned();
                if let Some(first) = what.get_mut(..1) {
                    first.make_ascii_lowercase();
                }
                format!("macOS asked to let a program{in_tab} read {what}")
            }
            NeedCause::ProtectedRead { .. } => {
                format!("macOS refused aterm a read of protected files{in_tab}")
            }
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl PendingDecision {
    /// A launch-check half as the worker would deliver it: `marker` on disk,
    /// no record.
    pub fn launch(marker: Option<Marker>) -> Self {
        Self {
            marker,
            record: AccessRecord::default(),
            why: DecideWhy::Launch,
            verified_here: None,
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl NeedWords {
    /// A sample need for tests of the reporter and the host: an app-data
    /// prompt from tab 1, first epoch.
    pub fn sample() -> Self {
        Self {
            cause: NeedCause::PromptObserved {
                service: Some(Folder::AppData),
                session: Some(1),
            },
            tab_number: Some(1),
            regressed: false,
        }
    }
}

/// What the marker records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// *Not now* was pressed: the card is not offered again for the life of
    /// the config directory.
    NotNow,
    /// *Open Settings* was pressed and the outcome is not yet known (see the
    /// module doc). Consumed by the decision that resolves it.
    Opened,
}

impl Marker {
    /// The recorded token.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotNow => "not-now",
            Self::Opened => "opened",
        }
    }

    /// Parse a marker's text; anything else is not a record this code wrote.
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "not-now" => Some(Self::NotNow),
            "opened" => Some(Self::Opened),
            _ => None,
        }
    }
}

/// Where the marker lives for this config, or `None` with no config path.
pub fn marker_path(config_path: Option<&Path>) -> Option<PathBuf> {
    config_path
        .and_then(Path::parent)
        .map(|dir| dir.join(MARKER))
}

/// The recorded marker, if any. A disk read — worker thread only.
pub fn read_marker(config_path: Option<&Path>) -> Option<Marker> {
    marker_path(config_path)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_MARKER_BYTES))
        .and_then(|text| Marker::parse(&text))
}

/// Every marker and record write in this process holds this: the owner's
/// answer and an `opened` can be recorded from two workers at once, and
/// `record_marker`'s keep-`not-now` check (and the record's read-merge-write)
/// must see the other's write, not race it.
static MARKER_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Record `marker`. Best-effort, worker thread only: an unwritable config
/// directory means the card comes back at the next need, which errs toward
/// disclosure. An `opened` never replaces a `not-now` (see the body).
pub fn record_marker(config_path: Option<&Path>, marker: Marker) -> std::io::Result<()> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = marker_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record the answer in",
        ));
    };
    // An `opened` never replaces a `not-now`: the Security page's *Open Privacy
    // & Security…* (which `key` can press) is not an answer to the card, and
    // replacing the marker would bring back a declined question.
    if marker == Marker::Opened
        && crate::config_marker::read_marker(&path, MAX_MARKER_BYTES)
            .and_then(|text| Marker::parse(&text))
            == Some(Marker::NotNow)
    {
        return Ok(());
    }
    // Ordered: a lost or torn marker reads as none and brings the card back, which
    // errs toward disclosure.
    crate::config_marker::write_marker(
        &path,
        marker.as_str(),
        MAX_MARKER_BYTES,
        crate::durable::Flush::Ordered,
    )
}

/// Clear an `opened` marker once its grant has been acknowledged. A `not-now`
/// answer is never touched. Worker thread only.
pub fn clear_opened(config_path: Option<&Path>) -> std::io::Result<()> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = marker_path(config_path) else {
        return Ok(());
    };
    if crate::config_marker::read_marker(&path, MAX_MARKER_BYTES)
        .and_then(|text| Marker::parse(&text))
        != Some(Marker::Opened)
    {
        return Ok(());
    }
    crate::config_marker::remove_marker(&path)
}

/// What the record file holds: unix seconds, each field independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessRecord {
    /// When the question was last raised.
    pub asked_at: Option<u64>,
    /// When the owner last pressed *Open Settings* (through either surface).
    pub opened_at: Option<u64>,
    /// When the window's own probe last read the grant as held.
    pub verified_at: Option<u64>,
    /// The build number that was running then.
    pub verified_build: Option<u64>,
}

impl AccessRecord {
    /// The field-wise MAX of two records: no field ever moves backwards, so
    /// two processes writing at once cannot undo each other.
    pub fn merged(self, other: Self) -> Self {
        let max = |a: Option<u64>, b: Option<u64>| match (a, b) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        Self {
            asked_at: max(self.asked_at, other.asked_at),
            opened_at: max(self.opened_at, other.opened_at),
            verified_at: max(self.verified_at, other.verified_at),
            verified_build: max(self.verified_build, other.verified_build),
        }
    }

    /// The file's text: only the fields that are set.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (key, value) in [
            ("asked_at", self.asked_at),
            ("opened_at", self.opened_at),
            ("verified_at", self.verified_at),
            ("verified_build", self.verified_build),
        ] {
            if let Some(value) = value {
                out.push_str(&format!("{key}={value}\n"));
            }
        }
        out
    }

    /// Parse a record's text. STRICT: a line that is not `key=<unsigned>` for
    /// one of the four keys makes the whole file "not a record this code
    /// wrote" and it reads as empty (the card errs toward speaking once,
    /// never toward a crash or a guess).
    pub fn parse(text: &str) -> Self {
        let mut record = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Self::default();
            };
            let Ok(value) = value.trim().parse::<u64>() else {
                return Self::default();
            };
            let slot = match key.trim() {
                "asked_at" => &mut record.asked_at,
                "opened_at" => &mut record.opened_at,
                "verified_at" => &mut record.verified_at,
                "verified_build" => &mut record.verified_build,
                _ => return Self::default(),
            };
            *slot = Some(value);
        }
        record
    }
}

/// Where the record lives for this config, or `None` with no config path.
pub fn record_path(config_path: Option<&Path>) -> Option<PathBuf> {
    config_path
        .and_then(Path::parent)
        .map(|dir| dir.join(RECORD))
}

/// The recorded facts; empty when there is no record. A disk read — worker
/// thread only.
pub fn read_record(config_path: Option<&Path>) -> AccessRecord {
    record_path(config_path)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_RECORD_BYTES))
        .map(|text| AccessRecord::parse(&text))
        .unwrap_or_default()
}

/// Merge `update` into the record on disk (field-wise max, under the shared
/// write lock) and return what is there now. Best-effort, worker thread only.
pub fn write_record(
    config_path: Option<&Path>,
    update: AccessRecord,
) -> std::io::Result<AccessRecord> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = record_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to keep the record in",
        ));
    };
    let current = crate::config_marker::read_marker(&path, MAX_RECORD_BYTES)
        .map(|text| AccessRecord::parse(&text))
        .unwrap_or_default();
    let merged = current.merged(update);
    if merged != current || !path.exists() {
        // Ordered: a field-wise max merge; an older or absent record parses to the
        // default and the next event merges it again.
        crate::config_marker::write_marker(
            &path,
            &merged.render(),
            MAX_RECORD_BYTES,
            crate::durable::Flush::Ordered,
        )?;
    }
    Ok(merged)
}

/// Where a grant was verified: `<hardware UUID> <bundle id>` per line
/// (`aterm_containment::consent::host_uuid`), beside the record. The record lives in the
/// config directory, which a dotfile sync or Migration Assistant carries to another Mac,
/// and which every aterm on a Mac shares (the release, `aterm (dev)`, an ad hoc build:
/// each its own TCC identity); this says whether the `verified_at` there was seen HERE,
/// for THIS bundle. A set, not the last line: two Macs or two bundles sharing the
/// directory must not erase each other. Its own file, because [`AccessRecord::parse`]
/// reads a record with an unknown key as empty.
pub const VERIFIED_HOST: &str = "privacy-fda-verified-host";

/// The most Macs the stamp names; the oldest leaves first.
const MAX_VERIFIED_HOSTS: usize = 16;

/// Each line is a 36-character UUID, a space, a bundle id and a newline.
const MAX_VERIFIED_HOST_BYTES: usize = MAX_VERIFIED_HOSTS * 160;

fn verified_host_path(config_path: Option<&Path>) -> Option<PathBuf> {
    config_path
        .and_then(Path::parent)
        .map(|dir| dir.join(VERIFIED_HOST))
}

/// One stamp line: this Mac and this bundle.
fn verified_key(here: &str, bundle_id: &str) -> String {
    format!("{here} {bundle_id}")
}

/// Whether the record's grant was verified on the Mac whose hardware UUID is `here`, for
/// `bundle_id`: `None` when any is unknown (no stamp — a record from an older build — no
/// UUID, no bundle id), which keeps the record's word. A disk read — worker thread only.
pub fn verified_here(
    config_path: Option<&Path>,
    here: Option<&str>,
    bundle_id: Option<&str>,
) -> Option<bool> {
    let stamp = verified_host_path(config_path)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_VERIFIED_HOST_BYTES))?;
    let key = verified_key(here?, bundle_id?);
    Some(stamp.lines().any(|line| line.trim() == key))
}

/// The stamp's next text: `current`'s lines with `key` added (none twice), the newest
/// [`MAX_VERIFIED_HOSTS`] kept. `None` when `key` is already there. Pure.
fn with_verified_host(current: &str, key: &str) -> Option<String> {
    let mut hosts: Vec<&str> = current
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if hosts.contains(&key) {
        return None;
    }
    hosts.push(key);
    let skip = hosts.len().saturating_sub(MAX_VERIFIED_HOSTS);
    Some(
        hosts[skip..]
            .iter()
            .map(|host| format!("{host}\n"))
            .collect(),
    )
}

/// Note that the grant was verified on the Mac whose hardware UUID is `here`, for
/// `bundle_id`. Best-effort, worker thread only.
pub fn write_verified_host(
    config_path: Option<&Path>,
    here: &str,
    bundle_id: &str,
) -> std::io::Result<()> {
    let _serial = MARKER_WRITES.lock().unwrap_or_else(|p| p.into_inner());
    let Some(path) = verified_host_path(config_path) else {
        return Ok(());
    };
    let current =
        crate::config_marker::read_marker(&path, MAX_VERIFIED_HOST_BYTES).unwrap_or_default();
    match with_verified_host(&current, &verified_key(here, bundle_id)) {
        // Ordered: a lost line means the grant is not seen here, so it is verified
        // again; the set only grows.
        Some(text) => crate::config_marker::write_marker(
            &path,
            &text,
            MAX_VERIFIED_HOST_BYTES,
            crate::durable::Flush::Ordered,
        ),
        None => Ok(()),
    }
}

/// This process's start, in unix seconds. Set by [`note_process_start`] at the
/// top of the GUI's entry; a successor is a new process and so a new value.
static PROCESS_STARTED_AT: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// The current unix time in seconds (`0` if the clock is before the epoch).
pub fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Stamp this process's start. Idempotent: the first call wins.
pub fn note_process_start() {
    let _ = PROCESS_STARTED_AT.set(now_unix_secs());
}

/// When this process started, in unix seconds ([`note_process_start`]; a
/// process that never stamped reports its first ask, which errs toward the
/// window being NEWER than any press, i.e. toward speaking).
pub fn process_started_at() -> u64 {
    *PROCESS_STARTED_AT.get_or_init(now_unix_secs)
}

/// Why a worker was started: the launch check (never asks) or a need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecideWhy {
    Launch,
    Need(NeedCause),
}

/// The worker's half of a decision, held by the host while a pending probe is
/// waited on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingDecision {
    pub marker: Option<Marker>,
    pub record: AccessRecord,
    pub why: DecideWhy,
    /// Whether the record's grant was verified on this Mac ([`verified_here`]).
    pub verified_here: Option<bool>,
}

/// The host-side state of the card that is not the lifecycle machine: the
/// in-flight decision, the last need, the words, and a cache of what the
/// worker read from disk merged with what this process wrote since. One
/// struct so `App` carries one field.
#[derive(Clone, Copy, Debug, Default)]
pub struct CardHost {
    /// A verdict that arrived while the probe was still pending.
    pub pending: Option<PendingDecision>,
    /// The cause the IN-FLIGHT need worker was started for (kept across a
    /// lapse so the retry decides the same need, not a launch check).
    pub need: Option<NeedCause>,
    /// The latest need that arrived while the card could not decide it (busy,
    /// launch check still to run, an incoming handoff uncommitted, no event
    /// loop). Decided when the card is idle again.
    pub kept_need: Option<NeedCause>,
    /// The words of the question that is due.
    pub words: Option<NeedWords>,
    /// The record on disk, as last read, merged with this process's writes.
    pub record: AccessRecord,
    /// The marker on disk, as last read or written.
    pub marker: Option<Marker>,
    /// Need events counted this process.
    pub needs: u32,
    /// The last need that was not coalesced ([`NEED_DEBOUNCE`]).
    pub need_last: Option<Instant>,
    /// The last time a need was written to `aterm.log` ([`NEED_LOG_EVERY`]).
    pub need_logged: Option<Instant>,
    /// A kept need must not be decided again before this
    /// ([`NotDue::Settling`]).
    pub settle_retry_at: Option<Instant>,
}

/// The `state=` token of the `access_card` line of `aterm ctl privacy`: where
/// the question stands, from the marker, the record and the window's own probe.
/// `quiet` (never asked, or the week has run out), `asked`, `opened` (the owner
/// pressed *Open Settings* and no verification followed), `granted`, `declined`.
pub fn report_state(
    marker: Option<Marker>,
    record: &AccessRecord,
    fda: FdaState,
    now_secs: u64,
) -> &'static str {
    if marker == Some(Marker::NotNow) {
        return "declined";
    }
    if fda == FdaState::Granted {
        return "granted";
    }
    // `opened` lapses with the rule it reports (`OwnerActed`): past the hold the
    // card would ask again, so the token must not tell a reader nothing more
    // will be asked.
    if record.opened_at.is_some_and(|at| {
        now_secs < at.saturating_add(OWNER_ACTED_HOLD.as_secs())
            && record.verified_at.is_none_or(|verified| verified <= at)
    }) {
        return "opened";
    }
    if record.asked_at.is_some_and(|at| {
        now_secs < at.saturating_add(RENEW_AFTER.as_secs())
            && record.verified_at.is_none_or(|verified| verified <= at)
    }) {
        return "asked";
    }
    "quiet"
}

/// Everything the launch decision reads. All of it observed; none of it
/// inferred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardFacts {
    /// `[privacy] enabled && [privacy] notice`.
    pub offered: bool,
    /// The instance has no window (inert probe arms).
    pub headless: bool,
    /// The cached Full Disk Access verdict.
    pub fda: FdaState,
    /// The designated requirement class a grant would be keyed to.
    pub dr: DrClass,
}

/// Everything the need decision reads: the launch facts plus the clock, this
/// process's age and why the card was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NeedFacts {
    pub card: CardFacts,
    pub now_secs: u64,
    /// When THIS process started ([`process_started_at`]).
    pub process_started_at: u64,
    pub cause: NeedCause,
}

/// Why the card is NOT due — one named reason, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotDue {
    /// `[privacy] enabled = false` or `[privacy] notice = false`.
    Off,
    /// A headless instance cannot observe a denial.
    Headless,
    /// *Not now* is already on record.
    Answered,
    /// The probe reports the grant is held.
    Granted,
    /// The probe did not observe a denial (`unknown`: refused, or the lane
    /// is off) — not a denial, so not a card.
    NotObservedDenied,
    /// A grant keyed to this requirement class would not survive a rebuild.
    GrantUnstable(DrClass),
    /// The prompt was for a service this card is not about (its Allow is
    /// durable, so one answer ends it): a log line only.
    NotThisClass,
    /// The owner pressed *Open Settings* and this window started before the
    /// press: it cannot be expected to see the switch, and asking again would
    /// be the non-actionable alert the owner ruled out.
    OwnerActed,
    /// The question was already asked this epoch.
    RecentlyAsked,
    /// The launch check never asks: the card waits for a real need.
    NoNeedYet,
    /// The launch check found the grant held, as it was before.
    Held,
    /// A grant was verified before and the probe reads off now; the words of
    /// the next ask will say so.
    ReadsOffAfterVerified,
    /// A verified grant reads off inside [`LAUNCH_SETTLE`] of this process
    /// starting: not concluded. The host keeps the need and decides it again
    /// once the window has settled.
    Settling,
}

impl std::fmt::Display for NotDue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("[privacy] notice is off"),
            Self::Headless => f.write_str("headless instance"),
            Self::Answered => f.write_str("already answered (not now)"),
            Self::Granted => f.write_str("full disk access is granted"),
            Self::NotObservedDenied => f.write_str("the probe did not observe a denial"),
            Self::GrantUnstable(dr) => {
                write!(f, "dr={}: a grant would not survive a rebuild", dr.as_str())
            }
            Self::NotThisClass => f.write_str("a prompt for a service whose Allow is durable"),
            Self::OwnerActed => f.write_str(
                "the owner opened Settings and this window started before that; it may not see the switch",
            ),
            Self::RecentlyAsked => f.write_str("already asked this epoch"),
            Self::NoNeedYet => {
                f.write_str("no need yet: the card asks only after macOS shows the dialog")
            }
            Self::Held => f.write_str("full disk access is held, as verified before"),
            Self::ReadsOffAfterVerified => {
                f.write_str("full disk access was verified before and reads off now")
            }
            Self::Settling => f.write_str(
                "full disk access was verified before and reads off in this process's first \
                 seconds; read again once it has settled",
            ),
        }
    }
}

/// What the need decision resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NeedVerdict {
    /// Raise the question once the cached probe confirms the denial.
    /// `regressed`: a verified grant later read as off, so the words say so.
    Ask { regressed: bool },
    /// Nothing to show, and why.
    Quiet(NotDue),
}

/// What the launch decision resolved to. NEVER an ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchVerdict {
    /// The grant the owner started (or was asked for) is now held: record it
    /// once (the ✓ record), clear the `opened` marker, write `verified_at`.
    Confirm,
    /// Nothing to show, and why.
    Quiet(NotDue),
}

/// What the card's lifecycle machine records of a decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Post the question once the cached probe confirms the denial.
    Offer,
    /// Post the ✓ record and clear the `opened` marker.
    Confirm,
    /// Nothing to show, and why.
    Quiet(NotDue),
}

impl From<&NeedVerdict> for Verdict {
    fn from(verdict: &NeedVerdict) -> Self {
        match verdict {
            NeedVerdict::Ask { .. } => Self::Offer,
            NeedVerdict::Quiet(why) => Self::Quiet(why.clone()),
        }
    }
}

impl From<&LaunchVerdict> for Verdict {
    fn from(verdict: &LaunchVerdict) -> Self {
        match verdict {
            LaunchVerdict::Confirm => Self::Confirm,
            LaunchVerdict::Quiet(why) => Self::Quiet(why.clone()),
        }
    }
}

/// Whether a need verdict is the ANOMALY the log keeps a trace of: macOS showed
/// a dialog for a program of this aterm while this process reads Full Disk
/// Access as granted (a program whose macOS record does not match the
/// window's). It asks nothing, since the switch is on and the owner has
/// nothing to do; the log line is the only trace.
pub fn is_prompt_while_granted(cause: NeedCause, verdict: &NeedVerdict) -> bool {
    matches!(cause, NeedCause::PromptObserved { .. })
        && matches!(verdict, NeedVerdict::Quiet(NotDue::Granted))
}

/// THE NEED DECISION. Pure over the facts, the marker and the record; see the
/// module doc for why each rule exists. The first that applies wins.
pub fn decide_need(
    facts: &NeedFacts,
    marker: Option<Marker>,
    record: &AccessRecord,
) -> NeedVerdict {
    let card = &facts.card;
    if !card.offered {
        return NeedVerdict::Quiet(NotDue::Off);
    }
    if card.headless {
        return NeedVerdict::Quiet(NotDue::Headless);
    }
    if let NeedCause::PromptObserved { service, .. } = facts.cause
        && service != Some(Folder::AppData)
    {
        return NeedVerdict::Quiet(NotDue::NotThisClass);
    }
    if marker == Some(Marker::NotNow) {
        return NeedVerdict::Quiet(NotDue::Answered);
    }
    if !card.dr.grant_stable() {
        return NeedVerdict::Quiet(NotDue::GrantUnstable(card.dr));
    }
    if card.fda == FdaState::Granted {
        return NeedVerdict::Quiet(NotDue::Granted);
    }
    // A VERIFIED GRANT THAT READS OFF IN A FRESH PROCESS DOES NOT CONCLUDE: the
    // probe may have raced the update (the `.rollback` window). The host keeps
    // the need and decides it again after `LAUNCH_SETTLE`; a second denied read
    // is what marks the grant regressed.
    if card.fda == FdaState::Denied
        && record.verified_at.is_some()
        && facts.now_secs
            < facts
                .process_started_at
                .saturating_add(LAUNCH_SETTLE.as_secs())
    {
        return NeedVerdict::Quiet(NotDue::Settling);
    }
    // THE OWNER WHO ACTED. `opened` is set, the hold has not lapsed, no
    // verification is newer than the press, and this process started BEFORE
    // the press. If a grant reaches running processes, the probe would already
    // read `granted` (the rule above); if it does not, this window cannot see
    // the switch, and a further ask is exactly the alert that cannot be acted
    // on. A window that started AFTER the press and still reads `denied` is
    // evidence the switch is off, and the renewal below decides.
    if let Some(opened_at) = record.opened_at
        && facts.now_secs < opened_at.saturating_add(OWNER_ACTED_HOLD.as_secs())
        && record
            .verified_at
            .is_none_or(|verified| verified <= opened_at)
        && facts.process_started_at < opened_at
    {
        return NeedVerdict::Quiet(NotDue::OwnerActed);
    }
    if card.fda != FdaState::Denied {
        return NeedVerdict::Quiet(NotDue::NotObservedDenied);
    }
    if let Some(asked_at) = record.asked_at
        && facts.now_secs < asked_at.saturating_add(RENEW_AFTER.as_secs())
        && record
            .verified_at
            .is_none_or(|verified| verified <= asked_at)
    {
        return NeedVerdict::Quiet(NotDue::RecentlyAsked);
    }
    NeedVerdict::Ask {
        regressed: record.verified_at.is_some(),
    }
}

/// THE LAUNCH DECISION: runs once per process at the first park and never
/// posts a question. An `opened` marker (or an ask on record) beside a probe
/// that now reads `granted` is confirmed once; everything else is a log line.
/// A [`NotDue::NoNeedYet`] beside a probe reading `denied` is what the window's
/// own Full Disk Access hand-over follows (`App::note_launch_fda`); the card
/// itself asks nothing.
pub fn decide_launch(
    facts: &CardFacts,
    marker: Option<Marker>,
    record: &AccessRecord,
) -> LaunchVerdict {
    if !facts.offered {
        return LaunchVerdict::Quiet(NotDue::Off);
    }
    if facts.headless {
        return LaunchVerdict::Quiet(NotDue::Headless);
    }
    if marker == Some(Marker::NotNow) {
        return LaunchVerdict::Quiet(NotDue::Answered);
    }
    match facts.fda {
        FdaState::Granted => {
            if marker == Some(Marker::Opened)
                || (record.asked_at.is_some() && record.verified_at.is_none())
            {
                LaunchVerdict::Confirm
            } else if record.verified_at.is_some() {
                LaunchVerdict::Quiet(NotDue::Held)
            } else {
                LaunchVerdict::Quiet(NotDue::Granted)
            }
        }
        FdaState::Denied if record.verified_at.is_some() => {
            LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified)
        }
        FdaState::Denied | FdaState::Unknown => LaunchVerdict::Quiet(NotDue::NoNeedYet),
    }
}

/// Whether a new process should RE-ASSERT a standing question at its launch
/// check: the record says it was asked, no grant was verified after, and the
/// owner has neither declined nor acted (`marker` is empty). Only the digest
/// host acts on it (`App::reassert_fda_need`); the row host has nothing to
/// re-assert, a row being a moment and not a standing fact.
pub fn should_reassert(record: &AccessRecord, marker: Option<Marker>, fda: FdaState) -> bool {
    marker.is_none()
        && fda != FdaState::Granted
        && record.asked_at.is_some()
        && record
            .verified_at
            .is_none_or(|verified| verified <= record.asked_at.unwrap_or(0))
}

/// The per-process lifecycle of the card, driven from the event loop's park
/// point. Every transition is a method, so the machine is table-testable
/// without an `App`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardPhase {
    /// Nothing asked yet — the launch-time one-shot has not run.
    Idle,
    /// The worker is warming the identity and reading the marker and the
    /// record, since `since` — stamped so a verdict that never arrives
    /// ([`DECIDE_TIMEOUT`]) is asked for again instead of waiting for a wake
    /// that cannot come.
    Deciding { since: Instant },
    /// Due; the question is raised at the next park whose cached probe still
    /// reads `denied` (a pending probe is waited on, for at most
    /// [`WATCH_FOR`]).
    Due,
    /// The question is raised since `since` (or the owner opened Settings
    /// then); the instance watches the probe for the grant and the question
    /// for its end.
    Watching { since: Instant },
    /// Not currently active. A later need ([`CardState::on_need`]) re-arms it.
    Settled,
}

/// See [`CardPhase`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardState {
    phase: CardPhase,
    /// When the offer became due — the anchor of a due card's wait on a
    /// pending probe.
    due_since: Option<Instant>,
    /// When the question was FIRST raised in this episode.
    first_raised: Option<Instant>,
    /// The owner pressed something on the row (Open Settings, or Details).
    owner_acted: bool,
    /// How many times the one-shot has asked its worker for a verdict
    /// ([`DECIDE_ATTEMPTS`]).
    decide_attempts: u8,
    /// A need arrived while the card was busy (launch check under way, or a
    /// question already raised): it is re-evaluated the moment the phase
    /// settles ([`CardState::take_pending_need`]).
    pending_need: bool,
}

impl CardState {
    pub const fn new() -> Self {
        Self {
            phase: CardPhase::Idle,
            due_since: None,
            decide_attempts: 0,
            first_raised: None,
            owner_acted: false,
            pending_need: false,
        }
    }

    pub const fn phase(self) -> CardPhase {
        self.phase
    }

    /// Every active phase owns an absolute expiry, even with no probe result.
    /// The host combines this with its refresh deadline, so a slow or stuck
    /// probe cannot extend a phase's lifetime.
    pub fn lifecycle_deadline(self) -> Option<Instant> {
        match self.phase {
            CardPhase::Idle | CardPhase::Settled => None,
            CardPhase::Deciding { since } => Some(since + DECIDE_TIMEOUT),
            CardPhase::Due => self.due_since.map(|since| since + WATCH_FOR),
            CardPhase::Watching { since } => Some(since + WATCH_FOR),
        }
    }

    /// Retire an expired probe wait or grant watch BEFORE asking for another
    /// observation. Deciding uses its bounded retry path instead; this method
    /// never consumes a marker that the next process may still acknowledge.
    pub fn expire_wait(&mut self, now: Instant) -> bool {
        if matches!(self.phase, CardPhase::Due | CardPhase::Watching { .. })
            && self
                .lifecycle_deadline()
                .is_some_and(|deadline| now >= deadline)
        {
            self.settle();
            true
        } else {
            false
        }
    }

    /// Admit one lifecycle step against the CURRENT settings, including a
    /// pending worker verdict or a raised question. Turning the notice off is
    /// an answer for this process: a late worker cannot bring it back. The
    /// host also resolves the card's own rows (`resolve_key_prefix("privacy.")`).
    pub fn admit_current_policy(&mut self, enabled: bool, notice: bool) -> bool {
        if enabled && notice {
            true
        } else {
            self.settle();
            // A need kept from before is answered too: re-enabling the notice
            // waits for a NEW need.
            self.pending_need = false;
            false
        }
    }

    /// The launch one-shot fired: a worker is deciding. Only from
    /// [`CardPhase::Idle`].
    pub fn begin_deciding(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Idle {
            return false;
        }
        self.phase = CardPhase::Deciding { since: now };
        self.decide_attempts = self.decide_attempts.saturating_add(1);
        true
    }

    /// A NEED arrived (macOS showed the dialog, or aterm's own read was
    /// refused). From `Settled` the card re-arms into `Deciding` with a fresh
    /// attempt budget and a clean episode, and `true` says the host should
    /// start its worker. In any other phase the need is kept
    /// ([`Self::take_pending_need`]) and `false` is returned: `Idle` has its
    /// launch check still to run, `Deciding` has a verdict coming, and a `Due`
    /// or `Watching` card already covers it.
    pub fn on_need(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Settled {
            self.pending_need = true;
            return false;
        }
        *self = Self {
            phase: CardPhase::Deciding { since: now },
            decide_attempts: 1,
            ..Self::new()
        };
        true
    }

    /// Keep a need without acting on it (the host cannot decide right now: an
    /// incoming handoff is uncommitted, or there is no event loop yet). It is
    /// given up by [`Self::take_pending_need`] once the card is settled.
    pub fn defer_need(&mut self) {
        self.pending_need = true;
    }

    /// The grant answers a need that was kept before it: drop the kept need
    /// without deciding it (the dialog it stands for predates the grant, and
    /// a later dialog is a new need).
    pub fn discard_pending_need(&mut self) {
        self.pending_need = false;
    }

    /// Whether a need was kept while busy; consumed by the call. Only a
    /// `Settled` card gives it up (the host then calls [`Self::on_need`]).
    pub fn take_pending_need(&mut self) -> bool {
        if self.phase == CardPhase::Settled && self.pending_need {
            self.pending_need = false;
            true
        } else {
            false
        }
    }

    /// The worker's verdict has not arrived within [`DECIDE_TIMEOUT`].
    pub fn deciding_lapsed(&self, now: Instant) -> bool {
        matches!(self.phase, CardPhase::Deciding { since }
            if now.duration_since(since) >= DECIDE_TIMEOUT)
    }

    /// A lapsed decision: ask again from `Idle`, or settle for good once
    /// [`DECIDE_ATTEMPTS`] are spent. `true` when another attempt will be made.
    /// Deciding is not terminal, so a dropped wake (an incoming handoff swallows
    /// every non-Commit wake until it commits) cannot silently kill the card.
    pub fn reopen_deciding(&mut self) -> bool {
        if self.decide_attempts >= DECIDE_ATTEMPTS {
            self.settle();
            return false;
        }
        self.phase = CardPhase::Idle;
        true
    }

    /// The main thread decided. A verdict that arrives in any phase but
    /// [`CardPhase::Deciding`] is stale (a second worker, a re-entrant wake)
    /// and is ignored. `Confirm` settles here: the caller posts the ✓ record
    /// and clears the marker.
    pub fn on_decided(&mut self, verdict: &Verdict, now: Instant) -> bool {
        if !matches!(self.phase, CardPhase::Deciding { .. }) {
            return false;
        }
        self.phase = match verdict {
            Verdict::Offer => {
                self.due_since.get_or_insert(now);
                CardPhase::Due
            }
            Verdict::Confirm | Verdict::Quiet(_) => CardPhase::Settled,
        };
        true
    }

    /// Whether the question may be raised at `now`: only while due, and only
    /// inside the wait a pending probe is given ([`WATCH_FOR`] from
    /// `due_since`) — a probe that never completes does not keep a card due
    /// for the life of the process.
    pub fn raise_allowed(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Due {
            return false;
        }
        !self.expire_wait(now)
    }

    /// The question was raised at `now`.
    pub fn on_raised(&mut self, now: Instant) {
        if self.phase == CardPhase::Due {
            self.phase = CardPhase::Watching { since: now };
            self.first_raised.get_or_insert(now);
        }
    }

    /// The owner opened the row's Details: read; the watch continues.
    pub fn on_owner_acted(&mut self) {
        if matches!(self.phase, CardPhase::Watching { .. }) {
            self.owner_acted = true;
        }
    }

    /// The row left the band without a press — it folded on its own patience,
    /// and neither capsule nor Details was ever touched. That is the
    /// unanswered path: settled (`true`); `asked_at` is on record, so the next
    /// ask waits for [`RENEW_AFTER`]. A row the owner acted on keeps its watch
    /// (`false`): Open Settings restated it to the route words and the grant
    /// may still be observed after those fold; Details marked it read.
    pub fn on_row_retired(&mut self) -> bool {
        if !matches!(self.phase, CardPhase::Watching { .. }) || self.owner_acted {
            return false;
        }
        self.settle();
        true
    }

    /// The owner pressed *Open Settings* through either UI: start a bounded
    /// watch even if the card was idle or settled. The host admits this
    /// explicit gesture against current settings before calling it.
    pub fn on_opened_settings(&mut self, now: Instant) {
        self.phase = CardPhase::Watching { since: now };
        self.owner_acted = true;
    }

    /// Whether the probe should be re-checked at `now`. True only while
    /// watching and inside [`WATCH_FOR`]; a watch past its window settles.
    pub fn wants_probe(&mut self, now: Instant) -> bool {
        matches!(self.phase, CardPhase::Watching { .. }) && !self.expire_wait(now)
    }

    /// Nothing more in this episode: *Not now*, the grant observed, or the
    /// decision said no. A later need may re-arm the card.
    pub fn settle(&mut self) {
        self.phase = CardPhase::Settled;
    }
}

impl Default for CardState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AccessRecord, CardFacts, CardPhase, CardState, DECIDE_ATTEMPTS, DECIDE_TIMEOUT,
        LAUNCH_SETTLE, LaunchVerdict, MARKER, MAX_MARKER_BYTES, MAX_RECORD_BYTES, Marker,
        NeedCause, NeedFacts, NeedVerdict, NeedWords, NotDue, OWNER_ACTED_HOLD, RECORD,
        RENEW_AFTER, Verdict, WATCH_FOR, clear_opened, decide_launch, decide_need,
        is_prompt_while_granted, marker_path, read_marker, read_record, record_marker, record_path,
        report_state, should_reassert, write_record, write_verified_host,
    };
    use aterm_containment::consent::{DrClass, FdaState, Folder};

    /// The stamp is a set: each Mac that verified the grant stays named whatever another
    /// sharing the directory writes later, none twice, the newest sixteen kept; a Mac
    /// not named reads `Some(false)`, and no stamp or no UUID reads `None`.
    #[test]
    fn the_verified_stamp_keeps_every_mac_that_verified() {
        use super::{MAX_VERIFIED_HOSTS, verified_here, with_verified_host, write_verified_host};
        let a = "AAAAAAAA-0000-0000-0000-000000000000";
        let b = "BBBBBBBB-0000-0000-0000-000000000000";
        let once = with_verified_host("", a).expect("a new Mac is added");
        assert_eq!(once, format!("{a}\n"));
        assert_eq!(with_verified_host(&once, a), None, "never twice");
        let both = with_verified_host(&once, b).expect("a second Mac is added");
        assert_eq!(both, format!("{a}\n{b}\n"));
        let mut many = String::new();
        for n in 0..=MAX_VERIFIED_HOSTS {
            many = with_verified_host(&many, &format!("{n:08}-0000-0000-0000-000000000000"))
                .expect("added");
        }
        assert_eq!(many.lines().count(), MAX_VERIFIED_HOSTS);
        assert!(!many.contains("00000000-0000"), "the oldest left first");

        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("aterm.toml");
        let release = Some("com.aterm.aterm");
        let dev = Some("com.aterm.aterm.dev");
        assert_eq!(
            verified_here(Some(&config), Some(a), release),
            None,
            "no stamp"
        );
        write_verified_host(Some(&config), a, "com.aterm.aterm").expect("stamp");
        write_verified_host(Some(&config), b, "com.aterm.aterm").expect("stamp");
        assert_eq!(verified_here(Some(&config), Some(a), release), Some(true));
        assert_eq!(verified_here(Some(&config), Some(b), release), Some(true));
        assert_eq!(
            verified_here(Some(&config), Some(a), dev),
            Some(false),
            "another bundle's grant on this Mac is not this one's"
        );
        assert_eq!(
            verified_here(
                Some(&config),
                Some("CCCCCCCC-0000-0000-0000-000000000000"),
                release
            ),
            Some(false)
        );
        assert_eq!(verified_here(Some(&config), None, release), None, "no UUID");
        assert_eq!(
            verified_here(Some(&config), Some(a), None),
            None,
            "no bundle id"
        );
    }
    use std::time::{Duration, Instant};

    fn denied_release() -> CardFacts {
        CardFacts {
            offered: true,
            headless: false,
            fda: FdaState::Denied,
            dr: DrClass::Identity,
        }
    }

    /// The two bounded waiting phases share one expiry contract: the clock step
    /// is the host reaching the phase's absolute deadline, which a pending
    /// observation cannot extend. `Buggy = 1` (a due card that never expires)
    /// is the negative control.
    fn bounded_wait_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            MacosAccessCardBoundedWait {
                const Buggy = 0;
                // Due, Watching, Settled, matching current_policy.
                var phase = 2;
                var expired = 0;
                action Raise when (phase == 2 && expired == 0) {
                    phase = 3;
                }
                action Expire when (phase > 1 && phase <= 3 && expired == 0) {
                    expired = 1;
                    phase = if Buggy == 1 && phase == 2 { phase } else { 4 };
                }
                invariant Bounds: phase > 1 && phase <= 4 && expired <= 1;
                invariant NoWorkPastCap: expired == 0 || phase == 4;
            }
        }
    }

    #[test]
    fn bounded_wait_model_proves_and_catches_a_never_shown_card() {
        let model = bounded_wait_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    /// Drives the real due and watching states to just before and exactly at
    /// their scheduled expiry; expiry settles without further pending
    /// observations.
    #[test]
    fn bounded_wait_conforms_at_each_real_phase_deadline() {
        let now = Instant::now();
        let model = bounded_wait_model();
        let scenarios: &[&[&str]] = &[&[], &["Raise"]];
        let mut negative_controls = 0;
        for actions in scenarios {
            let mut actual = CardState::new();
            assert_eq!(actual.lifecycle_deadline(), None);
            assert!(actual.begin_deciding(now));
            assert_eq!(actual.lifecycle_deadline(), Some(now + DECIDE_TIMEOUT));
            assert!(actual.on_decided(&Verdict::Offer, now));
            assert_eq!(
                actual.lifecycle_deadline(),
                Some(now + WATCH_FOR),
                "a due card waits on a pending probe with the watch's patience"
            );
            let mut expected = model.init_state();
            for (step, action) in actions.iter().enumerate() {
                let at = now + Duration::from_secs(step as u64 + 1);
                match *action {
                    "Raise" => actual.on_raised(at),
                    _ => unreachable!(),
                }
                assert!(model.fire(action, &mut expected));
                let phase = match actual.phase() {
                    CardPhase::Due => 2,
                    CardPhase::Watching { .. } => 3,
                    _ => panic!("unexpected waiting phase"),
                };
                assert_eq!(phase, expected["phase"]);
            }
            let deadline = actual.lifecycle_deadline().expect("every wait has a cap");
            assert!(!actual.expire_wait(deadline - Duration::from_nanos(1)));
            assert_eq!(actual.lifecycle_deadline(), Some(deadline));
            let before = expected.clone();
            assert!(actual.expire_wait(deadline));
            assert!(model.fire("Expire", &mut expected));
            assert_eq!(actual.phase(), CardPhase::Settled);
            assert_eq!(expected["phase"], 4);
            assert!(model.check_invariant("NoWorkPastCap", &expected));
            assert_eq!(
                actual.lifecycle_deadline(),
                None,
                "a settled card owns no deadline: there is no daily return any more"
            );
            assert!(!actual.raise_allowed(deadline));
            assert!(!actual.wants_probe(deadline));
            if before["phase"] == 2 {
                let mut old = expected.clone();
                old.insert("phase", 2);
                assert!(!model.check_invariant("NoWorkPastCap", &old));
                negative_controls += 1;
            }
        }
        assert_eq!(negative_controls, 1, "the first-offer omission");
    }

    /// The live settings guard over the lifecycle phases: disabling a visible
    /// or queued card retires it for good. `Buggy = 1` guards only at launch.
    fn current_policy_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            MacosAccessCardCurrentPolicy {
                const Buggy = 0;
                var enabled = 1;
                var notice = 1;
                // Idle, Deciding, Due, Watching, Settled.
                var phase = 0;
                action Begin when (phase == 0 && enabled == 1 && notice == 1) {
                    phase = 1;
                }
                action Decide when (phase == 1 && enabled == 1 && notice == 1) {
                    phase = 2;
                }
                action Raise when (phase == 2 && enabled == 1 && notice == 1) {
                    phase = 3;
                }
                action Finish when (phase == 3) {
                    phase = 4;
                }
                action DisableMaster when (enabled == 1) {
                    enabled = 0;
                    phase = if Buggy == 1 && phase > 0 { phase } else { 4 };
                }
                action DisableNotice when (notice == 1) {
                    notice = 0;
                    phase = if Buggy == 1 && phase > 0 { phase } else { 4 };
                }
                action EnableBoth when (enabled == 0 || notice == 0) {
                    enabled = 1;
                    notice = 1;
                }
                // A NEED re-arms a settled card (`CardState::on_need`). It is
                // taken only while both switches are on; `Buggy = 1` is a
                // re-arm that ignores them, which breaks `DisabledSettles`.
                action Rearm when (phase == 4 && (Buggy == 1 || (enabled == 1 && notice == 1))) {
                    phase = 1;
                }
                invariant Bounds: enabled <= 1 && notice <= 1 && phase <= 4;
                invariant DisabledSettles: enabled == 1 && notice == 1 || phase == 4;
            }
        }
    }

    #[test]
    fn current_policy_model_proves_and_catches_a_stale_card() {
        let model = current_policy_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    /// Tier-1: drive all five phases, then disable either or both settings; a
    /// late verdict or re-enabling cannot resurrect a settled card. The
    /// launch-only guard is projected as the negative control.
    #[test]
    fn current_policy_conforms_in_every_real_phase() {
        fn phase_value(state: CardState) -> i64 {
            match state.phase() {
                CardPhase::Idle => 0,
                CardPhase::Deciding { .. } => 1,
                CardPhase::Due => 2,
                CardPhase::Watching { .. } => 3,
                CardPhase::Settled => 4,
            }
        }

        let now = Instant::now();
        let model = current_policy_model();
        let mut actual = CardState::new();
        let mut expected = model.init_state();
        let mut phases = vec![(actual, expected.clone())];
        for action in ["Begin", "Decide", "Raise", "Finish"] {
            match action {
                "Begin" => assert!(actual.begin_deciding(now)),
                "Decide" => assert!(actual.on_decided(&Verdict::Offer, now)),
                "Raise" => actual.on_raised(now),
                "Finish" => actual.settle(),
                _ => unreachable!(),
            }
            assert!(model.fire(action, &mut expected));
            assert_eq!(phase_value(actual), expected["phase"]);
            phases.push((actual, expected.clone()));
        }
        let mut caught_old_guards = 0;
        for (before, projection) in phases {
            for (enabled, notice) in [(true, true), (false, true), (true, false), (false, false)] {
                let mut actual = before;
                let mut expected = projection.clone();
                let admitted = actual.admit_current_policy(enabled, notice);
                assert_eq!(admitted, enabled && notice);
                if !enabled {
                    assert!(model.fire("DisableMaster", &mut expected));
                }
                if !notice {
                    assert!(model.fire("DisableNotice", &mut expected));
                }
                assert_eq!(phase_value(actual), expected["phase"]);
                assert!(model.check_invariant("DisabledSettles", &expected));
                if admitted {
                    assert_eq!(actual, before, "an enabled policy preserves every phase");
                } else {
                    let mut old = expected.clone();
                    old.insert("phase", phase_value(before));
                    if !matches!(before.phase(), CardPhase::Idle | CardPhase::Settled) {
                        assert!(!model.check_invariant("DisabledSettles", &old));
                        caught_old_guards += 1;
                    }
                    assert!(!actual.on_decided(&Verdict::Offer, now));
                    assert!(!actual.on_decided(&Verdict::Confirm, now));
                    assert!(actual.admit_current_policy(true, true));
                    assert!(model.fire("EnableBoth", &mut expected));
                    assert_eq!(phase_value(actual), expected["phase"]);
                    assert_eq!(actual.phase(), CardPhase::Settled);
                }
            }
        }
        assert_eq!(
            caught_old_guards, 9,
            "three active phases, three disabled policies"
        );

        // A NEED re-arms a settled card only under both switches; the host
        // admits the current policy first, so a disabled card is never armed.
        for (enabled, notice) in [(true, true), (false, true), (true, false), (false, false)] {
            let mut actual = CardState::new();
            actual.settle();
            let mut expected = model.init_state();
            for action in ["Begin", "Decide", "Raise", "Finish"] {
                assert!(model.fire(action, &mut expected));
            }
            if !enabled {
                assert!(model.fire("DisableMaster", &mut expected));
            }
            if !notice {
                assert!(model.fire("DisableNotice", &mut expected));
            }
            assert_eq!(expected["phase"], 4);
            let admitted = actual.admit_current_policy(enabled, notice);
            assert_eq!(admitted, model.action_enabled("Rearm", &expected));
            if admitted {
                assert!(actual.on_need(now));
                assert!(model.fire("Rearm", &mut expected));
                assert_eq!(phase_value(actual), expected["phase"]);
            } else {
                assert_eq!(actual.phase(), CardPhase::Settled);
                // The old omitted guard: re-arming regardless.
                let mut buggy = expected.clone();
                buggy.insert("phase", 1);
                assert!(!model.check_invariant("DisabledSettles", &buggy));
            }
        }
    }

    fn need_facts(cause: NeedCause) -> NeedFacts {
        NeedFacts {
            card: denied_release(),
            now_secs: 10_000_000,
            process_started_at: 9_999_000,
            cause,
        }
    }

    fn app_data_prompt() -> NeedCause {
        NeedCause::PromptObserved {
            service: Some(Folder::AppData),
            session: Some(1),
        }
    }

    /// A VERIFIED GRANT THAT READS OFF IN A FRESH PROCESS DOES NOT CONCLUDE.
    /// An update's first seconds sit inside the `.rollback` window, where a
    /// held grant can read off; the need is neither asked nor marked
    /// regressed, and after `LAUNCH_SETTLE` the same facts do conclude.
    #[test]
    fn a_verified_grant_reading_off_in_a_fresh_process_is_settling_not_regressed() {
        let verified = AccessRecord {
            verified_at: Some(9_000_000),
            ..AccessRecord::default()
        };
        let fresh = NeedFacts {
            process_started_at: 10_000_000 - 5,
            ..need_facts(app_data_prompt())
        };
        assert_eq!(
            decide_need(&fresh, None, &verified),
            NeedVerdict::Quiet(NotDue::Settling)
        );
        let settled = NeedFacts {
            process_started_at: 10_000_000 - LAUNCH_SETTLE.as_secs(),
            ..fresh.clone()
        };
        assert_eq!(
            decide_need(&settled, None, &verified),
            NeedVerdict::Ask { regressed: true },
            "the second denied read marks the grant regressed"
        );
        // Never verified: nothing to doubt, so a fresh process asks at once.
        assert_eq!(
            decide_need(&fresh, None, &AccessRecord::default()),
            NeedVerdict::Ask { regressed: false }
        );
    }

    /// A PROMPT WHILE THE PROBE READS GRANTED ASKS NOTHING and is the one case
    /// the log keeps a trace of; aterm's own refused read is not that anomaly.
    #[test]
    fn a_prompt_while_the_probe_reads_granted_logs_an_anomaly_and_asks_nothing() {
        let granted = NeedFacts {
            card: CardFacts {
                fda: FdaState::Granted,
                ..denied_release()
            },
            ..need_facts(app_data_prompt())
        };
        let verdict = decide_need(&granted, None, &AccessRecord::default());
        assert_eq!(verdict, NeedVerdict::Quiet(NotDue::Granted));
        assert!(is_prompt_while_granted(app_data_prompt(), &verdict));
        assert!(!is_prompt_while_granted(
            NeedCause::ProtectedRead { session: None },
            &verdict
        ));
        assert!(!is_prompt_while_granted(
            app_data_prompt(),
            &NeedVerdict::Ask { regressed: false }
        ));
    }

    /// `state=opened` lapses with the rule it reports: past `OWNER_ACTED_HOLD`
    /// the card asks again, so the token must not say nothing more is coming.
    #[test]
    fn the_opened_state_lapses_with_the_owner_acted_hold() {
        let record = AccessRecord {
            opened_at: Some(1_000_000),
            ..AccessRecord::default()
        };
        let hold = OWNER_ACTED_HOLD.as_secs();
        assert_eq!(
            report_state(None, &record, FdaState::Denied, 1_000_000 + hold - 1),
            "opened"
        );
        assert_eq!(
            report_state(None, &record, FdaState::Denied, 1_000_000 + hold),
            "quiet"
        );
    }

    /// Every rule is a way to be wrong, and each refusal is named, in the order
    /// a reader would want explained: the switch, the instance, the class of
    /// prompt, the answer, the identity, the grant, the owner who acted, the
    /// probe, the epoch. An `opened` marker is NOT an answer.
    #[test]
    fn decide_need_is_a_table_of_named_refusals_then_an_ask() {
        let none = AccessRecord::default();
        let facts = need_facts(app_data_prompt());
        assert_eq!(
            decide_need(&facts, None, &none),
            NeedVerdict::Ask { regressed: false }
        );
        assert_eq!(
            decide_need(&facts, Some(Marker::Opened), &none),
            NeedVerdict::Ask { regressed: false },
            "an opened marker is not an answer"
        );
        let off = NeedFacts {
            card: CardFacts {
                offered: false,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&off, None, &none),
            NeedVerdict::Quiet(NotDue::Off)
        );
        let headless = NeedFacts {
            card: CardFacts {
                headless: true,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&headless, None, &none),
            NeedVerdict::Quiet(NotDue::Headless)
        );
        // A prompt for any other service is a log line only; an unknown one too.
        for service in [Some(Folder::Documents), Some(Folder::Desktop), None] {
            let other = need_facts(NeedCause::PromptObserved {
                service,
                session: None,
            });
            assert_eq!(
                decide_need(&other, None, &none),
                NeedVerdict::Quiet(NotDue::NotThisClass),
                "{service:?}"
            );
        }
        // aterm's own refused read is a need of its own kind.
        let eperm = need_facts(NeedCause::ProtectedRead { session: Some(2) });
        assert_eq!(
            decide_need(&eperm, None, &none),
            NeedVerdict::Ask { regressed: false }
        );
        assert_eq!(
            decide_need(&facts, Some(Marker::NotNow), &none),
            NeedVerdict::Quiet(NotDue::Answered)
        );
        for dr in [DrClass::Cdhash, DrClass::Unsigned, DrClass::Unknown] {
            let unstable = NeedFacts {
                card: CardFacts {
                    dr,
                    ..denied_release()
                },
                ..facts.clone()
            };
            assert_eq!(
                decide_need(&unstable, None, &none),
                NeedVerdict::Quiet(NotDue::GrantUnstable(dr)),
                "{dr:?} is not grant-stable"
            );
        }
        let granted = NeedFacts {
            card: CardFacts {
                fda: FdaState::Granted,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&granted, None, &none),
            NeedVerdict::Quiet(NotDue::Granted)
        );
        let not_observed = NeedFacts {
            card: CardFacts {
                fda: FdaState::Unknown,
                ..denied_release()
            },
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&not_observed, None, &none),
            NeedVerdict::Quiet(NotDue::NotObservedDenied),
            "unknown is not denied"
        );
        // The epoch: asked inside the week is refused, the week renews it.
        let asked = AccessRecord {
            asked_at: Some(facts.now_secs - 60),
            ..none
        };
        assert_eq!(
            decide_need(&facts, None, &asked),
            NeedVerdict::Quiet(NotDue::RecentlyAsked)
        );
        let week = RENEW_AFTER.as_secs();
        let inside = NeedFacts {
            now_secs: facts.now_secs - 61 + week,
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&inside, None, &asked),
            NeedVerdict::Quiet(NotDue::RecentlyAsked),
            "one second short of asked_at + the week is still inside it"
        );
        let renewed = NeedFacts {
            now_secs: facts.now_secs - 60 + week,
            ..facts.clone()
        };
        assert_eq!(
            decide_need(&renewed, None, &asked),
            NeedVerdict::Ask { regressed: false },
            "the renewal boundary is exact"
        );
        // Every reason renders as one sentence a log line can carry.
        for reason in [
            NotDue::Off,
            NotDue::Headless,
            NotDue::Answered,
            NotDue::Granted,
            NotDue::NotObservedDenied,
            NotDue::GrantUnstable(DrClass::Cdhash),
            NotDue::NotThisClass,
            NotDue::OwnerActed,
            NotDue::RecentlyAsked,
            NotDue::NoNeedYet,
            NotDue::Held,
            NotDue::ReadsOffAfterVerified,
        ] {
            let line = reason.to_string();
            assert!(!line.is_empty() && !line.contains('\n'), "{line:?}");
        }
    }

    /// A grant verified earlier and read as off now opens a NEW epoch: the ask
    /// is allowed at once (the epoch it belonged to is over) and its words say
    /// so, and once asked it is `RecentlyAsked` again.
    #[test]
    fn an_ask_after_a_verified_grant_reads_regressed_and_opens_a_new_epoch() {
        let facts = need_facts(app_data_prompt());
        let t = facts.now_secs;
        let record = AccessRecord {
            asked_at: Some(t - 3600),
            verified_at: Some(t - 1800),
            verified_build: Some(7),
            ..AccessRecord::default()
        };
        assert_eq!(
            decide_need(&facts, None, &record),
            NeedVerdict::Ask { regressed: true },
            "an ask made before the verification does not bar the new epoch"
        );
        let asked_again = AccessRecord {
            asked_at: Some(t - 10),
            ..record
        };
        assert_eq!(
            decide_need(&facts, None, &asked_again),
            NeedVerdict::Quiet(NotDue::RecentlyAsked)
        );
    }

    /// The launch check NEVER asks: it confirms a grant, or it says why not.
    #[test]
    fn decide_launch_never_asks_and_confirms_only_an_opened_grant() {
        let none = AccessRecord::default();
        let denied = denied_release();
        assert_eq!(
            decide_launch(&denied, None, &none),
            LaunchVerdict::Quiet(NotDue::NoNeedYet),
            "a denied probe alone raises nothing"
        );
        assert_eq!(
            decide_launch(&denied, Some(Marker::Opened), &none),
            LaunchVerdict::Quiet(NotDue::NoNeedYet),
            "opened-but-still-denied is not an answer, and not a question either"
        );
        let off = CardFacts {
            offered: false,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&off, None, &none),
            LaunchVerdict::Quiet(NotDue::Off)
        );
        let headless = CardFacts {
            headless: true,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&headless, None, &none),
            LaunchVerdict::Quiet(NotDue::Headless)
        );
        assert_eq!(
            decide_launch(&denied, Some(Marker::NotNow), &none),
            LaunchVerdict::Quiet(NotDue::Answered)
        );
        let granted = CardFacts {
            fda: FdaState::Granted,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&granted, None, &none),
            LaunchVerdict::Quiet(NotDue::Granted)
        );
        assert_eq!(
            decide_launch(&granted, Some(Marker::Opened), &none),
            LaunchVerdict::Confirm,
            "the flip the owner started in an earlier process is acknowledged once"
        );
        let asked = AccessRecord {
            asked_at: Some(5),
            ..none
        };
        assert_eq!(
            decide_launch(&granted, None, &asked),
            LaunchVerdict::Confirm,
            "a grant after an ask is acknowledged whichever gesture made it"
        );
        let verified = AccessRecord {
            asked_at: Some(5),
            verified_at: Some(9),
            ..none
        };
        assert_eq!(
            decide_launch(&granted, None, &verified),
            LaunchVerdict::Quiet(NotDue::Held),
            "already verified: nothing to say again"
        );
        assert_eq!(
            decide_launch(&granted, Some(Marker::NotNow), &none),
            LaunchVerdict::Quiet(NotDue::Answered)
        );
        assert_eq!(
            decide_launch(&denied, None, &verified),
            LaunchVerdict::Quiet(NotDue::ReadsOffAfterVerified),
            "quiet and LOGGED; the next ask's words say it regressed"
        );
        let unknown = CardFacts {
            fda: FdaState::Unknown,
            ..denied_release()
        };
        assert_eq!(
            decide_launch(&unknown, None, &none),
            LaunchVerdict::Quiet(NotDue::NoNeedYet)
        );
        // Re-assertion (the digest host): asked, not verified after, and the
        // owner neither declined nor acted.
        assert!(should_reassert(&asked, None, FdaState::Denied));
        assert!(!should_reassert(
            &asked,
            Some(Marker::Opened),
            FdaState::Denied
        ));
        assert!(!should_reassert(
            &asked,
            Some(Marker::NotNow),
            FdaState::Denied
        ));
        assert!(!should_reassert(&asked, None, FdaState::Granted));
        assert!(!should_reassert(&none, None, FdaState::Denied));
        assert!(!should_reassert(&verified, None, FdaState::Denied));
    }

    /// THE OWNER WHO ACTED (the reviewer's scenario): `Open Settings`, the
    /// switch on, `Later` on Apple's sheet, a window that still reads
    /// `denied`, the renewal elapsed, a program of that older window prompts.
    /// Nothing is asked. A window that started AFTER the press, and the
    /// lapsing of the hold, allow the ask again.
    #[test]
    fn an_owner_who_opened_settings_is_not_asked_again_by_an_older_window() {
        let t = 10_000_000_u64;
        let record = AccessRecord {
            asked_at: Some(t - 100),
            opened_at: Some(t - 50),
            ..AccessRecord::default()
        };
        let older_window = NeedFacts {
            now_secs: t + RENEW_AFTER.as_secs() + 1,
            process_started_at: t - 5_000,
            ..need_facts(app_data_prompt())
        };
        assert_eq!(
            decide_need(&older_window, None, &record),
            NeedVerdict::Quiet(NotDue::OwnerActed),
            "the week elapsed and the probe reads denied, but this window predates the press"
        );
        // The hold lapses after fourteen days.
        let lapsed = NeedFacts {
            now_secs: t - 50 + OWNER_ACTED_HOLD.as_secs(),
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&lapsed, None, &record),
            NeedVerdict::Ask { regressed: false },
            "the hold lapses after fourteen days and the week has long elapsed"
        );
        let just_inside = NeedFacts {
            now_secs: t - 51 + OWNER_ACTED_HOLD.as_secs(),
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&just_inside, None, &record),
            NeedVerdict::Quiet(NotDue::OwnerActed)
        );
        // A window that started after the press and still reads denied is
        // evidence the switch is off: the weekly renewal decides.
        let newer_window = NeedFacts {
            process_started_at: t,
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&newer_window, None, &record),
            NeedVerdict::Ask { regressed: false }
        );
        let newer_soon = NeedFacts {
            now_secs: t + 60,
            process_started_at: t,
            ..older_window.clone()
        };
        assert_eq!(
            decide_need(&newer_soon, None, &record),
            NeedVerdict::Quiet(NotDue::RecentlyAsked)
        );
        // A verification newer than the press ends the owner's act.
        let verified = AccessRecord {
            verified_at: Some(t),
            ..record
        };
        assert_eq!(
            decide_need(&older_window, None, &verified),
            NeedVerdict::Ask { regressed: true }
        );
    }

    /// The marker: path beside `aterm.toml`, one token, round-trips through
    /// the shared marker rules, tolerates a hand edit's trailing newline, and
    /// `clear_opened` removes ONLY an `opened` marker.
    #[test]
    fn the_marker_round_trips_beside_the_config() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-consent-card-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().unwrap().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("aterm.toml");
        assert_eq!(marker_path(Some(&config)), Some(dir.join(MARKER)));
        assert_eq!(marker_path(None), None);
        assert_eq!(read_marker(Some(&config)), None);
        assert!(record_marker(None, Marker::NotNow).is_err());
        assert!(
            clear_opened(None).is_ok(),
            "nothing to clear is not an error"
        );

        record_marker(Some(&config), Marker::Opened).unwrap();
        assert_eq!(read_marker(Some(&config)), Some(Marker::Opened));
        clear_opened(Some(&config)).unwrap();
        assert_eq!(read_marker(Some(&config)), None, "opened is consumed");
        assert!(!dir.join(MARKER).exists());

        record_marker(Some(&config), Marker::NotNow).unwrap();
        assert_eq!(read_marker(Some(&config)), Some(Marker::NotNow));
        record_marker(Some(&config), Marker::Opened).unwrap();
        assert_eq!(
            read_marker(Some(&config)),
            Some(Marker::NotNow),
            "opening Settings is not an answer: it never replaces the owner's not-now"
        );
        clear_opened(Some(&config)).unwrap();
        assert_eq!(
            read_marker(Some(&config)),
            Some(Marker::NotNow),
            "an answer is the owner's and is never cleared"
        );
        for marker in [Marker::NotNow, Marker::Opened] {
            assert!(marker.as_str().len() <= MAX_MARKER_BYTES);
            assert_eq!(Marker::parse(marker.as_str()), Some(marker));
        }
        // A hand edit with a trailing newline still reads as the answer.
        std::fs::write(dir.join(MARKER), "not-now\n").unwrap();
        assert_eq!(read_marker(Some(&config)), Some(Marker::NotNow));
        // Something else wearing the name is not an answer: the card shows.
        std::fs::write(dir.join(MARKER), "x".repeat(MAX_MARKER_BYTES + 1)).unwrap();
        assert_eq!(read_marker(Some(&config)), None);
        std::fs::write(dir.join(MARKER), "granted").unwrap();
        assert_eq!(
            read_marker(Some(&config)),
            None,
            "an unknown token is no record"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The card's answer, the access record and the verified-host line are only
    /// ordered ahead of their renames (aterm-durable's counting seam): each one lost
    /// brings its question back, or is merged again, and nothing off this device
    /// waits on it.
    #[test]
    fn the_cards_records_are_only_ordered() {
        use aterm_durable::{Flush, seam::Recording};
        let dir = std::env::temp_dir().join(format!(
            "aterm-consent-card-flush-{}-{}",
            std::process::id(),
            aterm_time::since_epoch().unwrap().as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let config = dir.join("aterm.toml");
        let recording = Recording::start();
        record_marker(Some(&config), Marker::NotNow).unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "the answer");
        let update = AccessRecord {
            verified_at: Some(7),
            ..AccessRecord::default()
        };
        write_record(Some(&config), update).unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "the access record");
        write_verified_host(Some(&config), "HOST-UUID", "com.aterm.aterm").unwrap();
        assert_eq!(recording.take(), [Flush::Ordered], "the verified host");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The event line names the cause, and the tab only when it is known.
    #[test]
    fn the_entry_line_names_the_cause_and_the_tab_it_knows() {
        let tab = NeedWords {
            cause: app_data_prompt(),
            tab_number: Some(1),
            regressed: false,
        };
        let no_tab = NeedWords {
            tab_number: None,
            ..tab
        };
        assert!(tab.event_line().contains("tab 1"));
        assert!(!no_tab.event_line().contains("tab"), "never guessed");
        let regressed = NeedWords {
            regressed: true,
            ..tab
        };
        let eperm = NeedWords {
            cause: NeedCause::ProtectedRead { session: Some(2) },
            tab_number: Some(2),
            regressed: false,
        };
        // Each cause is named: a regression and a refused read do not read as a prompt.
        assert_ne!(regressed.event_line(), tab.event_line());
        assert_ne!(eperm.event_line(), tab.event_line());
        assert!(eperm.event_line().contains("tab 2"));
    }

    /// THE ASKED-ONCE MODEL (replaces the daily-return `AccessCardReoffer`). One
    /// question per EPOCH, however many process generations an update brings;
    /// the only legitimate resets are `Regress` (a verified grant reads off), a
    /// `Renew` (a week) and the lapsing of the owner's hold, and `Update` is
    /// none of them. `opened`/`fresh` say an owner who acted on an older window
    /// is not asked again. `Buggy = 1` reproduces two histories: the 8-posts-in-
    /// 3.8-days shape (`Need, Ask, Update, Need, Ask`: a new process forgot it
    /// had asked) AND the reviewer's owner (`Need, Ask, Open, Renew, Need, Ask`:
    /// an ask to an owner who followed the instruction and whose window is
    /// older than the press). Time is abstracted: `Renew` is `RENEW_AFTER`
    /// elapsing and `HoldLapses` is `OWNER_ACTED_HOLD` elapsing, which is
    /// longer, so it renews the ask as well.
    fn asked_once_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AccessCardAskedOnce {
                const Buggy = 0;
                const Cap = 3;
                var gen = 0;
                var asked = 0;
                var raised = 0;
                var granted = 0;
                var need = 0;
                var opened = 0;
                var fresh = 1;
                var acted_ask = 0;
                action Need when (need == 0) { need = 1; }
                action Ask when (need == 1 && granted == 0 && asked == 0 && (opened == 0 || fresh == 1 || Buggy == 1)) {
                    asked = 1; raised = raised + 1; need = 0;
                    acted_ask = if opened == 1 && fresh == 0 { 1 } else { acted_ask };
                }
                action Suppress when (need == 1 && (asked == 1 || granted == 1 || (opened == 1 && fresh == 0 && Buggy == 0))) { need = 0; }
                action Open when (asked == 1 && opened == 0 && granted == 0) { opened = 1; fresh = 0; }
                action HoldLapses when (opened == 1 && fresh == 0) { opened = 0; fresh = 1; asked = 0; raised = 0; }
                action Grant when (granted == 0) { granted = 1; }
                action Regress when (granted == 1) { granted = 0; asked = 0; raised = 0; opened = 0; fresh = 1; }
                action Renew when (asked == 1 && granted == 0) { asked = 0; raised = 0; }
                action Update when (gen <= Cap - 1) {
                    gen = gen + 1; need = 0; fresh = 1;
                    asked = if Buggy == 1 { 0 } else { asked };
                }
                invariant AtMostOncePerEpoch: raised <= 1;
                invariant NoAskToAnOwnerWhoActedOnAnOlderWindow: acted_ask == 0;
            }
        }
    }

    #[test]
    fn asked_once_model_proves_and_catches_the_historical_shapes() {
        let model = asked_once_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    /// The real rules, driven through simulated restarts. The record is carried
    /// ON DISK between generations (a new `CardState` and a new process start
    /// each time, the record file the only memory), the model's actions are
    /// fired in lockstep, and its variables are projected from the real state.
    struct Epochs {
        dir: std::path::PathBuf,
        config: std::path::PathBuf,
        fda: FdaState,
        now: u64,
        started: u64,
        generation: i64,
        raised: i64,
        need: bool,
        acted_ask: bool,
        /// The historical defect: a process that keeps no record.
        forgets_record: bool,
        /// The reviewer's defect: an ask that ignores the window's age.
        ignores_window_age: bool,
        /// A negative control: the run is EXPECTED to depart from the model,
        /// so its guards are not asserted (its invariants are the check).
        lenient: bool,
    }

    impl Epochs {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "aterm-asked-once-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let config = dir.join("aterm.toml");
            Self {
                dir,
                config,
                fda: FdaState::Denied,
                now: 10_000_000,
                started: 10_000_000,
                generation: 0,
                raised: 0,
                need: false,
                acted_ask: false,
                forgets_record: false,
                ignores_window_age: false,
                lenient: false,
            }
        }

        fn record(&self) -> AccessRecord {
            read_record(Some(&self.config))
        }

        fn facts(&self) -> NeedFacts {
            NeedFacts {
                card: CardFacts {
                    fda: self.fda,
                    ..denied_release()
                },
                now_secs: self.now,
                process_started_at: if self.ignores_window_age {
                    u64::MAX
                } else {
                    self.started
                },
                cause: app_data_prompt(),
            }
        }

        fn verdict(&self) -> NeedVerdict {
            decide_need(&self.facts(), None, &self.record())
        }

        fn write(&self, update: AccessRecord) {
            write_record(Some(&self.config), update).unwrap();
        }

        fn project(&self) -> std::collections::BTreeMap<&'static str, i64> {
            let record = self.record();
            let granted = self.fda == FdaState::Granted;
            let live = |at: Option<u64>, span: Duration| {
                at.is_some_and(|at| {
                    self.now < at + span.as_secs()
                        && (granted || record.verified_at.is_none_or(|v| v <= at))
                })
            };
            let asked = live(record.asked_at, RENEW_AFTER);
            let opened = live(record.opened_at, OWNER_ACTED_HOLD);
            let fresh = !opened || self.started >= record.opened_at.unwrap_or(0);
            std::collections::BTreeMap::from([
                ("gen", self.generation),
                ("asked", i64::from(asked)),
                ("raised", self.raised),
                ("granted", i64::from(granted)),
                ("need", i64::from(self.need)),
                ("opened", i64::from(opened)),
                ("fresh", i64::from(fresh)),
                ("acted_ask", i64::from(self.acted_ask)),
            ])
        }

        /// Do the real thing the model action stands for; `Err` if a decision
        /// disagrees with the model's guard.
        fn apply(
            &mut self,
            model: &aterm_spec::derive::Model,
            action: &str,
            before: &std::collections::BTreeMap<&'static str, i64>,
        ) -> Result<(), String> {
            self.now += 1;
            match action {
                "Need" => self.need = true,
                "Ask" | "Suppress" => {
                    // The host keeps a need that arrives inside the new
                    // process's `LAUNCH_SETTLE` and decides it once the
                    // window has settled (`NotDue::Settling`): the model's
                    // decision moment is that later one.
                    self.now = self.now.max(self.started + LAUNCH_SETTLE.as_secs());
                    let verdict = self.verdict();
                    let asks = matches!(verdict, NeedVerdict::Ask { .. });
                    if asks != (action == "Ask") {
                        return Err(format!("the real decision {verdict:?} is not {action}"));
                    }
                    let other = if action == "Ask" { "Suppress" } else { "Ask" };
                    if !self.lenient && model.action_enabled(other, before) {
                        return Err(format!("the model allows {other} too"));
                    }
                    self.need = false;
                    if asks {
                        self.acted_ask =
                            self.acted_ask || (before["opened"] == 1 && before["fresh"] == 0);
                        self.raised += 1;
                        self.write(AccessRecord {
                            asked_at: Some(self.now),
                            ..AccessRecord::default()
                        });
                    }
                }
                "Open" => self.write(AccessRecord {
                    opened_at: Some(self.now),
                    ..AccessRecord::default()
                }),
                "HoldLapses" => {
                    self.now += OWNER_ACTED_HOLD.as_secs() + 1;
                    self.raised = 0;
                }
                "Grant" => {
                    self.fda = FdaState::Granted;
                    self.write(AccessRecord {
                        verified_at: Some(self.now),
                        verified_build: Some(7),
                        ..AccessRecord::default()
                    });
                }
                "Regress" => {
                    self.fda = FdaState::Denied;
                    self.raised = 0;
                }
                "Renew" => {
                    self.now += RENEW_AFTER.as_secs() + 1;
                    self.raised = 0;
                }
                "Update" => {
                    self.generation += 1;
                    self.need = false;
                    self.started = self.now;
                    if self.forgets_record {
                        let _ = std::fs::remove_file(record_path(Some(&self.config)).unwrap());
                    }
                }
                other => return Err(format!("unknown action {other}")),
            }
            Ok(())
        }
    }

    impl Drop for Epochs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Run a scenario in lockstep; returns the last projected state.
    fn run_epochs(
        epochs: &mut Epochs,
        actions: &[&str],
    ) -> std::collections::BTreeMap<&'static str, i64> {
        let model = asked_once_model();
        let mut expected = model.init_state();
        assert_eq!(epochs.project(), expected, "both start in the same state");
        for action in actions {
            let before = expected.clone();
            assert!(
                model.action_enabled(action, &before),
                "{action}: the model forbids it in {before:?}"
            );
            epochs
                .apply(&model, action, &before)
                .unwrap_or_else(|why| panic!("{action}: {why}"));
            assert!(model.fire(action, &mut expected));
            assert_eq!(
                epochs.project(),
                expected,
                "{action}: the real state departs from the model"
            );
        }
        expected
    }

    /// TIER 1: the real `decide_need` and record persistence across process
    /// generations follow the model, action by action, and the model's
    /// invariants hold in every state reached.
    #[test]
    fn asked_once_conforms_across_process_generations() {
        let model = asked_once_model();
        for (tag, actions) in [
            (
                "eight-updates",
                &[
                    "Need", "Ask", "Update", "Need", "Suppress", "Update", "Need", "Suppress",
                ][..],
            ),
            (
                "opened-then-updated",
                &["Need", "Ask", "Open", "Update", "Need", "Suppress"][..],
            ),
            (
                "owner-acted",
                &["Need", "Ask", "Open", "Renew", "Need", "Suppress"][..],
            ),
            (
                "hold-lapses",
                &["Need", "Ask", "Open", "HoldLapses", "Need", "Ask"][..],
            ),
            (
                "grant-and-regress",
                &[
                    "Need", "Ask", "Grant", "Need", "Suppress", "Regress", "Need", "Ask",
                ][..],
            ),
            (
                "renews-weekly",
                &["Need", "Ask", "Renew", "Need", "Ask"][..],
            ),
            (
                "granted-first",
                &["Grant", "Update", "Regress", "Need", "Ask"][..],
            ),
        ] {
            let mut epochs = Epochs::new(tag);
            let end = run_epochs(&mut epochs, actions);
            assert!(model.check_invariant("AtMostOncePerEpoch", &end), "{tag}");
            assert!(
                model.check_invariant("NoAskToAnOwnerWhoActedOnAnOlderWindow", &end),
                "{tag}"
            );
        }
    }

    /// NEGATIVE CONTROLS: the two defects the model exists for, replayed on
    /// the real rules, are caught by its invariants.
    #[test]
    fn asked_once_catches_the_process_that_forgets_and_the_ask_that_ignores_the_window() {
        let model = asked_once_model();
        // 1. Per-process behaviour: a new process keeps no record (the 8 posts
        //    in 3.8 days). The real rules ask a second time in one epoch.
        let mut forgetful = Epochs::new("forgets");
        forgetful.forgets_record = true;
        forgetful.lenient = true;
        let model_free = |epochs: &mut Epochs, action: &str| {
            let before = epochs.project();
            epochs.apply(&model, action, &before).unwrap();
        };
        for action in ["Need", "Ask", "Update", "Need", "Ask"] {
            model_free(&mut forgetful, action);
        }
        assert_eq!(forgetful.raised, 2, "the per-process card asks twice");
        assert!(
            !model.check_invariant("AtMostOncePerEpoch", &forgetful.project()),
            "the invariant reports it"
        );
        // 2. The reviewer's owner: an ask that ignores how old the window is.
        let mut blind = Epochs::new("blind");
        blind.ignores_window_age = true;
        blind.lenient = true;
        for action in ["Need", "Ask", "Open", "Renew", "Need", "Ask"] {
            model_free(&mut blind, action);
        }
        assert!(
            blind.acted_ask,
            "asked an owner who acted on an older window"
        );
        assert!(
            !model.check_invariant("NoAskToAnOwnerWhoActedOnAnOlderWindow", &blind.project()),
            "the invariant reports it"
        );
    }

    /// The record is four `key=value` lines, written as a field-wise max (two
    /// processes cannot move a field backwards), and a foreign file is NO
    /// record.
    #[test]
    fn the_access_record_round_trips_and_a_foreign_file_is_no_record() {
        let epochs = Epochs::new("record");
        let config = Some(epochs.config.as_path());
        assert_eq!(read_record(config), AccessRecord::default());
        assert!(write_record(None, AccessRecord::default()).is_err());
        let first = AccessRecord {
            asked_at: Some(100),
            opened_at: Some(200),
            verified_at: None,
            verified_build: None,
        };
        assert_eq!(write_record(config, first).unwrap(), first);
        assert_eq!(read_record(config), first);
        // An older value never replaces a newer one; a new field is added.
        let merged = write_record(
            config,
            AccessRecord {
                asked_at: Some(50),
                opened_at: Some(300),
                verified_at: Some(400),
                verified_build: Some(9),
            },
        )
        .unwrap();
        assert_eq!(
            merged,
            AccessRecord {
                asked_at: Some(100),
                opened_at: Some(300),
                verified_at: Some(400),
                verified_build: Some(9),
            },
            "opened_at is written as a max"
        );
        assert_eq!(AccessRecord::parse(&merged.render()), merged);
        assert!(merged.render().len() <= MAX_RECORD_BYTES);
        let maximal = AccessRecord {
            asked_at: Some(u64::MAX),
            opened_at: Some(u64::MAX),
            verified_at: Some(u64::MAX),
            verified_build: Some(u64::MAX),
        };
        assert!(
            maximal.render().len() <= MAX_RECORD_BYTES,
            "four maximal fields still fit the bound"
        );
        // A foreign file, an unknown key, a bad number, or an oversized file
        // reads as an empty record: the card errs toward speaking once.
        let path = epochs.dir.join(RECORD);
        for foreign in [
            "hello\n",
            "asked_at=abc\n",
            "asked_at=1\nsecret=2\n",
            "asked_at=1\nnot a line\n",
        ] {
            std::fs::write(&path, foreign).unwrap();
            assert_eq!(read_record(config), AccessRecord::default(), "{foreign:?}");
        }
        std::fs::write(&path, "asked_at=1\n".repeat(MAX_RECORD_BYTES)).unwrap();
        assert_eq!(read_record(config), AccessRecord::default(), "oversized");
        // A hand edit with blank lines and spaces still reads.
        std::fs::write(&path, "\nasked_at = 12\n\nverified_at=13\n").unwrap();
        assert_eq!(
            read_record(config),
            AccessRecord {
                asked_at: Some(12),
                verified_at: Some(13),
                ..AccessRecord::default()
            }
        );
    }

    /// `on_need` re-arms a SETTLED card (and only that); a need that arrives
    /// while the card is busy is kept and given up when it settles; the daily
    /// return is gone (`REOFFER_AFTER`, `reoffer_due` and their deadline).
    #[test]
    fn on_need_re_arms_a_settled_card_and_reoffer_no_longer_exists() {
        let t0 = Instant::now();
        let mut s = CardState::new();
        // Idle: the launch check has not run; the need waits for it.
        assert!(!s.on_need(t0));
        assert_eq!(s.phase(), CardPhase::Idle);
        assert!(!s.take_pending_need(), "only a settled card gives it up");
        assert!(s.begin_deciding(t0));
        assert!(!s.on_need(t0), "a verdict is coming: the need is kept");
        assert!(s.on_decided(&Verdict::Quiet(NotDue::NoNeedYet), t0));
        assert_eq!(s.phase(), CardPhase::Settled);
        assert!(s.take_pending_need(), "re-evaluated the moment it settles");
        assert!(!s.take_pending_need(), "and only once");
        // Settled -> Deciding with a fresh episode and a full attempt budget.
        assert!(s.on_need(t0 + Duration::from_secs(1)));
        assert!(matches!(s.phase(), CardPhase::Deciding { .. }));
        assert_eq!(
            s.lifecycle_deadline(),
            Some(t0 + Duration::from_secs(1) + DECIDE_TIMEOUT)
        );
        assert!(s.on_decided(&Verdict::Offer, t0));
        assert_eq!(s.phase(), CardPhase::Due);
        // A Due or Watching card already covers a further need.
        assert!(!s.on_need(t0));
        assert!(s.raise_allowed(t0));
        s.on_raised(t0);
        assert!(!s.on_need(t0));
        assert!(!s.take_pending_need(), "still watching");
        // The row folding unanswered settles it, with NO daily return.
        assert!(s.on_row_retired());
        assert_eq!(s.phase(), CardPhase::Settled);
        assert_eq!(s.lifecycle_deadline(), None);
        assert!(
            s.take_pending_need(),
            "the need kept while watching returns"
        );
        // A retry budget is fresh for every need: three lapses, then settled.
        let mut lapses = CardState::new();
        lapses.settle();
        assert!(lapses.on_need(t0));
        for attempt in 1..=DECIDE_ATTEMPTS {
            assert!(lapses.deciding_lapsed(t0 + DECIDE_TIMEOUT));
            assert_eq!(lapses.reopen_deciding(), attempt < DECIDE_ATTEMPTS);
            if attempt < DECIDE_ATTEMPTS {
                assert!(lapses.begin_deciding(t0));
            }
        }
        assert_eq!(lapses.phase(), CardPhase::Settled);
    }

    /// A verdict that never arrives does not kill the card: an uncommitted
    /// incoming handoff drops every wake but Commit, so Deciding lapses, asks
    /// again, and gives up only after [`DECIDE_ATTEMPTS`].
    #[test]
    fn a_swallowed_verdict_is_asked_for_again_and_then_given_up_on() {
        let t0 = Instant::now();
        let mut s = CardState::new();
        for attempt in 1..=DECIDE_ATTEMPTS {
            assert!(s.begin_deciding(t0), "attempt {attempt} starts");
            assert!(!s.deciding_lapsed(t0), "not lapsed on the same instant");
            assert!(!s.deciding_lapsed(t0 + DECIDE_TIMEOUT - Duration::from_millis(1)));
            assert!(s.deciding_lapsed(t0 + DECIDE_TIMEOUT));
            let again = s.reopen_deciding();
            assert_eq!(
                again,
                attempt < DECIDE_ATTEMPTS,
                "attempt {attempt} of {DECIDE_ATTEMPTS}"
            );
        }
        assert_eq!(s.phase(), CardPhase::Settled, "the attempts are spent");
        assert_eq!(s.lifecycle_deadline(), None);
        assert!(!s.begin_deciding(t0), "and it does not start again");
        assert!(
            !s.on_decided(&Verdict::Offer, t0),
            "a late verdict is stale"
        );
        // A verdict that does arrive still ends the deciding phase.
        let mut ok = CardState::new();
        assert!(ok.begin_deciding(t0));
        assert!(ok.on_decided(&Verdict::Offer, t0));
        assert_eq!(ok.phase(), CardPhase::Due);
    }

    #[test]
    fn the_lifecycle_decides_once_watches_for_a_while_and_settles() {
        let t0 = Instant::now();
        let mut s = CardState::new();
        assert_eq!(s.phase(), CardPhase::Idle);
        assert!(!s.wants_probe(t0), "idle: nothing to watch");
        assert!(!s.on_row_retired(), "idle: no row to lose");
        assert!(s.begin_deciding(Instant::now()));
        assert!(!s.begin_deciding(Instant::now()), "the one-shot fires once");
        assert!(matches!(s.phase(), CardPhase::Deciding { .. }));
        s.on_raised(t0);
        assert!(
            matches!(s.phase(), CardPhase::Deciding { .. }),
            "nothing to raise yet"
        );

        assert!(s.on_decided(&Verdict::Offer, t0));
        assert_eq!(s.phase(), CardPhase::Due);
        assert!(
            !s.on_decided(&Verdict::Offer, t0),
            "a second verdict is stale"
        );
        assert!(!s.wants_probe(t0), "due but not posted: no watch yet");
        assert!(!s.on_row_retired(), "not posted: no row to lose");
        assert!(s.raise_allowed(t0), "the first raise is always allowed");
        s.on_raised(t0);
        assert_eq!(s.phase(), CardPhase::Watching { since: t0 });
        assert!(s.wants_probe(t0 + Duration::from_secs(60)));
        assert!(!s.raise_allowed(t0), "posted once; never twice");

        // A due card whose probe stays pending is retired unraised at the
        // watch's patience — and, never shown, gets no daily return.
        let mut d = CardState::new();
        assert!(d.begin_deciding(Instant::now()));
        assert!(d.on_decided(&Verdict::Offer, t0));
        assert!(d.raise_allowed(t0 + WATCH_FOR - Duration::from_secs(1)));
        assert!(!d.raise_allowed(t0 + WATCH_FOR));
        assert_eq!(d.phase(), CardPhase::Settled);
        assert_eq!(d.lifecycle_deadline(), None);

        // A `Confirm` verdict settles at once: the ✓ row is the caller's to
        // post, nothing waits for a slot.
        let mut c = CardState::new();
        assert!(c.begin_deciding(Instant::now()));
        assert!(c.on_decided(&Verdict::Confirm, t0));
        assert_eq!(c.phase(), CardPhase::Settled);
        assert_eq!(c.lifecycle_deadline(), None);

        // Opening Settings restarts the window and records the press, so the
        // row's fold is no longer an unanswered end.
        let t2 = t0 + Duration::from_secs(20);
        let mut o = CardState::new();
        assert!(o.begin_deciding(Instant::now()));
        assert!(o.on_decided(&Verdict::Offer, t0));
        o.on_raised(t0);
        o.on_opened_settings(t2);
        assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
        assert!(!o.on_row_retired());
        assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
        assert!(o.wants_probe(t2 + WATCH_FOR - Duration::from_secs(1)));
        assert!(!o.wants_probe(t2 + WATCH_FOR), "the watch is bounded");
        assert_eq!(o.phase(), CardPhase::Settled);
        o.on_opened_settings(t2);
        assert_eq!(
            o.phase(),
            CardPhase::Watching { since: t2 },
            "a new Settings gesture starts a fresh bounded watch"
        );
        assert!(!o.wants_probe(t2 + WATCH_FOR));
        assert_eq!(
            o.lifecycle_deadline(),
            None,
            "an explicit gesture never reoffers the card"
        );

        // Details: read, watch continues, never re-offered — the row's fold
        // is not an unanswered end.
        let mut b = CardState::new();
        assert!(b.begin_deciding(Instant::now()));
        assert!(b.on_decided(&Verdict::Offer, t0));
        b.on_raised(t0);
        b.on_owner_acted();
        assert!(b.wants_probe(t0 + Duration::from_secs(1)));
        assert!(!b.on_row_retired());
        assert_eq!(b.phase(), CardPhase::Watching { since: t0 });
        assert!(!b.wants_probe(t0 + WATCH_FOR));
        assert_eq!(b.lifecycle_deadline(), None, "read rows do not return");

        // A decision that says no settles without ever raising.
        let mut n = CardState::new();
        assert!(n.begin_deciding(Instant::now()));
        assert!(n.on_decided(&Verdict::Quiet(NotDue::Granted), t0));
        assert_eq!(n.phase(), CardPhase::Settled);

        // Not now / the grant observed: settle from watching.
        let mut w = CardState::new();
        assert!(w.begin_deciding(Instant::now()));
        assert!(w.on_decided(&Verdict::Offer, t0));
        w.on_raised(t0);
        w.settle();
        assert_eq!(w.phase(), CardPhase::Settled);
        assert!(!w.wants_probe(t0));
        assert!(!w.on_row_retired());
    }
}
