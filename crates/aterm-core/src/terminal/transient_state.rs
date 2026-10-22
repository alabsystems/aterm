// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Transient terminal state: the fields `reset_common_fields` clears together
//! ([`TransientState`]).

use super::response_rate_limiter::ResponseRateLimiter;
use super::types::SgrStackEntry;
use aterm_types::{PipelineTimestamps, Rgb};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use aterm_grid::ImageData;

// XTSAVE saved DEC private modes.
type XtsaveModesMap = HashMap<u16, bool>;

/// Default foreground (light grey), single-sourced from [`aterm_types::DEFAULT_FOREGROUND`]
/// so the runtime state and `TerminalConfig::default()` agree.
pub(super) const DEFAULT_FOREGROUND: Rgb = aterm_types::DEFAULT_FOREGROUND;

/// Default background (black); see [`DEFAULT_FOREGROUND`].
pub(super) const DEFAULT_BACKGROUND: Rgb = aterm_types::DEFAULT_BACKGROUND;

/// VT52 cursor addressing state: ESC Y row col collects two parameter bytes.
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

/// The cell at the latest ordered synchronized-output close. Session-only:
/// a restore starts without one, and a timeout does not manufacture one.
#[derive(Clone, Copy, Debug)]
pub(super) struct SyncEndCaret {
    pub(super) seq: u64,
    pub(super) screen: u64,
    pub(super) grid: (u16, u16),
    pub(super) caret: (u16, u16),
}

