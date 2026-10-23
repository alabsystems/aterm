// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The update lane's words.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aterm_messages::exceptions::{Entry, Family};
use aterm_messages::need::{Ask, Reason, Resurface, StandingWhy, Then, Why};
use aterm_messages::{
    Amount, DETAIL_LINE_CAP, Glyph, HOLD_STAGED_MANUAL, Hold, Intent, Message, Meter,
    PROGRESS_GRACE, Restatement, STALE_HANDOFF, STALE_UPDATE, Severity, Unit, tags,
};
use atpkg::progress::sanitize_for_tty;

use crate::app_update_handoff::HandoffUnavailable;
use crate::toolchain_words::{fill_permille, fmt_bytes};

/// The supersede key of the lane's own flow row: downloading → checking → staged → installing → finishing, each replacing the last.
pub(crate) const KEY_PROGRESS: &str = "update.progress";
/// An apply-lane outcome's key — never [`KEY_PROGRESS`]: both rows stay live, side by side.
pub(crate) const KEY_OUTCOME: &str = "update.outcome";
/// The key of the row that says a person's unsaved work holds the automatic lane ([`waits_for_you`]).
pub(crate) const KEY_WAITS_FOR_YOU: &str = "update.waits-for-you";
/// The health warnings' family key.
pub(crate) const KEY_HEALTH: &str = "update.health";
/// The key of the Linux notice that an installed executable waits for the next aterm a person starts ([`install_awaits_start`]).
pub(crate) const KEY_AWAITS_START: &str = "update.awaits-start";
/// The key of the Linux row that says a downloaded build is held for a person ([`linux_held`]).
pub(crate) const KEY_HELD: &str = "update.held";

/// Which half of updating a health warning is about: the warning's stable identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HealthKind {
    /// A verified build keeps failing to install (the updater's `apply` class, its overdue notice, the automatic lane's convergence).
    Install,
    /// Downloads fail or will not verify (`pipeline`, `stage`).
    Download,
    /// The newest release cannot be read or trusted (`manifest`, and any class this build does not know).
    Check,
    /// The window's watchdog: the update check stopped answering ([`CHECKER_STALLED_TITLE`]).
    Stalled,
    /// The window's watchdog: another aterm holds the machine's update check without making progress ([`checker_lock_held_body`]).
    LockHeld,
}

impl HealthKind {
    /// Every kind, in the order the page lists them.
    pub(crate) const ALL: [Self; 5] = [
        Self::Install,
        Self::Download,
        Self::Check,
        Self::Stalled,
        Self::LockHeld,
    ];

    /// The kind's word in the digest's key (`update.review.<slug>`), never the record's (`update.health.<slug>`).
    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Download => "download",
            Self::Check => "check",
            Self::Stalled => "stalled",
            Self::LockHeld => "lock-held",
        }
    }

    /// The kind an updater class names (`aterm_update::HealthNotify`'s classes).
    pub(crate) fn of_class(class: &str) -> Self {
        match class {
            "apply" => Self::Install,
            "pipeline" | "stage" => Self::Download,
            _ => Self::Check,
        }
    }

    /// The kind of a failing title the updater sent, read back to its class by the updater's own reader (`aterm_update::health_failing_class`), which knows today's words and older ones.
    pub(crate) fn of_updater_title(title: &str) -> Self {
        aterm_update::health_failing_class(title).map_or(Self::Check, Self::of_class)
    }

    /// The row's supersede key: one per kind, under [`KEY_HEALTH`].
    pub(crate) const fn key(self) -> &'static str {
        match self {
            Self::Install => "update.health.install",
            Self::Download => "update.health.download",
            Self::Check => "update.health.check",
            Self::Stalled => "update.health.stalled",
            Self::LockHeld => "update.health.lock-held",
        }
    }

    /// The kind a health row stands for, from its key, or, for a row an older build raised under the family key (carried across a handoff), from that build's words.
    pub(crate) fn of_row(msg: &Message) -> Option<Self> {
        let key = msg.key.as_deref()?;
        if key == KEY_HEALTH {
            return Some(if CHECKER_STALLED_TITLES.contains(&msg.title.as_str()) {
                Self::Stalled
            } else {
                Self::of_updater_title(&msg.title)
            });
        }
        Self::of_key(key)
    }

    /// The kind whose row key is `key` ([`Self::key`]); `None` for any other.
    pub(crate) fn of_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.key() == key)
    }
}

/// The live health warning a test reads, whatever its kind (the oldest first).
#[cfg(test)]
pub(crate) trait LiveHealth {
    fn live_health(&self) -> Option<&aterm_messages::Live>;
}

#[cfg(test)]
impl LiveHealth for aterm_messages::MessageCenter {
    fn live_health(&self) -> Option<&aterm_messages::Live> {
        self.live_rows()
            .find(|l| HealthKind::of_row(&l.msg).is_some())
    }
}

/// The flow row's glyph while it checks what arrived: `↻` is working; the phases that
/// move a build in wear [`MOVING_IN`].
const FLOW_GLYPH: char = '\u{21bb}';
/// `⇣`: a build moving in (downloading, installing, finishing) and the ready build that
/// waits for `Install now` (`✓` reads as done, a still `↻` as stalled).
const MOVING_IN: char = '\u{21e3}';
/// `✓` — the landing's record, and the ready build the lane installs by
/// itself (a record).
const READY: char = '\u{2713}';
/// `⚠` — an outcome the person acts on.
const NEEDS_YOU: char = '\u{26a0}';
/// `ℹ` — a record of the lane working (a record wears its
/// outcome's mark, never the working `↻` of a row still moving).
const NOTED: char = '\u{2139}';

/// How a staged build will be applied: what the staged row's detail may promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyPosture {
    /// `update.auto_apply` on (the default) and no veto.
    Automatic,
    /// The automatic lane is armed, but this process kept the image it was launched with and has no terminal open (`App::kept_image_waits_for_terminal`).
    WaitsForTerminal,
    /// `[update] auto_apply = false`.
    ManualByConfig,
    /// An environment veto for this run.
    VetoedByEnv { var: &'static str },
    /// The automatic lane stood down for this build after a physical handoff failure.
    ManualOnlyLatched { lapses: bool },
    /// The stood-down build is the installed activation and its boot trial has no launch to spare (`App::latched_activation_trial_spent`).
    TrialSpent,
    /// The in-session handoff cannot run in this process.
    HandoffDisabled {
        why: HandoffUnavailable,
        veto: Option<AutoApplyVeto>,
        image_kept: bool,
    },
}

/// Why the automatic apply lane would not arm even if the handoff were available.
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
            Self::Config => aterm_update_core::settings::install_switch_off(),
            Self::Env { var } => format!("${var} is set"),
        }
    }
}

/// The manual affordance, named the same way on every surface.
pub(crate) const INSTALL_FROM_MENU: &str = if cfg!(target_os = "macos") {
    "install it from the Version menu"
} else {
    "install it from the Version menu or the \u{21bb} button"
};

/// The flow row's phase words, point first and at most 40 characters each, in the order they happen.
pub(crate) const DOWNLOADING: &str = "downloading";
/// The container arrived: the size, digest, signature and Gatekeeper checks and the stage publish, seconds each.
pub(crate) const CHECKING: &str = "checking the download";
/// Staged, and the automatic lane installs it by itself: the staged record's detail.
pub(crate) const INSTALLS_BY_ITSELF: &str = "installs by itself within a minute";
/// The outgoing process is handing over.
#[cfg(any(unix, test))]
pub(crate) const INSTALLING: &str = "installing\u{2026}";
/// The successor's frames before Commit: what is typed now is queued and replayed, never lost.
pub(crate) const FINISHING: &str = "almost done \u{2014} what you type is kept";
/// The lane's one promise that it retries, said one way on every surface, with ` later` or ` in 6 h` after it where a time is known.
pub(crate) const TRIES_AGAIN: &str = "tries again by itself";
/// The one way out of a stood-down activation whose boot trial is spent ([`ApplyPosture::TrialSpent`]).
pub(crate) const NEWER_RELEASE_REPLACES: &str = "a newer release replaces it by itself";

/// Start a sentence: `checked 3 min ago` → `Checked 3 min ago`.
#[must_use]
pub(crate) fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// How a staged build is named to a person: `aterm vX`, or `build N` where its version string is the running build's own.
#[must_use]
pub(crate) fn staged_build_name(build: u64, version: &str) -> String {
    if version == crate::build_info::version_display() {
        format!("build {build}")
    } else {
        format!("aterm v{version}")
    }
}

/// A build on hold ([`ApplyPosture::TrialSpent`]), named `what` ([`staged_build_name`]), in one sentence with the staged record's words ([`staged_detail`]).
#[must_use]
pub(crate) fn trial_spent_refusal(what: &str) -> String {
    format!(
        "{what} is on hold: {}",
        staged_detail(ApplyPosture::TrialSpent)
    )
}
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

/// A version as every update title and record names it: `aterm v0.91.0`, never a bare `aterm 0.91.0`.
pub(crate) fn aterm_v(version: &str) -> String {
    format!("aterm v{}", v(version))
}

/// A phase title, verb first: "Downloading aterm v0.91.0".
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
    let mark = if phase == CHECKING {
        FLOW_GLYPH
    } else {
        MOVING_IN
    };
    row(Severity::Info, title)
        .glyph(glyph(mark))
        .line(phase)
        .no_excerpt()
        .meter(meter)
        .hold(hold)
        .key(KEY_PROGRESS)
}

/// The ready row's title, where a press or closed terminals install the build rather than the lane: "aterm vX is ready".
pub(crate) fn staged_title(version: &str) -> String {
    format!("{} is ready", aterm_v(version))
}

/// The staged record's title for [`ApplyPosture::TrialSpent`].
pub(crate) fn staged_on_hold_title(version: &str) -> String {
    format!("{} is on hold", aterm_v(version))
}

/// The `Install now` capsule only where a press is the way the build installs.
pub(crate) fn apply_capsule_for(posture: Option<ApplyPosture>, build: u64) -> Option<Intent> {
    match posture {
        Some(
            ApplyPosture::HandoffDisabled { .. }
            | ApplyPosture::Automatic
            | ApplyPosture::WaitsForTerminal
            | ApplyPosture::ManualOnlyLatched { lapses: true }
            | ApplyPosture::TrialSpent,
        ) => None,
        _ => Some(Intent::ApplyUpdate { build }),
    }
}

/// Whether a staged build is a decision (the ready row whose `Install now` is how it installs).
pub(crate) fn staged_is_decision(posture: Option<ApplyPosture>) -> bool {
    apply_capsule_for(posture, 0).is_some()
}

