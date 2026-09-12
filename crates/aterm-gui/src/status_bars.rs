// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE STATUS BARS — two thin, full-width, single-row chrome bands at the top of
//! every window: one for the ALab toolchain that `atpkg` is installing, one for
//! aterm's own self-update. Each bar exists ONLY while its lane has something to
//! say; it takes one terminal row while it is up (the grid is re-fitted under it,
//! exactly as the in-grid tab strip reserves its rows) and folds away when the
//! work ends, giving the row back.
//!
//! # Why rows, not an overlay
//!
//! The owner's brief (docs/design/STATUS-SURFACE.md, refined 2026-08-26): *"two
//! narrow status bars all the way across the top … push down the other windows
//! and when it's done fold up and the space expands … without overlaying the
//! terminal itself — the overlay blocks use of the terminal."* A floating card
//! covers cells a program may be painting and eats the presses that land on it;
//! a chrome ROW covers nothing — the terminal is exactly as usable with the bar
//! up as with it down, one row shorter. So the bars are composed-frame rows
//! (see `App::splice_tab_strip_with`), never a card in the notice slot.
//!
//! # What the bars may claim
//!
//! * The toolchain bar renders ONLY what [`crate::PkgProgressSnapshot`] supports
//!   (the classified read of `<prefix>/progress.json`): a not-running snapshot
//!   names its terminal outcome and never a live phase; an unknown schema `v`
//!   is one generic "Installing packages…" line. Program names are untrusted
//!   until they round-trip [`atpkg::store::ToolName`]; error text is
//!   control-stripped and capped ([`atpkg::progress::sanitize_for_tty`]).
//! * The update bar renders [`aterm_update::Progress`] as the updater reported it
//!   from inside its own check — download bytes are the `.part` file's size
//!   against the release asset's declared size (0 ⇒ no meter, honestly). For a
//!   STAGED build it also states the apply posture the App computed
//!   ([`ApplyPosture`]): HOW the build will be applied — in place, by itself or
//!   on a click — never a landing the lane has not been asked for, and never a
//!   restart, because the mechanism is the in-session overlap handoff and the
//!   shells keep running.
//!
//! # Lifecycle, and why a bar can never get stuck
//!
//! A bar opens on a lane's first report and stays while the lane is LIVE. Every
//! terminal report (installed / failed / staged / …) arms a fold deadline
//! ([`HOLD_OK`] for good news, [`HOLD_WARN`] for bad — long enough to read, short
//! enough that an ignored failure does not keep a row forever; the durable record
//! is Settings ▸ Packages / Settings ▸ Software Update, which a click on the bar
//! opens). [`StatusBars::settle`] retires expired bars; the App folds
//! [`StatusBars::deadline`] into its single `about_to_wait` deadline so the fold
//! happens on time and costs no idle wake otherwise (FL-1: hidden bars have a
//! `0` fingerprint and no deadline — byte-identical to the no-bar path).
//!
//! # Structure
//!
//! Everything here is PURE: [`StatusBars`] is the state machine (inputs are the
//! lane reports, a clock is injected), [`paint_rows`] turns it into
//! [`RenderCell`] rows at a width, and [`layout`] is the width law between them —
//! all three unit-test without a window.

use std::time::{Duration, Instant};

use aterm_core::terminal::{RenderCell, UnderlineStyle};
use aterm_render::Theme;

use crate::app_update_handoff::HandoffUnavailable;
use crate::chrome_band::{self, BandColors};
use crate::settings::{blank_row, write_str};

use atpkg::progress::{PROGRESS_VERSION, Phase, sanitize_for_tty};

/// How long a bar holds a GOOD terminal outcome before folding.
pub(crate) const HOLD_OK: Duration = Duration::from_secs(8);
/// How long a bar holds a BAD terminal outcome (failed / partial / unusable /
/// stopped) before folding. Longer than [`HOLD_OK`] because the user has to
/// read a sentence and may want to click through; bounded because a status bar
/// that never leaves is the floating card's mistake in a different shape.
pub(crate) const HOLD_WARN: Duration = Duration::from_secs(45);
/// How long a NOTICE holds — a text row posted through `aterm ctl appnotice`:
/// longer than [`HOLD_OK`] because it may name versions a person wants to read,
/// shorter than [`HOLD_WARN`] because nothing is wrong.
pub(crate) const HOLD_NOTICE: Duration = Duration::from_secs(30);
/// How long the "Claude Code X · Codex Y — aterm-managed, current" row holds.
/// Its own constant (UX review, 2026-09-10): good news that comes at every open
/// needs less time than a notice, and an undo-bearing machine-settings row may
/// be waiting behind it.
pub(crate) const HOLD_MANAGED: Duration = Duration::from_secs(15);
/// How long a STAGED update bar stays up while the AUTOMATIC lane is armed to
/// apply it — the pull-down IS the "update ready" surface now (2026-09-07),
/// so it holds until the apply lands (forced within ~2 min; the typing hold can
/// stand the lane down for up to ten) rather than folding after eight seconds
/// and leaving the moment to a floating card.
pub(crate) const HOLD_STAGED_AUTOMATIC: Duration = Duration::from_secs(10 * 60);
/// How long a STAGED bar holds when the build applies only on a press: long
/// enough to be seen and clicked, short enough that a declined update does not
/// keep a row (the Version menu's ⬆️ stays after it folds).
pub(crate) const HOLD_STAGED_MANUAL: Duration = Duration::from_secs(60);
/// The staleness cap on the "installing" / "finishing" bars that bracket the
/// in-session handoff: the readiness deadline that bounds the whole attempt is
/// env-clamped to 120 s, and every path that ends the freeze replaces the bar
/// explicitly — this is the backstop, not the lifetime.
pub(crate) const HANDOFF_STALE: Duration = Duration::from_secs(125);
/// How long a LIVE toolchain bar fed by the progress tailer may go without a
/// report before it folds quietly. The tailer posts every heartbeat change
/// (≤ 2 s apart while atpkg is alive) and one final read at child exit, so a
/// silence this long means the tailer is gone — a foreign writer the GUI cannot
/// tail, a wedged child — and the bar has nothing honest left to say.
const TAILED_STALE: Duration = Duration::from_secs(30);
/// The cap on an ANNOUNCEMENT-only toolchain bar (`seed-starting:` with no
/// progress file to tail — no store layout): a terminal marker normally answers
/// it; if none ever comes, it folds after the same hold the old pill had.
const ANNOUNCE_STALE: Duration = Duration::from_secs(20 * 60);
/// The cap on a LIVE update bar. The updater's download poller reports only on
/// size CHANGE and its verify phase (codesign / Gatekeeper) can sit silent for
/// tens of seconds, so this is long — but a check is bounded by curl's own
/// `--max-time` and always ends in a `Staged` / `Failed` / `Deferred` report
/// from the same process, so this only ever fires for a process that died.
const UPDATE_STALE: Duration = Duration::from_secs(15 * 60);
/// The meter's preferred width in cells, and the narrowest it is drawn at all.
const METER_PREFERRED: usize = 20;
const METER_MIN: usize = 8;
/// Left / right margin cells so the text never touches the window edge.
const MARGIN: usize = 1;
/// Cap on a sanitized failure reason inside a bar.
const ERROR_CAP: usize = 60;
/// Cap on an OUTCOME detail — long enough for the updater's health sentence
/// (count, date, cause, the `aterm-ctl` command and the Settings pointer) to
/// arrive whole. Measured on m21 (2026-09-10): the body was 178 chars, the
/// Settings clause made it 211, and the old 160-char cap cut the row at
/// `Run \`aterm-c…` — both affordances lost on the one row whose job was to
/// route the user somewhere. Shaping ([`shape_detail`]) keeps the command
/// intact even when the prose is longer than this.
const DETAIL_CAP: usize = 240;
/// Cap on a program name inside a bar (the store's own names are short).
const NAME_CAP: usize = 24;
/// The fewest cells a detail is drawn in when it does not fit whole: below this
/// it is dropped rather than shown as a stub ("these are wha…" at 80 cols was
/// the row the 2026-09-10 review measured). Drawn whole-ish or not at all.
const DETAIL_FLOOR: usize = 20;

/// Which bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    /// The ALab toolchain install (`atpkg seed` / `atpkg update`).
    Toolchain,
    /// aterm's own self-update (download → verify → staged).
    Update,
}

/// How a STAGED build will be applied — what the update bar's detail line may
/// promise. Computed by the App (`App::apply_posture_for`), which is the only
/// holder of the policy, the environment vetoes and the lane's own state; the
/// bar just says it. The mechanism behind every arm is the in-session overlap
/// handoff — the successor adopts every window, tab, split and live shell — so
/// none of them asks for a restart; the last arm is the one where that handoff
/// has been switched off, and there the honest line is that nothing applies
/// while a terminal is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApplyPosture {
    /// `update.auto_apply` on (the default) and no veto: the seamless lane lands
    /// it at the first quiet moment, forced no later than
    /// `AUTOMATIC_UPDATE_ACTIVITY_GRACE`.
    Automatic,
    /// `[update] auto_apply = false`.
    ManualByConfig,
    /// An environment veto for this run: `ATERM_NO_AUTO_APPLY` or the
    /// `ATERM_DEBUG_RELAUNCH_NUDGE` screenshot seam.
    VetoedByEnv { var: &'static str },
    /// The automatic lane stood down for THIS build (failed / blocked attempts,
    /// or the typing hold). `lapses`: the latch carries a deadline and re-arms by
    /// itself.
    ManualOnlyLatched { lapses: bool },
    /// The in-session handoff cannot run in this process — `why` names the
    /// reason ([`HandoffUnavailable`]: the `ATERM_NO_SEAMLESS_UPDATE` opt-out,
    /// an explicit `--control-sock`, `--headless`, no event loop), read from the
    /// SAME predicate the apply gate reads (`App::seamless_handoff_unavailable`).
    /// That is NOT "a cold re-exec instead": the admission classifier
    /// (`native_update_admission::classify`) admits the cold lane only with
    /// zero live PTYs and REFUSES the apply while any terminal is open — no
    /// shell dies, nothing applies. With the automatic lane armed (`veto:
    /// None`) the update lands at the next launch (or once every terminal is
    /// closed); with it vetoed (`veto` names what stands in the way) that
    /// closed-terminals landing is a promise the lane never arms, so the
    /// sentence names the veto and the manual affordance instead. The one
    /// posture whose honest sentence is a warning, and the warning is "will not
    /// apply", never "will not survive"; it outranks every other arm, because
    /// the others change WHEN the build applies and this one changes WHETHER.
    HandoffDisabled {
        why: HandoffUnavailable,
        veto: Option<AutoApplyVeto>,
    },
}

/// Why the AUTOMATIC apply lane would not arm even if the handoff were
/// available — carried into [`ApplyPosture::HandoffDisabled`] so its fallback
/// clause cannot promise the cold apply "once every terminal is closed" over a
/// lane that never attempts it. Same precedence as the standalone arms:
/// the environment veto, then config, then the screenshot seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AutoApplyVeto {
    /// `[update] auto_apply = false`.
    Config,
    /// `$ATERM_NO_AUTO_APPLY` or the `$ATERM_DEBUG_RELAUNCH_NUDGE` screenshot
    /// seam.
    Env { var: &'static str },
}

impl AutoApplyVeto {
    /// The clause the sentence leads the fallback with — what the reader can
    /// act on, worded like the standalone arms for the same states.
    fn clause(self) -> String {
        match self {
            Self::Config => "auto-apply is off".to_string(),
            Self::Env { var } => format!("${var} is set"),
        }
    }
}

/// The manual affordance, named the same way on every surface. Off macOS the
/// Version menu is the palette's Version section and the in-grid tab strip
/// carries the `↻`.
const APPLY_FROM_MENU: &str = if cfg!(target_os = "macos") {
    "apply it from the Version menu"
} else {
    "apply it from the Version menu or the \u{21bb} button"
};

/// The press affordance on a Staged bar (2026-09-07): a left press on the row
/// applies the build in place, the way the retired floating card's press did.
/// Not offered where the handoff is off — there a press can only open the
/// details page, which the bar does for every other state.
const CLICK_TO_APPLY: &str = "click to apply now";

/// The "Installing / Finishing" row's title: names the version when there is
/// one, and says "update" when there is not (the re-exec QA seam applies with
/// no staged version — a bare "aterm v" was measured on glass 2026-09-07).
fn handoff_title(verb: &str, version: &str) -> String {
    let v = sanitize_for_tty(version, 32);
    if v.trim().is_empty() {
        format!("{verb} update")
    } else {
        format!("{verb} aterm v{v}")
    }
}

/// The separator between a bar title and the press affordance appended to it.
const TITLE_SEP: &str = " \u{b7} ";

/// `base` plus the press affordance ([`CLICK_TO_APPLY`]) wherever a press
/// applies — in the TITLE, which the layout keeps whole at every ordinary
/// width, never at the truncated end of the detail (where it was measured cut
/// off at 100 columns on 2026-09-07). Omitted where the handoff is off: there
/// a press can only open the details page.
fn with_affordance(base: &str, posture: Option<ApplyPosture>) -> String {
    if matches!(posture, Some(ApplyPosture::HandoffDisabled { .. })) {
        base.to_string()
    } else {
        format!("{base}{TITLE_SEP}{CLICK_TO_APPLY}")
    }
}

/// Re-state one STAGED bar for `build` under `posture`: its words, and its hold
/// re-anchored by posture — extended to the armed stretch for a lane that will
/// apply by itself, shortened to the manual hold for one standing down for good.
/// `false` (and untouched) for any other bar, or when nothing changed.
fn restate_staged_bar(bar: &mut Bar, build: u64, posture: ApplyPosture, now: Instant) -> bool {
    if bar.staged_build != Some(build) || bar.text.glyph != '\u{2713}' {
        return false;
    }
    let hold = now + staged_hold(Some(posture));
    bar.fold_at = Some(match posture {
        ApplyPosture::Automatic | ApplyPosture::ManualOnlyLatched { lapses: true } => {
            bar.fold_at.map_or(hold, |at| at.max(hold))
        }
        _ => bar.fold_at.map_or(hold, |at| at.min(hold)),
    });
    let base = without_affordance(&bar.text.title).to_string();
    let title = with_affordance(&base, Some(posture));
    let detail = staged_detail(build, posture);
    if bar.text.detail == detail && bar.text.title == title {
        return false;
    }
    bar.text.detail = detail;
    bar.text.title = title;
    true
}

/// A detail that ALWAYS keeps its command. Sanitized like every other cell
/// string; when longer than `cap`, the PROSE is what gives way — the tail from
/// the first backtick-quoted command sentence (`Run \`…\` …`) to the end is kept
/// whole and the prefix is cut to fit, with the ellipsis on the prose. A detail
/// without a command truncates from the right as before. Pure, so the width law
/// is testable on literal values.
pub(crate) fn shape_detail(detail: &str, cap: usize) -> String {
    let clean = sanitize_for_tty(detail, usize::MAX);
    if clean.chars().count() <= cap {
        return clean;
    }
    // The command sentence: "Run `…" when there is one, else the first backtick.
    let command_at = clean.find("Run `").or_else(|| clean.find('`')).map(|at| {
        // Back up to the start of the sentence the backtick sits in, so the
        // kept tail reads as a sentence and not as half of one.
        clean[..at].rfind(". ").map_or(at, |dot| dot + 2).min(at)
    });
    let Some(at) = command_at else {
        return sanitize_for_tty(&clean, cap);
    };
    let tail = &clean[at..];
    let tail_len = tail.chars().count();
    if tail_len + 2 > cap {
        // Even the command sentence alone is too long: keep as much of IT as fits
        // from its start — the command is at its front.
        return sanitize_for_tty(tail, cap);
    }
    let head_budget = cap - tail_len - 2;
    let mut out: String = clean[..at].trim_end().chars().take(head_budget).collect();
    out = out.trim_end().to_string();
    out.push_str("\u{2026} ");
    out.push_str(tail);
    out
}

/// A title without the press affordance — exact-suffix, so a version string
/// that happens to contain the separator is left alone. What the ledger
/// records (`appstatus` cannot click) and what a restatement rebuilds from.
fn without_affordance(title: &str) -> &str {
    title
        .strip_suffix(CLICK_TO_APPLY)
        .and_then(|t| t.strip_suffix(TITLE_SEP))
        .unwrap_or(title)
}

/// The Staged bar's title: "aterm vX is ready", with the press affordance
/// ([`with_affordance`]).
fn staged_title(version: &str, posture: Option<ApplyPosture>) -> String {
    with_affordance(
        &format!("aterm v{} is ready", sanitize_for_tty(version, 32)),
        posture,
    )
}

/// How long a Staged bar holds: while the AUTOMATIC lane is armed (or stands
/// down but retries by itself) the pull-down IS the update-ready surface and
/// waits for the apply; where the build applies only on a press it holds long
/// enough to be seen and clicked.
fn staged_hold(posture: Option<ApplyPosture>) -> Duration {
    match posture {
        Some(ApplyPosture::Automatic | ApplyPosture::ManualOnlyLatched { lapses: true }) => {
            HOLD_STAGED_AUTOMATIC
        }
        _ => HOLD_STAGED_MANUAL,
    }
}

/// The Staged bar's detail: `build N — verified; <how it applies>`. One
/// sentence per posture, each true of the mechanism it names and none of them
/// a restart. The press affordance is the TITLE's ([`staged_title`]), where
/// the width law keeps it visible.
#[must_use]
pub(crate) fn staged_detail(build: u64, posture: ApplyPosture) -> String {
    let how = match posture {
        ApplyPosture::Automatic => {
            // The promise tracks the constant that enforces it.
            let minutes = crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE
                .as_secs()
                .div_ceil(60);
            format!("applies in place within ~{minutes} min — your shells keep running")
        }
        ApplyPosture::ManualByConfig => format!(
            "auto-apply is off — {APPLY_FROM_MENU} or Software Update (in place; your shells \
             keep running)"
        ),
        ApplyPosture::VetoedByEnv { var } => {
            format!("${var} is set — {APPLY_FROM_MENU} (in place; your shells keep running)")
        }
        ApplyPosture::ManualOnlyLatched { lapses: true } => format!(
            "automatic apply stood down for now — it retries by itself, or {APPLY_FROM_MENU}"
        ),
        ApplyPosture::ManualOnlyLatched { lapses: false } => format!(
            "automatic apply stood down — it will not try again until you {APPLY_FROM_MENU}"
        ),
        // The load-bearing clause comes first: the layout truncates the detail
        // from the right when the window is narrow.
        ApplyPosture::HandoffDisabled { why, veto: None } => format!(
            "{} — the handoff is off: it will NOT apply while any terminal is open; \
             it lands at the next launch (or once every terminal is closed)",
            why.cause()
        ),
        // With the automatic lane ALSO vetoed, "once every terminal is closed"
        // would promise a landing the lane never arms: name the veto and the
        // manual affordance instead (the next-launch boot apply stays true —
        // it is not policy-gated).
        ApplyPosture::HandoffDisabled {
            why,
            veto: Some(veto),
        } => format!(
            "{} — the handoff is off: it will NOT apply while any terminal is open; \
             {} — {APPLY_FROM_MENU} once every terminal is closed, or it lands at the next launch",
            why.cause(),
            veto.clause()
        ),
    };
    format!("build {build} — verified; {how}")
}

/// The Staged line for a caller that computed no posture: only the manual
/// affordance, which is true whatever the policy — never a promise about the
/// automatic lane, and never "auto-apply is off" painted over a lane that may
/// be armed.
fn staged_detail_unknown(build: u64) -> String {
    format!("build {build} — verified; {APPLY_FROM_MENU} (in place; your shells keep running)")
}

/// The bar's colour mood — information, a good end, or something the user
/// should look at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tone {
    Info,
    Success,
    Warn,
}

/// The words on one bar, in the order they are laid out. Pure data so the
/// layout law and the painter are testable on literal values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BarText {
    /// The leading pictogram (⇣ ↻ ✓ ⚠ ⏸).
    pub glyph: char,
    /// Bold, always shown (truncated only on an absurdly narrow window).
    pub title: String,
    /// Secondary text after the title; the first thing dropped when narrow.
    pub detail: String,
    /// Right-aligned figures ("512 MB / 1.2 GB · 3 of 10"); dropped before
    /// the meter is, since the meter says the same thing at a glance.
    pub stats: String,
    pub tone: Tone,
}

/// One live bar.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bar {
    pub text: BarText,
    /// Determinate fill `0..=1`, or `None` — no meter (unknown total, or a
    /// phase that is not a byte stream).
    pub fill: Option<f32>,
    /// When this bar folds on its own; `None` while its lane is live.
    pub fold_at: Option<Instant>,
    /// For a LIVE bar: when it folds if no further report arrives (see
    /// [`TAILED_STALE`] and friends) — a reserved row must never outlive the
    /// process feeding it. `None` on a terminal bar (`fold_at` rules there).
    pub stale_at: Option<Instant>,
    /// The toolchain pass this bar was built from (`pass`, `started_unix`), so a
    /// live meter is clamped to its own high-water mark and never inherits a
    /// different pass's. `None` off the toolchain lane / before a snapshot.
    pub pass_id: Option<(String, u64)>,
    /// The build a STAGED update bar is about, so a later, better-informed
    /// posture can be re-stated on it ([`StatusBars::restate_apply_posture`]) and
    /// on nothing else. `None` on every other bar.
    pub staged_build: Option<u64>,
    /// The STANDING check-health warning ([`StatusBars::update_health_standing`]):
    /// no hold and no staleness cap — it leaves when the ledger heals
    /// ([`StatusBars::update_health_healed`]) or the process ends. `false` on
    /// every other bar.
    pub health: bool,
}

impl Bar {
    /// The "Installing" row ([`installing_bar`]) — what a refusal retires.
    fn is_installing(&self) -> bool {
        self.text.glyph == '\u{2191}' && self.text.title.starts_with("Installing ")
    }

    fn terminal(&self) -> bool {
        self.fold_at.is_some()
    }

