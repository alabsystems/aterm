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
//! consolidate this into one prompt. This module is that prompt: ONE decision
//! row on the message band (docs/DESIGN-unified-messages-2026-09-21.md §6
//! R15–R17; the retired floating card was the same question, 2026-09-07 →
//! 2026-09-22), with two capsules — *Open Settings*, which deep-links to the
//! Full Disk Access pane, and *Not now*, which records the answer so the row
//! never nags. The words are [`FDA_TITLE`] and [`FDA_DETAIL`]; the message
//! itself is `message_reporters::file_access_question`.
//!
//! # What it is not
//!
//! Not a consent dialog: aterm renders nothing macOS would render, answers
//! nothing, and the grant is still made by the human, in Settings (§0.2, §6).
//! Not an auto-open of System Settings: the pane opens on a press and never on
//! detection (§6). Not the warm-up: pressing nothing here touches a protected
//! folder, so no dialog is raised by the card itself (§3.5 stays fenced to the
//! Security panel). And not a coverage claim: the copy names the access check
//! and the Full Disk Access setting without promising fewer interruptions.
//! Which services a held grant silences is §7 S4's measurement and S4 is unrun.
//! A denied access probe is not proof that the System Settings switch is off:
//! the owner may already have enabled it. The card names unconfirmed access,
//! and its Settings follow-up asks the owner to leave an enabled switch on.
//!
//! # When it shows — every condition, because each one is a way to be wrong
//!
//! [`decide`] is the decision and it is pure. The card is due only when ALL
//! of these hold, and [`NotDue`] names which one failed so the log says why:
//!
//! * `[privacy] enabled` and `[privacy] notice` are on — re-read before every
//!   lifecycle step, so disabling either also retires a pending or visible card;
//! * the instance has a window — a headless instance holds the inert probe
//!   arm and could only ever invent a verdict;
//! * *Not now* is not on record — that answer latches for the life of the
//!   config directory;
//! * the probe actually OBSERVED `denied` — `unknown` (the probe refused, or
//!   the lane is off) is not a denial and earns no card; a pending worker is
//!   not a verdict, so its launch-time marker waits for the completed result;
//! * the running bundle's designated requirement is grant-STABLE
//!   (`DrClass::Identity`): a grant keyed to an ad-hoc cdhash dies on the next
//!   build (§2.2), so offering it would offer nothing. That is the whole
//!   identity test — a dev bundle signed with a stable identity may hold a
//!   grant of its own, so being a dev build is not by itself a refusal.
//!
//! A grant is never latched: the probe says `granted` on every launch while
//! it holds, and if the owner later removes it (a `tccutil reset`, a macOS
//! upgrade), the card is simply due again. Only *Not now* is remembered.
//!
//! # The two markers
//!
//! One file beside `aterm.toml` ([`MARKER`]) carries one token:
//! * `not-now` — the owner's answer; the card is not offered again;
//! * `opened` — the owner pressed *Open Settings* and the outcome is not yet
//!   known. It exists for Apple's own sheet, which tells the owner the app
//!   "will not have full disk access until it is quit" and offers Quit &
//!   Reopen: the NEXT process, finding `opened` beside a probe that now reads
//!   `granted`, shows the ✓ confirmation once and clears the marker, so the
//!   flip is acknowledged whichever process observes it. Finding `opened`
//!   beside `denied` is not an answer: the card is due again.
//!
//! # Lifetime, in one process
//!
//! [`CardState`] is the per-process machine the event loop drives from its
//! park point: the launch-time worker warms the signing identity and reads the
//! marker OFF the event loop (`codesign` and a disk read, neither of which may
//! stall the loop), the main thread decides against a completed CACHED probe
//! (a pending probe is waited on with the watch's own patience, [`WATCH_FOR`]),
//! and a due card posts its row — keyed `privacy.fda`, so there is only ever
//! one — and WATCHES two things: the probe, for the grant, for at most
//! [`WATCH_FOR`]; and the row itself, through `center.live(id)`. The
//! prompt-free access probe runs on a detached worker, with at most one in
//! flight; the event loop never joins it or calls its `open()`. An active
//! watch has a bounded refresh deadline, returning focus invalidates the
//! observation, and worker completion wakes the app. All windows read that
//! same App-owned result. This observes whether access became effective; it
//! does not read the System Settings switch or establish the permissions of
//! sessions adopted from another process. Nothing displaces the row: the
//! band ranks a decision row first and holds it for its own patience, so the
//! slot arbitration the floating card needed — a wait for the slot, a return
//! after displacement, a ✓ that queued behind another card — is gone with the
//! slot. A row that folds unanswered ([`CardState::on_row_retired`]) is not
//! latched: it returns after a day or at the next launch. Only an actually
//! shown, unanswered offer gets that daily return; quiet decisions, exhausted
//! initial attempts and a row the owner acted on stay settled.
//!
//! # No protected-folder literal lives here
//!
//! Nothing in this module names a folder or touches one (`grep_guard` B13).
//! The card's whole contact with the filesystem is the marker beside
//! `aterm.toml`, through [`crate::config_marker`].

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_containment::consent::{DrClass, FdaState};

