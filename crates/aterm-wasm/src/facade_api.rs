// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

//! The JS facade both web terminals export from one body: the engine-side
//! surface (the mode getters, key and mouse encoding, paste framing, selection,
//! search, scrollback, the spill buffers, the OSC drains and the width-reflow
//! pump) whose code does not depend on the renderer. The renderer-specific
//! exports (construction, fonts, theme, chrome, `render`) stay in each crate's
//! `lib.rs`.
//!
//! Shared with `aterm-gpu-web`, which mounts this file by `#[path]`; names
//! resolve against the crate root of whichever crate compiles it.

// The methods read the crate root's items, which differ per crate.
use super::*;

/// Map host-supplied display coordinates into the selection's terminal space.
/// An off-screen i32 endpoint remains off-screen even at the numeric limits.
#[inline]
fn display_row_to_terminal(term: &Terminal, display_row: i32) -> i32 {
    let offset = i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
    display_row.saturating_sub(offset)
}

/// Project the selection into the visible viewport, clipping off-screen ends.
/// Both web hosts use the same geometry for their public selection bounds.
#[inline]
pub(crate) fn selection_range_for(
    term: &Terminal,
    rows: usize,
    cols: usize,
) -> Option<SelectionRange> {
    let last_col = (cols as u16).saturating_sub(1);
    let proj = term.text_selection().project_range(last_col)?;
    let offset = i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
    let rows_i = i32::try_from(rows).unwrap_or(i32::MAX);
    let start_disp = proj.start_row.saturating_add(offset);
    let end_disp = proj.end_row.saturating_add(offset);
    if end_disp < 0 || start_disp >= rows_i {
        return None;
    }
    let (start_y, start_x) = if start_disp < 0 {
        (0u16, 0u16)
    } else {
        (start_disp as u16, proj.start_col)
    };
    let (end_y, end_x) = if end_disp >= rows_i {
        (rows_i.saturating_sub(1) as u16, last_col)
    } else {
        (end_disp as u16, proj.end_col)
    };
    Some(SelectionRange {
        start_x,
        start_y,
        end_x,
        end_y,
    })
}