/// Grouped transient terminal state. [`reset`](Self::reset) bumps `reset_generation` and
/// `screen_replaced` and clears the rest, except the replies and reports already owed to
/// the PTY peer (`response_buffer`, `report_buffer`) and the replies' rate limiter; the
/// monotonic counts and serials, which never
/// rewind (`sync_end_seq`, `full_clears`, `screen_mutations`, `kitty_serial`,
/// `print_anchor_seq`, `focus_arms`); the per-batch clocks, which each batch overwrites before any reader
/// runs; `kitty_last_assigned_id`, where the next `I=` id's search starts; and
/// `host_resyncs_alt_screen`, the embedder's host policy. New resettable fields go here
/// and in `reset`.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent terminal flags, not a state machine"
)]
pub(super) struct TransientState {
    /// Response buffer for DSR/DA and other terminal responses. Not reset: a reply
    /// already generated is owed to the PTY peer, so a query's answer cannot depend on
    /// whether a later RIS arrived in the same read.
    pub(super) response_buffer: Vec<u8>,
    /// The reports the terminal sends unasked, at a host's change rather than to
    /// answer a sequence: DEC 2048 in-band size reports and DSR 997 colour-scheme
    /// pushes. Kept apart from `response_buffer` so a host can send them as the
    /// input they are ([`super::Terminal::take_reports`]); `take_response` still
    /// hands over both. Shares that buffer's cap ([`Self::push_report`]). Not
    /// reset, for the same reason.
    pub(super) report_buffer: Vec<u8>,
    /// Token-bucket rate limiter gating `send_response`, so a peer spamming
    /// DSR/DA/DECRQSS cannot force unlimited responses even when the host drains the
    /// buffer in a tight loop. Not reset: an RIS would refill the bucket on demand.
    pub(super) response_rate_limiter: ResponseRateLimiter,
    /// Last graphic character received (for REP - CSI b). Stored raw
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
    /// Monotonic count of synchronized-update (mode 2026) closes: every
    /// `?2026l`, DECSTR/RIS reset and timeout force-clear. A host holding
    /// presents compares it across redraws to tell a closed bracket from one
    /// still open, which the mode level cannot under back-to-back brackets.
    /// Never reset to 0; resets bump it.
    pub(super) sync_end_seq: u64,
    /// Cursor cell at the latest ordered close, qualified by its count, screen
    /// and grid. A later cursor move does not change where that frame ended.
    pub(super) sync_end_caret: Option<SyncEndCaret>,
    /// Monotonic count of full resets (RIS and [`Terminal::reset`], both via
    /// `reset_common_fields`), never zeroed. The alt-screen archive compares it
    /// at every commit point to learn a reset wiped the screen, without the
    /// handler holding the archive.
    ///
    /// [`Terminal::reset`]: super::Terminal::reset
    pub(super) reset_generation: u64,
    /// Monotonic count of full clears: ED 2, and ED 0 with the cursor home
    /// (`resize_journal::is_full_clear`; ED 3 erases only history). Signals that
    /// an app repainted a displaced screen from scratch; each resize report
    /// carries its value for readers to compare. Never reset to 0.
    pub(super) full_clears: u64,
    /// Monotonic count of whole-screen replacements: alternate-screen enter and
    /// exit (47/1047/1049) and full resets. A swapped or wiped screen no longer
    /// holds the rows a resize displaced. Never reset to 0.
    pub(super) screen_replaced: u64,
    /// Monotonic count of the program's DEC 1004 enables: each `CSI ? 1004 h` (or
    /// XTRESTORE) that turns focus reporting on from off. Published lock-free
    /// ([`super::ModeMirror::focus_arms`]) so the host tells a program that turned it on
    /// in a session without the keyboard that it is unfocused, at once, rather than
    /// leaving it to believe the focus it assumes until the next focus change. Never
    /// reset; it wraps.
    pub(super) focus_arms: u32,
    /// The engine's render debt ([`RenderDebt`](super::RenderDebt)): what the
    /// alternate-screen, non-reflowed resizes run under `ResizePolicy::Native` since the
    /// screen was last repainted from scratch (a full clear, a screen switch, a reset)
    /// moved or cut. Kept beside the two counters that zero it, so it stays exact however
    /// far the 16-entry journal is overrun. Session-only: a checkpoint restore starts at
    /// nothing owed and the host seeds it
    /// ([`Terminal::seed_render_debt`](super::Terminal::seed_render_debt)).
    pub(super) render_debt: super::RenderDebt,
    /// Monotonic count of non-empty batches [`process_at`](super::Terminal::process_at)
    /// consumed, whatever they changed, plus every Kitty animation frame step
    /// ([`advance_kitty_animations`](super::Terminal::advance_kitty_animations)).
    /// `content_seq` misses some mutations (a VS15/VS16 width fix, an OSC 1337 image
    /// mark, a Kitty image transmitted or deleted under a placeholder cell, a frame step),
    /// so a reader that must never call a changed screen unchanged keys on this as well.
    /// Never reset to 0.
    pub(super) screen_mutations: u64,
    /// Whether the current synchronized-output window has accepted any complete PTY
    /// action since its `?2026h`. A host may present a just-closed frame while the mode
    /// already reads true again only while this is false: once the new window is dirty,
    /// the grid may hold a prefix of the next frame. Parser queries and no-ops count too,
    /// so the close+reopen present fails closed.
    pub(super) sync_open_dirty: bool,
    /// Whether the program's synchronized-output bracket is open by its own
    /// account: set by `?2026h`, cleared only by `?2026l`, DECSTR or RIS. Unlike
    /// `modes.synchronized_output` it survives the timeout force-clear. Read via
    /// [`Terminal::sync_frame_unfinished`](super::Terminal::sync_frame_unfinished).
    pub(super) app_sync_open: bool,
    /// Whether the program's open bracket (`app_sync_open`) has accepted any
    /// complete PTY action; the program-view twin of `sync_open_dirty`.
    pub(super) app_sync_dirty: bool,
    /// Logical "now" for the current batch, captured once at the top of
    /// [`Terminal::process_at`](super::Terminal::process_at). The bell rate limiter,
    /// `sync_start` and the mode-2026 timeout read it instead of the clock, so a
    /// `(bytes, instant)` schedule replays identically.
    pub(super) process_now: aterm_time::Instant,
    /// Wall-clock epoch milliseconds for the current batch, recorded by OSC 133/633
    /// marks B/C/D; captured with `process_now` so replay reproduces the
    /// `command_*_time_ms` values. `None` without a platform clock.
    pub(super) process_wall_ms: Option<u64>,
    /// SGR attribute stack for XTPUSHSGR/XTPOPSGR.
    pub(super) sgr_stack: VecDeque<SgrStackEntry>,
    /// Per-frame pipeline timing for keystroke-to-pixel decomposition.
    pub(super) pipeline_timestamps: PipelineTimestamps,
    /// Whether the last combining character added was a ZWJ (U+200D); gates the
    /// grid lookup in `should_combine_with_previous_zwj`.
    pub(super) last_combining_was_zwj: bool,
    /// Cached `current_hyperlink.is_some() || current_underline_color.is_some()`, so
    /// `write_char_core` makes one check per character instead of two.
    pub(super) has_transient_extras: bool,
    /// Set by the RIS handler for side effects it cannot perform inside the
    /// parser's dispatch (clearing secure keyboard entry and the parked
    /// selection); applied when the current `advance_fast` returns. The parser
    /// itself is not reset (ESC c already leaves it in Ground).
    pub(super) pending_parser_reset: bool,
    /// The main grid's `absolute_row_counter` when this batch parked it
    /// (smcup). The epilogue re-pin needs the lines the grid took before the
    /// swap in the same read. Last park wins.
    pub(super) alt_park_main_row_counter: Option<u64>,
    /// A reading position flattened off a grid swapped back in mid-batch
    /// (rmcup): `(the display_offset it was parked with, its
    /// absolute_row_counter at the swap)`. The rest of the batch writes through
    /// `row_index`, which subtracts `display_offset`, so the grid runs at 0 and
    /// the epilogue re-pins from this.
    pub(super) alt_restore_pin: Option<(usize, u64)>,
    /// Whether this batch left the alt screen at any point. `post_process`
    /// keys park/restore on start and end screens only, so an exit plus re-entry
    /// runs neither arm; this reports that the alt buffer was destroyed between.
    pub(super) alt_screen_left_in_batch: bool,
    /// The host asked for the primary screen (`Terminal::leave_alternate_screen`,
    /// the `mainscreen` verb) after an app died on the alternate screen. Taken
    /// by the next batch's parser advance, so the swap gets the same
    /// `post_process` and epilogue treatment as an in-band one.
    pub(super) host_leave_alternate_screen: bool,
    /// The current alternate screen was entered by `?47h`/`?1047h` (no cursor
    /// save) rather than `?1049h`. Read only by the orphaned-screen leave, which
    /// then keeps the alt cursor instead of restoring a main slot that may hold
    /// an earlier app's save. Not checkpointed: a restored session judges by the
    /// main slot alone, taking the 1047 exit when it is empty (no `?1049h` entered)
    /// and the 1049 exit when it holds a save, which may be an earlier app's.
    pub(super) alt_entered_without_cursor_save: bool,
    /// An orphaned alternate screen was left while a Windows console host sits between us
    /// and the app (`win32_input_active`): our screen is primary again but conhost's
    /// alternate buffer is still up. Its next alt-screen enter (a blank repaint then
    /// `?1049h`, in one read) or exit (`?1049l` then a repaint of its frozen primary)
    /// would overwrite what the shell printed since. Set by
    /// `TerminalHandler::leave_orphaned_alternate_screen`; taken by the first of those
    /// events, which first moves the screen's rows into the scrollback
    /// (`TerminalHandler::keep_screen_from_conhost_repaint`).
    pub(super) conhost_alt_screen_left_up: bool,
    /// HOST POLICY (the embedder's, set by `Terminal::set_host_resyncs_alt_screen`; kept
    /// across RIS, which `reset` leaves it out of): the console host between us and the app
    /// forwards an application's alternate-screen switches verbatim and never repaints the
    /// buffer it switches to, as Windows Terminal's OpenConsole does. Under such a host a
    /// LOCAL leave of an orphaned alternate screen puts us out of step with the host, which
    /// stays on its dead buffer. The 2026-09-29 snapshot recorded it (OpenConsole 1.24,
    /// measured 2026-09-28): the next full-screen app's `?1049l` returned the host to its
    /// primary buffer with the dead app's saved cursor and emitted only those 8 bytes, while
    /// a local leave had left our cursor elsewhere, so every absolute move the shell's line
    /// editor made afterwards landed on the wrong row. So a prompt on the alternate screen
    /// REQUESTS a host resync ([`Self::alt_screen_resync_requested`]) instead of leaving:
    /// the host's own `?1049l`, forwarded, then leaves both screens from the same bytes.
    /// Off by default, which is the local leave every other host gets.
    pub(super) host_resyncs_alt_screen: bool,
    /// A resync the embedder has not taken yet (`Terminal::take_alt_screen_resync_request`).
    pub(super) alt_screen_resync_requested: bool,
    /// [`Self::screen_replaced`] when the last resync was requested: one request per stuck
    /// screen. A resync that never arrives leaves the screen up (as Windows Terminal leaves
    /// it), and later prompts on it do not ask again.
    pub(super) alt_screen_resync_screen: Option<u64>,
    /// The alternate screen ([`Self::screen_replaced`] while it is up) a shell prompt proved
    /// dead under the host policy, or the embedder is making its host leave
    /// (`Terminal::expect_host_alt_screen_leave`): the `?1049l` that leaves THAT screen is
    /// the host's forwarded switch, and it releases the modes the dead app left on as the
    /// local leave does (`TerminalHandler::take_dead_alt_screen`). Taken by that switch;
    /// cleared by a reset and when the policy goes off.
    pub(super) dead_alt_screen: Option<u64>,
    /// Where the shell's latest prompt on a dead alternate screen started: the screen
    /// ([`Self::screen_replaced`] while it was up) and the cursor at the `A` that proved it
    /// dead under the host policy. Read by `Terminal::dead_alt_screen_carry`, which hands
    /// the prompt the shell drew there to the primary screen the host's leave brings back.
    /// Stale once that screen is gone (the screen id no longer matches); cleared by a reset.
    pub(super) dead_alt_prompt_at: Option<(u64, u16, u16)>,
    /// DEC 2048's enable-time size report is owed but held: the enable came on the primary
    /// screen behind a Windows console host, where the reader is as likely the shell as the
    /// program that asked (`TerminalHandler::hold_size_report`). Sent when the alternate
    /// screen comes up with the mode still on; dropped by `?2048l`, by a shell prompt (the
    /// program that asked has ended) and by a reset.
    pub(super) size_report_held: bool,
    /// The last DECSET/DECRST 2004 arrived after the shell's `133;D` and before its next
    /// `A`: the shell's own set-up for the prompt it is drawing, which the `A` that proves a
    /// full-screen app dead leaves alone (`TerminalHandler::release_dead_app_paste`).
    /// Cleared by a reset.
    pub(super) bracketed_paste_from_prompt: bool,
    /// XTSAVE (`CSI ? Ps s`) saved DEC private mode values, restored by
    /// XTRESTORE (`CSI ? Ps r`). Cleared on reset.
    pub(super) xtsave_modes: XtsaveModesMap,
    /// Whether the most recent OSC was terminated by BEL (0x07) rather than ST;
    /// responses echo the same terminator for programs that only recognize BEL.
    pub(super) last_osc_bel_terminated: bool,
    /// Kitty graphics image store: client image id to decoded image. `t`/`T`
    /// inserts (capped by `MAX_KITTY_IMAGES` / `MAX_KITTY_STORE_BYTES`), `p`
    /// looks up, `d` removes. Not checkpointed: apps re-send images after a
    /// restore.
    pub(super) kitty_images: HashMap<u32, Arc<ImageData>>,
    /// Kitty animations: image id to its frames (frame 0 is the base transmit;
    /// `a=f` appends or edits one, `a=c` composes onto one), playback settings
    /// and z-order among images of one z-index. `kitty_images[id]` always
    /// mirrors the current frame, so rendering is frame-agnostic; `a=a c=N`
    /// selects frame N and playback ([`KittyAnimation`]) steps it for every
    /// placement. Capped by `MAX_KITTY_FRAMES`.
    pub(super) kitty_frames: HashMap<u32, KittyAnimation>,
    /// Kitty placement variants: image id to re-laid-out copies for `a=p`
    /// displays whose footprint, z-index or source rectangle differ from the
    /// stored image's, oldest first. Charged to `kitty_total_bytes`. A variant no
    /// placement shows is a reusable cache entry, dropped first when the store
    /// needs room; all go with their id.
    pub(super) kitty_variants: HashMap<u32, Vec<KittyVariant>>,
    /// Kitty virtual placements (`U=1`): image id to its virtual placements,
    /// newest last, each drawn where a Unicode placeholder names it. Holds only
    /// the stored image or a charged variant; capped per image by
    /// `MAX_KITTY_VIRTUALS`.
    pub(super) kitty_virtual: HashMap<u32, Vec<KittyVirtual>>,
    /// Kitty relative placements (`P=`/`Q=`) on screen, each with its parent
    /// and offset; they move and are deleted with the parent. Pruned to on-screen
    /// placements whenever one is added.
    pub(super) kitty_relations: Vec<KittyRelation>,
    /// Kitty placements relative to a virtual placement (or to another such
    /// placement), oldest first: placed each frame where the virtual ancestor's
    /// placeholders show and deleted with their parent. Capped by
    /// `MAX_KITTY_FLOATING`.
    pub(super) kitty_floating: Vec<KittyFloating>,
    /// Kitty image numbers (`I=`): number → the id of the newest image
    /// transmitted under it (the terminal picks that id and reports it).
    pub(super) kitty_numbers: HashMap<u32, u32>,
    /// The last id the terminal assigned to an `I=` transmission; the next
    /// assignment probes upward from here past ids already in use.
    pub(super) kitty_last_assigned_id: u32,
    /// The last Kitty serial handed out: to a placement act
    /// (`aterm_grid::KittyPlacementTag::serial`) or to an image first
    /// transmitted (`KittyPlacementTag::image_serial`).
    pub(super) kitty_serial: u32,
    /// Running total of `ImageData.bytes.len()` over every slot in `kitty_images`,
    /// `kitty_frames` and `kitty_variants` (a shared base-transmit Arc counts each time),
    /// held under `MAX_KITTY_STORE_BYTES` so the per-item caps cannot multiply into a
    /// multi-GiB footprint.
    pub(super) kitty_total_bytes: usize,
    /// In-flight Kitty chunked transmission (`m=1`): the first chunk's command, its
    /// `payload` growing per chunk, finalized on `m=0`. Bounded by `MAX_KITTY_IMAGE_BYTES`.
    /// Dropped by a command that ends it (`KittyCommand::during_load`), a canceled chunk,
    /// a shell prompt or command start, and RIS.
    pub(super) kitty_pending: Option<crate::terminal::kitty_graphics::KittyCommand>,
    /// Per screen (`[main, alternate]`, indexed by `modes.alternate_screen`), an upper
    /// bound on the payload bytes of the inline images that screen shows, held under
    /// `MAX_LIVE_IMAGE_BYTES` by `TerminalHandler::admit_live_image` (which explains the
    /// bound). One per screen because a count sees only the active grid.
    pub(super) live_image_charge: [usize; 2],
    /// Edge-triggered BEL flag set by `handle_bell()` (after rate limiting) and
    /// drained by `Terminal::drain_bell()`, for polling hosts.
    pub(super) bell_pending: bool,
    /// App-event queue of OSC payloads `(code, payload)` the host polls via
    /// `Terminal::take_osc_event`: OSC 52 decoded clipboard, OSC 7 cwd, OSC 133 marks.
    pub(super) osc_events: OscEventQueue,
    /// Where the most recent PTY print run ended: the active-grid cursor `(row, col)`
    /// after each print action. The echo anchor the cursor-effect host samples
    /// (`Terminal::print_anchor`): a TUI that hides or parks the cursor still ends each
    /// keystroke's rewrite past the previous one's end. Cleared on reset.
    pub(super) print_anchor: Option<(u16, u16)>,
    /// Monotonic count of print actions, paired with [`Self::print_anchor`] so
    /// an unchanged end position still reads as new output. Never reset (a
    /// replayed old seq could alias a stale sample).
    pub(super) print_anchor_seq: u64,
    /// Monotonic count of kitty-graphics APC strings that ended (complete or cut short by
    /// CAN/SUB), every one counted whatever it did. Never reset. The crash-restore
    /// journal's recovery reads it around its replay
    /// ([`super::Terminal::kitty_graphics_commands`]).
    pub(super) kitty_commands: u64,
    /// Monotonic count of Kitty unscrolls (`CSI n + T`) run on the main screen, where
    /// they pull history onto it; never reset. Read by the crash-restore journal's
    /// recovery around its replay ([`super::Terminal::kitty_unscrolls`]).
    pub(super) kitty_unscrolls: u64,
}

