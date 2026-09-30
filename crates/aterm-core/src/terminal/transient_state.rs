// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Transient terminal state cleared on reset (#4307).
//!
//! [`TransientState`] bundles scalar fields and small buffers that are always
//! cleared together during `reset_common_fields`. Grouping these reduces the
//! reset function's parameter count and ensures new resettable fields only
//! need to be added in one place.

use super::response_rate_limiter::ResponseRateLimiter;
use super::types::SgrStackEntry;
use aterm_types::{PipelineTimestamps, Rgb};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use aterm_grid::ImageData;

// XTSAVE mode storage.
type XtsaveModesMap = HashMap<u16, bool>;

/// Default foreground/background — the VT/engine spec defaults (light grey on
/// black). These re-export the SINGLE source of truth in `aterm-types` so the
/// runtime terminal state and `TerminalConfig::default()` can never diverge (they
/// did historically: 229 here vs 255 in the config — a latent footgun the audit
/// flagged). See [`aterm_types::DEFAULT_FOREGROUND`].
pub(super) const DEFAULT_FOREGROUND: Rgb = aterm_types::DEFAULT_FOREGROUND;

/// See [`DEFAULT_FOREGROUND`]; the spec default background (black), single-sourced.
pub(super) const DEFAULT_BACKGROUND: Rgb = aterm_types::DEFAULT_BACKGROUND;

/// VT52 cursor addressing state.
///
/// VT52's direct cursor addressing (ESC Y row col) requires collecting
/// two parameter bytes after the ESC Y.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Vt52CursorState {
    /// Not collecting cursor position.
    #[default]
    None,
    /// Waiting for row byte (first parameter after ESC Y).
    WaitingRow,
    /// Waiting for column byte (second parameter after ESC Y).
    WaitingCol(u8),
}