/// The JS-visible surface. Each crate expands it onto its own terminal
/// type, under its own export attribute, with `web_terminal_impl!`: the
/// attribute names the JS class after the impl's type, which this shared
/// file cannot spell.
macro_rules! bind {
    ($terminal:ident, $export:ident) => {
        #[cfg_attr(target_arch = "wasm32", $export)]
        impl $terminal {
            /// Feed raw PTY output bytes into the engine.
            pub fn process(&mut self, bytes: &[u8]) {
                self.term.process(bytes);
                self.pump_reflow_on_output();
            }

            /// Feed PTY output as a JS string. The glue encodes it (UTF-8, via
            /// `encodeInto`) straight into wasm memory, so the host avoids a separate
            /// JS-side `TextEncoder.encode` allocation + copy on the hot output path.
            /// Byte-identical to `process(new TextEncoder().encode(s))`.
            pub fn process_str(&mut self, s: &str) {
                self.term.process(s.as_bytes());
                self.pump_reflow_on_output();
            }

            /// [`Self::new`] from a registered PRIMARY font handle.
            #[allow(clippy::too_many_arguments)]
            pub fn new_registered(
                rows: u16,
                cols: u16,
                font_handle: u32,
                px: f32,
                fg: u32,
                bg: u32,
                cursor: u32,
                selection: u32,
            ) -> Result<$terminal, String> {
                let bytes = registered_font(font_handle)?;
                Self::new(rows, cols, &bytes, px, fg, bg, cursor, selection)
            }

            /// Authorize OSC 52 clipboard *write* (set) so the engine queues OSC 52
            /// app-events for the host to drain via `take_osc_events`. Without this the
            /// engine is fail-closed (CF-004) and silently drops PTY-origin OSC 52 set
            /// sequences, so they never reach the host. The host still gates the actual
            /// clipboard write on its own user setting (defense in depth).
            pub fn authorize_clipboard_write(&mut self) {
                self.term.authorize_clipboard_access(ClipboardAccess::Write);
            }

            /// Revoke OSC 52 clipboard *write* authorization (the user toggled the
            /// clipboard setting off). Returns the engine to its fail-closed default.
            pub fn revoke_clipboard_write(&mut self) {
                self.term.revoke_clipboard_access(ClipboardAccess::Write);
            }

            /// Mint an EXTRA OSC 8 URI scheme onto the engine's safe allowlist —
            /// e.g. `authorize_hyperlink_scheme("orca")` so host-emitted
            /// `orca://` OSC-8 hyperlinks linkify. Returns `false`
            /// (refused, nothing changes) for a malformed / over-long scheme, a
            /// never-allow scheme (`javascript`/`data`/`file`/…, however cased), or
            /// when the bounded set (4) is full; `true` when live (idempotent).
            /// Every other OSC-8 guard — byte cap, control-char and BiDi filters,
            /// the OSC-8 capability gate — still applies to extra-scheme URIs.
            pub fn authorize_hyperlink_scheme(&mut self, scheme: &str) -> bool {
                self.term.authorize_hyperlink_scheme(scheme)
            }

            /// Remove a host-minted extra scheme (case-insensitive), restoring the
            /// engine's default allowlist posture for it.
            pub fn revoke_hyperlink_scheme(&mut self, scheme: &str) {
                self.term.revoke_hyperlink_scheme(scheme);
            }

            /// Drain the engine's pending query replies (DA1/DA2/DSR/CPR/DECRQM/OSC color/
            /// window-size, …) — the host forwards these to the PTY so the RENDERER (not the
            /// daemon, which stays silent) is the authoritative responder. Call after each
            /// `process`; returns `None` when nothing is pending.
            pub fn take_response(&mut self) -> Option<Vec<u8>> {
                self.term.take_response()
            }

            /// Drain pending OSC app-events as a JSON array of `[code, payload]` pairs
            /// (`[[7,"/home"],[52,"copied"]]`); `None` when the queue is empty. These
            /// carry REAL decoded payloads (OSC 52 clipboard / OSC 7 cwd / OSC 133 mark)
            /// the host routes to UI handlers — distinct from `take_response` (PTY replies).
            pub fn take_osc_events(&mut self) -> Option<String> {
                if !self.term.has_osc_events() {
                    return None;
                }
                let mut pairs = Vec::new();
                while let Some((code, payload)) = self.term.take_osc_event() {
                    pairs.push(format!("[{code},{}]", json_string(&payload)));
                }
                Some(format!("[{}]", pairs.join(",")))
            }

            /// Display-relative cursor column (0-based).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn cursor_x(&self) -> u16 {
                self.term.grid().cursor().col
            }

            /// Display-relative cursor row (0-based, top of viewport).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn cursor_y(&self) -> u16 {
                self.term.grid().cursor().row
            }

            /// The dedicated LIVE application cursor colour (OSC 12) as packed
            /// `0x00RRGGBB`, or `undefined` after OSC 21 `cursor=` selected dynamic
            /// foreground-following behavior (and in a raw unconfigured core). OSC 112
            /// restores the host-configured cursor baseline. Read per frame so
            /// glow/trail colour derivation can follow app-driven cursor changes.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn cursor_color(&self) -> Option<u32> {
                self.term
                    .cursor_color()
                    .map(|c| (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b))
            }

            /// Absolute row index of the live/last line (xterm `buffer.active.baseY`):
            /// `oldest_absolute_row() + scrollback_lines()`. `usize` → plain JS number.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn base_y(&self) -> usize {
                self.term.grid().base_y()
            }

            /// Absolute row index of the TOP visible line for the current viewport
            /// (`base_y - display_offset`); the search/link origin.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn display_origin_absolute(&self) -> usize {
                self.term.grid().display_origin_absolute()
            }

            /// Soft-wrap flag for a visible `row`: `true` if it continues the previous
            /// row (autowrap), `undefined`/`None` when out of range.
            pub fn row_is_wrapped(&self, row: u16) -> Option<bool> {
                self.term.grid().row_is_wrapped(row)
            }

            /// Logical length of a visible `row` (last non-empty cell + 1, 0 if blank);
            /// `None` when out of range.
            pub fn row_len(&self, row: u16) -> Option<u16> {
                self.term.grid().row_len(row)
            }

            /// Grapheme text at DISPLAY cell `row`/`col` (display_offset-aware, like
            /// `row_text`) — base char plus complex cluster and combining marks. Empty
            /// string for a blank cell, a wide-continuation spacer, or out-of-range
            /// coords. Hosts rebuild scrolled-back rows per-cell from this, so it must
            /// track the scroll position; the live-frame reader is `get_line_text`.
            pub fn cell_text(&self, row: u16, col: u16) -> String {
                self.with_display_row_cell(row, col, |(text, _)| text.clone())
                    .unwrap_or_default()
            }

            /// Whether the DISPLAY cell at `row`/`col` is a wide (double-width)
            /// character; `None` when out of range. Resolved through the same
            /// display-offset-aware row view as `cell_text` so a host's per-cell walk
            /// sees one coherent row.
            pub fn cell_is_wide(&self, row: u16, col: u16) -> Option<bool> {
                self.with_display_row_cell(row, col, |(_, wide)| *wide)
            }

            /// Drain the edge-triggered BEL flag: `true` if a BEL fired since the last
            /// call, then clears it (so a poll-based host can flash/ring without the
            /// synchronous bell callback).
            pub fn drain_bell(&mut self) -> bool {
                self.term.drain_bell()
            }

            /// Play the Kitty graphics animations up to now: a frame that came due is
            /// shown, and the next `render` draws it. Returns the milliseconds until
            /// the next frame is due — the host's one timer for them — or `undefined`
            /// while nothing plays on screen, so an idle page schedules nothing.
            pub fn advance_kitty_animations(&mut self) -> Option<f64> {
                let now = aterm_time::Instant::now();
                self.term
                    .advance_kitty_animations(now)
                    .next_frame_at
                    .map(|at| at.saturating_duration_since(now).as_secs_f64() * 1000.0)
            }

            /// Tell the engine the real device-pixel cell size so its CSI 14t/16t
            /// window/cell-pixel reports are accurate (the engine has no canvas otherwise).
            pub fn set_cell_pixel_size(&mut self, width: u16, height: u16) {
                self.term.set_cell_pixel_size(width, height);
            }

            /// Set the engine's scrollback line limit (history lines retained behind the live
            /// viewport). `lines == 0` means unlimited (bounded only by the byte budgets). The
            /// limit is ONE TOTAL retention count (audit E1) across the hot ring, staged
            /// lines, and the tiered store together — the store takes the remainder after the
            /// ring's fixed share, so "retain N lines" retains N, not N + ring. Targets the
            /// primary-content grid — reaching the saved primary through an alt screen; the
            /// alt buffer keeps its spec'd zero scrollback — and re-clamps the scroll
            /// position. Without this the engine keeps its construction default
            /// (`DEFAULT_LINE_LIMIT`, 100k total).
            pub fn set_scrollback_limit(&mut self, lines: u32) {
                // WF-1 gate (defensive): a shrink can re-clamp a scrolled viewport;
                // the engine marks damage for that, but one extra render is cheaper
                // than coupling this gate to that guarantee.
                self.note_host_visual_change();
                let limit = if lines == 0 {
                    None
                } else {
                    Some(lines as usize)
                };
                self.term.set_scrollback_line_limit(limit);
            }

            /// Advance a deferred width-change scrollback rewrap (stashed by
            /// [`Self::resize`]) by ONE BOUNDED step — at most the configured budget
            /// of history lines ([`Self::pump_reflow_budget`], default
            /// `REFLOW_STEP_BUDGET_LINES`) — re-attaching the rewrapped history when
            /// the step completes the job. Returns `true` while work REMAINS (the
            /// host should schedule another pump — a `setTimeout(0)` chain or
            /// `requestIdleCallback`); `false` once nothing is pending (the job just
            /// completed and re-attached — re-attach marks full damage, so the next
            /// `render` repaints — or there was nothing to do).
            ///
            /// COST: O(budget × cols) per call (`PendingScrollbackReflow::reflow_step`;
            /// a logical line is never split, so a soft-wrapped run longer than the
            /// budget is rewrapped whole by the step that completes it). Any pump
            /// schedule yields history content IDENTICAL to a one-shot rewrap —
            /// aterm-grid's `reflow_step_any_schedule_matches_one_shot` property.
            ///
            /// NEVER-PUMPED SAFETY: a host that never calls this still completes the
            /// rewrap — `render` pumps one step per frame once
            /// `REFLOW_PUMP_GRACE_RENDERS` frames have passed, `process` pumps one
            /// step per call while the detach-window backlog exceeds
            /// `REFLOW_BACKLOG_MAX_LINES` — and a torn-down module drops the job WITH
            /// the engine. There is no host behavior that leaves the store detached
            /// while the module keeps operating unboundedly.
            pub fn pump_reflow(&mut self) -> bool {
                let Some(pending) = self.pending_reflow.take() else {
                    return false;
                };
                self.reflow_grace = 0;
                // One bounded rewrap step; on completion, the O(1) re-attach (guarded
                // against reset/erase races inside `finish_resize_offload`).
                match pending.reflow_step(self.reflow_budget) {
                    ReflowStep::InProgress(job) => {
                        self.pending_reflow = Some(job);
                        true
                    }
                    ReflowStep::Done(reflowed) => {
                        // CONVERGENCE (RFL-3): a width change that landed while this
                        // job was stepping means the re-attach hands back a
                        // re-detached job at the settled width — keep pumping it; the
                        // `true` already means "schedule another pump".
                        match self.term.finish_resize_offload(reflowed) {
                            Some(follow) => {
                                self.pending_reflow = Some(follow);
                                true
                            }
                            None => false,
                        }
                    }
                }
            }

            /// Tune the per-pump rewrap budget (INPUT history lines per
            /// [`Self::pump_reflow`] step). `0` restores the default
            /// (`REFLOW_STEP_BUDGET_LINES`, 2_000 ≈ ~3ms native — see the constant's
            /// sizing note). Hosts with generous idle windows can raise it to finish
            /// deep histories in fewer tasks; latency-sensitive hosts can lower it.
            pub fn pump_reflow_budget(&mut self, max_lines: u32) {
                self.reflow_budget = if max_lines == 0 {
                    REFLOW_STEP_BUDGET_LINES
                } else {
                    max_lines as usize
                };
            }

            /// True while a deferred scrollback rewrap is stashed (deep history is
            /// temporarily detached: only the ring is visible/searchable; a partly
            /// stepped job holds its progress here between pumps). The host should
            /// keep scheduling [`Self::pump_reflow`] while this is set.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn reflow_pending(&self) -> bool {
                self.pending_reflow.is_some()
            }

            /// `true` when the LAST [`render`](Self::render) call was elided by the
            /// WF-1 frame gate: nothing observable had changed, so the framebuffer,
            /// `rgba`, and spill exports all retained the previous frame's bytes and
            /// `present_band_count()` reported 0. Hosts can use this (or the cheaper
            /// [`needs_frame`](Self::needs_frame) BEFORE calling `render`) to idle
            /// their loop; tests and benches use it as the gate's reach witness.
            pub fn last_render_skipped(&self) -> bool {
                self.last_render_gated
            }

            // ── SPILL BAND — the cross-pane window-space effects export ─────────────
            // The chrome band (`[head][pad][grid][pad]` minus the grid box) rendered as
            // straight-alpha RGBA the host may composite OVER NEIGHBOURING PANES:
            // source-over onto this pane's own theme bg reproduces the composed frame's
            // band bytes exactly (the seam-continuity law), so the `.pane` clip line
            // never shows a seam. Refreshed by `render()`; all identity at 0/0 chrome.
            // Spill is CPU integer math over the emission streams both engines feed, so
            // the bytes are renderer-independent.

            /// Monotone revision of the spill-band content: advances ONLY when the
            /// exported bytes changed. Typing-only frames with a settled (or
            /// grid-interior) glow, idle re-renders, and 0/0 chrome keep it still —
            /// an unchanged value is the engine's word that the host may skip its
            /// blit without reading a single spill byte.
            pub fn spill_rev(&self) -> u32 {
                self.spill.rev()
            }

            /// Number of dirty rects from the LAST `render()` (0 on a no-change
            /// frame). Read together with [`spill_rects_ptr`](Self::spill_rects_ptr).
            pub fn spill_rect_count(&self) -> u32 {
                (self.spill.rects().len() / 4) as u32
            }

            /// Byte offset (in wasm linear memory) of the packed dirty-rect array:
            /// `spill_rect_count()` rects of 4 `i32`s — `x, y, w, h`, FRAME-ABSOLUTE
            /// device px. Same read discipline as [`rgba_ptr`](Self::rgba_ptr):
            /// consume synchronously after `render()`, never cache the JS view.
            pub fn spill_rects_ptr(&self) -> usize {
                self.spill.rects().as_ptr() as usize
            }

            /// Byte offset (in wasm linear memory) of the straight-alpha RGBA spill
            /// buffer: four packed row-major strips — **top** `(0, 0, width,
            /// pad+head)`, **bottom** `(0, height−pad, width, pad)`, **left** `(0,
            /// pad+head, pad, gridH)`, **right** `(width−pad, pad+head, pad, gridH)`
            /// with `gridH = height − 2·pad − head` — in that order. The pointer is
            /// STABLE across frames (the buffer re-rasters in place); it moves only
            /// when chrome or the grid size changes, so a host may hold its view
            /// between frames of one geometry (wasm memory GROWTH still detaches JS
            /// views — rebuild per read, the `rgba_ptr` rule).
            pub fn spill_ptr(&self) -> usize {
                self.spill.rgba().as_ptr() as usize
            }

            /// Byte length of the spill buffer (`0` at 0/0 chrome — the identity law:
            /// no band, no bytes, no per-frame cost).
            pub fn spill_len(&self) -> usize {
                self.spill.rgba().len()
            }

            /// Include `HaloMode::Over` VEILS (light-theme smoke/steam) in the spill
            /// band (default `true`, keeping the seam-continuity law universal).
            /// `false` scopes the spill to additive light + fire ink — the policy
            /// escape if veils over neighbouring panes read badly; the band then
            /// intentionally diverges from the in-frame veil pixels at the clip line.
            /// Applies from the next `render()`.
            pub fn set_spill_include_veils(&mut self, on: bool) {
                // WF-1 gate: spill config changes the exports `render` refreshes; a
                // gated render would leave them stale against the new setting.
                self.note_host_visual_change();
                self.spill.set_include_veils(on);
            }

            /// Scroll the viewport through scrollback: positive `delta` reveals older
            /// lines, negative reveals newer. `render` already honors the display offset,
            /// so the host only needs to redraw afterwards.
            pub fn scroll_lines(&mut self, delta: i32) {
                // Whole-row navigation lands row-aligned: drop any banked sub-row
                // residual (the scroll_input_api reset-on-snap contract).
                self.scroll_input.reset();
                self.term.scroll_display(delta);
            }

            /// Snap the viewport to the live bottom (latest output).
            pub fn scroll_to_bottom(&mut self) {
                self.scroll_input.reset();
                self.term.scroll_to_bottom();
            }

            /// Snap the viewport to the oldest retained scrollback line.
            pub fn scroll_to_top(&mut self) {
                self.scroll_input.reset();
                self.term.scroll_to_top();
            }

            /// Lines the viewport is scrolled up from the live bottom (0 = at bottom).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn display_offset(&self) -> usize {
                self.term.grid().display_offset()
            }

            /// True when the alternate screen is active (TUIs own their own scrolling),
            /// so the host should let wheel events pass through to the app.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn is_alt_screen(&self) -> bool {
                self.term.is_alternate_screen()
            }

            /// True when DEC private mode 1007 (alternate scroll) is set: while the
            /// alternate screen is active and mouse tracking is off, the host converts
            /// wheel ticks into arrow-key presses (aterm-gui's WheelPlan behaviour) so
            /// TUIs without mouse support (less, man, plain vim) still wheel-scroll.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn is_alternate_scroll(&self) -> bool {
                self.term.modes().alternate_scroll()
            }

            /// True when DECCKM (application cursor keys) is set: the host must encode
            /// arrows/Home/End as SS3 (ESC O A) instead of CSI (ESC [ A) so full-screen
            /// apps (vi, less, readline) receive the sequences they expect.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn is_app_cursor_mode(&self) -> bool {
                self.term.modes().application_cursor_keys()
            }

            /// True when a TUI has enabled mouse tracking (any of DECSET 9/1000/1002/1003).
            /// The host then ENCODES canvas mouse events to the PTY instead of running
            /// selection/scroll/link for them (unless Shift is held = user override).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn is_mouse_tracking(&self) -> bool {
                self.term.mouse_tracking_enabled()
            }

            /// True when the active mouse mode reports MOTION (ButtonEvent 1002 = drag
            /// while a button is down, AnyEvent 1003 = all motion), so the host only
            /// forwards `mousemove` when an app actually wants it (no spam in 1000).
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn mouse_wants_motion(&self) -> bool {
                matches!(
                    self.term.mouse_mode(),
                    MouseMode::ButtonEvent | MouseMode::AnyEvent
                )
            }

            /// True for AnyEvent (1003): report motion even with NO button pressed.
            /// 1002 only reports motion while a button is held; the host uses this to
            /// decide whether a button-less `mousemove` should be forwarded.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn mouse_wants_any_motion(&self) -> bool {
                matches!(self.term.mouse_mode(), MouseMode::AnyEvent)
            }

            /// True when DECSET 1004 (focus reporting) is active: the host sends CSI I on
            /// focus-in and CSI O on focus-out so apps (vim, tmux) track terminal focus.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn is_focus_event_mode(&self) -> bool {
                self.term.focus_reporting_enabled()
            }

            /// True when DEC mode 2031 (color-scheme update notifications) is set: the
            /// app wants `CSI ? 997 ; n` on OS light/dark theme changes.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn is_color_scheme_updates_mode(&self) -> bool {
                self.term.report_color_scheme_enabled()
            }

            /// Active DECSCUSR cursor style as the discriminant of `aterm_core`'s
            /// `CursorStyle` (1=BlinkingBlock, 2=SteadyBlock, 3=BlinkingUnderline,
            /// 4=SteadyUnderline, 5=BlinkingBar, 6=SteadyBar, 7=Hidden, 8=HollowBlock).
            /// The renderer ALREADY paints this shape from the grid (the render input
            /// carries it), so this getter exists for host introspection/tests — no JS
            /// overlay is needed to draw the shape.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn cursor_style(&self) -> u8 {
                self.term.cursor_style() as u8
            }

            /// Set the host-preferred DEFAULT cursor style (shape used before any DECSCUSR and
            /// restored after RIS/DECSTR). `n` follows the DECSCUSR convention: 1=blinking
            /// block, 2=steady block, 3=blinking underline, 4=steady underline, 5=blinking bar,
            /// 6=steady bar; out-of-range (0, 7+) is ignored. Unlike a render override this does
            /// NOT clobber an app's live DECSCUSR (e.g. vim insert-mode bar).
            pub fn set_default_cursor_style(&mut self, n: u8) {
                // WF-1 gate: cursor-style presentation may repaint the cursor cell
                // without a grid write; bump rather than audit the engine's marking.
                self.note_host_visual_change();
                if let Some(style) = CursorStyle::from_param(u16::from(n)) {
                    self.term.set_default_cursor_style(style);
                }
            }

            /// Set the host's baseline colour scheme: the appearance of the background the
            /// host set (`set_theme`). `dark = true` selects dark, `false` light. A program's
            /// OSC 11 background outranks it while it holds (DSR 996 answers the background
            /// on screen). When the REPORTED scheme changes and the app enabled DEC mode
            /// 2031, the engine queues an unsolicited `CSI ? 997 ; Ps n` (1=dark, 2=light);
            /// drain it via `take_response` and forward to the PTY so subscribed apps live-
            /// update their theme. A no-op when the reported scheme is unchanged.
            ///
            /// With a selector installed by `set_theme_by_name`, this is the OS
            /// appearance instead: it picks the selector's side, installs it and
            /// reports the drawn theme's appearance (a plain theme reports the same
            /// in light and dark mode, as native does).
            pub fn set_color_scheme(&mut self, dark: bool) {
                // WF-1 gate (defensive): scheme changes can recolor engine-resolved
                // dynamic colors; whether the engine marks damage for every arm is its
                // business — one render buys certainty.
                self.note_host_visual_change();
                let scheme = if dark {
                    aterm_types::Appearance::Dark
                } else {
                    aterm_types::Appearance::Light
                };
                // With a selector installed by `set_theme_by_name`, this picks its
                // side and reports the palette on screen; else the host owns the
                // colours and only the reported baseline moves (theme_api).
                self.follow_appearance(scheme);
            }

            /// Encode a mouse-button PRESS at 0-based on-screen cell `col`/`row` for the
            /// app's active mouse mode+encoding (returns `None`/`undefined` when tracking
            /// is off). `button` is the raw X10 button code (0=left,1=middle,2=right) and
            /// `mods` is the OR of Shift(4)/Alt(8)/Ctrl(16) masks — the engine combines
            /// them. Bytes are sent verbatim to the PTY.
            pub fn encode_mouse_press(
                &self,
                col: u16,
                row: u16,
                button: u8,
                mods: u8,
            ) -> Option<Vec<u8>> {
                self.term.encode_mouse_press(button, col, row, mods)
            }

            /// Encode a mouse-button RELEASE (see [`Self::encode_mouse_press`]);
            /// `None` in X10 press-only mode.
            pub fn encode_mouse_release(
                &self,
                col: u16,
                row: u16,
                button: u8,
                mods: u8,
            ) -> Option<Vec<u8>> {
                self.term.encode_mouse_release(button, col, row, mods)
            }

            /// Encode mouse MOTION at `col`/`row`; `button` is the held button (3 = none).
            /// `None` unless the mode reports motion (1002 while a button is down, 1003
            /// always) — see [`Self::mouse_wants_motion`].
            pub fn encode_mouse_motion(
                &self,
                col: u16,
                row: u16,
                button: u8,
                mods: u8,
            ) -> Option<Vec<u8>> {
                self.term.encode_mouse_motion(button, col, row, mods)
            }

            /// Encode a mouse WHEEL tick at `col`/`row` (`up` = wheel-up); the host sends
            /// these instead of scrolling scrollback while tracking is on. `None` in X10.
            ///
            /// The JS-facing signature deliberately stays a VERTICAL bool even though
            /// the engine now takes the full 4-way [`aterm_types::mouse::WheelDir`]: a
            /// browser `WheelEvent` carries `deltaX` too, but widening this binding is a
            /// web-host feature with its own delta-normalization questions (`deltaMode`,
            /// momentum), not a side effect of the native wheel fix. Widen it here when
            /// a web host actually wants to send buttons 66/67.
            pub fn encode_mouse_wheel(
                &self,
                col: u16,
                row: u16,
                up: bool,
                mods: u8,
            ) -> Option<Vec<u8>> {
                let dir = if up {
                    aterm_types::mouse::WheelDir::Up
                } else {
                    aterm_types::mouse::WheelDir::Down
                };
                self.term.encode_mouse_wheel(dir, col, row, mods)
            }

            /// Encode a keyboard event through the engine's FULL encoder — legacy +
            /// xterm modifyOtherKeys + Kitty progressive enhancement, driven by the
            /// LIVE `Terminal::keyboard_mode()` (DECCKM/DECBKM/1035/1036/1039 and the
            /// negotiated Kitty flags are exact), replacing the host's legacy-only TS
            /// encoder that acked Kitty on the wire but could never speak it.
            ///
            /// `key` is a DOM `KeyboardEvent.key` value (mapped by the shared
            /// `aterm_types::keyboard::map_dom_key` table); `mods` is the engine
            /// `Modifiers` bitfield (SHIFT=1, ALT=2, CTRL=4, SUPER=8); `event_type` is
            /// 0=Press, 1=Repeat, 2=Release; `base_layout_key` is the US-QWERTY char of
            /// the physical key for Kitty `REPORT_ALTERNATE_KEYS` (pass `undefined`
            /// when unknown). Returns `None` when the event encodes to nothing (e.g. a
            /// release without the Kitty protocol) or the key has no terminal encoding
            /// (modifier-only / IME / unidentified DOM keys — never guessed).
            ///
            /// `mods` are the chord's modifiers only: a press AltGr chose (Windows
            /// reports it as `ctrlKey` + `altKey`, with `getModifierState("AltGraph")`
            /// true) goes in without CTRL and ALT, its glyph in `key`; a Ctrl+Alt left
            /// on it is encoded as a chord (`encode_dom_key`).
            pub fn encode_key(
                &self,
                key: &str,
                mods: u8,
                event_type: u8,
                base_layout_key: Option<char>,
            ) -> Option<Vec<u8>> {
                aterm_types::keyboard::encode_dom_key(
                    key,
                    mods,
                    event_type,
                    base_layout_key,
                    self.term.keyboard_mode(),
                )
            }

            /// Enable/disable the Kitty keyboard protocol capability (default ON). When
            /// disabled the engine acts as if the protocol is unsupported — no `CSI ? u`
            /// reply, push/set/pop consumed-and-ignored, `keyboard_mode` never carries
            /// kitty bits — for hosts whose platform consumes kitty sequences itself
            /// (Windows ConPTY; xterm.js `vtExtensions.kittyKeyboard = false`).
            pub fn set_kitty_keyboard_enabled(&mut self, enabled: bool) {
                self.term.set_kitty_keyboard_enabled(enabled);
            }

            /// Declare that the PTY behind this terminal is a Windows ConPTY (default
            /// off). Only then does conhost's `CSI ? 9001 h` take effect, so that
            /// `encode_key` sends the win32-input-mode key records conhost asked for
            /// (Shift/Ctrl+Enter, which legacy VT cannot spell). Anywhere else that
            /// request is program output and is ignored. A ConPTY host normally also
            /// turns the kitty protocol off ([`Self::set_kitty_keyboard_enabled`]).
            pub fn set_win32_input_enabled(&mut self, enabled: bool) {
                self.term.set_win32_input_enabled(enabled);
            }

            /// The live `Terminal::keyboard_mode()` as its raw bitflags value, for
            /// hosts that run the engine in a Web Worker: mirror these bits into the
            /// main-thread engine-state snapshot and feed them to the free
            /// [`encode_key_with_mode`], which encodes keydowns synchronously without
            /// an instance. `KeyboardMode` is a `bitflags` struct over `u32`.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn keyboard_mode_bits(&self) -> u32 {
                self.term.keyboard_mode().bits()
            }

            /// Begin a character selection at display `row`/`col` (clears any prior one).
            pub fn selection_start(&mut self, row: i32, col: u16) {
                // WF-1 gate: selection is Terminal-held but marks NO grid damage (the
                // native GUI folds it as its own RepaintKey term for the same reason).
                // The ONE-SHOT selection mutators (this one, word, line, finish, clear)
                // bump unconditionally: a host calls them once per gesture, so an
                // idempotence check would only ever buy back a frame that is never
                // requested twice. `selection_extend` is the exception and explains
                // itself there — it is the only one a host calls at pointer cadence.
                self.note_host_visual_change();
                let row = display_row_to_terminal(&self.term, row);
                self.term.text_selection_mut().start_selection(
                    row,
                    col,
                    SelectionSide::Left,
                    SelectionType::Simple,
                );
            }

            /// Select the whole word/URL at display `row`/`col` (double-click) and return
            /// its text: a Semantic selection EXPANDED to the word's inclusive cell span
            /// (smart_word_at's end col is exclusive); on whitespace it falls back to the
            /// clicked cell. The selection stays active so the highlight paints.
            ///
            /// PHYSICAL ROW, unlike the desktop GUI: `aterm_control::selection::word_span`
            /// runs the rules over the LOGICAL line (soft-wrapped rows joined), so a word
            /// straddling the wrap is selected whole there (a RAGGED wrap, cut short by a
            /// double-width glyph, still splits one) and only its clicked-row
            /// fragment here. This facade's own `smart_word_at` path has not been moved
            /// over — stated so the difference is known rather than discovered.
            pub fn selection_word(&mut self, row: i32, col: u16) -> Option<String> {
                // WF-1 gate (see selection_start).
                self.note_host_visual_change();
                // smart_word_at is display-offset-aware (takes the DISPLAY row); the
                // selection anchor must be terminal-relative.
                let (start, last) =
                    match self
                        .term
                        .smart_word_at(row as usize, col as usize, &self.smart)
                    {
                        Some((s, e)) => (s as u16, e.saturating_sub(1).max(s) as u16),
                        None => (col, col),
                    };
                let term_row = display_row_to_terminal(&self.term, row);
                let sel = self.term.text_selection_mut();
                sel.start_selection(term_row, col, SelectionSide::Left, SelectionType::Semantic);
                sel.expand_semantic(start, last);
                sel.complete_selection();
                self.term.selection_to_string()
            }

            /// Select the whole PHYSICAL row at display `row` (triple-click) and return
            /// its text: a Lines selection expanded to the full row width. `col` is
            /// accepted for a uniform host API but unused (whole row).
            ///
            /// The desktop GUI's triple-click selects the LOGICAL line
            /// (`aterm_control::selection::select_line`, which walks
            /// `Terminal::logical_line_span`), so a soft-wrapped line comes back whole
            /// there and truncated at the window width here. Not moved over with it.
            pub fn selection_line(&mut self, row: i32, col: u16) -> Option<String> {
                self.note_host_visual_change(); // WF-1 gate (see selection_start)
                let _ = col;
                let row = display_row_to_terminal(&self.term, row);
                let max_col = (self.cols as u16).saturating_sub(1);
                let sel = self.term.text_selection_mut();
                sel.start_selection(row, 0, SelectionSide::Left, SelectionType::Lines);
                sel.expand_lines(max_col);
                sel.complete_selection();
                self.term.selection_to_string()
            }

            /// Move the selection endpoint to `row`/`col` (during a drag).
            ///
            /// WF-1 gate, and the ONE exception to the unconditional-bump rule above:
            /// this is the only `note_host_visual_change` caller a host invokes at
            /// POINTER cadence. A pointer moving sub-cell re-asserts the SAME cell
            /// across consecutive frames, and each such re-assertion would reopen the
            /// gate for a frame whose only outcome was a `DamageOutcome::GateHit` and
            /// zero present bands — a full `cell_frame_into` resolve plus a full
            /// `compute_dirty_rows` row walk, to conclude nothing changed. So bump only
            /// when the RESOLVED selection actually differs.
            ///
            /// The comparison is over the engine's own `TextSelection` value (all
            /// four fields: state, type, start anchor, end anchor) taken around the
            /// update, NOT over the caller's `(row, col)` arguments. That is deliberate
            /// — an argument shadow gets three cases wrong:
            /// - the DISPLAY row has to be converted first: auto-scroll during a drag
            ///   moves `display_offset` under a stationary display row, which IS a real
            ///   move of the terminal-relative anchor;
            /// - there is no shadow to reset, so `selection_start`/`word`/`line`/
            ///   `finish`/`clear` cannot leave a stale one that swallows the first
            ///   extend of the NEXT drag;
            /// - the engine moves selection anchors behind this facade's back when
            ///   content scrolls (`Terminal`'s `adjust_for_scroll`), after which
            ///   byte-identical arguments genuinely change the span.
            ///
            /// `TextSelection` is four small scalars, so clone-and-compare is O(1) and
            /// unconditionally cheaper than the frame it elides. It is also EXACTLY the
            /// predicate the renderer's own row diff applies (`prev_input.selection !=
            /// input.selection`, aterm-render), so "equal here" is precisely "no frame
            /// to draw" — this cannot suppress a bump for a selection that would have
            /// painted differently.
            pub fn selection_extend(&mut self, row: i32, col: u16) {
                let row = display_row_to_terminal(&self.term, row);
                let before = self.term.text_selection().clone();
                self.term
                    .text_selection_mut()
                    .update_selection(row, col, SelectionSide::Right);
                if *self.term.text_selection() != before {
                    self.note_host_visual_change();
                }
            }

            /// Finalize the selection (mouse released).
            pub fn selection_finish(&mut self) {
                self.note_host_visual_change(); // WF-1 gate (see selection_start)
                self.term.text_selection_mut().complete_selection();
            }

            /// Drop the current selection so the highlight clears on the next render.
            pub fn selection_clear(&mut self) {
                self.note_host_visual_change(); // WF-1 gate (see selection_start)
                self.term.text_selection_mut().clear();
            }

            /// Override the characters that BREAK a double-click word (the host's
            /// word-separator setting, xterm.js `wordSeparators` semantics): a word
            /// becomes a maximal run of NON-separator characters. `undefined` restores
            /// the engine's default class-based word logic (alphanumeric + `_`)
            /// exactly. Smart-selection RULES (url/file_path/email/…) still take
            /// precedence for both `selection_word` and `link_at`; the separators only
            /// shape the plain-word fallback.
            pub fn set_word_separators(&mut self, separators: Option<String>) {
                self.smart.set_word_separators(separators.as_deref());
            }

            /// The selected text, if any (`None` when the selection is empty).
            pub fn selection_text(&self) -> Option<String> {
                self.term.selection_to_string()
            }

            /// Current selection bounds in DISPLAY viewport cell coords (0 = top visible
            /// row), side-adjusted to match `selection_text` and the painted highlight.
            /// `None` when there is no selection OR it lies fully outside the viewport.
            pub fn selection_range(&self) -> Option<SelectionRange> {
                selection_range_for(&self.term, self.rows, self.cols)
            }

            /// Detect a link under display `row`/`col`, the cell the canvas painted
            /// there. Prefers an OSC-8 hyperlink, then falls back to smart-selection
            /// rules (url/file_path). Returns `None` for plain words. `kind`: 0=osc8,
            /// 1=url, 2=file_path, 3=other. The link is the one painted under the cell:
            /// a DEC double-width line paints grid column `c` at cells `2c` and `2c + 1`,
            /// and a BiDi row in visual order, so `col` is mapped to the logical column
            /// first (`Terminal::painted_cell_logical`, as the desktop pointer maps
            /// it). The hit's `start_col`/`end_col` are LOGICAL grid columns: on a
            /// doubled line or a BiDi-reordered row they are not the painted cells.
            pub fn link_at(&self, row: u16, col: u16) -> Option<LinkHit> {
                let (col, _) = self.term.painted_cell_logical(row, col);
                // OSC-8 lookups are NOT display_offset-aware (only valid at the live
                // bottom), so only consult hyperlink_at when the viewport isn't scrolled.
                if self.term.grid().display_offset() == 0 {
                    if let Some(url) = self.term.hyperlink_at(row, col) {
                        let url = url.to_string();
                        let (s, e) = self.osc8_span(row, col);
                        return Some(LinkHit {
                            url,
                            start_col: s,
                            end_col: e,
                            kind: 0,
                        });
                    }
                }

                // Smart-selection fallback: display_offset-aware, so it works on any
                // scrollback row, and the word's text is read from the cells its
                // columns name, not sliced out of the row's text by columns.
                let (matched, sc, ec) =
                    self.term
                        .smart_word_text_at(row as usize, col as usize, &self.smart)?;
                let kind = classify(&matched);
                if kind == 3 {
                    // A plain word is not a link.
                    return None;
                }
                Some(LinkHit {
                    url: matched,
                    start_col: sc as u16,
                    end_col: ec as u16,
                    kind,
                })
            }

            /// Scroll-correct text of a display `row` (display_offset-aware), for a TS
            /// fallback that re-runs link matching in JS.
            pub fn row_text(&self, row: u16) -> Option<String> {
                self.term.display_row_text(row as usize)
            }

            /// Serialize the terminal to a REPLAYABLE ANSI string — the aterm-native
            /// replacement for `@xterm/addon-serialize`'s `serialize({scrollback})`.
            /// `scrollback_rows` = `None` prepends ALL history, `Some(n)` the last `n`,
            /// `Some(0)` viewport-only. The body is `aterm_core`'s
            /// `Terminal::serialize_ansi`, shared with aterm-gpu-web.
            pub fn serialize(&self, scrollback_rows: Option<u32>) -> String {
                self.term.serialize_ansi(self.rows, scrollback_rows)
            }

            /// Scrollback HISTORY ONLY (the main buffer's off-screen lines, even while
            /// the alt screen is active) as flowing text + CRLF; `max_rows` caps to the
            /// last `n` lines (`None` = all). The body is `aterm_core`'s
            /// `Terminal::serialize_scrollback_ansi`, shared with aterm-gpu-web.
            pub fn serialize_scrollback(&self, max_rows: Option<u32>) -> String {
                self.term.serialize_scrollback_ansi(max_rows)
            }

            /// The last completed OSC-133 block's output as JSON, following the
            /// `take_osc_events` JSON-drain convention (CM-A3, "Copy Last Command
            /// Output"):
            ///   `{"status":"ok","text":"…","exitCode":0}` — output read in full
            ///     (`exitCode` is `null` when the block was finalized without OSC 133 D,
            ///     e.g. an interrupted command whose next prompt closed it);
            ///   `{"status":"evicted"}` — the block's rows scrolled past the scrollback
            ///     cap (DL-1: an honest marker, never silently-shifted/empty text);
            ///   `undefined` — nothing to copy: no shell integration, no finished block
            ///     yet (incl. a snapshot-rehydrated pane — blocks are excluded from
            ///     checkpoints), or the block never reached its output phase.
            ///
            /// `&self`: the read rides `Terminal::last_completed_block`, so the facade
            /// avoids the `&mut` `output_blocks()` (`make_contiguous`) path.
            pub fn last_command_output(&self) -> Option<String> {
                let block = self.term.last_completed_block()?;
                match self.term.block_output_text(block) {
                    BlockText::Text(text) => {
                        let exit = block
                            .exit_code
                            .map_or_else(|| "null".to_owned(), |c| c.to_string());
                        Some(format!(
                            "{{\"status\":\"ok\",\"text\":{},\"exitCode\":{exit}}}",
                            json_string(&text)
                        ))
                    }
                    BlockText::Evicted => Some("{\"status\":\"evicted\"}".to_owned()),
                    BlockText::NotAvailable => None,
                }
            }

            /// The window title (OSC 0/2), or `None` when unset — replaces the separate
            /// title channel that fed off the shadow xterm so snapshots keep window titles.
            pub fn title(&self) -> Option<String> {
                let title = self.term.title();
                if title.is_empty() {
                    None
                } else {
                    Some(title.to_string())
                }
            }

            /// Whether bracketed-paste mode (DECSET 2004) is active — a read-only view of
            /// the mode, for hosts that want to SHOW it (a paste affordance, a debug
            /// overlay). It is not a licence to assemble the paste framing in JS: the
            /// guards are only safe when they wrap already-sanitized text, so route every
            /// paste through [`Self::format_paste`], which reads this same mode.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn bracketed_paste_mode(&self) -> bool {
                self.term.modes().bracketed_paste()
            }

            /// The ONE paste encoder — the engine's own `Terminal::format_paste`, the same
            /// call the native app's input seam makes. Give it the raw clipboard text; send
            /// the returned bytes to the PTY verbatim.
            ///
            /// It strips the control bytes a hostile clipboard smuggles in (ESC, the whole
            /// C1 block, the C0 signal bytes), folds line breaks to CR, and only THEN adds
            /// the `ESC[200~ … ESC[201~` guards when bracketed-paste mode is on. That order
            /// is the security property: a pasted `ESC[201~` — or its 8-bit `0x9B 2 0 1 ~`
            /// spelling, which terminates the region just as well once 8-bit controls are
            /// honored — is already inert text by the time the closing guard is appended,
            /// so it cannot close its own bracket and have the tail run as keystrokes. A
            /// host that wraps the raw text itself has that hole, whichever order it picks.
            ///
            /// Bytes, not a string: the output is a PTY byte stream, and the guards are
            /// protocol framing rather than text.
            pub fn format_paste(&self, text: &str) -> Vec<u8> {
                self.term.format_paste(text)
            }

            /// Search the full retained buffer (scrollback + visible) for `query`,
            /// returning matches as a flat `[abs_line, start_col, len]` triplet array so
            /// the JS host can highlight + scroll without re-scanning text. Lines are
            /// ABSOLUTE rows (the index's native coordinate); the host maps them to
            /// display rows via [`Self::search_display_origin`] /
            /// [`Self::scroll_search_line_into_view`], which stay correct as the
            /// viewport scrolls. Empty `query` (or a regex error) yields an empty array.
            ///
            /// One-shot: pays the whole index build in this call and DROPS the
            /// engine's incomplete-results signal. Prefer
            /// [`Self::search_budgeted`], which slices the work across calls
            /// and reports `incomplete_index`.
            pub fn search(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
            ) -> Vec<u32> {
                if query.is_empty() {
                    return Vec::new();
                }
                // Reuse the cached full-content index (O(1) on unchanged content). When
                // is_regex is false this is a plain substring search; when true the engine
                // compiles `query` as a regex (an invalid pattern yields Err → empty array,
                // so a half-typed regex highlights nothing rather than throwing).
                let Ok(results) =
                    self.term
                        .indexed_search()
                        .search_results_opts(query, case_sensitive, is_regex)
                else {
                    return Vec::new();
                };
                let mut out = Vec::with_capacity(results.matches.len() * 3);
                for m in &results.matches {
                    out.push(u32::try_from(m.line).unwrap_or(u32::MAX));
                    out.push(u32::try_from(m.start_col).unwrap_or(u32::MAX));
                    out.push(u32::try_from(m.len()).unwrap_or(u32::MAX));
                }
                out
            }

            /// Metadata for a [`Self::search`]-contract query — most
            /// importantly the engine's `incomplete` signal, which that legacy export
            /// has always DROPPED (E9a, correctness-first): when index eviction or the
            /// engine's match cap truncated the results, the host has been presenting
            /// a truncated match list/count as if it were exhaustive.
            ///
            /// Stateless on purpose: it re-runs `query` against the SAME cached
            /// full-content index `search` uses (O(1) index reuse on unchanged
            /// content, so the added cost is one query, never a rebuild) and reports
            /// on exactly the results that call would return — no staleness if the
            /// host asks without (or long after) a paired `search`. Empty query or
            /// invalid regex: `incomplete == false`, `match_count == 0`, mirroring
            /// the legacy export's empty array.
            pub fn search_meta(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
            ) -> SearchMeta {
                if query.is_empty() {
                    return SearchMeta {
                        incomplete: false,
                        match_count: 0,
                    };
                }
                let Ok(results) =
                    self.term
                        .indexed_search()
                        .search_results_opts(query, case_sensitive, is_regex)
                else {
                    return SearchMeta {
                        incomplete: false,
                        match_count: 0,
                    };
                };
                SearchMeta {
                    incomplete: results.incomplete,
                    match_count: u32::try_from(results.matches.len()).unwrap_or(u32::MAX),
                }
            }

            /// Absolute row of display row 0 at the live bottom (`display_offset == 0`):
            /// `oldest_absolute_row + scrollback_lines`. A match at absolute `line` is at
            /// display row `line - origin + display_offset`, so the host computes the
            /// on-screen cell of any [`Self::search`] match without a round-trip.
            #[cfg_attr(target_arch = "wasm32", $export(getter))]
            pub fn search_display_origin(&self) -> u32 {
                let grid = self.term.grid();
                let origin = grid
                    .oldest_absolute_row()
                    .saturating_add(grid.scrollback_lines() as u64);
                u32::try_from(origin).unwrap_or(u32::MAX)
            }

            /// Scroll the viewport so the match at absolute `line` is visible, placing it
            /// at (or near) the top row. Clamps the target display_offset to the retained
            /// scrollback so a live-region match snaps to the bottom. Host redraws after.
            pub fn scroll_search_line_into_view(&mut self, line: u32) {
                let grid = self.term.grid();
                let origin = grid
                    .oldest_absolute_row()
                    .saturating_add(grid.scrollback_lines() as u64);
                let scrollback = grid.scrollback_lines();
                let current = grid.display_offset();
                // Target offset that lands `line` on display row 0; clamp to [0, scrollback].
                let want = origin.saturating_sub(u64::from(line));
                let want = (want as usize).min(scrollback);
                // scroll_display takes a delta (positive = older); convert from current.
                let delta = want as i64 - current as i64;
                if let Ok(delta) = i32::try_from(delta) {
                    self.term.scroll_display(delta);
                }
            }

            /// Budgeted, resumable variant of [`Self::search`] (P1.1): runs at
            /// most `row_budget` rows of index-build + verification per call and
            /// returns a [`BudgetedSearchResult`] with a cursor to continue, so the
            /// host can slice a deep-scrollback search across event-loop turns and
            /// CANCEL a superseded query mid-search (drop the cursor; the next call
            /// with a different pattern supersedes the in-flight state).
            ///
            /// Pass `resume_cursor: None` (or a stale value) to start; pass
            /// each step's `cursor` back to continue. A cursor is only valid for the
            /// same engine instance, pattern/options, and unchanged content — any
            /// mismatch restarts from scratch (fresh cursor, progress reset), never
            /// stale results. CPU/GPU wasm modules are separate cursor domains; keep a
            /// token with the engine that issued it. On the
            /// Each response contains a stable match DELTA (at most 4,096 records).
            /// When `reset` is true (or `search_id` changes), clear prior deltas before
            /// appending this step; this makes even a one-turn stale-content restart
            /// unambiguous after the resume cursor disappears. When `complete == true`,
            /// the deltas for that `search_id` equal a one-shot [`Self::search`].
            /// Unlike that legacy API,
            /// `incomplete_index` reports eviction or match-cap truncation and
            /// `lowest_retained_line` identifies the searchable suffix. Empty query or
            /// invalid regex: an immediate empty `complete` result (matching the legacy
            /// API's silence on half-typed regexes). `row_budget == 0` is clamped to one
            /// row so a scanning turn always progresses; a turn may instead drain a
            /// bounded delta backlog without scanning another row.
            pub fn search_budgeted(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
                resume_cursor: Option<u64>,
                row_budget: u32,
            ) -> BudgetedSearchResult {
                let empty = || BudgetedSearchResult {
                    matches: Vec::new(),
                    complete: true,
                    cursor: None,
                    search_id: None,
                    reset: true,
                    incomplete_index: false,
                    lowest_retained_line: 0,
                    rows_fed: 0,
                    total_rows: 0,
                };
                if query.is_empty() {
                    // An empty query also abandons any in-flight search: the host
                    // cleared the find field, so free the partial index now.
                    self.term.cancel_budgeted_search();
                    return empty();
                }
                let Ok(step) = self.term.search_budgeted(
                    query,
                    case_sensitive,
                    is_regex,
                    resume_cursor,
                    row_budget as usize,
                ) else {
                    return empty();
                };
                let mut matches = Vec::with_capacity(step.results.matches.len() * 3);
                for m in &step.results.matches {
                    matches.push(u32::try_from(m.line).unwrap_or(u32::MAX));
                    matches.push(u32::try_from(m.start_col).unwrap_or(u32::MAX));
                    matches.push(u32::try_from(m.len()).unwrap_or(u32::MAX));
                }
                BudgetedSearchResult {
                    matches,
                    complete: step.complete,
                    cursor: step.cursor,
                    search_id: Some(step.search_id),
                    reset: step.reset,
                    incomplete_index: step.results.incomplete,
                    lowest_retained_line: u32::try_from(step.results.lowest_retained_line)
                        .unwrap_or(u32::MAX),
                    rows_fed: u32::try_from(step.rows_fed).unwrap_or(u32::MAX),
                    total_rows: u32::try_from(step.total_rows).unwrap_or(u32::MAX),
                }
            }

            /// Drop any in-flight [`Self::search_budgeted`] state (frees the
            /// partial index; outstanding cursors go stale and restart if resumed).
            /// Call when the find UI closes or the query is abandoned between slices.
            pub fn search_budgeted_cancel(&mut self) {
                self.term.cancel_budgeted_search();
            }

            /// Release the search index's heap (fed E-1 `search_index_release`): drops
            /// the cached full-content index AND any in-flight budgeted search so a
            /// dormant/closed pane's index footprint returns to the allocator, making
            /// federation idle-eviction REAL rather than a logical clear that retains
            /// peak capacity. The next search rebuilds from the live buffer — one
            /// rebuild paid, byte-identical results.
            pub fn search_index_release(&mut self) {
                self.term.release_search_index();
            }

            /// Batch row-range export for the P7 grid mirror (E9): the text/wrap/len
            /// (+ per-column wide map) of `count` DISPLAY rows starting at `first_row`
            /// (display_offset-aware, same coords as [`Self::row_text`]) in ONE
            /// wasm-boundary crossing, replacing the per-row `row_text` +
            /// `row_is_wrapped` + `row_len` + per-cell `cell_is_wide` walk. Returns a
            /// JSON array of exactly `count` records
            /// `{text, wrapped, len, widths?}` in row order; `widths` is a per-column
            /// digit string (`'2'` wide lead / `'1'` otherwise, length == cols) OMITTED
            /// for all-narrow rows so the host reuses its cached all-`'1'` string.
            /// `undefined` when the range is unavailable (a row is out of the live grid,
            /// e.g. resize skew) — the host falls back to the per-row path that frame.
            pub fn row_range_json(&self, first_row: u32, count: u32) -> Option<String> {
                let rows = u32::try_from(self.rows).unwrap_or(u32::MAX);
                let cols = self.cols;
                let end = first_row.checked_add(count)?;
                // Any row past the live grid ⇒ range unavailable this frame.
                if end > rows {
                    return None;
                }
                let mut out = String::from("[");
                for y in first_row..end {
                    if y != first_row {
                        out.push(',');
                    }
                    let row_u16 = u16::try_from(y).ok()?;
                    // Text matches the per-row `row_text` fallback exactly
                    // (display_row_text), so a path switch never spuriously re-dirties.
                    let text = self.term.display_row_text(y as usize).unwrap_or_default();
                    let wrapped = self.term.grid().row_is_wrapped(row_u16).unwrap_or(false);
                    let len = self
                        .term
                        .grid()
                        .row_len(row_u16)
                        .map_or(self.cols, usize::from);
                    // Per-column wide map from the same source `cell_is_wide` reads; the
                    // continuation spacer of a wide cell reports narrow, matching the
                    // host's `cell_is_wide(y,x) ? '2' : '1'` per-cell walk.
                    //
                    // Read straight off ONE row view rather than through
                    // `display_row_grapheme_cells`: that accessor builds a
                    // `Vec<(String, bool)>` with a heap `String` per column carrying the
                    // resolved cluster text — text this loop never touches (the row's
                    // text came from `display_row_text` above), so a 200-col mirror
                    // refresh allocated and freed up to 200 Strings + a Vec PER ROW
                    // purely to read one bit each. `view.cell(col).is_wide()` is exactly
                    // the predicate that accessor computes for its `.1`, so the digit
                    // string is unchanged; an out-of-grid row yields `Empty`, whose
                    // `cell()` is `None` ⇒ the all-`'1'`/omitted shape the old `None`
                    // arm produced. It also materializes a scrolled-in history row once
                    // per record instead of twice.
                    let view = self.term.grid().visible_row_view(row_u16);
                    let mut widths = String::with_capacity(cols);
                    let mut any_wide = false;
                    for col in 0..cols {
                        let wide = u16::try_from(col)
                            .ok()
                            .and_then(|c| view.cell(c))
                            .is_some_and(|c| c.is_wide());
                        if wide {
                            widths.push('2');
                            any_wide = true;
                        } else {
                            widths.push('1');
                        }
                    }
                    out.push_str("{\"text\":");
                    out.push_str(&json_string(&text));
                    out.push_str(",\"wrapped\":");
                    out.push_str(if wrapped { "true" } else { "false" });
                    out.push_str(",\"len\":");
                    out.push_str(&len.to_string());
                    if any_wide {
                        out.push_str(",\"widths\":");
                        out.push_str(&json_string(&widths));
                    }
                    out.push('}');
                }
                out.push(']');
                Some(out)
            }

            /// Federated search summary (fed E-1): the span-marked, snippet-enriched
            /// results for `query` in ONE call — superseding the bare-triplet
            /// [`Self::search`] + per-match `row_text` round-trip. Returns JSON
            /// `{matches:[{absRow,col,len,snippet}], total, incomplete}` where `matches`
            /// is capped to `max_matches` (0 = uncapped), `total` is the full match
            /// count before the cap, `incomplete` is the engine's eviction/match-cap
            /// truncation signal (which [`Self::search`] drops), and `snippet`
            /// is the match line's text (absolute-row coordinate, the same space
            /// federated matches carry). Empty query or invalid regex ⇒
            /// `{matches:[],total:0,incomplete:false}` (mirroring `search`'s silence).
            ///
            /// A bounded READ over an already-built full-content index — not a
            /// from-scratch rebuild on the hot path
            /// ([`Terminal::search_summary_results`]): after a
            /// [`Self::search_budgeted`] scan completes over the same query +
            /// content snapshot, THAT retained index answers directly (zero rebuild);
            /// otherwise the O(1)-reused one-shot index ([`Terminal::indexed_search`])
            /// serves it, rebuilding only on a content-key miss. Either way only the
            /// `≤max_matches` capped rows pay a snippet read. Keeps the E9a
            /// [`Self::search_meta`] `{incomplete,match_count}` shape as a
            /// compat alias — this export supersedes it with snippets.
            pub fn search_summary(
                &mut self,
                query: &str,
                case_sensitive: bool,
                is_regex: bool,
                max_matches: u32,
            ) -> Option<String> {
                let empty = || Some("{\"matches\":[],\"total\":0,\"incomplete\":false}".to_owned());
                if query.is_empty() {
                    return empty();
                }
                let (matches, total, incomplete) = {
                    let Ok(results) =
                        self.term
                            .search_summary_results(query, case_sensitive, is_regex)
                    else {
                        return empty();
                    };
                    let total = results.matches.len();
                    let cap = if max_matches == 0 {
                        total
                    } else {
                        (max_matches as usize).min(total)
                    };
                    // Copy the capped (line, start_col, len) triplets out so the &self
                    // index borrow ends before the &self snippet reads below.
                    let matches: Vec<(u64, usize, usize)> = results.matches[..cap]
                        .iter()
                        .map(|m| (m.line as u64, m.start_col, m.len()))
                        .collect();
                    (matches, total, results.incomplete)
                };
                let mut out = String::from("{\"matches\":[");
                for (i, (abs, col, len)) in matches.iter().enumerate() {
                    if i != 0 {
                        out.push(',');
                    }
                    let snippet = self.term.abs_row_text(*abs).unwrap_or_default();
                    out.push_str("{\"absRow\":");
                    out.push_str(&abs.to_string());
                    out.push_str(",\"col\":");
                    out.push_str(&col.to_string());
                    out.push_str(",\"len\":");
                    out.push_str(&len.to_string());
                    out.push_str(",\"snippet\":");
                    out.push_str(&json_string(&snippet));
                    out.push('}');
                }
                out.push_str("],\"total\":");
                out.push_str(&total.to_string());
                out.push_str(",\"incomplete\":");
                out.push_str(if incomplete { "true" } else { "false" });
                out.push('}');
                Some(out)
            }
        }
    };
}