impl TransientState {
    pub(super) fn new() -> Self {
        Self {
            response_buffer: Vec::new(),
            report_buffer: Vec::new(),
            response_rate_limiter: ResponseRateLimiter::new(),
            last_graphic_char: None,
            current_hyperlink: None,
            current_hyperlink_id: None,
            current_underline_color: None,
            vt52_cursor_state: Vt52CursorState::None,
            sync_start: None,
            sync_end_seq: 0,
            sync_end_caret: None,
            reset_generation: 0,
            full_clears: 0,
            screen_replaced: 0,
            focus_arms: 0,
            render_debt: super::RenderDebt::default(),
            screen_mutations: 0,
            sync_open_dirty: false,
            app_sync_open: false,
            app_sync_dirty: false,
            #[expect(
                clippy::disallowed_methods,
                reason = "a seed process_at overwrites before any reader runs"
            )]
            process_now: aterm_time::Instant::now(),
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
            host_resyncs_alt_screen: false,
            alt_screen_resync_requested: false,
            alt_screen_resync_screen: None,
            dead_alt_screen: None,
            dead_alt_prompt_at: None,
            size_report_held: false,
            bracketed_paste_from_prompt: false,
            xtsave_modes: XtsaveModesMap::default(),
            last_osc_bel_terminated: false,
            kitty_images: HashMap::new(),
            kitty_frames: HashMap::new(),
            kitty_variants: HashMap::new(),
            kitty_virtual: HashMap::new(),
            kitty_relations: Vec::new(),
            kitty_floating: Vec::new(),
            kitty_numbers: HashMap::new(),
            kitty_last_assigned_id: 0,
            kitty_serial: 0,
            kitty_total_bytes: 0,
            kitty_pending: None,
            live_image_charge: [0; 2],
            bell_pending: false,
            osc_events: OscEventQueue::default(),
            print_anchor: None,
            print_anchor_seq: 0,
            kitty_commands: 0,
            kitty_unscrolls: 0,
        }
    }

    /// Bytes owed to the PTY peer: the replies and the reports, which share
    /// [`super::MAX_RESPONSE_BUFFER_SIZE`].
    pub(super) fn owed_len(&self) -> usize {
        self.response_buffer
            .len()
            .saturating_add(self.report_buffer.len())
    }

    /// Queue `report` in `report_buffer` unless it would take what is owed past
    /// [`super::MAX_RESPONSE_BUFFER_SIZE`]: every caller writes past
    /// `send_response`'s rate limit, so a toggle flood or a resize storm
    /// against a host that does not drain must not grow it. Whether it went in.
    pub(super) fn push_report(&mut self, report: &[u8]) -> bool {
        let fits = self.owed_len().saturating_add(report.len()) <= super::MAX_RESPONSE_BUFFER_SIZE;
        if fits {
            self.report_buffer.extend_from_slice(report);
        }
        fits
    }

    /// Recompute the cached `has_transient_extras` flag.
    #[inline]
    pub(super) fn update_has_transient_extras(&mut self) {
        self.has_transient_extras =
            self.current_hyperlink.is_some() || self.current_underline_color.is_some();
    }

    /// Clear the transient state (called during terminal reset), keeping what
    /// the struct doc names.
    pub(super) fn reset(&mut self) {
        self.last_graphic_char = None;
        self.current_hyperlink = None;
        self.current_hyperlink_id = None;
        self.current_underline_color = None;
        self.vt52_cursor_state = Vt52CursorState::default();
        // A reset closes any open sync window. These counters are bumped, never zeroed:
        // a reader comparing across the reset must see the change.
        if self.sync_start.is_some() {
            self.sync_end_seq += 1;
        }
        self.reset_generation = self.reset_generation.wrapping_add(1);
        // A full reset replaces the whole screen; `full_clears` is untouched, since a
        // reader keys on either advancing.
        self.screen_replaced = self.screen_replaced.wrapping_add(1);
        self.render_debt = super::RenderDebt::default();
        self.sync_start = None;
        self.sync_end_caret = None;
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
        // A reset leaves the alternate screen: a pending resync is moot.
        // `host_resyncs_alt_screen` is host policy and stays.
        self.alt_screen_resync_requested = false;
        self.alt_screen_resync_screen = None;
        self.dead_alt_screen = None;
        self.dead_alt_prompt_at = None;
        // RIS turns 2048 off: nothing is owed.
        self.size_report_held = false;
        self.bracketed_paste_from_prompt = false;
        self.xtsave_modes.clear();
        self.last_osc_bel_terminated = false;
        self.bell_pending = false;
        self.osc_events.clear();
        // The echo anchor names a pre-reset coordinate space; its seq is never rezeroed.
        self.print_anchor = None;
        // RIS clears the Kitty graphics store (as kitty and xterm do) and its byte budget.
        self.kitty_images.clear();
        self.kitty_frames.clear();
        self.kitty_variants.clear();
        self.kitty_virtual.clear();
        self.kitty_relations.clear();
        self.kitty_floating.clear();
        self.kitty_numbers.clear();
        self.kitty_total_bytes = 0;
        // An in-flight chunked transmission (`m=1`) would otherwise merge into the first
        // post-reset Kitty command and keep up to MAX_KITTY_IMAGE_BYTES alive.
        self.kitty_pending = None;
        // Neither reset screen shows an image.
        self.live_image_charge = [0; 2];
    }
}