/// Grouped transient terminal state cleared on reset (#4307).
///
/// Bundles scalar fields and small buffers that are always cleared together
/// during `reset_common_fields`. Grouping these reduces the reset function's
/// parameter count and ensures new resettable fields only need to be added
/// in one place (this struct + its `reset()` method).
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent terminal flags, not a state machine"
)]
pub(super) struct TransientState {
    /// Response buffer for DSR/DA and other terminal responses.
    pub(super) response_buffer: Vec<u8>,
    /// Token-bucket rate limiter gating `send_response` (Part of #7874).
    ///
    /// Prevents response-amplification DoS: a malicious peer spamming
    /// DSR/DA/DECRQSS cannot force unlimited response generation even
    /// when the host drains the buffer in a tight loop.
    pub(super) response_rate_limiter: ResponseRateLimiter,
    /// Last graphic character received (for REP - CSI b). Stored RAW
    /// (pre-charset-translation): xterm CASE_REP re-translates it through
    /// the GL charset that is current at repeat time.
    pub(super) last_graphic_char: Option<char>,
    /// Current hyperlink (OSC 8).
    pub(super) current_hyperlink: Option<Arc<str>>,
    /// Current hyperlink ID (OSC 8 `id=` parameter).
    pub(super) current_hyperlink_id: Option<Arc<str>>,
    /// Current underline color (SGR 58).
    pub(super) current_underline_color: Option<u32>,
    /// VT52 cursor addressing state.
    pub(super) vt52_cursor_state: Vt52CursorState,
    /// Timestamp when synchronized output mode (2026) was enabled.
    /// `aterm_time::Instant` (std on native, JS clock on wasm) to match `process_now`.
    pub(super) sync_start: Option<aterm_time::Instant>,
    /// Monotonic count of synchronized-update (mode 2026) WINDOW CLOSES — every
    /// `?2026l`, DECSTR/RIS reset, and timeout force-clear bumps it. A host that
    /// holds presents during a sync window compares this across redraws to tell
    /// "the bracket I armed on CLOSED (a complete frame is ready — present it)"
    /// from "the same bracket is still open": the mode LEVEL alone cannot make
    /// that distinction when a flood of back-to-back brackets keeps the sampled
    /// level true (the ~1 present/timeout freeze). Never reset to 0 — the reset
    /// paths bump it instead, so any change means "at least one close".
    pub(super) sync_end_seq: u64,
    /// Monotonic count of FULL RESETS (RIS `ESC c` and [`Terminal::reset`]) —
    /// bumped by [`reset`](Self::reset), which both paths reach through
    /// `reset_common_fields`, and never zeroed. The alt-screen archive compares it
    /// at every commit point to learn that a reset wiped the screen it was
    /// following, without the handler ever holding the archive (observation state
    /// stays out of the handler, like `watchers`).
    ///
    /// [`Terminal::reset`]: super::Terminal::reset
    pub(super) reset_generation: u64,
    /// Monotonic count of FULL CLEARS processed — ED 2, and ED 0 issued with
    /// the cursor home (`resize_journal::is_full_clear`; ED 3 only erases the
    /// history behind the screen, so it is not one). The one signal
    /// that an app repainted a displaced screen from scratch rather than
    /// diffing onto it, so each resize report carries its value at the resize
    /// and a reader compares. Never reset to 0, like `reset_generation`: the
    /// reader keys on it advancing.
    pub(super) full_clears: u64,
    /// Monotonic count of whole-screen REPLACEMENTS — alternate-screen enter
    /// and exit (47/1047/1049) and full resets, which [`reset`](Self::reset)
    /// bumps. A screen that was swapped or wiped after a resize no longer
    /// holds the rows the resize displaced. Never reset to 0.
    pub(super) screen_replaced: u64,
    /// Whether the CURRENT synchronized-output window has accepted any complete
    /// PTY action since its opening `?2026h`. A host may safely present a
    /// just-closed frame while the mode level already reads true again only
    /// while this is false: once the new window is dirty, the mutable grid may
    /// already contain a prefix of the next frame. Conservative by design —
    /// parser queries/no-ops count too, so the exceptional close+reopen present
    /// license fails closed rather than risking a torn frame.
    pub(super) sync_open_dirty: bool,
    /// Whether the PROGRAM's synchronized-output bracket is still open by its
    /// own account: set by the `?2026h` that opens it, cleared only by what
    /// the program itself (or a reset) says ends it — `?2026l`, DECSTR, RIS.
    /// Unlike `modes.synchronized_output` it SURVIVES the terminal's
    /// timeout force-clear, because the program has not finished its frame
    /// just because the terminal stopped waiting for it. Read through
    /// [`Terminal::sync_frame_unfinished`](super::Terminal::sync_frame_unfinished).
    pub(super) app_sync_open: bool,
    /// Whether the program's open bracket (`app_sync_open`) has accepted any
    /// complete PTY action since it opened — the program-view twin of
    /// `sync_open_dirty`, which a timeout does not retire either.
    pub(super) app_sync_dirty: bool,
    /// Logical "now" for the current `process_at()` batch — the single
    /// timestamp every state-affecting time read in the pipeline observes.
    ///
    /// Captured once at the top of [`Terminal::process_at`] (the public
    /// [`Terminal::process`] passes `Instant::now()`) and read by the bell
    /// rate-limiter, `sync_start` arming, and the mode-2026 timeout check.
    /// Routing those through one injected instant — instead of each calling
    /// `Instant::now()` independently — makes `process_at` replayable: feeding
    /// the same `(bytes, instant)` schedule reproduces identical grid/cursor/
    /// mode state regardless of real wall-clock pacing. The value is always
    /// overwritten before any reader runs, so its initial/reset value is never
    /// observed.
    pub(super) process_now: aterm_time::Instant,
    /// Wall-clock epoch milliseconds for the current `process_at()` batch — the
    /// single wall reading every shell-integration command/output mark records
    /// (OSC 133/633 marks B/C/D). Captured alongside [`process_now`] so replay
    /// reproduces identical `command_*_time_ms` values from the recorded
    /// schedule instead of re-reading the host clock. `None` when the platform
    /// clock is unavailable. Always overwritten before any reader runs.
    pub(super) process_wall_ms: Option<u64>,
    /// SGR attribute stack for XTPUSHSGR/XTPOPSGR.
    pub(super) sgr_stack: VecDeque<SgrStackEntry>,
    /// Per-frame pipeline timing for keystroke-to-pixel decomposition (#5560).
    pub(super) pipeline_timestamps: PipelineTimestamps,
    /// Whether the last combining character added was a ZWJ (U+200D).
    ///
    /// Used to fast-path `should_combine_with_previous_zwj` — the full grid
    /// lookup is only needed when this is true, which is <0.1% of characters.
    pub(super) last_combining_was_zwj: bool,
    /// Cached flag: true when `current_hyperlink.is_some() || current_underline_color.is_some()`.
    /// Avoids 2 per-character Option checks in `write_char_core`.
    pub(super) has_transient_extras: bool,
    /// Set by the RIS handler to carry the RIS side effects it cannot reach
    /// from inside the parser's dispatch loop — clearing secure keyboard entry
    /// and the parked selection — which the processing loop applies once the
    /// current `advance_fast` call returns (#7153). The name is historical: the
    /// parser is NOT reset (ESC c already leaves it in Ground, and a reset after
    /// the whole chunk dropped a sequence left unfinished at its end, audit K6).
    pub(super) pending_parser_reset: bool,
    /// SELECTION CUSTODY Phase 3 — the MAIN grid's `absolute_row_counter` at the
    /// instant this batch parked it (smcup). The SCR-1 epilogue re-pins that grid
    /// and needs the lines it took BEFORE the swap: output can precede an smcup in
    /// the same read, and those lines really did enter the main grid's scrollback.
    /// Last park wins — a batch that enters twice re-parks from the later state.
    pub(super) alt_park_main_row_counter: Option<u64>,
    /// SELECTION CUSTODY Phase 3 — a reading position flattened off a grid that was
    /// swapped back IN mid-batch (rmcup): `(the display_offset it was parked with,
    /// that grid's absolute_row_counter at the swap)`.
    ///
    /// The rest of the batch is written through `row_index`, which subtracts
    /// `display_offset`, so an incoming grid MUST be at 0 for the same reason the
    /// batch prologue forces the active one to 0. The epilogue re-pins from this,
    /// advancing by whatever entered that grid's scrollback after the swap.
    pub(super) alt_restore_pin: Option<(usize, u64)>,
    /// SELECTION CUSTODY Phase 3 — did this batch LEAVE the alt screen at any point?
    /// `post_process` keys park/restore on the batch's start and end screen only, so
    /// an exit followed by a re-entry runs neither arm; this is how it learns that
    /// the alt buffer the current selection names has been destroyed in between.
    pub(super) alt_screen_left_in_batch: bool,
    /// The host asked for the PRIMARY screen (`Terminal::leave_alternate_screen`,
    /// the `mainscreen` control verb): an app died on the alternate screen and
    /// its `?1049l` never came. Taken by the next batch's parser advance, which
    /// leaves the screen from inside the batch so the swap gets the same
    /// `post_process` and epilogue treatment as one the app sent.
    pub(super) host_leave_alternate_screen: bool,
    /// The alternate screen now up was entered by `?47h`/`?1047h`, which save no
    /// cursor, rather than by `?1049h`. Read by the orphaned-screen leave alone,
    /// so it exits the way the dead app's own reset would have: those modes carry
    /// the alt cursor over, where a 1049 exit would restore the main slot — and
    /// that slot can still hold an EARLIER 1049 app's save (a restore leaves it
    /// set), which would put the prompt on a row from long ago. Not checkpointed:
    /// a restored session takes the 1049 exit, the form nearly every app uses.
    pub(super) alt_entered_without_cursor_save: bool,
    /// An orphaned alternate screen was left while a Windows console host sits
    /// between us and the app (`win32_input_mode`, which only conhost asks for):
    /// OUR screen is the primary one again, but conhost's own alternate buffer is
    /// still up — nothing ever told it to leave — and everything the shell prints
    /// goes into that buffer. MEASURED (2026-09-27, Windows 11 ConPTY, the `cast`
    /// tap): conhost's next alt-screen enter arrived as a full blank repaint of
    /// the viewport followed by `?1049h` in one read, and its exit as `?1049l`
    /// followed by a full repaint of its PRIMARY buffer, frozen at the dead
    /// app's start. Both repaints land on our primary screen and overwrote every
    /// row the shell printed after the recovery (two commands and their output,
    /// gone from screen and scrollback). Set by the orphaned leave, which moves
    /// the rows from before the recovery out of reach itself
    /// (`TerminalHandler::leave_orphaned_alternate_screen`); taken by the first
    /// of those two events, which first moves the screen's rows into the
    /// scrollback (`TerminalHandler::keep_screen_from_conhost_repaint`).
    pub(super) conhost_alt_screen_left_up: bool,
    /// XTSAVE (CSI ? Ps s) saved DEC private mode values.
    ///
    /// Maps mode number to its saved boolean state. Restored by XTRESTORE
    /// (CSI ? Ps r). Cleared on terminal reset. Part of #7318.
    pub(super) xtsave_modes: XtsaveModesMap,
    /// Whether the most recent OSC was terminated by BEL (0x07) rather than ST.
    ///
    /// Used by OSC 52 clipboard query responses to echo the same terminator
    /// for compatibility with programs that only recognize BEL-terminated
    /// responses (#7548).
    pub(super) last_osc_bel_terminated: bool,
    /// Kitty graphics protocol image store: client image id → decoded image data
    /// (KITTY-CORE). A `t`/`T` transmission inserts here (capped — see
    /// `MAX_KITTY_IMAGES` / `MAX_KITTY_STORE_BYTES` in the handler); a `p` display
    /// looks it up; a `d` delete removes. Transient (not checkpointed): images are
    /// ephemeral and re-sent by the application after a restore.
    pub(super) kitty_images: HashMap<u32, Arc<ImageData>>,
    /// Kitty ANIMATION frame store: image id → its frames (frame 0 is the base
    /// transmit; `a=f` appends or edits one, `a=c` composes onto one).
    /// `kitty_images[id]` always mirrors the CURRENT frame, so the render path is
    /// frame-agnostic; `a=a c=N` re-points it at frame N, and every placement of
    /// the image with it. Capped by `MAX_KITTY_FRAMES` in the handler.
    pub(super) kitty_frames: HashMap<u32, Vec<Arc<ImageData>>>,
    /// Kitty PLACEMENT VARIANTS: image id → re-laid-out copies of its image for
    /// `a=p` displays whose footprint, z-index or source rectangle differ from
    /// the stored image's (a footprint lives in `ImageData`, so a different one
    /// is a different payload). Charged to `kitty_total_bytes` like any slot; a
    /// variant nothing else holds any more is dropped the next time one is
    /// added, and all of them go with their id.
    pub(super) kitty_variants: HashMap<u32, Vec<KittyVariant>>,
    /// Kitty VIRTUAL placements (`U=1`): image id → its virtual placements,
    /// newest last. Each is what a Unicode placeholder naming it draws. Holds
    /// only the stored image or a charged variant, so it adds nothing to the
    /// budget; capped per image by `MAX_KITTY_VIRTUALS` in the handler.
    pub(super) kitty_virtual: HashMap<u32, Vec<KittyVirtual>>,
    /// Kitty RELATIVE placements (`P=`/`Q=`) on screen: each one with its
    /// parent and its offset. A relative placement moves with its parent and
    /// is deleted with it. Pruned to the placements still on screen whenever
    /// one is added.
    pub(super) kitty_relations: Vec<KittyRelation>,
    /// Kitty image NUMBERS (`I=`): number → the id of the NEWEST image
    /// transmitted under it (the terminal picks that id and reports it).
    pub(super) kitty_numbers: HashMap<u32, u32>,
    /// The last id the terminal assigned to an `I=` transmission; the next
    /// assignment probes upward from here past ids already in use.
    pub(super) kitty_last_assigned_id: u32,
    /// The last Kitty placement serial handed out (see
    /// `aterm_grid::KittyPlacementTag::serial`).
    pub(super) kitty_placement_serial: u32,
    /// Running total of `ImageData.bytes.len()` summed across every stored slot
    /// in `kitty_images` + `kitty_frames` + `kitty_variants` (each slot counted
    /// independently, even when a base-transmit Arc is shared between
    /// `kitty_images[id]` and `kitty_frames[id][0]`). The handler enforces a
    /// GLOBAL `MAX_KITTY_STORE_BYTES` ceiling against this so the per-item caps
    /// (`MAX_KITTY_IMAGES` count, `MAX_KITTY_FRAMES` and `MAX_KITTY_VARIANTS` per
    /// id, `MAX_KITTY_IMAGE_BYTES` per image) can no longer multiply into a
    /// multi-GiB resident OOM — mirroring the DCS / inline-image total budgets.
    /// Decremented on delete/clear and on base-transmit replacement.
    pub(super) kitty_total_bytes: usize,
    /// In-flight Kitty CHUNKED transmission (`m=1`): the first chunk's command
    /// (control metadata) with its `payload` growing as continuation chunks append,
    /// finalized on the `m=0` chunk. `None` between transmissions. Bounded by
    /// `MAX_KITTY_IMAGE_BYTES` in the handler.
    pub(super) kitty_pending: Option<crate::terminal::kitty_graphics::KittyCommand>,
    /// Edge-triggered BEL flag set by `handle_bell()` (after its rate-limit),
    /// drained read+clear by `Terminal::drain_bell()`. Lets a polling host (the
    /// wasm renderer) detect a bell without wiring the synchronous `bell_callback`.
    pub(super) bell_pending: bool,
    /// App-event queue: REAL OSC payloads (code, payload) the host polls via
    /// `Terminal::take_osc_event` — OSC 52 decoded clipboard, OSC 7 cwd path,
    /// OSC 133 shell mark. Distinct from `response_buffer` (PTY replies); drained
    /// like `take_response()`. Capped in `queue_osc_event`. Cleared on reset.
    pub(super) osc_events: VecDeque<(u32, String)>,
    /// Where the most recent PTY PRINT run ended — the active-grid cursor
    /// `(row, col)` sampled immediately after each parser print action
    /// (`print` / `print_ascii_bulk` / `print_unicode_bulk`). This is the
    /// ECHO ANCHOR the cursor-effect host samples (`Terminal::print_anchor`):
    /// a TUI whose repaint bracket hides or parks the DEC cursor still ends
    /// each keystroke's whole-row rewrite exactly `typed cells` past the
    /// previous rewrite's end, so end-to-end is the echo sweep the hidden
    /// cursor cannot witness. Observability only — never read by any parser
    /// or grid decision. Cleared on reset.
    pub(super) print_anchor: Option<(u16, u16)>,
    /// Monotonic count of print actions, paired with [`Self::print_anchor`]
    /// so an unchanged end position still reads as "output landed". Never
    /// reset to zero (a host compares equality against its last-seen value,
    /// and a reset that replayed an old seq could alias a stale sample).
    pub(super) print_anchor_seq: u64,
}

