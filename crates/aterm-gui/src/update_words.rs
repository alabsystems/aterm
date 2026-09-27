// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE UPDATE LANE'S WORDS — every sentence aterm's self-update says on the
//! message band or in the message log, as PURE builders that return an
//! [`aterm_messages::Message`] (or a [`Restatement`]) for the message center
//! (docs/DESIGN-unified-messages-2026-09-21.md §3.1, §6 R35–R38, rulings
//! 56–59 and 143). Nothing here keeps state, reads a clock or paints a cell:
//! the two clock helpers take both clocks as arguments.
//!
//! # One update, one row, a title per phase (ruling 143)
//!
//! From the first downloaded byte to the switch the update is ONE row — the
//! lane's flow row, keyed [`KEY_PROGRESS`], and what it IS is the host's
//! state (`App::update_flow`), never its words (ruling 57). Its title takes
//! §10's form, verb first and a title per phase: `Downloading aterm vX` (a
//! fill with its ETA, or busy with the bytes so far), `Checking aterm vX`
//! (busy), `Installing aterm vX` for the switch, and the successor's
//! `Finishing aterm vX`. Every phase word rides behind `Details ›` and in the
//! log ([`Message::no_excerpt`], ruling 77). The landing is the flow row's
//! Complete echo and a RECORD in the old row's words ([`landed`]: "Updated to
//! aterm vX").
//!
//! A STAGED build the automatic lane installs by itself takes NO row: its
//! words are a RECORD, and the download row, if one is up, ends in its
//! Complete echo (the owner's silent path, 2026-09-24,
//! DESIGN-atpkg-vendor-direct-updates §5.3(d)). Where a PRESS is how the
//! build installs — the lane is off, or has stopped — the staged row is the
//! READY row ("aterm vX is ready") with the `Install now` capsule
//! ([`apply_capsule_for`]); every other posture is a record too. The words
//! never describe a press (the capsule is the affordance).
//!
//! # What may be claimed
//!
//! The rows render [`aterm_update::Progress`] as the updater reported it from
//! inside its own check — download bytes are the `.part` file's size against
//! the release asset's declared size (0 ⇒ a busy row with the bytes so far,
//! honestly). For a STAGED build they also state the apply posture the App
//! computed ([`ApplyPosture`]) — how the build will install, never a restart:
//! the mechanism is the in-session overlap handoff and the shells keep
//! running.
//!
//! # Keys
//!
//! The lane's own progress — downloading, checking, staged, installing,
//! finishing — is ONE row ([`KEY_PROGRESS`]): each new phase supersedes the
//! last in its slot and the log keeps every one (the times that answer
//! "downloaded but didn't install"). An apply-lane OUTCOME the person acts on
//! ([`KEY_OUTCOME`]) and the check-health warning ([`KEY_HEALTH`]) are rows
//! of their own, so an outcome never covers the flow row. Every outcome the
//! lane answers by itself — a retry it scheduled, a download it will fetch
//! again — and the lane's durable facts (downloaded, the switch started or
//! stopped, how long the update took, a download postponed, the health
//! healed, the landing) are [`Hold::LogOnly`] records (ruling 76).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_messages::{
    Amount, DETAIL_LINE_CAP, Glyph, HOLD_STAGED_MANUAL, Hold, Intent, Message, Meter,
    PROGRESS_GRACE, Restatement, STALE_HANDOFF, STALE_UPDATE, Severity, Unit, tags,
};
use atpkg::progress::sanitize_for_tty;

use crate::app_update_handoff::HandoffUnavailable;
use crate::toolchain_words::{fill_permille, fmt_bytes};

/// The supersede key of the lane's own flow row: downloading → checking →
/// staged → installing → finishing, each replacing the last.
pub(crate) const KEY_PROGRESS: &str = "update.progress";
/// An apply-lane outcome's key (R37) — never [`KEY_PROGRESS`]: both rows
/// stay live, side by side.
pub(crate) const KEY_OUTCOME: &str = "update.outcome";
/// The check-health warning's key (R38).
pub(crate) const KEY_HEALTH: &str = "update.health";

/// The glyph of an update in progress — the flow row's from the first byte
/// to the switch. `↻` is "working", `✓` is "done", `⚠` is "needs you".
const FLOW_GLYPH: char = '\u{21bb}';
/// `✓` — the ready row and the landing's record.
const READY: char = '\u{2713}';
/// `⚠` — an outcome the person acts on.
const NEEDS_YOU: char = '\u{26a0}';

/// How a STAGED build will be applied — what the staged row's detail line may
/// promise. Computed by the App (`App::apply_posture_for`), which is the only
/// holder of the policy, the environment vetoes and the lane's own state; the
/// row just says it. The mechanism behind every arm is the in-session overlap
/// handoff — the successor adopts every window, tab, split and live shell — so
/// none of them asks for a restart; the last arm is the one where that handoff
/// has been switched off, and there the honest line is that nothing installs
/// while a terminal is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyPosture {
    /// `update.auto_apply` on (the default) and no veto: the seamless lane lands
    /// it at the first quiet moment, and no later than the ladder's bound
    /// (`native_update_auto_intent::LANDS_WITHIN`, within a minute) whatever the
    /// terminal does.
    Automatic,
    /// `[update] auto_apply = false`.
    ManualByConfig,
    /// An environment veto for this run: only the `ATERM_DEBUG_RELAUNCH_NUDGE`
    /// screenshot seam, a development seam no shipped binary reads (the user veto
    /// `ATERM_NO_AUTO_APPLY` is gone, 2026-09-23 — `[update] auto_apply` is the switch).
    VetoedByEnv { var: &'static str },
    /// The automatic lane stood down for THIS build after a physical handoff
    /// failure. `lapses`: the schedule carries a retry deadline and re-arms by
    /// itself; otherwise it has converged and only a person moves it.
    ManualOnlyLatched { lapses: bool },
    /// The in-session handoff cannot run in this process — `why` names the
    /// reason ([`HandoffUnavailable`]: a control socket this process does not own,
    /// `--headless`, no event loop), read from the SAME predicate the apply
    /// gate reads (`App::seamless_handoff_unavailable`). That is NOT "a cold
    /// re-exec instead": the admission classifier
    /// (`native_update_admission::classify`) admits the cold lane only with
    /// zero live PTYs and REFUSES the apply while any terminal is open — no
    /// shell dies, nothing installs. With the automatic lane armed (`veto:
    /// None`) the update lands once every terminal is closed; with it vetoed
    /// (`veto` names what stands in the way) that closed-terminals landing is a
    /// promise the lane never arms, so the sentence names the veto and the
    /// menu instead. It outranks every other arm, because the others change
    /// WHEN the build installs and this one changes WHETHER.
    HandoffDisabled {
        why: HandoffUnavailable,
        veto: Option<AutoApplyVeto>,
    },
}

/// Why the AUTOMATIC apply lane would not arm even if the handoff were
/// available — carried into [`ApplyPosture::HandoffDisabled`] so its fallback
/// clause cannot promise the install "once every terminal is closed" over a
/// lane that never attempts it. Same precedence as the standalone arms:
/// config, then the screenshot seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoApplyVeto {
    /// `[update] auto_apply = false`.
    Config,
    /// The `$ATERM_DEBUG_RELAUNCH_NUDGE` screenshot seam (a development build's).
    Env { var: &'static str },
}

impl AutoApplyVeto {
    /// The clause the sentence leads the fallback with — what the reader can
    /// act on, worded like the standalone arms for the same states.
    fn clause(self) -> String {
        match self {
            Self::Config => "automatic install is off".to_string(),
            Self::Env { var } => format!("${var} is set"),
        }
    }
}

/// The manual affordance, named the same way on every surface — the Version
/// menu's row is "Install aterm vX now" (`menu::staged_apply_label`). Off macOS
/// the Version menu is the palette's Version section and the in-grid tab strip
/// carries the `↻`.
pub(crate) const INSTALL_FROM_MENU: &str = if cfg!(target_os = "macos") {
    "install it from the Version menu"
} else {
    "install it from the Version menu or the \u{21bb} button"
};

/// The flow row's phase words, point first and at most 40 characters each —
/// what is happening now, in the order it happens
/// (`every_flow_detail_is_short_and_point_first`). They ride behind
/// `Details ›` and in the log; the title already says the phase. Never "build
/// N", "verified", "staging" or "in place" (2026-09-23 audit: internal words).
pub(crate) const DOWNLOADING: &str = "downloading";
/// The container arrived: the size, digest, signature and Gatekeeper checks
/// and the stage publish, seconds each.
pub(crate) const CHECKING: &str = "checking the download";
/// Staged, and the automatic lane will install it by itself — the staged
/// RECORD's detail: the ladder lands it within a minute of arming, whatever the
/// terminal does (`native_update_auto_intent::LANDS_WITHIN` plus the switch,
/// asserted there to be at most a minute). In words, never `LANDS_WITHIN / 60`
/// arithmetic: that printed "within 1 min", and "within 0 min" for any bound
/// under a minute.
pub(crate) const INSTALLS_BY_ITSELF: &str = "installs by itself within a minute";
/// The outgoing process is handing over.
#[cfg(any(unix, test))]
pub(crate) const INSTALLING: &str = "installing\u{2026}";
/// The successor's frames before Commit: what is typed now is queued and
/// replayed, never lost — the one sentence about the person's work, said
/// once, behind `Details ›` (it changes nothing the person does).
pub(crate) const FINISHING: &str = "almost done \u{2014} what you type is kept";
/// The lane's ONE promise that it retries, said one way on every surface
/// (audit 2026-09-24: five phrasings of one promise), with ` later` or ` in
/// 6 h` after it where a time is known.
pub(crate) const TRIES_AGAIN: &str = "tries again by itself";
/// A download that failed: the next check retries it.
pub(crate) const DOWNLOAD_FAILED: &str = TRIES_AGAIN;

/// The `Software Update` capsule: the page that holds the durable record.
fn software_update() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::SoftwareUpdate
            .path()
            .to_string(),
    }
}

/// A glyph from the band's closed set; every glyph named here is in it.
fn glyph(ch: char) -> Glyph {
    Glyph::or_fallback(ch)
}

/// An update-lane row.
fn row(severity: Severity, title: impl AsRef<str>) -> Message {
    Message::new(tags::UPDATE, severity, title)
}

/// A version as the rows print it: sanitized, bounded.
fn v(version: &str) -> String {
    sanitize_for_tty(version, 32)
}

/// A version as every update title and record names it: `aterm v0.91.0` —
/// one spelling, never a bare `aterm 0.91.0` beside it (audit 2026-09-24).
pub(crate) fn aterm_v(version: &str) -> String {
    format!("aterm v{}", v(version))
}

/// A phase title, verb first (§10's form, ruling 143): "Downloading aterm
/// v0.91.0". Says "Installing update" when there is no version (the re-exec
/// QA seam applies with none — a bare "aterm v" was measured on glass
/// 2026-09-07).
pub(crate) fn flow_title(verb: &str, version: &str) -> String {
    let v = v(version);
    if v.trim().is_empty() {
        format!("{verb} update")
    } else {
        format!("{verb} {}", aterm_v(&v))
    }
}

/// The flow row: its phase title, the working glyph, `phase` behind
/// `Details ›`, a meter (a fill, or busy), `hold`, the progress key.
fn flow(title: String, phase: &str, meter: Meter, hold: Hold) -> Message {
    row(Severity::Info, title)
        .glyph(glyph(FLOW_GLYPH))
        .line(phase)
        .no_excerpt()
        .meter(meter)
        .hold(hold)
        .key(KEY_PROGRESS)
}

/// The READY row's title, where a press or closed terminals install the build
/// rather than the lane: "aterm vX is ready". The press affordance is the
/// `Install now` CAPSULE ([`apply_capsule_for`]), so the words are the same
/// under every posture.
pub(crate) fn staged_title(version: &str) -> String {
    format!("{} is ready", aterm_v(version))
}

