// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Transient terminal state: the fields `reset_common_fields` clears together ([`TransientState`]).

use super::response_rate_limiter::ResponseRateLimiter;
use super::types::SgrStackEntry;
use aterm_types::{PipelineTimestamps, Rgb};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use aterm_grid::ImageData;

type XtsaveModesMap = HashMap<u16, bool>;

/// Default foreground (light grey), single-sourced from [`aterm_types::DEFAULT_FOREGROUND`] so the runtime state and `TerminalConfig::default()` agree.
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

/// The cell at the latest ordered synchronized-output close.
#[derive(Clone, Copy, Debug)]
pub(super) struct SyncEndCaret {
    pub(super) seq: u64,
    pub(super) screen: u64,
    pub(super) grid: (u16, u16),
    pub(super) caret: (u16, u16),
}

/// The screen at the program's last DEC 2026 close ([`TransientState::app_sync_closed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ClosedFrame {
    /// The active grid's content generation at the close.
    pub(super) content_gen: u64,
    /// `screen_replaced` at the close.
    pub(super) screen: u64,
    /// The grid's `(rows, cols)` at the close.
    pub(super) grid: (u16, u16),
}

/// Grouped transient terminal state.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent terminal flags, not a state machine"
)]
pub(super) struct TransientState {
    /// Response buffer for DSR/DA and other terminal responses.
    pub(super) response_buffer: Vec<u8>,
    /// The reports the terminal sends unasked, at a host's change rather than to answer a sequence: DEC 2048 in-band size reports and DSR 997 colour-scheme pushes.
    pub(super) report_buffer: Vec<u8>,
    /// Token-bucket rate limiter gating `send_response`, so a peer spamming DSR/DA/DECRQSS cannot force unlimited responses even when the host drains the buffer in a tight loop.
    pub(super) response_rate_limiter: ResponseRateLimiter,
    /// Last graphic character received (for REP - CSI b).
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
    pub(super) sync_start: Option<aterm_time::Instant>,
    /// Monotonic count of synchronized-update (mode 2026) closes: every `?2026l`, DECSTR/RIS reset and timeout force-clear.
    pub(super) sync_end_seq: u64,
    /// Cursor cell at the latest ordered close, qualified by its count, screen and grid.
    pub(super) sync_end_caret: Option<SyncEndCaret>,
    /// Monotonic count of full resets (RIS and [`Terminal::reset`], both via `reset_common_fields`), never zeroed.
    pub(super) reset_generation: u64,
    /// Monotonic count of full clears: ED 2, and ED 0 with the cursor home (`resize_journal::is_full_clear`; ED 3 erases only history).
    pub(super) full_clears: u64,
    /// Monotonic count of whole-screen replacements: alternate-screen enter and exit (47/1047/1049) and full resets.
    pub(super) screen_replaced: u64,
    /// Monotonic count of the program's DEC 1004 enables: each `CSI ? 1004 h` (or XTRESTORE) that turns focus reporting on from off.
    pub(super) focus_arms: u32,
    /// The engine's render debt ([`RenderDebt`](super::RenderDebt)): what the alternate-screen, non-reflowed resizes run under `ResizePolicy::Native` since the screen was last repainted from scratch (a full clear, a screen switch, a reset) moved or cut.
    pub(super) render_debt: super::RenderDebt,
    /// Monotonic count of non-empty batches [`process_at`](super::Terminal::process_at) consumed, whatever they changed, plus every Kitty animation frame step ([`advance_kitty_animations`](super::Terminal::advance_kitty_animations)).
    pub(super) screen_mutations: u64,
    /// Whether the current synchronized-output window has accepted any complete PTY action since its `?2026h`.
    pub(super) sync_open_dirty: bool,
    /// Whether the program's synchronized-output bracket is open by its own account: set by `?2026h`, cleared only by `?2026l`, DECSTR or RIS.
    pub(super) app_sync_open: bool,
    /// Whether the program's open bracket (`app_sync_open`) has accepted any complete PTY action; the program-view twin of `sync_open_dirty`.
    pub(super) app_sync_dirty: bool,
    /// When the screen's run of unfinished frames began: the `process_now` of the first batch that ended with the program's frame unfinished ([`Terminal::sync_frame_unfinished`](super::Terminal::sync_frame_unfinished)) since the last batch that ended with it whole, `None` after a batch that ended whole.
    pub(super) frame_unfinished_since: Option<aterm_time::Instant>,
    /// The screen as the program's last DEC 2026 frame left it: the active grid's content generation, the screen and the grid size at the `?2026l` that closed a frame it drew into (the content moved since its `?2026h`).
    pub(super) app_sync_closed: Option<ClosedFrame>,
    /// The active grid's content generation and screen at the program's opening `?2026h`: a close that finds them unmoved drew nothing, and finishes no frame.
    pub(super) app_sync_open_mark: Option<(u64, u64)>,
    /// When the screen last changed, and the mark it changed to: the `process_now` of the last batch that ended with the active grid's content generation or screen other than the batch before left them (`Terminal::note_batch_end`).
    pub(super) drawn: Option<(aterm_time::Instant, (u64, u64))>,
    /// Logical "now" for the current batch, captured once at the top of [`Terminal::process_at`](super::Terminal::process_at).
    pub(super) process_now: aterm_time::Instant,
    /// Wall-clock epoch milliseconds for the current batch, recorded by OSC 133/633 marks B/C/D; captured with `process_now` so replay reproduces the `command_*_time_ms` values.
    pub(super) process_wall_ms: Option<u64>,
    /// SGR attribute stack for XTPUSHSGR/XTPOPSGR.
    pub(super) sgr_stack: VecDeque<SgrStackEntry>,
    /// Per-frame pipeline timing for keystroke-to-pixel decomposition.
    pub(super) pipeline_timestamps: PipelineTimestamps,
    /// Whether the last combining character added was a ZWJ (U+200D); gates the grid lookup in `should_combine_with_previous_zwj`.
    pub(super) last_combining_was_zwj: bool,
    /// Cached `current_hyperlink.is_some() || current_underline_color.is_some()`, so `write_char_core` makes one check per character instead of two.
    pub(super) has_transient_extras: bool,
    /// Set by the RIS handler for side effects it cannot perform inside the parser's dispatch (clearing secure keyboard entry and the parked selection); applied when the current `advance_fast` returns.
    pub(super) pending_parser_reset: bool,
    /// The main grid's `absolute_row_counter` when this batch parked it (smcup).
    pub(super) alt_park_main_row_counter: Option<u64>,
    /// A reading position flattened off a grid swapped back in mid-batch (rmcup): `(the display_offset it was parked with, its absolute_row_counter at the swap)`.
    pub(super) alt_restore_pin: Option<(usize, u64)>,
    /// Whether this batch left the alt screen at any point.
    pub(super) alt_screen_left_in_batch: bool,
    /// The host asked for the primary screen (`Terminal::leave_alternate_screen`, the `mainscreen` verb) after an app died on the alternate screen.
    pub(super) host_leave_alternate_screen: bool,
    /// The current alternate screen was entered by `?47h`/`?1047h` (no cursor save) rather than `?1049h`.
    pub(super) alt_entered_without_cursor_save: bool,
    /// An orphaned alternate screen was left while a Windows console host sits between us and the app (`win32_input_active`): our screen is primary again but conhost's alternate buffer is still up.
    pub(super) conhost_alt_screen_left_up: bool,
    /// HOST POLICY (the embedder's, set by `Terminal::set_host_resyncs_alt_screen`; kept across RIS, which `reset` leaves it out of): the console host between us and the app forwards an application's alternate-screen switches verbatim and never repaints the buffer it switches to, as Windows Terminal's OpenConsole does.
    pub(super) host_resyncs_alt_screen: bool,
    /// A resync the embedder has not taken yet (`Terminal::take_alt_screen_resync_request`).
    pub(super) alt_screen_resync_requested: bool,
    /// [`Self::screen_replaced`] when the last resync was requested: one request per stuck screen.
    pub(super) alt_screen_resync_screen: Option<u64>,
    /// The alternate screen ([`Self::screen_replaced`] while it is up) a shell prompt proved dead under the host policy, or the embedder is making its host leave (`Terminal::expect_host_alt_screen_leave`): the `?1049l` that leaves THAT screen is the host's forwarded switch, and it releases the modes the dead app left on as the local leave does (`TerminalHandler::take_dead_alt_screen`).
    pub(super) dead_alt_screen: Option<u64>,
    /// Where the shell's latest prompt on a dead alternate screen started: the screen ([`Self::screen_replaced`] while it was up) and the cursor at the `A` that proved it dead under the host policy.
    pub(super) dead_alt_prompt_at: Option<(u64, u16, u16)>,
    /// DEC 2048's enable-time size report is owed but held: the enable came on the primary screen behind a Windows console host, where the reader is as likely the shell as the program that asked (`TerminalHandler::hold_size_report`).
    pub(super) size_report_held: bool,
    /// The last DECSET/DECRST 2004 arrived after the shell's `133;D` and before its next `A`: the shell's own set-up for the prompt it is drawing, which the `A` that proves a full-screen app dead leaves alone (`TerminalHandler::release_dead_app_paste`).
    pub(super) bracketed_paste_from_prompt: bool,
    /// XTSAVE (`CSI ? Ps s`) saved DEC private mode values, restored by XTRESTORE (`CSI ? Ps r`).
    pub(super) xtsave_modes: XtsaveModesMap,
    /// Whether the most recent OSC was terminated by BEL (0x07) rather than ST; responses echo the same terminator for programs that only recognize BEL.
    pub(super) last_osc_bel_terminated: bool,
    /// Kitty graphics image store: client image id to decoded image.
    pub(super) kitty_images: HashMap<u32, Arc<ImageData>>,
    /// Kitty animations: image id to its frames (frame 0 is the base transmit; `a=f` appends or edits one, `a=c` composes onto one), playback settings and z-order among images of one z-index.
    pub(super) kitty_frames: HashMap<u32, KittyAnimation>,
    /// Kitty placement variants: image id to re-laid-out copies for `a=p` displays whose footprint, z-index or source rectangle differ from the stored image's, oldest first.
    pub(super) kitty_variants: HashMap<u32, Vec<KittyVariant>>,
    /// Kitty virtual placements (`U=1`): image id to its virtual placements, newest last, each drawn where a Unicode placeholder names it.
    pub(super) kitty_virtual: HashMap<u32, Vec<KittyVirtual>>,
    /// Kitty relative placements (`P=`/`Q=`) on screen, each with its parent and offset; they move and are deleted with the parent.
    pub(super) kitty_relations: Vec<KittyRelation>,
    /// Kitty placements relative to a virtual placement (or to another such placement), oldest first: placed each frame where the virtual ancestor's placeholders show and deleted with their parent.
    pub(super) kitty_floating: Vec<KittyFloating>,
    /// Kitty image numbers (`I=`): number → the id of the newest image transmitted under it (the terminal picks that id and reports it).
    pub(super) kitty_numbers: HashMap<u32, u32>,
    /// The last id the terminal assigned to an `I=` transmission; the next assignment probes upward from here past ids already in use.
    pub(super) kitty_last_assigned_id: u32,
    /// The last Kitty serial handed out: to a placement act (`aterm_grid::KittyPlacementTag::serial`) or to an image first transmitted (`KittyPlacementTag::image_serial`).
    pub(super) kitty_serial: u32,
    /// Running total of `ImageData.bytes.len()` over every slot in `kitty_images`, `kitty_frames` and `kitty_variants` (a shared base-transmit Arc counts each time), held under `MAX_KITTY_STORE_BYTES` so the per-item caps cannot multiply into a multi-GiB footprint.
    pub(super) kitty_total_bytes: usize,
    /// In-flight Kitty chunked transmission (`m=1`): the first chunk's command, its `payload` growing per chunk, finalized on `m=0`.
    pub(super) kitty_pending: Option<crate::terminal::kitty_graphics::KittyCommand>,
    /// Per screen (`[main, alternate]`, indexed by `modes.alternate_screen`), an upper bound on the payload bytes of the inline images that screen shows, held under `MAX_LIVE_IMAGE_BYTES` by `TerminalHandler::admit_live_image` (which explains the bound).
    pub(super) live_image_charge: [usize; 2],
    /// Edge-triggered BEL flag set by `handle_bell()` (after rate limiting) and drained by `Terminal::drain_bell()`, for polling hosts.
    pub(super) bell_pending: bool,
    /// App-event queue of OSC payloads `(code, payload)` the host polls via `Terminal::take_osc_event`: OSC 52 decoded clipboard, OSC 7 cwd, OSC 133 marks.
    pub(super) osc_events: OscEventQueue,
    /// Where the most recent PTY print run ended: the active-grid cursor `(row, col)` after each print action.
    pub(super) print_anchor: Option<(u16, u16)>,
    /// Monotonic count of print actions, paired with [`Self::print_anchor`] so an unchanged end position still reads as new output.
    pub(super) print_anchor_seq: u64,
    /// Monotonic count of kitty-graphics APC strings that ended (complete or cut short by CAN/SUB), every one counted whatever it did.
    pub(super) kitty_commands: u64,
    /// Monotonic count of Kitty unscrolls (`CSI n + T`) run on the main screen, where they pull history onto it; never reset.
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
            frame_unfinished_since: None,
            app_sync_closed: None,
            app_sync_open_mark: None,
            drawn: None,
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

