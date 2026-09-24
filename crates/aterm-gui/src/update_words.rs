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
//! (busy), `Installing aterm vX` — the automatic staged row, busy with the
//! time word `within a minute`, because a verified update installs by itself
//! within a minute (the owner) — `Installing aterm vX` again for the switch,
//! and the successor's `Finishing aterm vX`. Every phase word rides behind
//! `Details ›` and in the log ([`Message::no_excerpt`], ruling 77); only what
//! holds the install ([`holds_restatement`]) is painted, because it changes
//! what the person does. The landing is the flow row's Complete echo and a
//! RECORD in the old row's words ([`landed`]: "Updated to aterm vX").
//!
//! Where a PRESS is how the build installs — the lane is off, or has stopped
//! — the staged row is the READY row instead ("aterm vX is ready") with the
//! `Install now` capsule ([`apply_capsule_for`]); a posture nobody can press
//! and that the lane does not land within a minute is a RECORD. The words
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

/// The staleness backstop on the AUTOMATIC lane's staged row — the update's
/// flow row, "installs within a minute" (2026-09-23). That row is LIVE, not
/// held: the landing replaces it (the switch's row, then the successor's
/// Complete echo), a stand-down restates it to the ready row's hold or folds
/// it to a record, and an outcome beside it never covers it. This only
/// retires a row whose landing never came, and it outlasts the whole ladder
/// (`native_update_auto_intent::LANDS_WITHIN`) plus the longest a launched
/// successor may be held, so the row can never fold while the update it
/// announces is still on its way — the 2026-09-23 audit found the old
/// ten-minute hold (`aterm_messages::HOLD_STAGED_AUTOMATIC`, retired) folding
/// five minutes before the fifteen-minute bound it promised.
pub(crate) const STAGED_AUTOMATIC_STALE: Duration =
    Duration::from_secs(crate::native_update_auto_intent::LANDS_WITHIN.as_secs() + 120);

#[cfg(unix)]
const _: () = assert!(
    STAGED_AUTOMATIC_STALE.as_secs()
        >= crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
            + crate::app_update_handoff::PRELAUNCH_HOLD_MAX.as_secs()
);

/// The glyph of an update in progress — the flow row's from the first byte
/// to the switch. `↻` is "working", `✓` is "done", `⚠` is "needs you".
const FLOW_GLYPH: char = '\u{21bb}';
/// `✓` — the ready row and the landing's record.
const READY: char = '\u{2713}';
/// `⚠` — the flow row while it waits on a person ([`Holds::Editor`]), and an
/// outcome the person acts on.
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
/// Staged, and the automatic lane will install it by itself: the ladder lands
/// it within a minute of arming, whatever the terminal does
/// (`native_update_auto_intent::LANDS_WITHIN` plus the switch, asserted there to
/// be at most a minute). In words, never `LANDS_WITHIN / 60` arithmetic: that
/// printed "within 1 min", and "within 0 min" for any bound under a minute.
pub(crate) const INSTALLS_BY_ITSELF: &str = "installs within a minute \u{2014} keep working";
/// The automatic staged row's time slot (ruling 143): how long the person
/// waits, said in the row's stats while it is busy.
pub(crate) const WITHIN_A_MINUTE: &str = "within a minute";
/// …while only the person's typing is holding it (the ladder's keys-only
/// phase refused a park for a keystroke). Never "pause": the owner's
/// 2026-09-18 ruling keeps stall words off the update rows. PAINTED: it
/// changes what the person does (ruling 77).
pub(crate) const TYPING_HOLDS_IT: &str = "finishes when you stop typing";
/// …while unsaved editor or Settings work is holding it (the close preflight
/// refused the install for it): the one thing the person can do, instead of a
/// one-minute promise the lane cannot keep until they do it. PAINTED.
pub(crate) const EDITOR_HOLDS_IT: &str = "save or close the open editor to finish";
/// The outgoing process is handing over.
pub(crate) const INSTALLING: &str = "installing\u{2026}";
/// The successor's frames before Commit: what is typed now is queued and
/// replayed, never lost — the one sentence about the person's work, said
/// once, behind `Details ›` (it changes nothing the person does).
pub(crate) const FINISHING: &str = "almost done \u{2014} what you type is kept";
/// A download that failed: the next check retries it.
pub(crate) const DOWNLOAD_FAILED: &str = "will try again by itself";

