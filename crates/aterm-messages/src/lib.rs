// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-messages` — one band, one model, one log.
//!
//! The platform-neutral engine behind aterm's unified message system
//! (docs/DESIGN-unified-messages-2026-09-21.md §1): the message model, the
//! glass policy (which rows are on the band, in what order, for how long), the
//! single-row width law, the durable tagged log ring and its line codec, the
//! wire grammars and the seamless-handoff carry. It emits **layouts in
//! cells** ([`glass::Presentation`]); the host paints them, appends the log
//! lines it drains, and performs the plain-data [`model::Intent`]s a press
//! returns (`ATERM_DESIGN` §2.2: the engine requests, the frontend performs).
//!
//! # Contract
//!
//! * **Clockless.** Every method that reads time takes `now: Instant`
//!   ([`Instant`] is `aterm_time::Instant` — exactly `std::time::Instant` on
//!   native). A crate-local test greps these sources for a clock read and
//!   asserts zero hits, so the engine can be driven deterministically and
//!   on wasm.
//! * **No platform.** No `std::fs`, no threads, no sockets, no `RenderCell`,
//!   no theme: the host owns the file, the cells and the colours.
//! * **Bounded.** [`MAX_LIVE`] unretired messages, [`LOG_CAP`] records in the
//!   ring, [`PENDING_PERSIST_CAP`] undrained persist lines, every string
//!   sanitized and capped at ingress — a hostile reporter cannot grow the
//!   process or forge a row.
//! * **Constants, not knobs** (D8): every hold, cap and floor is a `pub const`
//!   here, measured against the status bars it replaces.
//!
//! # Indicators and motion
//!
//! A row's INDICATOR is its meter's state (design ruling 139, the merge's
//! M4): a fill draws the bar, [`Meter::busy`] the comet and the spinner, and
//! neither draws nothing. The METER IS THE ROW (ruling 55, kept by ruling
//! 136): a metered row's meter is `(0, cols, fill)` and a busy row's track
//! `(0, cols)`, laid under every word and mapped by the host onto the
//! window's pixels, gutters included — they cost the words no room and are
//! never shed ([`glass`]). The MOTION is the engine's (ruling 140): the host
//! reads one frame at a time through [`MessageCenter::motion`] and wakes at
//! [`MessageCenter::motion_deadline`], the only clock, on one 33 ms grid; a
//! frame hands it FRACTIONS of the row ([`animate::Surface`]) and the
//! spinner's glyph, never a pixel. A frame never enters the center, so it
//! never bumps a revision, never re-grids and never arms
//! [`MessageCenter::deadline`]; [`MessageCenter::busy_on_glass`] says whether
//! a committed row is busy.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
// Test code is exempt from the pedantic bar, as in aterm-core; production
// code is still held to all + pedantic.
#![cfg_attr(test, allow(clippy::all, clippy::pedantic))]
#![allow(
    clippy::module_name_repetitions,
    reason = "the design names the types by their home (LogRecord/LogLine in log.rs, MessageCenter in center.rs); the qualified names read at every call site"
)]

/// A span of time — `core::time::Duration` on every target.
pub use aterm_time::Duration;
/// The monotonic clock the host injects — `std::time::Instant` on native.
pub use aterm_time::Instant;

pub mod animate;
pub mod carry;
pub mod center;
pub mod glass;
pub mod log;
pub mod model;
pub mod progress;
pub mod text;
pub mod wire;
pub mod words;

#[cfg(test)]
mod adversarial;

pub use animate::{Anim, BandMotion, FineTone, Look, Pace, ROW, RowMotion, Stop, Surface, Tone};
pub use carry::{CarriedMessage, Carry};
pub use center::{Echo, EchoKind, Live, MessageCenter, Outcome, PostOutcome, Posted, Settled};
pub use glass::{CapsuleLayout, CapsuleRole, Hit, Links, Presentation, RowKind, RowLayout};
pub use log::{CodecError, LogLine, LogRecord, LogState, MessageLog, Retired};
pub use model::{
    ActionIndex, Amount, Decision, Glyph, Hold, Intent, Load, Message, MessageId, Meter, Origin,
    Restatement, Severity, Tag, TagError, Unit, WallStamp, tags,
};
pub use progress::{Eta, ProgressTrack};