/// The `Install now` capsule ONLY where a press is the way the build installs
/// (2026-09-18): not where the handoff is off (a press can only open the
/// details page), and not where the lane installs by itself — a row that
/// asked "click to apply now" for an update the automatic lane would land
/// within seconds read as an instruction, the owner clicked, and the click
/// took the explicit lane's immediate freeze. A caller that computed no
/// posture (`None`) keeps the manual affordance: no shipping caller passes
/// `None` for a Staged report (`note_update_progress` always computes one),
/// and its paired detail ([`staged_detail_unknown`]) names only the menu.
pub(crate) fn apply_capsule_for(posture: Option<ApplyPosture>, build: u64) -> Option<Intent> {
    match posture {
        Some(
            ApplyPosture::HandoffDisabled { .. }
            | ApplyPosture::Automatic
            | ApplyPosture::ManualOnlyLatched { lapses: true },
        ) => None,
        _ => Some(Intent::ApplyUpdate { build }),
    }
}

/// Whether a staged build is a DECISION — the ready row whose `Install now`
/// is how it installs — exactly where [`apply_capsule_for`] offers the
/// capsule (design §10.5 H2).
pub(crate) fn staged_is_decision(posture: Option<ApplyPosture>) -> bool {
    apply_capsule_for(posture, 0).is_some()
}

/// The staged row's detail: how the build installs, point first. One sentence
/// per posture, each true of the mechanism it names and none of them a
/// restart. The build number is `appstatus`'s and Settings' to show, never the
/// glass's; the press is the capsule's.
#[must_use]
pub(crate) fn staged_detail(posture: ApplyPosture) -> String {
    match posture {
        ApplyPosture::Automatic => INSTALLS_BY_ITSELF.to_string(),
        ApplyPosture::ManualByConfig => "automatic install is off".to_string(),
        ApplyPosture::VetoedByEnv { var } => format!("${var} is set \u{2014} {INSTALL_FROM_MENU}"),
        ApplyPosture::ManualOnlyLatched { lapses: true } => {
            format!("didn't install \u{2014} {TRIES_AGAIN} later")
        }
        ApplyPosture::ManualOnlyLatched { lapses: false } => {
            "automatic install didn't work".to_string()
        }
        // The load-bearing clause comes first: the width law cuts the detail
        // from the right when the window is narrow. Nothing installs while a
        // terminal is open (the admission classifier refuses it), so that is
        // the promise, and the only one.
        ApplyPosture::HandoffDisabled { why, veto: None } => format!(
            "{} \u{2014} installs once every terminal is closed",
            why.cause()
        ),
        // With the automatic lane ALSO vetoed, "installs once every terminal is
        // closed" would promise a landing the lane never arms: name the veto
        // and the menu instead.
        ApplyPosture::HandoffDisabled {
            why,
            veto: Some(veto),
        } => format!(
            "{}; {} \u{2014} {INSTALL_FROM_MENU} once every terminal is closed",
            why.cause(),
            veto.clause()
        ),
    }
}

/// The Staged line for a caller that computed no posture: only the manual
/// affordance, which is true whatever the policy — never a promise about the
/// automatic lane, and never "automatic install is off" painted over a lane
/// that may be armed.
pub(crate) fn staged_detail_unknown() -> String {
    INSTALL_FROM_MENU.to_string()
}

/// The staged sentence as the row's detail LINES: one line when it fits the
/// line cap (every sentence today does — the longest, the handoff-off one
/// with the config veto, is about 150 chars), else split at its joints with
/// every word kept ([`aterm_messages::text::split_sentence`]).
pub(crate) fn staged_detail_lines(posture: Option<ApplyPosture>) -> Vec<String> {
    let sentence = posture.map_or_else(staged_detail_unknown, staged_detail);
    aterm_messages::text::split_sentence(&sentence, DETAIL_LINE_CAP)
}

/// R35 (Staged) — a build is staged and verified. Two answers (ruling 143, as
/// amended 2026-09-24 by the owner's silent path, §5.3(d)):
///
/// * a press installs it ([`staged_is_decision`]): the READY row, `aterm vX is
///   ready`, Info with `✓`, how it installs behind `Details ›`, the
///   `Install now` capsule, held [`HOLD_STAGED_MANUAL`] — raised once per
///   build and posture by the host (ruling 119);
/// * anything else — the automatic lane that lands it within a minute, a
///   stand-down that retries later, the handoff off — is a RECORD with the
///   posture's sentence verbatim: it lands by itself (or once the terminals
///   close), and nothing on the glass could be pressed. No row, no re-grid;
///   the Version menu's ⬆️ says it is there.
///
/// The App re-states the words once the lane has actually armed or stood
/// down for this build ([`restate_apply_posture`]).
pub(crate) fn staged(version: &str, build: u64, posture: Option<ApplyPosture>) -> Message {
    let title = staged_title(version);
    match apply_capsule_for(posture, build) {
        Some(install) => row(Severity::Info, title)
            .glyph(glyph(READY))
            .lines(staged_detail_lines(posture))
            .no_excerpt()
            .action(install)
            .hold(Hold::For(HOLD_STAGED_MANUAL))
            .key(KEY_PROGRESS),
        None => row(Severity::Success, title)
            .glyph(glyph(READY))
            .lines(staged_detail_lines(posture))
            .hold(Hold::LogOnly)
            .key(KEY_PROGRESS),
    }
}

/// Re-state HOW a staged `build` installs on the live ready row — the
/// refinement the App posts once the lane has actually armed (or stood down)
/// for it, a moment after the `Staged` report said the policy line: the FULL
/// words of [`staged`] under `posture` — title, glyph, tone, detail, capsule
/// and lifetime (the center re-anchors a held row on glass). A posture whose
/// words are a RECORD is not a restatement: the host folds the row and
/// records them (`App::restate_staged_bar_posture`). The host applies it only
/// to the flow state's staged row for this build (`App::update_flow`), never
/// to the switch's row that replaces it.
pub(crate) fn restate_apply_posture(
    version: &str,
    build: u64,
    posture: ApplyPosture,
) -> Restatement {
    crate::messages_host::restatement_of(&staged(version, build, Some(posture)))
}

/// R35 — one report from inside the updater's own check
/// ([`aterm_update::Progress`]). Only DOWNLOAD-and-later phases raise a row:
/// a check that finds nothing to do (the common case) never reaches here and
/// never moves the grid. `posture` is how a STAGED build will be installed,
/// as the App computed it (`App::apply_posture_for`) — read only by the
/// `Staged` arm; `flow_version` is the version the live flow row names, which
/// a failed or postponed download's report does not carry.
///
/// Downloading and checking are PROGRESS (design §10.3 U1, U2): the download's
/// bytes are its [`Amount`] (the ETA's only input), and both wait out the
/// progress grace, so a fast download never touches the glass. A failed or
/// postponed download is a RECORD: the next check fetches it again by itself,
/// and the health lane escalates a failure that persists.
pub(crate) fn progress(
    p: &aterm_update::Progress,
    posture: Option<ApplyPosture>,
    flow_version: &str,
) -> Message {
    use aterm_update::Progress as P;
    match p {
        P::Downloading {
            version,
            bytes_done,
            bytes_total,
        } => {
            // A known total is a fill; an unknown one is honest motion with
            // the bytes so far — never an invented fraction.
            let meter = if *bytes_total > 0 {
                let done = (*bytes_done).min(*bytes_total);
                Meter {
                    fill_permille: fill_permille(done, *bytes_total),
                    stats: crate::toolchain_words::byte_stats(done, *bytes_total),
                    amount: Some(Amount {
                        series: Amount::series_of(version),
                        done,
                        total: *bytes_total,
                        unit: Unit::Bytes,
                    }),
                    load: None,
                    busy: false,
                    level: false,
                }
            } else {
                Meter::busy(fmt_bytes(*bytes_done))
            };
            flow(
                flow_title("Downloading", version),
                DOWNLOADING,
                meter,
                Hold::Live {
                    stale_after: STALE_UPDATE,
                },
            )
            .reveal_after(PROGRESS_GRACE)
        }
        // What a finished check proved (ruling 154): `Checked aterm vX` said
        // only that it looked.
        P::Verifying { version } => flow(
            flow_title("Checking", version),
            CHECKING,
            Meter::busy(""),
            Hold::Live {
                stale_after: STALE_UPDATE,
            },
        )
        .finished_as(flow_title("Verified", version))
        .reveal_after(PROGRESS_GRACE),
        P::Staged { version, build } => staged(version, *build, posture),
        P::Failed { detail } => download_failed(flow_version, detail),
        P::Deferred { detail } => download_postponed(detail),
    }
}

/// A download that FAILED, on record (ruling 143: the next check retries it —
/// a self-retry is a record, and the flow row it ends is the Fault echo): the
/// title by outcome, "Couldn't download aterm vX", "will try again by itself",
/// and the updater's own sentence WHOLE — never pre-cut to a width (design
/// ruling 64).
pub(crate) fn download_failed(flow_version: &str, detail: &str) -> Message {
    let v = v(flow_version);
    let title = if v.trim().is_empty() {
        "Couldn't download the update".to_string()
    } else {
        format!("Couldn't download aterm v{v}")
    };
    row(Severity::Warn, title)
        .glyph(glyph(FLOW_GLYPH))
        .line(DOWNLOAD_FAILED)
        .sentence(detail)
        .hold(Hold::LogOnly)
}

/// A download the updater POSTPONED (a metered network, a busy machine): no
/// row — the lane will come back to it — a RECORD with the updater's whole
/// sentence ("Update download postponed").
pub(crate) fn download_postponed(detail: &str) -> Message {
    row(Severity::Info, "Update download postponed")
        .glyph(glyph(FLOW_GLYPH))
        .sentence(detail)
        .hold(Hold::LogOnly)
}

/// R36 — THE SWITCH BEGINS: the outgoing process is about to park its readers.
/// `Installing aterm vX`, busy — posted over the live flow row (same key), or
/// as the lane's only row on an explicit install with nothing up — under the
/// handoff's staleness cap: the readiness deadline that bounds the whole
/// attempt is env-clamped to 120 s, and every path that ends the freeze
/// replaces the row explicitly — the cap is the backstop, not the lifetime.
#[cfg(any(unix, test))]
pub(crate) fn installing(version: &str) -> Message {
    flow(
        flow_title("Installing", version),
        INSTALLING,
        Meter::busy(""),
        Hold::Live {
            stale_after: STALE_HANDOFF,
        },
    )
}

/// R36 — THE SUCCESSOR, before Commit: it inherited the parent's flow row
/// across the carry and says its own phase in it — `Finishing aterm vX`,
/// busy; what is typed now is kept ([`FINISHING`], behind `Details ›`). The
/// host `restate`s the carried row with these words; `version` is the running
/// build's. Its Complete echo — the landing's moment — says `Installed aterm
/// vX` (ruling 154): `Finished aterm vX` named no deed, and main's `Updated
/// to aterm vX` is a cell wider than the title it replaces, so it stays the
/// record's words ([`landed`]).
pub(crate) fn finishing(version: &str) -> Message {
    flow(
        flow_title("Finishing", version),
        FINISHING,
        Meter::busy(""),
        Hold::Live {
            stale_after: STALE_HANDOFF,
        },
    )
    .finished_as(flow_title("Installed", version))
}