/// The staged row's detail: how the build installs, point first.
#[must_use]
pub(crate) fn staged_detail(posture: ApplyPosture) -> String {
    match posture {
        ApplyPosture::Automatic => INSTALLS_BY_ITSELF.to_string(),
        ApplyPosture::WaitsForTerminal => {
            "installs by itself once a terminal tab is open".to_string()
        }
        ApplyPosture::ManualByConfig => aterm_update_core::settings::install_switch_off(),
        ApplyPosture::VetoedByEnv { var } => format!("${var} is set \u{2014} {INSTALL_FROM_MENU}"),
        ApplyPosture::ManualOnlyLatched { lapses: true } => {
            format!("didn't install \u{2014} {TRIES_AGAIN} later")
        }
        ApplyPosture::ManualOnlyLatched { lapses: false } => {
            "automatic install didn't work".to_string()
        }
        ApplyPosture::TrialSpent => {
            format!("starting it would roll it back \u{2014} {NEWER_RELEASE_REPLACES}")
        }
        ApplyPosture::HandoffDisabled {
            why,
            veto: None,
            image_kept: false,
        } => format!(
            "{} \u{2014} installs once every terminal is closed",
            why.cause()
        ),
        ApplyPosture::HandoffDisabled {
            why,
            veto: Some(veto),
            image_kept: false,
        } => format!(
            "{}; {} \u{2014} {INSTALL_FROM_MENU} once every terminal is closed",
            why.cause(),
            veto.clause()
        ),
        ApplyPosture::HandoffDisabled {
            why,
            veto: None,
            image_kept: true,
        } => format!(
            "{} \u{2014} installs once aterm is quit and opened again",
            why.cause()
        ),
        ApplyPosture::HandoffDisabled {
            why,
            veto: Some(veto),
            image_kept: true,
        } => format!(
            "{}; {} \u{2014} {INSTALL_FROM_MENU} once aterm is quit and opened again",
            why.cause(),
            veto.clause()
        ),
    }
}

/// The Staged line for a caller that computed no posture: only the manual affordance, which is true whatever the policy.
pub(crate) fn staged_detail_unknown() -> String {
    INSTALL_FROM_MENU.to_string()
}

/// The staged sentence as the row's detail lines.
pub(crate) fn staged_detail_lines(posture: Option<ApplyPosture>) -> Vec<String> {
    let sentence = posture.map_or_else(staged_detail_unknown, staged_detail);
    aterm_messages::text::split_sentence(&sentence, DETAIL_LINE_CAP)
}

/// Staged: a build is staged and verified.
pub(crate) fn staged(version: &str, build: u64, posture: Option<ApplyPosture>) -> Message {
    if posture == Some(ApplyPosture::TrialSpent) {
        return row(Severity::Info, staged_on_hold_title(version))
            .glyph(glyph(NOTED))
            .lines(staged_detail_lines(posture))
            .hold(Hold::LogOnly)
            .key(KEY_PROGRESS);
    }
    let title = staged_title(version);
    match apply_capsule_for(posture, build) {
        Some(install) => row(Severity::Info, title)
            .glyph(glyph(MOVING_IN))
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

/// Re-state how a staged `build` installs on the live ready row once the lane has actually armed (or stood down) for it.
pub(crate) fn restate_apply_posture(
    version: &str,
    build: u64,
    posture: ApplyPosture,
) -> Restatement {
    crate::messages_host::restatement_of(&staged(version, build, Some(posture)))
}

/// One report from inside the updater's own check ([`aterm_update::Progress`]).
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
        P::Withheld { detail } => download_withheld(flow_version, detail),
    }
}

/// A download that failed, on record (the next check retries it, and the flow row it ends is the Fault echo).
pub(crate) fn download_failed(flow_version: &str, detail: &str) -> Message {
    let v = v(flow_version);
    let title = if v.trim().is_empty() {
        "Couldn't download the update".to_string()
    } else {
        format!("Couldn't download aterm v{v}")
    };
    row(Severity::Warn, title)
        .glyph(glyph(NEEDS_YOU))
        .line(DOWNLOAD_FAILED)
        .sentence(detail)
        .hold(Hold::LogOnly)
}

/// A download the updater postponed (a metered network, a busy machine): no row, since the lane comes back to it.
pub(crate) fn download_postponed(detail: &str) -> Message {
    row(Severity::Info, "Update download postponed")
        .glyph(glyph(NOTED))
        .sentence(detail)
        .hold(Hold::LogOnly)
}

/// A download the updater withheld.
pub(crate) fn download_withheld(flow_version: &str, detail: &str) -> Message {
    let v = v(flow_version);
    let title = if v.trim().is_empty() {
        "Update not installed".to_string()
    } else {
        format!("Not installing aterm v{v}")
    };
    row(Severity::Info, title)
        .glyph(glyph(NOTED))
        .sentence(detail)
        .hold(Hold::LogOnly)
}

/// The switch begins; the outgoing process is about to park its readers.
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

/// The successor before Commit, which inherited the parent's flow row across the carry and says its own phase in it.
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

/// The new build took over (or a cold-lane boot found it already running).
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

/// An update that could not carry every tab's scrollback.
#[cfg(any(unix, test))]
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

/// The row for `lines` of scrollback an update left behind in `tabs` tabs.
#[cfg(any(unix, test))]
pub(crate) fn scrollback_left_behind(
    lines: u64,
    tabs: usize,
    withheld: u64,
    held: u64,
    held_failed: u64,
) -> Message {
    if withheld == 0 && held == 0 && held_failed == 0 {
        return scrollback_lost(lines, tabs);
    }
    if held == 0 && held_failed == 0 && withheld >= lines {
        return scrollback_withheld(lines, tabs);
    }
    let title = if held_failed == 0 && withheld.saturating_add(held) >= lines {
        "Update left scrollback behind"
    } else {
        "Couldn't carry all scrollback"
    };
    let mut message = row(Severity::Warn, title)
        .glyph(glyph(NEEDS_YOU))
        .line(scrollback_lost_words(lines, tabs));
    if withheld > 0 {
        message = message.line(format!(
            "{withheld} of them because the new version asked this update to carry no scrollback"
        ));
    }
    if held > 0 {
        message = message.line(format!(
            "{held} of them in panes kept open after their command ended, whose screen alone \
             crossed"
        ));
    }
    if held_failed > 0 {
        message = message.line(format!(
            "{held_failed} of them in panes kept open after their command ended, which this \
             update failed to carry"
        ));
    }
    message
        .sentence("`aterm ctl status` in a tab says history_lost= for it")
        .hold(Hold::Default)
        .key(KEY_SCROLLBACK)
}

/// For a carry the successor's handoff policy withheld.
#[cfg(any(unix, test))]
pub(crate) fn scrollback_withheld(lines: u64, tabs: usize) -> Message {
    row(Severity::Warn, "Update left scrollback behind")
        .glyph(glyph(NEEDS_YOU))
        .line(scrollback_lost_words(lines, tabs))
        .sentence(
            "the new version asked this update to carry no scrollback; `aterm ctl status` in a \
             tab says history_lost= for it",
        )
        .hold(Hold::Default)
        .key(KEY_SCROLLBACK)
}

/// The supersede key for scrollback loss: one scrollback row per update.
#[cfg(any(unix, test))]
pub(crate) const KEY_SCROLLBACK: &str = "update.scrollback";

/// The supersede key of the carried-Settings-draft loss row.
pub(crate) const KEY_SETTINGS_DRAFTS: &str = "update.settings-drafts";

/// What a seamless successor shows in a pane the outgoing process handed no session for (`RestoreManifest::placed_for_handed`).
pub(crate) const PANE_NOT_CARRIED: &str =
    "Nothing was running in this pane when aterm updated, so there was no session to carry.";

/// D5: a desk the update folded to fit.
pub(crate) fn layout_folded(fold: &crate::restore::LayoutFold) -> Option<Message> {
    if !fold.folded() {
        return None;
    }
    let tabs_line = format!(
        "a window holds at most {} tabs: the tabs past that are in the next window with room",
        crate::restore::MAX_TABS_PER_WINDOW
    );
    let message = if fold.windows > 0 {
        let title = match fold.windows {
            1 => "1 window was merged into tabs during the update".to_string(),
            n => format!("{n} windows were merged into tabs during the update"),
        };
        let mut message = row(Severity::Info, title).line(format!(
            "an update brings back at most {} windows: the tabs of the rest are in the last one",
            crate::restore::MAX_WINDOWS
        ));
        if fold.tabs > 0 {
            message = message.line(match fold.tabs {
                1 => "1 tab moved to another window too".to_string(),
                n => format!("{n} tabs moved to another window too"),
            });
            message = message.line(tabs_line);
        }
        message
    } else {
        let title = match fold.tabs {
            1 => "1 tab was moved to another window during the update".to_string(),
            n => format!("{n} tabs were moved to another window during the update"),
        };
        row(Severity::Info, title).line(tabs_line)
    };
    Some(
        message
            .glyph(glyph(NOTED))
            .hold(Hold::Default)
            .key(KEY_LAYOUT_FOLDED),
    )
}

/// The supersede key of the folded-desk record: one per update.
pub(crate) const KEY_LAYOUT_FOLDED: &str = "update.layout-folded";

/// How many lost drafts the row spells out, each on its own line.
const SETTINGS_DRAFTS_SHOWN: usize = 16;

/// An update carried unsaved Settings text across and the new build could not reopen some of it.
pub(crate) fn settings_drafts_not_reopened(
    lost: &[crate::restore::SettingsDraftRestore],
    unreadable: usize,
    saved_in: Option<&std::path::Path>,
) -> Message {
    let count = lost.len().saturating_add(unreadable);
    let title = if count == 1 {
        "Couldn't reopen a Settings draft".to_string()
    } else {
        format!("Couldn't reopen {count} Settings drafts")
    };
    let mut m = row(Severity::Warn, title).glyph(glyph(NEEDS_YOU));
    for draft in lost.iter().take(SETTINGS_DRAFTS_SHOWN) {
        m = m.line(format!(
            "{}: {}",
            sanitize_for_tty(&draft.key, 64),
            sanitize_for_tty(&draft.text, DETAIL_LINE_CAP)
        ));
    }
    let unshown = lost.len().saturating_sub(SETTINGS_DRAFTS_SHOWN);
    if unshown > 0 {
        m = m.line(format!("and {unshown} more"));
    }
    match unreadable {
        0 => {}
        1 => m = m.line("1 draft could not be read"),
        n => m = m.line(format!("{n} drafts could not be read")),
    }
    if let Some(saved) = saved_in {
        m = m.line(format!(
            "the full text is saved in {}",
            sanitize_for_tty(&saved.display().to_string(), DETAIL_LINE_CAP)
        ));
    }
    m.sentence(
        "the update carried this unsaved text but this version could not put it back; type it \
         again in Settings to keep it",
    )
    .action(Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::Home
            .path()
            .to_string(),
    })
    .hold(Hold::Default)
    .key(KEY_SETTINGS_DRAFTS)
}