// ---------------------------------------------------------------------------
// Constants (D8: constants, not knobs).
// ---------------------------------------------------------------------------

/// Visible band rows, including the overflow row (D1).
pub const MAX_ROWS: u16 = 3;
/// Unretired messages (glass + queue). Past it a post evicts.
pub const MAX_LIVE: usize = 64;
/// In-memory log ring; also the on-disk retention.
pub const LOG_CAP: usize = 512;
/// Undrained persist lines. Past it the oldest is dropped and counted.
pub const PENDING_PERSIST_CAP: usize = 1024;
/// Title cap in chars, after sanitize; one line.
pub const TITLE_CAP: usize = 120;
/// Chars per detail line, after sanitize.
pub const DETAIL_LINE_CAP: usize = 240;
/// Detail lines; `detail[0]` is the band's excerpt source.
pub const DETAIL_LINES_CAP: usize = 24;
/// Supersede-key chars.
pub const KEY_CAP: usize = 48;
/// Meter stats chars (volatile: never persisted, never spoken).
pub const STATS_CAP: usize = 40;
/// Authored intents per message; `Details ›` is implicit.
pub const MAX_ACTIONS: usize = 2;
/// Cells each side of a row (`status_bars.rs:2365-2373`).
pub const MARGIN: usize = 1;
/// The glyph's column.
pub const GLYPH_COL: usize = 1;
/// The title's first column.
pub const TITLE_COL: usize = 3;
/// Cells clear before the first capsule.
pub const BEFORE_CAPSULES: usize = 2;
/// Cells between capsules.
pub const CAPSULE_GAP: usize = 1;
/// Below this many cells the excerpt is DROPPED, never stubbed
/// (`status_bars.rs:143-147`).
pub const DETAIL_FLOOR: usize = 20;
/// The title elides no further; capsules never drop.
pub const TITLE_MIN: usize = 12;
/// The joint between a detail's pieces, the one every words builder writes;
/// [`text::shape_detail`] sheds pieces at it (`status_bars.rs:150-152`).
pub const PIECE_SEP: &str = " \u{00b7} ";

/// A Success row's default hold (`HOLD_OK`, `status_bars.rs:69`).
pub const HOLD_SUCCESS: Duration = Duration::from_secs(8);
/// An Info row's default hold (`HOLD_NOTICE`, `status_bars.rs:78`).
pub const HOLD_INFO: Duration = Duration::from_secs(30);
/// A Warn row's default hold (`status_bars.rs:74`).
pub const HOLD_WARN: Duration = Duration::from_secs(45);
/// An Error row's default hold — an error earns a longer read than a warning.
pub const HOLD_ERROR: Duration = Duration::from_secs(60);
// `HOLD_STAGED_AUTOMATIC` (10 min) RETIRED 2026-09-23 (design ruling 57,
// kept by ruling 143): the automatic staged row is `Hold::Live` and BUSY
// under the host's `update_words::STAGED_AUTOMATIC_STALE`, which must track
// the apply ladder's `LANDS_WITHIN` — a host constant the engine cannot see.
/// A staged update that applies only on a press (`status_bars.rs:96`).
pub const HOLD_STAGED_MANUAL: Duration = Duration::from_secs(60);
/// A decision row (the retired toast's `ADMIN_STEP_TTL`, notice.rs:146).
pub const HOLD_ASK: Duration = Duration::from_mins(10);
/// A route row after Open Settings: a terse restate of the question's row
/// (the owner's attention rule, 2026-09-23 — the design's ruling 46; it was
/// the retired card's 90 s `OPENED_SETTINGS_TTL`). The route stays behind
/// Details and on Settings ▸ Messages.
pub const HOLD_ROUTE: Duration = Duration::from_secs(15);
/// A failed gesture (was the 5.4 s pill).
pub const HOLD_GESTURE: Duration = Duration::from_secs(12);
/// A tailed toolchain meter folds after this much silence (`status_bars.rs:107`).
pub const STALE_TAILED: Duration = Duration::from_secs(30);
/// An announcement-only toolchain row's cap (`status_bars.rs:111`).
pub const STALE_ANNOUNCE: Duration = Duration::from_mins(20);
/// A live update row's cap (`status_bars.rs:128`).
pub const STALE_UPDATE: Duration = Duration::from_mins(15);
/// A carried row's cap across the seamless handoff (`status_bars.rs:101`).
pub const STALE_HANDOFF: Duration = Duration::from_secs(125);
/// D1 hysteresis: grow now, shrink after this much quiet.
pub const SHRINK_QUIET: Duration = Duration::from_millis(1500);
/// A queued row folds unseen after `max(hold, this)`.
pub const OVERFLOW_PATIENCE_MIN: Duration = Duration::from_secs(60);