/// Whether the lane is WORKING on the install under `posture` — the automatic
/// lane armed, landing it within a minute — and so whether its staged row is
/// the busy flow row on the glass. A lane standing down with a retry
/// scheduled installs by itself too, but the retry is ten minutes to six
/// hours away (wave-B review, 2026-09-23: a spinner over that wait
/// overstated it), so its staged build is a record.
pub(crate) fn lane_is_working(posture: Option<ApplyPosture>) -> bool {
    matches!(posture, Some(ApplyPosture::Automatic))
}

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

/// A phase title, verb first (§10's form, ruling 143): "Downloading aterm
/// v0.91.0". Says "Installing update" when there is no version (the re-exec
/// QA seam applies with none — a bare "aterm v" was measured on glass
/// 2026-09-07).
pub(crate) fn flow_title(verb: &str, version: &str) -> String {
    let v = v(version);
    if v.trim().is_empty() {
        format!("{verb} update")
    } else {
        format!("{verb} aterm v{v}")
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
    format!("aterm v{} is ready", v(version))
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
            "didn't install \u{2014} will try again later".to_string()
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

/// R35 (Staged) — a build is staged and verified. Three answers (ruling 143):
///
/// * the lane installs it within a minute ([`lane_is_working`]): the flow row
///   ON THE GLASS — `Installing aterm vX`, busy, the time word
///   [`WITHIN_A_MINUTE`] in its stats, LIVE under [`STAGED_AUTOMATIC_STALE`]
///   until the landing replaces it — a timed wait the person should expect;
/// * a press installs it ([`staged_is_decision`]): the READY row, `aterm vX is
///   ready`, Info with `✓`, how it installs behind `Details ›`, the
///   `Install now` capsule, held [`HOLD_STAGED_MANUAL`] — raised once per
///   build and posture by the host (ruling 119);
/// * anything else — a stand-down that retries later, the handoff off — is a
///   RECORD with the posture's sentence verbatim: it lands by itself (or once
///   the terminals close), and nothing on the glass could be pressed.
///
/// The App re-states the words once the lane has actually armed or stood
/// down for this build ([`restate_apply_posture`]).
pub(crate) fn staged(version: &str, build: u64, posture: Option<ApplyPosture>) -> Message {
    if lane_is_working(posture) {
        return flow(
            flow_title("Installing", version),
            INSTALLS_BY_ITSELF,
            Meter::busy(WITHIN_A_MINUTE),
            Hold::Live {
                stale_after: STAGED_AUTOMATIC_STALE,
            },
        );
    }
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

/// Re-state HOW a staged `build` installs on the live staged row — the
/// refinement the App posts once the lane has actually armed (or stood down)
/// for it, a moment after the `Staged` report painted the policy line: the
/// FULL words of [`staged`] under `posture` — title, glyph, tone, detail,
/// animation, capsule and lifetime — so a ready↔flow change re-words,
/// re-tones and re-lives the same row (the center re-arms a Live row's cap
/// from now and re-anchors a held one on glass). A posture whose words are a
/// RECORD is not a restatement: the host folds the row and records them
/// (`App::restate_staged_bar_posture`). The host applies it only to the
/// flow state's staged row for this build (`App::update_flow`), never to the
/// switch's row that replaces it.
pub(crate) fn restate_apply_posture(
    version: &str,
    build: u64,
    posture: ApplyPosture,
) -> Restatement {
    crate::messages_host::restatement_of(&staged(version, build, Some(posture)))
}

/// What holds the install of a staged build, said on its flow row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Holds {
    /// Only the person's typing (the ladder's keys-only phase): the lane is
    /// still at work, so the row keeps moving.
    Typing,
    /// Unsaved editor or Settings work (the close preflight): the lane waits
    /// on a person, so the row is still and says what to do.
    Editor,
}

/// The flow row re-worded to what holds it (words, tone, glyph and indicator
/// only — its title and lifetime stay), PAINTED: both change what the person
/// does (ruling 77). The typing words keep the row working (`↻`, Info, busy);
/// the editor words make it a wait on a person (`⚠`, Warn, STILL — ruling
/// 139: a Live row blocked on the person carries no indicator). The Warn tone
/// on the flow row replaces the separate ⚠ blocker row that used to stand
/// beside it: two rows for one fact is a grow and a shrink of every running
/// TUI. Every posture restatement sets the words back, so the next probe
/// animates it again and the refusal it meets says the editor again in the
/// same turn.
pub(crate) fn holds_restatement(h: Holds) -> Restatement {
    match h {
        Holds::Typing => Restatement {
            detail: Some(vec![TYPING_HOLDS_IT.to_string()]),
            meter: Some(Some(Meter::busy(""))),
            severity: Some(Severity::Info),
            glyph: Some(glyph(FLOW_GLYPH)),
            excerpt: Some(true),
            ..Restatement::default()
        },
        Holds::Editor => Restatement {
            detail: Some(vec![EDITOR_HOLDS_IT.to_string()]),
            meter: Some(None),
            severity: Some(Severity::Warn),
            glyph: Some(glyph(NEEDS_YOU)),
            excerpt: Some(true),
            ..Restatement::default()
        },
    }
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
/// running): a RECORD (ruling 141) — the flow row's Complete echo and the
/// landing surge are the moment. "✓ Updated to aterm vX" ("Updated to build
/// N" with no version), no claim about the shells. How long the update took
/// is a record of its own ([`installed_after`]).
pub(crate) fn landed(version: &str, build: u64) -> Message {
    let v = v(version);
    let title = if v.trim().is_empty() {
        format!("Updated to build {build}")
    } else {
        format!("Updated to aterm v{v}")
    };
    row(Severity::Success, title)
        .glyph(glyph(READY))
        .hold(Hold::LogOnly)
}

/// The landing's RECORD: how long the update took from THIS build's finished
/// download — "Installed aterm vX 42 s after it downloaded". Words distinct
/// from the landing's, so Settings ▸ Messages does not read one title
/// twice. Only when this process (or the predecessor that handed over)
/// downloaded this very build.
pub(crate) fn installed_after(version: &str, took: Duration) -> Message {
    row(
        Severity::Success,
        format!(
            "Installed aterm v{} {} after it downloaded",
            v(version),
            span_words(took)
        ),
    )
    .hold(Hold::LogOnly)
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

/// THE DOWNLOAD, ON RECORD: when a build was downloaded and verified — the
/// fact that answers "downloaded but didn't install".
pub(crate) fn downloaded(version: &str, build: u64) -> Message {
    row(
        Severity::Success,
        format!("Downloaded aterm {}", v(version)),
    )
    .line(format!("build {build}, verified and ready to install"))
    .hold(Hold::LogOnly)
}

/// THE SWITCH TO A NEW VERSION BEGINS, on record — written before the park,
/// because a line queued for after the process execs is a line never written.
/// `target` is the version being installed; `None` is a same-image switch (a
/// reload of this very build), which installs nothing whatever is staged.
pub(crate) fn switch_started(target: Option<&str>, running: &str, running_build: u64) -> Message {
    let title = target.map_or_else(
        || "Reloading aterm in place".to_string(),
        |version| format!("Installing aterm {}", v(version)),
    );
    row(Severity::Info, title)
        .glyph(glyph(FLOW_GLYPH))
        .line(format!("from aterm {} (build {running_build})", v(running)))
        .hold(Hold::LogOnly)
}

/// THE SWITCH THAT DID NOT HAPPEN, on record: an attempt [`switch_started`]
/// wrote down ended with this process still running — refused, failed, or
/// stood down because the terminal was in use (`routine`) — so the record
/// never leaves an "Installing" that was not. `why` is the attempt's own
/// account, whole.
pub(crate) fn switch_stopped(
    target: Option<&str>,
    running: &str,
    routine: bool,
    why: &str,
) -> Message {
    let running = v(running);
    let (title, detail) = match (target, routine) {
        (Some(version), true) => (
            format!("Waiting to install aterm {}", v(version)),
            format!(
                "the terminal was in use, so the switch stopped safely and aterm \
                 {running} kept running; it tries again on its own"
            ),
        ),
        (Some(version), false) => (
            format!("aterm {} was not installed", v(version)),
            format!("the switch stopped safely and aterm {running} kept running: {why}"),
        ),
        (None, true) => (
            "Waiting to reload aterm".to_string(),
            "the terminal was in use, so the reload stopped safely; it tries again on its \
             own"
            .to_string(),
        ),
        (None, false) => (
            "aterm was not reloaded".to_string(),
            format!("the reload stopped safely: {why}"),
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
    let msg = row(severity, sanitize_for_tty(title, 80))
        .sentence(detail)
        .action(Intent::ApplyUpdate { build })
        .key(KEY_OUTCOME)
        .no_excerpt();
    if severity >= Severity::Warn {
        msg.glyph(glyph(NEEDS_YOU))
    } else {
        msg.glyph(glyph(FLOW_GLYPH))
            .hold(Hold::For(HOLD_STAGED_MANUAL))
    }
}

/// R37 — an attempt that FAILED and that nothing retries, a FAILURE row
/// (ruling 143, main's "Update didn't finish"): Warn, `⚠`, the warning's
/// hold, the `Software Update` capsule — the page that says what happened.
/// `detail` is painted only when it is a blocker the person can clear
/// (`actionable`); a mechanism's account rides behind `Details ›`.
pub(crate) fn failed(title: &str, detail: &str, actionable: bool) -> Message {
    let msg = row(Severity::Warn, sanitize_for_tty(title, 80))
        .glyph(glyph(NEEDS_YOU))
        .sentence(detail)
        .action(software_update())
        .key(KEY_OUTCOME);
    if actionable { msg } else { msg.no_excerpt() }
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
        format!("it had said: {said_title}")
    } else {
        format!("it had said: {said_title} \u{2014} {said_line0}")
    };
    row(Severity::Success, "aterm updates work again")
        .line(said)
        .hold(Hold::LogOnly)
}

/// The OS notification's body for the same warning: when it started and where
/// the rest is. The updater's whole sentence — the count, a raw timestamp, the
/// cause and a command — is the log's and Software Update's, never a banner's.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
        completes(
            &staged("0.91.0", 7, Some(ApplyPosture::Automatic)),
            "Installed aterm v0.91.0",
        );
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
            landed("0.48.0", 7),
            installed_after("0.48.0", Duration::from_secs(42)),
            downloaded("0.48.0", 7),
            switch_started(Some("0.48.0"), "0.47.0", 6),
            switch_stopped(Some("0.48.0"), "0.47.0", false, "child died"),
            outcome('\u{21bb}', "Update installed", "x", Severity::Info),
            needs_install("Update installed", "x", Severity::Info, 7),
            needs_install("Couldn't install aterm v0.48.0", "x", Severity::Warn, 7),
            failed("Update didn't finish", "", false),
            health_warning("aterm can't install updates", "3 checks"),
            health_recovered("aterm can't install updates", "since Sep 14"),
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
            landed("0.91.0", 7),
            installed_after("0.91.0", Duration::from_secs(42)),
            downloaded("0.91.0", 7),
            switch_started(Some("0.91.0"), "0.90.0", 6),
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
                crate::app_update_screen::UPDATE_INSTALLED_TITLE,
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
            failed("Update waits for the editor", EDITOR_HOLDS_IT, true),
            health_recovered("aterm can't install updates", "since Sep 14"),
            staged("0.91.0", 7, None),
        ];
        for posture in every_posture() {
            all.push(staged("0.91.0", 7, Some(posture)));
        }
        for class in ["apply", "pipeline", "stage", "manifest"] {
            all.push(health_warning(
                aterm_update::health_failing_title(class),
                "3 failed checks in a row since 2026-09-14T22:04:36Z: x.",
            ));
        }
        // The holds restated onto the automatic flow row.
        for h in [Holds::Typing, Holds::Editor] {
            let mut m = staged("0.91.0", 7, Some(ApplyPosture::Automatic));
            let r = holds_restatement(h);
            m.detail = r.detail.expect("words");
            m.meter = r.meter.expect("an indicator decision");
            m.severity = r.severity.expect("a tone");
            all.push(m);
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
    /// RESTART (ruling 143). Where the lane lands it within a minute the row is
    /// the flow row on the glass — `Installing aterm vX`, busy, `within a
    /// minute` in its time slot, LIVE under the backstop that outlasts the
    /// ladder; where a press installs it, the ready row with the `Install now`
    /// capsule, held for the press; every other posture is a RECORD.
    #[test]
    fn a_staged_row_says_how_the_update_installs_and_never_asks_for_a_restart() {
        use ApplyPosture as P;
        for posture in every_posture() {
            let m = staged("0.67.0", 7, Some(posture));
            if lane_is_working(Some(posture)) {
                assert_eq!(m.title, "Installing aterm v0.67.0", "{posture:?}");
                assert_eq!(m.glyph.ch(), FLOW_GLYPH);
                assert_eq!(m.severity, Severity::Info);
                assert_eq!(
                    m.hold,
                    Hold::Live {
                        stale_after: STAGED_AUTOMATIC_STALE
                    }
                );
                assert!(busy(&m), "{posture:?}: a timed wait moves");
                assert_eq!(stats_of(&m), WITHIN_A_MINUTE, "the time word");
                assert!(!m.excerpt);
            } else if staged_is_decision(Some(posture)) {
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
            "didn't install \u{2014} will try again later"
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
    /// armed or stood down — the FULL words, so a ready↔flow change re-words,
    /// re-tones and re-lives the same row.
    #[test]
    fn a_posture_restatement_carries_the_full_words() {
        use ApplyPosture as P;
        let r = restate_apply_posture("0.67.0", 7, P::Automatic);
        assert_eq!(r.title.as_deref(), Some("Installing aterm v0.67.0"));
        assert_eq!(r.detail, Some(vec![INSTALLS_BY_ITSELF.to_string()]));
        assert_eq!(r.actions, Some(Vec::new()));
        assert_eq!(r.severity, Some(Severity::Info));
        assert_eq!(r.glyph, Some(glyph(FLOW_GLYPH)));
        assert_eq!(r.excerpt, Some(false));
        assert_eq!(
            r.hold,
            Some(Hold::Live {
                stale_after: STAGED_AUTOMATIC_STALE
            })
        );
        assert_eq!(r.meter, Some(Some(Meter::busy(WITHIN_A_MINUTE))));
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

    /// WHAT HOLDS THE INSTALL is said on the flow row: typing keeps it
    /// working (`↻`, moving); unsaved editor work makes it a wait on a person
    /// (`⚠` Warn, still — no animation wake for a wait).
    #[test]
    fn what_holds_the_install_is_a_restatement_of_the_flow_row() {
        let typing = holds_restatement(Holds::Typing);
        assert_eq!(typing.detail, Some(vec![TYPING_HOLDS_IT.to_string()]));
        assert_eq!(typing.severity, Some(Severity::Info));
        assert_eq!(typing.glyph, Some(glyph(FLOW_GLYPH)));
        assert_eq!(typing.meter, Some(Some(Meter::busy(""))));
        assert_eq!(
            typing.excerpt,
            Some(true),
            "it changes what the person does"
        );
        assert_eq!(typing.title, None, "the title stays");
        assert_eq!(typing.hold, None, "and so does its lifetime");
        let editor = holds_restatement(Holds::Editor);
        assert_eq!(editor.detail, Some(vec![EDITOR_HOLDS_IT.to_string()]));
        assert_eq!(editor.severity, Some(Severity::Warn));
        assert_eq!(editor.glyph, Some(glyph(NEEDS_YOU)));
        assert_eq!(editor.meter, Some(None), "a wait on a person is still");
        assert_eq!(editor.excerpt, Some(true));
        assert_eq!(editor.hold, None);
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
            TYPING_HOLDS_IT,
            EDITOR_HOLDS_IT,
            INSTALLING,
            FINISHING,
            DOWNLOAD_FAILED,
            WITHIN_A_MINUTE,
        ]
        .map(str::to_string)
        .to_vec();
        for posture in every_posture()
            .into_iter()
            .filter(|p| lane_is_working(Some(*p)))
        {
            details.push(staged_detail(posture));
        }
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
        sentences.extend(
            [
                DOWNLOADING,
                CHECKING,
                TYPING_HOLDS_IT,
                EDITOR_HOLDS_IT,
                DOWNLOAD_FAILED,
            ]
            .map(str::to_string),
        );
        for m in [
            installing("0.88.0"),
            finishing("0.88.0"),
            landed("0.88.0", 7),
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
        let l = landed("9.9.9", 7);
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
        assert_eq!(landed("", 7).title, "Updated to build 7");
    }

    /// THE UPDATE'S RECORDS: when it downloaded, when the switch began or
    /// stopped, and how long it took — each a `LogOnly` record with no key.
    #[test]
    fn the_update_records_say_what_happened_when() {
        let d = downloaded("0.79.0", 7);
        assert_eq!(d.title, "Downloaded aterm 0.79.0");
        assert_eq!(d.detail, vec!["build 7, verified and ready to install"]);
        assert_eq!(d.hold, Hold::LogOnly);
        assert_eq!(d.key, None);
        let s = switch_started(Some("0.79.0"), "0.78.0", 6);
        assert_eq!(s.title, "Installing aterm 0.79.0");
        assert_eq!(s.detail, vec!["from aterm 0.78.0 (build 6)"]);
        assert_eq!(s.hold, Hold::LogOnly);
        assert_eq!(
            switch_started(None, "0.78.0", 6).title,
            "Reloading aterm in place"
        );
        let waiting = switch_stopped(Some("0.79.0"), "0.78.0", true, "typing");
        assert_eq!(waiting.title, "Waiting to install aterm 0.79.0");
        assert_eq!(waiting.severity, Severity::Info);
        let not = switch_stopped(Some("0.79.0"), "0.78.0", false, "the proof timed out");
        assert_eq!(not.title, "aterm 0.79.0 was not installed");
        assert_eq!(not.severity, Severity::Warn);
        assert!(not.detail.join(" ").ends_with("the proof timed out"));
        assert_eq!(
            switch_stopped(None, "0.78.0", true, "x").title,
            "Waiting to reload aterm"
        );
        assert_eq!(
            switch_stopped(None, "0.78.0", false, "x").title,
            "aterm was not reloaded"
        );
        let took = installed_after("0.79.0", Duration::from_secs(42));
        assert_eq!(
            took.title,
            "Installed aterm v0.79.0 42 s after it downloaded"
        );
        assert_eq!(took.hold, Hold::LogOnly);
        assert_ne!(
            took.title,
            landed("0.79.0", 7).title,
            "two titles, one each"
        );
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
            crate::app_update_screen::UPDATE_INSTALLED_TITLE,
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
        let blocked = failed("Update waits for the editor", EDITOR_HOLDS_IT, true);
        assert!(blocked.excerpt, "a blocker the person clears is painted");
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
            vec!["it had said: aterm can't install updates \u{2014} since Sep 14"]
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
        surfaces.push((
            "updater outcome",
            crate::native_updater_service::NativeUpdaterService::installed_activation_outcome(7),
        ));
        surfaces.push((
            "updater outcome",
            crate::native_updater_service::NativeUpdaterService::apply_attempt_stopped_outcome(
                "child died",
            ),
        ));
        for (title, detail) in [
            (
                crate::app_update_screen::UPDATE_INSTALLED_TITLE,
                crate::app_update_screen::UPDATE_INSTALLED_DETAIL,
            ),
            (
                crate::app_update_screen::UPDATE_DIDNT_INSTALL,
                INSTALL_FROM_MENU,
            ),
            ("Couldn't install aterm v9.9.9", INSTALL_FROM_MENU),
            ("Couldn't install aterm v9.9.9", "will try again by itself"),
            ("Couldn't download aterm v9.9.9", DOWNLOAD_FAILED),
            ("Update didn't finish", ""),
        ] {
            surfaces.push(("outcome row", format!("{title} — {detail}")));
        }
        for m in [
            installing("9.9.9"),
            finishing("9.9.9"),
            landed("9.9.9", 7),
            landed("", 7),
            download_failed("9.9.9", "x"),
            download_postponed("busy"),
            downloaded("9.9.9", 7),
            installed_after("9.9.9", Duration::from_secs(42)),
            switch_started(Some("9.9.9"), "9.9.8", 6),
            switch_started(None, "9.9.8", 6),
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
        for holds in [Holds::Typing, Holds::Editor] {
            surfaces.push((
                "flow row (holds)",
                holds_restatement(holds)
                    .detail
                    .unwrap_or_default()
                    .join(" "),
            ));
        }
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

    /// NOTHING THIS MODULE SAYS ASKS FOR A RESTART OR A RELAUNCH (owner
    /// ruling, 2026-08-30): every builder's own words, under every posture.
    #[test]
    fn no_update_word_asks_for_a_restart_or_a_relaunch() {
        let mut sentences: Vec<String> = every_posture()
            .iter()
            .map(|posture| staged_detail(*posture))
            .collect();
        sentences.push(staged_detail_unknown());
        for m in [
            installing("9.9.9"),
            finishing("9.9.9"),
            landed("9.9.9", 7),
            landed("", 7),
            download_postponed("busy"),
        ] {
            sentences.push(m.title);
            sentences.extend(m.detail);
        }
        assert!(sentences.len() >= 20, "the guard covers the lane");
        for text in sentences {
            let lower = text
                .replace("ATERM_DEBUG_RELAUNCH_NUDGE", "<seam>")
                .to_lowercase();
            assert!(
                !lower.contains("restart")
                    && !lower.contains("relaunch")
                    && !lower.contains("reopen"),
                "asks for a restart: {text:?}"
            );
        }
    }

    /// AND THE SOURCE SAYS IT NOWHERE. The fault-word guard above checks the
    /// VALUES the builders produce, so a surface nobody added to a list, or a
    /// sentence its needles cannot see — "quit and open aterm again", "activates
    /// it at the next launch" — passes it. This one reads the SHIPPING lines of every
    /// update-lane file in this crate, plus every `.rs` file of `aterm-cli` and
    /// `aterm-update` (test code dropped, comment lines dropped), for the
    /// shapes a restart prompt takes. The things that genuinely
    /// apply at the next launch (`columns`/`lines`, the GPU backend, `net.key`,
    /// the `[packages]` service, the Windows backdrop, the config validator's
    /// dead worker) are allow-listed by an anchor on the same line, and the
    /// handoff-off posture's fallback clause ("… or once every terminal is
    /// closed") carries its own. The shell twin, `tools/grep_guard.sh` B12, reads
    /// every file in the crate with the same needles and anchors. Both lists are
    /// assembled at runtime so no test source can trip them.
    #[test]
    fn no_update_lane_source_prompts_a_restart() {
        let join = |parts: &[(&str, &str)]| -> Vec<String> {
            parts.iter().map(|(a, b)| format!("{a}{b}")).collect()
        };
        let needles = join(&[
            ("restart ", "aterm"),
            ("restart ", "now"),
            ("restart ", "to apply"),
            ("restart ", "to finish"),
            ("restart ", "to update"),
            ("relaunch ", "aterm"),
            ("relaunch ", "once"),
            ("relaunch ", "now"),
            ("relaunch ", "to "),
            ("before ", "relaunch"),
            (" on ", "relaunch"),
            ("& ", "relaunch"),
            ("quit and ", "open"),
            ("quit and ", "reopen"),
            ("open aterm ", "again"),
            ("start aterm ", "again"),
            ("re-", "launch"),
            ("re-open ", "aterm"),
            ("re-open ", "the app"),
            ("re-open ", "it"),
            ("re", "boot"),
            ("next ", "launch"),
            ("restart ", "required"),
            ("requires ", "a restart"),
            ("needs ", "a restart"),
            ("reopen ", "aterm"),
            ("reopen ", "the app"),
            ("relaunch ", "required"),
            ("needs ", "a relaunch"),
            ("relaunch ", "the app"),
        ]);
        let anchors = join(&[
            ("once every ", "terminal is closed"),
            ("applies ", "next launch"),
            ("closing or ", "next launch"),
            ("automatic ", "checks"),
            ("manual ", "now"),
            ("[", "packages]"),
            ("next ", "package operation"),
            ("net", ".key"),
            ("columns", "/lines"),
            ("gpu ", "applies"),
            ("gpu rendering ", "(restart)"),
            ("this ", "launch"),
            ("open a new window ", "or restart"),
            ("config ", "validator"),
            ("back", "drop"),
            ("own apply ", "lane"),
            ("launch is the ", "fallback"),
            ("retired-wording ", "detector"),
            // The dead accessibility publisher, added 2026-08-31 alongside
            // a11y_backend.rs joining the file list below. A restart is the
            // only recovery there — one backend per process, by `OnceLock` —
            // and this ruling is about the UPDATE lane, which updates in place.
            ("restart aterm ", "to retry"),
        ]);
        let gui = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<std::path::PathBuf> = [
            // a11y_backend.rs was MISSING here until 2026-08-31, and that is how
            // b571a20b9's restart prompt shipped with this test green: only
            // tools/grep_guard.sh's B12 — which reads every shipping line of the
            // crate rather than an enumerated set — ever saw it. An enumerated
            // list is a list somebody has to remember to extend.
            "a11y_backend.rs",
            "update_words.rs",
            "toolchain_words.rs",
            "messages_host.rs",
            "message_band.rs",
            "message_inbox.rs",
            "message_reporters.rs",
            "menu.rs",
            "palette.rs",
            "app_palette.rs",
            "robi_bubble.rs",
            "consent_card.rs",
            "relaunch_notice.rs",
            "app_update_screen.rs",
            "app_update_handoff.rs",
            "app_native.rs",
            "native_settings.rs",
            "native_ui.rs",
            "native_updater_service.rs",
            "native_update_admission.rs",
            "update_apply_trouble.rs",
            "control.rs",
        ]
        .iter()
        .map(|file| gui.join(file))
        .collect();
        // The CLI and the updater ship update-lane strings of their own (the
        // `update` verb's output, the updater's log and status lines), which
        // the include-set above could never see — every `.rs` file of both
        // crates, derived, so a new module cannot dodge the scan.
        for dir in ["../aterm-cli/src", "../aterm-update/src"] {
            let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
            let mut siblings: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
                .map(|entry| entry.expect("read_dir entry").path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
                .collect();
            siblings.sort();
            assert!(!siblings.is_empty(), "{} scanned no files", dir.display());
            files.append(&mut siblings);
        }
        let mut scanned = 0usize;
        for path in files {
            let file = path.display();
            let source =
                std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{file}: {error}"));
            // Test code is not a shipping line. The gui files keep their tests
            // in one trailing `mod tests`; the CLI and the updater interleave
            // several `#[cfg(test)] mod …` blocks with production, so gate on
            // the attribute the way grep_guard's `np_strip` does: a column-0
            // `#[cfg(test)]` opens a skip that a block ends at its column-0 `}`
            // and a single item ends at its `;`.
            let mut in_test_block = false;
            let mut test_item_armed = false;
            for (n, line) in source.lines().enumerate() {
                if in_test_block {
                    if line == "}" {
                        in_test_block = false;
                    }
                    continue;
                }
                if test_item_armed {
                    if line.ends_with('{') {
                        in_test_block = true;
                        test_item_armed = false;
                    } else if line.ends_with(';') {
                        test_item_armed = false;
                    }
                    continue;
                }
                if line == "#[cfg(test)]" {
                    test_item_armed = true;
                    continue;
                }
                if line == "mod tests {" {
                    break;
                }
                let code = line.trim_start();
                if code.starts_with("//") || code.starts_with('*') || code.starts_with("/*") {
                    continue;
                }
                scanned += 1;
                let lower = line.to_lowercase();
                if anchors.iter().any(|anchor| lower.contains(anchor.as_str())) {
                    continue;
                }
                for needle in &needles {
                    assert!(
                        !lower.contains(needle.as_str()),
                        "{file}:{} prompts a restart ({needle:?}): {}\n\
                         an update is applied in place by the in-session handoff — the \
                         shells keep running — and no update surface asks for a restart; \
                         a setting that genuinely applies at the next launch names its \
                         anchor on the same line",
                        n + 1,
                        line.trim()
                    );
                }
            }
        }
        assert!(scanned > 20_000, "the guard read the lane: {scanned} lines");
    }
}
