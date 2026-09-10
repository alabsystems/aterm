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
//! (`notice::privacy_notice_once`) but never wired, so nothing at launch ever
//! told the owner there was one switch to flip, and the per-folder dialogs
//! stayed the only surface. The owner's direction (2026-09-07) is that aterm
//! consolidate this into one prompt. This module is that prompt: ONE passive
//! card, the same shape as the first-launch admin card
//! (`docs/GOLDEN-INSTALL-PATH.md` "the one prompt"), with two controls —
//! *Open Settings*, which deep-links to the Full Disk Access pane, and *Not
//! now*, which records the answer so the card never nags.
//!
//! # What it is not
//!
//! Not a consent dialog: aterm renders nothing macOS would render, answers
//! nothing, and the grant is still made by the human, in Settings (§0.2, §6).
//! Not an auto-open of System Settings: the pane opens on a press and never on
//! detection (§6). Not the warm-up: pressing nothing here touches a protected
//! folder, so no dialog is raised by the card itself (§3.5 stays fenced to the
//! Security panel). And not a coverage claim: the copy says FEWER prompts and
//! names the grant as the one switch macOS offers, and stops there, because
//! which services a held grant silences is §7 S4's measurement and S4 is unrun.
//!
//! # When it shows — every condition, because each one is a way to be wrong
//!
//! [`decide`] is the decision and it is pure. The card is due only when ALL
//! of these hold, and [`NotDue`] names which one failed so the log says why:
//!
//! * `[privacy] enabled` and `[privacy] notice` are on — the same switch that
//!   gated the pill, so a config that silenced the pill silences the card;
//! * the instance has a window — a headless instance holds the inert probe
//!   arm and could only ever invent a verdict;
//! * *Not now* is not on record — that answer latches for the life of the
//!   config directory;
//! * the probe actually OBSERVED `denied` — `unknown` (the probe refused, or
//!   the lane is off) is not a denial and earns no card;
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
//! stall the loop), the main thread decides against the CACHED probe, the card
//! waits for the shared notice slot to be free rather than clobbering an admin
//! card or an update-ready card, and once raised the instance WATCHES the probe
//! for the grant — one cached `open()` per `probe_interval_ms`, for at most
//! [`WATCH_FOR`] — so a switch flipped while the owner is in Settings is
//! noticed without anyone pressing anything more. Every other producer writes
//! the single notice slot unconditionally, so a raised card CAN be displaced
//! (the admin card lands seconds after it on a fresh Mac); a displaced card
//! that the owner never pressed goes back to waiting for the slot and is
//! re-raised when it frees, for as long as its own hold would have run
//! ([`RERAISE_WITHIN`]). A card that lifts away unanswered is not latched: it
//! returns at the next launch, exactly like the admin card returns on the next
//! pass.
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
/// `packages_screen::ADMIN_STEP_MARKER` and `connections`' first-use notice).
pub(crate) const MARKER: &str = "privacy-access-card-answered";

/// A marker holds one token; anything bigger is something else wearing the
/// name and reads as "never answered" — the card shows, which errs toward
/// disclosure.
pub(crate) const MAX_MARKER_BYTES: usize = 64;

/// How long, after the card is raised, the instance keeps re-checking the
/// probe for the grant. Bounded so a process that outlives an unanswered card
/// does not carry a per-park check forever; the check itself is one cached
/// `open()` per `[privacy] probe_interval_ms`.
pub(crate) const WATCH_FOR: Duration = Duration::from_secs(30 * 60);

/// How long after its FIRST raise a displaced card may be raised again — the
/// card's own hold (`notice::ADMIN_STEP_TTL`). Past it the card has had its
/// turn for this process; the next launch asks again.
pub(crate) const RERAISE_WITHIN: Duration = crate::notice::ADMIN_STEP_TTL.saturating_mul(2);

/// How long the launch one-shot waits for its worker's verdict before asking
/// again. A verdict CAN be lost: an uncommitted incoming handoff drops every
/// wake but Commit, and the tick that starts the one-shot has no way to know a
/// wake it posted was dropped.
const DECIDE_TIMEOUT: Duration = Duration::from_secs(20);

/// How many verdicts the one-shot will wait for before giving up for the life
/// of the process.
pub(crate) const DECIDE_ATTEMPTS: u8 = 3;

/// The pill that replaces the card once the probe observes the grant. It
/// names the fact and stops: which services the grant covers and how far it
/// reaches are §7 S4's and S1's measurements, and neither has been run.
pub(crate) const GRANTED_CAPTION: &str = "\u{2713} Full disk access \u{2014} granted to aterm";

/// How long the "opened System Settings" pill stays up. The owner is in
/// another app reading a pane; the ordinary 5.4 s pill would be gone before
/// they look back, and the route in words is the one thing this pill carries.
pub(crate) const OPENED_SETTINGS_TTL: Duration = Duration::from_secs(90);