impl TransientState {
    pub(super) fn new() -> Self {
        Self {
            response_buffer: Vec::new(),
            response_rate_limiter: ResponseRateLimiter::new(),
            last_graphic_char: None,
            current_hyperlink: None,
            current_hyperlink_id: None,
            current_underline_color: None,
            vt52_cursor_state: Vt52CursorState::None,
            sync_start: None,
            sync_end_seq: 0,
            reset_generation: 0,
            full_clears: 0,
            screen_replaced: 0,
            sync_open_dirty: false,
            app_sync_open: false,
            app_sync_dirty: false,
            // Placeholders; overwritten at the top of every process_at() before
            // any reader runs, so this value is never observed as state.
            // aterm_time::Instant::now(): std on native, JS clock on wasm (std panics there).
            // The marker below is on the CALL line on purpose: grep_guard family C
            // requires the exemption same-line, and on the preceding line it read as
            // an unexplained clock read (which is what the module-wide C3 now sees).
            process_now: aterm_time::Instant::now(), // CLOCK-EXEMPT: seed only
            process_wall_ms: None,
            sgr_stack: VecDeque::new(),
            pipeline_timestamps: PipelineTimestamps::default(),
            last_combining_was_zwj: false,
            has_transient_extras: false,
            pending_parser_reset: false,
            alt_park_main_row_counter: None,
            alt_restore_pin: None,
            alt_screen_left_in_batch: false,
            host_leave_alternate_screen: false,
            alt_entered_without_cursor_save: false,
            conhost_alt_screen_left_up: false,
            xtsave_modes: XtsaveModesMap::default(),
            last_osc_bel_terminated: false,
            kitty_images: HashMap::new(),
            kitty_frames: HashMap::new(),
            kitty_variants: HashMap::new(),
            kitty_virtual: HashMap::new(),
            kitty_relations: Vec::new(),
            kitty_numbers: HashMap::new(),
            kitty_last_assigned_id: 0,
            kitty_placement_serial: 0,
            kitty_total_bytes: 0,
            kitty_pending: None,
            bell_pending: false,
            osc_events: VecDeque::new(),
            print_anchor: None,
            print_anchor_seq: 0,
        }
    }