    /// The instant this bar leaves on its own: its hold, or its staleness cap.
    fn retires_at(&self) -> Option<Instant> {
        self.fold_at.or(self.stale_at)
    }
}

/// How many finished activities the ledger keeps.
///
/// Presentation is ephemeral — a bar folds and the sentence is gone — but the
/// RECORD is not: `aterm ctl appstatus` answers "what has this app been doing"
/// after the fact, which is the question an operator (or a driving agent) asks
/// once the screen has moved on. A fixed ring, because the alternative is a list
/// that grows for the life of a process that may run for weeks.
const LEDGER_ROWS: usize = 32;

/// One finished activity, kept after its bar folded.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LedgerRow {
    pub lane: Lane,
    /// The bar's last words — what the user would have read.
    pub title: String,
    pub detail: String,
    /// How it ended, from the tone the last bar carried.
    pub outcome: Outcome,
    /// When the bar retired, as this process's monotonic clock.
    pub finished: Instant,
}

/// How a finished activity ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Ok,
    Warn,
}

impl Outcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
        }
    }
}

impl Lane {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Toolchain => "toolchain",
            Self::Update => "update",
        }
    }
}

/// The "Installing" row: the words the outgoing process shows from the park
/// until the successor takes over ([`StatusBars::update_installing`]). Not a
/// Staged bar (`staged_build: None`): a press on it opens the details, and the
/// posture restatement leaves it alone.
fn installing_bar(version: &str, now: Instant) -> Bar {
    Bar {
        text: BarText {
            glyph: '\u{2191}',
            title: handoff_title("Installing", version),
            detail: "your shells are safe; the screen pauses for a moment".to_string(),
            stats: String::new(),
            tone: Tone::Info,
        },
        fill: None,
        fold_at: None,
        stale_at: Some(now + HANDOFF_STALE),
        pass_id: None,
        staged_build: None,
        health: false,
    }
}

/// The two-lane state.
#[derive(Default, Debug)]
pub(crate) struct StatusBars {
    toolchain: Option<Bar>,
    update: Option<Bar>,
    /// Finished activities, oldest first, capped at [`LEDGER_ROWS`].
    ledger: std::collections::VecDeque<LedgerRow>,
    /// The words of a toolchain bar re-seeded from a handoff carry
    /// ([`Self::seed_carried`]): this process has no feed for the pass they
    /// describe. Compared at Commit, so a bar this process has since written
    /// itself is recognised by its different words and left alone.
    carried_toolchain: Option<BarText>,
    /// The Staged bar an OUTCOME row was posted over ([`Self::update_outcome`]):
    /// the build is still staged and the press still applies it, so when the
    /// outcome folds the ready row comes back — within its own hold — rather
    /// than leaving the lane empty after one transient blocker. Cleared by
    /// any new report on the lane.
    staged_behind_outcome: Option<Bar>,
    /// The STANDING health warning an OUTCOME row was posted over
    /// ([`Self::update_outcome`], [`Self::update_health_standing`] while another
    /// row holds the lane): the ledger still says checks are failing, so when the
    /// covering row retires the warning comes back — restated with its newest
    /// words — rather than vanishing for the rest of the process (the updater's
    /// `HealthNotify` fires ONCE per class, so nothing else would restate it;
    /// 2026-09-10 review). It leaves only through [`Self::update_health_healed`],
    /// into the ledger, like the row on glass.
    health_behind_outcome: Option<Bar>,
    /// TERMINAL toolchain rows waiting behind the live one, oldest first, each
    /// with the hold it takes when it is promoted ([`Self::settle_with`]). One
    /// pass can end with several things to say — "installed", then "Claude Code
    /// X · Codex Y — aterm-managed, current", then "machine settings applied" —
    /// and a lane holds one bar; replacement by assignment showed only the last
    /// and DROPPED the rest, unrecorded (they never folded, so they never reached
    /// the ledger). Queued rows are read in order and each retires into the
    /// ledger like any other.
    toolchain_queue: std::collections::VecDeque<(Bar, Duration)>,
    /// The `managed-current:` wire text the lane last raised a row for. atpkg
    /// prints the marker on EVERY pass, changed or not, and the recurring 6 h
    /// pass parses it exactly like the seed pass — so the row went up on every
    /// no-change pass, moving the grid a row for 30 s twice (two PTY resizes for
    /// a running TUI; UX review, 2026-09-10). The same text again is recorded
    /// in the ledger and not posted; the first pass after launch always posts.
    last_managed_current: Option<String>,
}

impl StatusBars {
    /// The successor's bars at construction: the rows the outgoing process
    /// carried ([`crate::session_store::WindowCarry::bars`]), re-seeded live,
    /// with the update lane's words replaced by "finishing" (`finishing` is the
    /// running build's version) when this process is the handed-off successor
    /// and the carry had an update row. Empty carry ⇒ the default (no bars).
    pub(crate) fn from_handoff_carry(
        bars: &[crate::session_store::CarriedBar],
        finishing: Option<&str>,
        now: Instant,
    ) -> Self {
        let mut this = Self::default();
        this.seed_carried(bars, now);
        if let Some(version) = finishing
            && this.update.is_some()
        {
            this.update_finishing(version, now);
        }
        this
    }

    /// How many chrome rows the bars take right now (0, 1 or 2).
    pub(crate) fn rows(&self) -> u16 {
        u16::from(self.toolchain.is_some()) + u16::from(self.update.is_some())
    }

    /// The live bars, top to bottom: toolchain above update.
    pub(crate) fn bars(&self) -> impl Iterator<Item = (Lane, &Bar)> {
        self.toolchain
            .iter()
            .map(|b| (Lane::Toolchain, b))
            .chain(self.update.iter().map(|b| (Lane::Update, b)))
    }

    /// Which lane occupies bar row `index` (0 = topmost), if any.
    pub(crate) fn lane_at(&self, index: usize) -> Option<Lane> {
        self.bars().nth(index).map(|(lane, _)| lane)
    }

    /// Retire every bar whose hold has elapsed — or whose feed went silent past
    /// its staleness cap. Returns whether the GLASS changed: a row count that
    /// moved (the caller's re-grid trigger), or the Staged row put back in
    /// place of a folded outcome ([`Self::update_outcome`]).
    /// Shipping code settles through `App::settle_status_bars`, which knows
    /// whether a handoff is freezing the holds; this is the plain form the
    /// tests read.
    #[cfg(test)]
    pub(crate) fn settle(&mut self, now: Instant) -> bool {
        self.settle_with(now, true)
    }

    /// [`Self::settle`], with the HOLDS optionally suspended: `holds == false`
    /// retires only what has passed its STALENESS CAP. That is the shape a
    /// handoff freeze needs — the row count is frozen for Commit while the
    /// screen is parked, so a bar folding on its hold would leave the committed
    /// row painted as a hole for the rest of the freeze — without disarming the
    /// cap that exists for exactly the case where Commit never comes.
    pub(crate) fn settle_with(&mut self, now: Instant, holds: bool) -> bool {
        let before = self.rows();
        let mut retired: Vec<LedgerRow> = Vec::new();
        let mut update_retired = false;
        for (lane, slot) in [
            (Lane::Toolchain, &mut self.toolchain),
            (Lane::Update, &mut self.update),
        ] {
            if slot.as_ref().is_some_and(|b| {
                let at = if holds { b.retires_at() } else { b.stale_at };
                at.is_some_and(|at| now >= at)
            }) {
                // THE ROW OUTLIVES THE BAR. What the user could have read goes
                // into the ledger as it leaves the glass, so `appstatus` can
                // answer for it afterwards.
                if let Some(bar) = slot.take() {
                    update_retired |= lane == Lane::Update;
                    retired.push(LedgerRow {
                        lane,
                        // Never "click to apply now" in the record of a row
                        // that is no longer on glass.
                        title: without_affordance(&bar.text.title).to_string(),
                        detail: bar.text.detail,
                        outcome: match bar.text.tone {
                            Tone::Warn => Outcome::Warn,
                            Tone::Info | Tone::Success => Outcome::Ok,
                        },
                        finished: now,
                    });
                }
            }
        }
        for row in retired {
            self.record(row);
        }
        // The outcome has had its say: the ready row it covered returns, if its
        // own hold has not run out meanwhile. Same row count — a repaint, not a
        // re-grid.
        let mut restored = false;
        if update_retired
            && let Some(staged) = self.staged_behind_outcome.take()
            && staged.fold_at.is_some_and(|at| now < at)
        {
            self.update = Some(staged);
            restored = true;
        }
        // No ready row to hand back: the standing health warning the outcome
        // covered returns instead (it waits behind a restored Staged row, and
        // comes back when THAT folds — a stage outranks it, as ever).
        if update_retired
            && !restored
            && let Some(health) = self.health_behind_outcome.take()
        {
            self.update = Some(health);
            restored = true;
        }
        // The toolchain lane is free: the next queued terminal row takes it, with
        // its hold anchored NOW (not at the instant it was posted, or a row queued
        // behind a long hold would arrive already expired). Same row count — a
        // repaint, not a re-grid.
        let mut promoted = false;
        if self.toolchain.is_none()
            && let Some((mut bar, hold)) = self.toolchain_queue.pop_front()
        {
            bar.fold_at = Some(now + hold);
            bar.stale_at = None;
            self.toolchain = Some(bar);
            promoted = true;
        }
        self.rows() != before || restored || promoted
    }

    /// A finished activity into the ledger, oldest first, capped at
    /// [`LEDGER_ROWS`].
    fn record(&mut self, row: LedgerRow) {
        if self.ledger.len() == LEDGER_ROWS {
            self.ledger.pop_front();
        }
        self.ledger.push_back(row);
    }

    /// The finished activities, oldest first — the `appstatus` ledger.
    pub(crate) fn ledger(&self) -> impl Iterator<Item = &LedgerRow> {
        self.ledger.iter()
    }

    /// The `appstatus` reply body: one `activity` row per LIVE bar, then one per
    /// finished activity the ring still holds, oldest first — the order a reader
    /// reconstructs the sequence in.
    ///
    /// Rendered here, next to the state, rather than in the control verb: the
    /// verb thread cannot see App state at all, and the wire grammar is a
    /// SEPARATE safety question from the glass. `layout` sanitizes for cells;
    /// this percent-encodes for a line-oriented protocol, so a program name with
    /// a space — or a newline out of some installer's stderr — cannot forge a
    /// row or truncate the reply.
    pub(crate) fn activity_rows(&self, now: Instant) -> Vec<String> {
        let enc = crate::control::pct_encode;
        let mut rows = Vec::with_capacity(self.rows() as usize + self.ledger.len());
        for (lane, bar) in self.bars() {
            let progress = bar.fill.map_or_else(
                || "-".to_string(),
                |f| format!("{}/100", (f.clamp(0.0, 1.0) * 100.0).round() as u32),
            );
            rows.push(format!(
                "activity kind={} phase=live progress={progress} title={} detail={} stats={} outcome=-",
                lane.as_str(),
                enc(&bar.text.title),
                enc(&bar.text.detail),
                enc(&bar.text.stats),
            ));
        }
        for row in self.ledger() {
            rows.push(format!(
                "activity kind={} phase=done progress=- title={} detail={} stats= outcome={} since_ms={}",
                row.lane.as_str(),
                enc(&row.title),
                enc(&row.detail),
                row.outcome.as_str(),
                now.saturating_duration_since(row.finished).as_millis(),
            ));
        }
        rows
    }