/// The poll-host app-event queue ([`TransientState::osc_events`]): OSC payloads
/// `(code, payload)` in arrival order, bounded by count and by total payload bytes.
///
/// A count cap alone would let 1024 OSC 52 sets of 64 KiB pin ~64 MiB per tab, and only
/// the wasm and gpu-web hosts drain this queue. Past either bound the oldest events are
/// dropped, so a host that polls late still sees the newest clipboard, cwd and mark.
#[derive(Debug, Default)]
pub(super) struct OscEventQueue {
    events: VecDeque<(u32, String)>,
    bytes: usize,
}

impl OscEventQueue {
    /// Most events held at once.
    pub(super) const MAX_EVENTS: usize = 1024;
    /// Most payload bytes held at once, summed over every queued event. Holds
    /// sixteen maximal OSC 52 sets; a single payload larger than this is not
    /// queued at all.
    pub(super) const MAX_BYTES: usize = 1024 * 1024;

    /// Append an event, first dropping the oldest ones it would push past a bound.
    pub(super) fn push(&mut self, code: u32, payload: String) {
        if payload.len() > Self::MAX_BYTES {
            return;
        }
        self.shed(1, payload.len());
        self.bytes += payload.len();
        self.events.push_back((code, payload));
    }

    /// Drop the oldest events until `events` more events of `bytes` more
    /// payload bytes fit within both bounds.
    fn shed(&mut self, events: usize, bytes: usize) {
        while self.events.len() + events > Self::MAX_EVENTS || self.bytes + bytes > Self::MAX_BYTES
        {
            if self.pop_front().is_none() {
                break;
            }
        }
    }