/// R36 — THE NEW BUILD TOOK OVER (or a cold-lane boot found it already
/// running): a RECORD (ruling 141) — the flow row's Complete echo, where one
/// was up, is the moment. "✓ Updated to aterm vX" ("Updated to build
/// N" with no version), no claim about the shells. `took` — how long the
/// update took from THIS build's finished download, when this process (or
/// the predecessor that handed over) downloaded this very build — is a line
/// of the same record: one landing, one entry (audit 2026-09-24).
///
/// A REPAINT IS ON THE RECORD (the 2026-09-22/23 update audit, plan P1-5):
/// when the successor adopted `repainted` sessions onto a blank screen (the
/// seamless lane's `IncomingHandoff::repainted_tabs`; always 0 on the cold
/// lane, which carried no screen), the record names how many and why those
/// tabs came over blank. Until then the only word was a WARN line in the log,
/// and the user found a blank tab with no word about why. The record, not a
/// row: the landing is one of the lane's durable facts (ruling 76, ruling
/// 141), and a blank tab is nothing for the person to do — the handoff sent
/// it a size pulse (`spawn::adoption_needs_size_pulse`), so its program
/// redraws its own screen.
pub(crate) fn landed(
    version: &str,
    build: u64,
    repainted: usize,
    took: Option<Duration>,
) -> Message {
    let title = if v(version).trim().is_empty() {
        format!("Updated to build {build}")
    } else {
        format!("Updated to {}", aterm_v(version))
    };
    let mut msg = row(Severity::Success, title)
        .glyph(glyph(READY))
        .hold(Hold::LogOnly);
    if let Some(took) = took {
        msg = msg.line(format!("installed {} after the download", span_words(took)));
    }
    match repainted_words(repainted) {
        Some(words) => msg.line(words).sentence(if repainted == 1 {
            "its screen was blank after the update; the program in it was asked to redraw"
        } else {
            "their screens were blank after the update; the programs in them were asked to \
             redraw"
        }),
        None => msg,
    }
}

/// R39 — AN UPDATE THAT COULD NOT CARRY EVERY TAB'S SCROLLBACK (2026-09-26).
/// An in-session update carries each tab's whole history
/// (`crate::handoff_history`); a history that changed under its export,
/// outran it, or whose sidecar did not arrive whole crossed with only the
/// screen carry's newest lines. The lines left behind are counted per tab —
/// `status` says each tab's running `history_lost=` — and said HERE, once per
/// update, as a FAILURE: nothing to press, and a loss is never only a log
/// line (it was, until this row: 30293eb3a). The count rides the detail,
/// and the sentence says what did come across and where the per-tab count is.
pub(crate) fn scrollback_lost(lines: u64, tabs: usize) -> Message {
    row(Severity::Warn, "Couldn't carry all scrollback")
        .glyph(glyph(NEEDS_YOU))
        .line(scrollback_lost_words(lines, tabs))
        .sentence(
            "everything on screen and the newest lines came across; `aterm ctl status` in a \
             tab says history_lost= for it",
        )
        .hold(Hold::Default)
        .key(KEY_SCROLLBACK)
}

/// The supersede key of R39: one scrollback row per update.
pub(crate) const KEY_SCROLLBACK: &str = "update.scrollback";

/// "1 older line stayed behind in 1 tab" / "12345 older lines stayed behind
/// in 3 tabs".
fn scrollback_lost_words(lines: u64, tabs: usize) -> String {
    let lines = if lines == 1 {
        "1 older line".to_string()
    } else {
        format!("{lines} older lines")
    };
    let tabs = if tabs == 1 {
        "1 tab".to_string()
    } else {
        format!("{tabs} tabs")
    };
    format!("{lines} stayed behind in {tabs}")
}

/// "1 tab repainted" / "3 tabs repainted", or `None` for a handoff that
/// carried every screen exactly.
fn repainted_words(repainted: usize) -> Option<String> {
    match repainted {
        0 => None,
        1 => Some("1 tab repainted".to_string()),
        n => Some(format!("{n} tabs repainted")),
    }
}

/// A span as the durable record says it: "42 s", "1 min", "3 min 5 s", "2 h",
/// "2 h 10 min".
pub(crate) fn span_words(span: Duration) -> String {
    let secs = span.as_secs();
    if secs < 60 {
        format!("{secs} s")
    } else if secs < 3600 {
        match secs % 60 {
            0 => format!("{} min", secs / 60),
            rest => format!("{} min {rest} s", secs / 60),
        }
    } else {
        match secs % 3600 / 60 {
            0 => format!("{} h", secs / 3600),
            rest => format!("{} h {rest} min", secs / 3600),
        }
    }
}

/// When `build` — the one a handoff is about to install — finished
/// downloading in this process (`verified`, `App::update_verified`), on the
/// wall clock in Unix milliseconds: what the handoff carry hands the successor
/// (`WindowCarry::update_verified_unix_ms`). `None` for any other build.
/// `now` and `wall` are one reading of the two clocks.
#[cfg(any(unix, test))]
pub(crate) fn verified_unix_ms(
    verified: Option<(u64, Instant)>,
    build: u64,
    now: Instant,
    wall: SystemTime,
) -> Option<u64> {
    let (verified, at) = verified?;
    if verified != build {
        return None;
    }
    let at = wall.checked_sub(now.saturating_duration_since(at))?;
    let ms = at.duration_since(UNIX_EPOCH).ok()?.as_millis();
    u64::try_from(ms).ok()
}

/// THE SUCCESSOR, running `build`: the predecessor's [`verified_unix_ms`] for
/// it, placed on this process's clock (`now` and `wall` one reading of the
/// two), so the landing it records says how long the whole update took.
/// `None` — an older build's carry, a predecessor that did not download this
/// build itself — leaves nothing to measure from.
pub(crate) fn carried_verification(
    unix_ms: Option<u64>,
    build: u64,
    now: Instant,
    wall: SystemTime,
) -> Option<(u64, Instant)> {
    let at = UNIX_EPOCH.checked_add(Duration::from_millis(unix_ms?))?;
    let ago = wall.duration_since(at).unwrap_or(Duration::ZERO);
    now.checked_sub(ago).map(|at| (build, at))
}

/// THE SWITCH TO A NEW VERSION BEGINS, on record — written before the park,
/// because a line queued for after the process execs is a line never written.
/// `target` is the version being installed; `None` is a same-image switch (a
/// reload of this very build), which installs nothing whatever is staged.
#[cfg(any(unix, test))]
pub(crate) fn switch_started(target: Option<&str>, running: &str) -> Message {
    let title = target.map_or_else(
        || "Reloading aterm in place".to_string(),
        |version| flow_title("Installing", version),
    );
    row(Severity::Info, title)
        .glyph(glyph(FLOW_GLYPH))
        .line(format!("from {}", aterm_v(running)))
        .hold(Hold::LogOnly)
}

/// THE SWITCH THAT DID NOT HAPPEN, on record: an attempt [`switch_started`]
/// wrote down ended with this process still running — refused, failed, or
/// stood down because the terminal was in use (`routine`) — so the record
/// never leaves an "Installing" that was not. `why` is the attempt's own
/// account, whole.
#[cfg(any(unix, test))]
pub(crate) fn switch_stopped(
    target: Option<&str>,
    running: &str,
    routine: bool,
    why: &str,
) -> Message {
    let running = aterm_v(running);
    let (title, detail) = match (target, routine) {
        (Some(version), true) => (
            format!("Waiting to install {}", aterm_v(version)),
            format!("you were typing, so {running} kept running; it {TRIES_AGAIN}"),
        ),
        (Some(version), false) => (
            format!("{} was not installed", aterm_v(version)),
            format!("{running} kept running: {why}"),
        ),
        (None, true) => (
            "Waiting to reload aterm".to_string(),
            format!("you were typing, so aterm kept running; it {TRIES_AGAIN}"),
        ),
        (None, false) => (
            "aterm was not reloaded".to_string(),
            format!("aterm kept running: {why}"),
        ),
    };
    row(
        if routine {
            Severity::Info
        } else {
            Severity::Warn
        },
        title,
    )
    .glyph(glyph(if routine { FLOW_GLYPH } else { NEEDS_YOU }))
    .sentence(detail)
    .hold(Hold::LogOnly)
}

/// R37 — an apply-lane OUTCOME the lane answers BY ITSELF, on record (ruling
/// 143): a retry it scheduled, a blocker that clears by itself, a person's
/// attempt the lane will make again. The words are main's (ruling 68); the
/// glass is the flow row's echo, never a second row. A warning keeps the
/// `Software Update` capsule for the page's re-offer. The whole sentence is
/// kept (design ruling 64).
pub(crate) fn outcome(glyph_ch: char, title: &str, detail: &str, severity: Severity) -> Message {
    let msg = row(severity, sanitize_for_tty(title, 80))
        .glyph(glyph(glyph_ch))
        .sentence(detail)
        .hold(Hold::LogOnly)
        .key(KEY_OUTCOME);
    if severity == Severity::Warn {
        msg.action(software_update())
    } else {
        msg
    }
}

/// R37 — the lane STOPPED and only a press moves the build (ruling 143): a
/// DECISION row, `Install now` (`Intent::ApplyUpdate`) as its capsule — the
/// Version menu's own affordance, on the glass — main's words for the title
/// ("Couldn't install aterm vX", "Update didn't install", "Update
/// installed"). Where else the press lives rides behind `Details ›` on
/// every tone — the capsule already says it (ruling 77; ruling 161). Warn
/// where an attempt failed (`⚠`); Info where the build is on disk and waits
/// for a press — main's ruling-68 row, `↻ Update installed`: the WORKING
/// glyph, since a done-mark beside `Install now` read as finished while
/// asking to install (review 2026-09-24). The tone's hold.
pub(crate) fn needs_install(title: &str, detail: &str, severity: Severity, build: u64) -> Message {
    needs_install_because(title, None, detail, severity, build)
}

/// [`needs_install`] with the failure's SHORT CAUSE painted beside the
/// title, where one is known (`Couldn't install aterm v0.92.0 · disk full`,
/// design ruling 246): it changes what the person does before pressing
/// `Install now`. A lane that TRIES AGAIN BY ITSELF asks nothing of the
/// person — a record, never a row (the owner: "FYI/CYA messages need to go
/// to the log"); the health lane raises `aterm can't install updates` if the
/// retries keep failing.
pub(crate) fn needs_install_because(
    title: &str,
    cause: Option<&str>,
    detail: &str,
    severity: Severity,
    build: u64,
) -> Message {
    if detail.contains(TRIES_AGAIN) {
        return outcome(FLOW_GLYPH, title, detail, Severity::Info);
    }
    let msg = row(severity, sanitize_for_tty(title, 80));
    let msg = match cause {
        Some(cause) => msg.line(cause).sentence(detail),
        None => msg.sentence(detail).no_excerpt(),
    }
    .action(Intent::ApplyUpdate { build })
    .key(KEY_OUTCOME);
    if severity >= Severity::Warn {
        msg.glyph(glyph(NEEDS_YOU))
    } else {
        msg.glyph(glyph(FLOW_GLYPH))
            .hold(Hold::For(HOLD_STAGED_MANUAL))
    }
}

/// The few words a failure's own message comes down to, where they change
/// what the person does (design ruling 246): `disk full`, `no permission`,
/// `read-only disk`; `None` for anything else, whose account stays behind
/// `Details ›` and in the log.
pub(crate) fn short_cause(message: &str) -> Option<&'static str> {
    let m = message.to_ascii_lowercase();
    if m.contains("no space left") || m.contains("enospc") || m.contains("disk full") {
        Some("disk full")
    } else if m.contains("read-only file system") || m.contains("erofs") {
        Some("read-only disk")
    } else if m.contains("permission denied")
        || m.contains("operation not permitted")
        || m.contains("eacces")
        || m.contains("eperm")
    {
        Some("no permission")
    } else {
        None
    }
}

/// R37 — an attempt that FAILED and that nothing retries, a FAILURE row
/// (ruling 143, main's "Update didn't finish"): Warn, `⚠`, the warning's
/// hold, the `Software Update` capsule — the page that says what happened;
/// a mechanism's account rides behind `Details ›`. A blocker the person can
/// clear (`actionable`) is the other way round: its words are painted, and
/// they ARE the press — a capsule that cannot clear it only took the columns
/// they need (2026-09-24: at 80 columns "Save or close the…", at 60 nothing).
pub(crate) fn failed(title: &str, detail: &str, actionable: bool) -> Message {
    let msg = row(Severity::Warn, sanitize_for_tty(title, 80))
        .glyph(glyph(NEEDS_YOU))
        .sentence(detail)
        .key(KEY_OUTCOME);
    if actionable {
        msg
    } else {
        msg.action(software_update()).no_excerpt()
    }
}