/// "1 older line stayed behind in 1 tab" / "12345 older lines stayed behind
/// in 3 tabs".
#[cfg(any(unix, test))]
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

/// A span as the durable record says it: "42 s", "1 min", "3 min 5 s", "2 h", "2 h 10 min".
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

/// When `build` — the one a handoff is about to install.
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

/// The successor, running `build`.
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

/// The switch to a new version begins, on record.
#[cfg(any(unix, test))]
pub(crate) fn switch_started(target: Option<&str>, running: &str) -> Message {
    let title = target.map_or_else(
        || "Reloading aterm in place".to_string(),
        |version| flow_title("Installing", version),
    );
    row(Severity::Info, title)
        .glyph(glyph(NOTED))
        .line(format!("from {}", aterm_v(running)))
        .hold(Hold::LogOnly)
}

/// The cold lane went on without its quit layout.
#[cfg(any(unix, test))]
pub(crate) fn cold_lane_tabs_not_kept(cause: &str) -> Message {
    row(Severity::Warn, "Open tabs not kept for the update")
        .glyph(Severity::Warn.default_glyph())
        .sentence(format!(
            "aterm could not save which document and Settings tabs were open ({cause}), so the \
             new version starts without them; no unsaved work was open"
        ))
        .hold(Hold::LogOnly)
}

/// A busy terminal, in the one spelling every busy stand-down takes.
#[cfg(any(unix, test))]
const BUSY: &str = "the terminal was busy";

/// A stopped switch's reason in a person's words, as `(a substring of the reason, the
/// words)`, keyed on the producers' own literals in `app_update_handoff.rs` and
/// `native_update_auto_intent.rs`: the park gate's waits (a hold-cap stand-down names the
/// last one it waited on, so they come first), the stand-downs and misses of both lanes,
/// and the fork lane's activity refusals.
#[cfg(any(unix, test))]
const STAND_DOWN_CAUSES: &[(&str, &str)] = &[
    (
        "a video recording is running",
        "a video recording was running",
    ),
    (
        "command has exited but its pane is still open",
        "a tab's command had ended with its pane still open",
    ),
    (
        "folder-access warm-up",
        "a folder-access warm-up was still running",
    ),
    (
        "a restore reopened are still being relaunched",
        "the agents a restore reopened were still starting",
    ),
    ("inside the typing gap", "you were typing"),
    (
        "every terminal session closed",
        "every tab closed while the new version started",
    ),
    (
        "session closed during automatic reader park",
        "a tab's command ended as the install began",
    ),
    (
        "handoff policy is being read",
        "the new version was still being checked",
    ),
    (
        "pre-park verification is being read",
        "the new version was still being checked",
    ),
    ("quiet epoch", BUSY),
    ("output is still streaming", BUSY),
    ("output waiting that its reader has not taken", BUSY),
    ("input or PTY output arrived", BUSY),
    ("handoff park deadline", BUSY),
    ("capture exceeded", BUSY),
    ("stayed busy", BUSY),
    ("parked mid-sequence", BUSY),
    ("activity revoked", BUSY),
    ("ended ActivityRevoked", BUSY),
    ("did not park within", BUSY),
    ("proof deadline passed before the grant", BUSY),
    ("within the hold cap", "the terminal never went quiet"),
];

/// What a stopped switch's `why` comes down to for a person: a stand-down's
/// cause ([`STAND_DOWN_CAUSES`]), else a failure the apply lane can name
/// ([`crate::update_apply_trouble::named_clause`]); `None` for anything
/// else, whose account stays in aterm.log.
#[cfg(any(unix, test))]
fn stopped_cause(why: &str) -> Option<&'static str> {
    STAND_DOWN_CAUSES
        .iter()
        .find(|(key, _)| why.contains(key))
        .map(|(_, words)| *words)
        .or_else(|| crate::update_apply_trouble::named_clause(why))
}

/// The switch that did not happen, on record.
#[cfg(any(unix, test))]
pub(crate) fn switch_stopped(
    target: Option<&str>,
    running: &str,
    routine: bool,
    why: &str,
) -> Message {
    let running = target.map_or_else(|| "aterm".to_string(), |_| aterm_v(running));
    let past_cap = past_cap_cause(why);
    let kept = match past_cap.or_else(|| stopped_cause(why)) {
        Some(cause) => format!("{running} kept running: {cause}"),
        None => format!("{running} kept running"),
    };
    let detail = if routine {
        let tail = if past_cap.is_some() {
            "tries again when you close some"
        } else {
            TRIES_AGAIN
        };
        format!("{kept}; it {tail}")
    } else {
        kept
    };
    let title = match (target, routine) {
        (_, true) => waiting_to_install(Some(target)),
        (Some(version), false) => format!("Couldn't install {}", aterm_v(version)),
        (None, false) => "Couldn't reload aterm".to_string(),
    };
    row(
        if routine {
            Severity::Info
        } else {
            Severity::Warn
        },
        title,
    )
    .glyph(glyph(if routine { NOTED } else { NEEDS_YOU }))
    .sentence(detail)
    .hold(Hold::LogOnly)
}

/// The title of a switch the lane will make again by itself.
#[cfg(any(unix, test))]
pub(crate) fn waiting_to_install(switch: Option<Option<&str>>) -> String {
    match switch {
        Some(Some(version)) => format!("Waiting to install {}", aterm_v(version)),
        Some(None) => "Waiting to reload aterm".to_string(),
        None => "Update waiting".to_string(),
    }
}

/// `said`, then — after ` · `.
pub(crate) fn with_named_cause(said: &str, reason: &str) -> String {
    match crate::update_apply_trouble::named_clause(reason) {
        Some(cause) => format!("{said} \u{b7} {cause}"),
        None => said.to_string(),
    }
}

/// What `CaptureRefusal`'s Display puts ahead of a refusal of the open tabs
/// together; the refusal's own words follow it.
#[cfg(any(unix, test))]
const CAPTURE_REFUSED: &str = "the park's capture refused: ";

/// What a capture refusal past a cap no update carries says is past it and
/// where (`restore::RestoreManifest::why_not_whole`), without the refusal's
/// prefix; `None` for any other account.
#[cfg(any(unix, test))]
fn past_cap_cause(why: &str) -> Option<&str> {
    why.contains(crate::restore::UPDATE_CAP_KEY).then(|| {
        why.split_once(CAPTURE_REFUSED)
            .map_or(why, |(_, cause)| cause)
    })
}

/// Whether a capture refusal names one tab (`CaptureRefusal`'s "the park's capture refused
/// session N: …") rather than the open tabs together (their layout, or their records over
/// what one handover carries), which no single tab changing is owed to.
#[cfg(any(unix, test))]
fn capture_refusal_names_a_tab(message: &str) -> bool {
    message.contains("refused session")
}

/// Record of a capture refusal retried when tabs change ([`capture_refusal_names_a_tab`], [`crate::update_apply_trouble::named_clause`]).
#[cfg(any(unix, test))]
pub(crate) fn capture_refused_waiting(message: &str, spacing_left: Duration) -> String {
    let past_cap;
    let said = if let Some(cause) = past_cap_cause(message) {
        past_cap = format!("{cause} \u{b7} tries again when you close some");
        past_cap.as_str()
    } else if capture_refusal_names_a_tab(message) {
        "one tab couldn't move to the new version \u{b7} tries again when that tab changes"
    } else if crate::update_apply_trouble::named_clause(message)
        == Some(crate::update_apply_trouble::TABS_OVER_HANDOVER)
    {
        "too many tabs were open to move to the new version at once \u{b7} tries again when \
         you close some"
    } else {
        "the open tabs couldn't move to the new version \u{b7} tries again when the tabs change"
    };
    if spacing_left <= crate::app_native::CAPTURE_REFUSAL_PROBE {
        return said.to_string();
    }
    match spacing_left.as_secs().div_ceil(60) {
        1 => format!("{said}, in a minute at the soonest"),
        minutes => format!("{said}, in {minutes} minutes at the soonest"),
    }
}

/// An apply-lane outcome the lane answers by itself, on record.
pub(crate) fn outcome(title: &str, detail: &str, severity: Severity) -> Message {
    let msg = row(severity, sanitize_for_tty(title, 80))
        .glyph(severity.default_glyph())
        .sentence(detail)
        .hold(Hold::LogOnly)
        .key(KEY_OUTCOME);
    if severity == Severity::Warn {
        msg.action(software_update())
    } else {
        msg
    }
}

/// The lane stopped and only a press moves the build.
pub(crate) fn needs_install(title: &str, detail: &str, severity: Severity, build: u64) -> Message {
    needs_install_because(title, None, detail, severity, build)
}

/// [`needs_install`] with the failure's short cause painted beside the title where one is known (`Couldn't install aterm v0.92.0 · disk full`).
pub(crate) fn needs_install_because(
    title: &str,
    cause: Option<&str>,
    detail: &str,
    severity: Severity,
    build: u64,
) -> Message {
    if detail.contains(TRIES_AGAIN) {
        return outcome(title, detail, Severity::Info);
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
        msg.glyph(glyph(MOVING_IN))
            .hold(Hold::For(HOLD_STAGED_MANUAL))
    }
}