    /// Remove and return the oldest event.
    pub(super) fn pop_front(&mut self) -> Option<(u32, String)> {
        let event = self.events.pop_front()?;
        self.bytes -= event.1.len();
        Some(event)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
    }

    /// Keep only the events `keep` accepts.
    pub(super) fn retain(&mut self, mut keep: impl FnMut(u32, &str) -> bool) {
        let bytes = &mut self.bytes;
        self.events.retain(|(code, payload)| {
            let kept = keep(*code, payload);
            if !kept {
                *bytes -= payload.len();
            }
            kept
        });
    }

    /// Rewrite payloads in place; `rewrite` returns a replacement or `None` to
    /// leave the payload as it is.
    pub(super) fn rewrite(&mut self, mut rewrite: impl FnMut(u32, &str) -> Option<String>) {
        for (code, payload) in &mut self.events {
            if let Some(new) = rewrite(*code, payload) {
                self.bytes = self.bytes - payload.len() + new.len();
                *payload = new;
            }
        }
        // A rewritten payload can be longer (a row number gains a digit).
        self.shed(0, 0);
    }

    /// The queued events, oldest first.
    #[cfg(test)]
    pub(super) fn iter(&self) -> impl Iterator<Item = &(u32, String)> {
        self.events.iter()
    }