/// The marker beside `aterm.toml` (the config-dir latch idiom of
/// `connections`' first-use notice, [`crate::config_marker`]).
pub(crate) const MARKER: &str = "privacy-access-card-answered";

/// A marker holds one token; anything bigger is something else wearing the
/// name and reads as "never answered" — the card shows, which errs toward
/// disclosure.
pub(crate) const MAX_MARKER_BYTES: usize = 64;

/// How long, after the card is raised, the instance keeps re-checking the
/// probe for the grant — and how long a due card waits on a probe that is
/// still pending before it is retired unraised. Bounded so a process that
/// outlives an unanswered card does not carry a per-park check forever; fresh
/// observations are made off the event loop at the bounded
/// `[privacy] probe_interval_ms` cadence.
pub(crate) const WATCH_FOR: Duration = Duration::from_secs(30 * 60);

/// How long the launch one-shot waits for its worker's verdict before asking
/// again. A verdict CAN be lost: an uncommitted incoming handoff drops every
/// wake but Commit, and the tick that starts the one-shot has no way to know a
/// wake it posted was dropped.
const DECIDE_TIMEOUT: Duration = Duration::from_secs(20);

/// How many verdicts the one-shot will wait for before giving up for the life
/// of the process.
pub(crate) const DECIDE_ATTEMPTS: u8 = 3;

/// How long a card that lifted away UNANSWERED — no marker written, the owner
/// never pressed it — waits before it is offered again in the same process.
/// The card was launch-only: an update that landed overnight raised it to an
/// empty room, it expired, and the owner met the next morning's dialogs with
/// no card and no pointer to Settings until the next update. A card the owner
/// answered is never re-offered (the marker decides that, in every process).
pub(crate) const REOFFER_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// The row's title (R15): the ASK — a question, so it claims nothing.
///
/// # What it says, and what it deliberately does not
///
/// The title asks for the grant. The first detail line ([`FDA_DETAIL`])
/// acknowledges that the owner may already have enabled Full Disk Access: the
/// current process's access probe does not read the System Settings toggle.
/// It rides behind Details: the row paints its question alone (review round
/// 2, 2026-09-23 — `File access not confirmed · Full Disk Access may already
/// be enabled` was a status and a hedge, and never said what it asked). It
/// does NOT name a folder, does NOT say which folders the grant covers, and
/// does NOT promise the interruptions go away. Coverage is §7 S4's claim to
/// make and S4 has not been run on this machine; scope is S1's and S1 has not
/// been run either, so no scope sentence ships at all (§3.4's own escalation).
/// The owner's ruling is that mitigating this annoyance is acceptable — so the
/// row must never read as elimination.
///
/// # Why it is short
///
/// The band's width law keeps a decision row's capsules whole and shortens
/// the words, so on an ordinary window the question must fit beside "Open
/// Settings" and "Not now" or the owner sees two buttons and no ask. Title
/// and detail together are held under ~75 characters (pinned by
/// `message_reporters::the_file_access_message_is_honest_and_fence_clean`).
///
/// # Why the wording is a const, and why it is here
///
/// A row that fires once per install is a string nobody re-reads in the
/// running app, so it is the easiest place in the product for an unmeasured
/// claim to sit unchallenged. Pinned as one const beside the decision it
/// belongs to, it is one grep away and the copy fence can hold it.
pub(crate) const FDA_TITLE: &str = "Allow Full Disk Access?";

/// The row's first detail line (R15): the switch may already be on — a failed
/// probe does not read it. See [`FDA_TITLE`] for the fence it lives under.
pub(crate) const FDA_DETAIL: &str = "Full Disk Access may already be enabled";

/// The ✓ row that supersedes the question once the probe observes the grant
/// (R17; `message_reporters::file_access_granted`). It names the fact and
/// stops: which services the grant covers and how far it reaches are §7 S4's
/// and S1's measurements, and neither has been run. The success glyph is the
/// row's own; the words carry no marker.
pub(crate) const GRANTED_CAPTION: &str = "Full disk access \u{2014} granted to aterm";

/// The words the row takes after *Open Settings* (R16): `openURL:` reports
/// only that System Settings TOOK the URL, never that it scrolled to the row,
/// so the route in words (`route`, from `menu::privacy_settings_path_words`)
/// goes out on every outcome, FIRST — a terse restate is the outcome and the
/// route (the owner's attention rule, 2026-09-23). Behind it: finding aterm
/// there — an app has a switch in that list only once it is listed, and the
/// `+` is how a human adds it — what to do if it is already on, and what
/// aterm does next (re-checks its probe); nothing the B10/B12 phrase fence
/// forbids. One sentence in the `"<title> — <detail>"` shape;
/// `messages_host::route_words` splits it at the dash into the row's title
/// and at its `; ` joints into the row's detail lines, the route the first.
pub(crate) fn opened_settings_caption(opened: bool, route: &str) -> String {
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

/// What the marker records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Marker {
    /// The owner pressed *Not now*: the card is not offered again for the
    /// life of the config directory.
    NotNow,
    /// The owner pressed *Open Settings* and the outcome is not yet known —
    /// see the module doc. Consumed by the decision that resolves it.
    Opened,
}

