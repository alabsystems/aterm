// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-messages`: one band, one model, one log.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
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
pub(crate) mod carry;
pub(crate) mod center;
pub mod chrome;
pub(crate) mod digest;
pub mod drive;
pub mod exceptions;
pub(crate) mod glass;
pub mod ink;
pub mod log;
pub(crate) mod model;
pub mod need;
pub mod page;
pub mod paint;
pub mod palette;
pub mod presence;
pub mod progress;
pub mod text;
pub mod tokens;
pub mod waits;
pub mod wire;
pub mod words;

#[cfg(test)]
mod adversarial;
#[cfg(test)]
mod tokens_tests;

pub use animate::{Anim, BandMotion, FineTone, Look, Pace, ROW, RowMotion, Stop, Surface, Tone};
pub use carry::{CarriedMessage, Carry};
pub use center::{
    CatchUp, DigestSync, Echo, EchoKind, Live, MessageCenter, Outcome, PostOutcome, Posted, Settled,
};
pub use digest::{DigestEntry, DigestView, RECENT_FILED, ReportScope, report as exceptions_report};
pub use glass::{CapsuleLayout, CapsuleRole, Hit, Links, Presentation, RowKind, RowLayout};
pub use log::{CodecError, LogLine, LogRecord, LogState, MessageLog, Retired, Shelf};
pub use model::{
    ActionIndex, Amount, Decision, Glyph, Hold, Intent, LINK_URL_CAP, Load, Loads, Message,
    MessageId, Meter, Origin, Restatement, Severity, Tag, TagError, Unit, UpgradeWord, WallStamp,
    link_host, tags,
};
pub use progress::{Eta, ProgressTrack, Waiter};
pub use waits::{SessionWait, WaitEnd, WaitKind, WaitSample, WaitStep};
pub use wire::{
    Applied, DoneHow, DoneRequest, LaterRequest, NoticeRequest, Paint, Press, ProgressRequest,
    WireGate, WireIndicator,
};

/// Visible band rows, including the overflow row.
pub const MAX_ROWS: u16 = 3;
/// Unretired messages (glass + queue).
pub(crate) const MAX_LIVE: usize = 64;
/// Records in the in-memory log ring.
pub const LOG_CAP: usize = 512;
/// Undrained persist lines.
pub(crate) const PENDING_PERSIST_CAP: usize = 1024;
/// Title cap in chars, after sanitize; one line.
pub const TITLE_CAP: usize = 120;
/// Chars per detail line, after sanitize.
pub const DETAIL_LINE_CAP: usize = 240;
/// Detail lines; `detail[0]` is the band's excerpt source.
pub const DETAIL_LINES_CAP: usize = 24;
/// Supersede-key chars.
pub(crate) const KEY_CAP: usize = 48;
/// Meter stats chars (volatile: never persisted, never spoken).
pub(crate) const STATS_CAP: usize = 40;
/// Authored intents per message; `Details ›` is implicit.
pub(crate) const MAX_ACTIONS: usize = 2;
/// Cells each side of a row.
pub const MARGIN: usize = 1;
/// The glyph's column.
pub const GLYPH_COL: usize = 1;
/// The title's first column.
pub const TITLE_COL: usize = 3;
/// Cells clear before the first capsule.
pub(crate) const BEFORE_CAPSULES: usize = 2;
/// Cells between capsules.
pub(crate) const CAPSULE_GAP: usize = 1;
/// Below this many cells the excerpt is dropped, never stubbed, unless its whole lead clause fits.
pub(crate) const DETAIL_FLOOR: usize = 20;
/// The cells a title elides to and no further, in whole words; capsules never drop.
pub(crate) const TITLE_MIN: usize = 12;
/// The joint between a detail's pieces, the one every words builder writes.
pub const PIECE_SEP: &str = " \u{00b7} ";