web_terminal_impl!(bind);

#[cfg(test)]
mod tests {
    use super::*;

    /// JS can supply any i32 endpoint. Selection remains clipped to the
    /// viewport while scrolling changes the coordinate origin underneath it.
    #[test]
    fn hostile_selection_rows_stay_offscreen_without_wrapping() {
        let font = include_bytes!("../../aterm-render/assets/DejaVuSansMono.ttf");
        let mut t = WebTerminal::new(3, 8, font, 14.0, 0, 0, 0, 0).expect("bundled face");
        for _ in 0..8 {
            t.process(b"line\r\n");
        }
        t.selection_start(i32::MAX, 0);
        t.selection_extend(i32::MAX, 7);
        t.scroll_lines(4);
        assert!(
            t.selection_range().is_none(),
            "a far-below row must stay below"
        );

        t.selection_start(i32::MIN, 0);
        t.selection_extend(i32::MAX, 7);
        t.selection_finish();
        let range = t.selection_range().expect("selection crosses the viewport");
        assert_eq!((range.start_y(), range.end_y()), (0, 2));
        assert_eq!((range.start_x(), range.end_x()), (0, 7));

        assert!(t.selection_line(i32::MIN, 0).is_none());
        assert!(t.selection_range().is_none());
        assert!(t.selection_word(i32::MIN, 0).is_none());
        assert!(t.selection_range().is_none());
    }
}