impl Marker {
    /// The recorded token.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotNow => "not-now",
            Self::Opened => "opened",
        }
    }

    /// Parse a marker's text; anything else is not a record this code wrote.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "not-now" => Some(Self::NotNow),
            "opened" => Some(Self::Opened),
            _ => None,
        }
    }
}

/// Where the marker lives for this config, or `None` with no config path.
pub(crate) fn marker_path(config_path: Option<&Path>) -> Option<PathBuf> {
    config_path
        .and_then(Path::parent)
        .map(|dir| dir.join(MARKER))
}

/// The recorded marker, if any. A disk read — worker thread only.
pub(crate) fn read_marker(config_path: Option<&Path>) -> Option<Marker> {
    marker_path(config_path)
        .and_then(|path| crate::config_marker::read_marker(&path, MAX_MARKER_BYTES))
        .and_then(|text| Marker::parse(&text))
}

/// Record `marker`. Best-effort, worker thread only: an unwritable config
/// directory means the card comes back at the next launch, which errs toward
/// disclosure.
pub(crate) fn record_marker(config_path: Option<&Path>, marker: Marker) -> std::io::Result<()> {
    let Some(path) = marker_path(config_path) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory to record the answer in",
        ));
    };
    crate::config_marker::write_marker(&path, marker.as_str(), MAX_MARKER_BYTES)
}

/// Clear an `opened` marker once the grant it was waiting on has been
/// acknowledged. Only an `opened` marker is touched: a `not-now` answer is
/// the owner's and stays. Worker thread only.
pub(crate) fn clear_opened(config_path: Option<&Path>) -> std::io::Result<()> {
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

/// Everything the decision reads. All of it observed; none of it inferred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CardFacts {
    /// `[privacy] enabled && [privacy] notice`.
    pub(crate) offered: bool,
    /// The instance has no window (inert probe arms).
    pub(crate) headless: bool,
    /// The cached Full Disk Access verdict.
    pub(crate) fda: FdaState,
    /// The designated requirement class a grant would be keyed to.
    pub(crate) dr: DrClass,
}

/// Why the card is NOT due — one named reason, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NotDue {
    /// `[privacy] enabled = false` or `[privacy] notice = false`.
    Off,
    /// A headless instance cannot observe a denial.
    Headless,
    /// The owner already answered *Not now*.
    Answered,
    /// The probe reports the grant is held.
    Granted,
    /// The probe did not observe a denial (`unknown`: refused, or the lane
    /// is off) — not a denial, so not a card.
    NotObservedDenied,
    /// A grant keyed to this requirement class would not survive a rebuild.
    GrantUnstable(DrClass),
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
        }
    }
}

/// What the launch-time decision resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Post the row once the cached probe confirms the denial.
    Offer,
    /// The owner opened Settings from an earlier process and the grant is now
    /// held: post the ✓ confirmation once and clear the `opened` marker.
    Confirm,
    /// Nothing to show, and why.
    Quiet(NotDue),
}

/// THE DECISION. Pure over the facts and the marker; see the module doc for
/// why each condition exists. An `opened` marker is not an answer — it only
/// turns a `granted` verdict into a one-time confirmation.
pub(crate) fn decide(facts: &CardFacts, marker: Option<Marker>) -> Verdict {
    if !facts.offered {
        return Verdict::Quiet(NotDue::Off);
    }
    if facts.headless {
        return Verdict::Quiet(NotDue::Headless);
    }
    if marker == Some(Marker::NotNow) {
        return Verdict::Quiet(NotDue::Answered);
    }
    match facts.fda {
        FdaState::Granted if marker == Some(Marker::Opened) => return Verdict::Confirm,
        FdaState::Granted => return Verdict::Quiet(NotDue::Granted),
        FdaState::Unknown => return Verdict::Quiet(NotDue::NotObservedDenied),
        FdaState::Denied => {}
    }
    if !facts.dr.grant_stable() {
        return Verdict::Quiet(NotDue::GrantUnstable(facts.dr));
    }
    Verdict::Offer
}

/// The per-process lifecycle of the card, driven from the event loop's park
/// point. Every transition is a method so the machine is table-testable
/// without an `App`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardPhase {
    /// Nothing asked yet — the launch-time one-shot has not run.
    Idle,
    /// The worker is warming the identity and reading the marker, since
    /// `since` — stamped so a verdict that never arrives ([`DECIDE_TIMEOUT`])
    /// is asked for again instead of waiting for a wake that cannot come.
    Deciding { since: Instant },
    /// Due; the row is posted at the next park whose cached probe still reads
    /// `denied` (a pending probe is waited on, for at most [`WATCH_FOR`]).
    Due,
    /// The row is posted since `since` (or the owner opened Settings then);
    /// the instance watches the probe for the grant and the row for its end.
    Watching { since: Instant },
    /// The episode ended. An actually shown but unanswered offer may return
    /// after REOFFER_AFTER; explicit answers and quiet decisions remain settled.
    Settled,
}

