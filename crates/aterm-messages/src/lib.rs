// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm-messages` — one band, one model, one log.
//!
//! The platform-neutral engine behind aterm's unified message system
//! (docs/DESIGN-unified-messages-2026-09-21.md): the message model, the glass
//! policy (which rows are on the band, in what order, for how long), the
//! single-row width law, the tagged log ring and its line codec, the wire
//! grammars and the seamless-handoff carry. It emits layouts in cells
//! ([`glass::Presentation`]); the host paints them, appends the log lines it
//! drains, and performs the plain-data [`model::Intent`]s a press returns.
//!
//! # Contract
//!
//! * **Clockless.** Every method that reads time takes `now: Instant`
//!   ([`Instant`] is `aterm_time::Instant`), so the engine runs
//!   deterministically and on wasm.
//! * **No platform.** No `std::fs`, threads, sockets or theme: [`paint`] lays
//!   out the band's structure (character, ink slot and ground per cell) and
//!   [`ink`] resolves it over a plain-RGB palette derived from a theme's
//!   background, foreground and cursor; the host owns the file, the cells and
//!   the theme.
//! * **Bounded.** [`MAX_LIVE`] unretired messages, [`LOG_CAP`] ring records,
//!   [`PENDING_PERSIST_CAP`] undrained persist lines; every string is sanitized
//!   and capped at ingress, so a hostile reporter cannot grow the process or
//!   forge a row.
//! * **Constants, not knobs.** Every hold, cap and floor is a `pub const` here.
//!
//! # Indicators and motion
//!
//! A row's indicator is its meter's state: a fill draws the bar,
//! [`Meter::busy`] the comet, neither draws nothing. The meter spans the whole
//! row (`(0, cols, fill)`, a busy track `(0, cols)`), laid under the words and
//! mapped by the host onto the window's pixels, gutters included, so it costs
//! the words no room and is never shed. Motion is the engine's: the host reads
//! one frame at a time through [`MessageCenter::motion`] and wakes at
//! [`MessageCenter::motion_deadline`] on one 33 ms grid; a frame carries
//! fractions of the row ([`animate::Surface`]) and an echo's glyph, never a
//! pixel. A frame never enters the center: it bumps no revision, re-grids
//! nothing and never arms [`MessageCenter::deadline`].
//! [`MessageCenter::busy_on_glass`] says whether a committed row is busy.
//!
//! A host runs all of it through one driver, [`drive`]: the step (settle,
//! commit, drain, the wire's paced paint), each view's layout, motion frame,
//! next change and paint key, and the deadline folds; the host injects the
//! clock, the afford, the freeze and each view's look.
//!
//! # The presence row
//!
//! The same chrome carries the per-tab presence row (who drives the session,
//! what its agent is doing, its mail and its link) and the window's rim. Its
//! pure core is [`presence`]: the facts a host gathers, the per-session slot and
//! its story, the levels, the words and their width law, the tones, the painted
//! cells and the per-window view. What only a host knows (an agent wall's
//! vocabulary, its input stall, its harness's name, the wire's percent codec)
//! comes in through [`presence::Host`]; sequencing is [`presence::drive`] over
//! the host's [`presence::drive::Desk`].
//!
//! # The window's chrome rows together
//!
//! The strip, the band and the presence row share one window's grid, and
//! each spaces only its OWN fall from its own rise. [`chrome`] (design
//! ruling 414) is the floor across them: after any rise of a
//! window's chrome rows, no source's quiet fall re-grids it back inside
//! [`chrome::CHROME_REGROW_SETTLE`]. The host notes the rises and hands the
//! floor in ([`drive::Commit::floor`], [`presence::drive::Desk::chrome_floor`]);
//! the fall laws read it.
//!
//! # The Settings ▸ Messages page
//!
//! [`page`] builds the log page's model once over the center
//! ([`page::MessagesState::of`]): every record newest first in the wire's state
//! words, the live rows' current words, the band row each is on, which authored
//! intents can still be pressed, the tags in chip order, and the page's words,
//! filter, day clock and press feedback. Host facts (clock, log folder, UTC
//! offset, staged build, agent upgrades, detail-line budget) come in through
//! [`page::Host`]; the host lays the page out and paints it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(clippy::all)]
#![deny(clippy::pedantic)]
// Test code is exempt from the pedantic bar; production code is not.
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
pub mod strain;
pub mod text;
pub mod waits;
pub mod wire;
pub mod words;

#[cfg(test)]
mod adversarial;

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
/// The glint's travel along the FILL; the rest of [`GLINT_PERIOD`] asks no
/// frames.
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

// ---- Strain: explaining very heavy system use ------------------------------
//
// The strain engine ([`strain`]) puts one row on the glass, `Typing slowed by
// <cause>`, only while aterm measured slow typing (felt), a resource is past
// its enter line (heavy), the cause is named from what was measured (named)
// and it is not the person's own command or an aterm job that already has a
// row (not own). Everything else is a record. Nothing is sampled until felt is
// suspected, so a calm machine arms nothing.