    /// Total payload bytes queued.
    #[cfg(test)]
    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }
}

/// One Kitty image's animation (see `TransientState::kitty_frames`): its
/// frames, root first, and how they play, as kitty plays them.
#[derive(Debug, Clone)]
pub(super) struct KittyAnimation {
    /// The frames, the root (the transmitted image) first.
    pub(super) frames: Vec<KittyFrame>,
    /// `s=`: stopped, running, or running while more frames load.
    pub(super) state: KittyPlayState,
    /// How many times the animation plays before it rests on its last frame
    /// (`v=` less one); `0` plays it forever.
    pub(super) max_loops: u32,
    /// Times it has come round since its state was last set.
    pub(super) loops: u32,
    /// When the frame shown went up: its gap runs from here.
    pub(super) shown_at: aterm_time::Instant,
    /// When the image was first transmitted: its place among images of one
    /// z-index (`aterm_grid::KittyPlacementTag::image_serial`).
    pub(super) image_serial: std::num::NonZeroU32,
}

impl KittyAnimation {
    /// A still image: `root` alone, stopped, shown since `now`, first
    /// transmitted at `image_serial`.
    pub(super) fn new(
        root: Arc<ImageData>,
        image_serial: std::num::NonZeroU32,
        now: aterm_time::Instant,
    ) -> Self {
        Self {
            frames: vec![KittyFrame {
                image: root,
                // Kitty gives the root no gap: a client sets one with `a=a`.
                gap_ms: 0,
            }],
            state: KittyPlayState::Stopped,
            max_loops: 0,
            loops: 0,
            shown_at: now,
            image_serial,
        }
    }