/// See [`CardPhase`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CardState {
    phase: CardPhase,
    /// When the offer became due — the anchor of a due card's wait on a
    /// pending probe.
    due_since: Option<Instant>,
    /// When the row was FIRST posted in this process: what makes an expired
    /// episode an "actually shown" one, which is the only kind that returns.
    first_raised: Option<Instant>,
    /// The owner pressed something on the row (Open Settings, or Details).
    /// A row the owner acted on is never re-offered in this process.
    owner_acted: bool,
    /// How many times the launch one-shot has asked its worker for a verdict
    /// ([`DECIDE_ATTEMPTS`]).
    decide_attempts: u8,
    /// When the card settled unanswered, for [`REOFFER_AFTER`].
    settled_at: Option<Instant>,
}

impl CardState {
    pub(crate) const fn new() -> Self {
        Self {
            phase: CardPhase::Idle,
            due_since: None,
            decide_attempts: 0,
            settled_at: None,
            first_raised: None,
            owner_acted: false,
        }
    }

    pub(crate) const fn phase(self) -> CardPhase {
        self.phase
    }

    /// Every active phase owns an absolute expiry, even with no probe result.
    /// The host combines this with its refresh deadline, so a slow or stuck
    /// probe cannot extend a phase's lifetime.
    pub(crate) fn lifecycle_deadline(self) -> Option<Instant> {
        match self.phase {
            CardPhase::Idle => None,
            CardPhase::Settled => self.settled_at.map(|at| at + REOFFER_AFTER),
            CardPhase::Deciding { since } => Some(since + DECIDE_TIMEOUT),
            CardPhase::Due => self.due_since.map(|since| since + WATCH_FOR),
            CardPhase::Watching { since } => Some(since + WATCH_FOR),
        }
    }

    /// Retire an expired probe wait or grant watch BEFORE asking for another
    /// observation. Deciding uses its bounded retry path instead; this method
    /// never consumes a marker that the next process may still acknowledge.
    /// A watch that ends with the row posted and never pressed is the
    /// UNANSWERED path: it settles with the daily return.
    pub(crate) fn expire_wait(&mut self, now: Instant) -> bool {
        if matches!(self.phase, CardPhase::Due | CardPhase::Watching { .. })
            && self
                .lifecycle_deadline()
                .is_some_and(|deadline| now >= deadline)
        {
            let unanswered = self.first_raised.is_some() && !self.owner_acted;
            self.settle();
            if unanswered {
                self.settled_at = Some(now);
            }
            true
        } else {
            false
        }
    }

    /// Admit one lifecycle step against the CURRENT settings, including a
    /// pending worker verdict or a posted row. Turning the notice off is an
    /// answer for this process: a late worker cannot bring it back. The host
    /// also resolves the card's own rows (`resolve_key_prefix("privacy.")`).
    pub(crate) fn admit_current_policy(&mut self, enabled: bool, notice: bool) -> bool {
        if enabled && notice {
            true
        } else {
            self.settle();
            false
        }
    }

    /// The one-shot fired: a worker is deciding. Only from [`CardPhase::Idle`].
    pub(crate) fn begin_deciding(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Idle {
            return false;
        }
        self.phase = CardPhase::Deciding { since: now };
        self.decide_attempts = self.decide_attempts.saturating_add(1);
        true
    }

    /// The worker's verdict has not arrived within [`DECIDE_TIMEOUT`].
    pub(crate) fn deciding_lapsed(&self, now: Instant) -> bool {
        matches!(self.phase, CardPhase::Deciding { since }
            if now.duration_since(since) >= DECIDE_TIMEOUT)
    }

    /// A lapsed decision: ask again from `Idle`, or settle for good once
    /// [`DECIDE_ATTEMPTS`] have been spent. `true` when another attempt will be
    /// made. DECIDING IS NOT A TERMINAL STATE — a dropped wake (the incoming
    /// handoff swallows every non-Commit wake until it commits) must not make
    /// the card silently dead for the life of the process.
    pub(crate) fn reopen_deciding(&mut self) -> bool {
        if self.decide_attempts >= DECIDE_ATTEMPTS {
            self.settle();
            return false;
        }
        self.phase = CardPhase::Idle;
        true
    }

