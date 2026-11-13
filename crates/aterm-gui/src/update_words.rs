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

const FLOW_GLYPH: char = '\u{21bb}';
const MOVING_IN: char = '\u{21e3}';
const READY: char = '\u{2713}';
const NEEDS_YOU: char = '\u{26a0}';
const NOTED: char = '\u{2139}';

/// How a staged build will be applied: what the staged row's detail may promise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyPosture {
    /// `update.auto_apply` on (the default) and no veto.
    Automatic,
    /// The automatic lane is armed, but this process keeps the image it was launched with, has
    /// no live terminal session, and its last admission found the sessionless handoff unable to
    /// carry it (`App::kept_image_installs_at_next_launch`).
    AtNextLaunch,
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

fn software_update() -> Intent {
    Intent::OpenSettings {
        route: crate::native_settings::SettingsRoute::SoftwareUpdate
            .path()
            .to_string(),
    }
}

fn glyph(ch: char) -> Glyph {
    Glyph::or_fallback(ch)
}

fn row(severity: Severity, title: impl AsRef<str>) -> Message {
    Message::new(tags::UPDATE, severity, title)
}

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

fn flow(title: String, phase: &str, meter: Meter, hold: Hold) -> Message {
    let mark = if phase == CHECKING {
        FLOW_GLYPH
    } else {
        MOVING_IN
    };
    row(Severity::Info, title)
        .glyph(glyph(mark))
        .line(phase)
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
pub(crate) const fn apply_capsule_for(posture: Option<ApplyPosture>, build: u64) -> Option<Intent> {
    match posture {
        Some(
            ApplyPosture::HandoffDisabled { .. }
            | ApplyPosture::Automatic
            | ApplyPosture::AtNextLaunch
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
        ApplyPosture::AtNextLaunch => "installs once aterm is quit and opened again".to_string(),
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

/// When `build` — the one a handoff is about to install — finished
/// downloading in this process (`verified`, `App::update_verified`), on the
/// wall clock in Unix milliseconds: what the handoff carry hands the successor
/// (`WindowCarry::update_verified_unix_ms`).
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

/// `said`, then — after ` · ` — the apply failure `reason` in a person's words
/// when the apply lane can name it ([`crate::update_apply_trouble::named_clause`]).
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
        None => msg.sentence(detail),
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
        msg.action(software_update())
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

fn health_excerpt_since(line: &str, now_unix: i64) -> Option<i64> {
    let (month, day) = line.strip_prefix("since ")?.split_once(' ')?;
    let month = MONTHS.iter().position(|m| *m == month)? + 1;
    if !day.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let day: u32 = day.parse().ok()?;
    let now = aterm_types::rfc3339::format_rfc3339(u64::try_from(now_unix).ok()?);
    let year_str = now.get(..4)?;
    if !year_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i64 = year_str.parse().ok()?;
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

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// "Sep 14": the day the updater's sentence dates the failure from ([`aterm_update::health_notice_since`]), or `None` without a readable one.
pub(crate) fn health_since(body: &str) -> Option<String> {
    let stamp = aterm_update::health_notice_since(body)?;
    let m_str = stamp.get(5..7)?;
    let d_str = stamp.get(8..10)?;
    if !m_str.bytes().all(|b| b.is_ascii_digit()) || !d_str.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let month: usize = m_str.parse().ok()?;
    let day: u32 = d_str.parse().ok()?;
    Some(format!("{} {day}", MONTHS.get(month.checked_sub(1)?)?))
}

/// The supersede key of a dev build's standing: the newest standing a process read replaces the last.
pub(crate) const KEY_DEV_BUILD: &str = "update.dev-build";
/// The key of a dev build's shared-writes row ([`dev_build_entry`]).
pub(crate) const KEY_DEV_BUILD_WRITES: &str = "update.dev-build-writes";

/// What a dev build writes of the shared user state only the release writes unattended, in words, with what puts it back.
pub(crate) const fn dev_shared_writes(
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
#[path = "update_words_tests.rs"]
mod tests;