    /// The next instant a bar leaves on its own: a held outcome's fold, or a
    /// live bar's staleness cap. `None` with no bar up.
    /// Shipping code arms through `App::status_bars_deadline`, the twin of
    /// `App::settle_status_bars`; this is the plain form the tests read.
    #[cfg(test)]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline_with(true)
    }

    /// [`Self::deadline`] for a settle whose HOLDS are suspended
    /// (`holds == false`, the handoff freeze — see [`Self::settle_with`]): the
    /// next instant that settle would actually act on, which is a staleness cap.
    /// The two MUST agree: an armed deadline the settle then declines to act on
    /// is a wake that does nothing and re-arms the same past instant.
    pub(crate) fn deadline_with(&self, holds: bool) -> Option<Instant> {
        self.bars()
            .filter_map(|(_, b)| if holds { b.retires_at() } else { b.stale_at })
            .min()
    }

    /// The repaint-key term: **exactly `0` when no bar is up** (the byte-identical
    /// no-bar key), else a nonzero FNV-1a over everything the painter reads.
    pub(crate) fn fingerprint(&self) -> u64 {
        if self.rows() == 0 {
            return 0;
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut fold = |bytes: &[u8]| {
            for &b in bytes {
                h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        for (lane, bar) in self.bars() {
            fold(&[lane as u8, bar.text.tone as u8, u8::from(bar.terminal())]);
            let mut buf = [0u8; 4];
            fold(bar.text.glyph.encode_utf8(&mut buf).as_bytes());
            fold(bar.text.title.as_bytes());
            fold(&[0]);
            fold(bar.text.detail.as_bytes());
            fold(&[0]);
            fold(bar.text.stats.as_bytes());
            fold(&[0]);
            // Quantized to whole percent — the resolution the "NN%" readout
            // shows and finer than any meter the layout draws — so a byte tick
            // that cannot move a glyph does not re-present the frame. (The stats
            // string is MB-granular, so a download re-presents at most once per
            // megabyte; the tailer's 10 Hz is the ceiling either way.)
            let q = bar
                .fill
                .map_or(u32::MAX, |f| (f.clamp(0.0, 1.0) * 100.0) as u32);
            fold(&q.to_le_bytes());
        }
        h | 1
    }

    // ---- the toolchain lane -------------------------------------------------

    /// `atpkg` announced a pass (`seed-starting:` / `net-starting:`) — the bar
    /// opens NOW, before `progress.json` exists, because the extraction that
    /// follows is minutes long and gigabytes wide and an app doing that silently
    /// is indistinguishable from one that is misbehaving. `detail` is atpkg's
    /// own sentence; its trailing "(…)" carries the size, which the bar keeps.
    pub(crate) fn toolchain_announced(&mut self, detail: &str, now: Instant) {
        let size = detail
            .rsplit_once('(')
            .and_then(|(_, t)| t.strip_suffix(')'))
            .map(|s| sanitize_for_tty(s, 40))
            .filter(|s| !s.is_empty());
        self.toolchain = Some(Bar {
            text: BarText {
                glyph: '\u{21e3}',
                title: "Installing the ALab toolchain".to_string(),
                detail: size
                    .map_or_else(|| "starting…".to_string(), |s| format!("{s} — starting…")),
                stats: String::new(),
                tone: Tone::Info,
            },
            fill: None,
            fold_at: None,
            stale_at: Some(now + ANNOUNCE_STALE),
            pass_id: None,
            staged_build: None,
            health: false,
        });
    }

    /// One classified `progress.json` read from the child-scoped tailer
    /// (`Wake::PkgProgress`). `None` = the file vanished at child exit.
    ///
    /// A snapshot may NOT overwrite a terminal outcome a marker already posted
    /// (`installed` / `failed` / …): atpkg prints its markers AFTER `end_pass`,
    /// and the tailer's final read is posted after the marker line was read, so
    /// the marker's richer sentence arrives first and must win. The meter still
    /// completes.
    pub(crate) fn toolchain_snapshot(
        &mut self,
        snap: Option<&crate::PkgProgressSnapshot>,
        now: Instant,
    ) {
        let Some(snap) = snap else {
            // No data: a live bar without its file is a bar with nothing honest
            // to say — unless a terminal marker already gave it its last words.
            if self.toolchain.as_ref().is_some_and(|b| !b.terminal()) {
                self.toolchain = None;
            }
            return;
        };
        let f = &snap.file;
        if self.toolchain.as_ref().is_some_and(Bar::terminal) {
            if let Some(bar) = self.toolchain.as_mut()
                && bar.fill.is_some()
            {
                bar.fill = Some(1.0);
            }
            return;
        }
        if f.v != PROGRESS_VERSION {
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: '\u{21e3}',
                    title: "Installing packages…".to_string(),
                    detail: "a newer aterm is writing this progress format".to_string(),
                    stats: String::new(),
                    tone: Tone::Info,
                },
                fill: None,
                fold_at: if snap.running {
                    None
                } else {
                    Some(now + HOLD_OK)
                },
                stale_at: snap.running.then_some(now + TAILED_STALE),
                pass_id: None,
                staged_build: None,
                health: false,
            });
            return;
        }
        let title = match f.pass.as_str() {
            "seed" => "Preparing the ALab toolchain",
            "net" => "Installing the ALab toolchain",
            _ => "Installing packages",
        };
        let total = f.overall.programs_total;
        let done = f.overall.programs_done;
        // THE NO-OP PASS STAYS INVISIBLE. atpkg begins its "net" pass BEFORE the
        // signed index resolves — at every launch and every 6 h — and the common
        // outcome is a plan of zero programs. A bar that appears for that,
        // re-grids every window, says "nothing to do" and re-grids again is the
        // exact churn the owner's brief rules out; with no plan there is nothing
        // to show, and an announced bar (a pass that SAID it would install) keeps
        // its announcement until the plan lands.
        let planned = total > 0 || !f.programs.is_empty() || !f.queue.is_empty();
        if !planned {
            if !snap.running {
                // Ended having planned nothing: fold whatever was up, quietly.
                self.toolchain = None;
            }
            return;
        }
        if snap.running {
            let (detail, row_fill) = current_program_line(f);
            // A LIVE METER NEVER RUNS BACKWARDS within one pass. atpkg's `.part`
            // poller reports 0 for the instant between curl promoting the file
            // and the watch stopping, and a program's credit can dip for the
            // length of its verify phase (found by the 2026-08-26 progress-model
            // survey) — clamp to this pass's own high-water mark. A different
            // pass (identity: pass name + start stamp) starts fresh.
            let pass_id = Some((f.pass.clone(), f.started_unix));
            let peak = self
                .toolchain
                .as_ref()
                .filter(|b| !b.terminal() && b.pass_id == pass_id)
                .and_then(|b| b.fill);
            let fill = overall_fill(f)
                .or(row_fill)
                .map(|x| peak.map_or(x, |p| x.max(p)));
            let mut stats = String::new();
            if total > 0 {
                stats = format!("{done} of {total}");
            }
            if f.overall.bytes_total > 0 {
                if !stats.is_empty() {
                    stats.push_str(" · ");
                }
                stats.push_str(&format!(
                    "{} / {}",
                    fmt_bytes(f.overall.bytes_done.min(f.overall.bytes_total)),
                    fmt_bytes(f.overall.bytes_total)
                ));
            }
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: '\u{21e3}',
                    title: title.to_string(),
                    detail,
                    stats,
                    tone: Tone::Info,
                },
                fill,
                fold_at: None,
                stale_at: Some(now + TAILED_STALE),
                pass_id,
                staged_build: None,
                health: false,
            });
            return;
        }
        // NOT RUNNING: only a terminal outcome may be claimed (design §3).
        let failed = f
            .programs
            .values()
            .filter(|r| r.phase == Phase::Failed)
            .count();
        if f.ended_unix.is_some() {
            let (detail, tone, hold) = if failed == 0 {
                (
                    if total > 0 {
                        format!("all {total} installed")
                    } else {
                        "nothing to do".to_string()
                    },
                    Tone::Success,
                    HOLD_OK,
                )
            } else {
                (
                    format!(
                        "{} of {total} installed — {failed} failed · see Settings ▸ Packages",
                        done.saturating_sub(u32::try_from(failed).unwrap_or(u32::MAX))
                    ),
                    Tone::Warn,
                    HOLD_WARN,
                )
            };
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: if failed == 0 { '\u{2713}' } else { '\u{26a0}' },
                    title: title.to_string(),
                    detail,
                    stats: String::new(),
                    tone,
                },
                fill: (total > 0).then_some(1.0),
                fold_at: Some(now + hold),
                stale_at: None,
                pass_id: None,
                staged_build: None,
                health: false,
            });
        } else {
            // A live-looking file whose writer is gone (dead pid / stale
            // heartbeat): say so, name the next act, and never animate. The
            // next act is a command (or simply waiting: the 6-hourly pass
            // retries by itself) — never a reopen, which nothing here needs.
            self.toolchain = Some(Bar {
                text: BarText {
                    glyph: '\u{23f8}',
                    title: title.to_string(),
                    detail:
                        "stopped — run: aterm pkg update (the background pass retries on its own)"
                            .to_string(),
                    stats: if total > 0 {
                        format!("{done} of {total}")
                    } else {
                        String::new()
                    },
                    tone: Tone::Warn,
                },
                fill: overall_fill(f),
                fold_at: Some(now + HOLD_WARN),
                stale_at: None,
                pass_id: None,
                staged_build: None,
                health: false,
            });
        }
    }

    /// `seed-installed:` / `net-installed:` — the toolchain is here. `text` is
    /// the same sentence the pill used to carry (roster + the "open a new tab"
    /// clause, or the shell-integration caveat), authored by the caller.
    pub(crate) fn toolchain_installed(&mut self, text: &str, now: Instant) {
        // The sentence was authored for a pill with no title of its own; the
        // bar has one, so its opening is not repeated as the detail.
        let detail = text
            .strip_prefix("\u{2713} ALab toolchain installed: ")
            .unwrap_or(text);
        self.toolchain = Some(Bar {
            text: BarText {
                glyph: '\u{2713}',
                title: "ALab toolchain installed".to_string(),
                detail: sanitize_for_tty(detail, 160),
                stats: String::new(),
                tone: Tone::Success,
            },
            fill: Some(1.0),
            fold_at: Some(now + HOLD_OK),
            stale_at: None,
            pass_id: None,
            staged_build: None,
            health: false,
        });
    }

    /// `seed-done:` — THE POSITIVE TERMINAL: an announced pass that ended well and
    /// has no install roster to name. Distinct from [`Self::toolchain_installed`],
    /// which claims a roster; this one retires the announcement and claims nothing
    /// beyond the sentence atpkg itself printed.
    pub(crate) fn toolchain_ended(&mut self, detail: &str, now: Instant) {
        self.toolchain = Some(Bar {
            text: BarText {
                glyph: '\u{2713}',
                title: "ALab toolchain".to_string(),
                detail: sanitize_for_tty(detail, 160),
                stats: String::new(),
                tone: Tone::Success,
            },
            fill: Some(1.0),
            fold_at: Some(now + HOLD_OK),
            stale_at: None,
            pass_id: None,
            staged_build: None,
            health: false,
        });
    }

    /// A bad terminal outcome for the toolchain lane (`seed-partial:` /
    /// `seed-failed:` / `net-failed:` / `seed-unusable:` / the synthetic
    /// "child died after announcing"). `what` is the whole sentence.
    pub(crate) fn toolchain_failed(&mut self, what: &str, now: Instant) {
        // A FAILURE ROW NEVER INHERITS A FINISHED METER. Carrying the fill over is
        // honest while the bar being replaced is this pass's LIVE meter — the reader
        // sees how far it got before it broke. It was not honest when the bar being
        // replaced was itself a TERMINAL row, because a terminal row's meter is
        // always full: the owner's 2026-09-11 screenshot showed "⚠ ALab toolchain
        // install failed" beside a 100% bar, and the 100% came from a completed-pass
        // row the tailer had just built out of a 27-hour-old `progress.json` while
        // the verdict came from the live child. Two passes, one bar, and a reading
        // ("finished, and failed") that is a contradiction on its face even when the
        // failure is real.
        let fill = self
            .toolchain
            .as_ref()
            .filter(|b| !b.terminal())
            .and_then(|b| b.fill);
        self.toolchain = Some(Bar {
            text: BarText {
                glyph: '\u{26a0}',
                title: "ALab toolchain".to_string(),
                detail: sanitize_for_tty(what, 160),
                stats: String::new(),
                tone: Tone::Warn,
            },
            fill,
            fold_at: Some(now + HOLD_WARN),
            stale_at: None,
            pass_id: None,
            staged_build: None,
            health: false,
        });
    }

    /// Post a TERMINAL toolchain row that must be READ, not merely shown: behind
    /// a terminal row still inside its hold it queues ([`Self::settle_with`]
    /// promotes it when the lane frees); over a live row, or an empty lane, it
    /// is the lane's newest truth and goes up at once with `hold` from now.
    fn post_toolchain_terminal(&mut self, bar: Bar, hold: Duration, now: Instant) {
        self.post_toolchain_terminal_at(bar, hold, now, false);
    }

    /// [`Self::post_toolchain_terminal`], with the queue position chosen:
    /// `ahead_of_notices` puts a row that must be ACTED on (a machine-settings
    /// change with its undo) in front of the first queued Success notice (the
    /// managed-current row) — behind anything already queued ahead of that one,
    /// so several such rows keep their order — rather than 38 s behind the pass
    /// (UX review, 2026-09-10).
    fn post_toolchain_terminal_at(
        &mut self,
        mut bar: Bar,
        hold: Duration,
        now: Instant,
        ahead_of_notices: bool,
    ) {
        let held = self
            .toolchain
            .as_ref()
            .is_some_and(|live| live.terminal() && live.retires_at().is_some_and(|at| now < at));
        if held {
            let at = if ahead_of_notices {
                self.toolchain_queue
                    .iter()
                    .position(|(queued, _)| queued.text.tone == Tone::Success)
                    .unwrap_or(self.toolchain_queue.len())
            } else {
                self.toolchain_queue.len()
            };
            self.toolchain_queue.insert(at, (bar, hold));
            return;
        }
        bar.fold_at = Some(now + hold);
        bar.stale_at = None;
        self.toolchain = Some(bar);
    }

    /// `managed-current:` — every AGENT program (claude, codex) that is installed
    /// AND at the index pin, as atpkg lists it: `claude 2.1.267 (build 2026091001);
    /// codex 0.154.0 (build 2026091001)`. The row's TITLE names the programs the
    /// way a person knows them ("Claude Code 2.1.267 · Codex 0.154.0 —
    /// aterm-managed, current"); the detail says what the parsed NAMES run and
    /// carries the build ([`managed_current_words`]). Success, held
    /// [`HOLD_MANAGED`], queued behind a pass row so both are read (R6,
    /// 2026-09-10).
    ///
    /// ONCE PER TEXT PER LAUNCH: atpkg prints the marker on every pass, and the
    /// recurring pass would otherwise raise the row — and move the grid — every
    /// 6 h for nothing new. A repeat of the last text is recorded in the ledger
    /// and not posted; the first pass after launch posts, as the owner wants to
    /// see it at open.
    pub(crate) fn toolchain_managed_current(&mut self, text: &str, now: Instant) {
        let Some((title, detail)) = managed_current_words(text) else {
            return;
        };
        let key = text.trim();
        if self.last_managed_current.as_deref() == Some(key) {
            self.record(LedgerRow {
                lane: Lane::Toolchain,
                title,
                detail,
                outcome: Outcome::Ok,
                finished: now,
            });
            return;
        }
        self.last_managed_current = Some(key.to_string());
        self.post_toolchain_terminal(
            Bar {
                text: BarText {
                    glyph: '\u{2713}',
                    title,
                    detail,
                    stats: String::new(),
                    tone: Tone::Success,
                },
                fill: None,
                fold_at: None,
                stale_at: None,
                pass_id: None,
                staged_build: None,
                health: false,
            },
            HOLD_MANAGED,
            now,
        );
    }

    /// `machine-settings:` — the machine-level settings a pass CHANGED per
    /// doctor, as atpkg lists them: `spotlight-noindex 73 dir(s) migrated;
    /// universal-control disabled`. ONE ROW PER ITEM ([`machine_setting_words`]),
    /// Info, each held [`HOLD_WARN`]: the old single row put the Universal
    /// Control revert LAST on a right-truncating detail, invisible below 1328 px
    /// (UX review, 2026-09-10). The rows that carry an undo go first, and all of
    /// them go ahead of a queued managed-current notice, so the thing a person
    /// may want to reverse is the next thing they read after the pass row.
    /// The glyph is `↻` ("changed"), from the painter's documented set — `⚙`
    /// is outside it and rendered BLANK in the headless CPU font; `✓` stays the
    /// managed-current row's ("current").
    pub(crate) fn toolchain_machine_settings(&mut self, text: &str, now: Instant) {
        let (undo, plain): (Vec<BarText>, Vec<BarText>) = text
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(machine_setting_words)
            .partition(|words| !words.detail.is_empty());
        for words in undo.into_iter().chain(plain) {
            self.post_toolchain_terminal_at(
                Bar {
                    text: words,
                    fill: None,
                    fold_at: None,
                    stale_at: None,
                    pass_id: None,
                    staged_build: None,
                    health: false,
                },
                HOLD_WARN,
                now,
                true,
            );
        }
    }

    /// A plain text row on a lane, posted from OUTSIDE the process (`aterm ctl
    /// appnotice <lane> <text>`): the voice of an `aterm pkg install claude` run
    /// in a terminal, which has no GUI child to stream markers through. Info,
    /// held [`HOLD_NOTICE`]; the toolchain lane queues it like any terminal row,
    /// the update lane posts it as an outcome (the Staged row it may cover comes
    /// back when it folds).
    pub(crate) fn notice(&mut self, lane: Lane, text: &str, now: Instant) {
        let text = sanitize_for_tty(text, 120);
        match lane {
            Lane::Toolchain => self.post_toolchain_terminal(
                Bar {
                    text: BarText {
                        glyph: '\u{2139}',
                        title: text,
                        detail: String::new(),
                        stats: String::new(),
                        tone: Tone::Info,
                    },
                    fill: None,
                    fold_at: None,
                    stale_at: None,
                    pass_id: None,
                    staged_build: None,
                    health: false,
                },
                HOLD_NOTICE,
                now,
            ),
            Lane::Update => {
                self.update_outcome('\u{2139}', &text, "", Tone::Info, now);
                if let Some(bar) = self.update.as_mut() {
                    bar.fold_at = Some(now + HOLD_NOTICE);
                }
            }
        }
    }

    // ---- the update lane ----------------------------------------------------

    /// One report from inside the updater's own check ([`aterm_update::Progress`]).
    /// Only DOWNLOAD-and-later phases raise the bar: a check that finds nothing
    /// to do (the common case, every few minutes) must never move the grid.
    ///
    /// `posture` is how a STAGED build will be applied, as the App computed it
    /// (`App::apply_posture_for`) — read only by the `Staged` arm, whose detail
    /// line states it. `None` (a caller that computed none) states only the
    /// manual affordance.
    pub(crate) fn update_progress(
        &mut self,
        p: &aterm_update::Progress,
        posture: Option<ApplyPosture>,
        now: Instant,
    ) {
        use aterm_update::Progress as P;
        let v = |version: &str| sanitize_for_tty(version, 32);
        // A new report is the lane's newest truth: nothing older waits behind it.
        self.staged_behind_outcome = None;
        // …and a check that DOWNLOADS is a check that works: the standing health
        // warning, if it is up, is over — recorded, not silently dropped.
        if !matches!(p, P::Deferred { .. } | P::Failed { .. }) {
            self.update_health_healed(now);
        }
        match p {
            P::Downloading {
                version,
                bytes_done,
                bytes_total,
            } => {
                let (fill, stats) = if *bytes_total > 0 {
                    let done = (*bytes_done).min(*bytes_total);
                    (
                        Some(done as f32 / *bytes_total as f32),
                        format!("{} / {}", fmt_bytes(done), fmt_bytes(*bytes_total)),
                    )
                } else {
                    (None, fmt_bytes(*bytes_done))
                };
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{21bb}',
                        title: format!("aterm update v{}", v(version)),
                        detail: "downloading…".to_string(),
                        stats,
                        tone: Tone::Info,
                    },
                    fill,
                    fold_at: None,
                    stale_at: Some(now + UPDATE_STALE),
                    pass_id: None,
                    staged_build: None,
                    health: false,
                });
            }
            P::Verifying { version } => {
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{21bb}',
                        title: format!("aterm update v{}", v(version)),
                        detail: "verifying and staging…".to_string(),
                        stats: String::new(),
                        tone: Tone::Info,
                    },
                    fill: None,
                    fold_at: None,
                    stale_at: Some(now + UPDATE_STALE),
                    pass_id: None,
                    staged_build: None,
                    health: false,
                });
            }
            P::Staged { version, build } => {
                // HOW IT APPLIES — never "restart aterm to apply", which this line
                // said over the in-session handoff until 2026-08-30. The App
                // re-states the line once the lane has actually armed or stood
                // down for this build (`restate_apply_posture`), typically well
                // inside the hold.
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{2713}',
                        title: staged_title(version, posture),
                        detail: posture.map_or_else(
                            || staged_detail_unknown(*build),
                            |posture| staged_detail(*build, posture),
                        ),
                        stats: String::new(),
                        tone: Tone::Success,
                    },
                    // No meter: a full bar says nothing the ✓ does not, and
                    // its columns are the detail's at ordinary widths.
                    fill: None,
                    fold_at: Some(now + staged_hold(posture)),
                    stale_at: None,
                    pass_id: None,
                    staged_build: Some(*build),
                    health: false,
                });
            }
            P::Deferred { detail } => {
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{21bb}',
                        title: "aterm update deferred".to_string(),
                        detail: sanitize_for_tty(detail, 120),
                        stats: String::new(),
                        tone: Tone::Info,
                    },
                    fill: None,
                    fold_at: Some(now + HOLD_OK),
                    stale_at: None,
                    pass_id: None,
                    staged_build: None,
                    health: false,
                });
            }
            P::Failed { detail } => {
                self.update = Some(Bar {
                    text: BarText {
                        glyph: '\u{26a0}',
                        title: "aterm update failed".to_string(),
                        detail: format!(
                            "{} · see Settings ▸ Software Update",
                            sanitize_for_tty(detail, 100)
                        ),
                        stats: String::new(),
                        tone: Tone::Warn,
                    },
                    fill: None,
                    fold_at: Some(now + HOLD_WARN),
                    stale_at: None,
                    pass_id: None,
                    staged_build: None,
                    health: false,
                });
            }
        }
    }

    /// Re-state HOW a staged build applies on the live update bar — the
    /// refinement the App posts once the lane has actually armed (or stood down)
    /// for `build`, a moment after the `Staged` report painted the policy line.
    /// Rewrites only the words of a STAGED bar for exactly `build` (never the
    /// "Installing" row that replaces it). The hold is re-anchored BY POSTURE:
    /// a lane that will apply by itself extends it to the armed stretch (the
    /// `Staged` report's hold began at staging, and a lane that arms late in it
    /// must not fold the pull-down out from under the apply it promises — never
    /// shortened), and a lane standing down for good shortens it to the manual
    /// hold (a row that now says "until you apply it" does not keep the ten
    /// minutes it was armed with — never extended). Returns whether the words
    /// changed, so the caller can skip a repaint that would present nothing.
    pub(crate) fn restate_apply_posture(
        &mut self,
        build: u64,
        posture: ApplyPosture,
        now: Instant,
    ) -> bool {
        // The row on glass AND the one waiting behind an outcome
        // ([`Self::staged_behind_outcome`]): a lane that changes its mind while
        // an outcome covers the ready row must not hand back the old promise
        // when that outcome folds.
        let stash = self
            .staged_behind_outcome
            .as_mut()
            .is_some_and(|bar| restate_staged_bar(bar, build, posture, now));
        let live = self
            .update
            .as_mut()
            .is_some_and(|bar| restate_staged_bar(bar, build, posture, now));
        let _ = stash;
        // Only a change ON GLASS is a repaint.
        live
    }

    // -----------------------------------------------------------------------
    // THE UPDATE LANE'S OWN MOMENTS (2026-09-07): the bar is the ONE surface
    // the self-update speaks through — the floating cards are retired.
    // -----------------------------------------------------------------------

    /// APPLY BEGINS: the outgoing process is about to park its readers. Rewrite
    /// the LIVE update bar — if one is up — to say so. Never ADDS a row: the
    /// automatic lane's own re-check reads a re-grid's SIGWINCH as activity
    /// (the explicit lane adds one through [`Self::update_installing_added`]).
    /// The previous bar is returned so a refusal can put it back
    /// ([`Self::retire_installing`]).
    pub(crate) fn update_installing(&mut self, version: &str, now: Instant) -> Option<Bar> {
        let previous = self.update.clone()?;
        self.update = Some(installing_bar(version, now));
        Some(previous)
    }

    /// APPLY BEGINS on an EXPLICIT lane (the Version menu, Software Update, a
    /// clean quit) with NO row up: the row is ADDED — `true`, and the caller
    /// commits the re-grid at once, before the handoff carry is built and before
    /// the readers park, so the successor inherits the row it is told about.
    /// `false` with a row already up (then [`Self::update_installing`] is the
    /// call).
    pub(crate) fn update_installing_added(&mut self, version: &str, now: Instant) -> bool {
        if self.update.is_some() {
            return false;
        }
        self.update = Some(installing_bar(version, now));
        true
    }

    /// The handoff did not take over — a synchronous refusal before the park or
    /// an asynchronous one after it: the row goes back to what it was — the
    /// stashed words, or NO row where the explicit lane added one. A row that
    /// already moved on (an outcome posted over it) is left alone, so this is
    /// idempotent.
    pub(crate) fn retire_installing(&mut self, previous: Option<Bar>) {
        if self.update.as_ref().is_some_and(Bar::is_installing) {
            self.update = previous;
        }
    }

    /// THE SUCCESSOR, before Commit: it inherited a committed update row from the
    /// outgoing process (`WindowCarry::status_bar_rows`) and paints its own
    /// phase in it. Never a new row — the row is the carried one.
    pub(crate) fn update_finishing(&mut self, version: &str, now: Instant) {
        self.staged_behind_outcome = None;
        self.update_health_healed(now);
        self.update = Some(Bar {
            text: BarText {
                glyph: '\u{2191}',
                title: handoff_title("Finishing", version),
                detail: "keys you type now are queued and will arrive".to_string(),
                stats: String::new(),
                tone: Tone::Info,
            },
            fill: None,
            fold_at: None,
            stale_at: Some(now + HANDOFF_STALE),
            pass_id: None,
            staged_build: None,
            health: false,
        });
    }

    /// THE NEW BUILD TOOK OVER (or a cold-lane boot found it already running):
    /// the good news, held [`HOLD_OK`] and then folded into the ledger.
    pub(crate) fn update_landed(
        &mut self,
        version: &str,
        build: u64,
        kept_shells: bool,
        now: Instant,
    ) {
        self.staged_behind_outcome = None;
        self.update_health_healed(now);
        let v = sanitize_for_tty(version, 32);
        let title = if v.is_empty() {
            format!("Updated \u{2014} now on build {build}")
        } else {
            format!("Updated \u{2014} now on v{v}")
        };
        self.update = Some(Bar {
            text: BarText {
                glyph: '\u{2726}',
                title,
                // ONLY WHERE IT IS TRUE. The seamless lane carried the running
                // shells across and this is the whole promise; the cold lane
                // carried none, and saying it there is a plain falsehood next to
                // brand-new shells.
                detail: if kept_shells {
                    "your shells kept running".to_string()
                } else {
                    "the new build is running".to_string()
                },
                stats: String::new(),
                tone: Tone::Success,
            },
            fill: None,
            fold_at: Some(now + HOLD_OK),
            stale_at: None,
            pass_id: None,
            staged_build: None,
            health: false,
        });
    }

    /// One apply-lane OUTCOME the lane used to say on a floating card — waiting,
    /// delayed, paused, stopped, installed-and-activating, or a persistent
    /// failure. Info holds [`HOLD_OK`]; Warn holds [`HOLD_WARN`].
    pub(crate) fn update_outcome(
        &mut self,
        glyph: char,
        title: &str,
        detail: &str,
        tone: Tone,
        now: Instant,
    ) {
        let hold = match tone {
            Tone::Warn => HOLD_WARN,
            Tone::Info | Tone::Success => HOLD_OK,
        };
        // A Staged row the outcome covers comes back when the outcome folds
        // (`settle`): the stage is still there and the press still applies it.
        if let Some(staged) = self
            .update
            .as_ref()
            .filter(|bar| bar.staged_build.is_some() && bar.text.glyph == '\u{2713}')
        {
            self.staged_behind_outcome = Some(staged.clone());
        }
        // A STANDING health warning the outcome covers is not over — the ledger
        // has not healed — so it waits behind the outcome and returns when the
        // outcome folds ([`Self::health_behind_outcome`]).
        if let Some(health) = self.update.as_ref().filter(|bar| bar.health) {
            self.health_behind_outcome = Some(health.clone());
        }
        self.update = Some(Bar {
            text: BarText {
                glyph,
                title: sanitize_for_tty(title, 80),
                detail: shape_detail(detail, DETAIL_CAP),
                stats: String::new(),
                tone,
            },
            fill: None,
            fold_at: Some(now + hold),
            stale_at: None,
            pass_id: None,
            staged_build: None,
            health: false,
        });
    }

    /// THE STANDING CHECK-HEALTH WARNING (2026-09-10). The updater's ledger says
    /// checks are PERSISTENTLY failing (`failing_persistent`): the one state the
    /// pull-down — "the update-ready surface" — used to say nothing about for
    /// the life of the process beyond one 45-second outcome row. On m21 the
    /// pipeline had been `FAILING` since 2026-08-27 and the row was on glass for
    /// 45 s of a night. This row has NO hold and NO staleness cap: it stands
    /// until the ledger heals ([`Self::update_health_healed`]) or the process
    /// ends (a carried copy folds on the successor's handoff cap like any other).
    ///
    /// It yields to real activity on its lane: a live download, a staged build
    /// (a stage proves the check works), an installing/finishing row, an outcome
    /// — none are clobbered. The warning WAITS behind the row that holds the lane
    /// ([`Self::health_behind_outcome`], with the newest words) and takes the
    /// lane when that row retires, unless the ledger heals first. A standing row
    /// already up is REWRITTEN in place (a repaint, never a re-grid). Returns
    /// whether the glass changed.
    pub(crate) fn update_health_standing(&mut self, title: &str, detail: &str) -> bool {
        let text = BarText {
            glyph: '\u{26a0}',
            title: sanitize_for_tty(title, 80),
            detail: shape_detail(detail, DETAIL_CAP),
            stats: String::new(),
            tone: Tone::Warn,
        };
        let standing = |text| Bar {
            text,
            fill: None,
            fold_at: None,
            stale_at: None,
            pass_id: None,
            staged_build: None,
            health: true,
        };
        match self.update.as_mut() {
            Some(bar) if bar.health => {
                if bar.text == text {
                    return false;
                }
                bar.text = text;
                true
            }
            Some(_) => {
                self.health_behind_outcome = Some(standing(text));
                false
            }
            None => {
                self.health_behind_outcome = None;
                self.update = Some(standing(text));
                true
            }
        }
    }

    /// The ledger healed (a check completed end to end): the standing warning
    /// leaves the glass and its last words go to the ledger, exactly as a folded
    /// row's do — `appstatus` still answers for the stretch it stood. `true` when
    /// a row left.
    pub(crate) fn update_health_healed(&mut self, now: Instant) -> bool {
        let on_glass = self.update.as_ref().is_some_and(|bar| bar.health);
        let retired = if on_glass {
            self.update.take()
        } else {
            // Waiting behind an outcome ([`Self::health_behind_outcome`]): it
            // was on glass before the outcome covered it, so it is on record too.
            self.health_behind_outcome.take()
        };
        let Some(bar) = retired else {
            return false;
        };
        if self.ledger.len() == LEDGER_ROWS {
            self.ledger.pop_front();
        }
        self.ledger.push_back(LedgerRow {
            lane: Lane::Update,
            title: bar.text.title,
            detail: bar.text.detail,
            outcome: Outcome::Warn,
            finished: now,
        });
        on_glass
    }

    /// Whether the live update bar is the standing check-health warning.
    #[cfg(test)]
    pub(crate) fn update_bar_is_health(&self) -> bool {
        self.update.as_ref().is_some_and(|bar| bar.health)
    }

    /// Drop the Staged row waiting behind an outcome ([`Self::update_outcome`]):
    /// for an outcome that means the stage is no longer merely waiting — it is
    /// installed and activating — handing back "is ready · click to apply now"
    /// when the outcome folds would offer a build that is already on its way in.
    pub(crate) fn forget_staged_behind_outcome(&mut self) {
        self.staged_behind_outcome = None;
    }

    /// Whether the live update bar is a STAGED one — the state in which a press
    /// on it applies the build rather than opening the details page.
    pub(crate) fn update_bar_is_staged(&self) -> bool {
        self.update
            .as_ref()
            .is_some_and(|bar| bar.staged_build.is_some() && bar.text.glyph == '\u{2713}')
    }

    /// The bars as plain data for the handoff carry, top to bottom.
    pub(crate) fn carried(&self) -> Vec<crate::session_store::CarriedBar> {
        self.bars()
            .map(|(lane, bar)| crate::session_store::CarriedBar {
                lane: lane.as_str().to_string(),
                glyph: bar.text.glyph,
                title: bar.text.title.clone(),
                detail: bar.text.detail.clone(),
                stats: bar.text.stats.clone(),
                tone: match bar.text.tone {
                    Tone::Info => "info",
                    Tone::Success => "success",
                    Tone::Warn => "warn",
                }
                .to_string(),
                fill_permille: bar
                    .fill
                    .map(|f| (f.clamp(0.0, 1.0) * 1000.0).round() as u16),
            })
            .collect()
    }

    /// COMMIT on the successor: a carried TOOLCHAIN bar describes a pass this
    /// process never drove and will not hear from again — its words are the
    /// outgoing process's last, so it folds into the ledger after the ordinary
    /// hold instead of sitting live until the staleness cap. (The update lane's
    /// carried row is rewritten by the landing, not folded.) A toolchain bar
    /// this process has since written itself no longer says the carried words
    /// and is left alone.
    pub(crate) fn after_handoff_commit(&mut self, now: Instant) {
        if let Some(carried) = self.carried_toolchain.take()
            && let Some(bar) = self.toolchain.as_mut()
            && bar.text == carried
        {
            // Its last words, held long enough to read (by tone, like any
            // outcome), and NO meter: a feed this process cannot hear must not
            // be completed to 100 % by the terminal-outcome guard in
            // `toolchain_snapshot`.
            bar.fill = None;
            bar.fold_at = Some(
                now + match bar.text.tone {
                    Tone::Warn => HOLD_WARN,
                    Tone::Info | Tone::Success => HOLD_OK,
                },
            );
            bar.stale_at = None;
        }
    }

    /// Re-seed the bars a handoff carried (the successor's first frames, before
    /// Commit): each becomes a LIVE bar under the handoff's staleness cap, so a
    /// lane that never reports again folds on its own. The update lane's carried
    /// words are the outgoing process's "installing"; the successor says
    /// "finishing" instead ([`Self::update_finishing`]) right after this.
    /// Unknown lanes and tones are dropped rather than guessed.
    pub(crate) fn seed_carried(&mut self, bars: &[crate::session_store::CarriedBar], now: Instant) {
        for carried in bars {
            let tone = match carried.tone.as_str() {
                "info" => Tone::Info,
                "success" => Tone::Success,
                "warn" => Tone::Warn,
                _ => continue,
            };
            let bar = Bar {
                text: BarText {
                    glyph: carried.glyph,
                    title: sanitize_for_tty(&carried.title, 80),
                    detail: sanitize_for_tty(&carried.detail, 160),
                    stats: sanitize_for_tty(&carried.stats, 40),
                    tone,
                },
                fill: carried
                    .fill_permille
                    .map(|p| f32::from(p.min(1000)) / 1000.0),
                fold_at: None,
                stale_at: Some(now + HANDOFF_STALE),
                pass_id: None,
                staged_build: None,
                health: false,
            };
            match carried.lane.as_str() {
                "toolchain" => {
                    self.carried_toolchain = Some(bar.text.clone());
                    self.toolchain = Some(bar);
                }
                "update" => self.update = Some(bar),
                _ => {}
            }
        }
    }
}