    /// Recompute the cached `has_transient_extras` flag.
    #[inline]
    pub(super) fn update_has_transient_extras(&mut self) {
        self.has_transient_extras =
            self.current_hyperlink.is_some() || self.current_underline_color.is_some();
    }

    /// Clear all transient state (called during terminal reset).
    pub(super) fn reset(&mut self) {
        self.response_buffer.clear();
        self.last_graphic_char = None;
        self.current_hyperlink = None;
        self.current_hyperlink_id = None;
        self.current_underline_color = None;
        self.vt52_cursor_state = Vt52CursorState::default();
        // A reset CLOSES any open sync window — bump the close counter (never
        // zero it: a host comparing across the reset must still see "closed").
        if self.sync_start.is_some() {
            self.sync_end_seq += 1;
        }
        // Never zeroed either: the alt-screen archive keys its wipe on a change.
        self.reset_generation = self.reset_generation.wrapping_add(1);
        // A full reset replaces the whole screen; the counter itself survives.
        // `full_clears` is not touched: a reader keys on either advancing.
        self.screen_replaced = self.screen_replaced.wrapping_add(1);
        self.sync_start = None;
        self.sync_open_dirty = false;
        self.app_sync_open = false;
        self.app_sync_dirty = false;
        self.sgr_stack.clear();
        self.pipeline_timestamps = PipelineTimestamps::default();
        self.last_combining_was_zwj = false;
        self.has_transient_extras = false;
        self.pending_parser_reset = false;
        self.alt_park_main_row_counter = None;
        self.alt_restore_pin = None;
        self.alt_screen_left_in_batch = false;
        self.host_leave_alternate_screen = false;
        self.alt_entered_without_cursor_save = false;
        self.conhost_alt_screen_left_up = false;
        self.xtsave_modes.clear();
        self.last_osc_bel_terminated = false;
        self.bell_pending = false;
        self.osc_events.clear();
        // The echo anchor names a pre-reset coordinate space; the seq is
        // deliberately NOT rezeroed (see its field doc).
        self.print_anchor = None;
        // RIS clears the Kitty graphics store (matching kitty/xterm and the
        // `a=d` delete-all path); the global byte budget must drop in lockstep
        // so it cannot drift from the now-empty store and wrongly reject a later
        // valid in-budget image, and no stale id can still display post-reset.
        self.kitty_images.clear();
        self.kitty_frames.clear();
        self.kitty_variants.clear();
        self.kitty_virtual.clear();
        self.kitty_relations.clear();
        self.kitty_numbers.clear();
        self.kitty_total_bytes = 0;
        // Also abandon any IN-FLIGHT chunked transmission (`m=1`) accumulator: a
        // partial pre-reset transfer left here would be silently merged into the FIRST
        // post-reset Kitty command (handle_kitty_command branches on
        // `kitty_pending.is_some() || cmd.more`), gluing the new payload onto the stale
        // chunk and finalizing it with the pre-reset metadata — corrupting a legitimate
        // post-reset image and retaining up to MAX_KITTY_IMAGE_BYTES across a reset that
        // must free everything. Matches kitty/xterm dropping partial transfers on RIS.
        self.kitty_pending = None;
    }
}