/// The few words a failure's own message comes down to, where they change what the person does.
pub(crate) fn short_cause(message: &str) -> Option<&'static str> {
    let m = message.to_ascii_lowercase();
    if aterm_update::is_install_location_refusal(&m) {
        Some("folder not writable")
    } else if m.contains("no space left") || m.contains("enospc") || m.contains("disk full") {
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

/// An attempt that failed and that nothing retries, a failure row.
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

/// The automatic lane held by a person's unsaved work.
pub(crate) fn waits_for_you(blocker: &str) -> Message {
    row(
        Severity::Warn,
        sanitize_for_tty(crate::app_update_screen::UPDATE_WAITS_FOR_YOU, 80),
    )
    .glyph(glyph(NEEDS_YOU))
    .sentence(blocker)
    .now(Ask::standing(
        StandingWhy::OnlyYou(Reason::new(
            "the update cannot carry unsaved work, and only you can keep it",
        )),
        Then::step(match blocker {
            crate::App::UNSAVED_SETTINGS_BLOCKS_APPLY => "Save the unsaved text in Settings",
            crate::App::UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY => {
                "Save the work in the open editor and in Settings"
            }
            _ => "Save the work in the open editor",
        }),
    ))
    .key(KEY_WAITS_FOR_YOU)
}

/// An installed Linux executable waits for a start (`aterm_update::INSTALL_AWAITS_START_TITLE`).
pub(crate) fn install_awaits_start(body: &str) -> Message {
    row(
        Severity::Info,
        sanitize_for_tty(aterm_update::INSTALL_AWAITS_START_TITLE, 80),
    )
    .sentence(body)
    .action(software_update())
    .hold(Hold::Default)
    .key(KEY_AWAITS_START)
}

/// A Linux build downloaded and held for a person.
pub(crate) fn linux_held(version: &str) -> Message {
    row(Severity::Info, staged_title(version))
        .glyph(glyph(MOVING_IN))
        .sentence("Run `aterm update apply` to install it; the next aterm you start runs it.")
        .action(software_update())
        .hold(Hold::For(HOLD_STAGED_MANUAL))
        .key(KEY_HELD)
}

/// The check-health warning.
pub(crate) fn health_warning(kind: HealthKind, title: &str, body: &str) -> Message {
    let msg = row(Severity::Warn, sanitize_for_tty(title, 80));
    let msg = match health_since(body) {
        Some(since) => msg.line(format!("since {since}")),
        None => msg,
    };
    msg.sentence(body)
        .action(software_update())
        .hold(Hold::Default)
        .key(kind.key())
}

/// How long a failing kind goes on before it is worth a line of the digest.
pub(crate) const HEALTH_ENTRY_AFTER: Duration =
    aterm_messages::need::Horizon::UPDATES_FAILING.duration();

/// How much of [`HEALTH_ENTRY_AFTER`] is left for a failure whose sentence dates its streak (`body`, [`aterm_update::health_notice_since`]) at `now_unix`.
pub(crate) fn health_entry_delay(body: &str, now_unix: i64) -> Duration {
    let since = aterm_update::health_notice_since(body)
        .and_then(aterm_types::rfc3339::parse_utc)
        .or_else(|| health_excerpt_since(body, now_unix));
    let elapsed = since.map_or(0, |since| now_unix.saturating_sub(since).max(0));
    HEALTH_ENTRY_AFTER.saturating_sub(Duration::from_secs(u64::try_from(elapsed).unwrap_or(0)))
}

/// The start a carried warning's excerpt dates (`since Sep 14`, [`health_since`]'s words)
/// at `now_unix`: the end of that UTC day, in the latest year whose day has begun by now,
/// so the digest's day is never cut short.
fn health_excerpt_since(line: &str, now_unix: i64) -> Option<i64> {
    let (month, day) = line.strip_prefix("since ")?.split_once(' ')?;
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    let day: u32 = day.parse().ok()?;
    let now = aterm_types::rfc3339::format_rfc3339(u64::try_from(now_unix).ok()?);
    let year: i64 = now.get(..4)?.parse().ok()?;
    [year, year - 1].into_iter().find_map(|year| {
        let end =
            aterm_types::rfc3339::parse_utc(&format!("{year:04}-{month:02}-{day:02}T23:59:59Z"))?;
        (end - 86_399 <= now_unix).then_some(end)
    })
}

/// The digest's line for a kind of update failure that has gone on for a day ([`HEALTH_ENTRY_AFTER`]), under the key `update.review.<kind>`, never the record's `update.health.<kind>`.
pub(crate) fn update_health_entry(kind: HealthKind, body: &str) -> Entry {
    let (what, why, then) = match kind {
        HealthKind::Install => (
            "Updates are not installing",
            "a downloaded, verified update hasn\u{2019}t installed here",
            "Open Software Update and read the cause",
        ),
        HealthKind::Download => (
            "Updates are not downloading",
            "updates will not download or verify here, and aterm cannot change why",
            "Open Software Update and read the cause",
        ),
        HealthKind::Check => (
            "Update checks keep failing",
            "the newest release cannot be read or trusted from here",
            "Open Software Update and read the cause",
        ),
        HealthKind::Stalled => (
            "The update check stopped",
            "the update check stopped answering and is not coming back on its own",
            "Open Software Update and check by hand",
        ),
        HealthKind::LockHeld => (
            CHECKER_LOCK_HELD_TITLE,
            "it has held the update lock for a day (aterm.log names its pid when the lock \
             records one); this aterm checks without it",
            "Quit that aterm",
        ),
    };
    let then = match kind {
        HealthKind::LockHeld => Then::step(then),
        _ => Then::press(
            aterm_messages::need::Mover::open_settings(
                crate::native_settings::SettingsRoute::SoftwareUpdate.path(),
            ),
            then,
        ),
    };
    let mut entry = Entry::new(
        Family::Update,
        "",
        kind.slug(),
        what,
        Why::OnlyYou(Reason::new(why)),
        then,
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT);
    if let Some(since) = health_since(body) {
        entry = entry.more(format!("failing since {since}"));
    }
    entry
        .more(sanitize_for_tty(body, 200))
        .log_line(format!("update-health: {} for over a day", kind.slug()))
}

/// The title of the warning the window's watchdog raises when the update check stops answering (`crate::update_checker_watch`).
pub(crate) const CHECKER_STALLED_TITLE: &str = "Update check stopped";

/// The title of the warning the watchdog raises when another aterm holds the machine's update check without making progress ([`checker_lock_held_body`]).
pub(crate) const CHECKER_LOCK_HELD_TITLE: &str = "Another aterm's update check is stuck";

/// Every title the watchdog's warning has worn, today's first: a row an older
/// build carried under the family key is read back to its kind by these
/// ([`HealthKind::of_row`]).
const CHECKER_STALLED_TITLES: [&str; 2] = [CHECKER_STALLED_TITLE, "aterm's update check stopped"];

/// The body of the warning the window's watchdog (`crate::update_checker_watch`) raises under [`CHECKER_STALLED_TITLE`].
pub(crate) fn checker_stalled_body(
    for_secs: u64,
    phase: &str,
    replaced: bool,
    holds_lane: bool,
) -> String {
    let doing = match phase {
        "starting" => "starting",
        "settings" => "reading its settings",
        "bundle-probe" => "reading the installed app",
        "lock-wait" => "waiting for another aterm's check to finish",
        "checking" => "checking for a new version",
        "waiting" => "waiting between checks",
        _ => "running",
    };
    let minutes = for_secs.div_ceil(60).max(1);
    let then = match (replaced, holds_lane) {
        (true, false) => "aterm started a fresh one in its place",
        (true, true) => {
            "aterm started a fresh one, which cannot check until the stuck one finishes"
        }
        (false, _) => "aterm could not start a fresh one",
    };
    format!("the update check stopped answering {minutes} min ago while {doing}; {then}.")
}

/// Body of the warning under [`CHECKER_LOCK_HELD_TITLE`] when `checker.lock` is held past its bound `cycles` times (`aterm_update::checker_watch::CHECKER_UNGATED_AFTER`).
pub(crate) fn checker_lock_held_body(cycles: u64) -> String {
    format!(
        "another aterm has held the update check's lock through the last {cycles} checks \
         without finishing — it may be stopped or hung. This aterm checks for updates \
         without waiting for it."
    )
}

/// The warning healed, on record, once per announced episode: what it had said, so the page reads the recovery against it.
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

/// The OS notification's body for the same warning.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn health_notification_body(
    body: &str,
    remedy: crate::update_apply_trouble::PersonRemedy,
) -> String {
    let todo = remedy.row_tail();
    match health_since(body) {
        Some(since) => {
            format!("Since {since}: {todo}. Details: aterm Settings \u{25b8} Software Update.")
        }
        None => {
            let mut chars = todo.chars();
            let first = chars.next().map(|c| c.to_uppercase().collect::<String>());
            format!(
                "{}{}. Details: aterm Settings \u{25b8} Software Update.",
                first.unwrap_or_default(),
                chars.as_str()
            )
        }
    }
}

/// The OS notification's title for the same warning.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn health_notification_title(title: &str) -> String {
    let mut chars = title.chars();
    let lowered: String = chars
        .next()
        .map(|first| first.to_lowercase().chain(chars).collect())
        .unwrap_or_default();
    format!("aterm \u{00b7} {lowered}")
}

/// The month words of [`health_since`] and its reader, [`health_excerpt_since`].
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// "Sep 14": the day the updater's sentence dates the failure from ([`aterm_update::health_notice_since`]), or `None` without a readable one.
pub(crate) fn health_since(body: &str) -> Option<String> {
    let stamp = aterm_update::health_notice_since(body)?;
    let month: usize = stamp.get(5..7)?.parse().ok()?;
    let day: u32 = stamp.get(8..10)?.parse().ok()?;
    Some(format!("{} {day}", MONTHS.get(month.checked_sub(1)?)?))
}

/// The supersede key of a dev build's standing: the newest standing a process read replaces the last.
pub(crate) const KEY_DEV_BUILD: &str = "update.dev-build";
/// The key of a dev build's shared-writes row ([`dev_build_entry`]).
pub(crate) const KEY_DEV_BUILD_WRITES: &str = "update.dev-build-writes";

/// What a dev build writes of the shared user state only the release writes unattended, in words, with what puts it back.
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

/// What a dev build named `aterm.app` writes of the release's shared setup ([`dev_shared_writes`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DevSharedWrites {
    /// What it writes: `the PATH links, the shell hooks and the agent primer`.
    pub what: &'static str,
    /// What puts it back, run from the release: `` `aterm pkg repair` ``.
    pub repair: &'static str,
}

/// A dev build that writes the release's setup.
pub(crate) fn dev_build_entry(writes: DevSharedWrites) -> Entry {
    let DevSharedWrites { what, repair } = writes;
    Entry::new(
        Family::Update,
        "dev-build",
        "writes",
        "Release setup lost to dev build",
        Why::OnlyYou(Reason::new(format!("this dev build overwrote {what}"))),
        Then::step(format!("run {repair} from the release")),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::CHOICE)
    .more("this dev build's bundle is named aterm.app, which only the release may be")
    .more(
        "rebuild it with `targo --unverified ship dev-app`, which installs it as aterm \
         (dev).app, and remove this copy",
    )
}