/// The overall meter: the pass's download rollup, `None` when the pass planned
/// no bytes (a seed pass before its plan lands, or nothing to do).
fn overall_fill(f: &atpkg::progress::ProgressFile) -> Option<f32> {
    (f.overall.bytes_total > 0).then(|| {
        (f.overall.bytes_done.min(f.overall.bytes_total)) as f32 / f.overall.bytes_total as f32
    })
}

/// The program the pass is working on right now, as one honest phrase, plus
/// that program's own byte meter when its phase has one — the fallback fill for
/// a pass whose overall rollup is silent (the sealed-seed pass moves no download
/// bytes, so its rollup jumps per program; the extract meter is the live truth).
fn current_program_line(f: &atpkg::progress::ProgressFile) -> (String, Option<f32>) {
    // Mid-flight phases first, in pass order; a queued front-of-queue program
    // only when nothing is mid-flight.
    let active = f
        .programs
        .iter()
        .filter(|(_, r)| {
            matches!(
                r.phase,
                Phase::Download | Phase::Verify | Phase::Extract | Phase::Link
            )
        })
        .min_by_key(|(_, r)| r.phase as u8);
    let Some((raw, row)) =
        active.or_else(|| f.queue.first().and_then(|n| f.programs.get_key_value(n)))
    else {
        return (String::new(), None);
    };
    let Some(name) = admitted_name(raw) else {
        return (String::new(), None);
    };
    let metered = row.bytes_total > 0;
    let frac =
        metered.then(|| (row.bytes_done.min(row.bytes_total)) as f32 / row.bytes_total as f32);
    let line = match row.phase {
        Phase::Queued => format!("{name} — queued"),
        Phase::Download => {
            if metered {
                format!(
                    "{name} — downloading {} / {}",
                    fmt_bytes(row.bytes_done.min(row.bytes_total)),
                    fmt_bytes(row.bytes_total)
                )
            } else {
                format!("{name} — downloading")
            }
        }
        // Label-only phases: not byte streams atpkg can meter, and the bar does
        // not pretend otherwise.
        Phase::Verify => format!("{name} — verifying"),
        Phase::Extract => {
            if metered {
                format!(
                    "{name} — extracting {} / {}",
                    fmt_bytes(row.bytes_done.min(row.bytes_total)),
                    fmt_bytes(row.bytes_total)
                )
            } else {
                format!("{name} — extracting")
            }
        }
        Phase::Link => format!("{name} — linking"),
        Phase::Done => format!("{name} — installed"),
        Phase::Failed => match row.error.as_deref() {
            Some(e) => format!("{name} — failed: {}", sanitize_for_tty(e, ERROR_CAP)),
            None => format!("{name} — failed"),
        },
        Phase::Skipped => format!("{name} — already current"),
    };
    // "you asked" outranks "you wait"; a row pulled forward because a program the user
    // asked for REQUIRES it (§17.10) says so instead — it was not asked for, it was needed.
    let line = if row.bumped {
        match row.bumped_with.as_deref() {
            Some(with) => format!("{line} (bumped with {with})"),
            None => format!("{line} (you asked for this)"),
        }
    } else {
        line
    };
    (
        line,
        if matches!(row.phase, Phase::Download | Phase::Extract) {
            frac
        } else {
            None
        },
    )
}

/// A program name is UNTRUSTED until it round-trips the store's name gate; one
/// that fails simply has no words on the bar.
fn admitted_name(raw: &str) -> Option<String> {
    let name = atpkg::store::ToolName::new(raw)?;
    Some(sanitize_for_tty(name.as_str(), NAME_CAP))
}

/// Human byte figure, decimal units like every download dialog: `812 KB`,
/// `512 MB`, `1.2 GB`.
pub(crate) fn fmt_bytes(n: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    let f = n as f64;
    if f >= GB {
        format!("{:.1} GB", f / GB)
    } else if f >= MB {
        format!("{:.0} MB", f / MB)
    } else if f >= KB {
        format!("{:.0} KB", f / KB)
    } else {
        format!("{n} B")
    }
}

// ---------------------------------------------------------------------------
// The width law.
// ---------------------------------------------------------------------------

/// Where each piece of a bar lands at `cols` — the pure half of [`paint_rows`].
/// Columns are absolute; a `None` piece was dropped for want of room.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Layout {
    pub glyph_col: usize,
    pub title_col: usize,
    /// The title as drawn (possibly truncated with `…`).
    pub title: String,
    /// `(col, text)` — the detail as drawn, or `None` when dropped.
    pub detail: Option<(usize, String)>,
    /// `(col, width)` of the meter, or `None` (no fill, or no room).
    pub meter: Option<(usize, usize)>,
    /// `(col, text)` of the percentage, beside the meter.
    pub pct: Option<(usize, String)>,
    /// `(col, text)` of the right-aligned stats, or `None` when dropped.
    pub stats: Option<(usize, String)>,
}

/// Lay one bar out at `cols`. Priority when the row is too narrow, in the order
/// pieces are DROPPED: the stats (the meter already says what they say), then
/// the detail truncates and goes, then the meter shrinks to [`METER_MIN`], then
/// the meter goes, then the title truncates. The glyph and (some of) the title
/// always survive. The detail outranks the stats: "trust — extracting 120 MB /
/// 900 MB" is the sentence a user reads; "3 of 10 · 512 MB / 1.2 GB" is a figure
/// the meter repeats.
pub(crate) fn layout(text: &BarText, fill: Option<f32>, cols: usize) -> Layout {
    let width = |s: &str| s.chars().count();
    let budget = cols.saturating_sub(2 * MARGIN);
    let mut out = Layout {
        glyph_col: MARGIN,
        title_col: MARGIN + 2,
        ..Layout::default()
    };
    // "<glyph> <title>" — the head.
    let head_fixed = 2; // glyph + space
    let mut title = text.title.clone();
    if head_fixed + width(&title) > budget {
        title = truncate(&title, budget.saturating_sub(head_fixed));
    }
    let head = head_fixed + width(&title);
    out.title = title;
    let mut used = head;
    // The pieces that want to live on the right: meter + pct, then stats.
    let pct_text = fill.map(|f| format!("{:>3}%", (f.clamp(0.0, 1.0) * 100.0).floor() as u32));
    let pct_w = pct_text.as_ref().map_or(0, |p| 1 + width(p)); // " NN%"
    let meter_w_min = fill.map_or(0, |_| METER_MIN);
    let detail_w = if text.detail.is_empty() {
        0
    } else {
        2 + width(&text.detail)
    };
    let stats_w = if text.stats.is_empty() {
        0
    } else {
        2 + width(&text.stats)
    };

    // 1. meter at minimum width (+ pct) if it fits.
    let mut meter_w = 0;
    let mut have_pct = false;
    if fill.is_some() && used + 2 + meter_w_min + pct_w <= budget {
        meter_w = meter_w_min;
        have_pct = true;
        used += 2 + meter_w + pct_w;
    }
    // 2. the detail in full and the stats, if both fit; else the detail in full
    //    alone; else the detail truncated to what is left (the stats go first).
    let mut detail: Option<String> = None;
    let mut have_stats = false;
    if detail_w > 0 && used + detail_w + stats_w <= budget {
        detail = Some(text.detail.clone());
        have_stats = stats_w > 0;
        used += detail_w + stats_w;
    } else if detail_w > 0 && used + detail_w <= budget {
        detail = Some(text.detail.clone());
        used += detail_w;
    } else if detail_w > 0 {
        let room = budget.saturating_sub(used);
        if room >= 2 + DETAIL_FLOOR {
            // A detail with a backtick command keeps the command and cuts the
            // prose ([`shape_detail`]); its cap counts the cells BEFORE the
            // ellipsis, hence the extra one. A plain detail truncates.
            let d = if text.detail.contains('`') {
                shape_detail(&text.detail, room - 3)
            } else {
                truncate(&text.detail, room - 2)
            };
            used += 2 + width(&d);
            detail = Some(d);
        }
    } else if stats_w > 0 && used + stats_w <= budget {
        have_stats = true;
        used += stats_w;
    }
    // 4. grow the meter toward its preferred width with what remains.
    if meter_w > 0 {
        let spare = budget.saturating_sub(used);
        let grow = (METER_PREFERRED - meter_w).min(spare);
        meter_w += grow;
        used += grow;
    }
    // ---- place: head, detail left-to-right; stats, pct, meter right-to-left.
    let mut col = MARGIN + head;
    if let Some(d) = detail {
        col += 2;
        out.detail = Some((col, d));
    }
    let _ = col;
    let mut right = MARGIN + budget; // one past the last usable column
    if have_stats {
        right -= width(&text.stats);
        out.stats = Some((right, text.stats.clone()));
        right -= 2;
    }
    if have_pct && let Some(p) = pct_text {
        right -= width(&p);
        out.pct = Some((right, p));
        right -= 1;
    }
    if meter_w > 0 {
        right -= meter_w;
        out.meter = Some((right, meter_w));
    }
    let _ = used;
    out
}

/// The managed-current row's words from the wire text, or `None` for an empty
/// body. The title names the programs as a person knows them ("Claude Code
/// 2.1.267 · Codex 0.154.0 — aterm-managed, current"); the detail says what
/// the parsed NAMES run — "what `claude` and `codex` run in new tabs", one name
/// "what `claude` runs in a new tab" — built from the wire and never hardcoded,
/// so `gemini` reads as gemini. The builds ride the end: one distinct build
/// "· build 2026091001", differing ones "· builds 2026091001 / 2026091002",
/// nothing when the wire carried none.
fn managed_current_words(text: &str) -> Option<(String, String)> {
    let mut names: Vec<String> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    let mut builds: Vec<String> = Vec::new();
    for item in text.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        // `<name> <version> (build <N>)` — the version and the build are each
        // optional on the wire, so a bare name still renders.
        let (head, build) = match item.split_once('(') {
            Some((head, tail)) => (head.trim(), tail.trim_end_matches(')').trim()),
            None => (item, ""),
        };
        let mut words = head.split_whitespace();
        let name = words.next().unwrap_or_default();
        let version = words.next().unwrap_or_default();
        let display = match name {
            "claude" => "Claude Code",
            "codex" => "Codex",
            other => other,
        };
        names.push(if version.is_empty() {
            sanitize_for_tty(display, NAME_CAP)
        } else {
            sanitize_for_tty(&format!("{display} {version}"), NAME_CAP + 24)
        });
        // The command a tab runs is the wire name itself; a backtick in it would
        // break the quoting the detail relies on.
        commands.push(sanitize_for_tty(&name.replace('`', ""), NAME_CAP));
        let build = build.strip_prefix("build").map_or(build, str::trim);
        if !build.is_empty() {
            let build = sanitize_for_tty(build, 32);
            if !builds.contains(&build) {
                builds.push(build);
            }
        }
    }
    if names.is_empty() {
        return None;
    }
    let title = sanitize_for_tty(
        &format!(
            "{} \u{2014} aterm-managed, current",
            names.join(" \u{00b7} ")
        ),
        120,
    );
    let quoted: Vec<String> = commands.iter().map(|c| format!("`{c}`")).collect();
    let mut detail = match quoted.as_slice() {
        [one] => format!("what {one} runs in a new tab"),
        [head @ .., last] => format!("what {} and {last} run in new tabs", head.join(", ")),
        [] => unreachable!("names and commands are pushed together"),
    };
    match builds.as_slice() {
        [] => {}
        [one] => {
            detail.push_str(" \u{00b7} build ");
            detail.push_str(one);
        }
        many => {
            detail.push_str(" \u{00b7} builds ");
            detail.push_str(&many.join(" / "));
        }
    }
    Some((title, shape_detail(&detail, DETAIL_CAP)))
}

/// One machine-settings wire item as its own row's words. The two items atpkg
/// emits today read as a person would say them: `universal-control disabled`
/// → **"Universal Control disabled"** with the undo in the detail — the
/// pointer first (`aterm pkg doctor` prints the revert), then the revert
/// command itself, byte-identical to [`atpkg::machine::UNIVERSAL_CONTROL_REVERT`],
/// so a narrow row drops the command last of all; `spotlight-noindex N dir(s)
/// migrated` → **"Spotlight: N build dirs moved to .noindex"**, no detail. An
/// item this build does not know is its own title, verbatim. Info, `↻`.
fn machine_setting_words(item: &str) -> BarText {
    let item = sanitize_for_tty(item, 80);
    let (title, detail) = if item == atpkg::machine::UNIVERSAL_CONTROL_ENTRY {
        (
            "Universal Control disabled".to_string(),
            format!(
                "undo: `aterm pkg doctor` prints the revert \u{00b7} {}",
                atpkg::machine::UNIVERSAL_CONTROL_REVERT
            ),
        )
    } else if let Some(count) = spotlight_noindex_count(&item) {
        (
            format!(
                "Spotlight: {count} build {} moved to .noindex",
                if count == 1 { "dir" } else { "dirs" }
            ),
            String::new(),
        )
    } else {
        (item.clone(), String::new())
    };
    BarText {
        glyph: '\u{21bb}',
        title: sanitize_for_tty(&title, 120),
        detail: shape_detail(&detail, DETAIL_CAP),
        stats: String::new(),
        tone: Tone::Info,
    }
}

/// The N of `spotlight-noindex N dir(s) migrated`, when the item is that.
fn spotlight_noindex_count(item: &str) -> Option<u64> {
    item.strip_prefix("spotlight-noindex ")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// `s` cut to at most `max` cells, ending in `…` when anything was cut.
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut t: String = s.chars().take(max - 1).collect();
    t.push('\u{2026}');
    t
}

// ---------------------------------------------------------------------------
// The painter.
// ---------------------------------------------------------------------------

/// Paint every live bar as one `cols`-wide row each, top to bottom, on the
/// chrome band's material ([`chrome_band::band_colors`] — the same tone the
/// find bar and the config notices already use, so the window's in-grid chrome
/// reads as one surface). The LAST bar row carries the hairline that closes the
/// chrome against the terminal content beneath it.
/// One EMPTY band row, in the same colours a painted bar uses. The compose pads
/// with this when the geometry has committed more rows than the cache holds, so
/// a reserved row is never a hole the terminal's own top row shows through.
pub(crate) fn blank_band_row(cols: usize, theme: Theme) -> Vec<RenderCell> {
    let c = chrome_band::band_colors(theme);
    blank_row(cols, c.label, c.bar_bg, false)
}

pub(crate) fn paint_rows(bars: &StatusBars, cols: usize, theme: Theme) -> Vec<Vec<RenderCell>> {
    let c = chrome_band::band_colors(theme);
    let n = bars.rows() as usize;
    let mut rows: Vec<Vec<RenderCell>> = Vec::with_capacity(n);
    for (i, (_, bar)) in bars.bars().enumerate() {
        let mut row = blank_row(cols, c.label, c.bar_bg, false);
        paint_bar(&mut row, cols, bar, &c);
        if i + 1 == n {
            // The content-facing edge: one unbroken rule under the last bar,
            // text and all — the find bar's seam, the strip's `seal_strip_bottom`.
            for cell in &mut row {
                cell.underline = UnderlineStyle::Single;
                cell.underline_color = Some(c.label);
            }
        }
        rows.push(row);
    }
    rows
}