/// A Success row's default hold.
pub const HOLD_SUCCESS: Duration = Duration::from_secs(8);
/// An Info row's default hold.
pub(crate) const HOLD_INFO: Duration = Duration::from_secs(30);
/// A Warn row's default hold.
pub const HOLD_WARN: Duration = Duration::from_secs(45);
/// An Error row's default hold — an error earns a longer read than a warning.
pub(crate) const HOLD_ERROR: Duration = Duration::from_mins(1);
/// A staged update that applies only on a press.
pub const HOLD_STAGED_MANUAL: Duration = Duration::from_mins(1);
/// A decision row's hold.
pub const HOLD_ASK: Duration = Duration::from_mins(10);
/// A route row after Open Settings: a terse restate of the question's row.
pub const HOLD_ROUTE: Duration = Duration::from_secs(15);
/// A failed gesture's hold.
pub const HOLD_GESTURE: Duration = Duration::from_secs(12);
/// A tailed toolchain meter folds after this much silence.
pub const STALE_TAILED: Duration = Duration::from_secs(30);
/// An announcement-only toolchain row's cap.
pub const STALE_ANNOUNCE: Duration = Duration::from_mins(20);
/// A live update row's cap.
pub const STALE_UPDATE: Duration = Duration::from_mins(15);
/// A carried row's cap across the seamless handoff.
pub const STALE_HANDOFF: Duration = Duration::from_secs(125);
/// Band hysteresis: grow now, shrink after this much quiet.
pub const SHRINK_QUIET: Duration = Duration::from_millis(1500);
/// A queued row folds unseen after `max(hold, this)`.
pub(crate) const OVERFLOW_PATIENCE_MIN: Duration = Duration::from_mins(1);