    /// The main thread decided. A verdict that arrives in any phase but
    /// [`CardPhase::Deciding`] is stale (a second worker, a re-entrant wake)
    /// and is ignored. `Confirm` settles here: the caller posts the ✓ row
    /// (nothing queues behind another surface any more) and clears the marker.
    pub(crate) fn on_decided(&mut self, verdict: &Verdict, now: Instant) -> bool {
        if !matches!(self.phase, CardPhase::Deciding { .. }) {
            return false;
        }
        self.settled_at = None;
        self.phase = match verdict {
            Verdict::Offer => {
                self.due_since.get_or_insert(now);
                CardPhase::Due
            }
            Verdict::Confirm | Verdict::Quiet(_) => CardPhase::Settled,
        };
        true
    }

    /// Whether the row may be posted at `now`: only while due, and only
    /// inside the wait a pending probe is given ([`WATCH_FOR`] from
    /// `due_since`) — a probe that never completes does not keep a card due
    /// for the life of the process.
    pub(crate) fn raise_allowed(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Due {
            return false;
        }
        !self.expire_wait(now)
    }

    /// The row was posted at `now`.
    pub(crate) fn on_raised(&mut self, now: Instant) {
        if self.phase == CardPhase::Due {
            self.phase = CardPhase::Watching { since: now };
            self.first_raised.get_or_insert(now);
        }
    }

    /// The owner opened the row's Details: read, never re-offered in this
    /// process; the watch continues.
    pub(crate) fn on_owner_acted(&mut self) {
        if matches!(self.phase, CardPhase::Watching { .. }) {
            self.owner_acted = true;
        }
    }

    /// The row left the band without a press — it folded on its own
    /// patience, and neither capsule nor Details was ever touched. That is
    /// the unanswered path: settled, with the daily return an actually shown,
    /// unanswered offer gets (`true`). A row the owner acted on keeps its
    /// watch (`false`): Open Settings restated it to the route words and the
    /// grant may still be observed after those fold; Details marked it read.
    pub(crate) fn on_row_retired(&mut self, now: Instant) -> bool {
        if !matches!(self.phase, CardPhase::Watching { .. }) || self.owner_acted {
            return false;
        }
        self.settle();
        self.settled_at = Some(now);
        true
    }

    /// The owner pressed *Open Settings* through either UI: start a bounded
    /// watch even if the automatic card was idle or settled. The host admits
    /// this explicit gesture against current settings before calling it.
    pub(crate) fn on_opened_settings(&mut self, now: Instant) {
        self.phase = CardPhase::Watching { since: now };
        self.owner_acted = true;
        self.settled_at = None;
    }

    /// Whether the probe should be re-checked at `now`. True only while
    /// watching and inside [`WATCH_FOR`]; a watch past its window settles.
    pub(crate) fn wants_probe(&mut self, now: Instant) -> bool {
        matches!(self.phase, CardPhase::Watching { .. }) && !self.expire_wait(now)
    }

    /// Nothing more in this process: *Not now*, the grant observed, or the
    /// decision said no.
    pub(crate) fn settle(&mut self) {
        self.phase = CardPhase::Settled;
        self.settled_at = None;
    }

    /// A card that settled UNANSWERED is offered again after
    /// [`REOFFER_AFTER`]: back to `Idle`, attempts reset, `true`. A card the
    /// owner pressed is never re-offered here — its marker settles every later
    /// decision in every process. Only a shown unanswered row's end sets
    /// settled_at; neither quiet decisions nor exhausted retries poll daily.
    pub(crate) fn reoffer_due(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Settled || self.owner_acted {
            return false;
        }
        if self
            .settled_at
            .is_some_and(|at| now.saturating_duration_since(at) >= REOFFER_AFTER)
        {
            // A fresh episode: the prior day's anchors and the retry budget
            // would otherwise expire or refuse the new offer at once.
            *self = Self::new();
            true
        } else {
            false
        }
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
        CardFacts, CardPhase, CardState, DECIDE_ATTEMPTS, DECIDE_TIMEOUT, FDA_DETAIL, FDA_TITLE,
        GRANTED_CAPTION, MARKER, MAX_MARKER_BYTES, Marker, NotDue, REOFFER_AFTER, Verdict,
        WATCH_FOR, clear_opened, decide, marker_path, opened_settings_caption, read_marker,
        record_marker,
    };
    use aterm_containment::consent::{DrClass, FdaState};
    use std::time::{Duration, Instant};

    fn denied_release() -> CardFacts {
        CardFacts {
            offered: true,
            headless: false,
            fda: FdaState::Denied,
            dr: DrClass::Identity,
        }
    }

    /// The two bounded waiting phases share one expiry contract. The clock
    /// step represents the host reaching the current phase's absolute
    /// deadline; it cannot be extended by a pending observation. The
    /// historical first-offer omission is the negative control.
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

    /// Tier-1 drives the real due and watching states to just before and
    /// exactly at their actual scheduled expiry. Repeated pending
    /// observations need not arrive for expiry to settle.
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
                actual
                    .first_raised
                    .is_some()
                    .then_some(deadline + REOFFER_AFTER),
                "only a shown unanswered offer schedules a daily return"
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