// ---- Attention and motion (design §10.4.1) --------------------------------

/// A row that would appear and fold inside this costs two re-grids for
/// nothing: a progress row posted with `reveal_after(PROGRESS_GRACE)` reaches
/// the glass only if its work lasts this long.
pub const PROGRESS_GRACE: Duration = Duration::from_secs(2);
/// The one frame grid every motion phase is read on — the host's 30 fps cap.
pub const ANIM_FRAME: Duration = Duration::from_millis(33);
/// One comet's breathing crossing of the WHOLE ROW (design ruling 138): its
/// head enters at the window's left edge with the comet wholly outside, and
/// the crossing ends as its tail clears the right edge — [`COMET_PERMILLE`]
/// more than the row. The next comet enters as this one leaves, so the row is
/// never empty for longer than a frame. Three seconds, main's sweep (24
/// frames of 125 ms): 0.4 rows a second reads as calm at 80 columns (13
/// cells a second) and at 200 (33), where the pill's 2.4 s over twenty cells
/// was a scurry once the track became the window.
pub const COMET_PERIOD: Duration = Duration::from_millis(3000);
/// The comet's length in permille of the ROW (main's `COMET_PERMILLE`: a
/// fifth of the window), its soft leading edge included: a tail that rises
/// as the square of its length toward the hot head, then the leading edge
/// falling back to the track over [`COMET_LEAD_PERMILLE`]. No straight edge
/// anywhere — the ends of the row are the window's own edges.
pub const COMET_PERMILLE: u32 = 200;
/// The comet's soft LEADING edge, in permille of the row: the head's tone
/// falls to the track over this, ahead of the head. Four percent, not two:
/// on whole cells a 2 % lead is narrower than one cell plus its gutter, so
/// the head read as a straight edge — a 172-of-255 step between neighbouring
/// cells at 114 columns on a 2000 px window, 192 at 80 on 1000 px. At 4 % the
/// worst step on those windows is 104 and 138 (the merge's build stage,
/// 2026-09-24; the host's `the_comet_sweeps_the_window_edge_to_edge` pins it).
pub const COMET_LEAD_PERMILLE: u32 = 40;
/// The comet's hot head, lifted toward the glint (0–255).
pub const COMET_HEAD_LIFT: u8 = 150;
/// The spinner a busy row's glyph cell cycles through (main's braille
/// `dots`, the set Codex shows while it works): one cell in every face.
/// Paint, never a message glyph.
pub const SPINNER: [char; 10] = [
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280f}',
];
/// Frames of [`ANIM_FRAME`] per spinner step: 4 × 33 ms = 132 ms, main's
/// 125 ms cadence read on the one grid (design ruling 140).
pub const SPIN_FRAMES: u32 = 4;
/// The glint's cycle over a determinate bar: a travel, then rest.
pub const GLINT_PERIOD: Duration = Duration::from_millis(4000);
/// The glint's travel along the FILL; the rest of [`GLINT_PERIOD`] asks no
/// frames.
pub const GLINT_TRAVEL: Duration = Duration::from_millis(1600);
/// The first glint, after the motion epoch.
pub const GLINT_DELAY: Duration = Duration::from_millis(1200);
/// The glint's radius in permille of the row (six cells of support at 200
/// columns, two and a half at 80).
pub const GLINT_RADIUS_PERMILLE: u32 = 30;
/// The glint's peak lift (0–255).
pub const GLINT_PEAK: u8 = 190;
/// A data jump in a determinate fill eases out (cubic) over this.
pub const FILL_GLIDE: Duration = Duration::from_millis(180);
/// A Complete echo's fill to 100 %.
pub const ECHO_FILL: Duration = Duration::from_millis(200);
/// A Complete echo's bloom.
pub const ECHO_GLOW: Duration = Duration::from_millis(400);
/// An echo's fade.
pub const ECHO_FADE: Duration = Duration::from_millis(250);
/// A Fault echo's warn flash.
pub const ECHO_FAULT_FLASH: Duration = Duration::from_millis(350);
/// Heavy-load words show after the declared load lasted this long.
pub const LOAD_AFTER: Duration = Duration::from_secs(5);
/// A byte stream that has not advanced for this long reads "stalled".
pub const STALL_AFTER: Duration = Duration::from_secs(10);
/// A latched ETA whose completion instant has passed with nothing new stands
/// at `<5 s left` for at most this long — or a fifth of the estimate it latched,
/// when that is longer — and then goes blank: an estimate that ran out is not
/// a promise of five more seconds (review 2026-09-24).
pub const ETA_OVERDUE_MIN: Duration = Duration::from_secs(5);
/// A busy row shows its elapsed time from here on.
pub const ELAPSED_AFTER: Duration = Duration::from_secs(10);
/// The rate is the secant across at most this window.
pub const RATE_WINDOW: Duration = Duration::from_secs(20);
/// …and at least this span.
pub const RATE_MIN_SPAN: Duration = Duration::from_secs(3);
/// Samples closer than this share one slot of the ring.
pub const SAMPLE_MIN_GAP: Duration = Duration::from_millis(500);
/// The sample ring's cap.
pub const SAMPLES_CAP: usize = 48;
/// Projections older than this do not vote on the latch.
pub const STABLE_SPAN: Duration = Duration::from_secs(4);
/// The voting projections must span at least this long.
pub const STABLE_MIN: Duration = Duration::from_secs(2);
/// The projection ring's cap.
pub const PROJECTIONS_CAP: usize = 16;
/// The percent slot: `" 100%"`.
pub const PCT_W: usize = 5;
/// The elapsed slot: a clock, `"0:28"` … `"59:59"`, then `"1:02 h"`.
pub const ELAPSED_W: usize = 6;
/// The ETA slot: `"~59 min left"` or `"stalled"` — a remaining time says
/// `left`, so it never reads as the elapsed clock (review 2026-09-24).
pub const ETA_W: usize = 12;
/// The ETA slot's SHORT form, where the long one does not fit: `"59m left"`,
/// `"<5s left"` or `"stalled"` — the same numbers, changing at the same
/// instants, still saying `left` (at 80 columns a first run's row under disk
/// load has room for this and not for the long form).
pub const ETA_SHORT_W: usize = 8;
/// The ETA slot's word for a byte stream that stopped (painted in warn).
pub const STALLED_WORD: &str = "stalled";
/// A Fault echo's word in the row's time slot (painted in warn, like a
/// stall): the failure is said, not only tinted.
pub const FAILED_WORD: &str = "failed";
/// A Complete echo's word in the row's time slot (painted in the ink its ✓
/// wears): the payoff of a wait says it finished, where the words beside the
/// check mark still name the work (`✓ Downloading aterm v0.91.0 … 100%
/// done`, review round 3, 2026-09-24).
pub const DONE_WORD: &str = "done";
const _: () = assert!(
    STALLED_WORD.len() <= ETA_SHORT_W
        && ETA_SHORT_W < ETA_W
        && FAILED_WORD.len() <= ELAPSED_W
        && FAILED_WORD.len() <= ETA_SHORT_W
        && DONE_WORD.len() <= ELAPSED_W
        && DONE_WORD.len() <= ETA_SHORT_W
        && ELAPSED_W <= ETA_SHORT_W
);