    /// Bytes owed to the PTY peer: the replies and the reports, which share [`super::MAX_RESPONSE_BUFFER_SIZE`].
    #[inline]
    pub(super) const fn owed_len(&self) -> usize {
        self.response_buffer
            .len()
            .saturating_add(self.report_buffer.len())
    }

    /// Queue `report` in `report_buffer` unless it would take what is owed past [`super::MAX_RESPONSE_BUFFER_SIZE`]: every caller writes past `send_response`'s rate limit, so a toggle flood or a resize storm against a host that does not drain must not grow it.
    #[inline]
    pub(super) fn push_report(&mut self, report: &[u8]) -> bool {
        let fits = self.owed_len().saturating_add(report.len()) <= super::MAX_RESPONSE_BUFFER_SIZE;
        if fits {
            self.report_buffer.extend_from_slice(report);
        }
        fits
    }

    /// Recompute the cached `has_transient_extras` flag.
    #[inline]
    pub(super) const fn update_has_transient_extras(&mut self) {
        self.has_transient_extras =
            self.current_hyperlink.is_some() || self.current_underline_color.is_some();
    }

    /// Clear the transient state (called during terminal reset), keeping what the struct doc names.
    pub(super) fn reset(&mut self) {
        self.last_graphic_char = None;
        self.current_hyperlink = None;
        self.current_hyperlink_id = None;
        self.current_underline_color = None;
        self.vt52_cursor_state = Vt52CursorState::default();
        if self.sync_start.is_some() {
            self.sync_end_seq += 1;
        }
        self.reset_generation = self.reset_generation.wrapping_add(1);
        self.screen_replaced = self.screen_replaced.wrapping_add(1);
        self.render_debt = super::RenderDebt::default();
        self.sync_start = None;
        self.sync_end_caret = None;
        self.sync_open_dirty = false;
        self.app_sync_open = false;
        self.app_sync_dirty = false;
        self.app_sync_closed = None;
        self.app_sync_open_mark = None;
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
        self.alt_screen_resync_requested = false;
        self.alt_screen_resync_screen = None;
        self.dead_alt_screen = None;
        self.dead_alt_prompt_at = None;
        self.size_report_held = false;
        self.bracketed_paste_from_prompt = false;
        self.xtsave_modes.clear();
        self.last_osc_bel_terminated = false;
        self.bell_pending = false;
        self.osc_events.clear();
        self.print_anchor = None;
        self.kitty_images.clear();
        self.kitty_frames.clear();
        self.kitty_variants.clear();
        self.kitty_virtual.clear();
        self.kitty_relations.clear();
        self.kitty_floating.clear();
        self.kitty_numbers.clear();
        self.kitty_total_bytes = 0;
        self.kitty_pending = None;
        self.live_image_charge = [0; 2];
    }
}

