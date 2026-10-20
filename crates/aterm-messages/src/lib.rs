// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-messages`: one band, one model, one log.
//!
//! The platform-neutral engine behind aterm's message system
//! (docs/DESIGN-unified-messages-2026-09-21.md): the message model, the glass policy
//! (which rows are on the band, in what order, for how long), the single-row width law,
//! the tagged log ring and its line codec, the wire grammars and the seamless-handoff
//! carry. It emits layouts in cells ([`glass::Presentation`]); the host paints them,
//! appends the log lines it drains, and performs the plain-data [`model::Intent`]s a
//! press returns.
//!
//! # Contract
//!
//! * **Clockless.** Every method that reads time takes `now: Instant` ([`Instant`] is
//!   `aterm_time::Instant`), so the engine is deterministic and runs on wasm.
//! * **No platform.** No `std::fs`, threads, sockets or theme: [`paint`] lays out the
//!   band's structure (character, ink slot and ground per cell) and [`ink`] resolves it
//!   over a plain-RGB palette derived from a theme's background, foreground and cursor.
//! * **Bounded.** [`MAX_LIVE`] unretired messages, [`LOG_CAP`] ring records,
//!   [`PENDING_PERSIST_CAP`] undrained persist lines; every string is sanitized and
//!   capped at ingress, so a hostile reporter cannot grow the process or forge a row.
//! * **Constants, not knobs.** Every hold, cap and floor is a `const`.
//!
//! # Indicators and motion
//!
//! A row's indicator is its meter's state: a fill draws the bar, [`Meter::busy`] the
//! comet, neither draws nothing. The meter spans the whole row, laid under the words and
//! mapped by the host onto the window's pixels, gutters included, so it costs the words
//! no room. The host reads one motion frame at a time ([`MessageCenter::motion`]) and
//! wakes at [`MessageCenter::motion_deadline`] on the [`ANIM_FRAME`] grid. A frame
//! carries fractions of the row ([`animate::Surface`]), never a pixel, and never enters
//! the center: it bumps no revision, re-grids nothing and never arms
//! [`MessageCenter::deadline`]. A host runs all of it through one driver, [`drive`],
//! injecting the clock, the afford, the freeze and each view's look.
//!
//! # The rest of the chrome
//!
//! The window's needs border, its titlebar pill and the tab marks are [`presence`]: they
//! draw the host's published need verdict, what only a host knows comes in through
//! [`presence::Host`], and sequencing is [`presence::drive`] over the host's
//! [`presence::drive::Desk`]. Presence owns no chrome row. The strip and the band share
//! one window's grid, and [`chrome`] is the floor across them: after any rise, the band's
//! quiet fall does not re-grid the window back inside [`chrome::CHROME_REGROW_SETTLE`].
//! The host hands the floor in ([`drive::Commit::floor`]).
//!
//! [`page`] builds the Settings ▸ Messages page's model over the center
//! ([`page::MessagesState::of`]); host facts come in through [`page::Host`], and the host
//! lays the page out and paints it.

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
    ActionIndex, Amount, Decision, Glyph, Hold, Intent, Load, Loads, Message, MessageId, Meter,
    Origin, Restatement, Severity, Tag, TagError, Unit, UpgradeWord, WallStamp, tags,
};
pub use progress::{Eta, ProgressTrack, Waiter};
pub use waits::{SessionWait, WaitEnd, WaitKind, WaitSample, WaitStep};
pub use wire::{
    Applied, DoneHow, DoneRequest, NoticeRequest, Paint, Press, ProgressRequest, WireGate,
    WireIndicator,
};

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// Visible band rows, including the overflow row.
pub const MAX_ROWS: u16 = 3;
/// Unretired messages (glass + queue). Past it a post evicts.
pub(crate) const MAX_LIVE: usize = 64;
/// Records in the in-memory log ring: what Settings ▸ Messages shows and what
/// a launch's replay of the disk tail keeps. Once `WIRE_LOG_SHARE` of a full
/// ring's records are wire-owned, the oldest retired wire record goes first.
/// Disk retention is the host's (`aterm-gui`'s `messages_store`), in bytes.
pub const LOG_CAP: usize = 512;
/// Undrained persist lines. Past it the oldest is dropped and counted.
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
/// Below this many cells the excerpt is dropped, never stubbed, unless its
/// whole lead clause fits (`behind for 7 h`, `text::lead_clause`).
pub(crate) const DETAIL_FLOOR: usize = 20;
/// The title elides no further; capsules never drop.
pub(crate) const TITLE_MIN: usize = 12;
/// The joint between a detail's pieces, the one every words builder writes;
/// [`text::shape_detail`] sheds pieces at it.
pub const PIECE_SEP: &str = " \u{00b7} ";