    #[test]
    fn an_initial_observation_that_stays_pending_exhausts_scheduled_attempts() {
        let mut now = Instant::now();
        let mut actual = CardState::new();
        for attempt in 1..=DECIDE_ATTEMPTS {
            assert!(actual.begin_deciding(now));
            let deadline = actual
                .lifecycle_deadline()
                .expect("decision owns a deadline");
            assert_eq!(deadline, now + DECIDE_TIMEOUT);
            assert!(!actual.deciding_lapsed(deadline - Duration::from_nanos(1)));
            assert!(actual.deciding_lapsed(deadline));
            assert!(
                !actual.expire_wait(deadline),
                "decision uses bounded retries"
            );
            assert_eq!(actual.reopen_deciding(), attempt < DECIDE_ATTEMPTS);
            now = deadline;
        }
        assert_eq!(actual.phase(), CardPhase::Settled);
        assert_eq!(actual.lifecycle_deadline(), None);
        assert!(!actual.on_decided(&Verdict::Confirm, now));
        assert!(!actual.on_decided(&Verdict::Offer, now));
    }

    /// The live settings guard over the existing lifecycle phases. The
    /// historical guard ran only at launch: disabling a visible or queued
    /// card left it eligible to return. `Buggy=1` replays that omission.
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

    /// Tier-1: drive all five shipping phases, then disable either or both
    /// settings. A late verdict and re-enabling cannot resurrect a settled
    /// card. The old omitted guard is projected as the negative control.
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
    }

    /// Every condition is a way to be wrong, and each failure is named. The
    /// order is the order a reader would want explained: the switch, the
    /// instance, the answer, the verdict, the identity. And the one marker
    /// that is not an answer — `opened` — turns `granted` into a one-time
    /// confirmation and nothing else.
    #[test]
    fn the_card_is_due_only_when_every_condition_holds() {
        assert_eq!(decide(&denied_release(), None), Verdict::Offer);

        let off = CardFacts {
            offered: false,
            ..denied_release()
        };
        assert_eq!(decide(&off, None), Verdict::Quiet(NotDue::Off));

        let headless = CardFacts {
            headless: true,
            ..denied_release()
        };
        assert_eq!(decide(&headless, None), Verdict::Quiet(NotDue::Headless));

        assert_eq!(
            decide(&denied_release(), Some(Marker::NotNow)),
            Verdict::Quiet(NotDue::Answered)
        );
        assert_eq!(
            decide(&denied_release(), Some(Marker::Opened)),
            Verdict::Offer,
            "opened-but-still-denied is not an answer: the card is due again"
        );

        let granted = CardFacts {
            fda: FdaState::Granted,
            ..denied_release()
        };
        assert_eq!(decide(&granted, None), Verdict::Quiet(NotDue::Granted));
        assert_eq!(
            decide(&granted, Some(Marker::Opened)),
            Verdict::Confirm,
            "the flip the owner started in an earlier process is acknowledged once"
        );
        assert_eq!(
            decide(&granted, Some(Marker::NotNow)),
            Verdict::Quiet(NotDue::Answered)
        );
        let unknown = CardFacts {
            fda: FdaState::Unknown,
            ..denied_release()
        };
        assert_eq!(
            decide(&unknown, None),
            Verdict::Quiet(NotDue::NotObservedDenied),
            "unknown is not denied"
        );

        for dr in [DrClass::Cdhash, DrClass::Unsigned, DrClass::Unknown] {
            let unstable = CardFacts {
                dr,
                ..denied_release()
            };
            assert_eq!(
                decide(&unstable, None),
                Verdict::Quiet(NotDue::GrantUnstable(dr)),
                "{dr:?} is not grant-stable"
            );
        }

        // Every reason renders as one sentence a log line can carry.
        for reason in [
            NotDue::Off,
            NotDue::Headless,
            NotDue::Answered,
            NotDue::Granted,
            NotDue::NotObservedDenied,
            NotDue::GrantUnstable(DrClass::Cdhash),
        ] {
            let line = reason.to_string();
            assert!(!line.is_empty() && !line.contains('\n'), "{line:?}");
        }
    }

    /// The marker: path beside `aterm.toml`, one token, round-trips through
    /// the shared marker rules, tolerates a hand edit's trailing newline, and
    /// `clear_opened` removes ONLY an `opened` marker.
    #[test]
    fn the_marker_round_trips_beside_the_config() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-consent-card-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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

    /// The route and grant words are held to the same fences as the question
    /// (`message_reporters::the_file_access_message_is_honest_and_fence_clean`
    /// holds [`FDA_TITLE`]/[`FDA_DETAIL`]): the restart-phrase ruling, no
    /// coverage or scope claim, no promise of elimination — and each is one
    /// line, in the `"<title> — <detail>"` shape the row splits at, with no
    /// pictogram of its own (the glyph column is the band's).
    #[test]
    fn the_route_and_grant_words_are_fence_clean() {
        let route = "System Settings \u{25b8} Privacy & Security \u{25b8} Full Disk Access";
        for text in [
            GRANTED_CAPTION.to_string(),
            opened_settings_caption(true, route),
            opened_settings_caption(false, route),
        ] {
            let lower = text.to_ascii_lowercase();
            for banned in [
                "restart",
                "relaunch",
                "re-launch",
                "reopen",
                "re-open",
                "quit and",
                "next launch",
                "reboot",
                "launch aterm again",
            ] {
                assert!(!lower.contains(banned), "{banned:?} in {text:?}");
            }
            for unmeasured in [
                "cover",
                "documents",
                "desktop",
                "downloads",
                "volume",
                "icloud",
                "retires",
                "this process",
                "new processes",
            ] {
                assert!(!lower.contains(unmeasured), "{unmeasured:?} in {text:?}");
            }
            for promise in [
                "never again",
                "no more",
                "eliminat",
                "not be interrupted",
                "never be interrupted",
                "without interruption",
            ] {
                assert!(!lower.contains(promise), "{promise:?} in {text:?}");
            }
            assert!(text.contains(" \u{2014} "), "title — detail: {text:?}");
            assert!(!text.contains('\n'));
            assert!(
                text.chars().next().is_some_and(char::is_alphanumeric),
                "no pictogram in the words; the band's glyph column carries it: {text:?}"
            );
        }
        for opened in [true, false] {
            let text = opened_settings_caption(opened, route);
            assert!(text.contains(route));
            assert!(text.contains("if already enabled, leave it on"));
            assert!(!text.contains("turn on aterm"));
            assert!(
                text.contains('+'),
                "the whole route, listing included: {text}"
            );
        }
        assert_eq!(
            opened_settings_caption(true, route)
                .split_once(" \u{2014} ")
                .map(|(t, _)| t),
            Some("Opened System Settings")
        );
        assert_eq!(
            opened_settings_caption(false, route)
                .split_once(" \u{2014} ")
                .map(|(t, _)| t),
            Some("System Settings did not open")
        );
        assert!(FDA_TITLE.chars().count() + FDA_DETAIL.chars().count() <= 75);
    }

    fn reoffer_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            AccessCardReoffer {
                const Buggy = 0;
                var phase = 0;
                var answered = 0;
                var armed = 0;
                var fresh = 1;
                action Offer when (phase == 0) { phase = 1; }
                action Quiet when (phase == 0) { phase = 2; }
                action Answer when (phase == 1) {
                    phase = 2; answered = 1; armed = 0;
                }
                action Expire when (phase == 1) {
                    phase = 2; armed = if answered == 0 { 1 } else { 0 };
                }
                action Return when (phase == 2 && armed == 1) {
                    phase = 0; answered = 0; armed = 0;
                    fresh = if Buggy == 1 { 0 } else { 1 };
                }
                action Open when (phase <= 2) {
                    phase = 1; answered = 1; armed = 0; fresh = 1;
                }
                invariant FreshReturn: phase > 0 || fresh == 1;
                invariant AnswerStopsReoffer: armed == 0 || (phase == 2 && answered == 0);
                invariant Bounds: phase <= 2 && answered <= 1 && armed <= 1 && fresh <= 1;
            }
        }
    }

    /// The daily return, driven three ways: the watch expiring on a posted
    /// row (`Expire`), the row folding under the watch (`Fold` — the same
    /// unanswered path, `on_row_retired`), and the Settings gesture that
    /// restarts a settled watch with the owner's press on record.
    #[test]
    fn reoffer_and_settings_watch_conform_to_the_real_card_lifecycle() {
        use std::collections::BTreeMap;
        let model = reoffer_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
        let project = |s: CardState| {
            BTreeMap::from([
                (
                    "phase",
                    match s.phase {
                        CardPhase::Idle => 0,
                        CardPhase::Watching { .. } => 1,
                        CardPhase::Settled => 2,
                        _ => panic!("fixture projects only completed transitions"),
                    },
                ),
                ("answered", i64::from(s.owner_acted)),
                ("armed", i64::from(s.settled_at.is_some())),
                (
                    "fresh",
                    i64::from(
                        s.phase != CardPhase::Idle
                            || (s.first_raised.is_none() && s.due_since.is_none()),
                    ),
                ),
            ])
        };
        for actions in [
            &["Offer", "Expire", "Return", "Offer", "Answer"][..],
            &["Offer", "Fold", "Return", "Offer", "Fold"][..],
            &["Quiet", "Open", "Expire"][..],
        ] {
            let mut card = CardState::new();
            let mut state = model.init_state();
            let mut now = Instant::now();
            for action in actions {
                let fired = match *action {
                    "Offer" => {
                        assert!(card.begin_deciding(now));
                        assert!(card.on_decided(&Verdict::Offer, now));
                        assert!(card.raise_allowed(now));
                        card.on_raised(now);
                        "Offer"
                    }
                    "Quiet" => {
                        assert!(card.begin_deciding(now));
                        assert!(card.on_decided(&Verdict::Quiet(NotDue::Granted), now));
                        "Quiet"
                    }
                    "Answer" => {
                        card.on_owner_acted();
                        card.settle();
                        "Answer"
                    }
                    "Expire" => {
                        now += WATCH_FOR;
                        assert!(card.expire_wait(now));
                        assert_eq!(
                            card.lifecycle_deadline(),
                            (!card.owner_acted).then_some(now + REOFFER_AFTER)
                        );
                        "Expire"
                    }
                    // The row folded on its own patience, long before the
                    // watch would have: the model's Expire, reached sooner.
                    "Fold" => {
                        now += Duration::from_secs(600);
                        assert!(card.on_row_retired(now));
                        assert_eq!(card.lifecycle_deadline(), Some(now + REOFFER_AFTER));
                        "Expire"
                    }
                    "Return" => {
                        assert!(!card.reoffer_due(now + REOFFER_AFTER - Duration::from_nanos(1)));
                        now += REOFFER_AFTER;
                        assert!(card.reoffer_due(now));
                        "Return"
                    }
                    "Open" => {
                        card.on_opened_settings(now);
                        "Open"
                    }
                    _ => unreachable!(),
                };
                let actual = project(card);
                let (ok, why) = aterm_spec::verify::validate_transition_tiered(
                    &model,
                    &[],
                    &state,
                    &actual,
                    Some(fired),
                    "real access-card reoffer",
                );
                assert!(ok, "{action}: {actual:?}: {why}");
                if fired == "Return" {
                    let mut stale = card;
                    stale.first_raised = Some(now - REOFFER_AFTER);
                    let broken = project(stale);
                    assert!(!model.check_invariant("FreshReturn", &broken));
                    assert!(!model.successors(fired, &state).contains(&broken));
                }
                if fired == "Open" {
                    let mut old = actual.clone();
                    old.insert("phase", 2); // old Watching-only gesture lost this request
                    assert!(!model.successors(fired, &state).contains(&old));
                }
                state = actual;
            }
            let last = actions.last().copied();
            if last == Some("Fold") {
                assert!(
                    card.reoffer_due(now + REOFFER_AFTER),
                    "a row that folds unanswered returns the next day too"
                );
            } else {
                assert!(!card.reoffer_due(now + REOFFER_AFTER * 2));
                assert_eq!(
                    card.lifecycle_deadline(),
                    None,
                    "answered/quiet cards do not poll daily"
                );
            }
        }
    }

    /// A VERDICT THAT NEVER ARRIVES DOES NOT KILL THE CARD (2026-09-09). An
    /// uncommitted incoming handoff drops every wake but Commit, so the card's
    /// own verdict can be swallowed. Deciding therefore lapses, asks again, and
    /// gives up only after [`DECIDE_ATTEMPTS`] — never silently forever.
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
        assert!(!s.begin_deciding(t0), "and it does not start again");
        // A verdict that DOES arrive still ends the deciding phase.
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
        assert!(!s.on_row_retired(t0), "idle: no row to lose");
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
        assert!(!s.on_row_retired(t0), "not posted: no row to lose");
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

        // Opening Settings restarts the window and marks the owner's press:
        // the row's fold is not an unanswered end after that.
        let t2 = t0 + Duration::from_secs(20);
        let mut o = CardState::new();
        assert!(o.begin_deciding(Instant::now()));
        assert!(o.on_decided(&Verdict::Offer, t0));
        o.on_raised(t0);
        o.on_opened_settings(t2);
        assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
        assert!(!o.on_row_retired(t2 + Duration::from_secs(90)));
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
        assert!(!b.on_row_retired(t0 + Duration::from_secs(1)));
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
        assert!(!w.on_row_retired(t0));
        assert_eq!(CardState::default(), CardState::new());
    }

    /// THE ROW'S OWN END (design §3.10): a posted row that folds on its Ask
    /// hold with nothing pressed is the unanswered path — settled, with the
    /// daily return — while a row the owner acted on keeps its watch, and a
    /// settled or unposted card has no row to lose.
    #[test]
    fn a_row_that_folds_unanswered_settles_with_a_daily_return() {
        let t0 = Instant::now();
        let folded = t0 + Duration::from_secs(600);
        let mut s = CardState::new();
        assert!(s.begin_deciding(t0));
        assert!(s.on_decided(&Verdict::Offer, t0));
        s.on_raised(t0);
        assert!(s.on_row_retired(folded));
        assert_eq!(s.phase(), CardPhase::Settled);
        assert_eq!(s.lifecycle_deadline(), Some(folded + REOFFER_AFTER));
        assert!(
            !s.on_row_retired(folded),
            "settled: nothing to retire twice"
        );
        assert!(!s.reoffer_due(folded + REOFFER_AFTER - Duration::from_secs(1)));
        assert!(s.reoffer_due(folded + REOFFER_AFTER));
        assert_eq!(s.phase(), CardPhase::Idle, "a fresh episode");
        assert_eq!(s.lifecycle_deadline(), None);
    }
}