/// The pill after *Open Settings*: `openURL:` reports only that System
/// Settings TOOK the URL, never that it scrolled to the row, so the route in
/// words (`route`, from `menu::privacy_settings_path_words`) goes out on every
/// outcome. It names the whole route — an app has a switch in that list only
/// once it is listed, and the `+` is how a human adds it — states what aterm
/// does next (re-checks its probe), and asks for nothing the B10/B12 phrase
/// fence forbids.
pub(crate) fn opened_settings_caption(opened: bool, route: &str) -> String {
    if opened {
        format!(
            "\u{2699} Opened System Settings \u{2014} turn on aterm under {route} (add it with + if it \
             is not listed); aterm re-checks on its own"
        )
    } else {
        format!(
            "\u{2699} System Settings did not open \u{2014} the switch is at {route} (add aterm with + \
             if it is not listed)"
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
    /// Raise the card when the notice slot is free.
    Offer,
    /// The owner opened Settings from an earlier process and the grant is now
    /// held: show the ✓ confirmation once and clear the `opened` marker.
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
    /// Due; waiting for the shared notice slot to be free.
    Due,
    /// On screen since `since` (or displaced and waiting to return); the
    /// instance watches the probe for the grant.
    Watching { since: Instant },
    /// The grant was observed but another card held the slot: waiting to show
    /// the ✓ confirmation, since `since`. The `opened` marker is not consumed
    /// until it is shown, so a process that never gets the slot leaves the
    /// acknowledgement to the next one.
    Confirming { since: Instant },
    /// Nothing more will happen in this process.
    Settled,
}

/// See [`CardPhase`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CardState {
    phase: CardPhase,
    /// When the card was FIRST raised in this process — the anchor for
    /// [`RERAISE_WITHIN`].
    first_raised: Option<Instant>,
    /// The owner pressed something on the card (Open Settings, or the body).
    /// A card the owner acted on is never re-raised in this process.
    owner_acted: bool,
    /// How many times the launch one-shot has asked its worker for a verdict
    /// ([`DECIDE_ATTEMPTS`]).
    decide_attempts: u8,
}

impl CardState {
    pub(crate) const fn new() -> Self {
        Self {
            phase: CardPhase::Idle,
            decide_attempts: 0,
            first_raised: None,
            owner_acted: false,
        }
    }

    pub(crate) const fn phase(self) -> CardPhase {
        self.phase
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
            self.phase = CardPhase::Settled;
            return false;
        }
        self.phase = CardPhase::Idle;
        true
    }

    /// The main thread decided. A verdict that arrives in any phase but
    /// [`CardPhase::Deciding`] is stale (a second worker, a re-entrant wake)
    /// and is ignored. `Confirm` waits for the slot like `Offer` does: the
    /// confirmation is a one-time pill the caller raises when it can.
    pub(crate) fn on_decided(&mut self, verdict: &Verdict, now: Instant) -> bool {
        if !matches!(self.phase, CardPhase::Deciding { .. }) {
            return false;
        }
        self.phase = match verdict {
            Verdict::Offer => CardPhase::Due,
            Verdict::Confirm => CardPhase::Confirming { since: now },
            Verdict::Quiet(_) => CardPhase::Settled,
        };
        true
    }

    /// Whether a raise is still allowed at `now`: always for the first one; a
    /// RE-raise only inside [`RERAISE_WITHIN`] of the first — past it the card
    /// settles instead, so a card displaced late in its hold cannot come back
    /// for a whole second hold.
    pub(crate) fn raise_allowed(&mut self, now: Instant) -> bool {
        if self.phase != CardPhase::Due {
            return false;
        }
        let within = self
            .first_raised
            .is_none_or(|at| now.saturating_duration_since(at) < RERAISE_WITHIN);
        if !within {
            self.phase = CardPhase::Settled;
        }
        within
    }

    /// The card went on screen at `now`.
    pub(crate) fn on_raised(&mut self, now: Instant) {
        if self.phase == CardPhase::Due {
            self.phase = CardPhase::Watching { since: now };
            self.first_raised.get_or_insert(now);
        }
    }

    /// The grant was observed (while due, watching, or deciding) but the ✓
    /// could not be shown yet: wait for the slot.
    pub(crate) fn on_grant_awaiting_slot(&mut self, now: Instant) {
        if matches!(
            self.phase,
            CardPhase::Due | CardPhase::Watching { .. } | CardPhase::Deciding { .. }
        ) {
            self.phase = CardPhase::Confirming { since: now };
        }
    }

    /// Whether a pending confirmation has waited past its patience
    /// ([`WATCH_FOR`]): then it settles, leaving the `opened` marker for the
    /// next process to acknowledge.
    pub(crate) fn confirmation_expired(&mut self, now: Instant) -> bool {
        match self.phase {
            CardPhase::Confirming { since }
                if now.saturating_duration_since(since) >= WATCH_FOR =>
            {
                self.phase = CardPhase::Settled;
                true
            }
            _ => false,
        }
    }

    /// The owner pressed the card's body: dismissed for now, never re-raised
    /// in this process; the watch continues.
    pub(crate) fn on_owner_acted(&mut self) {
        if matches!(self.phase, CardPhase::Watching { .. }) {
            self.owner_acted = true;
        }
    }

    /// The owner pressed *Open Settings*: keep watching, restart the watch
    /// window — they are in Settings right now — and never re-raise.
    pub(crate) fn on_opened_settings(&mut self, now: Instant) {
        if matches!(self.phase, CardPhase::Watching { .. }) {
            self.phase = CardPhase::Watching { since: now };
            self.owner_acted = true;
        }
    }

    /// The card is no longer on the glass and the owner did not press it:
    /// another producer took the slot, or it lifted away. Within
    /// [`RERAISE_WITHIN`] of the first raise it goes back to waiting for the
    /// slot (`true`); past that, or after a press, it stays where it is.
    pub(crate) fn on_displaced(&mut self, now: Instant) -> bool {
        let CardPhase::Watching { .. } = self.phase else {
            return false;
        };
        if self.owner_acted {
            return false;
        }
        let within = self
            .first_raised
            .is_some_and(|at| now.saturating_duration_since(at) < RERAISE_WITHIN);
        if !within {
            return false;
        }
        self.phase = CardPhase::Due;
        true
    }

    /// Whether the probe should be re-checked at `now`. True only while
    /// watching and inside [`WATCH_FOR`]; a watch past its window settles.
    pub(crate) fn wants_probe(&mut self, now: Instant) -> bool {
        match self.phase {
            CardPhase::Watching { since } => {
                if now.saturating_duration_since(since) >= WATCH_FOR {
                    self.phase = CardPhase::Settled;
                    false
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    /// Nothing more in this process: *Not now*, the grant observed, or the
    /// decision said no.
    pub(crate) fn settle(&mut self) {
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
        CardFacts, CardPhase, CardState, DECIDE_ATTEMPTS, DECIDE_TIMEOUT, GRANTED_CAPTION, MARKER,
        MAX_MARKER_BYTES, Marker, NotDue, RERAISE_WITHIN, Verdict, WATCH_FOR, clear_opened, decide,
        marker_path, opened_settings_caption, read_marker, record_marker,
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

    /// The two follow-up pills are held to the same fences as the card: the
    /// restart-phrase ruling, no coverage or scope claim, no promise of
    /// elimination — and each parses as the notice grammar.
    #[test]
    fn the_follow_up_pills_are_fence_clean() {
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
            assert!(text.contains(" \u{2014} "), "notice grammar: {text:?}");
            assert!(!text.contains('\n'));
        }
        for opened in [true, false] {
            let text = opened_settings_caption(opened, route);
            assert!(text.contains(route));
            assert!(
                text.contains('+'),
                "the whole route, listing included: {text}"
            );
        }
        assert!(
            GRANTED_CAPTION.starts_with("\u{2713} "),
            "the success marker"
        );
    }

    /// The lifecycle: one decision per process, a stale verdict is ignored,
    /// the watch is bounded, a displaced card returns within its own hold
    /// unless the owner pressed it, and every terminal press settles it.
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

    /// THE RE-RAISE BUDGET MUST OUTLAST WHAT DISPLACES IT (2026-09-09): the
    /// admin card holds the slot for `ADMIN_STEP_TTL`, and the design promises
    /// the access card comes back when the slot frees. With the two equal the
    /// budget always closed first, so the return was unreachable in exactly the
    /// case the module names.
    #[test]
    fn a_displaced_card_can_still_return_after_the_card_that_displaced_it() {
        assert!(
            RERAISE_WITHIN > crate::notice::ADMIN_STEP_TTL,
            "the longest hold that can displace it must fit inside the budget"
        );
        let t0 = Instant::now();
        let mut s = CardState::new();
        assert!(s.begin_deciding(t0));
        assert!(s.on_decided(&Verdict::Offer, t0));
        assert!(s.raise_allowed(t0));
        s.on_raised(t0);
        // The admin card lands a moment later and takes the slot for its whole hold.
        let displaced_at = t0 + Duration::from_millis(500);
        assert!(s.on_displaced(displaced_at));
        let slot_frees = displaced_at + crate::notice::ADMIN_STEP_TTL;
        assert!(
            s.raise_allowed(slot_frees),
            "the slot frees inside the budget, so the card returns"
        );
    }

    #[test]
    fn the_lifecycle_decides_once_watches_for_a_while_and_settles() {
        let t0 = Instant::now();
        let mut s = CardState::new();
        assert_eq!(s.phase(), CardPhase::Idle);
        assert!(!s.wants_probe(t0), "idle: nothing to watch");
        assert!(!s.on_displaced(t0));
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
        assert!(!s.wants_probe(t0), "due but not on screen: no watch yet");
        assert!(!s.on_displaced(t0), "not raised: nothing to displace");
        assert!(s.raise_allowed(t0), "the first raise is always allowed");
        s.on_raised(t0);
        assert_eq!(s.phase(), CardPhase::Watching { since: t0 });
        assert!(s.wants_probe(t0 + Duration::from_secs(60)));

        // DISPLACED by another producer within the hold: back to Due, and
        // the anchor is the FIRST raise, so a second raise does not extend it.
        let t1 = t0 + Duration::from_secs(5);
        assert!(s.on_displaced(t1));
        assert_eq!(s.phase(), CardPhase::Due);
        assert!(!s.wants_probe(t1), "waiting for the slot: the watch pauses");
        assert!(s.raise_allowed(t1));
        s.on_raised(t1);
        assert_eq!(s.phase(), CardPhase::Watching { since: t1 });
        let late = t0 + RERAISE_WITHIN;
        assert!(!s.on_displaced(late), "past its own hold: it had its turn");
        assert_eq!(s.phase(), CardPhase::Watching { since: t1 });

        // A card displaced LATE in its hold goes to Due but may not come back
        // past the window: the raise is refused and the card settles.
        let mut d = CardState::new();
        assert!(d.begin_deciding(Instant::now()));
        assert!(d.on_decided(&Verdict::Offer, t0));
        d.on_raised(t0);
        assert!(d.on_displaced(t0 + RERAISE_WITHIN - Duration::from_secs(1)));
        assert_eq!(d.phase(), CardPhase::Due);
        assert!(!d.raise_allowed(t0 + RERAISE_WITHIN + Duration::from_secs(1)));
        assert_eq!(d.phase(), CardPhase::Settled);

        // THE GRANT SEEN WHILE ANOTHER CARD HOLDS THE SLOT: the confirmation
        // waits like the card does, and gives up only after its patience.
        let mut g = CardState::new();
        assert!(g.begin_deciding(Instant::now()));
        assert!(g.on_decided(&Verdict::Offer, t0));
        g.on_raised(t0);
        g.on_grant_awaiting_slot(t1);
        assert_eq!(g.phase(), CardPhase::Confirming { since: t1 });
        assert!(!g.wants_probe(t1), "observed: nothing more to probe");
        assert!(!g.on_displaced(t1), "not a card any more");
        assert!(!g.confirmation_expired(t1 + WATCH_FOR - Duration::from_secs(1)));
        assert_eq!(g.phase(), CardPhase::Confirming { since: t1 });
        assert!(g.confirmation_expired(t1 + WATCH_FOR));
        assert_eq!(g.phase(), CardPhase::Settled);
        let mut c = CardState::new();
        assert!(c.begin_deciding(Instant::now()));
        assert!(c.on_decided(&Verdict::Confirm, t0));
        assert_eq!(c.phase(), CardPhase::Confirming { since: t0 });

        // Opening Settings restarts the window and marks the owner's press:
        // no re-raise after that, however soon it is displaced.
        let t2 = t0 + Duration::from_secs(20);
        let mut o = CardState::new();
        assert!(o.begin_deciding(Instant::now()));
        assert!(o.on_decided(&Verdict::Offer, t0));
        o.on_raised(t0);
        o.on_opened_settings(t2);
        assert_eq!(o.phase(), CardPhase::Watching { since: t2 });
        assert!(!o.on_displaced(t2 + Duration::from_secs(1)));
        assert!(o.wants_probe(t2 + WATCH_FOR - Duration::from_secs(1)));
        assert!(!o.wants_probe(t2 + WATCH_FOR), "the watch is bounded");
        assert_eq!(o.phase(), CardPhase::Settled);
        o.on_opened_settings(t2);
        o.on_raised(t2);
        assert_eq!(o.phase(), CardPhase::Settled, "settled stays settled");

        // A body press: dismissed for now, watch continues, never re-raised.
        let mut b = CardState::new();
        assert!(b.begin_deciding(Instant::now()));
        assert!(b.on_decided(&Verdict::Offer, t0));
        b.on_raised(t0);
        b.on_owner_acted();
        assert!(b.wants_probe(t0 + Duration::from_secs(1)));
        assert!(!b.on_displaced(t0 + Duration::from_secs(1)));

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
        assert!(!w.on_displaced(t0));
        assert_eq!(CardState::default(), CardState::new());
    }
}