/// The strain row's supersede key; its records share it.
pub const STRAIN_KEY: &str = "system.strain";
/// A hardware key whose arrival-to-present took at least this long is slow.
pub const SLOW_KEY_MS: u32 = 60;
/// A key this slow is also a hitch: lag felt as a stall, not a drag.
pub(crate) const HITCH_MS: u32 = 250;
/// A main-loop turn at least this long, ending within the host's typing
/// tail, is a hitch (the host decides the tail and calls
/// [`strain::StrainTracker::note_freeze`]).
pub const FREEZE_MS: u32 = 400;
/// The trailing window [`strain::StrainTracker::felt`] reads.
pub(crate) const FEEL_WINDOW: Duration = Duration::from_secs(6);
/// Felt needs at least this many keys in the window, half of them slow…
pub(crate) const FEEL_MIN_KEYS: usize = 6;
/// …or this many hitches.
pub(crate) const FEEL_MIN_HITCHES: usize = 2;
/// The typing samples kept: O(1) per note, bounded forever.
pub(crate) const FEEL_RING: usize = 32;
/// The probe's cadence while the engine is Suspect or Open.
pub(crate) const STRAIN_SAMPLE_EVERY: Duration = Duration::from_secs(2);
/// A process sweep rides every this-many readings (the first reading of an
/// episode is a sweep: the baseline the next one takes deltas against).
pub(crate) const STRAIN_SCAN_EVERY: u32 = 2;
/// Consecutive readings past a kind's enter line before it is heavy.
pub(crate) const STRAIN_ENTER_READINGS: u8 = 2;
/// Consecutive readings past a kind's clear line before it clears.
pub(crate) const STRAIN_CLEAR_READINGS: u8 = 3;
/// Felt must have held this long before a row: the heavy-load words' own
/// onset ([`LOAD_AFTER`]).
pub(crate) const STRAIN_ONSET: Duration = LOAD_AFTER;
/// Suspect falls back to calm after felt has been false this long…
pub(crate) const STRAIN_UNFELT_CALM: Duration = Duration::from_secs(8);
/// …or after no hardware key for this long.
pub(crate) const STRAIN_NO_KEY_CALM: Duration = Duration::from_secs(15);
/// A Suspect that lasted this long leaves one record when it ends.
pub(crate) const STRAIN_RECORD_AFTER: Duration = Duration::from_secs(5);
/// An open row folds after no hardware key for this long…
pub(crate) const STRAIN_IDLE: Duration = Duration::from_secs(30);
/// …or after felt has been false this long across at least
/// [`FEEL_MIN_KEYS`] keys…
pub(crate) const STRAIN_UNFELT_FOLD: Duration = Duration::from_secs(20);
/// …or after this long on the glass, whatever the machine does.
pub(crate) const STRAIN_GLASS_MAX: Duration = Duration::from_mins(2);
/// Nothing folds the row sooner than this after it appeared.
pub(crate) const STRAIN_MIN_GLASS: Duration = Duration::from_secs(6);
/// The strain row's staleness cap: a host that stops sampling lets it fade.
pub const STALE_STRAIN: Duration = Duration::from_secs(30);
/// An unchanged open row is restated this often, so its staleness cap never
/// fades a row the engine still holds (an identical restatement repaints
/// nothing).
pub(crate) const STRAIN_KEEPALIVE: Duration = Duration::from_secs(10);
/// The gauge moves the row only by at least this much.
pub(crate) const STRAIN_RESTATE_PERMILLE: u16 = 20;
/// The same cause (kind and culprit) stays off the glass this long after
/// its fold…
pub(crate) const STRAIN_QUIET_SAME: Duration = Duration::from_mins(15);
/// …and any strain row this long; only an escalation breaks either.
pub(crate) const STRAIN_QUIET_ANY: Duration = Duration::from_mins(3);
/// Strain records per trailing [`STRAIN_RECORD_WINDOW`]; past it the next
/// record's title ends ` (+N more)`.
pub(crate) const STRAIN_RECORDS_PER_WINDOW: usize = 6;
/// That window.
pub(crate) const STRAIN_RECORD_WINDOW: Duration = Duration::from_secs(3600);
/// At most one `Memory pressure critical` record (while calm) per this.
pub(crate) const STRAIN_CRITICAL_EVERY: Duration = Duration::from_mins(10);
/// A culprit is named only past this many milli-cores…
pub(crate) const STRAIN_NAMED_MILLICORES: u32 = 1000;
/// …and this share (percent) of the machine's busy core-time.
pub(crate) const STRAIN_NAMED_SHARE_PCT: u32 = 40;
/// A memory culprit is named only past this share (percent) of the RAM.
pub(crate) const STRAIN_NAMED_MEMORY_PCT: u32 = 20;
const _: () = assert!(
    STRAIN_MIN_GLASS.as_secs() < STALE_STRAIN.as_secs()
        && STRAIN_KEEPALIVE.as_secs() < STALE_STRAIN.as_secs()
        && STRAIN_KEY.len() <= KEY_CAP
        && HITCH_MS > SLOW_KEY_MS
        && FEEL_MIN_KEYS <= FEEL_RING
);
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