/// The poll-host app-event queue ([`TransientState::osc_events`]): OSC payloads `(code, payload)` in arrival order, bounded by count and by total payload bytes.
#[derive(Debug, Default)]
pub(super) struct OscEventQueue {
    events: VecDeque<(u32, String)>,
    bytes: usize,
}

impl OscEventQueue {
    /// Most events held at once.
    pub(super) const MAX_EVENTS: usize = 1024;
    /// Most payload bytes held at once, summed over every queued event.
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
    #[inline]
    pub(super) fn pop_front(&mut self) -> Option<(u32, String)> {
        let event = self.events.pop_front()?;
        self.bytes -= event.1.len();
        Some(event)
    }

    #[inline]
    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[inline]
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

    /// Rewrite payloads in place; `rewrite` returns a replacement or `None` to leave the payload as it is.
    pub(super) fn rewrite(&mut self, mut rewrite: impl FnMut(u32, &str) -> Option<String>) {
        for (code, payload) in &mut self.events {
            if let Some(new) = rewrite(*code, payload) {
                self.bytes = self.bytes - payload.len() + new.len();
                *payload = new;
            }
        }
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

/// One Kitty image's animation (see `TransientState::kitty_frames`): its frames, root first, and how they play, as kitty plays them.
#[derive(Debug, Clone)]
pub(super) struct KittyAnimation {
    /// The frames, the root (the transmitted image) first.
    pub(super) frames: Vec<KittyFrame>,
    /// `s=`: stopped, running, or running while more frames load.
    pub(super) state: KittyPlayState,
    /// How many times the animation plays before it rests on its last frame (`v=` less one); `0` plays it forever.
    pub(super) max_loops: u32,
    /// Times it has come round since its state was last set.
    pub(super) loops: u32,
    /// When the frame shown went up: its gap runs from here.
    pub(super) shown_at: aterm_time::Instant,
    /// When the image was first transmitted: its place among images of one z-index (`aterm_grid::KittyPlacementTag::image_serial`).
    pub(super) image_serial: std::num::NonZeroU32,
}

impl KittyAnimation {
    /// A still image: `root` alone, stopped, shown since `now`, first transmitted at `image_serial`.
    pub(super) fn new(
        root: Arc<ImageData>,
        image_serial: std::num::NonZeroU32,
        now: aterm_time::Instant,
    ) -> Self {
        Self {
            frames: vec![KittyFrame {
                image: root,
                gap_ms: 0,
            }],
            state: KittyPlayState::Stopped,
            max_loops: 0,
            loops: 0,
            shown_at: now,
            image_serial,
        }
    }

    /// Whether it steps frames on its own: not stopped, more than one frame, some frame with a gap (a gapless frame is never shown), and loops left.
    #[inline]
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
    /// Milliseconds it shows before the next frame while the animation runs; `0` is a gapless frame, stepped over without being shown.
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
    /// The image (frame) it was cut from: a variant is reused only for the same source and layout.
    pub(super) source: Arc<ImageData>,
    /// The copy the put displays.
    pub(super) image: Arc<ImageData>,
}

/// One Kitty virtual placement (see `TransientState::kitty_virtual`).
#[derive(Debug, Clone)]
pub(super) struct KittyVirtual {
    /// Its image id, placement id (`p=`, `0` when the client named none) and serial — the identity a placement relative to it follows.
    pub(super) tag: aterm_grid::KittyPlacementTag,
    /// The image laid out as its put asked (`c=`/`r=`/crop/`z=`), for the image's current frame.
    pub(super) image: Arc<ImageData>,
}

/// One Kitty placement relative to a virtual placement, directly or through other such placements (see `TransientState::kitty_floating`).
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

/// Which placement a relative placement follows: `(image id, placement id)` when the parent has a placement id (a re-put moves it, and its children follow), else the parent's one-off serial.
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
    #[inline]
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
    #[inline]
    pub(super) const fn image_id(self) -> u32 {
        match self {
            Self::Named { image_id, .. } | Self::Serial { image_id, .. } => image_id,
        }
    }

    /// Whether `tag` is this parent.
    #[inline]
    pub(super) const fn names(self, tag: &aterm_grid::KittyPlacementTag) -> bool {
        match self {
            Self::Named {
                image_id,
                placement_id,
            } => tag.image_id == image_id && tag.placement_id == placement_id,
            Self::Serial { serial, .. } => tag.serial.get() == serial.get(),
        }
    }
}