/// R38 — THE CHECK-HEALTH WARNING. The updater's ledger says one half of the
/// lane is PERSISTENTLY failing, and the title says which in plain words
/// ("aterm can't install updates" / "can't download" / "can't check",
/// `aterm_update::health_failing_title`). The excerpt is since when ("since
/// Sep 14"); the updater's whole sentence — the count, the cause, the command
/// — is the further detail behind `Details ›` (the backticked command
/// survives the width law), and `Software Update` is the capsule. Held the
/// warning's 45 s, anchored at first glass — ONCE PER CLASS PER LAUNCH (the
/// updater announces a class once; a count change is a log line): a ⚠
/// standing for weeks with nothing to press is the blather the attention rule
/// moves to the log (design ruling 59). After it folds, the record, the
/// Settings headline and the OS banner carry it.
pub(crate) fn health_warning(title: &str, body: &str) -> Message {
    let msg = row(Severity::Warn, sanitize_for_tty(title, 80));
    let msg = match health_since(body) {
        Some(since) => msg.line(format!("since {since}")),
        None => msg,
    };
    msg.sentence(body)
        .action(software_update())
        .hold(Hold::Default)
        .key(KEY_HEALTH)
}

/// THE WARNING HEALED, on record — once per announced episode: what it had
/// said, so the page reads the recovery against it.
pub(crate) fn health_recovered(said_title: &str, said_line0: &str) -> Message {
    let said = if said_line0.is_empty() {
        format!("after \"{said_title}\"")
    } else {
        format!("after \"{said_title}\", {said_line0}")
    };
    row(Severity::Success, "aterm updates work again")
        .line(said)
        .hold(Hold::LogOnly)
}

/// The OS notification's body for the same warning: when it started and where
/// the rest is. The updater's whole sentence — the count, a raw timestamp, the
/// cause and a command — is the log's and Software Update's, never a banner's.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn health_notification_body(body: &str) -> String {
    match health_since(body) {
        Some(since) => format!("Since {since}. Details are in Settings \u{25b8} Software Update."),
        None => "Details are in Settings \u{25b8} Software Update.".to_string(),
    }
}

/// "Sep 14": the day the updater's sentence dates the failure from
/// ([`aterm_update::health_notice_since`]), or `None` without a readable one.
pub(crate) fn health_since(body: &str) -> Option<String> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let stamp = aterm_update::health_notice_since(body)?;
    let month: usize = stamp.get(5..7)?.parse().ok()?;
    let day: u32 = stamp.get(8..10)?.parse().ok()?;
    Some(format!("{} {day}", MONTHS.get(month.checked_sub(1)?)?))
}

/// The supersede key of a dev build's standing (gap #30): the newest standing a
/// process read replaces the last.
pub(crate) const KEY_DEV_BUILD: &str = "update.dev-build";
/// The key of a dev build's shared-writes row ([`dev_build_writes`]) — never
/// [`KEY_DEV_BUILD`]: what this copy writes is its own fact, said at launch, and no
/// reading of the channel may supersede it.
pub(crate) const KEY_DEV_BUILD_WRITES: &str = "update.dev-build-writes";

/// What a dev build writes of the shared user state only the release writes
/// unattended, in words, with what puts it back — or `None` when it writes none of it.
/// Those writers stand aside in any bundle that is not the release `aterm.app`
/// (`atpkg::hooks::runs_from_non_release_bundle`, 2026-09-23), so `non_release_bundle`
/// is `None` at once; a dev build whose bundle is NAMED `aterm.app` passes that name
/// test and runs them: its ALab tools passes (`packages_lane`, `[packages] enabled`)
/// lay the PATH links and the shell hooks, and its windows (`agents_prime`,
/// `agents_auto_prime`) write the agent primer.
pub(crate) fn dev_shared_writes(
    non_release_bundle: bool,
    packages_lane: bool,
    agents_prime: bool,
) -> Option<DevSharedWrites> {
    if non_release_bundle {
        return None;
    }
    let (what, repair) = match (packages_lane, agents_prime) {
        (true, true) => (
            "the PATH links, the shell hooks and the agent primer",
            "`aterm pkg repair` and `aterm agents install`",
        ),
        (true, false) => ("the PATH links and the shell hooks", "`aterm pkg repair`"),
        (false, true) => ("the agent primer", "`aterm agents install`"),
        (false, false) => return None,
    };
    Some(DevSharedWrites { what, repair })
}

/// What a dev build named `aterm.app` writes of the release's shared setup
/// ([`dev_shared_writes`]): `what`, in words, and `repair`, the release's verbs that
/// put it back — `aterm pkg repair` lays the PATH links and the shell hooks again, and
/// only `aterm agents install` rewrites the agent primer (`pkg repair` never touches
/// it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevSharedWrites {
    /// What it writes: `the PATH links, the shell hooks and the agent primer`.
    pub what: &'static str,
    /// What puts it back, run from the release: `` `aterm pkg repair` ``.
    pub repair: &'static str,
}

/// A DEV BUILD THAT WRITES THE RELEASE'S SETUP (gap #30): a dev-marked copy whose
/// bundle is named `aterm.app` runs the writers of shared user state only the release
/// runs unattended ([`dev_shared_writes`]). That is a failure only the person can
/// undo: a Standing ROW, saying what it writes (the painted excerpt) and what puts it
/// back. The loss — the release's setup — is the title, as the failure grammar names
/// one. It is this copy's own fact, read from its bundle and its switches when the
/// window starts, so it is said whatever the channel answers — offline, or with
/// automatic checks off — and never waits on [`dev_build`]'s reading.
pub(crate) fn dev_build_writes(writes: DevSharedWrites) -> Message {
    let DevSharedWrites { what, repair } = writes;
    row(Severity::Warn, "Release setup lost to dev build")
        .glyph(glyph(NEEDS_YOU))
        .line(format!("it writes {what}"))
        .line("this dev build's bundle is named aterm.app, which only the release may be")
        .sentence(format!(
            "rebuild it with tools/dev-app.sh, which installs it as aterm (dev).app, and remove \
             this copy, then run {repair} from the release"
        ))
        .hold(Hold::Standing)
        .key(KEY_DEV_BUILD_WRITES)
}