    /// Whether it steps frames on its own: not stopped, more than one frame,
    /// some frame with a gap (a gapless frame is never shown), and loops left.
    pub(super) fn plays(&self) -> bool {
        self.state != KittyPlayState::Stopped
            && self.frames.len() > 1
            && self.frames.iter().any(|frame| frame.gap_ms > 0)
            && (self.max_loops == 0 || self.loops < self.max_loops)
    }
}

/// One frame of a Kitty animation.
#[derive(Debug, Clone)]
pub(super) struct KittyFrame {
    /// Its pixels, laid out as the root is.
    pub(super) image: Arc<ImageData>,
    /// Milliseconds it shows before the next frame while the animation runs;
    /// `0` is a gapless frame, stepped over without being shown.
    pub(super) gap_ms: u32,
}

/// A Kitty animation's `s=` state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KittyPlayState {
    /// `s=1` (and a fresh image): frames change only when `c=` selects one.
    Stopped,
    /// `s=2`: runs, and at the last frame waits for more instead of looping.
    Loading,
    /// `s=3`: runs, looping back to the first frame after the last.
    Running,
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

/// One Kitty virtual placement (see `TransientState::kitty_virtual`).
#[derive(Debug, Clone)]
pub(super) struct KittyVirtual {
    /// Its image id, placement id (`p=`, `0` when the client named none) and
    /// serial — the identity a placement relative to it follows.
    pub(super) tag: aterm_grid::KittyPlacementTag,
    /// The image laid out as its put asked (`c=`/`r=`/crop/`z=`), for the
    /// image's current frame.
    pub(super) image: Arc<ImageData>,
}