/// A Success row's default hold.
pub const HOLD_SUCCESS: Duration = Duration::from_secs(8);
/// An Info row's default hold.
pub(crate) const HOLD_INFO: Duration = Duration::from_secs(30);
/// A Warn row's default hold.
pub const HOLD_WARN: Duration = Duration::from_secs(45);
/// An Error row's default hold — an error earns a longer read than a warning.
pub(crate) const HOLD_ERROR: Duration = Duration::from_secs(60);
/// A staged update that applies only on a press.
pub const HOLD_STAGED_MANUAL: Duration = Duration::from_secs(60);
/// A decision row's hold.
pub const HOLD_ASK: Duration = Duration::from_mins(10);
/// A route row after Open Settings: a terse restate of the question's row.
/// The route stays behind Details and on Settings ▸ Messages.
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
pub(crate) const OVERFLOW_PATIENCE_MIN: Duration = Duration::from_secs(60);

// ---- Attention and motion --------------------------------------------------

/// A row that would appear and fold inside this costs two re-grids for
/// nothing: a progress row posted with `reveal_after(PROGRESS_GRACE)` reaches
/// the glass only if its work lasts this long.
pub const PROGRESS_GRACE: Duration = Duration::from_secs(2);
/// The one frame grid every motion phase is read on — the host's 30 fps cap.
pub const ANIM_FRAME: Duration = Duration::from_millis(33);
/// One comet's crossing of the whole row: its head enters at the window's left
/// edge and the crossing ends as its tail clears the right edge
/// ([`COMET_PERMILLE`] more than the row). The next comet enters as this one
/// leaves, so the row is never empty for longer than a frame.
pub const COMET_PERIOD: Duration = Duration::from_millis(3000);
/// The comet's length in permille of the row, soft leading edge included: a
/// tail rising as the square of its length toward the hot head, then the
/// leading edge falling to the track over [`COMET_LEAD_PERMILLE`]. A quarter
/// keeps the head's steepest stretch under two levels a pixel at 80 columns.
pub const COMET_PERMILLE: u32 = 250;
/// The comet's soft leading edge in permille of the row: the head's tone falls
/// to the track over this, ahead of the head. Drawn to the pixel it is a
/// straight ramp; two levels a pixel at 80 columns needs about this much.
pub(crate) const COMET_LEAD_PERMILLE: u32 = 80;
/// The comet's hot head, lifted toward the glint (0–255).
pub(crate) const COMET_HEAD_LIFT: u8 = 150;
/// The glint's cycle over a determinate bar: a travel, then rest.
pub const GLINT_PERIOD: Duration = Duration::from_millis(4000);
/// The glint's travel along the fill; the rest of [`GLINT_PERIOD`] asks no frames.
pub const GLINT_TRAVEL: Duration = Duration::from_millis(1600);
/// The first glint, after the motion epoch.
pub const GLINT_DELAY: Duration = Duration::from_millis(1200);
/// The glint's radius in permille of the row (a raised-cosine lift about six
/// percent of the row wide), at least [`GLINT_MIN_HALF_CELLS`] half cells so a
/// narrow window's glint is never a one-column smudge.
pub(crate) const GLINT_RADIUS_PERMILLE: u32 = 30;
/// The glint's least radius in half cells of the row (a lift at least one and
/// a half cells wide).
pub(crate) const GLINT_MIN_HALF_CELLS: u32 = 3;
/// The glint's peak lift (0–255): the host's glint ink is itself one
/// perceptual step, so the peak reaches it whole.
pub(crate) const GLINT_PEAK: u8 = 255;
/// A data jump in a determinate fill glides over at least this long…
pub const FILL_GLIDE: Duration = Duration::from_millis(180);
/// …plus this much per percentage point it travels, so a big jump does not
/// hop many cells a frame…
pub const FILL_GLIDE_PER_POINT: Duration = Duration::from_millis(7);
/// …and never longer than this.
pub const FILL_GLIDE_MAX: Duration = Duration::from_millis(600);
/// The display's frame period a host that does not say otherwise is assumed
/// to have (60 Hz): glides and echoes are read on it while they are in
/// flight ([`MessageCenter::set_refresh`]).
pub const REFRESH_DEFAULT: Duration = Duration::from_micros(16_667);
/// A Complete echo's one conclusive sweep: a glint about four cells wide (at
/// 80 columns) crossing the whole row once, at twice the glint's pace.
pub const ECHO_SWEEP: Duration = Duration::from_millis(800);
/// The sweep's radius in permille of the row.
pub const ECHO_SWEEP_PERMILLE: u32 = 25;
/// An echo's fade.
pub(crate) const ECHO_FADE: Duration = Duration::from_millis(250);
/// A Fault echo's cross-fade from the fill to the warn wash: premultiplied,
/// never through another hue.
pub const ECHO_FAULT_CROSS: Duration = Duration::from_millis(120);
/// A Fault echo's warn wash holds until this far in, then fades.
pub const ECHO_FAULT_FLASH: Duration = Duration::from_millis(350);
/// A busy row's Fault: its comet, frozen where it failed, drains to the track
/// over this.
pub const ECHO_DRAIN: Duration = Duration::from_millis(80);
/// Heavy-load words show after the declared load lasted this long.
pub const LOAD_AFTER: Duration = Duration::from_secs(5);
/// A byte stream that has not advanced for this long reads "stalled".
pub const STALL_AFTER: Duration = Duration::from_secs(10);
/// A count (steps or items) reads "stalled" after this many of its own mean
/// gaps without an advance, never before [`STALL_AFTER`]: one item may take
/// long, so a count's patience is its own pace.
pub(crate) const COUNT_STALL_GAPS: u32 = 4;
/// A row ending its reveal grace whose work projects to end within this is
/// held back: a row shown for under a second and then its echo is the flash
/// the grace exists to prevent.
pub const REVEAL_MIN_LEFT: Duration = Duration::from_millis(1500);
/// …for at most this long past its grace: a projection that keeps saying
/// "almost done" and is wrong still reaches the glass.
pub const REVEAL_DEFER_MAX: Duration = Duration::from_secs(2);
/// How often a held-back reveal is looked at again.
pub(crate) const REVEAL_RECHECK: Duration = Duration::from_millis(250);
/// A latched ETA whose completion instant has passed with nothing new stands
/// at `<5 s left` for at most this long (or a fifth of the latched estimate,
/// if longer), then goes blank.
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
/// The percent slot: two cells of gap, then the percent right-aligned in four
/// (`"  100%"`).
pub(crate) const PCT_W: usize = 6;
/// The elapsed slot, labelled: `"for 59 min"`, never a bare `0:41` clock.
pub(crate) const ELAPSED_W: usize = 10;
/// The elapsed slot's short form where the long one does not fit: `"for 59m"`,
/// `"for 999h"`, changing at the same instants.
pub(crate) const ELAPSED_SHORT_W: usize = 8;
/// The ETA slot: `"59 min left"` or `"stalled"`; `left` already says it is an
/// estimate, so no `~`.
pub(crate) const ETA_W: usize = 11;
/// The ETA slot's short form where the long one does not fit: `"59m left"`,
/// `"<5s left"` or `"stalled"`, changing at the same instants.
pub(crate) const ETA_SHORT_W: usize = 8;
/// The ETA slot's word for a byte stream that stopped (painted in warn).
pub const STALLED_WORD: &str = "stalled";
/// A Fault echo's word in the row's time slot (painted in warn, like a
/// stall): the failure is said, not only tinted.
pub const FAILED_WORD: &str = "failed";
/// A Complete echo's word in the row's time slot when its title has no past
/// tense ([`Message::finished_words`] is `None`): the ✓ alone beside
/// `Uploading the backup` would read as still going.
pub const DONE_WORD: &str = "done";
// ---- The wire's `notice` verb ----------------------------------------------