/// A DEV BUILD'S STANDING AGAINST THE CHANNEL (gap #30, 2026-09-26). The updater
/// leaves a dev-marked copy (`tools/dev-app.sh`) alone on purpose, so nothing said
/// that a weeks-old `aterm (dev).app` launched from the Dock was running old code;
/// the window now reads the public channel's head once at start and daily
/// (`aterm_update::dev_channel`) and says where this copy stands, in the dev
/// channel's own words ([`aterm_update::dev_channel::DevLag::words`]):
///
/// * BEHIND — `Dev build, 2 releases behind aterm v0.93.0`, a RECORD: Info, since
///   running a dev build is the owner's choice and nothing is broken, and an Info row
///   on the glass is progress or a decision only (the owner's attention rule, ruling
///   76; "FYI/CYA messages need to go to the log"). It is on Settings ▸ Messages,
///   `messages.log` and `appstatus`, and `aterm update status` says it too.
/// * AT or NEWER THAN the newest release — nothing: the watch's log line keeps it.
///
/// What a dev build WRITES of the release's shared setup is not the channel's to say:
/// [`dev_build_writes`] is its own row, said at launch whatever the channel answers.
pub(crate) fn dev_build(lag: &aterm_update::dev_channel::DevLag) -> Option<Message> {
    let standing = lag.words();
    if !lag.is_behind() {
        return None;
    }
    Some(
        row(
            Severity::Info,
            sanitize_for_tty(&format!("Dev build, {standing}"), 80),
        )
        .line("the updater leaves a dev build alone, so it stays this old until rebuilt")
        .sentence(
            "rebuild it with tools/dev-app.sh, or use the release (tools/install.sh puts it in \
             Applications)",
        )
        .hold(Hold::LogOnly)
        .key(KEY_DEV_BUILD),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_messages::{HOLD_WARN, TITLE_CAP};

    fn fill_of(msg: &Message) -> Option<u16> {
        msg.meter.as_ref().and_then(|m| m.fill_permille)
    }

    fn stats_of(msg: &Message) -> &str {
        msg.meter.as_ref().map_or("", |m| m.stats.as_str())
    }

    fn busy(msg: &Message) -> bool {
        msg.meter.as_ref().is_some_and(|m| m.busy)
    }

    /// Everything a dev build named aterm.app writes with both switches on.
    const ALL_WRITES: DevSharedWrites = DevSharedWrites {
        what: "the PATH links, the shell hooks and the agent primer",
        repair: "`aterm pkg repair` and `aterm agents install`",
    };

    /// A dev build two releases behind the channel head.
    fn dev_behind() -> aterm_update::dev_channel::DevLag {
        aterm_update::dev_channel::DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(2),
        }
    }

    /// Every standing the dev-channel watch can report.
    fn every_dev_standing() -> Vec<aterm_update::dev_channel::DevLag> {
        use aterm_update::dev_channel::DevLag;
        vec![
            dev_behind(),
            DevLag::Behind {
                latest: "v0.93.0".into(),
                releases: Some(1),
            },
            DevLag::Behind {
                latest: "v1.0.0".into(),
                releases: None,
            },
            DevLag::Current {
                latest: "v0.93.0".into(),
            },
            DevLag::Ahead {
                latest: "v0.93.0".into(),
            },
        ]
    }

    /// A DEV BUILD'S STANDING (gap #30): behind the channel is a RECORD in the dev
    /// channel's words — `Dev build, 2 releases behind aterm v0.93.0`, Info, what to do
    /// behind it — because an Info row on the glass must be progress or a decision
    /// (ruling 76); at or newer than the newest release says nothing.
    #[test]
    fn a_dev_builds_standing_is_a_record() {
        use crate::message_reporters::{Attention, attention};
        use aterm_update::dev_channel::DevLag;
        let behind = dev_build(&dev_behind()).expect("behind is recorded");
        assert_eq!(behind.title, "Dev build, 2 releases behind aterm v0.93.0");
        assert_eq!(behind.severity, Severity::Info);
        assert_eq!(behind.hold, Hold::LogOnly);
        assert_eq!(behind.key.as_deref(), Some(KEY_DEV_BUILD));
        assert_eq!(attention(&behind), Ok(Attention::Record));
        assert!(
            behind.detail.iter().any(|l| l.contains("tools/dev-app.sh")),
            "what to do rides with it: {:?}",
            behind.detail
        );
        assert_eq!(
            aterm_messages::text::glass_title_fault(&behind.title),
            None,
            "the title keeps the glass form, should the owner rule it onto the glass"
        );
        let one = DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(1),
        };
        assert_eq!(
            dev_build(&one).map(|m| m.title),
            Some("Dev build, 1 release behind aterm v0.93.0".to_string())
        );
        let major = DevLag::Behind {
            latest: "v1.0.0".into(),
            releases: None,
        };
        assert_eq!(
            dev_build(&major).map(|m| m.title),
            Some("Dev build, older than aterm v1.0.0".to_string())
        );
        for quiet in [
            DevLag::Current {
                latest: "v0.93.0".into(),
            },
            DevLag::Ahead {
                latest: "v0.93.0".into(),
            },
        ] {
            assert_eq!(dev_build(&quiet), None, "{quiet:?}: nothing to say");
        }
    }

    /// A DEV BUILD NAMED aterm.app, which runs the release's unattended writers, is a
    /// Standing FAILURE row of its own — keyed apart from the standing, so no reading
    /// of the channel supersedes it — saying what it writes and the release's verb that
    /// puts EACH thing back: `aterm pkg repair` never touches the agent primer, so a
    /// row that names the primer names `aterm agents install` (measured: the one row
    /// seen live said "it writes the agent primer").
    #[test]
    fn a_dev_build_that_writes_the_release_setup_says_what_and_what_puts_it_back() {
        use crate::message_reporters::{Attention, attention};
        let row = dev_build_writes(ALL_WRITES);
        assert_eq!(row.severity, Severity::Warn);
        assert_eq!(row.hold, Hold::Standing);
        assert_eq!(row.key.as_deref(), Some(KEY_DEV_BUILD_WRITES));
        assert_ne!(
            row.key.as_deref(),
            Some(KEY_DEV_BUILD),
            "the channel never supersedes it"
        );
        assert_eq!(attention(&row), Ok(Attention::Failure));
        assert!(
            row.title.contains("lost"),
            "the loss is named: {}",
            row.title
        );
        assert_eq!(aterm_messages::text::glass_title_fault(&row.title), None);
        assert_eq!(
            row.detail[0],
            format!("it writes {}", ALL_WRITES.what),
            "it says what it writes"
        );
        let said = row.detail.join(" ");
        assert!(said.contains("`aterm pkg repair`"), "{said}");
        assert!(said.contains("`aterm agents install`"), "{said}");
        assert!(said.contains("tools/dev-app.sh"), "{said}");
        // What it writes is what its switches let it write, each with its own repair,
        // and nothing from a bundle that is not named aterm.app (the release-bundle
        // rule stands its writers down).
        assert_eq!(dev_shared_writes(false, true, true), Some(ALL_WRITES));
        assert_eq!(
            dev_shared_writes(false, true, false),
            Some(DevSharedWrites {
                what: "the PATH links and the shell hooks",
                repair: "`aterm pkg repair`",
            })
        );
        let primer = dev_shared_writes(false, false, true).expect("the primer alone is a write");
        assert_eq!(
            primer,
            DevSharedWrites {
                what: "the agent primer",
                repair: "`aterm agents install`",
            }
        );
        assert_eq!(dev_shared_writes(false, false, false), None);
        for packages in [false, true] {
            for prime in [false, true] {
                assert_eq!(dev_shared_writes(true, packages, prime), None);
            }
        }
        let only_primer = dev_build_writes(primer);
        assert_eq!(only_primer.detail[0], "it writes the agent primer");
        let said = only_primer.detail.join(" ");
        assert!(said.contains("`aterm agents install`"), "{said}");
        assert!(
            !said.contains("pkg repair"),
            "`aterm pkg repair` does not put the primer back: {said}"
        );
    }

    /// Every posture the App can compute, both vetoes of the disabled handoff
    /// included.
    fn every_posture() -> Vec<ApplyPosture> {
        use ApplyPosture as P;
        let mut postures = vec![
            P::Automatic,
            P::ManualByConfig,
            P::VetoedByEnv {
                var: "ATERM_DEBUG_RELAUNCH_NUDGE",
            },
            P::ManualOnlyLatched { lapses: true },
            P::ManualOnlyLatched { lapses: false },
        ];
        postures.extend(HandoffUnavailable::ALL.map(|why| P::HandoffDisabled { why, veto: None }));
        for why in HandoffUnavailable::ALL {
            postures.push(P::HandoffDisabled {
                why,
                veto: Some(AutoApplyVeto::Config),
            });
            postures.push(P::HandoffDisabled {
                why,
                veto: Some(AutoApplyVeto::Env {
                    var: "ATERM_DEBUG_RELAUNCH_NUDGE",
                }),
            });
        }
        postures
    }

    /// A LANE THAT TRIES AGAIN BY ITSELF ASKS NOTHING (design ruling 246):
    /// its `Couldn't install` is a record — FYI goes to the log, never the
    /// glass (the 11c capture: an empty band) — while a stopped lane keeps its
    /// row and its `Install now`, with the failure's few words beside the
    /// title where they change what the person does (the 11b capture:
    /// `Couldn't install aterm v0.92.0 · disk full`).
    #[test]
    fn a_self_retry_is_a_record_and_a_stopped_lane_says_its_cause() {
        let retry = needs_install(
            "Couldn't install aterm v0.92.0",
            TRIES_AGAIN,
            Severity::Warn,
            1234,
        );
        assert_eq!(retry.hold, Hold::LogOnly, "a record, never a row");
        let stopped = needs_install_because(
            "Couldn't install aterm v0.92.0",
            short_cause("rename: No space left on device (os error 28)"),
            INSTALL_FROM_MENU,
            Severity::Warn,
            1234,
        );
        assert_ne!(stopped.hold, Hold::LogOnly, "a row");
        assert!(stopped.excerpt, "its cause is painted");
        assert_eq!(
            stopped.detail.first().map(String::as_str),
            Some("disk full")
        );
        assert_eq!(stopped.actions, [Intent::ApplyUpdate { build: 1234 }]);
        // No cause known: the title and the press alone, as before.
        let bare = needs_install(
            "Couldn't install aterm v0.92.0",
            INSTALL_FROM_MENU,
            Severity::Warn,
            1234,
        );
        assert!(!bare.excerpt);
        for (said, want) in [
            ("No space left on device", Some("disk full")),
            ("ENOSPC", Some("disk full")),
            ("Read-only file system", Some("read-only disk")),
            (
                "Operation not permitted (os error 1)",
                Some("no permission"),
            ),
            ("the handoff timed out", None),
        ] {
            assert_eq!(short_cause(said), want, "{said}");
        }
    }

    /// EVERY FLOW ROW COMPLETES IN ITS FINISHED FORM (design ruling 154): the
    /// download says `Downloaded`, the check what it proved (`Verified`), and
    /// the install and the successor's finish the deed that landed
    /// (`Installed`) — never a participle beside a ✓ — with nothing else on the
    /// row moving at 60, 80, 120 and 160 columns.
    #[test]
    fn every_flow_row_completes_in_its_finished_form() {
        use crate::message_band::assert_completes_in_place as completes;
        let dl = |total| {
            progress(
                &aterm_update::Progress::Downloading {
                    version: "0.91.0".into(),
                    bytes_done: 31_000_000,
                    bytes_total: total,
                },
                None,
                "0.91.0",
            )
        };
        completes(&dl(74_000_000), "Downloaded aterm v0.91.0");
        completes(&dl(0), "Downloaded aterm v0.91.0");
        let check = progress(
            &aterm_update::Progress::Verifying {
                version: "0.91.0".into(),
            },
            None,
            "0.91.0",
        );
        completes(&check, "Verified aterm v0.91.0");
        completes(&installing("0.91.0"), "Installed aterm v0.91.0");
        completes(&finishing("0.91.0"), "Installed aterm v0.91.0");
        completes(&finishing(""), "Installed update");
        completes(&installing(""), "Installed update");
        // The successor restates the carried row with its own words: the
        // finished words ride the restatement.
        let r = crate::messages_host::restatement_of(&finishing("0.91.0"));
        assert_eq!(
            r.finished,
            Some(Some("Installed aterm v0.91.0".to_string()))
        );
    }

    /// Every row is an update row with a glyph from the band's closed set and
    /// a title inside the cap; the capsule route is the page's own path.
    #[test]
    fn every_update_row_carries_an_admitted_glyph_and_a_capped_title() {
        let rows = [
            progress(
                &aterm_update::Progress::Downloading {
                    version: "0.48.0".into(),
                    bytes_done: 1,
                    bytes_total: 2,
                },
                None,
                "",
            ),
            progress(
                &aterm_update::Progress::Verifying {
                    version: "0.48.0".into(),
                },
                None,
                "",
            ),
            staged("0.48.0", 7, Some(ApplyPosture::Automatic)),
            staged("0.48.0", 7, Some(ApplyPosture::ManualByConfig)),
            download_postponed("later"),
            download_failed("0.48.0", "zip sha256 mismatch"),
            installing("0.48.0"),
            finishing("0.48.0"),
            landed("0.48.0", 7, 0, None),
            landed("0.48.0", 7, 2, Some(Duration::from_secs(42))),
            switch_started(Some("0.48.0"), "0.47.0"),
            switch_stopped(Some("0.48.0"), "0.47.0", false, "child died"),
            outcome('\u{21bb}', "Update installed", "x", Severity::Info),
            needs_install("Update installed", "x", Severity::Info, 7),
            needs_install("Couldn't install aterm v0.48.0", "x", Severity::Warn, 7),
            failed("Update didn't finish", "", false),
            health_warning("aterm can't install updates", "3 checks"),
            health_recovered("aterm can't install updates", "since Sep 14"),
            scrollback_lost(12_345, 3),
            scrollback_lost(1, 1),
            dev_build(&dev_behind()).expect("a record"),
            dev_build_writes(ALL_WRITES),
        ];
        for m in &rows {
            assert_eq!(m.tag, tags::UPDATE, "{}", m.title);
            assert!(m.title.chars().count() <= TITLE_CAP, "{}", m.title);
            assert!(
                Glyph::ALLOWED.contains(&m.glyph.ch()) && m.glyph != Glyph::FALLBACK,
                "{}: {:?}",
                m.title,
                m.glyph
            );
        }
        assert_eq!(
            software_update(),
            Intent::OpenSettings {
                route: "/updates".into()
            }
        );
        assert_eq!(software_update().label(), "Software Update");
        for ch in [FLOW_GLYPH, NEEDS_YOU] {
            assert!(Glyph::new(ch).is_some(), "{ch:?} is in the closed set");
        }
    }

    /// ONE UPDATE, ONE ROW, A TITLE PER PHASE (ruling 143): every phase from
    /// the first byte to the switch is the flow row — `↻` Info, keyed
    /// `update.progress`, verb first — its phase word behind `Details ›`
    /// (never painted), a fill with its amount where the total is known and
    /// busy where it is not, both waiting out the progress grace.
    #[test]
    fn update_reports_map_to_the_flow_row() {
        use aterm_update::Progress as P;
        let m = progress(
            &P::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 74_000_000,
            },
            None,
            "",
        );
        assert_eq!(m.title, "Downloading aterm v0.48.0");
        assert_eq!(m.glyph.ch(), FLOW_GLYPH);
        assert_eq!(m.severity, Severity::Info);
        assert_eq!(m.detail, vec![DOWNLOADING]);
        assert!(
            !m.excerpt,
            "the title says the phase; the word is the log's"
        );
        assert_eq!(m.reveal_after, Some(PROGRESS_GRACE));
        assert_eq!(
            stats_of(&m),
            crate::toolchain_words::byte_stats(45_000_000, 74_000_000)
        );
        assert_eq!(fill_of(&m), Some(608));
        assert!(
            m.meter.as_ref().is_some_and(|m| m.amount.is_some()),
            "the bytes are the ETA's input"
        );
        assert!(!busy(&m), "a known total is a fill, not motion");
        assert_eq!(
            m.hold,
            Hold::Live {
                stale_after: STALE_UPDATE
            }
        );
        assert_eq!(m.key.as_deref(), Some(KEY_PROGRESS));
        assert!(m.actions.is_empty(), "no capsule but the implicit Details");
        // An unknown total is honest: motion and the bytes so far, no fill.
        let m = progress(
            &P::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 0,
            },
            None,
            "",
        );
        assert_eq!(fill_of(&m), None);
        assert!(busy(&m));
        assert_eq!(stats_of(&m), "45 MB");
        let m = progress(
            &P::Verifying {
                version: "0.48.0".into(),
            },
            None,
            "",
        );
        assert_eq!(m.title, "Checking aterm v0.48.0");
        assert_eq!(m.detail, vec![CHECKING]);
        assert!(busy(&m), "checking has no fraction: the row moves");
        assert!(!m.excerpt);
        // A failed download: a RECORD (the next check fetches it again), its
        // title by outcome, the updater's whole sentence kept.
        let cause = "zip sha256 mismatch: expected ab12, got cd34 — see                      https://github.com/alabsystems/aterm/releases/tag/v0.48.0 for the assets";
        let m = progress(
            &P::Failed {
                detail: cause.into(),
            },
            None,
            "0.48.0",
        );
        assert_eq!(m.title, "Couldn't download aterm v0.48.0");
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(m.detail[0], DOWNLOAD_FAILED);
        assert_eq!(m.detail[1..].join(" "), cause, "the whole sentence, kept");
        assert_eq!(m.hold, Hold::LogOnly, "a self-retry is a record");
        assert_eq!(m.key, None, "a record never supersedes the live flow row");
        assert!(
            m.actions.is_empty(),
            "a retried download asks nothing of anyone"
        );
        assert_eq!(
            download_failed("", "x").title,
            "Couldn't download the update"
        );
        // A postponed download is a record, never a row.
        let m = progress(
            &P::Deferred {
                detail: "the network is metered; checking again in 10 min".into(),
            },
            None,
            "0.48.0",
        );
        assert_eq!(m.title, "Update download postponed");
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key, None);
        assert_eq!(
            m.detail,
            vec!["the network is metered; checking again in 10 min"]
        );
        // No version to name: the re-exec QA seam.
        assert_eq!(flow_title("Installing", ""), "Installing update");
        assert_eq!(Severity::Warn.default_hold(), HOLD_WARN);
    }

    /// THE OWNER'S RULE OVER EVERY UPDATE BUILDER (ruling 76, ruling 143):
    /// every row the lane composes is progress with its indicator, a decision,
    /// a failure the person acts on, or a record — never a confirmation, an
    /// FYI or a still Info row on the glass — and every glass title is terse.
    #[test]
    fn every_update_message_earns_its_row() {
        use crate::message_reporters::{Attention, attention};
        use aterm_update::Progress as P;
        let mut all = vec![
            progress(
                &P::Downloading {
                    version: "0.91.0".into(),
                    bytes_done: 1,
                    bytes_total: 2,
                },
                None,
                "",
            ),
            progress(
                &P::Downloading {
                    version: "0.91.0".into(),
                    bytes_done: 1,
                    bytes_total: 0,
                },
                None,
                "",
            ),
            progress(
                &P::Verifying {
                    version: "0.91.0".into(),
                },
                None,
                "",
            ),
            progress(&P::Failed { detail: "x".into() }, None, "0.91.0"),
            progress(&P::Deferred { detail: "x".into() }, None, "0.91.0"),
            installing("0.91.0"),
            installing(""),
            finishing("0.91.0"),
            landed("0.91.0", 7, 0, Some(Duration::from_secs(42))),
            switch_started(Some("0.91.0"), "0.90.0"),
            switch_stopped(Some("0.91.0"), "0.90.0", true, "x"),
            switch_stopped(Some("0.91.0"), "0.90.0", false, "x"),
            outcome(
                '\u{21bb}',
                "Couldn't install aterm v0.91.0",
                "x",
                Severity::Info,
            ),
            outcome('\u{26a0}', "Update waits", "x", Severity::Warn),
            needs_install(
                "Couldn't install aterm v0.91.0",
                INSTALL_FROM_MENU,
                Severity::Warn,
                7,
            ),
            needs_install(
                &crate::app_update_screen::update_installed_title(Some("9.9.9")),
                crate::app_update_screen::UPDATE_INSTALLED_DETAIL,
                Severity::Info,
                7,
            ),
            needs_install(
                crate::app_update_screen::UPDATE_DIDNT_INSTALL,
                INSTALL_FROM_MENU,
                Severity::Warn,
                7,
            ),
            failed("Update didn't finish", "", false),
            failed(
                crate::app_update_screen::UPDATE_WAITS_FOR_YOU,
                crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
                true,
            ),
            health_recovered("aterm can't install updates", "since Sep 14"),
            staged("0.91.0", 7, None),
            scrollback_lost(12_345, 3),
        ];
        for lag in every_dev_standing() {
            all.extend(dev_build(&lag));
        }
        for packages in [false, true] {
            for prime in [false, true] {
                all.extend(dev_shared_writes(false, packages, prime).map(dev_build_writes));
            }
        }
        for posture in every_posture() {
            all.push(staged("0.91.0", 7, Some(posture)));
        }
        for class in ["apply", "pipeline", "stage", "manifest"] {
            all.push(health_warning(
                aterm_update::health_failing_title(class),
                "3 failed checks in a row since 2026-09-14T22:04:36Z: x.",
            ));
        }
        let mut classes = std::collections::BTreeSet::new();
        for m in &all {
            let class =
                attention(m).unwrap_or_else(|why| panic!("{why}: {:?} ({:?})", m.title, m.hold));
            classes.insert(format!("{class:?}"));
            if matches!(m.hold, Hold::Live { .. }) && m.severity < Severity::Warn {
                assert_eq!(class, Attention::Progress, "{}", m.title);
            }
        }
        assert_eq!(
            classes.len(),
            4,
            "the lane exercises progress, decision, failure and record: {classes:?}"
        );
    }

    /// A FAILED UPDATE KEEPS ITS WHOLE CAUSE (design ruling 64): the updater's
    /// 600-char sentence with a URL is every word behind the flow row's
    /// excerpt — never pre-cut at 100 chars as the old row was.
    #[test]
    fn a_failed_update_keeps_its_whole_cause() {
        let cause = format!(
            "the release asset could not be verified: {}see \
             https://github.com/alabsystems/aterm/releases/tag/v0.92.0 for the signed assets",
            "the digest did not match the manifest; ".repeat(12)
        );
        assert!(cause.chars().count() > 500);
        let m = progress(
            &aterm_update::Progress::Failed {
                detail: cause.clone(),
            },
            None,
            "0.92.0",
        );
        assert_eq!(m.detail[0], DOWNLOAD_FAILED);
        let words = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            words(&m.detail[1..].join(" ")).replace(";", ""),
            words(&cause).replace(";", ""),
            "every word of the cause, the URL whole"
        );
        assert!(m.detail.iter().any(|l| l.contains("releases/tag/v0.92.0")));
    }

    /// THE STAGED ROW SAYS HOW THE UPDATE INSTALLS, AND NEVER ASKS FOR A
    /// RESTART (ruling 143; the owner's silent path, 2026-09-24). Where a press
    /// installs it, the ready row with the `Install now` capsule, held for the
    /// press; every other posture — the automatic lane that lands it within a
    /// minute included — is a RECORD: no row, no re-grid.
    #[test]
    fn a_staged_row_says_how_the_update_installs_and_never_asks_for_a_restart() {
        use ApplyPosture as P;
        assert_eq!(
            staged("0.67.0", 7, Some(P::Automatic)).hold,
            Hold::LogOnly,
            "the automatic lane's staged build takes no row"
        );
        for posture in every_posture() {
            let m = staged("0.67.0", 7, Some(posture));
            if staged_is_decision(Some(posture)) {
                assert_eq!(m.title, "aterm v0.67.0 is ready", "{posture:?}");
                assert_eq!(m.glyph.ch(), '\u{2713}');
                assert_eq!(m.severity, Severity::Info, "a decision, not a confirmation");
                assert_eq!(m.hold, Hold::For(HOLD_STAGED_MANUAL));
                assert!(!m.excerpt, "the capsule is the press");
                assert!(!busy(&m), "a ready row waits; it does not move");
            } else {
                assert_eq!(m.title, "aterm v0.67.0 is ready", "{posture:?}");
                assert_eq!(m.hold, Hold::LogOnly, "{posture:?}: nothing to press");
                assert!(m.meter.is_none());
            }
            assert!(!m.title.to_lowercase().contains("click"), "{}", m.title);
            assert_eq!(m.key.as_deref(), Some(KEY_PROGRESS));
            assert_eq!(
                m.actions.contains(&Intent::ApplyUpdate { build: 7 }),
                matches!(
                    posture,
                    P::ManualByConfig
                        | P::VetoedByEnv { .. }
                        | P::ManualOnlyLatched { lapses: false }
                ),
                "{posture:?}: {:?}",
                m.actions
            );
            let sentence = m.detail.join("; ");
            assert_eq!(sentence, staged_detail(posture), "{posture:?}");
            assert!(
                m.detail
                    .iter()
                    .all(|l| l.chars().count() <= DETAIL_LINE_CAP && !l.ends_with('\u{2026}')),
                "{posture:?}: {:?}",
                m.detail
            );
            // Never the internal words (2026-09-23 audit).
            for word in [
                "build 7",
                "verified",
                "in place",
                "staging",
                "shells keep running",
            ] {
                assert!(
                    !sentence.contains(word),
                    "{posture:?}: {word:?} in {sentence}"
                );
            }
            let lower = sentence
                .replace("ATERM_DEBUG_RELAUNCH_NUDGE", "<seam>")
                .to_lowercase();
            assert!(
                !lower.contains("restart") && !lower.contains("relaunch"),
                "{posture:?} asks for a restart: {sentence}"
            );
            // THE HANDOFF-OFF LINE (kept from the pre-merge guard): its cause
            // leads, for every cause; it promises only what the admission
            // classifier does — nothing installs while a terminal is open — and
            // neither an in-place landing nor a kill it forbids; a vetoed lane
            // names the menu instead of a landing it never arms.
            if let P::HandoffDisabled { why, veto } = posture {
                assert!(
                    sentence.starts_with(why.cause()),
                    "{posture:?}: the cause leads: {sentence}"
                );
                assert!(
                    sentence.contains("once every terminal is closed"),
                    "{posture:?}: {sentence}"
                );
                assert_eq!(
                    sentence.contains(INSTALL_FROM_MENU),
                    veto.is_some(),
                    "{posture:?}: {sentence}"
                );
                assert!(
                    !lower.contains("keep running") && !lower.contains("survive"),
                    "with the handoff off the line may neither promise an in-place \
                     landing nor threaten a kill the admission gate forbids: {sentence}"
                );
            }
        }
        assert_eq!(staged_detail(P::Automatic), INSTALLS_BY_ITSELF);
        assert_eq!(staged_detail(P::ManualByConfig), "automatic install is off");
        assert_eq!(
            staged_detail(P::ManualOnlyLatched { lapses: true }),
            "didn't install \u{2014} tries again by itself later"
        );
        assert_eq!(
            staged_detail(P::ManualOnlyLatched { lapses: false }),
            "automatic install didn't work"
        );
        assert_eq!(
            staged_detail(P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None
            }),
            "--headless (no window) \u{2014} installs once every terminal is closed"
        );
        assert_eq!(
            staged_detail(P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: Some(AutoApplyVeto::Config)
            }),
            format!(
                "--headless (no window); automatic install is off \u{2014} {INSTALL_FROM_MENU} \
                 once every terminal is closed"
            )
        );
        // Every staged sentence fits one line today (the longest is about 150
        // chars): the split is a guard, not a layout.
        for posture in every_posture() {
            assert_eq!(staged_detail_lines(Some(posture)).len(), 1, "{posture:?}");
        }
        // A caller with no posture states only the manual affordance — true in
        // every posture the seamless lane serves, a promise in none — with the
        // capsule and the short hold: a decision.
        let m = staged("0.67.0", 7, None);
        assert_eq!(m.detail, vec![staged_detail_unknown()]);
        assert_eq!(staged_detail_unknown(), INSTALL_FROM_MENU);
        assert_eq!(m.actions, vec![Intent::ApplyUpdate { build: 7 }]);
        assert_eq!(m.hold, Hold::For(HOLD_STAGED_MANUAL));
        // NEVER "WITHIN N MIN" ARITHMETIC: the ladder lands within a minute,
        // and `LANDS_WITHIN / 60` printed "within 0 min" for it.
        assert!(crate::native_update_auto_intent::LANDS_WITHIN < Duration::from_secs(60));
        assert!(!INSTALLS_BY_ITSELF.contains("0 min"));
    }

    /// RESTATE: the App refines the staged row once the lane has actually
    /// armed or stood down — the FULL words. A lane that installs by itself
    /// (armed, or retrying later) is a RECORD, which the host folds the ready
    /// row into rather than restating it.
    #[test]
    fn a_posture_restatement_carries_the_full_words() {
        use ApplyPosture as P;
        let r = restate_apply_posture("0.67.0", 7, P::Automatic);
        assert_eq!(r.hold, Some(Hold::LogOnly));
        assert_eq!(r.detail, Some(vec![INSTALLS_BY_ITSELF.to_string()]));
        assert_eq!(r.actions, Some(Vec::new()));
        // STANDING DOWN FOR GOOD: the ready row, the capsule, the short hold,
        // still.
        let r = restate_apply_posture("0.67.0", 7, P::ManualOnlyLatched { lapses: false });
        assert_eq!(r.title.as_deref(), Some("aterm v0.67.0 is ready"));
        assert_eq!(r.actions, Some(vec![Intent::ApplyUpdate { build: 7 }]));
        assert_eq!(r.hold, Some(Hold::For(HOLD_STAGED_MANUAL)));
        assert_eq!(r.severity, Some(Severity::Info));
        assert_eq!(r.meter, Some(None));
        // A lane standing down with a retry scheduled installs by itself, but
        // minutes to hours away: its words are a RECORD, which the host folds
        // the row into rather than restating it.
        let r = restate_apply_posture("0.67.0", 7, P::ManualOnlyLatched { lapses: true });
        assert_eq!(r.hold, Some(Hold::LogOnly));
        assert_eq!(r.meter, Some(None));
        let r = restate_apply_posture(
            "0.67.0",
            7,
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None,
            },
        );
        assert_eq!(r.actions, Some(Vec::new()), "the handoff off strips it");
    }

    /// The design's §7.3 row: `Install now` rides the staged row ONLY where a
    /// press is how the build installs.
    #[test]
    fn the_staged_row_carries_install_only_under_a_manual_posture() {
        use ApplyPosture as P;
        assert_eq!(apply_capsule_for(Some(P::Automatic), 3), None);
        assert_eq!(
            apply_capsule_for(Some(P::ManualOnlyLatched { lapses: true }), 3),
            None
        );
        for why in HandoffUnavailable::ALL {
            assert_eq!(
                apply_capsule_for(Some(P::HandoffDisabled { why, veto: None }), 3),
                None
            );
        }
        for manual in [
            P::ManualByConfig,
            P::VetoedByEnv {
                var: "ATERM_DEBUG_RELAUNCH_NUDGE",
            },
            P::ManualOnlyLatched { lapses: false },
        ] {
            assert_eq!(
                apply_capsule_for(Some(manual), 3),
                Some(Intent::ApplyUpdate { build: 3 }),
                "{manual:?}"
            );
        }
        assert_eq!(
            apply_capsule_for(None, 3),
            Some(Intent::ApplyUpdate { build: 3 }),
            "no posture keeps the manual affordance"
        );
        assert_eq!(Intent::ApplyUpdate { build: 3 }.label(), "Install now");
    }

    /// R39 says how much scrollback stayed behind, in how many tabs, and
    /// where the per-tab count is — a failure on glass, never a record only.
    #[test]
    fn the_scrollback_row_counts_the_lines_and_the_tabs() {
        let m = scrollback_lost(12_345, 3);
        assert_eq!(m.title, "Couldn't carry all scrollback");
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(
            m.hold,
            Hold::Default,
            "a loss is on glass, not only on record"
        );
        assert_eq!(m.key.as_deref(), Some(KEY_SCROLLBACK));
        assert_eq!(m.detail[0], "12345 older lines stayed behind in 3 tabs");
        assert!(
            m.detail.iter().any(|l| l.contains("history_lost=")),
            "{:?}",
            m.detail
        );
        assert_eq!(
            scrollback_lost(1, 1).detail[0],
            "1 older line stayed behind in 1 tab"
        );
    }

    /// Every flow detail is short and point first: at most 40 characters, so
    /// the phase is read at a glance and survives any ordinary width.
    #[test]
    fn every_flow_detail_is_short_and_point_first() {
        use ApplyPosture as P;
        let mut details: Vec<String> = [
            DOWNLOADING,
            CHECKING,
            INSTALLS_BY_ITSELF,
            INSTALLING,
            FINISHING,
            DOWNLOAD_FAILED,
        ]
        .map(str::to_string)
        .to_vec();
        details.push(staged_detail(P::Automatic));
        for d in details {
            assert!(
                d.chars().count() <= 40,
                "{d:?} is {} chars",
                d.chars().count()
            );
            assert!(!d.is_empty());
        }
    }

    /// NO "LOCK", "BLOCKED", "ANOTHER ATERM", "PAUSE" OR "QUEUED" ON THE GLASS
    /// (owner rulings 2026-09-14 and 2026-09-18): every sentence the update
    /// lane composes for a flow, ready, installing or finishing row, under
    /// every posture, names what stays true and nothing that reads as a fault
    /// or a stall.
    #[test]
    fn the_update_lanes_sentences_carry_no_fault_or_stall_words() {
        let banned = [
            "lock",
            "blocked",
            "another aterm",
            "pause",
            "queued",
            "freeze",
        ];
        let mut sentences: Vec<String> = every_posture()
            .iter()
            .flat_map(|posture| {
                let m = staged("0.88.0", 7, Some(*posture));
                let mut words = vec![m.title];
                words.extend(m.detail);
                words
            })
            .collect();
        sentences.push(staged_detail_unknown());
        sentences.extend([DOWNLOADING, CHECKING, DOWNLOAD_FAILED].map(str::to_string));
        for m in [
            installing("0.88.0"),
            finishing("0.88.0"),
            landed("0.88.0", 7, 0, None),
        ] {
            sentences.push(m.title);
            sentences.extend(m.detail);
        }
        for sentence in sentences {
            let words = sentence.to_lowercase();
            for word in banned {
                assert!(!words.contains(word), "{word:?} in {sentence:?}");
            }
        }
    }

    /// THE SWITCH AND THE LANDING (rulings 141, 143): installing and
    /// finishing are the flow row, busy, under the handoff's cap, a title per
    /// phase and the phase word behind `Details ›`; both share the progress
    /// key so each replaces the last in place. The landing is the flow row's
    /// Complete echo on the glass and a RECORD in its own bare words, keyless
    /// (a record never supersedes a live row), claiming nothing about the
    /// shells.
    #[test]
    fn the_switch_and_the_landing_share_the_flow_row() {
        let i = installing("9.9.9");
        assert_eq!(i.title, "Installing aterm v9.9.9");
        assert_eq!(i.detail, vec![INSTALLING]);
        assert!(busy(&i));
        assert!(!i.excerpt);
        assert_eq!(
            i.hold,
            Hold::Live {
                stale_after: STALE_HANDOFF
            }
        );
        assert_eq!(installing("").title, "Installing update");
        let f = finishing("9.9.9");
        assert_eq!(f.title, "Finishing aterm v9.9.9");
        assert_eq!(f.detail, vec![FINISHING]);
        assert!(!f.excerpt, "a reassurance changes nothing the person does");
        assert!(busy(&f));
        assert_eq!(f.key, i.key);
        let l = landed("9.9.9", 7, 0, None);
        assert_eq!(l.title, "Updated to aterm v9.9.9");
        assert!(
            l.detail.is_empty(),
            "no claim about the shells: {:?}",
            l.detail
        );
        assert_eq!(l.glyph.ch(), '\u{2713}');
        assert_eq!(
            l.hold,
            Hold::LogOnly,
            "the echo is the glass's; the words a record"
        );
        assert_eq!(l.severity, Severity::Success);
        assert_eq!(l.key, None);
        assert_eq!(landed("", 7, 0, None).title, "Updated to build 7");
    }

    /// A REPAINT IS NEVER UNSEEN (the 2026-09-22/23 update audit, plan P1-5):
    /// when the successor adopted any session onto a blank screen, the landing
    /// RECORD names how many — singular and plural — and why those tabs came
    /// over blank; a desk that crossed exactly keeps main's bare record. One
    /// record for one fact: the landing is a durable fact and no row (ruling
    /// 76, ruling 141), and nothing on a blank tab is the person's to do.
    #[test]
    fn the_landing_names_the_tabs_it_repainted() {
        let one = landed("0.92.0", 7, 1, None);
        assert_eq!(one.title, "Updated to aterm v0.92.0");
        assert_eq!(one.detail[0], "1 tab repainted");
        assert!(
            one.detail[1..]
                .join(" ")
                .starts_with("its screen was blank after the update"),
            "{:?}",
            one.detail
        );
        assert!(
            one.detail.join(" ").contains("asked to redraw"),
            "{:?}",
            one.detail
        );
        assert_eq!(one.severity, Severity::Success);
        assert_eq!(one.hold, Hold::LogOnly, "still a record (ruling 141)");
        assert!(
            one.actions.is_empty(),
            "nothing to press: {:?}",
            one.actions
        );
        let three = landed("0.92.0", 7, 3, None);
        assert_eq!(three.detail[0], "3 tabs repainted");
        assert!(
            three.detail[1..].join(" ").starts_with("their screens"),
            "{:?}",
            three.detail
        );
        assert!(landed("0.92.0", 7, 0, None).detail.is_empty());
    }

    /// THE UPDATE'S RECORDS: when the switch began or stopped, and how long
    /// it took — each a `LogOnly` record with no key, every version spelled
    /// `aterm vX`, and the duration a line of the landing, never a second
    /// entry (audit 2026-09-24).
    #[test]
    fn the_update_records_say_what_happened_when() {
        let s = switch_started(Some("0.79.0"), "0.78.0");
        assert_eq!(s.title, "Installing aterm v0.79.0");
        assert_eq!(s.detail, vec!["from aterm v0.78.0"]);
        assert_eq!(s.hold, Hold::LogOnly);
        assert_eq!(
            switch_started(None, "0.78.0").title,
            "Reloading aterm in place"
        );
        let waiting = switch_stopped(Some("0.79.0"), "0.78.0", true, "typing");
        assert_eq!(waiting.title, "Waiting to install aterm v0.79.0");
        assert_eq!(
            waiting.detail.join(" "),
            "you were typing, so aterm v0.78.0 kept running; it tries again by itself"
        );
        assert_eq!(waiting.severity, Severity::Info);
        let not = switch_stopped(Some("0.79.0"), "0.78.0", false, "the proof timed out");
        assert_eq!(not.title, "aterm v0.79.0 was not installed");
        assert_eq!(
            not.detail.join(" "),
            "aterm v0.78.0 kept running: the proof timed out"
        );
        assert_eq!(not.severity, Severity::Warn);
        assert_eq!(
            switch_stopped(None, "0.78.0", true, "x").title,
            "Waiting to reload aterm"
        );
        assert_eq!(
            switch_stopped(None, "0.78.0", false, "x").title,
            "aterm was not reloaded"
        );
        for m in [s, waiting, not] {
            let words = std::iter::once(m.title.clone())
                .chain(m.detail.clone())
                .collect::<Vec<_>>()
                .join(" ");
            for jargon in ["switch", "carried", "on its own", "build "] {
                assert!(!words.contains(jargon), "{jargon:?} in {words:?}");
            }
        }
        let took = landed("0.79.0", 7, 0, Some(Duration::from_secs(42)));
        assert_eq!(took.title, "Updated to aterm v0.79.0");
        assert_eq!(took.detail, vec!["installed 42 s after the download"]);
        assert_eq!(took.hold, Hold::LogOnly);
        for (secs, words) in [
            (42, "42 s"),
            (60, "1 min"),
            (185, "3 min 5 s"),
            (7200, "2 h"),
            (7800, "2 h 10 min"),
        ] {
            assert_eq!(span_words(Duration::from_secs(secs)), words);
        }
    }

    /// THE LANDING'S CLOCKS: the download instant crosses the handoff on the
    /// wall clock and comes back on the successor's monotonic one, so the
    /// record says how long the whole update took — for this build only.
    #[test]
    fn the_download_instant_round_trips_the_carry_for_its_build() {
        let now = Instant::now();
        let wall = SystemTime::now();
        let at = now - Duration::from_secs(42);
        let ms = verified_unix_ms(Some((7, at)), 7, now, wall).expect("this build");
        assert_eq!(
            verified_unix_ms(Some((7, at)), 8, now, wall),
            None,
            "another build"
        );
        assert_eq!(verified_unix_ms(None, 7, now, wall), None);
        let (build, back) = carried_verification(Some(ms), 7, now, wall).expect("carried");
        assert_eq!(build, 7);
        let took = now.saturating_duration_since(back);
        assert!(
            took >= Duration::from_secs(41) && took <= Duration::from_secs(43),
            "{took:?}"
        );
        assert_eq!(carried_verification(None, 7, now, wall), None);
    }

    /// THE OUTCOMES (ruling 143): main's words, ruling 76 deciding the glass.
    /// What the lane answers by itself is a RECORD; a lane that stopped is a
    /// decision row whose `Install now` is the Version menu's press; an
    /// attempt nothing retries is a failure row with the page. None shares the
    /// flow row's key, so none covers it.
    #[test]
    fn an_outcome_is_a_record_unless_the_person_acts_on_it() {
        let retry = outcome(
            '\u{21bb}',
            "Couldn't install aterm v9.9.9",
            "will try again by itself",
            Severity::Info,
        );
        assert_eq!(retry.hold, Hold::LogOnly, "a self-retry is a record");
        assert!(retry.actions.is_empty());
        assert_eq!(retry.key.as_deref(), Some(KEY_OUTCOME));
        let warn = outcome('\u{26a0}', "Update waits", "x", Severity::Warn);
        assert_eq!(warn.hold, Hold::LogOnly);
        assert_eq!(warn.actions, vec![software_update()], "the page's re-offer");
        let stopped = needs_install(
            "Couldn't install aterm v9.9.9",
            INSTALL_FROM_MENU,
            Severity::Warn,
            7,
        );
        assert_eq!(stopped.hold, Hold::Default, "the warning's hold");
        assert_eq!(stopped.actions, vec![Intent::ApplyUpdate { build: 7 }]);
        assert_eq!(stopped.glyph.ch(), NEEDS_YOU);
        assert!(!stopped.excerpt, "the capsule is the press");
        assert_ne!(stopped.key, Some(KEY_PROGRESS.to_string()));
        let installed = needs_install(
            &crate::app_update_screen::update_installed_title(Some("9.9.9")),
            crate::app_update_screen::UPDATE_INSTALLED_DETAIL,
            Severity::Info,
            7,
        );
        assert_eq!(installed.hold, Hold::For(HOLD_STAGED_MANUAL));
        assert_eq!(installed.actions, vec![Intent::ApplyUpdate { build: 7 }]);
        // Main's ruling-68 row: the working glyph — never a done-mark
        // beside `Install now` — and the menu's words behind Details, since
        // the capsule beside it is that press (ruling 161).
        assert_eq!(installed.glyph.ch(), FLOW_GLYPH);
        assert!(!installed.excerpt, "the capsule is the press");
        assert_eq!(
            installed.detail,
            [crate::app_update_screen::UPDATE_INSTALLED_DETAIL]
        );
        let bare = failed("Update didn't finish", "", false);
        assert!(bare.detail.is_empty(), "the capsules are the press");
        assert_eq!(bare.hold, Hold::Default);
        assert_eq!(bare.severity, Severity::Warn);
        assert_eq!(bare.actions, vec![software_update()]);
        let blocked = failed(
            crate::app_update_screen::UPDATE_WAITS_FOR_YOU,
            crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            true,
        );
        assert!(blocked.excerpt, "a blocker the person clears is painted");
        assert!(blocked.actions.is_empty(), "its words are the press");
        assert!(!failed("x", "handoff proof ended TimedOut", false).excerpt);
    }

    /// A PERSISTENT FAILURE IS READ ONCE PER CLASS: the title names the broken
    /// half, the excerpt since when, the whole sentence behind Details (its
    /// command intact), the Software Update capsule, the warning's hold —
    /// never Standing (design ruling 59).
    #[test]
    fn the_health_warning_names_the_half_and_since_when() {
        let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: release manifests \
                    exist but cannot be downloaded. Run `aterm ctl update status` for details.";
        for class in ["apply", "pipeline", "stage", "manifest"] {
            let m = health_warning(aterm_update::health_failing_title(class), body);
            assert!(m.title.starts_with("aterm can't "), "{}", m.title);
            assert_eq!(m.detail[0], "since Sep 14", "{class}");
            assert!(
                m.detail[1..]
                    .join(" ")
                    .contains("Run `aterm ctl update status` for details."),
                "{:?}",
                m.detail
            );
            assert_eq!(m.hold, Hold::Default);
            assert_eq!(m.severity, Severity::Warn);
            assert_eq!(m.key.as_deref(), Some(KEY_HEALTH));
            assert_eq!(m.actions, vec![software_update()]);
            assert!(!m.detail.join(" ").contains("click"));
        }
        let undated = health_warning("aterm can't check for updates", "no date here");
        assert_eq!(undated.detail, vec!["no date here"]);
        assert_eq!(
            health_notification_body(body),
            "Since Sep 14. Details are in Settings \u{25b8} Software Update."
        );
        assert_eq!(
            health_notification_body("no date"),
            "Details are in Settings \u{25b8} Software Update."
        );
        let healed = health_recovered("aterm can't install updates", "since Sep 14");
        assert_eq!(healed.title, "aterm updates work again");
        assert_eq!(
            healed.detail,
            vec!["after \"aterm can't install updates\", since Sep 14"]
        );
        assert_eq!(healed.hold, Hold::LogOnly);
    }

    /// The disabled-handoff line says what the admission classifier does:
    /// nothing installs while a terminal is open, and it lands once they are
    /// closed — never a threat to the shells, never a restart.
    #[test]
    fn the_disabled_handoff_line_says_what_the_admission_classifier_does() {
        use crate::native_update_admission::{
            AdmissionBlock, AdmissionDecision, AdmissionFacts, ApplyLane, classify,
        };
        let opted_out = |live_ptys: usize| AdmissionFacts {
            staged_verified: true,
            seamless_capable: false,
            native_state_certified: true,
            live_ptys,
            foreground_jobs: 0,
            unknown_foregrounds: 0,
        };
        assert_eq!(
            classify(opted_out(1)),
            AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless),
            "one open terminal: the install is refused and nothing dies"
        );
        assert_eq!(
            classify(opted_out(0)),
            AdmissionDecision::Apply(ApplyLane::Cold),
            "no terminal at all: the cold re-exec"
        );
        for why in HandoffUnavailable::ALL {
            let line = staged_detail(ApplyPosture::HandoffDisabled { why, veto: None });
            assert!(
                line.starts_with(why.cause())
                    && line.ends_with("installs once every terminal is closed"),
                "{line}"
            );
            let lower = line.to_lowercase();
            assert!(
                !lower.contains("survive")
                    && !lower.contains("keep running")
                    && !lower.contains("restart")
                    && !lower.contains("relaunch")
                    && !lower.contains("version menu"),
                "{line}"
            );
        }
    }

    /// NOTHING IN THE UPDATE LANE ASKS FOR A RESTART OR A RELAUNCH (owner ruling,
    /// 2026-08-30). The mechanism is the in-session overlap handoff; every sentence
    /// a person can read on the way to it — the band's rows, the Version-menu /
    /// palette row, the trouble tails, the admission refusals, the close-preflight
    /// blockers, the installed-on-disk outcomes, the records — is pinned here ONCE,
    /// so the words cannot drift back one surface at a time. "Restart" and
    /// "relaunch" stay reserved for the things that genuinely need one
    /// (`columns`/`lines`, the GPU renderer choice, the Windows backdrop, Rosetta),
    /// none of which is an update. (docs/RFC-proof-carrying-dsu.md cites it.)
    #[test]
    fn no_update_surface_asks_for_a_restart_or_a_relaunch() {
        use crate::native_update_admission::{AdmissionBlock, AdmissionFacts};
        use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};

        let mut surfaces: Vec<(&str, String)> = Vec::new();
        for posture in every_posture() {
            surfaces.push(("staged row", staged_detail(posture)));
        }
        surfaces.push(("staged row (no posture)", staged_detail_unknown()));
        surfaces.push((
            "Version menu row",
            crate::menu::staged_apply_label("\u{2191}", 7, "9.9.9", None),
        ));
        surfaces.push((
            "Version menu row (same version)",
            crate::menu::staged_apply_label(
                "\u{2191}",
                7,
                crate::build_info::version_display(),
                None,
            ),
        ));
        for retry in [ApplyRetry::Scheduled, ApplyRetry::ManualOnly] {
            let trouble = ApplyTrouble::new(
                2,
                "overlap handoff failed safely: handoff proof ended ChildDied",
                retry,
            )
            .expect("two failed applies");
            surfaces.push(("trouble row tail", trouble.row_tail()));
            surfaces.push(("trouble sentence", trouble.sentence()));
            surfaces.push(("trouble compact", trouble.compact()));
            surfaces.push(("trouble micro", trouble.micro()));
            surfaces.push((
                "Version menu row (troubled)",
                crate::menu::staged_apply_label("\u{2191}", 7, "9.9.9", Some(&trouble)),
            ));
        }
        let facts = AdmissionFacts {
            staged_verified: true,
            seamless_capable: false,
            native_state_certified: false,
            live_ptys: 2,
            foreground_jobs: 1,
            unknown_foregrounds: 1,
        };
        for block in [
            AdmissionBlock::UnverifiedStage,
            AdmissionBlock::NativeStateUncertified,
            AdmissionBlock::LivePtysNeedSeamless,
            AdmissionBlock::ForegroundProbeUnknown,
        ] {
            surfaces.push(("admission refusal", block.message(facts)));
        }
        for blocker in [
            crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
            crate::App::RESTORE_IN_FLIGHT_BLOCKS_APPLY,
            crate::App::INSTALLED_ACTIVATES_IN_PLACE,
        ] {
            surfaces.push(("close-preflight / installed", blocker.to_string()));
        }
        for version in [Some("9.9.9"), None] {
            surfaces.push((
                "updater outcome",
                crate::native_updater_service::NativeUpdaterService::installed_activation_outcome(
                    version,
                ),
            ));
        }
        surfaces.push((
            "updater outcome",
            crate::native_updater_service::NativeUpdaterService::apply_attempt_stopped_outcome(
                "child died",
            ),
        ));
        for (title, detail) in [
            (
                &*crate::app_update_screen::update_installed_title(Some("9.9.9")),
                crate::app_update_screen::UPDATE_INSTALLED_DETAIL,
            ),
            (
                crate::app_update_screen::UPDATE_DIDNT_INSTALL,
                INSTALL_FROM_MENU,
            ),
            ("Couldn't install aterm v9.9.9", INSTALL_FROM_MENU),
            ("Couldn't install aterm v9.9.9", TRIES_AGAIN),
            ("Couldn't download aterm v9.9.9", DOWNLOAD_FAILED),
            ("Update didn't finish", ""),
        ] {
            surfaces.push(("outcome row", format!("{title} — {detail}")));
        }
        for m in [
            installing("9.9.9"),
            finishing("9.9.9"),
            landed("9.9.9", 7, 0, Some(Duration::from_secs(42))),
            landed("9.9.9", 7, 1, None),
            landed("9.9.9", 7, 3, None),
            landed("", 7, 0, None),
            download_failed("9.9.9", "x"),
            download_postponed("busy"),
            switch_started(Some("9.9.9"), "9.9.8"),
            switch_started(None, "9.9.8"),
            health_recovered("aterm can't install updates", "since Sep 14"),
        ]
        .into_iter()
        .chain([true, false].into_iter().flat_map(|routine| {
            [
                switch_stopped(Some("9.9.9"), "9.9.8", routine, "x"),
                switch_stopped(None, "9.9.8", routine, "x"),
            ]
        })) {
            surfaces.push((
                "row or record",
                format!("{} — {}", m.title, m.detail.join(" ")),
            ));
        }
        surfaces.push((
            "blocker row",
            crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string(),
        ));
        for class in ["apply", "pipeline", "stage", "manifest"] {
            surfaces.push((
                "health row",
                aterm_update::health_failing_title(class).to_string(),
            ));
        }
        assert!(surfaces.len() >= 40, "the guard covers the whole lane");
        for (surface, text) in surfaces {
            // An environment variable's NAME is an identifier, not a prompt.
            let lower = text
                .replace("ATERM_DEBUG_RELAUNCH_NUDGE", "<seam>")
                .to_lowercase();
            assert!(
                !lower.contains("restart")
                    && !lower.contains("relaunch")
                    && !lower.contains("reopen"),
                "{surface} asks for a restart: {text:?}"
            );
        }
    }
}