#[cfg(test)]
mod tests {
    /// Every source file of this crate, by name, as compiled — no `std::fs`
    /// walk, so the fence holds on a target with no filesystem too.
    const SOURCES: &[(&str, &str)] = &[
        ("lib.rs", include_str!("lib.rs")),
        ("model.rs", include_str!("model.rs")),
        ("center.rs", include_str!("center.rs")),
        ("glass.rs", include_str!("glass.rs")),
        ("log.rs", include_str!("log.rs")),
        ("text.rs", include_str!("text.rs")),
        ("wire.rs", include_str!("wire.rs")),
        ("words.rs", include_str!("words.rs")),
        ("carry.rs", include_str!("carry.rs")),
        ("progress.rs", include_str!("progress.rs")),
        ("animate.rs", include_str!("animate.rs")),
    ];

    /// Invariant 1: the engine never samples a clock. Family C's C3 walks only
    /// core/src/terminal (tools/grep_guard.sh:608), so the crate carries its
    /// own fence: `Instant::now()` / `SystemTime::now()` count == 0 in the
    /// SHIPPED code — everything before each file's `#[cfg(test)]` module
    /// (tests mint a base instant to drive the engine from, as the status
    /// bars' tests do; the grep guard's own scan strips test blocks the
    /// same way).
    #[test]
    fn no_wall_clock_in_this_crate() {
        // Assembled so this test's own text is not a hit.
        let needle = ["::", "now()"].concat();
        let marker = ["#[cfg(", "test)]"].concat();
        let mut hits = Vec::new();
        for (name, src) in SOURCES {
            let shipped = src.split(&marker).next().unwrap_or("");
            assert!(shipped.len() > 100, "{name}: the shipped half is not empty");
            for (i, line) in shipped.lines().enumerate() {
                if line.contains(&needle) {
                    hits.push(format!("{name}:{}: {}", i + 1, line.trim()));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "clock reads in the engine:\n{}",
            hits.join("\n")
        );
        // The fence is complete by construction: every `pub mod x;` of this
        // file names a source in SOURCES, so a new shipped module cannot
        // arrive unscanned (the `#[cfg(test)]` modules are not shipped).
        let modules: Vec<&str> = include_str!("lib.rs")
            .lines()
            .filter_map(|l| l.strip_prefix("pub mod ")?.strip_suffix(';'))
            .collect();
        assert_eq!(modules.len(), 10, "{modules:?}");
        for m in &modules {
            let file = format!("{m}.rs");
            assert!(
                SOURCES.iter().any(|(name, _)| *name == file),
                "{file} is a shipped module outside the clock fence"
            );
        }
        assert_eq!(
            SOURCES.len(),
            modules.len() + 1,
            "lib.rs plus every shipped module, nothing else"
        );
    }

    /// The constants are the status bars' numbers, not new ones.
    #[test]
    fn holds_are_the_measured_status_bar_values() {
        use super::*;
        assert_eq!(HOLD_SUCCESS, Duration::from_secs(8));
        assert_eq!(HOLD_INFO, Duration::from_secs(30));
        assert_eq!(HOLD_WARN, Duration::from_secs(45));
        assert!(HOLD_ERROR > HOLD_WARN, "an error earns a longer read");
        assert_eq!(STALE_HANDOFF, Duration::from_secs(125));
        assert!(OVERFLOW_PATIENCE_MIN >= HOLD_INFO);
        assert!(TITLE_MIN < TITLE_CAP && DETAIL_FLOOR < DETAIL_LINE_CAP);
    }
}

#[cfg(test)]
mod review_tests;

#[cfg(test)]
mod attention_tests;