fn paint_bar(row: &mut [RenderCell], cols: usize, bar: &Bar, c: &BandColors) {
    let l = layout(&bar.text, bar.fill, cols);
    let ink = match bar.text.tone {
        Tone::Info => c.value,
        Tone::Success => c.value,
        Tone::Warn => c.warn,
    };
    let accent = match bar.text.tone {
        Tone::Warn => c.warn,
        _ => c.accent,
    };
    // Glyph: text presentation on purpose — ⚠ and ✓ have emoji forms, and a
    // colour emoji in a chrome row would be two cells wide in one.
    if l.glyph_col < cols {
        let mut g = chrome_band::cell(bar.text.glyph, accent, c.bar_bg, true, false);
        g.text_presentation = true;
        row[l.glyph_col] = g;
    }
    write_str(row, cols, l.title_col, &l.title, ink, c.bar_bg, true);
    if let Some((col, d)) = &l.detail {
        write_str(row, cols, *col, d, c.label, c.bar_bg, false);
    }
    if let Some((col, w)) = l.meter
        && let Some(f) = bar.fill
    {
        // Half-cell resolution: `2w` steps, a full cell is two, a half cell is
        // the left-half block in the accent over the track.
        let steps = (f.clamp(0.0, 1.0) * (2 * w) as f32).round() as usize;
        for i in 0..w {
            let x = col + i;
            if x >= cols {
                break;
            }
            let cell_steps = steps.saturating_sub(2 * i).min(2);
            row[x] = match cell_steps {
                2 => chrome_band::cell(' ', c.meter_track, accent, false, false),
                1 => chrome_band::cell('\u{258c}', accent, c.meter_track, false, false),
                _ => chrome_band::cell(' ', c.meter_track, c.meter_track, false, false),
            };
        }
    }
    if let Some((col, p)) = &l.pct {
        write_str(row, cols, *col, p, ink, c.bar_bg, false);
    }
    if let Some((col, s)) = &l.stats {
        write_str(row, cols, *col, s, c.label, c.bar_bg, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn t0() -> Instant {
        Instant::now()
    }

    /// THE OWNER'S SCREENSHOT, 2026-09-11: "⚠ ALab toolchain install failed — see
    /// Settings ▸ Packages" beside a FULL progress bar.
    ///
    /// The two come from different sources and the bar fused them. The verdict is the
    /// live child's; the meter was inherited from whatever bar the failure replaced —
    /// and on that launch the bar it replaced was a COMPLETED-pass row the tailer had
    /// just built out of a 27-hour-old `progress.json`, because the child was refused
    /// at atpkg's dispatch edge and never wrote a pass of its own. "Finished, and
    /// failed" is a contradiction on its face even when the failure is real.
    #[test]
    fn a_failure_row_does_not_inherit_a_finished_pass_meter() {
        let mut bars = StatusBars::default();
        let now = t0();
        // A terminal SUCCESS row: full meter, by construction.
        bars.toolchain_ended("the pass finished; 12 ALab program(s) are installed", now);
        assert_eq!(
            bars.toolchain.as_ref().and_then(|b| b.fill),
            Some(1.0),
            "the success row is the one that carries a full meter"
        );
        // …and then a real failure lands on the same lane.
        bars.toolchain_failed("install failed — see Settings ▸ Packages", now);
        assert_eq!(
            bars.toolchain.as_ref().and_then(|b| b.fill),
            None,
            "a failure must not be painted at 100% — the meter belonged to another pass"
        );
    }

    /// …while the inheritance that EARNED its place survives: a failure arriving over
    /// this pass's own LIVE meter keeps it, so the reader sees how far the pass got
    /// before it broke.
    #[test]
    fn a_failure_row_keeps_the_live_meter_of_the_pass_it_reports() {
        let mut bars = StatusBars::default();
        let now = t0();
        let snap = crate::PkgProgressSnapshot {
            file: file(Some(4242), "net"),
            running: true,
        };
        bars.toolchain_snapshot(Some(&snap), now);
        let live = bars.toolchain.as_ref().and_then(|b| b.fill);
        assert!(
            live.is_some_and(|f| f > 0.0 && f < 1.0),
            "a live partial meter: {live:?}"
        );
        bars.toolchain_failed("install failed — see Settings ▸ Packages", now);
        assert_eq!(
            bars.toolchain.as_ref().and_then(|b| b.fill),
            live,
            "the pass's own progress is the one honest thing to keep"
        );
    }

    fn file(running_pid: Option<u32>, pass: &str) -> atpkg::progress::ProgressFile {
        atpkg::progress::ProgressFile {
            v: PROGRESS_VERSION,
            pid: running_pid,
            pass: pass.to_string(),
            started_unix: 1_700_000_000,
            heartbeat_unix: 1_700_000_000,
            overall: atpkg::progress::Overall {
                programs_done: 3,
                programs_total: 10,
                bytes_done: 512_000_000,
                bytes_total: 1_200_000_000,
            },
            queue: vec!["ty".into(), "ay".into()],
            programs: BTreeMap::from([
                (
                    "trust".to_string(),
                    atpkg::progress::ProgramProgress {
                        phase: Phase::Extract,
                        bytes_done: 120_000_000,
                        bytes_total: 900_000_000,
                        build: Some(5520),
                        bumped: false,
                        bumped_with: None,
                        error: None,
                    },
                ),
                (
                    "ty".to_string(),
                    atpkg::progress::ProgramProgress {
                        phase: Phase::Queued,
                        bytes_done: 0,
                        bytes_total: 0,
                        build: None,
                        bumped: false,
                        bumped_with: None,
                        error: None,
                    },
                ),
            ]),
            ended_unix: None,
        }
    }

    fn snap(running: bool) -> crate::PkgProgressSnapshot {
        crate::PkgProgressSnapshot {
            file: file(Some(7), "net"),
            running,
        }
    }

    fn text_of(row: &[RenderCell]) -> String {
        row.iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn hidden_bars_are_the_zero_key_and_no_deadline() {
        let bars = StatusBars::default();
        assert_eq!(bars.rows(), 0);
        assert_eq!(bars.fingerprint(), 0);
        assert_eq!(bars.deadline(), None);
        assert!(paint_rows(&bars, 80, Theme::default()).is_empty());
    }

    #[test]
    fn an_announcement_opens_the_toolchain_bar_before_any_snapshot() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_announced(
            "installing 10 ALab program(s) from the bundled registry (about 3 GB on disk when finished)",
            now,
        );
        assert_eq!(bars.rows(), 1);
        let (lane, bar) = bars.bars().next().unwrap();
        assert_eq!(lane, Lane::Toolchain);
        assert_eq!(bar.text.title, "Installing the ALab toolchain");
        assert!(
            bar.text.detail.contains("about 3 GB on disk when finished"),
            "{}",
            bar.text.detail
        );
        assert_eq!(bar.fill, None, "no meter before the file exists");
        assert_eq!(bar.fold_at, None, "live: no fold");
        assert_eq!(bar.stale_at, Some(now + ANNOUNCE_STALE), "…but a cap");
        assert_ne!(bars.fingerprint(), 0);
        // A pass that plans nothing never opens a bar of its own, and closes an
        // announced one when it ends empty — the every-6-hours no-op stays invisible.
        let mut quiet = StatusBars::default();
        let mut f = file(Some(7), "net");
        f.overall = atpkg::progress::Overall::default();
        f.queue.clear();
        f.programs.clear();
        quiet.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: true,
            }),
            now,
        );
        assert_eq!(quiet.rows(), 0, "unplanned running pass: no bar");
        f.pid = None;
        f.ended_unix = Some(1);
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: false,
            }),
            now,
        );
        assert_eq!(bars.rows(), 0, "announced, then ended empty: folded");
    }

    #[test]
    fn a_live_meter_never_runs_backwards_within_a_pass() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        let high = bars.bars().next().unwrap().1.fill.unwrap();
        let mut dip = file(Some(7), "net");
        dip.overall.bytes_done = 10;
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: dip.clone(),
                running: true,
            }),
            now,
        );
        assert_eq!(
            bars.bars().next().unwrap().1.fill,
            Some(high),
            "the dip is clamped"
        );
        // A NEW pass identity starts from its own truth.
        dip.started_unix += 1;
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: dip,
                running: true,
            }),
            now,
        );
        assert!(bars.bars().next().unwrap().1.fill.unwrap() < high);
    }

    #[test]
    fn a_live_bar_whose_feed_went_silent_folds_at_its_cap() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        assert_eq!(bars.deadline(), Some(now + TAILED_STALE));
        assert!(!bars.settle(now + TAILED_STALE / 2));
        assert!(bars.settle(now + TAILED_STALE), "no report for 30 s ⇒ gone");
        assert_eq!(bars.rows(), 0);
        // A fresh report re-arms the cap.
        bars.toolchain_snapshot(Some(&snap(true)), now);
        bars.toolchain_snapshot(Some(&snap(true)), now + Duration::from_secs(20));
        assert!(!bars.settle(now + TAILED_STALE));
        assert_eq!(
            bars.deadline(),
            Some(now + Duration::from_secs(20) + TAILED_STALE)
        );
    }

    #[test]
    fn a_running_snapshot_reports_the_current_program_and_the_rollup() {
        let mut bars = StatusBars::default();
        bars.toolchain_snapshot(Some(&snap(true)), t0());
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Installing the ALab toolchain");
        assert_eq!(bar.text.detail, "trust — extracting 120 MB / 900 MB");
        assert_eq!(bar.text.stats, "3 of 10 · 512 MB / 1.2 GB");
        let f = bar.fill.unwrap();
        assert!((f - 512.0 / 1200.0).abs() < 1e-3, "{f}");
        assert_eq!(bar.fold_at, None);
    }

    #[test]
    fn the_seed_pass_has_its_own_title_and_falls_back_to_the_extract_meter() {
        let mut bars = StatusBars::default();
        let mut f = file(Some(7), "seed");
        f.overall.bytes_total = 0;
        f.overall.bytes_done = 0;
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: true,
            }),
            t0(),
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Preparing the ALab toolchain");
        let fill = bar
            .fill
            .expect("the extract meter stands in for a silent rollup");
        assert!((fill - 120.0 / 900.0).abs() < 1e-3);
        assert_eq!(bar.text.stats, "3 of 10");
    }

    #[test]
    fn not_running_claims_only_terminal_states() {
        // Ended cleanly: success, folds after HOLD_OK.
        let mut bars = StatusBars::default();
        let mut f = file(None, "net");
        f.ended_unix = Some(1_700_000_100);
        let now = t0();
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.tone, Tone::Success);
        assert_eq!(bar.text.detail, "all 10 installed");
        assert_eq!(bar.fold_at, Some(now + HOLD_OK));
        assert_eq!(bar.fill, Some(1.0));
        // Ended with a failure: warn, the longer hold.
        f.programs.get_mut("trust").unwrap().phase = Phase::Failed;
        let mut bars = StatusBars::default();
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f.clone(),
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.tone, Tone::Warn);
        assert!(bar.text.detail.contains("1 failed"), "{}", bar.text.detail);
        assert_eq!(bar.fold_at, Some(now + HOLD_WARN));
        // Dead writer, no clean end: "stopped", names the next act, never a live phase.
        f.ended_unix = None;
        let mut bars = StatusBars::default();
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert!(
            bar.text.detail.starts_with("stopped — "),
            "{}",
            bar.text.detail
        );
        assert!(!bar.text.detail.contains("extracting"));
        // The next act is the command (or nothing: the background pass retries by
        // itself) — a stalled toolchain pass never asks for a reopen.
        assert!(
            bar.text.detail.contains("aterm pkg update"),
            "{}",
            bar.text.detail
        );
        let lower = bar.text.detail.to_lowercase();
        assert!(
            !lower.contains("reopen") && !lower.contains("restart") && !lower.contains("relaunch"),
            "{}",
            bar.text.detail
        );
        assert_eq!(bar.fold_at, Some(now + HOLD_WARN));
    }

    #[test]
    fn a_marker_outcome_outranks_the_final_snapshot_and_none_never_erases_it() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        bars.toolchain_installed(
            "✓ ALab toolchain installed: ay, trust — open a new tab to use them",
            now,
        );
        // The tailer's final read lands AFTER the marker: it may complete the
        // meter, never replace the words.
        let mut f = file(None, "net");
        f.ended_unix = Some(1);
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: false,
            }),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "ALab toolchain installed");
        assert!(bar.text.detail.contains("open a new tab"));
        assert_eq!(bar.fill, Some(1.0));
        // …and the vanished-file clear keeps the last words too.
        bars.toolchain_snapshot(None, now);
        assert_eq!(bars.rows(), 1);
        // Whereas a LIVE bar whose file vanished has nothing honest left to say.
        let mut live = StatusBars::default();
        live.toolchain_snapshot(Some(&snap(true)), now);
        live.toolchain_snapshot(None, now);
        assert_eq!(live.rows(), 0);
    }

    #[test]
    fn settle_folds_expired_bars_and_reports_the_row_change() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_failed(
            "⚠ ALab toolchain install failed — see Settings ▸ Packages",
            now,
        );
        bars.update_progress(
            &aterm_update::Progress::Staged {
                version: "0.48.0".into(),
                build: 99,
            },
            None,
            now,
        );
        assert_eq!(bars.rows(), 2);
        // A staged build with no computed posture holds the MANUAL stretch —
        // longer than a warning's hold — so the toolchain's warning folds first.
        assert_eq!(
            bars.deadline(),
            Some(now + HOLD_WARN),
            "the earliest fold wins"
        );
        assert!(!bars.settle(now), "nothing due yet");
        assert!(bars.settle(now + HOLD_WARN), "the toolchain bar folded");
        assert_eq!(bars.rows(), 1);
        assert_eq!(bars.lane_at(0), Some(Lane::Update));
        assert!(bars.settle(now + HOLD_STAGED_MANUAL));
        assert_eq!(bars.rows(), 0);
        assert_eq!(bars.fingerprint(), 0);
    }

    /// THE ROW OUTLIVES THE BAR. A folded bar's sentence is gone from the glass,
    /// and that is exactly when someone asks what the machine was doing — so the
    /// retirement writes it down, keeping the tone as the outcome.
    #[test]
    fn a_folded_bar_leaves_its_sentence_in_the_ledger() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_failed("⚠ ALab toolchain install failed", now);
        bars.update_progress(
            &aterm_update::Progress::Staged {
                version: "0.48.0".into(),
                build: 99,
            },
            Some(ApplyPosture::Automatic),
            now,
        );
        assert_eq!(bars.ledger().count(), 0, "nothing has folded yet");

        // The warn bar folds first; the armed staged row holds its automatic
        // stretch (the pull-down is the update-ready surface).
        assert!(bars.settle(now + HOLD_WARN), "the warn bar folded first");
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), 1, "only the folded lane is recorded");
        assert_eq!(rows[0].lane, Lane::Toolchain);
        assert_eq!(
            rows[0].outcome,
            Outcome::Warn,
            "a warn tone retires as a warn outcome, not an ok one"
        );
        assert_eq!(rows[0].finished, now + HOLD_WARN);
        assert!(
            !rows[0].title.is_empty(),
            "the words the user could have read survive the fold"
        );

        assert!(
            bars.settle(now + HOLD_STAGED_AUTOMATIC),
            "then the staged row"
        );
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            (rows[0].lane, rows[1].lane),
            (Lane::Toolchain, Lane::Update),
            "oldest first — the order they left the glass"
        );
        assert_eq!(rows[1].outcome, Outcome::Ok);
        assert!(
            rows[1].detail.contains("applies in place") && !rows[1].detail.contains("restart"),
            "the ledger row carries the honest sentence: {:?}",
            rows[1].detail
        );
        assert_eq!(bars.rows(), 0, "the ledger holds no rows on the glass");
        assert_eq!(
            bars.fingerprint(),
            0,
            "FL-1: a full ledger is still an idle zero — the record is not a repaint"
        );
    }

    /// ONE PASS, FOUR THINGS TO SAY (R6, 2026-09-10): "installed", then the
    /// machine settings — one row per changed item, the undo-bearing one first —
    /// then the managed agents. The lane holds one bar, so the later rows QUEUE
    /// and are read in order as each earlier one folds — and every one of them
    /// lands in the ledger. Replacement by assignment showed only the last and
    /// dropped the rest without a record; the machine rows go AHEAD of the
    /// managed notice (UX review, 2026-09-10) so the undo is read 8 s after the
    /// pass, not 38.
    #[test]
    fn terminal_rows_queue_behind_the_live_one_and_are_read_in_order() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_installed(
            "\u{2713} ALab toolchain installed: claude, codex \u{2014} open a new tab to use them",
            now,
        );
        bars.toolchain_managed_current(
            "claude 2.1.267 (build 2026091001); codex 0.154.0 (build 2026091001)",
            now,
        );
        bars.toolchain_machine_settings(
            "spotlight-noindex 73 dir(s) migrated; universal-control disabled",
            now,
        );
        assert_eq!(bars.rows(), 1, "one lane, one row");
        let title = |bars: &StatusBars| bars.bars().next().unwrap().1.text.title.clone();
        assert_eq!(title(&bars), "ALab toolchain installed");

        // The installed row folds; the undo-bearing machine row is promoted in
        // its place — a repaint, same row count — ahead of the managed notice
        // that was queued before it.
        assert!(bars.settle(now + HOLD_OK), "the glass changed");
        assert_eq!(bars.rows(), 1);
        assert_eq!(title(&bars), "Universal Control disabled");
        let (_, bar) = bars.bars().next().unwrap();
        assert_eq!(bar.text.tone, Tone::Info);
        assert_eq!(bar.text.glyph, '\u{21bb}');
        assert_eq!(
            bar.text.detail,
            format!(
                "undo: `aterm pkg doctor` prints the revert \u{00b7} {}",
                atpkg::machine::UNIVERSAL_CONTROL_REVERT
            )
        );
        assert!(
            bar.text.detail.ends_with(
                "defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges"
            ),
            "the revert is byte-identical to atpkg's: {}",
            bar.text.detail
        );
        assert_eq!(
            bar.fold_at,
            Some(now + HOLD_OK + HOLD_WARN),
            "the hold is anchored at promotion, not at posting"
        );
        assert_eq!(bars.ledger().count(), 1);

        // Then the Spotlight row — its own row, no detail to truncate.
        assert!(bars.settle(now + HOLD_OK + HOLD_WARN));
        assert_eq!(title(&bars), "Spotlight: 73 build dirs moved to .noindex");
        let (_, bar) = bars.bars().next().unwrap();
        assert_eq!(bar.text.tone, Tone::Info);
        assert!(bar.text.detail.is_empty(), "{}", bar.text.detail);
        assert_eq!(bars.ledger().count(), 2);

        // Then the managed-current notice, on its own shorter hold.
        assert!(bars.settle(now + HOLD_OK + 2 * HOLD_WARN));
        assert_eq!(
            title(&bars),
            "Claude Code 2.1.267 \u{00b7} Codex 0.154.0 \u{2014} aterm-managed, current"
        );
        let (_, bar) = bars.bars().next().unwrap();
        assert_eq!(bar.text.tone, Tone::Success);
        assert_eq!(bar.text.glyph, '\u{2713}');
        assert_eq!(
            bar.text.detail,
            "what `claude` and `codex` run in new tabs \u{00b7} build 2026091001"
        );
        assert_eq!(
            bar.fold_at,
            Some(now + HOLD_OK + 2 * HOLD_WARN + HOLD_MANAGED)
        );
        assert_eq!(bars.ledger().count(), 3);

        // And it folds too; the ledger has all four, oldest first.
        assert!(bars.settle(now + HOLD_OK + 2 * HOLD_WARN + HOLD_MANAGED));
        assert_eq!(bars.rows(), 0);
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].title, "ALab toolchain installed");
        assert_eq!(rows[1].title, "Universal Control disabled");
        assert_eq!(rows[2].title, "Spotlight: 73 build dirs moved to .noindex");
        assert!(rows[3].title.starts_with("Claude Code 2.1.267"));
        assert!(
            rows.iter()
                .all(|r| r.lane == Lane::Toolchain && r.outcome == Outcome::Ok)
        );
        assert!(
            !bars.settle(now + Duration::from_secs(3600)),
            "nothing left to promote"
        );
    }

    /// The R6 rows on an EMPTY lane go up at once, and their words: one agent
    /// alone, an unknown name, a bare name, a build-less item; a Spotlight item
    /// alone raises only its own row; nothing at all for an empty marker body.
    #[test]
    fn the_managed_and_machine_rows_render_their_words() {
        let now = t0();
        let mut bars = StatusBars::default();
        bars.toolchain_managed_current("claude 2.1.267 (build 2026091001)", now);
        assert_eq!(bars.rows(), 1);
        let (_, bar) = bars.bars().next().unwrap();
        assert_eq!(
            bar.text.title,
            "Claude Code 2.1.267 \u{2014} aterm-managed, current"
        );
        assert_eq!(
            bar.text.detail, "what `claude` runs in a new tab \u{00b7} build 2026091001",
            "one name reads singular"
        );
        assert_eq!(bar.fold_at, Some(now + HOLD_MANAGED));
        assert_eq!(bar.fill, None, "a notice carries no meter");

        let mut bars = StatusBars::default();
        bars.toolchain_managed_current("gemini; codex 0.154.0", now);
        let (_, bar) = bars.bars().next().unwrap();
        assert_eq!(
            bar.text.title,
            "gemini \u{00b7} Codex 0.154.0 \u{2014} aterm-managed, current"
        );
        assert_eq!(
            bar.text.detail, "what `gemini` and `codex` run in new tabs",
            "the names are the wire's, never a hardcoded pair; no builds, no build clause"
        );

        let mut bars = StatusBars::default();
        bars.toolchain_managed_current("  ;  ", now);
        assert_eq!(bars.rows(), 0, "an empty body raises nothing");

        let mut bars = StatusBars::default();
        bars.toolchain_machine_settings("spotlight-noindex 12 dir(s) migrated", now);
        assert_eq!(bars.rows(), 1);
        let (_, bar) = bars.bars().next().unwrap();
        assert_eq!(bar.text.title, "Spotlight: 12 build dirs moved to .noindex");
        assert!(
            bar.text.detail.is_empty(),
            "nothing to undo, nothing to say"
        );
        assert_eq!(bar.text.tone, Tone::Info);
        assert_eq!(
            bar.text.glyph, '\u{21bb}',
            "from the painter's documented set (⇣ ↻ ✓ ⚠ ⏸) — ⚙ rendered blank headless"
        );
        assert_eq!(bar.fold_at, Some(now + HOLD_WARN));
        assert!(
            !bars.settle(now + HOLD_WARN - Duration::from_secs(1)),
            "nothing queued behind a lone item"
        );

        // A terminal row replaces a LIVE (announced, meter-less) row — the pass is
        // over and this is its newest truth — rather than queueing behind it.
        let mut bars = StatusBars::default();
        bars.toolchain_announced("installing 2 program(s) (about 1 GB)", now);
        bars.toolchain_managed_current("codex 0.154.0 (build 2026091001)", now);
        assert_eq!(bars.rows(), 1);
        assert!(
            bars.bars()
                .next()
                .unwrap()
                .1
                .text
                .title
                .starts_with("Codex 0.154.0")
        );
    }

    /// THE MANAGED ROW POSTS ONCE PER TEXT PER LAUNCH (UX review, 2026-09-10).
    /// atpkg prints `managed-current:` on every pass and the recurring 6 h pass
    /// parses it like the seed pass, so the row — and the grid move it costs a
    /// running TUI — recurred every 6 h with nothing new. The first pass after
    /// launch posts; the same text again is ledger-only; a CHANGED text posts.
    #[test]
    fn the_managed_row_posts_once_per_text_and_a_repeat_is_ledger_only() {
        let now = t0();
        let mut bars = StatusBars::default();
        let wire = "claude 2.1.267 (build 2026091001); codex 0.154.0 (build 2026091001)";
        bars.toolchain_managed_current(wire, now);
        assert_eq!(bars.rows(), 1, "the first pass after launch posts");
        assert!(bars.settle(now + HOLD_MANAGED));
        assert_eq!(bars.rows(), 0);
        assert_eq!(bars.ledger().count(), 1);

        // Six hours on: the same text, with or without whitespace around it.
        let later = now + Duration::from_secs(6 * 3600);
        bars.toolchain_managed_current(&format!("  {wire} "), later);
        assert_eq!(bars.rows(), 0, "no row, no grid move");
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), 2, "but the pass is on the record");
        assert!(rows[1].title.starts_with("Claude Code 2.1.267"));
        assert_eq!(rows[1].outcome, Outcome::Ok);
        assert_eq!(rows[1].finished, later);

        // A new version: a new row.
        bars.toolchain_managed_current(
            "claude 2.1.268 (build 2026091002); codex 0.154.0 (build 2026091001)",
            later,
        );
        assert_eq!(bars.rows(), 1);
        let (_, bar) = bars.bars().next().unwrap();
        assert!(bar.text.title.starts_with("Claude Code 2.1.268"));
        assert_eq!(
            bar.text.detail,
            "what `claude` and `codex` run in new tabs \u{00b7} builds 2026091002 / 2026091001",
            "differing builds are listed"
        );
        // And a repeat of THAT is ledger-only again.
        bars.toolchain_managed_current(
            "claude 2.1.268 (build 2026091002); codex 0.154.0 (build 2026091001)",
            later,
        );
        assert!(
            bars.toolchain_queue.is_empty(),
            "nothing queued behind the live row"
        );
        assert_eq!(bars.ledger().count(), 3);
    }

    /// MACHINE SETTINGS: ONE ROW PER CHANGED ITEM, THE UNDO-BEARING ONE FIRST,
    /// AHEAD OF A QUEUED NOTICE (UX review, 2026-09-10). The old single row put
    /// "universal-control disabled — revert: `defaults …`" last on a
    /// right-truncating detail, invisible below 1328 px. An item this build does
    /// not know is its own title, verbatim.
    #[test]
    fn machine_settings_post_one_row_per_item_undo_first_and_ahead_of_notices() {
        let now = t0();
        let mut bars = StatusBars::default();
        bars.toolchain_installed(
            "\u{2713} ALab toolchain installed: claude, codex \u{2014} open a new tab to use them",
            now,
        );
        bars.toolchain_managed_current("claude 2.1.267 (build 2026091001)", now);
        bars.toolchain_machine_settings(
            "spotlight-noindex 1 dir(s) migrated; something-new tuned; universal-control disabled",
            now,
        );
        let queued: Vec<String> = bars
            .toolchain_queue
            .iter()
            .map(|(bar, _)| bar.text.title.clone())
            .collect();
        assert_eq!(
            queued,
            [
                "Universal Control disabled",
                "Spotlight: 1 build dir moved to .noindex",
                "something-new tuned",
                "Claude Code 2.1.267 \u{2014} aterm-managed, current",
            ],
            "undo first, then wire order, all ahead of the managed notice"
        );
        let holds: Vec<Duration> = bars.toolchain_queue.iter().map(|(_, h)| *h).collect();
        assert_eq!(holds, [HOLD_WARN, HOLD_WARN, HOLD_WARN, HOLD_MANAGED]);
        let unknown = &bars.toolchain_queue[2].0.text;
        assert_eq!(unknown.tone, Tone::Info);
        assert!(unknown.detail.is_empty());
        assert_eq!(unknown.glyph, '\u{21bb}');

        // The undo row's detail: the pointer first, the revert command last,
        // byte-identical to what `aterm pkg doctor` prints.
        let undo = &bars.toolchain_queue[0].0.text;
        assert_eq!(
            undo.detail,
            "undo: `aterm pkg doctor` prints the revert \u{00b7} \
             defaults -currentHost delete com.apple.universalcontrol Disable; \
             defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges"
        );
        assert_eq!(
            atpkg::machine::UNIVERSAL_CONTROL_REVERT,
            "defaults -currentHost delete com.apple.universalcontrol Disable; \
             defaults -currentHost delete com.apple.universalcontrol DisableMagicEdges"
        );

        // A machine row arriving on an EMPTY lane goes up at once; a second
        // item queues behind it in order.
        let mut bars = StatusBars::default();
        bars.toolchain_machine_settings(
            "spotlight-noindex 73 dir(s) migrated; universal-control disabled",
            now,
        );
        assert_eq!(bars.rows(), 1);
        assert_eq!(
            bars.bars().next().unwrap().1.text.title,
            "Universal Control disabled"
        );
        assert_eq!(bars.toolchain_queue.len(), 1);
        assert!(bars.settle(now + HOLD_WARN));
        assert_eq!(
            bars.bars().next().unwrap().1.text.title,
            "Spotlight: 73 build dirs moved to .noindex"
        );
        let mut bars = StatusBars::default();
        bars.toolchain_machine_settings(" ; ", now);
        assert_eq!(bars.rows(), 0, "an empty body raises nothing");
    }

    /// `aterm ctl appnotice <lane> <text>`: an Info row on either lane, held
    /// [`HOLD_NOTICE`], recorded when it folds — the terminal-run install's voice.
    #[test]
    fn a_notice_posts_a_text_row_on_either_lane_and_is_recorded() {
        let now = t0();
        let mut bars = StatusBars::default();
        bars.notice(
            Lane::Toolchain,
            "aterm pkg install claude: 2.1.267 installed",
            now,
        );
        bars.notice(Lane::Update, "hello\u{1b}[2J from the terminal", now);
        assert_eq!(bars.rows(), 2);
        let rows: Vec<_> = bars.bars().collect();
        assert_eq!(rows[0].0, Lane::Toolchain);
        assert_eq!(
            rows[0].1.text.title,
            "aterm pkg install claude: 2.1.267 installed"
        );
        assert_eq!(rows[0].1.text.tone, Tone::Info);
        assert_eq!(rows[0].1.fold_at, Some(now + HOLD_NOTICE));
        assert_eq!(rows[1].0, Lane::Update);
        assert_eq!(
            rows[1].1.text.title, "hello[2J from the terminal",
            "sanitized"
        );
        assert_eq!(rows[1].1.fold_at, Some(now + HOLD_NOTICE));
        assert!(bars.settle(now + HOLD_NOTICE));
        assert_eq!(bars.rows(), 0);
        assert_eq!(bars.ledger().count(), 2);
    }

    /// The row m21 actually showed (2026-09-10): the updater's 178-char health body
    /// plus the GUI's Settings clause, cut at `Run \`aterm-c…` by the old 160-char
    /// cap — both affordances gone. The command sentence now survives whatever the
    /// prose does, and the prose is what carries the ellipsis.
    #[test]
    fn the_command_in_a_detail_survives_the_cap() {
        let body = "20 consecutive checks since 2026-08-27T22:04:36Z: release manifests exist \
                    but cannot be downloaded \u{2014} this build's update pipeline is likely \
                    broken. Run `aterm-ctl update status` for details. \u{2014} see Settings \
                    \u{25b8} Software Update";
        assert!(body.chars().count() > 160);
        // Under the shipping cap the whole sentence fits.
        let whole = shape_detail(body, DETAIL_CAP);
        assert_eq!(whole, body, "a body under the cap is untouched");
        // Under a cap it cannot fit, the COMMAND is whole and the prose gives way.
        let shaped = shape_detail(body, 120);
        assert!(shaped.chars().count() <= 120, "{}", shaped.chars().count());
        assert!(
            shaped.contains("Run `aterm-ctl update status` for details."),
            "the command sentence is intact: {shaped}"
        );
        assert!(
            shaped.contains("Settings \u{25b8} Software Update"),
            "and so is the pointer after it: {shaped}"
        );
        assert!(
            shaped.starts_with("20 consecutive checks") && shaped.contains("\u{2026} Run `"),
            "the prose is what was cut, with the ellipsis on it: {shaped}"
        );
        // No command: plain right truncation, as before.
        let plain = "x".repeat(300);
        assert_eq!(
            shape_detail(&plain, 10),
            format!("{}\u{2026}", "x".repeat(10))
        );
        // Control characters never reach the cell either way.
        assert_eq!(shape_detail("a\u{1b}[2Jb. Run `c`", 200), "a[2Jb. Run `c`");
        // Through the outcome row itself.
        let mut bars = StatusBars::default();
        bars.update_outcome(
            '\u{26a0}',
            "aterm auto-update is failing",
            body,
            Tone::Warn,
            t0(),
        );
        let (_, bar) = bars.bars().next().unwrap();
        assert!(bar.text.detail.contains("`aterm-ctl update status`"));
    }

    /// THE STANDING WARNING: a persistent check failure stays on the pull-down until
    /// the ledger heals — not for 45 s of a process that runs for weeks. It never
    /// folds on a hold, it is restated in place, it yields to a staged build, and
    /// when the ledger heals it leaves through the ledger like any other row.
    #[test]
    fn a_persistent_check_failure_stands_until_the_ledger_heals() {
        let mut bars = StatusBars::default();
        let now = t0();
        let detail = "20 consecutive checks since 2026-08-27T22:04:36Z: … Run `aterm-ctl update status` for details.";
        assert!(bars.update_health_standing("aterm auto-update is failing", detail));
        assert!(bars.update_bar_is_health());
        assert_eq!(bars.rows(), 1);
        assert_eq!(
            bars.deadline(),
            None,
            "no hold and no staleness cap: nothing to wake for"
        );
        // A day later it is still there, and the ledger has nothing.
        assert!(!bars.settle(now + Duration::from_secs(24 * 60 * 60)));
        assert_eq!(bars.rows(), 1);
        assert_eq!(bars.ledger().count(), 0);
        // Restated with the same words: no change; with new words: rewritten in place.
        assert!(!bars.update_health_standing("aterm auto-update is failing", detail));
        assert!(bars.update_health_standing(
            "aterm auto-update is failing",
            "21 consecutive checks … Run `aterm-ctl update status` for details."
        ));
        assert_eq!(bars.rows(), 1, "a repaint, never a re-grid");
        let (_, bar) = bars.bars().next().unwrap();
        assert!(bar.text.detail.starts_with("21 consecutive"));
        assert_eq!(bar.text.tone, Tone::Warn);
        // The wire says it is live and warn-toned.
        let rows = bars.activity_rows(now);
        assert!(
            rows[0].starts_with("activity kind=update phase=live "),
            "{}",
            rows[0]
        );
        // Healed: it leaves through the ledger.
        assert!(bars.update_health_healed(now + HOLD_WARN));
        assert_eq!(bars.rows(), 0);
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            (rows[0].lane, rows[0].outcome),
            (Lane::Update, Outcome::Warn)
        );
        assert!(rows[0].detail.contains("`aterm-ctl update status`"));
        assert!(
            !bars.update_health_healed(now),
            "idempotent: nothing to heal twice"
        );

        // It yields to a staged build — a stage proves the check works.
        let mut bars = StatusBars::default();
        bars.update_progress(
            &aterm_update::Progress::Staged {
                version: "0.81.0".into(),
                build: 99,
            },
            Some(ApplyPosture::Automatic),
            now,
        );
        assert!(!bars.update_health_standing("aterm auto-update is failing", detail));
        assert!(bars.update_bar_is_staged(), "the staged row is untouched");
        // …and a download that begins over a standing warning retires it INTO the
        // ledger rather than dropping it.
        let mut bars = StatusBars::default();
        assert!(bars.update_health_standing("aterm auto-update is failing", detail));
        bars.update_progress(
            &aterm_update::Progress::Downloading {
                version: "0.81.0".into(),
                bytes_done: 1,
                bytes_total: 2,
            },
            None,
            now,
        );
        assert!(!bars.update_bar_is_health());
        assert_eq!(
            bars.ledger().count(),
            1,
            "the warning it replaced is on record"
        );
    }

    /// AN OUTCOME OVER THE STANDING WARNING QUEUES IT, NEVER DROPS IT (2026-09-10
    /// review). The updater's `HealthNotify` fires once per class per process, so
    /// a warning an apply-lane outcome (or an `aterm ctl appnotice update …`)
    /// replaced by assignment was gone for the life of the process — the ledger
    /// still failing, the glass silent. Now it waits behind the outcome, with its
    /// newest words, and returns when the outcome folds; a Staged row that was
    /// also waiting outranks it; and a heal while it waits retires it INTO the
    /// ledger, so nothing comes back.
    #[test]
    fn an_outcome_over_a_standing_health_row_restates_it_when_the_outcome_folds() {
        let mut bars = StatusBars::default();
        let now = t0();
        let detail = "20 consecutive checks since 2026-08-27T22:04:36Z: … Run `aterm-ctl update status` for details.";
        assert!(bars.update_health_standing("aterm auto-update is failing", detail));

        // A notice covers it: the glass shows the notice, the ledger has nothing
        // (the warning was not retired — it is waiting).
        bars.notice(Lane::Update, "update paused by the operator", now);
        assert!(!bars.update_bar_is_health());
        assert_eq!(bars.rows(), 1);
        assert_eq!(
            bars.ledger().count(),
            0,
            "queued, not dropped into the ledger"
        );
        // Restated while covered: the glass does not change, the words do.
        assert!(!bars.update_health_standing(
            "aterm auto-update is failing",
            "21 consecutive checks … Run `aterm-ctl update status` for details."
        ));
        assert!(!bars.update_bar_is_health());
        // The notice folds: it goes to the ledger and the warning is back, with
        // the newest words, standing (no hold, no staleness cap).
        assert!(bars.settle(now + HOLD_NOTICE));
        assert!(bars.update_bar_is_health(), "the warning returned");
        assert_eq!(bars.rows(), 1);
        let (_, bar) = bars.bars().next().unwrap();
        assert!(
            bar.text.detail.starts_with("21 consecutive"),
            "{}",
            bar.text.detail
        );
        assert_eq!(bar.fold_at, None);
        assert_eq!(bar.stale_at, None);
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].lane, rows[0].outcome), (Lane::Update, Outcome::Ok));
        assert!(rows[0].title.contains("paused"));

        // A Warn outcome covers it, then the ledger heals while it waits: the
        // warning retires into the ledger from the queue, and NOTHING returns
        // when the outcome folds.
        bars.update_outcome(
            '\u{26a0}',
            "Update delayed",
            "a shell is busy",
            Tone::Warn,
            now,
        );
        assert!(!bars.update_bar_is_health());
        assert!(
            !bars.update_health_healed(now),
            "nothing left the GLASS — the outcome is still up"
        );
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), 2, "…but the queued warning is on record");
        assert_eq!(rows[1].outcome, Outcome::Warn);
        assert!(rows[1].detail.starts_with("21 consecutive"));
        assert!(bars.settle(now + HOLD_WARN));
        assert_eq!(bars.rows(), 0, "healed: the warning does not come back");
        assert!(!bars.update_health_healed(now), "idempotent");

        // A Staged row waiting behind the same outcome outranks the warning: the
        // ready row returns first; the warning returns when the ready row folds.
        let mut bars = StatusBars::default();
        bars.update_progress(
            &aterm_update::Progress::Staged {
                version: "0.81.0".into(),
                build: 99,
            },
            Some(ApplyPosture::Automatic),
            now,
        );
        assert!(!bars.update_health_standing("aterm auto-update is failing", detail));
        assert!(bars.update_bar_is_staged());
        bars.update_outcome('\u{23f8}', "Update paused", "", Tone::Info, now);
        assert!(bars.settle(now + HOLD_OK));
        assert!(
            bars.update_bar_is_staged(),
            "the ready row comes back first"
        );
        let staged_folds = bars.deadline().expect("the staged row has a hold");
        assert!(bars.settle(staged_folds));
        assert!(
            bars.update_bar_is_health(),
            "…and the warning takes the lane when the ready row folds"
        );
        assert_eq!(bars.ledger().count(), 2, "the outcome and the staged row");
    }

    /// The wire is a SEPARATE grammar from the glass. `layout` makes a hostile
    /// string safe to PAINT; this makes it safe to PARSE — a space, a newline or
    /// a `=` out of some installer's stderr must not forge a field, split a row,
    /// or truncate the reply a driving agent is reading.
    #[test]
    fn activity_rows_cannot_be_forged_by_the_words_in_them() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_failed(
            "boom\nactivity kind=update phase=done outcome=ok stats= detail= title=forged",
            now,
        );
        let rows = bars.activity_rows(now);
        assert_eq!(rows.len(), 1, "one bar is one row, whatever it says");
        let row = &rows[0];
        assert!(
            !row.contains('\n') && !row.contains('\r'),
            "no row can break the line framing: {row}"
        );
        assert_eq!(
            row.matches("activity kind=").count(),
            1,
            "the payload cannot mint a second activity: {row}"
        );
        assert!(
            row.starts_with("activity kind=toolchain phase=live "),
            "the lane and phase are the renderer's to say, not the text's: {row}"
        );
        assert!(row.ends_with(" outcome=-"), "a live row has no outcome yet");
    }

    /// The shape `appstatus` promises: live rows first, then the ledger
    /// oldest-first, each finished row carrying how it ended and how long ago.
    #[test]
    fn activity_rows_put_the_live_bars_above_the_finished_ones() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_failed("install failed", now);
        assert!(bars.settle(now + HOLD_WARN), "it folds into the ledger");
        bars.update_progress(
            &aterm_update::Progress::Downloading {
                version: "0.62.0".into(),
                bytes_done: 45,
                bytes_total: 90,
            },
            None,
            now + HOLD_WARN,
        );

        let rows = bars.activity_rows(now + HOLD_WARN + Duration::from_millis(1500));
        assert_eq!(rows.len(), 2, "one live, one finished");
        assert!(
            rows[0].starts_with("activity kind=update phase=live progress=50/100 "),
            "the live download leads, with its meter: {}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("activity kind=toolchain phase=done progress=- "),
            "the finished install follows: {}",
            rows[1]
        );
        assert!(
            rows[1].contains(" outcome=warn ") && rows[1].ends_with(" since_ms=1500"),
            "how it ended and how long ago: {}",
            rows[1]
        );
    }

    /// A process can run for weeks. The ledger is a RING, so the record is
    /// bounded: the newest [`LEDGER_ROWS`] survive and the oldest fall off.
    #[test]
    fn the_ledger_is_a_ring_that_keeps_the_newest() {
        let mut bars = StatusBars::default();
        let mut now = t0();
        for i in 0..(LEDGER_ROWS + 5) {
            bars.update_progress(
                &aterm_update::Progress::Staged {
                    version: format!("0.{i}.0"),
                    build: i as u64,
                },
                None,
                now,
            );
            now += HOLD_STAGED_MANUAL;
            assert!(bars.settle(now), "each staged bar folds in turn");
        }
        let rows: Vec<_> = bars.ledger().collect();
        assert_eq!(rows.len(), LEDGER_ROWS, "the ring is capped");
        assert!(
            rows[0].title.contains("0.5.") || rows[0].detail.contains("0.5."),
            "the OLDEST kept row is the 6th activity, not the 1st: {:?}",
            rows[0]
        );
        assert!(
            rows.windows(2).all(|w| w[0].finished <= w[1].finished),
            "still oldest-first after eviction"
        );
    }

    #[test]
    fn update_reports_map_to_bar_states() {
        use aterm_update::Progress as P;
        let mut bars = StatusBars::default();
        let now = t0();
        bars.update_progress(
            &P::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 74_000_000,
            },
            None,
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "aterm update v0.48.0");
        assert_eq!(bar.text.stats, "45 MB / 74 MB");
        assert!((bar.fill.unwrap() - 45.0 / 74.0).abs() < 1e-3);
        assert_eq!(bar.fold_at, None);
        // An unknown total is honest: no meter, just the bytes so far.
        bars.update_progress(
            &P::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 0,
            },
            None,
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.fill, None);
        assert_eq!(bar.text.stats, "45 MB");
        bars.update_progress(
            &P::Verifying {
                version: "0.48.0".into(),
            },
            None,
            now,
        );
        assert_eq!(
            bars.bars().next().unwrap().1.text.detail,
            "verifying and staging…"
        );
        bars.update_progress(
            &P::Staged {
                version: "0.48.0".into(),
                build: 99,
            },
            Some(ApplyPosture::Automatic),
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(
            bar.text.title,
            "aterm v0.48.0 is ready · click to apply now"
        );
        assert_eq!(
            bar.text.detail,
            "build 99 — verified; applies in place within ~2 min — your shells keep running"
        );
        assert_eq!(bar.fill, None, "no meter on a staged row");
        assert!(!bar.text.detail.contains("restart"));
        assert_eq!(bar.staged_build, Some(99));
        // The pull-down IS the update-ready surface: it holds while the
        // automatic lane is armed to apply.
        assert_eq!(bar.fold_at, Some(now + HOLD_STAGED_AUTOMATIC));
        bars.update_progress(
            &P::Failed {
                detail: "zip sha256 mismatch".into(),
            },
            None,
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.tone, Tone::Warn);
        assert!(bar.text.detail.contains("Software Update"));
        assert_eq!(bar.fold_at, Some(now + HOLD_WARN));
    }

    /// THE UPDATE LANE'S OWN MOMENTS (2026-09-07). A staged row holds for the
    /// automatic stretch when the lane is armed, for the manual stretch
    /// otherwise, and carries the press affordance except where the handoff is
    /// off; "installing" REWRITES the live row (the EXPLICIT lane adds one
    /// through `update_installing_added`, which a refusal takes away again) and hands back the
    /// words a synchronous refusal restores; "finishing" and "landed" say their
    /// piece; an outcome holds by its tone; and the staged predicate is exactly
    /// the state a press applies in.
    #[test]
    fn the_update_lane_speaks_through_its_row_at_every_moment() {
        use ApplyPosture as P;
        let now = t0();
        let staged = |bars: &mut StatusBars, posture: Option<P>| {
            bars.update_progress(
                &aterm_update::Progress::Staged {
                    version: "0.76.0".into(),
                    build: 7,
                },
                posture,
                now,
            );
        };
        // Holds by posture.
        let mut bars = StatusBars::default();
        staged(&mut bars, Some(P::Automatic));
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.fold_at, Some(now + HOLD_STAGED_AUTOMATIC));
        assert!(
            bar.text.title.ends_with(CLICK_TO_APPLY),
            "the press affordance rides in the title: {}",
            bar.text.title
        );
        assert!(bars.update_bar_is_staged());
        staged(&mut bars, Some(P::ManualOnlyLatched { lapses: true }));
        assert_eq!(
            bars.bars().next().unwrap().1.fold_at,
            Some(now + HOLD_STAGED_AUTOMATIC)
        );
        staged(&mut bars, Some(P::ManualByConfig));
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.fold_at, Some(now + HOLD_STAGED_MANUAL));
        assert!(bar.text.title.ends_with(CLICK_TO_APPLY));
        staged(&mut bars, None);
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.fold_at, Some(now + HOLD_STAGED_MANUAL));
        assert!(bar.text.title.ends_with(CLICK_TO_APPLY));
        // Where the handoff is off a press can only open the details.
        for why in HandoffUnavailable::ALL {
            staged(&mut bars, Some(P::HandoffDisabled { why, veto: None }));
            let bar = bars.bars().next().unwrap().1;
            assert!(
                !bar.text.title.contains(CLICK_TO_APPLY),
                "{}",
                bar.text.title
            );
            assert_eq!(bar.fold_at, Some(now + HOLD_STAGED_MANUAL));
        }

        // INSTALLING rewrites the live row and returns the previous words…
        let mut bars = StatusBars::default();
        staged(&mut bars, Some(P::Automatic));
        let previous = bars.update_installing("0.76.0", now);
        assert!(previous.is_some());
        assert_eq!(bars.rows(), 1, "never a new row");
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Installing aterm v0.76.0");
        assert!(bar.text.detail.contains("shells are safe"));
        assert_eq!(bar.fold_at, None, "live until the successor takes over");
        assert_eq!(bar.stale_at, Some(now + HANDOFF_STALE));
        assert!(!bars.update_bar_is_staged(), "a press no longer applies");
        assert_eq!(
            bar.staged_build, None,
            "not a staged row: the posture restatement leaves it alone"
        );
        assert!(!bars.restate_apply_posture(7, P::Automatic, now));
        // …which a refusal restores exactly.
        bars.retire_installing(previous);
        assert!(bars.update_bar_is_staged());
        assert_eq!(
            bars.bars().next().unwrap().1.text.title,
            "aterm v0.76.0 is ready · click to apply now"
        );
        // With NO row up, installing adds none…
        let mut none = StatusBars::default();
        assert!(none.update_installing("0.76.0", now).is_none());
        assert_eq!(none.rows(), 0);
        none.retire_installing(None);
        assert_eq!(none.rows(), 0);
        // …unless the EXPLICIT lane adds one, which a refusal takes away again.
        assert!(none.update_installing_added("0.76.0", now));
        assert_eq!(none.rows(), 1);
        assert!(
            !none.update_installing_added("0.76.0", now),
            "one row, once"
        );
        assert!(!none.update_bar_is_staged());
        none.retire_installing(None);
        assert_eq!(none.rows(), 0);
        // A row that moved on to an outcome is not retired.
        let mut moved = StatusBars::default();
        staged(&mut moved, Some(P::Automatic));
        let previous = moved.update_installing("0.76.0", now);
        moved.update_outcome('\u{21bb}', "Update waiting", "x", Tone::Info, now);
        moved.retire_installing(previous);
        assert_eq!(moved.bars().next().unwrap().1.text.title, "Update waiting");
        // With no staged VERSION (the re-exec seam) the title says "update",
        // never a bare "aterm v".
        let mut unversioned = StatusBars::default();
        staged(&mut unversioned, Some(P::Automatic));
        let _ = unversioned.update_installing("", now);
        assert_eq!(
            unversioned.bars().next().unwrap().1.text.title,
            "Installing update"
        );
        unversioned.update_finishing("  ", now);
        assert_eq!(
            unversioned.bars().next().unwrap().1.text.title,
            "Finishing update"
        );

        // FINISHING (the successor's inherited row) and LANDED.
        let mut bars = StatusBars::default();
        bars.update_finishing("0.76.0", now);
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Finishing aterm v0.76.0");
        assert!(bar.text.detail.contains("queued"));
        assert_eq!(bar.stale_at, Some(now + HANDOFF_STALE));
        bars.update_landed("0.76.0", 7, true, now);
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Updated \u{2014} now on v0.76.0");
        assert_eq!(bar.text.tone, Tone::Success);
        assert_eq!(bar.fold_at, Some(now + HOLD_OK));
        bars.update_landed("", 7, true, now);
        assert_eq!(
            bars.bars().next().unwrap().1.text.title,
            "Updated \u{2014} now on build 7"
        );

        // OUTCOMES hold by tone.
        let mut bars = StatusBars::default();
        bars.update_outcome(
            '\u{2191}',
            "Update delayed",
            "retries on its own",
            Tone::Info,
            now,
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.fold_at, Some(now + HOLD_OK));
        assert_eq!(bar.text.title, "Update delayed");
        bars.update_outcome(
            '\u{26a0}',
            "Update paused",
            "see Version menu",
            Tone::Warn,
            now,
        );
        assert_eq!(bars.bars().next().unwrap().1.fold_at, Some(now + HOLD_WARN));
        assert!(!bars.update_bar_is_staged());
    }

    /// THE HANDOFF CARRY: the bars cross the swap as plain words, re-seed as
    /// live bars under the handoff's staleness cap, keep their order, and the
    /// successor's update row moves on to "finishing" only when the carry had
    /// one; an unknown lane or tone is dropped, not guessed.
    #[test]
    fn the_bars_cross_the_handoff_as_carried_words() {
        let now = t0();
        let mut bars = StatusBars::default();
        bars.toolchain_announced("installing ay, trust", now);
        bars.update_progress(
            &aterm_update::Progress::Staged {
                version: "0.76.0".into(),
                build: 7,
            },
            Some(ApplyPosture::Automatic),
            now,
        );
        let _ = bars.update_installing("0.76.0", now);
        let carried = bars.carried();
        assert_eq!(carried.len(), 2);
        assert_eq!(carried[0].lane, "toolchain");
        assert_eq!(carried[1].lane, "update");
        assert_eq!(carried[1].title, "Installing aterm v0.76.0");
        assert_eq!(carried[1].tone, "info");

        let later = now + Duration::from_secs(3);
        let successor = StatusBars::from_handoff_carry(&carried, Some("0.76.0"), later);
        assert_eq!(successor.rows(), 2);
        assert_eq!(successor.lane_at(0), Some(Lane::Toolchain));
        let (_, update) = successor.bars().nth(1).unwrap();
        assert_eq!(update.text.title, "Finishing aterm v0.76.0");
        assert_eq!(update.stale_at, Some(later + HANDOFF_STALE));
        let (_, toolchain) = successor.bars().next().unwrap();
        assert_eq!(toolchain.text.title, "Installing the ALab toolchain");
        assert_eq!(toolchain.fold_at, None);
        assert_eq!(toolchain.stale_at, Some(later + HANDOFF_STALE));

        // No update row carried: nothing invented.
        let only_toolchain = &carried[..1];
        let successor = StatusBars::from_handoff_carry(only_toolchain, Some("0.76.0"), later);
        assert_eq!(successor.rows(), 1);
        assert_eq!(successor.lane_at(0), Some(Lane::Toolchain));
        // COMMIT: the carried toolchain words fold after the ordinary hold —
        // this process has no feed for that pass — and the update row is the
        // landing's to rewrite.
        let mut successor = successor;
        successor.after_handoff_commit(later);
        let toolchain = successor.bars().next().unwrap().1;
        assert_eq!(toolchain.fold_at, Some(later + HOLD_OK));
        assert_eq!(toolchain.stale_at, None);
        assert_eq!(
            toolchain.fill, None,
            "no meter for a feed this process cannot hear"
        );
        assert!(
            successor.settle(later + HOLD_OK),
            "the carried toolchain row folds into the ledger"
        );
        assert_eq!(successor.rows(), 0);
        // A carried WARNING keeps the warning's read time, and a carried live
        // meter is dropped rather than completed by the successor's first
        // snapshot.
        let warned = vec![crate::session_store::CarriedBar {
            lane: "toolchain".into(),
            glyph: '\u{26a0}',
            title: "ALab toolchain".into(),
            detail: "trust — extracting 120 MB / 900 MB".into(),
            stats: String::new(),
            tone: "warn".into(),
            fill_permille: Some(430),
        }];
        let mut warned = StatusBars::from_handoff_carry(&warned, None, later);
        assert_eq!(warned.bars().next().unwrap().1.fill, Some(0.43));
        warned.after_handoff_commit(later);
        let bar = warned.bars().next().unwrap().1;
        assert_eq!(bar.fold_at, Some(later + HOLD_WARN));
        assert_eq!(bar.fill, None);
        warned.toolchain_snapshot(Some(&snap(true)), later);
        assert_eq!(
            warned.bars().next().unwrap().1.fill,
            None,
            "the terminal-outcome guard has no meter to complete"
        );
        // …and only the CARRIED words: a toolchain bar this process has since
        // written itself keeps its own deadlines.
        let mut own = StatusBars::from_handoff_carry(&carried, Some("0.76.0"), later);
        own.toolchain_announced("installing ay, trust (900 MB)", later);
        own.after_handoff_commit(later);
        assert_eq!(own.bars().next().unwrap().1.fold_at, None);
        // Not the successor of a handoff: the words come back as they were.
        let plain = StatusBars::from_handoff_carry(&carried, None, later);
        assert_eq!(
            plain.bars().nth(1).unwrap().1.text.title,
            "Installing aterm v0.76.0"
        );
        // Junk is dropped.
        let junk = vec![
            crate::session_store::CarriedBar {
                lane: "mystery".into(),
                glyph: '?',
                title: "x".into(),
                detail: String::new(),
                stats: String::new(),
                tone: "info".into(),
                fill_permille: None,
            },
            crate::session_store::CarriedBar {
                lane: "update".into(),
                glyph: '?',
                title: "x".into(),
                detail: String::new(),
                stats: String::new(),
                tone: "loud".into(),
                fill_permille: Some(7000),
            },
        ];
        assert_eq!(StatusBars::from_handoff_carry(&junk, None, later).rows(), 0);
        assert_eq!(
            StatusBars::from_handoff_carry(&[], Some("0.76.0"), later).rows(),
            0
        );
    }

    #[test]
    fn hostile_strings_are_sanitized_before_they_become_cells() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_failed("bad\u{1b}[31m thing\u{7}", now);
        let d = &bars.bars().next().unwrap().1.text.detail;
        assert!(!d.contains('\u{1b}') && !d.contains('\u{7}'), "{d:?}");
        // A program name that fails the store gate has no row words at all.
        let mut bars = StatusBars::default();
        let mut f = file(Some(7), "net");
        f.programs.clear();
        f.queue = vec!["../evil".into()];
        f.programs.insert(
            "../evil".into(),
            atpkg::progress::ProgramProgress {
                phase: Phase::Download,
                bytes_done: 1,
                bytes_total: 2,
                build: None,
                bumped: false,
                bumped_with: None,
                error: None,
            },
        );
        bars.toolchain_snapshot(
            Some(&crate::PkgProgressSnapshot {
                file: f,
                running: true,
            }),
            now,
        );
        assert_eq!(bars.bars().next().unwrap().1.text.detail, "");
    }

    #[test]
    fn the_layout_degrades_in_priority_order() {
        let text = BarText {
            glyph: '\u{21e3}',
            title: "Installing the ALab toolchain".into(),
            detail: "trust — extracting 120 MB / 900 MB".into(),
            stats: "3 of 10 · 512 MB / 1.2 GB".into(),
            tone: Tone::Info,
        };
        // Wide: everything, meter at its preferred width, stats flush right.
        let l = layout(&text, Some(0.43), 160);
        assert_eq!(l.title, text.title);
        assert!(l.detail.is_some());
        assert_eq!(l.meter.map(|m| m.1), Some(METER_PREFERRED));
        assert_eq!(l.pct.as_ref().map(|p| p.1.as_str()), Some(" 43%"));
        let (sc, s) = l.stats.clone().unwrap();
        assert_eq!(
            sc + s.chars().count(),
            160 - MARGIN,
            "stats end at the margin"
        );
        // The meter sits left of " NN%", which sits left of the stats.
        let (mc, mw) = l.meter.unwrap();
        assert_eq!(mc + mw + 1, l.pct.as_ref().unwrap().0);
        assert!(l.pct.as_ref().unwrap().0 + 4 + 2 == sc);
        // Narrower: the stats go first.
        let l = layout(&text, Some(0.43), 70);
        assert!(l.stats.is_none());
        assert!(l.detail.is_some());
        assert!(l.meter.is_some());
        // Narrower still: the detail truncates, then goes; the meter shrinks.
        let l = layout(&text, Some(0.43), 48);
        let d = l.detail.as_ref().map(|d| d.1.clone()).unwrap_or_default();
        assert!(d.is_empty() || d.ends_with('\u{2026}'), "{d}");
        assert!(l.meter.is_some_and(|m| m.1 >= METER_MIN));
        // Tiny: glyph + title only, the title truncated.
        let l = layout(&text, Some(0.43), 16);
        assert!(l.meter.is_none() && l.pct.is_none() && l.detail.is_none());
        assert!(l.title.ends_with('\u{2026}'));
        assert!(l.title.chars().count() + 2 <= 16 - 2 * MARGIN);
        // Every placed piece stays inside the row.
        for cols in [8usize, 12, 20, 33, 47, 61, 80, 200] {
            let l = layout(&text, Some(0.5), cols);
            let end = |c: usize, s: &str| c + s.chars().count();
            assert!(end(l.title_col, &l.title) <= cols - MARGIN, "cols {cols}");
            if let Some((c, d)) = &l.detail {
                assert!(end(*c, d) <= cols - MARGIN, "cols {cols}");
            }
            if let Some((c, w)) = l.meter {
                assert!(c + w <= cols - MARGIN, "cols {cols}");
                assert!(c >= l.title_col + l.title.chars().count(), "cols {cols}");
            }
            if let Some((c, s)) = &l.stats {
                assert_eq!(end(*c, s), cols - MARGIN, "cols {cols}");
            }
        }
    }

    /// A DETAIL IS DRAWN WHOLE-ISH OR NOT AT ALL, AND A COMMAND OUTRANKS ITS
    /// PROSE (UX review, 2026-09-10). At 80 cols the managed row showed "these
    /// are wha…" — a stub that said nothing; the floor is [`DETAIL_FLOOR`] cells
    /// now. When a detail with a backtick command must be cut, the prose gives
    /// way and the command survives, still inside the row.
    #[test]
    fn a_short_detail_is_dropped_and_a_cut_detail_keeps_its_command() {
        let title = "Claude Code 2.1.267 \u{00b7} Codex 0.154.0 \u{2014} aterm-managed, current";
        let text = BarText {
            glyph: '\u{2713}',
            title: title.into(),
            detail: "what `claude` and `codex` run in new tabs \u{00b7} build 2026091001".into(),
            stats: String::new(),
            tone: Tone::Success,
        };
        // 80 cols: 16 cells left after the head — below the floor, no stub.
        let l = layout(&text, None, 80);
        assert_eq!(l.title, title);
        assert!(l.detail.is_none(), "{:?}", l.detail);
        // Just enough room for the floor: the detail is shaped, and the
        // commands are whole while the prose carries the ellipsis.
        let cols = 2 * MARGIN + 2 + title.chars().count() + 2 + DETAIL_FLOOR + 8;
        let l = layout(&text, None, cols);
        let (col, d) = l.detail.clone().expect("a detail at the floor");
        assert!(d.contains("`claude`"), "{d}");
        assert!(d.contains('\u{2026}'), "{d}");
        assert!(col + d.chars().count() <= cols - MARGIN, "{d}");
        // One cell short of the floor: nothing.
        let l = layout(&text, None, cols - 9);
        assert!(l.detail.is_none(), "{:?}", l.detail);
        // The undo row: the pointer outranks the revert command, which is the
        // last thing to go — and never the first.
        let undo = BarText {
            glyph: '\u{21bb}',
            title: "Universal Control disabled".into(),
            detail: format!(
                "undo: `aterm pkg doctor` prints the revert \u{00b7} {}",
                atpkg::machine::UNIVERSAL_CONTROL_REVERT
            ),
            stats: String::new(),
            tone: Tone::Info,
        };
        for cols in [60usize, 70, 80, 100, 130, 150] {
            let l = layout(&undo, None, cols);
            let (col, d) = l.detail.clone().expect("the undo row keeps a detail");
            assert!(d.contains("`aterm pkg doctor`"), "cols {cols}: {d}");
            assert!(col + d.chars().count() <= cols - MARGIN, "cols {cols}: {d}");
        }
        // 28 cells of head, 183 of detail (both deletes): whole from 212 cols.
        let l = layout(&undo, None, 240);
        assert!(
            l.detail
                .unwrap()
                .1
                .ends_with("com.apple.universalcontrol DisableMagicEdges"),
            "wide enough, the revert is whole"
        );
        // A plain detail still truncates from the right.
        let plain = BarText {
            glyph: '\u{2713}',
            title: "ALab toolchain installed".into(),
            detail: "x".repeat(200),
            stats: String::new(),
            tone: Tone::Success,
        };
        let l = layout(&plain, None, 60);
        let (col, d) = l.detail.unwrap();
        assert!(d.ends_with('\u{2026}') && d.starts_with("xxxx"));
        assert_eq!(col + d.chars().count(), 60 - MARGIN);
    }

    #[test]
    fn painted_rows_are_exactly_cols_wide_and_carry_the_words() {
        let mut bars = StatusBars::default();
        let now = t0();
        bars.toolchain_snapshot(Some(&snap(true)), now);
        bars.update_progress(
            &aterm_update::Progress::Downloading {
                version: "0.48.0".into(),
                bytes_done: 45_000_000,
                bytes_total: 74_000_000,
            },
            None,
            now,
        );
        // Wide enough for every piece of the toolchain row (the detail outranks
        // the stats, so a narrower row would drop the figures first).
        let rows = paint_rows(&bars, 140, Theme::default());
        assert_eq!(rows.len(), 2);
        for row in &rows {
            assert_eq!(row.len(), 140);
        }
        let top = text_of(&rows[0]);
        assert!(top.contains("Installing the ALab toolchain"), "{top}");
        assert!(top.contains("trust — extracting"), "{top}");
        assert!(top.contains("42%"), "{top}");
        assert!(top.ends_with("3 of 10 · 512 MB / 1.2 GB"), "{top}");
        let bottom = text_of(&rows[1]);
        assert!(bottom.contains("aterm update v0.48.0"), "{bottom}");
        assert!(bottom.contains("60%"), "{bottom}");
        // The meter is drawn as background cells: filled ones wear the accent.
        let c = chrome_band::band_colors(Theme::default());
        let l = layout(
            &bars.bars().next().unwrap().1.text,
            Some(512.0 / 1200.0),
            140,
        );
        let (mc, mw) = l.meter.unwrap();
        assert_eq!(rows[0][mc].bg, c.accent, "first meter cell is filled");
        assert_eq!(
            rows[0][mc + mw - 1].bg,
            c.meter_track,
            "last meter cell is track"
        );
        // Only the LAST bar row closes the chrome with the seam.
        assert_eq!(rows[0][0].underline, UnderlineStyle::None);
        assert_eq!(rows[1][0].underline, UnderlineStyle::Single);
        // The glyph is text-presentation, never an emoji.
        assert!(rows[0][MARGIN].text_presentation);
    }

    #[test]
    fn bytes_read_like_a_download_dialog() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(812_000), "812 KB");
        assert_eq!(fmt_bytes(512_000_000), "512 MB");
        assert_eq!(fmt_bytes(1_200_000_000), "1.2 GB");
    }

    /// THE STAGED BAR SAYS HOW THE UPDATE APPLIES, AND NEVER ASKS FOR A RESTART.
    ///
    /// This line read "build N — verified; restart aterm to apply" over the
    /// in-session overlap handoff — the mechanism that exists so that nobody
    /// restarts — until 2026-08-30. Every posture the App can compute has its own
    /// honest sentence; the one that warns (`HandoffDisabled`, for every reason
    /// the handoff can be unavailable) warns that nothing applies while a
    /// terminal is open — not that shells die, which the admission classifier
    /// never permits (see
    /// `the_disabled_handoff_line_says_what_the_admission_classifier_does`).
    #[test]
    fn a_staged_bar_says_how_the_update_applies_and_never_asks_for_a_restart() {
        use ApplyPosture as P;
        let staged = aterm_update::Progress::Staged {
            version: "0.67.0".into(),
            build: 7,
        };
        let now = t0();
        let mut cases: Vec<(P, Vec<&str>)> = vec![
            (
                P::Automatic,
                vec!["applies in place", "~2 min", "shells keep running"],
            ),
            (P::ManualByConfig, vec!["auto-apply is off", "Version menu"]),
            (
                P::VetoedByEnv {
                    var: "ATERM_NO_AUTO_APPLY",
                },
                vec!["$ATERM_NO_AUTO_APPLY", "Version menu"],
            ),
            (
                P::VetoedByEnv {
                    var: "ATERM_DEBUG_RELAUNCH_NUDGE",
                },
                vec!["$ATERM_DEBUG_RELAUNCH_NUDGE"],
            ),
            (
                P::ManualOnlyLatched { lapses: true },
                vec!["retries by itself", "Version menu"],
            ),
            (
                P::ManualOnlyLatched { lapses: false },
                vec!["until you apply it"],
            ),
        ];
        // Every reason the handoff can be unavailable gets the same warning, led
        // by its own cause — the thing the reader can act on.
        for why in HandoffUnavailable::ALL {
            cases.push((
                P::HandoffDisabled { why, veto: None },
                vec![
                    why.cause(),
                    "will NOT apply while any terminal is open",
                    "lands at the next launch (or once every terminal is closed)",
                ],
            ));
        }
        // With the automatic lane ALSO vetoed the fallback names the veto and
        // the manual affordance instead of the closed-terminals landing the
        // lane never attempts.
        cases.push((
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: Some(AutoApplyVeto::Config),
            },
            vec![
                "auto-apply is off",
                "will NOT apply while any terminal is open",
                "Version menu",
                "once every terminal is closed",
            ],
        ));
        cases.push((
            P::HandoffDisabled {
                why: HandoffUnavailable::OptedOut,
                veto: Some(AutoApplyVeto::Env {
                    var: "ATERM_NO_AUTO_APPLY",
                }),
            },
            vec!["$ATERM_NO_AUTO_APPLY is set", "Version menu"],
        ));
        for (posture, must_say) in cases {
            let mut bars = StatusBars::default();
            bars.update_progress(&staged, Some(posture), now);
            let bar = bars.bars().next().unwrap().1;
            assert!(
                bar.text.title.starts_with("aterm v0.67.0 is ready"),
                "{}",
                bar.text.title
            );
            assert_eq!(
                bar.text.title.contains(CLICK_TO_APPLY),
                !matches!(posture, P::HandoffDisabled { .. }),
                "the press affordance rides in the title wherever a press applies: {}",
                bar.text.title
            );
            assert_eq!(bar.staged_build, Some(7));
            assert_eq!(bar.text.detail, staged_detail(7, posture));
            assert!(
                bar.text.detail.starts_with("build 7 — verified; "),
                "{posture:?}: {}",
                bar.text.detail
            );
            for words in must_say {
                assert!(
                    bar.text.detail.contains(words),
                    "{posture:?} must say {words:?}: {}",
                    bar.text.detail
                );
            }
            // The one veto's NAME carries the word (`$ATERM_DEBUG_RELAUNCH_NUDGE`
            // is an identifier the line must name so the reader can unset it);
            // the sentence around it may not.
            let lower = bar
                .text
                .detail
                .replace("ATERM_DEBUG_RELAUNCH_NUDGE", "<seam>")
                .to_lowercase();
            assert!(
                !lower.contains("restart") && !lower.contains("relaunch"),
                "{posture:?} asks for a restart: {}",
                bar.text.detail
            );
            if matches!(posture, P::HandoffDisabled { .. }) {
                assert!(
                    !lower.contains("keep running") && !lower.contains("survive"),
                    "with the handoff off the line may neither promise an in-place \
                     landing nor threaten a kill the admission gate forbids: {}",
                    bar.text.detail
                );
            }
        }

        // A caller with no posture states only the manual affordance — true in
        // every posture the seamless lane serves, a promise in none.
        let mut bars = StatusBars::default();
        bars.update_progress(&staged, None, now);
        let detail = bars.bars().next().unwrap().1.text.detail.clone();
        assert!(
            detail.contains("Version menu")
                && !detail.contains("auto-apply is off")
                && !detail.contains("~2 min")
                && !detail.to_lowercase().contains("restart"),
            "{detail}"
        );

        // RESTATE: the App refines the line once the lane has actually armed —
        // the same bar; the hold re-anchored to the armed stretch.
        let fold_at = bars.bars().next().unwrap().1.fold_at;
        assert_eq!(
            fold_at,
            Some(now + HOLD_STAGED_MANUAL),
            "no posture yet: the short hold"
        );
        assert!(
            bars.restate_apply_posture(7, P::Automatic, now),
            "the live Staged bar for build 7 is rewritten"
        );
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.text.detail, staged_detail(7, P::Automatic));
        assert!(
            bar.text.title.ends_with(CLICK_TO_APPLY),
            "{}",
            bar.text.title
        );
        assert_eq!(
            bar.fold_at,
            Some(now + HOLD_STAGED_AUTOMATIC),
            "arming re-anchors the hold: the pull-down waits for the apply it promises"
        );
        assert!(
            !bars.restate_apply_posture(7, P::Automatic, now),
            "the same words again are not a repaint"
        );
        // STANDING DOWN FOR GOOD shortens the hold to the manual one and keeps
        // the affordance (a press still applies); the handoff going OFF strips
        // the affordance; arming again restores both.
        let title_before = bars.bars().next().unwrap().1.text.title.clone();
        assert!(bars.restate_apply_posture(7, P::ManualOnlyLatched { lapses: false }, now));
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(bar.fold_at, Some(now + HOLD_STAGED_MANUAL), "shortened");
        assert_eq!(bar.text.title, title_before);
        assert!(bars.restate_apply_posture(
            7,
            P::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None,
            },
            now,
        ));
        let bar = bars.bars().next().unwrap().1;
        assert!(
            !bar.text.title.contains(CLICK_TO_APPLY),
            "{}",
            bar.text.title
        );
        assert_eq!(bar.text.title, without_affordance(&title_before));
        assert!(bars.restate_apply_posture(7, P::Automatic, now));
        let bar = bars.bars().next().unwrap().1;
        assert_eq!(
            bar.text.title, title_before,
            "rebuilt from the exact suffix"
        );
        assert_eq!(
            bar.fold_at,
            Some(now + HOLD_STAGED_AUTOMATIC),
            "extended again"
        );
        assert!(
            !bars.restate_apply_posture(8, P::ManualByConfig, now),
            "another build's posture is not this bar's"
        );
        assert_eq!(
            bars.bars().next().unwrap().1.text.detail,
            staged_detail(7, P::Automatic)
        );
        assert!(
            !bars.settle(now + HOLD_OK),
            "an armed staged bar outlives the plain hold: the pull-down waits for the apply"
        );
        assert!(
            bars.settle(now + HOLD_STAGED_AUTOMATIC),
            "the bar folds on time"
        );
        assert!(
            !bars.restate_apply_posture(7, P::ManualOnlyLatched { lapses: true }, now),
            "nothing to restate once the bar has folded"
        );
        // A bar that is not a Staged bar is never restated.
        bars.update_progress(
            &aterm_update::Progress::Verifying {
                version: "0.67.0".into(),
            },
            None,
            now,
        );
        assert!(!bars.restate_apply_posture(7, P::Automatic, now));
    }

    /// THE PRESS AFFORDANCE IS ON GLASS AT ORDINARY WIDTHS (2026-09-07): it
    /// rides in the title, which the layout keeps whole; the detail is what a
    /// narrow window truncates — and where "click to apply now" used to be cut
    /// off at 100 columns.
    #[test]
    fn the_staged_rows_press_affordance_survives_ordinary_widths() {
        let now = Instant::now();
        let staged = aterm_update::Progress::Staged {
            version: "0.76.0".into(),
            build: 7,
        };
        let mut bars = StatusBars::default();
        bars.update_progress(&staged, Some(ApplyPosture::Automatic), now);
        for cols in [80usize, 100, 120] {
            let rows = paint_rows(&bars, cols, Theme::default());
            let text = text_of(&rows[0]);
            assert!(text.contains(CLICK_TO_APPLY), "{cols} cols: {text}");
        }
        // Where the handoff is off a press only opens the details: no affordance.
        let mut off = StatusBars::default();
        off.update_progress(
            &staged,
            Some(ApplyPosture::HandoffDisabled {
                why: HandoffUnavailable::Headless,
                veto: None,
            }),
            now,
        );
        let text = text_of(&paint_rows(&off, 120, Theme::default())[0]);
        assert!(!text.contains(CLICK_TO_APPLY), "{text}");
    }

    /// THE LANDING LINE ONLY CLAIMS WHAT THE LANE DID (2026-09-09): the
    /// seamless lane carried the running shells, the cold lane carried none.
    #[test]
    fn the_landing_row_claims_the_shells_only_where_they_were_kept() {
        let now = Instant::now();
        let mut kept = StatusBars::default();
        kept.update_landed("0.79.0", 7, true, now);
        let bar = kept.bars().next().unwrap().1;
        assert_eq!(bar.text.title, "Updated \u{2014} now on v0.79.0");
        assert_eq!(bar.text.detail, "your shells kept running");
        let mut cold = StatusBars::default();
        cold.update_landed("0.79.0", 7, false, now);
        let bar = cold.bars().next().unwrap().1;
        assert_eq!(bar.text.detail, "the new build is running");
        assert!(
            !bar.text.detail.contains("shells"),
            "no promise about shells this lane never had"
        );
    }

    /// A HANDOFF FREEZE SUSPENDS THE HOLDS, NOT THE CAPS (2026-09-08): the row
    /// count is frozen for Commit, so a hold that elapses mid-freeze must not
    /// leave the committed row a hole — but a successor whose Commit never
    /// arrives still gets its carried row back at `HANDOFF_STALE`, which is the
    /// only thing that ends it (`incoming_handoff_pending` is cleared at Commit
    /// and nowhere else).
    #[test]
    fn a_frozen_settle_suspends_holds_and_still_honours_the_staleness_cap() {
        let now = Instant::now();
        // A HOLD: kept through the freeze, retired once it is over.
        let mut held = StatusBars::default();
        held.update_outcome(
            '\u{2726}',
            "Updated",
            "your shells kept running",
            Tone::Success,
            now,
        );
        assert!(
            !held.settle_with(now + HOLD_OK, false),
            "the freeze keeps the words"
        );
        assert_eq!(held.rows(), 1);
        assert!(held.settle_with(now + HOLD_OK, true));
        assert_eq!(held.rows(), 0);
        // A CAP: retired even while frozen — the successor whose Commit never came.
        let carried = vec![crate::session_store::CarriedBar {
            lane: "update".into(),
            glyph: '\u{2191}',
            title: "Installing aterm v0.76.0".into(),
            detail: "your shells are safe".into(),
            stats: String::new(),
            tone: "info".into(),
            fill_permille: None,
        }];
        let mut stuck = StatusBars::from_handoff_carry(&carried, Some("0.76.0"), now);
        assert_eq!(stuck.rows(), 1);
        assert!(!stuck.settle_with(now + HANDOFF_STALE - Duration::from_millis(1), false));
        assert_eq!(
            stuck.deadline_with(false),
            Some(now + HANDOFF_STALE),
            "and THAT is the instant the loop arms while frozen — never the hold \
             it would decline to act on"
        );
        assert!(
            stuck.settle_with(now + HANDOFF_STALE, false),
            "the cap runs even under the freeze"
        );
        assert_eq!(stuck.rows(), 0);
        // The armed deadline and the settle agree in both directions: a bar
        // whose HOLD has already passed arms nothing while frozen (an armed
        // past instant is a wake that does nothing and re-arms itself).
        let mut held_past = StatusBars::default();
        held_past.update_outcome('\u{2726}', "Updated", "x", Tone::Success, now);
        assert_eq!(held_past.deadline_with(true), Some(now + HOLD_OK));
        assert_eq!(held_past.deadline_with(false), None);
    }

    /// A TRANSIENT OUTCOME DOES NOT LOSE THE READY ROW (2026-09-08): the outcome
    /// covers the Staged bar for its hold and the ready row returns when it
    /// folds — within the ready row's own hold, and never after a newer report.
    #[test]
    fn the_staged_row_returns_when_an_outcome_over_it_folds() {
        let now = Instant::now();
        let staged = aterm_update::Progress::Staged {
            version: "0.76.0".into(),
            build: 7,
        };
        let mut bars = StatusBars::default();
        bars.update_progress(&staged, Some(ApplyPosture::Automatic), now);
        bars.update_outcome(
            '\u{2191}',
            "Update waiting",
            "unsaved text",
            Tone::Info,
            now,
        );
        assert!(
            !bars.update_bar_is_staged(),
            "the outcome is up, not the ready row"
        );
        assert!(!bars.settle(now + HOLD_OK - Duration::from_millis(1)));
        assert!(
            bars.settle(now + HOLD_OK),
            "the glass changed: the ready row is back"
        );
        assert_eq!(bars.rows(), 1, "in place — no re-grid");
        assert!(bars.update_bar_is_staged());
        assert!(
            bars.bars()
                .next()
                .unwrap()
                .1
                .text
                .title
                .ends_with(CLICK_TO_APPLY),
            "with its affordance"
        );
        assert_eq!(bars.ledger().last().unwrap().title, "Update waiting");
        // An outcome that means the stage is on its way in gives nothing back.
        let mut installed = StatusBars::default();
        installed.update_progress(&staged, Some(ApplyPosture::Automatic), now);
        installed.update_outcome(
            '\u{2191}',
            "Update installed",
            "activating",
            Tone::Info,
            now,
        );
        installed.forget_staged_behind_outcome();
        assert!(installed.settle(now + HOLD_OK));
        assert_eq!(installed.rows(), 0);
        // A LANE THAT CHANGES ITS MIND while the outcome covers the ready row
        // hands back the NEW promise, not the old one.
        let mut turned = StatusBars::default();
        turned.update_progress(&staged, Some(ApplyPosture::Automatic), now);
        turned.update_outcome('\u{2191}', "Update waiting", "x", Tone::Info, now);
        assert!(
            !turned.restate_apply_posture(7, ApplyPosture::ManualByConfig, now),
            "nothing on glass changed: the outcome row is up"
        );
        assert!(turned.settle(now + HOLD_OK));
        let back = turned.bars().next().unwrap().1;
        assert_eq!(
            back.text.detail,
            staged_detail(7, ApplyPosture::ManualByConfig)
        );
        assert_eq!(
            back.fold_at,
            Some(now + HOLD_STAGED_MANUAL),
            "and the hold the new posture asks for"
        );
        // …but not past its own hold.
        let mut late = StatusBars::default();
        late.update_progress(&staged, Some(ApplyPosture::ManualByConfig), now);
        late.update_outcome('\u{2191}', "Update waiting", "x", Tone::Info, now);
        assert!(late.settle(now + HOLD_STAGED_MANUAL));
        assert_eq!(late.rows(), 0, "the ready row's minute had run out");
        // …and never over a newer report on the lane.
        let mut newer = StatusBars::default();
        newer.update_progress(&staged, Some(ApplyPosture::Automatic), now);
        newer.update_outcome('\u{2191}', "Update waiting", "x", Tone::Info, now);
        newer.update_progress(
            &aterm_update::Progress::Verifying {
                version: "0.77.0".into(),
            },
            None,
            now,
        );
        assert!(!newer.settle(now + HOLD_OK));
        assert!(
            newer
                .bars()
                .next()
                .unwrap()
                .1
                .text
                .detail
                .contains("verifying"),
            "the newer report stands"
        );
    }

    /// THE DISABLED-HANDOFF LINE SAYS WHAT THE ADMISSION CLASSIFIER DOES.
    ///
    /// With `$ATERM_NO_SEAMLESS_UPDATE` set the apply is not "cold instead of
    /// seamless": `seamless_capable` is false, and
    /// `native_update_admission::classify` REFUSES the apply while any PTY is
    /// live — only an exact zero-PTY process may take the cold lane. The bar said
    /// "applies by cold re-exec; your shells will NOT survive" — a kill the
    /// classifier never permits — until 2026-08-30. The sentence and the gate are
    /// pinned together here so they cannot drift apart again.
    #[test]
    fn the_disabled_handoff_line_says_what_the_admission_classifier_does() {
        use crate::native_update_admission::{
            AdmissionBlock, AdmissionDecision, AdmissionFacts, ApplyLane, classify,
        };
        let opted_out = |live_ptys: usize| AdmissionFacts {
            staged_verified: true,
            // `overlap_available && !live.is_empty()` with the opt-out set.
            seamless_capable: false,
            native_state_certified: true,
            live_ptys,
            foreground_jobs: 0,
            unknown_foregrounds: 0,
        };
        assert_eq!(
            classify(opted_out(1)),
            AdmissionDecision::Block(AdmissionBlock::LivePtysNeedSeamless),
            "one open terminal: the apply is refused and nothing dies"
        );
        assert_eq!(
            classify(opted_out(0)),
            AdmissionDecision::Apply(ApplyLane::Cold),
            "no terminal at all: the cold re-exec"
        );
        // `seamless_capable` is one predicate's `is_none()`
        // (`App::seamless_handoff_unavailable`), so every reason it can name gets
        // this same sentence, led by its own cause.
        for why in HandoffUnavailable::ALL {
            let line = staged_detail(7, ApplyPosture::HandoffDisabled { why, veto: None });
            assert!(
                line.starts_with(&format!(
                    "build 7 — verified; {} — the handoff is off: ",
                    why.cause()
                )) && line.contains("will NOT apply while any terminal is open")
                    && line.contains("lands at the next launch (or once every terminal is closed)"),
                "{line}"
            );
            let lower = line.to_lowercase();
            assert!(
                !lower.contains("survive")
                    && !lower.contains("keep running")
                    && !lower.contains("cold re-exec")
                    && !lower.contains("restart")
                    && !lower.contains("relaunch")
                    && !lower.contains("version menu"),
                "the line may neither threaten a kill the gate forbids nor promise a \
                 landing (or a click) it refuses: {line}"
            );
        }
        assert_eq!(
            staged_detail(
                7,
                ApplyPosture::HandoffDisabled {
                    why: HandoffUnavailable::OptedOut,
                    veto: None
                }
            ),
            "build 7 — verified; $ATERM_NO_SEAMLESS_UPDATE is set — the handoff is off: \
             it will NOT apply while any terminal is open; \
             it lands at the next launch (or once every terminal is closed)"
        );
        assert!(
            staged_detail(
                7,
                ApplyPosture::HandoffDisabled {
                    why: HandoffUnavailable::ExplicitControlSock,
                    veto: None
                }
            )
            .starts_with(
                "build 7 — verified; $ATERM_CONTROL_SOCK names an explicit socket path \
                 (--control-sock) — "
            )
        );
        // The vetoed twin: names the veto, points at the manual affordance
        // (qualified by the zero-terminal admission), keeps the true
        // next-launch fallback — and drops the closed-terminals promise of the
        // armed lane.
        let vetoed = staged_detail(
            7,
            ApplyPosture::HandoffDisabled {
                why: HandoffUnavailable::OptedOut,
                veto: Some(AutoApplyVeto::Config),
            },
        );
        assert_eq!(
            vetoed,
            format!(
                "build 7 — verified; $ATERM_NO_SEAMLESS_UPDATE is set — the handoff is off: \
                 it will NOT apply while any terminal is open; auto-apply is off — \
                 {APPLY_FROM_MENU} once every terminal is closed, or it lands at the next launch"
            )
        );
        assert!(
            !vetoed.contains("(or once every terminal is closed)"),
            "the vetoed lane may not promise the automatic closed-terminals landing: {vetoed}"
        );
    }

    /// NOTHING IN THE UPDATE LANE ASKS FOR A RESTART OR A RELAUNCH (owner ruling,
    /// 2026-08-30). The mechanism is the in-session overlap handoff; every sentence
    /// a person can read on the way to it — this bar, the Version-menu / palette
    /// row, the trouble tails, the admission refusals, the close-preflight
    /// blockers, the installed-on-disk outcomes, the status pill — is pinned here
    /// ONCE, so the words cannot drift back one surface at a time. "Restart" and
    /// "relaunch" stay reserved for the things that genuinely need one
    /// (`columns`/`lines`, the GPU renderer choice, the Windows backdrop, Rosetta),
    /// none of which is an update.
    #[test]
    fn no_update_surface_asks_for_a_restart_or_a_relaunch() {
        use crate::native_update_admission::{AdmissionBlock, AdmissionFacts};
        use crate::update_apply_trouble::{ApplyRetry, ApplyTrouble};
        use ApplyPosture as P;

        let mut surfaces: Vec<(&str, String)> = Vec::new();
        let mut postures = vec![
            P::Automatic,
            P::ManualByConfig,
            P::VetoedByEnv {
                var: "ATERM_NO_AUTO_APPLY",
            },
            P::VetoedByEnv {
                var: "ATERM_DEBUG_RELAUNCH_NUDGE",
            },
            P::ManualOnlyLatched { lapses: true },
            P::ManualOnlyLatched { lapses: false },
        ];
        postures.extend(HandoffUnavailable::ALL.map(|why| P::HandoffDisabled { why, veto: None }));
        postures.push(P::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: Some(AutoApplyVeto::Config),
        });
        postures.push(P::HandoffDisabled {
            why: HandoffUnavailable::Headless,
            veto: Some(AutoApplyVeto::Env {
                var: "ATERM_NO_AUTO_APPLY",
            }),
        });
        for posture in postures {
            surfaces.push(("status bar", staged_detail(7, posture)));
        }
        surfaces.push(("status bar (no posture)", staged_detail_unknown(7)));
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
        surfaces.push((
            "close-preflight blocker",
            crate::App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string(),
        ));
        surfaces.push((
            "close-preflight blocker",
            crate::App::RESTORE_IN_FLIGHT_BLOCKS_APPLY.to_string(),
        ));
        surfaces.push((
            "installed-on-disk outcome",
            crate::App::INSTALLED_ACTIVATES_IN_PLACE.to_string(),
        ));
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
        surfaces.push((
            "update row (installed)",
            format!(
                "{} — {}",
                crate::app_update_screen::UPDATE_INSTALLED_TITLE,
                crate::app_update_screen::UPDATE_INSTALLED_DETAIL
            ),
        ));
        // The update lane's other row words (2026-09-07): every moment the
        // floating cards used to carry, now on the bar.
        let now = std::time::Instant::now();
        let mut lane = StatusBars::default();
        assert!(
            lane.update_installing_added("9.9.9", now),
            "PRECONDITION: the installing row is up, so its words are read"
        );
        surfaces.push((
            "update row (installing)",
            lane.bars()
                .next()
                .map(|(_, b)| format!("{} — {}", b.text.title, b.text.detail))
                .expect("the installing row is up"),
        ));
        lane.update_finishing("9.9.9", now);
        surfaces.push((
            "update row (finishing)",
            lane.bars()
                .next()
                .map(|(_, b)| format!("{} — {}", b.text.title, b.text.detail))
                .unwrap_or_default(),
        ));
        lane.update_landed("9.9.9", 7, true, now);
        surfaces.push((
            "update row (landed)",
            lane.bars()
                .next()
                .map(|(_, b)| format!("{} — {}", b.text.title, b.text.detail))
                .unwrap_or_default(),
        ));
        for (title, detail) in [
            ("Update waiting", "see Version menu"),
            (
                "Update waiting",
                "retries on its own, or use the Version menu",
            ),
            ("Update delayed", "retries on its own"),
            ("Update paused", "see Version menu"),
            ("Update stopped safely", "see Settings ▸ Software Update"),
        ] {
            surfaces.push(("update row (outcome)", format!("{title} — {detail}")));
        }
        assert!(surfaces.len() >= 25, "the guard covers the whole lane");
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

    /// AND THE SOURCE SAYS IT NOWHERE. The guard above checks the VALUES of an
    /// enumerated list, so a surface nobody added to the list, or a sentence its
    /// three words cannot see — "quit and open aterm again", "activates it at the
    /// next launch" — passes it. This one reads the SHIPPING lines of every
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
            "status_bars.rs",
            "menu.rs",
            "palette.rs",
            "app_palette.rs",
            "notice.rs",
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