/// A row that would appear and fold inside this costs two re-grids for nothing.
pub const PROGRESS_GRACE: Duration = Duration::from_secs(2);
/// The one frame grid every motion phase is read on — the host's 30 fps cap.
pub const ANIM_FRAME: Duration = Duration::from_millis(33);
/// One comet's crossing of the whole row.
pub const COMET_PERIOD: Duration = Duration::from_secs(3);
/// The comet's length in permille of the row, soft leading edge included.
pub const COMET_PERMILLE: u32 = 250;
/// The comet's soft leading edge in permille of the row.
pub(crate) const COMET_LEAD_PERMILLE: u32 = 80;
/// The comet's hot head, lifted toward the glint (0–255).
pub(crate) const COMET_HEAD_LIFT: u8 = 150;
/// The glint's cycle over a determinate bar: a travel, then rest.
pub const GLINT_PERIOD: Duration = Duration::from_secs(4);
/// The glint's travel along the fill; the rest of [`GLINT_PERIOD`] asks no frames.
pub const GLINT_TRAVEL: Duration = Duration::from_millis(1600);
/// The first glint, after the motion epoch.
pub const GLINT_DELAY: Duration = Duration::from_millis(1200);
/// The glint's radius in permille of the row.
pub(crate) const GLINT_RADIUS_PERMILLE: u32 = 30;
/// The glint's least radius in half cells of the row (a lift at least one and a half cells wide).
pub(crate) const GLINT_MIN_HALF_CELLS: u32 = 3;
/// The glint's peak lift (0–255).
pub(crate) const GLINT_PEAK: u8 = 255;
/// A data jump in a determinate fill glides over at least this long…
pub const FILL_GLIDE: Duration = Duration::from_millis(180);
/// …plus this much per percentage point it travels, so a big jump does not hop many cells a frame…
pub const FILL_GLIDE_PER_POINT: Duration = Duration::from_millis(7);
/// …and never longer than this.
pub const FILL_GLIDE_MAX: Duration = Duration::from_millis(600);
/// The display's frame period a host that does not say otherwise is assumed to have (60 Hz).
pub const REFRESH_DEFAULT: Duration = Duration::from_micros(16_667);
/// A Complete echo's one conclusive sweep.
pub const ECHO_SWEEP: Duration = Duration::from_millis(800);
/// The sweep's radius in permille of the row.
pub const ECHO_SWEEP_PERMILLE: u32 = 25;
/// An echo's fade.
pub(crate) const ECHO_FADE: Duration = Duration::from_millis(250);
/// A Fault echo's cross-fade from the fill to the warn wash.
pub const ECHO_FAULT_CROSS: Duration = Duration::from_millis(120);
/// A Fault echo's warn wash holds until this far in, then fades.
pub const ECHO_FAULT_FLASH: Duration = Duration::from_millis(350);
/// A busy row's Fault: its comet, frozen where it failed, drains to the track over this.
pub const ECHO_DRAIN: Duration = Duration::from_millis(80);
/// Heavy-load words show after the declared load lasted this long.
pub const LOAD_AFTER: Duration = Duration::from_secs(5);
/// A byte stream that has not advanced for this long reads "stalled".
pub const STALL_AFTER: Duration = Duration::from_secs(10);
/// A count (steps or items) reads "stalled" after this many of its own mean gaps.
pub(crate) const COUNT_STALL_GAPS: u32 = 4;
/// A row ending its reveal grace whose work projects to end within this is held back.
pub const REVEAL_MIN_LEFT: Duration = Duration::from_millis(1500);
/// …for at most this long past its grace.
pub const REVEAL_DEFER_MAX: Duration = Duration::from_secs(2);
/// How often a held-back reveal is looked at again.
pub(crate) const REVEAL_RECHECK: Duration = Duration::from_millis(250);
/// How long an overdue latched ETA stands at `<5 s left`.
pub(crate) const ETA_OVERDUE_MIN: Duration = Duration::from_secs(5);
/// A busy row shows its elapsed time from here on.
pub const ELAPSED_AFTER: Duration = Duration::from_secs(10);
/// The rate is the secant across at most this window.
pub(crate) const RATE_WINDOW: Duration = Duration::from_secs(20);
/// …and at least this span.
pub(crate) const RATE_MIN_SPAN: Duration = Duration::from_secs(3);
/// Samples closer than this share one slot of the ring.
pub(crate) const SAMPLE_MIN_GAP: Duration = Duration::from_millis(500);
/// The sample ring's cap.
pub(crate) const SAMPLES_CAP: usize = 48;
/// Projections older than this do not vote on the latch.
pub(crate) const STABLE_SPAN: Duration = Duration::from_secs(4);
/// The voting projections must span at least this long.
pub(crate) const STABLE_MIN: Duration = Duration::from_secs(2);
/// The projection ring's cap.
pub(crate) const PROJECTIONS_CAP: usize = 16;
/// The percent slot: two cells of gap, then the percent right-aligned in four (`"  100%"`).
pub(crate) const PCT_W: usize = 6;
/// The elapsed slot, labelled: `"for 59 min"`, never a bare `0:41` clock.
pub(crate) const ELAPSED_W: usize = 10;
/// The elapsed slot's short form where the long one does not fit.
pub(crate) const ELAPSED_SHORT_W: usize = 8;
/// The ETA slot: `"59 min left"` or `"stalled"`; `left` already says it is an estimate, so no `~`.
pub(crate) const ETA_W: usize = 11;
/// The ETA slot's short form where the long one does not fit.
pub(crate) const ETA_SHORT_W: usize = 8;
/// The ETA slot's word for a byte stream that stopped (painted in warn).
pub const STALLED_WORD: &str = "stalled";
/// A Fault echo's word in the row's time slot: the failure is said, not only tinted.
pub const FAILED_WORD: &str = "failed";
/// A Complete echo's word in the row's time slot when its title has no past tense.
pub const DONE_WORD: &str = "done";