/// One Kitty placement relative to a virtual placement, directly or through other such
/// placements (see `TransientState::kitty_floating`). Not stamped into cells: it shows
/// wherever its virtual ancestor's placeholders are printed, so the render snapshot places
/// it each frame (`kitty_floating::floating_origins`).
#[derive(Debug, Clone)]
pub(super) struct KittyFloating {
    /// Its image id, placement id and serial.
    pub(super) tag: aterm_grid::KittyPlacementTag,
    /// The image laid out as its put asked, for the image's current frame.
    pub(super) image: Arc<ImageData>,
    /// The placement it is relative to.
    pub(super) parent: KittyFloatParent,
    /// Its offset from the parent's top-left cell, in columns (`H=`).
    pub(super) dx: i32,
    /// Its offset from the parent's top-left cell, in rows (`V=`).
    pub(super) dy: i32,
}

/// What a [`KittyFloating`] placement is relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KittyFloatParent {
    /// A virtual placement (`TransientState::kitty_virtual`).
    Virtual(KittyParent),
    /// Another floating placement.
    Floating(KittyParent),
}

/// One Kitty relative placement (see `TransientState::kitty_relations`).
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

/// Which placement a relative placement follows: `(image id, placement id)` when the
/// parent has a placement id (a re-put moves it, and its children follow), else the
/// parent's one-off serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KittyParent {
    /// The parent's image id and non-zero placement id.
    Named {
        /// The parent's image id.
        image_id: u32,
        /// The parent's placement id (never `0`).
        placement_id: u32,
    },
    /// An unnamed parent: its image id and serial.
    Serial {
        /// The parent's image id.
        image_id: u32,
        /// The parent's serial.
        serial: std::num::NonZeroU32,
    },
}

impl KittyParent {
    /// The key a relative placement of `parent` follows.
    pub(super) const fn of(parent: aterm_grid::KittyPlacementTag) -> Self {
        if parent.placement_id == 0 {
            Self::Serial {
                image_id: parent.image_id,
                serial: parent.serial,
            }
        } else {
            Self::Named {
                image_id: parent.image_id,
                placement_id: parent.placement_id,
            }
        }
    }

    /// The parent's image id.
    pub(super) const fn image_id(self) -> u32 {
        match self {
            Self::Named { image_id, .. } | Self::Serial { image_id, .. } => image_id,
        }
    }

    /// Whether `tag` is this parent.
    pub(super) fn names(self, tag: &aterm_grid::KittyPlacementTag) -> bool {
        match self {
            Self::Named {
                image_id,
                placement_id,
            } => tag.image_id == image_id && tag.placement_id == placement_id,
            Self::Serial { serial, .. } => tag.serial == serial,
        }
    }
}