/// A successor LaunchServices has no pid for.
#[cfg(target_os = "macos")]
pub(crate) fn launch_unregistered_entry(pid: i32) -> Entry {
    stale_launch_record_entry(
        Why::OnlyYou(Reason::new(STALE_RECORD_REASON)),
        format!("LaunchServices answered the update's launch of pid {pid} with no pid"),
        format!("update: LaunchServices keeps a stale record of the successor (pid {pid})"),
    )
}

/// The same entry from this process's own measure (`App::note_own_launch_record`).
#[cfg(target_os = "macos")]
pub(crate) fn own_launch_record_stale_entry(pid: u32) -> Entry {
    stale_launch_record_entry(
        Why::OnlyYou(Reason::new(STALE_RECORD_REASON)),
        format!("LaunchServices lists aterm with no pid, and none of its entries is pid {pid}"),
        format!("launch record: LaunchServices keeps a stale record of this process (pid {pid})"),
    )
}

/// The same entry from a renewal's successor (`pid`, launched by pid `from`) that measured its own record stale too.
#[cfg(target_os = "macos")]
pub(crate) fn own_launch_record_renewed_stale_entry(pid: u32, from: u32) -> Entry {
    stale_launch_record_entry(
        Why::Spent {
            tried: Reason::new("reloaded itself in place to renew it"),
            reason: Reason::new(STALE_RECORD_REASON),
        },
        format!(
            "a fresh launch (pid {pid}) from pid {from}: LaunchServices still lists aterm with \
             no pid"
        ),
        format!(
            "launch record: LaunchServices keeps a stale record of this process (pid {pid}), a \
             fresh launch from pid {from}"
        ),
    )
}

/// The same entry once every renewal attempt (`attempts`) this process made ended without a successor taking over.
#[cfg(target_os = "macos")]
pub(crate) fn own_launch_record_renewal_spent_entry(pid: u32, attempts: u8) -> Entry {
    stale_launch_record_entry(
        Why::Spent {
            tried: Reason::new(format!("tried {attempts} times to reload itself in place")),
            reason: Reason::new(STALE_RECORD_REASON),
        },
        format!("LaunchServices lists aterm with no pid, and none of its entries is pid {pid}"),
        format!(
            "launch record: LaunchServices keeps a stale record of this process (pid {pid}); \
             {attempts} renewal attempts did not finish"
        ),
    )
}

/// Why the person, and not aterm, ends a stale record: once aterm's own renewal has not
/// run or not worked, only their fresh open gives macOS a new record.
#[cfg(target_os = "macos")]
const STALE_RECORD_REASON: &str =
    "only quitting aterm and opening it again gives macOS a new record of it";

#[cfg(target_os = "macos")]
fn stale_launch_record_entry(why: Why, evidence: String, log_line: String) -> Entry {
    Entry::new(
        Family::Update,
        "launch-registration",
        "stale",
        "macOS lost track of updated aterm",
        why,
        Then::step("quit aterm with \u{2318}Q, then open it again"),
    )
    .severity(Severity::Warn)
    .resurface(Resurface::WAIT)
    .more(evidence)
    .more(
        "until then System Settings\u{2019} Quit & Reopen can miss it and its menu-bar \
         item may not appear",
    )
    .log_line(log_line)
}