/// A wire progress row's staleness cap.
pub(crate) const STALE_WIRE: Duration = Duration::from_mins(2);
/// The most wire rows live at once.
pub(crate) const WIRE_LIVE_CAP: usize = MAX_ROWS as usize;
/// The most wire entries open in the digest at once (`notice later`).
pub(crate) const WIRE_LATER_CAP: usize = 4;
/// New wire messages — rows and records, each a log line — per trailing [`WIRE_MINT_WINDOW`].
pub(crate) const WIRE_MINTS_PER_WINDOW: usize = 60;
/// The title and detail bytes those mints may carry per window.
pub(crate) const WIRE_BYTES_PER_WINDOW: usize = 16 * 1024;
/// `notice act` presses per [`WIRE_MINT_WINDOW`]: each repaints, may open a window and logs a line.
pub(crate) const WIRE_PRESSES_PER_WINDOW: usize = 10;
/// The most wire-owned records the log ring keeps once full.
pub(crate) const WIRE_LOG_SHARE: usize = 128;
/// That window.
pub(crate) const WIRE_MINT_WINDOW: Duration = Duration::from_mins(1);
/// The least gap between two paints a wire line causes: one motion frame.
pub const WIRE_PAINT_GAP: Duration = ANIM_FRAME;
/// The wire's key namespace.
pub const WIRE_KEY_PREFIX: &str = "wire.";
/// A wire key's longest spelling after [`WIRE_KEY_PREFIX`].
pub(crate) const WIRE_KEY_MAX: usize = 40;
/// A glass title is at most this many words ([`text::title_words`])…
pub(crate) const GLASS_TITLE_WORDS: usize = 6;
/// …and this many characters: short enough to survive beside two capsules.
pub const GLASS_TITLE_CHARS: usize = 48;
const _: () =
    assert!(WIRE_KEY_PREFIX.len() + WIRE_KEY_MAX <= KEY_CAP && GLASS_TITLE_CHARS < TITLE_CAP);
const _: () = assert!(WIRE_LOG_SHARE < LOG_CAP && WIRE_LIVE_CAP < WIRE_LOG_SHARE);
const _: () = assert!(WIRE_LATER_CAP * 3 <= need::DIGEST_CAP);

const _: () = assert!(
    STALLED_WORD.len() <= ETA_SHORT_W
        && ETA_SHORT_W < ETA_W
        && FAILED_WORD.len() <= ELAPSED_SHORT_W
        && FAILED_WORD.len() <= ETA_SHORT_W
        && DONE_WORD.len() <= ELAPSED_SHORT_W
        && DONE_WORD.len() <= ETA_SHORT_W
        && ELAPSED_SHORT_W < ELAPSED_W
);

const _: () = assert!(
    HOLD_ERROR.as_millis() > HOLD_WARN.as_millis()
        && OVERFLOW_PATIENCE_MIN.as_millis() >= HOLD_INFO.as_millis()
        && TITLE_MIN < TITLE_CAP
        && DETAIL_FLOOR < DETAIL_LINE_CAP
);

#[cfg(test)]
mod attention_tests;
#[cfg(test)]
mod band_tests;
#[cfg(test)]
mod door_tests;
#[cfg(test)]
mod drive_tests;
#[cfg(test)]
mod entry_tests;
#[cfg(test)]
mod exception_fuzz_tests;
#[cfg(test)]
mod family_tests;
#[cfg(test)]
mod later_tests;
#[cfg(test)]
mod need_tests;
#[cfg(test)]
mod overflow_tests;
#[cfg(test)]
mod page_tests;
#[cfg(test)]
mod presence_drive_tests;
#[cfg(test)]
mod presence_tests;
#[cfg(test)]
mod progress_words_tests;
#[cfg(test)]
mod record_end_tests;
#[cfg(test)]
mod regression_tests;
#[cfg(test)]
mod review_tests;
#[cfg(test)]
mod row_buttons_tests;
#[cfg(test)]
mod row_words_tests;
#[cfg(test)]
mod width_law_tests;