/// A wire progress row's staleness cap: an abandoned row fades (`Stale`, a
/// Vanish echo) this long after its last line; a live script re-sends its line
/// (an identical line costs nothing painted and re-arms the cap).
pub(crate) const STALE_WIRE: Duration = Duration::from_secs(120);
/// The most wire rows live at once: the glass's [`MAX_ROWS`], overflow row
/// included, so everything one script has in flight is on the glass. A
/// restate or a same-key supersede does not count against it.
pub(crate) const WIRE_LIVE_CAP: usize = MAX_ROWS as usize;
/// New wire messages — rows and records, each a log line — per trailing
/// [`WIRE_MINT_WINDOW`].
pub(crate) const WIRE_MINTS_PER_WINDOW: usize = 60;
/// The title and detail bytes those mints may carry per window: each is
/// written to `messages.log` twice (Posted and Retired), so a flood of
/// full-detail records cannot rotate aterm's own lines out of the file.
pub(crate) const WIRE_BYTES_PER_WINDOW: usize = 16 * 1024;
/// `notice act` presses per [`WIRE_MINT_WINDOW`]: each repaints, may open a
/// window and logs a line.
pub(crate) const WIRE_PRESSES_PER_WINDOW: usize = 10;
/// The most wire-owned records the log ring keeps once full: past it a new
/// record evicts the oldest retired wire record, so a script can never push
/// aterm's own history out of the ring.
pub(crate) const WIRE_LOG_SHARE: usize = 128;
/// That window.
pub(crate) const WIRE_MINT_WINDOW: Duration = Duration::from_secs(60);
/// The least gap between two paints a wire line causes: one motion frame.
pub const WIRE_PAINT_GAP: Duration = ANIM_FRAME;
/// The wire's key namespace: `notice` can only name a key under it, so it can
/// never restate, end, supersede or record over a host row.
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

const _: () = assert!(
    STALLED_WORD.len() <= ETA_SHORT_W
        && ETA_SHORT_W < ETA_W
        && FAILED_WORD.len() <= ELAPSED_SHORT_W
        && FAILED_WORD.len() <= ETA_SHORT_W
        && DONE_WORD.len() <= ELAPSED_SHORT_W
        && DONE_WORD.len() <= ETA_SHORT_W
        && ELAPSED_SHORT_W < ELAPSED_W
);

// An error earns a longer read than a warning; an unseen row's patience is
// at least an Info row's hold; the width law's floors sit under its caps.
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