/// A dev build's standing against the channel.
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
            "rebuild it with `targo --unverified ship dev-app`, or use the release \
             (tools/install.sh puts it in Applications)",
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

    /// D5: a calm Info row on glass, by what was folded (windows merged into tabs, tabs
    /// moved, both), singular and plural, and nothing for a layout that folded nothing.
    #[test]
    fn a_folded_desk_is_said_as_a_calm_record() {
        use crate::restore::LayoutFold;
        assert!(layout_folded(&LayoutFold::default()).is_none());
        assert!(
            layout_folded(&LayoutFold {
                front: vec![7],
                ..LayoutFold::default()
            })
            .is_none(),
            "a front list alone moved nothing"
        );
        let said = |windows, tabs| {
            let m = layout_folded(&LayoutFold {
                windows,
                tabs,
                front: vec![0],
            })
            .expect("something folded");
            assert_eq!(m.severity, Severity::Info, "a record, never a warning");
            assert_eq!(m.glyph, glyph(NOTED));
            assert_eq!(m.hold, Hold::Default, "on glass");
            assert_eq!(m.key.as_deref(), Some(KEY_LAYOUT_FOLDED));
            (m.title.clone(), m.detail.clone())
        };
        let (windows, detail) = said(6, 0);
        assert!(
            windows.contains('6') && detail.len() == 1,
            "{windows} {detail:?}"
        );
        let (tabs, detail) = said(0, 44);
        assert!(
            tabs.contains("44") && detail.len() == 1,
            "{tabs} {detail:?}"
        );
        assert_ne!(said(1, 0).0, said(6, 0).0.replace('6', "1"), "singular");
        let (_, detail) = said(2, 4);
        assert_eq!(
            detail.len(),
            3,
            "both folds, each with its limit: {detail:?}"
        );
        assert!(detail[1].contains('4'), "{detail:?}");
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

    /// Behind the channel is an Info record in the dev channel's words with what to do
    /// behind it; at or newer than the newest release says nothing.
    #[test]
    fn a_dev_builds_standing_is_a_record() {
        use crate::message_reporters::{Attention, attention};
        use aterm_update::dev_channel::DevLag;
        let behind = dev_build(&dev_behind()).expect("behind is recorded");
        assert_eq!(behind.severity, Severity::Info);
        assert_eq!(behind.hold, Hold::LogOnly);
        assert_eq!(behind.key.as_deref(), Some(KEY_DEV_BUILD));
        assert_eq!(attention(&behind), Ok(Attention::Record));
        assert_eq!(
            aterm_messages::text::glass_title_fault(&behind.title),
            None,
            "the title keeps the glass form, should the owner rule it onto the glass"
        );
        let one = DevLag::Behind {
            latest: "v0.93.0".into(),
            releases: Some(1),
        };
        let one = dev_build(&one).expect("behind is recorded").title;
        assert!(one.contains("v0.93.0"), "{one}");
        assert_ne!(one, behind.title, "one release is not two");
        let major = DevLag::Behind {
            latest: "v1.0.0".into(),
            releases: None,
        };
        assert!(
            dev_build(&major).is_some_and(|m| m.title.contains("v1.0.0")),
            "an unknown count names the release"
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

    /// A dev build named aterm.app is a digest entry of its own, keyed apart from the
    /// standing, saying what it writes and the release's verb that puts each thing back:
    /// `aterm pkg repair` never touches the agent primer, so the primer names
    /// `aterm agents install`.
    #[test]
    fn a_dev_build_that_writes_the_release_setup_says_what_and_what_puts_it_back() {
        let entry = dev_build_entry(ALL_WRITES);
        let msg = entry.message().expect("the words pass the rule");
        assert_eq!(msg.severity, Severity::Warn);
        assert_eq!(msg.hold, Hold::LogOnly, "an entry is never a band row");
        assert_eq!(entry.key(), "update.review.dev-build.writes");
        assert_ne!(
            msg.key.as_deref(),
            Some(KEY_DEV_BUILD),
            "the channel never supersedes it"
        );
        assert_ne!(
            msg.key.as_deref(),
            Some(KEY_DEV_BUILD_WRITES),
            "an exception never shares a key with a row or a record"
        );
        assert!(
            msg.title.contains("lost"),
            "the loss is named: {}",
            msg.title
        );
        assert_eq!(aterm_messages::text::glass_title_fault(&msg.title), None);
        assert!(
            msg.detail[0].contains(ALL_WRITES.what),
            "it says what it writes: {:?}",
            msg.detail
        );
        let said = msg.detail.join(" ");
        assert!(said.contains("`aterm pkg repair`"), "{said}");
        assert!(said.contains("`aterm agents install`"), "{said}");
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
        let only_primer = dev_build_entry(primer).message().expect("the words pass");
        assert!(
            only_primer.detail[0].contains(primer.what),
            "{:?}",
            only_primer.detail
        );
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
            P::WaitsForTerminal,
            P::ManualByConfig,
            P::VetoedByEnv {
                var: "ATERM_DEBUG_RELAUNCH_NUDGE",
            },
            P::ManualOnlyLatched { lapses: true },
            P::ManualOnlyLatched { lapses: false },
            P::TrialSpent,
        ];
        for image_kept in [false, true] {
            for why in HandoffUnavailable::ALL {
                for veto in [
                    None,
                    Some(AutoApplyVeto::Config),
                    Some(AutoApplyVeto::Env {
                        var: "ATERM_DEBUG_RELAUNCH_NUDGE",
                    }),
                ] {
                    postures.push(P::HandoffDisabled {
                        why,
                        veto,
                        image_kept,
                    });
                }
            }
        }
        postures
    }

    /// The install switch has one name on the staged line too: a build that
    /// `[update] auto_apply` holds for a press says the switch's name as
    /// Settings paints it right under that line on Software Update ("Install
    /// updates automatically"), alone or after a handoff cause, as the Linux
    /// held download does.
    #[test]
    fn a_build_held_by_the_install_switch_names_the_switch() {
        use ApplyPosture as P;
        let named = format!(
            "\u{201c}{}\u{201d} is off",
            crate::prefs::UPDATE_INSTALL_LABEL
        );
        for posture in every_posture() {
            let held = matches!(
                posture,
                P::ManualByConfig
                    | P::HandoffDisabled {
                        veto: Some(AutoApplyVeto::Config),
                        ..
                    }
            );
            let sentence = staged_detail(posture);
            assert_eq!(sentence.contains(&named), held, "{posture:?}: {sentence}");
            assert!(
                !sentence.contains("automatic install is off"),
                "{posture:?}: {sentence}"
            );
        }
        assert_eq!(staged_detail(P::ManualByConfig), named);
    }

    /// A lane that tries again by itself asks nothing:
    /// its `Couldn't install` is a record — FYI goes to the log, never the
    /// glass — while a stopped lane keeps its
    /// row and its `Install now`, with the failure's few words beside the
    /// title where they change what the person does (`Couldn't install aterm v0.92.0 · disk full`).
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

    /// Every flow row completes in its finished form: `Downloaded`,
    /// `Verified`, `Installed`, never a participle beside a ✓, with nothing else on the
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
            download_withheld("0.48.0", "held: build 7 is quarantined"),
            download_failed("0.48.0", "zip sha256 mismatch"),
            installing("0.48.0"),
            finishing("0.48.0"),
            landed("0.48.0", 7, 0, None),
            landed("0.48.0", 7, 2, Some(Duration::from_secs(42))),
            switch_started(Some("0.48.0"), "0.47.0"),
            switch_stopped(Some("0.48.0"), "0.47.0", false, "child died"),
            outcome("Update installed", "x", Severity::Info),
            needs_install("Update installed", "x", Severity::Info, 7),
            needs_install("Couldn't install aterm v0.48.0", "x", Severity::Warn, 7),
            failed("Couldn't finish the update", "", false),
            health_warning(
                HealthKind::Install,
                aterm_update::health_failing_title("apply"),
                "3 checks",
            ),
            health_recovered("Couldn't install updates", "since Sep 14"),
            scrollback_lost(12_345, 3),
            scrollback_lost(1, 1),
            scrollback_withheld(12_345, 3),
            scrollback_left_behind(12_345, 3, 10_000, 0, 0),
            scrollback_left_behind(12_345, 3, 0, 345, 0),
            scrollback_left_behind(12_345, 3, 0, 0, 345),
            settings_drafts_not_reopened(&[draft("font_family", "Menlo")], 0, None),
            settings_drafts_not_reopened(&[], 2, None),
            dev_build(&dev_behind()).expect("a record"),
            dev_build_entry(ALL_WRITES)
                .message()
                .expect("the words pass"),
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
        for ch in [FLOW_GLYPH, NEEDS_YOU, MOVING_IN, NOTED, READY] {
            assert!(Glyph::new(ch).is_some(), "{ch:?} is in the closed set");
        }
    }

    /// A build whose next start its boot trial rolls back is not ready: an Info record
    /// with `ℹ`, `aterm vX is on hold`, its sentence naming the newer release.
    #[test]
    fn a_spent_trial_is_recorded_on_hold_not_ready() {
        let m = staged("0.95.0", 11, Some(ApplyPosture::TrialSpent));
        assert_eq!(m.severity, Severity::Info);
        assert_eq!(m.glyph.ch(), NOTED);
        assert_eq!(m.hold, Hold::LogOnly);
        assert!(m.actions.is_empty(), "no press");
        assert!(
            m.detail
                .iter()
                .any(|line| line.contains(NEWER_RELEASE_REPLACES)),
            "{:?}",
            m.detail
        );
        let ready = staged("0.95.0", 11, Some(ApplyPosture::Automatic));
        assert_eq!(ready.title, staged_title("0.95.0"));
        assert_eq!(ready.glyph.ch(), READY);
    }

    /// Every phase from the first byte to the switch is the flow row (Info, keyed
    /// `update.progress`, verb first), its phase word behind `Details ›`, a
    /// fill with its amount where the total is known and busy where it is not, both
    /// waiting out the progress grace.
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
        assert_eq!(m.glyph.ch(), MOVING_IN);
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
        assert_eq!(m.glyph.ch(), FLOW_GLYPH, "checking is the working mark");
        assert_eq!(m.detail, vec![CHECKING]);
        assert!(busy(&m), "checking has no fraction: the row moves");
        assert!(!m.excerpt);
        let cause = "zip sha256 mismatch: expected ab12, got cd34 — see                      https://github.com/alabsystems/aterm/releases/tag/v0.48.0 for the assets";
        let m = progress(
            &P::Failed {
                detail: cause.into(),
            },
            None,
            "0.48.0",
        );
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(m.detail[0], DOWNLOAD_FAILED);
        assert_eq!(m.detail[1..].join(" "), cause, "the whole sentence, kept");
        assert_eq!(m.hold, Hold::LogOnly, "a self-retry is a record");
        assert_eq!(m.key, None, "a record never supersedes the live flow row");
        assert!(
            m.actions.is_empty(),
            "a retried download asks nothing of anyone"
        );
        let m = progress(
            &P::Deferred {
                detail: "the network is metered; checking again in 10 min".into(),
            },
            None,
            "0.48.0",
        );
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key, None);
        assert_eq!(
            m.detail,
            vec!["the network is metered; checking again in 10 min"]
        );
        let m = progress(
            &P::Withheld {
                detail: "held: build 7 was signed by machine m11, which the machine roster \
                         has revoked"
                    .into(),
            },
            None,
            "0.48.0",
        );
        assert_eq!(m.hold, Hold::LogOnly);
        assert_eq!(m.key, None);
        assert!(
            !m.detail.iter().any(|d| d.contains("again")),
            "{:?}",
            m.detail
        );
        assert_eq!(flow_title("Installing", ""), "Installing update");
        assert_eq!(Severity::Warn.default_hold(), HOLD_WARN);
    }

    /// Every row the lane composes is progress with its indicator, a decision, a failure
    /// the person acts on, or a record, never a confirmation or a still Info row on the
    /// glass, and every glass title is terse.
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
            progress(&P::Withheld { detail: "x".into() }, None, "0.91.0"),
            installing("0.91.0"),
            installing(""),
            finishing("0.91.0"),
            landed("0.91.0", 7, 0, Some(Duration::from_secs(42))),
            switch_started(Some("0.91.0"), "0.90.0"),
            switch_stopped(Some("0.91.0"), "0.90.0", true, "x"),
            switch_stopped(Some("0.91.0"), "0.90.0", false, "x"),
            outcome("Couldn't install aterm v0.91.0", "x", Severity::Info),
            outcome("Update waits", "x", Severity::Warn),
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
            failed("Couldn't finish the update", "", false),
            failed(
                crate::app_update_screen::UPDATE_WAITS_FOR_YOU,
                crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
                true,
            ),
            waits_for_you(crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY),
            waits_for_you(crate::App::UNSAVED_SETTINGS_BLOCKS_APPLY),
            waits_for_you(crate::App::UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY),
            health_recovered("Couldn't install updates", "since Sep 14"),
            staged("0.91.0", 7, None),
            scrollback_lost(12_345, 3),
            scrollback_withheld(12_345, 3),
            scrollback_left_behind(12_345, 3, 10_000, 0, 0),
            scrollback_left_behind(12_345, 3, 0, 345, 0),
            scrollback_left_behind(12_345, 3, 0, 0, 345),
            settings_drafts_not_reopened(&[draft("font_family", "Menlo")], 1, None),
        ];
        for lag in every_dev_standing() {
            all.extend(dev_build(&lag));
        }
        for packages in [false, true] {
            for prime in [false, true] {
                all.extend(
                    dev_shared_writes(false, packages, prime)
                        .map(|w| dev_build_entry(w).message().expect("the words pass")),
                );
            }
        }
        for posture in every_posture() {
            all.push(staged("0.91.0", 7, Some(posture)));
        }
        for class in ["apply", "pipeline", "stage", "manifest"] {
            all.push(health_warning(
                HealthKind::of_class(class),
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

    /// A failed update keeps its whole cause: a 600-char sentence with a
    /// URL is every word behind the flow row's excerpt, never pre-cut.
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

    /// The staged row says how the update installs and never asks for a restart:
    /// where a press installs it, the ready row with the `Install now` capsule; every
    /// other posture, the automatic lane included, is a record.
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
                assert_eq!(m.glyph.ch(), MOVING_IN, "a build to install (ruling 303)");
                assert_eq!(m.severity, Severity::Info, "a decision, not a confirmation");
                assert_eq!(m.hold, Hold::For(HOLD_STAGED_MANUAL));
                assert!(!m.excerpt, "the capsule is the press");
                assert!(!busy(&m), "a ready row waits; it does not move");
            } else {
                let title = if posture == P::TrialSpent {
                    "aterm v0.67.0 is on hold"
                } else {
                    "aterm v0.67.0 is ready"
                };
                assert_eq!(m.title, title, "{posture:?}");
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
            if let P::HandoffDisabled {
                why,
                veto,
                image_kept,
            } = posture
            {
                assert!(
                    sentence.starts_with(why.cause()),
                    "{posture:?}: the cause leads: {sentence}"
                );
                assert_eq!(
                    sentence.contains("once every terminal is closed"),
                    !image_kept,
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
        for posture in every_posture() {
            assert_eq!(staged_detail_lines(Some(posture)).len(), 1, "{posture:?}");
        }
        let m = staged("0.67.0", 7, None);
        assert_eq!(m.detail, vec![staged_detail_unknown()]);
        assert_eq!(staged_detail_unknown(), INSTALL_FROM_MENU);
        assert_eq!(m.actions, vec![Intent::ApplyUpdate { build: 7 }]);
        assert_eq!(m.hold, Hold::For(HOLD_STAGED_MANUAL));
        assert!(crate::native_update_auto_intent::LANDS_WITHIN < Duration::from_secs(60));
        assert!(!INSTALLS_BY_ITSELF.contains("0 min"));
    }

    /// The App refines the staged row with the full words once the lane has armed or stood
    /// down; a lane that installs by itself (armed, or retrying later) is a record, which
    /// the host folds the ready row into rather than restating it.
    #[test]
    fn a_posture_restatement_carries_the_full_words() {
        use ApplyPosture as P;
        let r = restate_apply_posture("0.67.0", 7, P::Automatic);
        assert_eq!(r.hold, Some(Hold::LogOnly));
        assert_eq!(r.detail, Some(vec![INSTALLS_BY_ITSELF.to_string()]));
        assert_eq!(r.actions, Some(Vec::new()));
        let r = restate_apply_posture("0.67.0", 7, P::ManualOnlyLatched { lapses: false });
        assert_eq!(r.actions, Some(vec![Intent::ApplyUpdate { build: 7 }]));
        assert_eq!(r.hold, Some(Hold::For(HOLD_STAGED_MANUAL)));
        assert_eq!(r.severity, Some(Severity::Info));
        assert_eq!(r.meter, Some(None));
        let r = restate_apply_posture("0.67.0", 7, P::ManualOnlyLatched { lapses: true });
        assert_eq!(r.hold, Some(Hold::LogOnly));
        assert_eq!(r.meter, Some(None));
        let r = restate_apply_posture(
            "0.67.0",
            7,
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None,
                image_kept: false,
            },
        );
        assert_eq!(r.actions, Some(Vec::new()), "the handoff off strips it");
    }

    /// `Install now` rides the staged row only where a press is how the build installs
    /// (design §7.3).
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
                apply_capsule_for(
                    Some(P::HandoffDisabled {
                        why,
                        veto: None,
                        image_kept: false
                    }),
                    3
                ),
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
    }

    /// The scrollback row says how much scrollback stayed behind, in how many tabs, and where the per-tab
    /// count is: a failure on glass, never a record only.
    #[test]
    fn the_scrollback_row_counts_the_lines_and_the_tabs() {
        let m = scrollback_lost(12_345, 3);
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

    /// The scrollback row says why: a carry the successor's policy withheld is the new version's request,
    /// with no claim that the screen or its newest lines crossed; a failure keeps the
    /// failure row word for word; a mix claims nothing about what crossed.
    #[test]
    fn the_scrollback_row_says_a_withheld_carry_as_the_policys_request() {
        let text = |m: &Message| format!("{} | {}", m.title, m.detail.join(" | "));
        let comfort = "the newest lines came across";
        let asked = "the new version asked this update to carry no scrollback";

        let failed = scrollback_left_behind(12_345, 3, 0, 0, 0);
        assert_eq!(failed, scrollback_lost(12_345, 3), "a failure is unchanged");
        assert!(text(&failed).contains(comfort), "{}", text(&failed));
        assert!(!text(&failed).contains(asked), "{}", text(&failed));

        let withheld = scrollback_left_behind(12_345, 3, 12_345, 0, 0);
        assert_eq!(withheld, scrollback_withheld(12_345, 3));
        assert_eq!(
            withheld.detail[0],
            "12345 older lines stayed behind in 3 tabs"
        );
        assert!(text(&withheld).contains(asked), "{}", text(&withheld));
        assert!(
            !text(&withheld).contains("came across") && !text(&withheld).contains("on screen"),
            "no claim about what crossed: {}",
            text(&withheld)
        );

        let mixed = scrollback_left_behind(12_345, 3, 10_000, 0, 0);
        assert_eq!(mixed.detail[0], "12345 older lines stayed behind in 3 tabs");
        assert!(
            mixed.detail[1].starts_with("10000 of them because the new version asked"),
            "{:?}",
            mixed.detail
        );
        assert!(!text(&mixed).contains("came across"), "{}", text(&mixed));

        let held = scrollback_left_behind(500, 1, 0, 500, 0);
        assert_eq!(held.detail[0], "500 older lines stayed behind in 1 tab");
        assert_eq!(
            held.detail[1],
            "500 of them in panes kept open after their command ended, whose screen alone \
             crossed",
        );
        assert!(
            !text(&held).contains("came across") && !text(&held).contains(asked),
            "{}",
            text(&held)
        );
        let held_and_failed = scrollback_left_behind(12_345, 3, 0, 500, 0);
        assert!(
            !text(&held_and_failed).contains("came across"),
            "{}",
            text(&held_and_failed)
        );

        let held_carry_failed = scrollback_left_behind(500, 1, 0, 0, 500);
        assert_eq!(
            held_carry_failed.detail[0],
            "500 older lines stayed behind in 1 tab"
        );
        assert!(
            held_carry_failed.detail[1].starts_with(
                "500 of them in panes kept open after their command ended, which this update \
                 failed to carry"
            ),
            "{:?}",
            held_carry_failed.detail
        );
        assert!(
            !text(&held_carry_failed).contains("came across")
                && !text(&held_carry_failed).contains("on screen")
                && !text(&held_carry_failed).contains(asked),
            "{}",
            text(&held_carry_failed)
        );
        let held_failed_beside_a_tab = scrollback_left_behind(12_345, 3, 0, 0, 500);
        assert!(
            !text(&held_failed_beside_a_tab).contains("came across"),
            "{}",
            text(&held_failed_beside_a_tab)
        );
        let by_design_and_failed = scrollback_left_behind(800, 2, 0, 300, 500);
        assert!(
            by_design_and_failed.detail[1].starts_with("300 of them in panes kept open")
                && by_design_and_failed.detail[2].starts_with("500 of them in panes kept open")
                && by_design_and_failed.detail[2].ends_with("which this update failed to carry"),
            "one line per cause: {:?}",
            by_design_and_failed.detail
        );
        assert_ne!(failed.title, withheld.title);
        assert_eq!(held.title, withheld.title, "by design is no failure");
        for failure in [
            &mixed,
            &held_and_failed,
            &held_carry_failed,
            &held_failed_beside_a_tab,
            &by_design_and_failed,
        ] {
            assert_eq!(failure.title, failed.title, "{:?}", failure.detail);
        }

        for m in [
            &failed,
            &withheld,
            &mixed,
            &held,
            &held_and_failed,
            &held_carry_failed,
            &held_failed_beside_a_tab,
            &by_design_and_failed,
        ] {
            assert_eq!(m.severity, Severity::Warn, "{}", m.title);
            assert_eq!(m.hold, Hold::Default, "a loss is on glass: {}", m.title);
            assert_eq!(m.key.as_deref(), Some(KEY_SCROLLBACK), "{}", m.title);
            assert!(text(m).contains("history_lost="), "{}", text(m));
        }
    }

    fn draft(key: &str, text: &str) -> crate::restore::SettingsDraftRestore {
        crate::restore::SettingsDraftRestore {
            key: key.to_string(),
            text: text.to_string(),
        }
    }

    /// A carried Settings draft the new build could not reopen is a failure on glass naming
    /// each field with the text typed (it exists nowhere else now), counting the unreadable
    /// ones; it opens Settings and never asks for a restart.
    #[test]
    fn a_settings_draft_the_update_could_not_reopen_is_said_with_its_text() {
        let m = settings_drafts_not_reopened(&[draft("font_family", "Menlo Nerd")], 0, None);
        assert_eq!(m.severity, Severity::Warn);
        assert_eq!(
            m.hold,
            Hold::Default,
            "a loss is on glass, not only on record"
        );
        assert_eq!(m.key.as_deref(), Some(KEY_SETTINGS_DRAFTS));
        assert_eq!(m.detail[0], "font_family: Menlo Nerd");
        assert!(
            m.actions.iter().any(|intent| matches!(
                intent,
                Intent::OpenSettings { route }
                    if route == crate::native_settings::SettingsRoute::Home.path()
            )),
            "{:?}",
            m.actions
        );
        let words = m.detail.join(" ").to_lowercase();
        assert!(
            !words.contains("restart") && !words.contains("relaunch"),
            "{words}"
        );

        let unread = settings_drafts_not_reopened(&[draft("a", "1")], 2, None);
        assert!(
            unread
                .detail
                .iter()
                .any(|line| line == "2 drafts could not be read"),
            "{:?}",
            unread.detail
        );
        let many = (0..20)
            .map(|index| draft(&format!("k{index}"), "v"))
            .collect::<Vec<_>>();
        let capped = settings_drafts_not_reopened(&many, 0, None);
        assert!(
            capped.detail.iter().any(|line| line == "and 4 more"),
            "{:?}",
            capped.detail
        );
    }

    /// Installing and finishing are the flow row (busy, under the handoff's cap, a title per
    /// phase, the phase word behind `Details ›`), sharing the progress key so each replaces
    /// the last.
    #[test]
    fn the_switch_and_the_landing_share_the_flow_row() {
        let i = installing("9.9.9");
        assert_eq!(i.detail, vec![INSTALLING]);
        assert!(busy(&i));
        assert!(!i.excerpt);
        assert_eq!(
            i.hold,
            Hold::Live {
                stale_after: STALE_HANDOFF
            }
        );
        let f = finishing("9.9.9");
        assert_eq!(f.detail, vec![FINISHING]);
        assert!(!f.excerpt, "a reassurance changes nothing the person does");
        assert!(busy(&f));
        assert_eq!(f.key, i.key);
        let l = landed("9.9.9", 7, 0, None);
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
    }

    /// A successor that adopted sessions onto a blank screen says in the landing record how
    /// many tabs it repainted and why; a desk that crossed exactly keeps the bare record.
    #[test]
    fn the_landing_names_the_tabs_it_repainted() {
        let one = landed("0.92.0", 7, 1, None);
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

    /// The update's records (the switch began, stopped, how long it took) are keyless
    /// `LogOnly` records, every version spelled `aterm vX`, the duration a line of the
    /// landing, never a second entry.
    #[test]
    fn the_update_records_say_what_happened_when() {
        let s = switch_started(Some("0.79.0"), "0.78.0");
        assert!(s.detail[0].contains("aterm v0.78.0"), "{:?}", s.detail);
        assert_eq!(s.hold, Hold::LogOnly);
        let busy = "the park missed its budget on every rung (a PTY reader missed the 20 ms \
                    handoff park deadline); the machine is busy, the candidate is not in \
                    question, and the ladder retries";
        let said = |retries, why| {
            switch_stopped(Some("0.79.0"), "0.78.0", retries, why)
                .detail
                .join(" ")
        };
        let waiting = switch_stopped(Some("0.79.0"), "0.78.0", true, busy);
        assert!(
            waiting.detail.join(" ").contains("busy"),
            "{:?}",
            waiting.detail
        );
        assert_eq!(waiting.severity, Severity::Info);
        let unnamed = said(true, "x");
        assert!(!unnamed.contains(':'), "{unnamed}");
        let storage = "the park missed its budget on every rung (visible checkpoint set could \
                       not reserve bounded storage; update handoff stayed in place); the \
                       machine is busy, the candidate is not in question, and the ladder \
                       retries";
        for why in [
            storage,
            "visible checkpoint set could not reserve bounded storage; update handoff stayed \
             in place",
        ] {
            assert_eq!(said(true, why), unnamed, "{why}");
        }
        let not = switch_stopped(
            Some("0.79.0"),
            "0.78.0",
            false,
            "handoff proof ended TimedOut",
        );
        assert_ne!(not.detail.join(" "), said(false, "x"), "a timeout is named");
        assert_eq!(not.severity, Severity::Warn);
        let cap = "overlap handoff failed safely: the park's capture refused: tab 1 of window 1 \
                   has 300 panes, more than the 256 in one tab an update can carry";
        assert!(
            said(false, cap).contains("300 panes"),
            "{}",
            said(false, cap)
        );
        assert_ne!(said(true, cap), said(false, cap));
        for m in [s, waiting, not] {
            let words = std::iter::once(m.title.clone())
                .chain(m.detail.clone())
                .collect::<Vec<_>>()
                .join(" ");
            for jargon in [
                "switch",
                "carried",
                "on its own",
                "build ",
                "park",
                "rung",
                "ladder",
                "candidate",
            ] {
                assert!(!words.contains(jargon), "{jargon:?} in {words:?}");
            }
        }
        let took = landed("0.79.0", 7, 0, Some(Duration::from_secs(42)));
        assert!(took.detail[0].contains(&span_words(Duration::from_secs(42))));
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

    /// Every stand-down reason the lanes write, copied from its producer, reads as what it
    /// means: never "park", "rung", "PTY", "successor" or a second promise to retry.
    #[test]
    fn every_stand_down_reason_is_said_in_a_persons_words() {
        for why in [
            "the terminal never offered a moment to pause in within the hold cap (120 s \
                 after the successor dialled; the park was last held back because a video \
                 recording is running)",
            "the terminal never offered a moment to pause in within the hold cap (120 s \
                 after the successor dialled; the park was last held back because a session's \
                 command has exited but its pane is still open)",
            "the terminal never offered a moment to pause in within the hold cap (120 s \
                 after the successor dialled; the park was last held back because a keystroke \
                 landed in an aterm window inside the typing gap)",
            "the terminal never offered a moment to pause in within the hold cap (120 s \
                 after the successor dialled; the park was last held back because a session \
                 has output waiting that its reader has not taken)",
            "the terminal never offered a moment to pause in within the hold cap (120 s \
                 after the successor dialled)",
            "every terminal session closed while the successor booted",
            "build 9's handoff policy is being read before the fork lane parks; the \
                 attempt after it ends parks under it",
            "build 9's pre-park verification is being read before the cold lane \
                 relaunches; the attempt after it ends relaunches under it",
            "input or PTY output arrived before automatic reader park",
            "terminal activity has not reached the automatic-update quiet epoch",
            "a PTY reader missed the 20 ms handoff park deadline",
            "handoff proof capture exceeded the 20 ms deadline",
            "a PTY session closed during automatic reader park",
            "structural activity revoked the handoff while the successor held the claim",
            "a video recording is running",
        ] {
            let words = switch_stopped(Some("0.79.0"), "0.78.0", true, why)
                .detail
                .join(" ");
            for jargon in ["park", "rung", "PTY", "successor", "hold cap", "epoch"] {
                assert!(!words.contains(jargon), "{why}: {words}");
            }
        }
    }

    /// A failed install's record says the ledger's reason in a person's words, or nothing
    /// past its own words; the ledger's text is never spliced in.
    #[test]
    fn a_failed_install_record_never_splices_the_ledgers_text() {
        let named = with_named_cause(
            "another download replaced it",
            "overlap handoff failed safely: handoff proof ended ChildDied",
        );
        assert!(named.starts_with("another download replaced it"), "{named}");
        assert!(
            !named.contains("ChildDied") && !named.contains("handoff"),
            "{named}"
        );
        assert_eq!(
            with_named_cause(
                "the next check downloads it again",
                "updater apply preflight became stale"
            ),
            "the next check downloads it again"
        );
    }

    /// The cold lane's lost tabs are a record with no Software Update capsule,
    /// which could not bring them back, and no apply outcome's key.
    #[test]
    fn tabs_not_kept_offers_no_action_that_cannot_help() {
        let msg = cold_lane_tabs_not_kept("open restore lock: Permission denied (os error 13)");
        assert_eq!(msg.severity, Severity::Warn);
        assert!(msg.actions.is_empty(), "{:?}", msg.actions);
        assert_ne!(msg.key.as_deref(), Some(KEY_OUTCOME));
    }

    /// A capture refusal names one tab only when its message names one; the
    /// layout and the records over one handover's limit are the open tabs'.
    #[test]
    fn a_capture_refusal_record_names_the_tabs_it_is_about() {
        assert!(
            capture_refused_waiting(
                "overlap handoff failed safely: the park's capture refused session 3: visible \
                 checkpoint set could not be committed canonically: too many sessions",
                Duration::ZERO,
            )
            .starts_with("one tab couldn't move")
        );
        let layout = capture_refused_waiting(
            "overlap handoff failed safely: the park's capture refused: handoff layout could not \
             be committed canonically",
            Duration::ZERO,
        );
        assert!(
            layout.starts_with("the open tabs couldn't move"),
            "{layout}"
        );
        let over_cap = capture_refused_waiting(OVER_CAP_MESSAGE, Duration::ZERO);
        assert_eq!(
            over_cap.split(" \u{b7} ").next(),
            crate::update_apply_trouble::named_clause(OVER_CAP_MESSAGE),
            "the cause in the Version menu's words: {over_cap}"
        );
        assert!(
            over_cap.ends_with("tries again when you close some"),
            "{over_cap}"
        );
        assert!(
            layout.ends_with("tries again when the tabs change"),
            "{layout}"
        );
        for said in [layout, over_cap] {
            assert!(!said.contains("that tab"), "{said}");
        }
        for message in [
            "overlap handoff failed safely: the park's capture refused: 16500 tabs are open, more \
             than the 16384 (64 windows of 256) an update can carry",
            "the park's capture refused: 16500 tabs are open, more than the 16384 (64 windows of \
             256) an update can carry",
        ] {
            let said = capture_refused_waiting(message, Duration::ZERO);
            assert!(
                said.starts_with("16500 tabs are open") && said.ends_with("when you close some"),
                "{said}"
            );
        }
    }

    const OVER_CAP_MESSAGE: &str = "overlap handoff failed safely: could not write the \
                                    authenticated handoff manifest (the open tabs' records are \
                                    over the 4194304-byte limit one handover carries)";

    /// The second refusal holds the next try 10 minutes and the third 30, however the
    /// tabs change (`app_native::capture_refusal_spacing`), so those records say when the
    /// next try is; the first, released at the next probe, does not.
    #[test]
    fn a_refusal_that_came_back_says_when_the_next_try_is() {
        use crate::app_native::{CAPTURE_REFUSAL_PROBE, capture_refusal_spacing};
        let left = |refusals| capture_refusal_spacing(refusals) - Duration::from_millis(3);
        let first = capture_refused_waiting(OVER_CAP_MESSAGE, left(1));
        assert!(
            first.ends_with("tries again when you close some"),
            "{first}"
        );
        assert!(!first.contains("soonest"), "{first}");
        let second = capture_refused_waiting(OVER_CAP_MESSAGE, left(2));
        assert!(second.ends_with("in 10 minutes at the soonest"), "{second}");
        let third = capture_refused_waiting(OVER_CAP_MESSAGE, left(3));
        assert!(third.ends_with("in 30 minutes at the soonest"), "{third}");
        let one_tab = capture_refused_waiting(
            "overlap handoff failed safely: the park's capture refused session 3: visible \
             checkpoint set could not be committed canonically: too many sessions",
            left(2),
        );
        assert!(
            one_tab.ends_with("tries again when that tab changes, in 10 minutes at the soonest"),
            "{one_tab}"
        );
        let near = capture_refused_waiting(OVER_CAP_MESSAGE, Duration::from_secs(90));
        assert!(near.ends_with(", in 2 minutes at the soonest"), "{near}");
        let one = capture_refused_waiting(
            OVER_CAP_MESSAGE,
            CAPTURE_REFUSAL_PROBE + Duration::from_secs(1),
        );
        assert!(one.ends_with(", in a minute at the soonest"), "{one}");
        assert_eq!(
            capture_refused_waiting(OVER_CAP_MESSAGE, left(3))
                .split(" \u{b7} ")
                .next(),
            crate::update_apply_trouble::named_clause(OVER_CAP_MESSAGE)
        );
    }

    /// The download instant crosses the handoff on the wall clock and comes back on the
    /// successor's monotonic one, so the record says how long the whole update took, for
    /// this build only.
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

    /// What the lane answers by itself is a record; a lane that stopped is a decision row
    /// whose `Install now` is the Version menu's press; an attempt nothing retries is a
    /// failure row with the page.
    #[test]
    fn an_outcome_is_a_record_unless_the_person_acts_on_it() {
        let retry = outcome(
            "Couldn't install aterm v9.9.9",
            "will try again by itself",
            Severity::Info,
        );
        assert_eq!(retry.hold, Hold::LogOnly, "a self-retry is a record");
        assert!(retry.actions.is_empty());
        assert_eq!(retry.key.as_deref(), Some(KEY_OUTCOME));
        let warn = outcome("Update waits", "x", Severity::Warn);
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
        assert_eq!(installed.glyph.ch(), MOVING_IN);
        let ready = staged("9.9.9", 7, Some(ApplyPosture::ManualByConfig));
        assert_eq!(installed.title, ready.title, "one sentence…");
        assert_eq!(installed.glyph, ready.glyph, "…one mark");
        assert!(!installed.excerpt, "the capsule is the press");
        assert_eq!(
            installed.detail,
            [crate::app_update_screen::UPDATE_INSTALLED_DETAIL]
        );
        let bare = failed("Couldn't finish the update", "", false);
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

    /// A persistent failure is one warning per class: the title names the broken half, the
    /// excerpt since when, the whole sentence behind Details, the Software Update capsule,
    /// the warning's hold, never Standing.
    #[test]
    fn the_health_warning_names_the_half_and_since_when() {
        let body = "20 failed checks in a row since 2026-09-14T22:04:36Z: release manifests \
                    exist but cannot be downloaded.";
        for class in ["apply", "pipeline", "stage", "manifest"] {
            let m = health_warning(
                HealthKind::of_class(class),
                aterm_update::health_failing_title(class),
                body,
            );
            assert!(m.title.starts_with("Couldn't "), "{}", m.title);
            assert_eq!(m.detail[0], "since Sep 14", "{class}");
            assert!(
                m.detail[1..]
                    .join(" ")
                    .contains("release manifests exist but cannot be downloaded."),
                "{:?}",
                m.detail
            );
            assert_eq!(m.hold, Hold::Default);
            assert_eq!(m.severity, Severity::Warn);
            assert_eq!(m.key.as_deref(), Some(HealthKind::of_class(class).key()));
            assert_eq!(HealthKind::of_row(&m), Some(HealthKind::of_class(class)));
            assert_eq!(m.actions, vec![software_update()]);
            assert!(!m.detail.join(" ").contains("click"));
        }
        let undated = health_warning(
            HealthKind::Check,
            aterm_update::health_failing_title("manifest"),
            "no date here",
        );
        assert_eq!(undated.detail, vec!["no date here"]);
        {
            use crate::update_apply_trouble::PersonRemedy;
            let dated = health_notification_body(body, PersonRemedy::InstallSignedRelease);
            assert!(dated.starts_with("Since Sep 14"), "{dated}");
            assert_ne!(
                dated,
                health_notification_body(body, PersonRemedy::MoveOrAskAdmin),
                "each remedy its own words"
            );
            let undated = health_notification_body("no date", PersonRemedy::InstallSignedRelease);
            assert!(!undated.contains("Since"), "{undated}");
        }
        for title in [
            aterm_update::health_failing_title("apply"),
            CHECKER_STALLED_TITLE,
        ] {
            assert!(health_notification_title(title).starts_with("aterm"));
        }
        let healed = health_recovered("Couldn't install updates", "since Sep 14");
        assert_eq!(
            healed.detail,
            vec!["after \"Couldn't install updates\", since Sep 14"]
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
            image_kept: false,
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
            let line = staged_detail(ApplyPosture::HandoffDisabled {
                why,
                veto: None,
                image_kept: false,
            });
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
}