/// One re-laid-out copy of a Kitty image (see `TransientState::kitty_variants`).
#[derive(Debug)]
pub(super) struct KittyVariant {
    /// The image (frame) it was cut from: a variant is reused only for the
    /// same source and layout.
    pub(super) source: Arc<ImageData>,
    /// The copy the put displays.
    pub(super) image: Arc<ImageData>,
}

/// One Kitty VIRTUAL placement (see `TransientState::kitty_virtual`).
#[derive(Debug, Clone)]
pub(super) struct KittyVirtual {
    /// Its placement id (`p=`); `0` when the client named none.
    pub(super) placement_id: u32,
    /// The image laid out as its put asked (`c=`/`r=`/crop/`z=`), for the
    /// image's current frame.
    pub(super) image: Arc<ImageData>,
}

/// One Kitty RELATIVE placement (see `TransientState::kitty_relations`).
#[derive(Debug, Clone, Copy)]
pub(super) struct KittyRelation {
    /// The relative placement: its image, placement id and serial.
    pub(super) child: aterm_grid::KittyPlacementTag,
    /// The placement it is relative to.
    pub(super) parent: KittyParent,
    /// Its offset from the parent's top-left cell, in columns (`H=`).
    pub(super) dx: i32,
    /// Its offset from the parent's top-left cell, in rows (`V=`).
    pub(super) dy: i32,
}

/// Which placement a relative placement follows: by `(image id, placement
/// id)` when the parent has a placement id — a re-put MOVES that placement,
/// and its children follow it — else by the parent's one-off serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KittyParent {
    /// The parent's image id and non-zero placement id.
    Named {
        /// The parent's image id.
        image_id: u32,
        /// The parent's placement id (never `0`).
        placement_id: u32,
    },
    /// An unnamed parent's serial.
    Serial(std::num::NonZeroU32),
}

impl KittyParent {
    /// The key a relative placement of `parent` follows.
    pub(super) const fn of(parent: aterm_grid::KittyPlacementTag) -> Self {
        if parent.placement_id == 0 {
            Self::Serial(parent.serial)
        } else {
            Self::Named {
                image_id: parent.image_id,
                placement_id: parent.placement_id,
            }
        }
    }

    /// Whether `tag` is this parent.
    pub(super) fn names(self, tag: &aterm_grid::KittyPlacementTag) -> bool {
        match self {
            Self::Named {
                image_id,
                placement_id,
            } => tag.image_id == image_id && tag.placement_id == placement_id,
            Self::Serial(serial) => tag.serial == serial,
        }
    }
}