#[cfg(test)]
mod tests {
    /// Every source file of this crate, by name, as compiled — no `std::fs`
    /// walk, so the fence holds on a target with no filesystem too.
    const SOURCES: &[(&str, &str)] = &[
        ("lib.rs", include_str!("lib.rs")),
        ("model.rs", include_str!("model.rs")),
        ("need.rs", include_str!("need.rs")),
        ("center.rs", include_str!("center.rs")),
        ("chrome.rs", include_str!("chrome.rs")),
        ("digest.rs", include_str!("digest.rs")),
        ("exceptions.rs", include_str!("exceptions.rs")),
        ("glass.rs", include_str!("glass.rs")),
        ("log.rs", include_str!("log.rs")),
        ("text.rs", include_str!("text.rs")),
        ("wire.rs", include_str!("wire.rs")),
        ("words.rs", include_str!("words.rs")),
        ("carry.rs", include_str!("carry.rs")),
        ("progress.rs", include_str!("progress.rs")),
        ("animate.rs", include_str!("animate.rs")),
        ("strain.rs", include_str!("strain.rs")),
        ("waits.rs", include_str!("waits.rs")),
        ("palette.rs", include_str!("palette.rs")),
        ("paint.rs", include_str!("paint.rs")),
        ("page.rs", include_str!("page.rs")),
        ("ink.rs", include_str!("ink.rs")),
        ("drive.rs", include_str!("drive.rs")),
        ("presence/mod.rs", include_str!("presence/mod.rs")),
        ("presence/model.rs", include_str!("presence/model.rs")),
        ("presence/words.rs", include_str!("presence/words.rs")),
        ("presence/paint.rs", include_str!("presence/paint.rs")),
        ("presence/view.rs", include_str!("presence/view.rs")),
        ("presence/drive.rs", include_str!("presence/drive.rs")),
    ];

    /// Invariant 1: the engine never samples a clock. `tools/grep_guard.sh`'s
    /// C3 walks only core/src/terminal, so the crate carries its own fence: no
    /// `::now()` in the SHIPPED code, everything before each file's
    /// `#[cfg(test)]` MODULE (tests mint a base instant). A `#[cfg(test)]` on a
    /// single accessor ends nothing, so the scan cannot quietly shrink.
    #[test]
    fn no_wall_clock_in_this_crate() {
        // Assembled so this test's own text is not a hit.
        let needle = ["::", "now()"].concat();
        let marker = ["\n#[cfg(", "test)]\n"].concat();
        let mut hits = Vec::new();
        for (name, src) in SOURCES {
            let module_start = src.match_indices(&marker).find(|(at, _)| {
                let next = src[at + marker.len()..].lines().next().unwrap_or("");
                next.starts_with("mod ") || next.starts_with("pub(crate) mod ")
            });
            let shipped = module_start.map_or(*src, |(at, _)| &src[..at]);
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
        // The fence is complete by construction: every shipped `mod x;` of this
        // file names a source in SOURCES — `x.rs`, or a directory module's
        // `x/mod.rs` together with every `mod y;` that file declares
        // (`x/y.rs`) — so a new shipped module cannot arrive unscanned (the
        // `#[cfg(test)]` modules are not shipped).
        let decls = |src: &'static str| -> Vec<&'static str> {
            src.lines()
                .filter_map(|l| {
                    l.strip_prefix("pub mod ")
                        .or_else(|| l.strip_prefix("pub(crate) mod "))
                        .or_else(|| l.strip_prefix("mod "))?
                        .strip_suffix(';')
                })
                .collect()
        };
        let source = |file: &str| SOURCES.iter().find(|(name, _)| *name == file);
        let modules: Vec<&str> = include_str!("lib.rs")
            .lines()
            .filter_map(|l| {
                l.strip_prefix("pub mod ")
                    .or_else(|| l.strip_prefix("pub(crate) mod "))?
                    .strip_suffix(';')
            })
            .collect();
        assert_eq!(modules.len(), 22, "{modules:?}");
        let mut scanned = 1;
        for m in &modules {
            let flat = format!("{m}.rs");
            let dir = format!("{m}/mod.rs");
            let Some((name, src)) = source(&flat).or_else(|| source(&dir)) else {
                panic!("{flat} is a shipped module outside the clock fence");
            };
            scanned += 1;
            if *name == dir {
                for sub in decls(src) {
                    let file = format!("{m}/{sub}.rs");
                    assert!(
                        source(&file).is_some(),
                        "{file} is a shipped module outside the clock fence"
                    );
                    scanned += 1;
                }
            }
        }
        assert_eq!(
            SOURCES.len(),
            scanned,
            "lib.rs plus every shipped module, nothing else"
        );
    }

    /// The holds are the measured status-bar values.
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
