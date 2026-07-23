// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pointer, selection, hover, and link handling: the mouse/cursor event handlers
//! plus the selection gesture state machine (click streaks, word/line/block
//! select, drag, copy), pane-under-pointer focus, pixel→cell mapping, and the
//! scroll snap. A verbatim inherent-impl split of `App`.

use std::cell::Cell;
use std::time::Instant;

use aterm_core::selection::{SelectionSide, SelectionType};
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::window::CursorIcon;

use crate::app_render::{pixel_to_term_cell, strip_col_for_pixel};
use crate::input::{InputEvent, Source};
use crate::{
    App, GestureOrigin, MULTI_CLICK_MS, WindowId, control, is_safe_url, pane, plain_url_at,
    term_lock,
};

/// Map a winit mouse button to the engine's [`aterm_types::mouse::MouseButton`]
/// for an [`InputEvent::MouseButton`]. `None` for buttons the GUI does not report
/// (Back/Forward/Other), so the handler can early-return.
pub(crate) fn winit_mouse_button(b: WinitMouseButton) -> Option<aterm_types::mouse::MouseButton> {
    use aterm_types::mouse::MouseButton;
    match b {
        WinitMouseButton::Left => Some(MouseButton::Left),
        WinitMouseButton::Middle => Some(MouseButton::Middle),
        WinitMouseButton::Right => Some(MouseButton::Right),
        _ => None,
    }
}

/// Whether the "open link" modifier is held: Cmd (Super) on macOS — its native
/// convention — and Ctrl on every other platform, because Linux/X11 desktops grab
/// the Super (Windows) key, so a Super-click would never reach aterm. Mirrors the
/// keybinding accelerator choice (Ctrl/Ctrl+Shift on Linux).
fn link_modifier_held(mods: winit::keyboard::ModifiersState) -> bool {
    #[cfg(target_os = "macos")]
    {
        mods.super_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        mods.control_key()
    }
}

/// Audit M6: whether a left press starts a LOCAL text selection instead of being
/// reported to a tracking app — always when nothing tracks the mouse, and with
/// Option held even when something does (iTerm2's bypass gesture), so a PID can be
/// copied out of htop. The matching release settles the selection without leaking a
/// release report (`selecting` wins in `on_mouse_input`), keeping press/release
/// symmetric for the app.
pub(crate) fn press_starts_selection(tracking: bool, option_held: bool) -> bool {
    !tracking || option_held
}

/// Audit M6: selection kind for a single left press. Option+drag is the rectangular
/// [`SelectionType::Block`] ONLY when the press was local anyway (tracking OFF);
/// while Option is doing the tracking-bypass job it selects normally (`Simple`) —
/// the modifier is spent on the bypass, matching iTerm2.
pub(crate) fn press_selection_kind(option_held: bool, tracking: bool) -> SelectionType {
    if option_held && !tracking {
        SelectionType::Block
    } else {
        SelectionType::Simple
    }
}

// Divider-drag relayout throttle — the pane-split analogue of the window
// live-resize throttle (`on_resize_throttled`): every applied ratio step resizes
// both child panes' engines, and a width change rewraps the ENTIRE off-screen
// scrollback per pane on the event-loop thread, so unthrottled per-move applies
// hitch the drag. The ratio and the relayout apply TOGETHER (at most once per
// [`crate::RESIZE_THROTTLE`] window), so layout rects and engine/PTY sizes never
// diverge mid-drag and no coalesced intermediate size ever reaches `term.resize`
// (the recorder sees only applied sizes). `finish_divider_drag` flushes a
// coalesced step so the release always lands the final pointer position. Pointer
// events run on the winit event-loop thread, so thread-local state suffices;
// entries carry the window id so a stale stamp can never gate another window.
thread_local! {
    /// When a divider drag last APPLIED a relayout (leading-edge stamp).
    static PANE_DRAG_APPLIED_AT: Cell<Option<(WindowId, Instant)>> = const { Cell::new(None) };
    /// Window with a coalesced (skipped) drag step awaiting the release flush.
    static PANE_DRAG_PENDING: Cell<Option<WindowId>> = const { Cell::new(None) };
}

impl App {
    /// Modal palette pointer boundary. It clears any hover/press retained by the native
    /// app underneath, synchronizes the cursor to the palette row hover, and reports
    /// whether the caller must swallow the gesture. This runs before native hit testing
    /// for motion, buttons, and wheel input, preventing a visible palette from becoming an
    /// invisible click-through layer over an editor or Settings control.
    pub(crate) fn palette_claims_pointer(&mut self, wid: WindowId) -> bool {
        if self
            .windows
            .get(&wid)
            .is_none_or(|window| window.palette().is_none())
        {
            return false;
        }

        let mut changed = None;
        if let Some((_, view)) = self.active_native_view(wid)
            && let Some(state) = self.native_runtime.view_state_mut(view)
        {
            let common = state.common_mut();
            let hovered = common.hovered.take().is_some();
            let pressed = common.pressed.take().is_some();
            if hovered || pressed {
                changed = Some(view);
            }
        }
        if let Some(view) = changed {
            // Hover/pressed are stamp-neutral visual state: damage the one leaf
            // so its wash repaints (the same law as the `on_cursor_moved` hover
            // path), but KEEP the retained scene — closing the modal re-resolves
            // the pointer against that hit tree immediately, before any repaint.
            self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
            self.request_redraw_all_windows();
        }
        self.sync_palette_pointer_cursor(wid);
        true
    }

    /// Convert a window-space point into the palette's card-local geometry and return the
    /// filtered row it hits. Terminal frames composite the card at
    /// `(pad, pad_top + head)`;
    /// native tabs composite it inside the scaled app texture below host tab chrome. These
    /// are the exact two placement laws in `app_render`, reversed here before calling the
    /// shared painter/hit rectangle.
    fn palette_row_at_pointer(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let window = self.windows.get(&wid)?;
        let palette = window.palette()?;
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let panel_rows = window.overlay_rows();
        let (frame_x, frame_y) = self.window_to_frame(wid, x, y);
        let native = self.active_native_view(wid).is_some();
        let (scale, local_x, local_y, font_px) = if native {
            let scale = window.scale.max(f64::EPSILON);
            (
                scale,
                (frame_x - pad as f64) / scale,
                (frame_y - self.native_content_origin_y(wid) as f64) / scale,
                13.0,
            )
        } else {
            (
                1.0,
                frame_x - pad as f64,
                frame_y - (pad_top + self.win_head(wid)) as f64,
                self.win_font_px(wid),
            )
        };
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32 / scale as f32,
            ch: ch as f32 / scale as f32,
            font_px,
            cols: window.cols as usize,
            panel_rows,
        };
        crate::palette::palette_row_hit(palette, &geom, local_x as f32, local_y as f32)
    }

    /// Keep the OS cursor aligned with the palette's current row hover. The existing
    /// `hover_pointer` bit is safe to reuse while the modal is open because the underlying
    /// native/terminal hover authority has already been cleared at the boundary above.
    pub(crate) fn sync_palette_pointer_cursor(&mut self, wid: WindowId) {
        let pointer = self
            .windows
            .get(&wid)
            .and_then(|window| window.palette())
            .is_some_and(crate::palette::PaletteState::pointer_over_row);
        if let Some(window) = self.windows.get_mut(&wid)
            && (window.hover_pointer != pointer || window.native_text_cursor)
        {
            window.hover_pointer = pointer;
            window.native_text_cursor = false;
            if let Some(os_window) = &window.os_window {
                os_window.set_cursor(if pointer {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                });
            }
        }
    }

    fn repaint_palette_pointer(&mut self, wid: WindowId, changed: bool) {
        self.sync_palette_pointer_cursor(wid);
        if !changed {
            return;
        }
        if let Some(window) = self
            .windows
            .get(&wid)
            .and_then(|window| window.os_window.as_ref())
        {
            window.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Hover-select the row under a pointer motion; outside points remain swallowed while
    /// clearing the pointer cursor. Selection changes repaint the same ring the hit rect
    /// came from.
    pub(crate) fn palette_pointer_motion(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.palette_row_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.pointer_hover(hit));
        self.repaint_palette_pointer(wid, changed);
    }

    fn palette_pointer_press(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.palette_row_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.pointer_press(hit));
        self.repaint_palette_pointer(wid, changed);
    }

    fn palette_pointer_release(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.palette_row_at_pointer(wid, x, y);
        let (changed, activate) = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .map_or((false, false), |palette| palette.pointer_release(hit));
        self.repaint_palette_pointer(wid, changed);
        if activate {
            self.palette_activate();
        }
    }

    /// Scroll the filtered palette band, then resolve the stationary pointer against the
    /// new painted rows. This keeps hover/selection and hit geometry in lockstep and
    /// cancels an armed click before any row can move beneath its release.
    fn palette_pointer_wheel(&mut self, wid: WindowId, delta: isize) {
        let mut changed = self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.scroll_by(delta));
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |window| window.last_cursor_px);
        let hit = self.palette_row_at_pointer(wid, x, y);
        changed |= self
            .windows
            .get_mut(&wid)
            .and_then(|window| window.palette_mut())
            .is_some_and(|palette| palette.pointer_hover(hit));
        self.repaint_palette_pointer(wid, changed);
    }

    /// The FOCUSED pane's top-left `(row_off, col_off)` cell offset in window
    /// `wid`'s grid. `(0, 0)` when the focused pane fills the window (no splits) — so
    /// subtracting it from a window mouse cell is a no-op on the single-pane path,
    /// keeping mouse handling byte-identical. Used to translate window mouse coords
    /// into the focused pane's local grid (its engine expects pane-local cells).
    pub(crate) fn focused_pane_origin(&self, wid: WindowId) -> (u16, u16) {
        self.active_visible_leaf_plan(wid)
            .and_then(|plan| plan.leaf(plan.focused).cloned())
            .map_or((0, 0), |leaf| {
                (
                    leaf.rect.origin.y.round().max(0.0) as u16,
                    leaf.rect.origin.x.round().max(0.0) as u16,
                )
            })
    }

    /// The FOCUSED pane's full rect `(row_off, col_off, rows, cols)` in window `wid`'s
    /// grid — the geometry [`focused_pane_origin`] returns only the offset of. The
    /// whole window `(0, 0, rows, cols)` on the single-pane fast path. Used to
    /// translate AND CLAMP a window mouse cell into the focused pane's own grid, so a
    /// drag that crosses a divider into a neighbouring pane still resolves to a cell
    /// INSIDE the focused pane (whose engine is sized to this sub-rect), rather than
    /// addressing an out-of-pane cell.
    pub(crate) fn focused_pane_rect(&self, wid: WindowId) -> (u16, u16, u16, u16) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0, 0, 0, 0);
        };
        self.active_visible_leaf_plan(wid)
            .and_then(|plan| plan.leaf(plan.focused).cloned())
            .map_or((0, 0, ws.rows, ws.cols), |leaf| {
                (
                    leaf.rect.origin.y.round().max(0.0) as u16,
                    leaf.rect.origin.x.round().max(0.0) as u16,
                    (leaf.rect.size.height.round() as u16).max(1),
                    (leaf.rect.size.width.round() as u16).max(1),
                )
            })
    }

    /// Click-to-focus in window `wid`: if its last pointer position (window cell)
    /// lands on a pane OTHER than the focused one, move focus there (re-mirroring the
    /// control socket + renderer onto it) and re-derive the pane-local mouse cell.
    /// Returns `true` iff focus moved (the caller then swallows the press). A press
    /// in the already-focused pane, on a divider, or in a single-pane tab returns
    /// `false` (the press proceeds to the normal selection/tracking path).
    pub(crate) fn focus_pane_under_pointer(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (wr, wc) = ws.last_mouse_window_cell;
        let Some(plan) = self.active_visible_leaf_plan(wid) else {
            return false;
        };
        if plan.leaves.len() == 1 {
            return false;
        }
        let Some(hit) = self.visible_view_at_cell(wid, wr, wc) else {
            return false; // divider / outside grid: nothing to focus
        };
        if hit == plan.focused {
            return false; // already focused: proceed with the normal press
        }
        let terminal_projection = self.active_tree(wid).is_some();
        let moved = if terminal_projection {
            let Some(session) = self
                .view_store
                .get(hit)
                .copied()
                .and_then(crate::tab_model::View::terminal_session)
            else {
                return false;
            };
            self.active_tree_mut(wid)
                .is_some_and(|tree| tree.set_focus(session))
        } else {
            self.windows
                .get_mut(&wid)
                .and_then(|window| window.tab_set.active_mut())
                .is_some_and(|tab| tab.set_focus(hit))
        };
        if !moved {
            return false;
        }
        if terminal_projection {
            let active = self.windows.get(&wid).map_or(0, |ws| ws.tabs.active);
            let synced = self.sync_tab_model_from_layout(wid, active);
            debug_assert!(synced);
        }
        // Re-derive the pane-local mouse cell for the newly-focused pane so any
        // follow-up gesture uses its grid; re-mirror term/master/socket onto it.
        let (ro, co) = self.focused_pane_origin(wid);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_cell = (wr.saturating_sub(ro), wc.saturating_sub(co));
        }
        self.sync_window(wid);
        true
    }

    /// If window `wid`'s last pointer position lands on a pane DIVIDER, arm a
    /// divider-resize drag on it and return `true` (the caller then swallows the
    /// press — it neither focuses a pane nor starts a selection). Returns `false`
    /// for a press inside a pane / a single-pane (or zoomed) tab, so the normal
    /// press path proceeds. The armed [`pane::DividerHit`] is held in
    /// `ws.divider_drag` until release; `drag_divider` consumes it on each move.
    pub(crate) fn begin_divider_drag(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (wr, wc) = ws.last_mouse_window_cell;
        let hit = if let Some(tree) = self.active_tree(wid) {
            if tree.len() == 1 {
                return false;
            }
            let Some(hit) = tree.divider_at(wr, wc, ws.rows, ws.cols) else {
                return false;
            };
            crate::DividerDrag::Terminal(hit)
        } else {
            let Some(plan) = self.active_visible_leaf_plan(wid) else {
                return false;
            };
            let Some(divider) = plan.divider_at(crate::tab_model::LogicalPoint {
                x: f32::from(wc),
                y: f32::from(wr),
            }) else {
                return false;
            };
            crate::DividerDrag::Canonical {
                path: divider.path.clone(),
                axis: divider.axis,
            }
        };
        // Resize-cursor affordance for the drag: E-W for a vertical divider (columns
        // move), N-S for a horizontal one (rows move).
        let icon = match &hit {
            crate::DividerDrag::Terminal(hit) => match hit.dir {
                pane::SplitDir::Vertical => CursorIcon::ColResize,
                pane::SplitDir::Horizontal => CursorIcon::RowResize,
            },
            crate::DividerDrag::Canonical { axis, .. } => match axis {
                crate::tab_model::SplitAxis::Horizontal => CursorIcon::ColResize,
                crate::tab_model::SplitAxis::Vertical => CursorIcon::RowResize,
            },
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.divider_drag = Some(hit);
            if let Some(w) = &ws.os_window {
                w.set_cursor(icon);
            }
        }
        // A fresh drag starts with a clean throttle: its first move must apply
        // immediately (leading edge), and no stale pending from a drag that ended
        // without a release (focus loss drops the gesture) may flush into it.
        PANE_DRAG_APPLIED_AT.with(|c| c.set(None));
        PANE_DRAG_PENDING.with(|c| c.set(None));
        true
    }

    /// Mid-drag of a pane divider: map the current pointer (window cell) to the held
    /// divider's split ratio and apply it, then relay out (resize every pane's
    /// engine/PTY) and repaint. A no-op when no divider is being dragged. The ratio
    /// is clamped inside [`pane::PaneTree::set_divider_ratio`], so a drag past either
    /// edge just pins the boundary at the `[MIN_RATIO, MAX_RATIO]` floor/ceiling.
    /// Applies at most once per [`crate::RESIZE_THROTTLE`] window (leading edge);
    /// steps inside the window are coalesced into a pending flag the next
    /// out-of-window move or [`Self::finish_divider_drag`] flushes.
    pub(crate) fn drag_divider(&mut self, wid: WindowId) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        let Some(hit) = ws.divider_drag.clone() else {
            return;
        };
        let now = Instant::now();
        let gated = PANE_DRAG_APPLIED_AT.with(|c| {
            matches!(c.get(), Some((w, t)) if w == wid
                && now.saturating_duration_since(t) < crate::RESIZE_THROTTLE)
        });
        if gated {
            PANE_DRAG_PENDING.with(|c| c.set(Some(wid)));
            return;
        }
        if self.apply_divider_drag(wid, &hit) {
            PANE_DRAG_APPLIED_AT.with(|c| c.set(Some((wid, now))));
            PANE_DRAG_PENDING.with(|c| c.set(None));
        }
    }

    /// One divider-drag step: map the current pointer to the held divider's ratio,
    /// apply it, relay out (resize every pane's engine/PTY), and repaint. Returns
    /// whether the ratio was written (a stale hit whose path no longer names a
    /// split leaves the tree untouched). Ratio + relayout land together, so layout
    /// rects and engine/PTY sizes never diverge mid-drag.
    fn apply_divider_drag(&mut self, wid: WindowId, hit: &crate::DividerDrag) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let (wr, wc) = ws.last_mouse_window_cell;
        let (applied, terminal_projection) = match hit {
            crate::DividerDrag::Terminal(hit) => {
                let Some(tree) = self.active_tree(wid) else {
                    return false;
                };
                let Some(ratio) = tree.ratio_for_pointer(hit, wr, wc) else {
                    return false;
                };
                (
                    self.active_tree_mut(wid)
                        .is_some_and(|tree| tree.set_divider_ratio(hit, ratio)),
                    true,
                )
            }
            crate::DividerDrag::Canonical { path, .. } => {
                let Some(plan) = self.active_visible_leaf_plan(wid) else {
                    return false;
                };
                let Some(divider) = plan.dividers.iter().find(|divider| divider.path == *path)
                else {
                    return false;
                };
                let Some(ratio) = plan.ratio_for_pointer(
                    divider,
                    crate::tab_model::LogicalPoint {
                        x: f32::from(wc),
                        y: f32::from(wr),
                    },
                ) else {
                    return false;
                };
                (
                    self.windows
                        .get_mut(&wid)
                        .and_then(|window| window.tab_set.active_mut())
                        .is_some_and(|tab| tab.set_divider_ratio(path, ratio)),
                    false,
                )
            }
        };
        if !applied {
            return false;
        }
        if terminal_projection {
            let active = self.windows.get(&wid).map_or(0, |ws| ws.tabs.active);
            let synced = self.sync_tab_model_from_layout(wid, active);
            debug_assert!(synced);
        }
        // Resize every pane's engine/PTY to its new sub-rect, then repaint the frame.
        self.resize_panes(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        true
    }

    /// End any in-flight divider drag (left release). Returns whether a drag was
    /// active (the caller then swallows the release rather than completing a
    /// selection that never started). Flushes a throttle-coalesced final step
    /// first, so the engines/PTY always land on the released pointer position (a
    /// coalesced intermediate size must never be the terminal one).
    pub(crate) fn finish_divider_drag(&mut self, wid: WindowId) -> bool {
        let Some(hit) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.divider_drag.clone())
        else {
            return false;
        };
        if PANE_DRAG_PENDING.with(|c| c.get()) == Some(wid) {
            PANE_DRAG_PENDING.with(|c| c.set(None));
            self.apply_divider_drag(wid, &hit);
        }
        PANE_DRAG_APPLIED_AT.with(|c| c.set(None));
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.divider_drag = None;
            // Restore the default cursor (the hover state machine re-asserts the
            // link pointer on the next move if warranted).
            ws.hover_pointer = false;
            ws.native_text_cursor = false;
            if let Some(w) = &ws.os_window {
                w.set_cursor(CursorIcon::Default);
            }
        }
        true
    }

    /// Settle any in-flight pointer drag on `wid` — divider resize and/or text
    /// selection — exactly as their left-release paths in [`Self::on_mouse_input`]
    /// would. Called when a modal overlay opens (it steals the mouse, so the drag's
    /// release will be swallowed) and on a release the modal gate swallows; without
    /// this, `divider_drag`/`selecting` stay wedged and every pointer motion keeps
    /// resizing the split / growing the selection under (and after) the modal.
    pub(crate) fn settle_pointer_drags(&mut self, wid: WindowId) {
        if self.finish_divider_drag(wid)
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.held_mouse_button = None;
        }
        if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            // A settle is housekeeping on a HUMAN drag whose release was swallowed
            // by a modal — never a scoped-edge gesture — so copy-on-select is not
            // suppressed here (false).
            self.finish_selection(wid, false);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.held_mouse_button = None;
                // Clear the LEFT reported bit like the release path does, so a later
                // non-reporting press can't leak an orphan release (`1` = Left).
                ws.reported_buttons &= !1u8;
            }
        }
        // An ABOUT text-selection drag settles too (a missed release — focus loss
        // mid-sweep): finish WITHOUT the release path's copy-on-select/link-open —
        // a settle is housekeeping, not a user gesture.
        let settled = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.about_mut())
            .is_some_and(|a| {
                if !a.dragging() {
                    return false;
                }
                let _ = a.disarm_link();
                let _ = a.sel_finish();
                true
            });
        if settled && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// Cmd-C: copy the selected text to the macOS system clipboard (`pbcopy`).
    /// Returns whether anything was copied; the selection is NOT cleared (so a
    /// highlight survives the copy, and repeated copies work).
    pub(crate) fn copy_selection(&self) -> bool {
        let Some(wid) = self.frontmost_window else {
            return false;
        };
        let Some(terminal) = self.front_terminal(wid) else {
            return false;
        };
        let Some(text) = term_lock(&terminal.term).selection_to_string() else {
            return false;
        };
        !text.is_empty() && control::pbcopy(&text)
    }

    /// Map a pixel position to a 0-based (row, col) TERMINAL grid cell of window
    /// `wid`, clamped to the grid. Three insets are stripped first (see
    /// [`pixel_to_term_cell`]): the chrome `head` band (y only), the interior `pad`
    /// border around the whole window, then the `tab_strip_rows` pixel rows of the
    /// strip — so a click in the terminal region lands on the right terminal row,
    /// and a click in the strip/pad/head border clamps to terminal row 0 (the
    /// caller intercepts strip clicks via [`Self::strip_col_at`] BEFORE using
    /// this). With `pad == 0` && `tab_strip_rows == 0` && `head == 0` this is
    /// byte-identical to the pre-strip mapping.
    pub(crate) fn pixel_to_cell(&self, wid: WindowId, x: f64, y: f64) -> (u16, u16) {
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        let (x, y) = self.window_to_frame(wid, x, y);
        pixel_to_term_cell(
            x,
            y,
            cw,
            ch,
            rows,
            cols,
            self.tab_strip_rows,
            pad,
            pad_top,
            head,
        )
    }

    /// Refresh the remembered cells under the pointer — the raw WINDOW cell
    /// (click-to-focus hit-testing) and the FOCUSED-PANE-LOCAL cell (PTY mouse
    /// reports), the latter CLAMPED to the pane's own grid so a drag crossing a
    /// divider stays inside the focused pane's sub-rect. Returns
    /// `((window_row, window_col), (pane_row, pane_col))`. Shared by the normal
    /// motion path and the About-modal gate: the modal swallows motion, but must
    /// not leave a STALE cell for the first click after a keyboard close.
    fn refresh_mouse_cell(&mut self, wid: WindowId, x: f64, y: f64) -> ((u16, u16), (u16, u16)) {
        let (row, col) = self.pixel_to_cell(wid, x, y);
        let (ro, co, prows, pcols) = self.focused_pane_rect(wid);
        let lr = row.saturating_sub(ro).min(prows.saturating_sub(1));
        let lc = col.saturating_sub(co).min(pcols.saturating_sub(1));
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (row, col);
            ws.last_mouse_cell = (lr, lc);
        }
        ((row, col), (lr, lc))
    }

    /// W1 (kill the compositor stretch): where the (padded) FRAME's top-left sits
    /// inside the raw window — the leading remainder bands the present paths place
    /// the frame behind (`aterm_render::band_offset`; the render side's
    /// `content_off`). SIGNED because a transient/tiny destination crops the
    /// centred frame and therefore has a negative source offset. `(0, 0)` at
    /// exact grid fit, headless, and pre-attach, so
    /// every consumer is byte-identical there. Derived from the exact composed
    /// frame extent the presenters use, including independent top/bottom padding,
    /// so pointer geometry and pixels can't disagree in the settled state.
    pub(crate) fn frame_origin(&self, wid: WindowId) -> (i64, i64) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0, 0);
        };
        let Some(size) = ws.win_px else {
            return (0, 0);
        };
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        let composed_rows = usize::from(ws.rows)
            .saturating_add(usize::from(self.tab_strip_rows))
            .saturating_add(usize::from(self.hud_rows.min(ws.hud_cap)));
        let frame_w = usize::from(ws.cols)
            .saturating_mul(cw)
            .saturating_add(pad.saturating_mul(2));
        let frame_h = composed_rows
            .saturating_mul(ch)
            .saturating_add(head)
            .saturating_add(pad_top)
            .saturating_add(pad);
        (
            aterm_render::band_offset(size.width as usize, frame_w),
            aterm_render::band_offset(size.height as usize, frame_h),
        )
    }

    /// Translate a window-space pixel to FRAME-space (strip the leading bands),
    /// clamped at 0 so a click IN a band maps to the frame edge — the same
    /// semantics a click in the pad border already had. The single entry seam for
    /// every pointer→frame consumer (cells, strip, settings/about overlays, IME).
    pub(crate) fn window_to_frame(&self, wid: WindowId, x: f64, y: f64) -> (f64, f64) {
        let (ox, oy) = self.frame_origin(wid);
        ((x - ox as f64).max(0.0), (y - oy as f64).max(0.0))
    }

    /// If pixel position `(x, y)` lands in window `wid`'s tab-strip region (the top
    /// `tab_strip_rows` pixel rows), return its strip COLUMN; otherwise `None` (the
    /// click is in the terminal region and maps to a cell as usual). Always `None`
    /// when the strip is disabled. Used by the mouse handlers to intercept strip
    /// clicks BEFORE the focused-pane cell mapping.
    pub(crate) fn strip_col_at(&self, wid: WindowId, x: f64, y: f64) -> Option<u16> {
        if !self.tab_strip_enabled() {
            return None;
        }
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols);
        let (x, y) = self.window_to_frame(wid, x, y);
        strip_col_for_pixel(x, y, cw, ch, cols, self.tab_strip_rows, pad, pad_top, head)
    }

    /// A left click on the Cmd-F find bar's `Aa` (case) / `.*` (regex) indicators fires
    /// the matching toggle — the SAME action as the ⌥⌘C / ⌥⌘R chords, so a mouse user
    /// can flip modes without the keyboard. `row`/`col` are TERMINAL cell coordinates
    /// (`pixel_to_cell`), matching the geometry `splice_find_bar` recorded in
    /// [`crate::FindBarHit`]. Precise: returns `false` (unconsumed) off the indicator
    /// spans or when not searching, so ordinary terminal clicks fall through untouched.
    pub(crate) fn find_bar_click(&mut self, wid: WindowId, row: u16, col: u16) -> bool {
        let Some(hit) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.find_bar_hit.clone())
        else {
            return false;
        };
        if row as usize != hit.row {
            return false;
        }
        let col = col as usize;
        if hit.case_cols.is_some_and(|r| r.contains(&col)) {
            self.search_toggle_case();
            true
        } else if hit.regex_cols.is_some_and(|r| r.contains(&col)) {
            self.search_toggle_regex();
            true
        } else {
            false
        }
    }

    /// Pixel-precise gate for [`Self::find_bar_click`]: `true` only when window pixel
    /// `(x, y)` lands INSIDE the find bar's actual cell band, not merely on the row
    /// [`Self::pixel_to_cell`] would clamp it to. That clamp is the hazard: a click in
    /// the top `pad_top` border (or anywhere above the grid) snaps to row 0 —
    /// coincidentally the bar's row in the usual TOP placement — and a click in the
    /// bottom `pad` snaps to the last terminal row (the bar's row when it floats to the
    /// bottom), so without this gate either would false-toggle a mode. The band is
    /// `[pad_top + head + frame_row*ch, +ch)` on the frame's y-axis, where
    /// `frame_row = tab_strip_rows + hit.row` mirrors the strip prepend `splice_find_bar`
    /// applied; the x-check rejects clicks past the grid's right edge (which clamp onto
    /// the last column). `None` bar ⇒ `false`. Uses the SAME [`Self::window_to_frame`]
    /// band-strip as `pixel_to_cell`, so the two can't disagree in the settled state.
    pub(crate) fn find_bar_pixel_hit(&self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some(hit_row) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.find_bar_hit.as_ref())
            .map(|h| h.row)
        else {
            return false;
        };
        let (cw, ch) = self.win_cell_size(wid);
        let (cw, ch) = (cw.max(1), ch.max(1));
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols) as usize;
        // UNCLAMPED frame coords: `window_to_frame` clamps with `.max(0.0)`, which would
        // turn a click ABOVE the frame (negative y — e.g. the TOP pad over a top-anchored
        // bar) into frame-y 0 and land it in the row-0 band. Subtract the origin directly
        // so a genuinely above-grid click stays negative and is rejected below. (The
        // bottom-anchored bar never risked this; the top bar does.)
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64, y - oy as f64);
        let frame_row = self.tab_strip_rows as usize + hit_row;
        let top = (pad_top + self.win_head(wid) + frame_row * ch) as f64;
        if fy < top || fy >= top + ch as f64 || fx < pad as f64 {
            return false;
        }
        // Reject clicks past the right edge — `pixel_to_cell` would clamp their column
        // onto the last grid cell, which can land on an indicator span.
        let gx = (fx as usize).saturating_sub(pad);
        (gx / cw) < cols
    }

    /// Convert a raw window-space pointer to Settings-card-local pixels through
    /// the same frame remainder, asymmetric top inset, and chrome headroom used
    /// by the compositor. Press and drag must share this seam or the colour wheel
    /// visibly jumps as soon as the pointer moves.
    fn settings_card_point(&self, wid: WindowId, x: f64, y: f64) -> (f32, f32) {
        let (x, y) = self.window_to_frame(wid, x, y);
        (
            (x - self.win_pad(wid) as f64) as f32,
            (y - self.win_pad_top(wid) as f64 - self.win_head(wid) as f64) as f32,
        )
    }

    /// Resolve a raw window point only when it lies inside the exact Settings
    /// card rectangle the compositor publishes. Unlike `pixel_to_cell`, this is
    /// deliberately unclamped: the four remainder/padding bands surrounding the
    /// card are modal chrome, never aliases for sidebar rows or right-edge widgets.
    fn settings_card_point_if_inside(&self, wid: WindowId, x: f64, y: f64) -> Option<(f32, f32)> {
        let ws = self.windows.get(&wid)?;
        ws.settings()?;
        let (cw, ch) = self.win_cell_size(wid);
        let (origin_x, origin_y) = self.frame_origin(wid);
        let left = origin_x.saturating_add(self.win_pad(wid) as i64) as f64;
        let top = origin_y
            .saturating_add(self.win_pad_top(wid) as i64)
            .saturating_add(self.win_head(wid) as i64) as f64;
        let right = left + f64::from(ws.cols) * cw.max(1) as f64;
        let bottom = top + ws.settings_panel_rows() as f64 * ch.max(1) as f64;
        (x >= left && x < right && y >= top && y < bottom)
            .then_some(((x - left) as f32, (y - top) as f32))
    }

    /// While window `wid`'s Settings overlay is open, map pixel `(x, y)` to the CONTROL
    /// INDEX under the pointer in the CONTENT pane's group band. The band's placement
    /// comes from the same [`crate::settings::pane_geom_cells`] the painter uses, and
    /// the row walk is the SAME layout (grouped, or the flat search list while
    /// filtering) — so a click maps to exactly the control drawn there. `None` in the
    /// sidebar, on captions/footnotes/gaps, on the title/preview/footer, or when closed.
    pub(crate) fn settings_row_at(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let ws = self.windows.get(&wid)?;
        let s = ws.settings()?;
        let (cw, ch) = self.win_cell_size(wid);
        let panel_rows = ws.settings_panel_rows(); // single source (C2)
        if panel_rows < 2 {
            return None;
        }
        let (cx, cy) = self.settings_card_point_if_inside(wid, x, y)?;
        let pg = crate::settings::pane_geom_cells(ws.cols as usize, panel_rows);
        if cx < pg.sidebar_w_cells * cw as f32 {
            return None; // the sidebar is not a control row (settings_click routes it)
        }
        let frame_row = cy as usize / ch.max(1);
        let rel = frame_row.checked_sub(pg.groups.0)?;
        let band = pg.group_band();
        if s.filtering() {
            // The flat search list: the SAME masked layout the painter renders.
            let mask = s.visible_mask();
            match crate::settings::body_layout_masked(&s.fields, mask.as_deref(), s.scroll, band)
                .get(rel)
            {
                Some(&crate::settings::BodyRow::Control(idx)) => Some(idx),
                _ => None,
            }
        } else {
            // The SAME wrap width the painter laid the band out with, so a click
            // under a wrapped footnote resolves the row actually drawn there.
            let wrap = crate::settings::footnote_wrap_chars(ws.cols as usize);
            let rows = crate::settings::category_layout(&s.fields, s.category, wrap);
            match crate::settings::group_row_at(&rows, s.scroll, band, rel) {
                Some(crate::settings::GroupRow::Control(idx)) => Some(idx),
                _ => None, // caption / footnote / gap / past the painted window
            }
        }
    }

    /// A LEFT PRESS inside the open Settings overlay, x-aware and non-destructive:
    ///
    /// - popup menu open → a press on an option COMMITS it; anywhere else closes the
    ///   menu with no change (the gesture is still swallowed — the panel stays modal);
    /// - a row that is NOT selected → only SELECT it (no mutation on first click; an
    ///   in-flight free-form edit on another row is abandoned first, exactly like Esc);
    /// - the already-selected row → activate ONLY in its widget region (open menu /
    ///   toggle / cycle / begin edit); a press on the label region is a no-op.
    ///
    /// Pixel→geometry uses the SAME card placement as the painter (`splice_settings_panel`
    /// composites the card at `(pad, pad_top + head)`) and the same [`crate::settings::menu_geom`] /
    /// [`crate::settings::widget_hit_left`] the pixels come from, so click == pixels.
    pub(crate) fn settings_click(&mut self, wid: WindowId, x: f64, y: f64) {
        let (window_x, window_y) = (x, y);
        let (cx, cy) = self.settings_card_point(wid, window_x, window_y);
        let card_point = self.settings_card_point_if_inside(wid, window_x, window_y);

        // The open COLOUR WHEEL captures every press (mutually exclusive with the
        // menu below): a press on the disk/slider scrubs it and ARMS a drag (motion
        // keeps scrubbing until release), the hex readout takes focus, the popover
        // chrome swallows, and a click-away cancels with NO change — all resolved
        // through the same pure `wheel_hit` the painter's geometry comes from.
        let wheel_up = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.settings())
            .is_some_and(|s| s.wheel.is_some());
        if wheel_up {
            let hit = self.settings_geom_front().and_then(|geom| {
                self.windows
                    .get(&wid)
                    .and_then(|ws| ws.settings())
                    .and_then(|s| crate::settings::wheel_hit(s, &geom, cx, cy))
            });
            match hit {
                Some(crate::settings::WheelHit::Disk { h, s }) => {
                    self.settings_wheel_press_disk(h, s);
                }
                Some(crate::settings::WheelHit::Slider { v }) => {
                    self.settings_wheel_press_slider(v);
                }
                Some(crate::settings::WheelHit::Hex) => self.settings_wheel_focus_hex(),
                Some(crate::settings::WheelHit::Body) => {} // swallowed; stays modal
                None => self.settings_wheel_cancel(),
            }
            return;
        }

        // The open popup menu captures every press.
        let menu_open = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.settings())
            .is_some_and(|s| s.menu.is_some());
        if menu_open {
            let hit = self.settings_geom_front().and_then(|geom| {
                self.windows
                    .get(&wid)
                    .and_then(|ws| ws.settings())
                    .and_then(|s| crate::settings::menu_hit(s, &geom, cx, cy))
            });
            match hit {
                Some(oi) => {
                    // Land the highlight on the pressed option, then commit through the
                    // one shared seam (an already-current option closes with no change).
                    if let Some(s) = self.windows.get_mut(&wid).and_then(|ws| ws.settings_mut())
                        && let Some(m) = s.menu.as_mut()
                    {
                        m.highlighted = oi;
                    }
                    self.settings_menu_commit();
                }
                None => self.settings_menu_cancel(),
            }
            return;
        }

        // Popup click-away handling above intentionally observes outside-card
        // presses. Ordinary landing/sidebar/control activation never does: an
        // exterior band must remain inert instead of clamping onto a card edge.
        let Some((cx, cy)) = card_point else {
            return;
        };

        // The §L landing page captures every press: the send bubble mails the
        // suggestion, the Get-started bubble enters the panel, anything else is
        // swallowed (the hero is modal like the rest of the card). Geometry from
        // the SAME pure `landing_geom` the painter placed the bubbles with.
        let landing = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.settings())
            .is_some_and(|s| s.landing);
        if landing {
            if let Some(geom) = self.settings_geom_front() {
                let lg = crate::settings::landing_geom(&geom);
                let (scx, scy, sr) = lg.send;
                let (bx, by, bw, bh) = lg.btn;
                if (cx - scx).powi(2) + (cy - scy).powi(2) <= sr * sr {
                    self.settings_comment_send();
                } else if cx >= bx && cx < bx + bw && cy >= by && cy < by + bh {
                    self.settings_landing_get_started();
                }
            }
            return;
        }

        // SIDEBAR clicks: the search field focuses search; a category row selects that
        // category (pane → sidebar). Same pure row map the painter placed them with.
        let sidebar = self.windows.get(&wid).and_then(|ws| {
            ws.settings()?;
            let (cw, ch) = self.win_cell_size(wid);
            let pg = crate::settings::pane_geom_cells(ws.cols as usize, ws.settings_panel_rows());
            (cx < pg.sidebar_w_cells * cw as f32).then(|| {
                let row = (cy / ch.max(1) as f32).max(0.0) as usize;
                crate::settings::sidebar_hit(row, ws.settings_panel_rows())
            })
        });
        match sidebar {
            Some(Some(crate::settings::SidebarHit::Search)) => {
                // An in-flight edit is abandoned, like Esc — a pending buffer must
                // never survive the focus move into the search bar (both key paths
                // check `editing` before `searching`, so a surviving buffer would
                // swallow every keystroke the user thinks is typing a query).
                self.settings_edit_cancel();
                self.settings_search_begin();
                return;
            }
            Some(Some(crate::settings::SidebarHit::Category(sec))) => {
                // An in-flight edit is abandoned, like Esc — a pending buffer must
                // never commit against a row of the newly-selected category.
                self.settings_edit_cancel();
                self.settings_set_category(sec);
                return;
            }
            Some(None) => return, // sidebar margin — swallowed, panel stays modal
            None => {}
        }

        let Some(idx) = self.settings_row_at(wid, window_x, window_y) else {
            return; // title / preview / caption / footer — swallowed, nothing to do
        };
        let (selected, editing) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.settings())
            .map_or((false, false), |s| {
                (s.action_target() == Some(idx), s.editing.is_some())
            });
        if !selected {
            // First click only selects; a live edit on the old row is abandoned so the
            // pending buffer can never commit against the newly-selected row.
            if editing {
                self.settings_edit_cancel();
            }
            self.settings_select(idx);
            return;
        }
        // Second click, on the selected row: activate only in the widget region.
        let in_widget = self.settings_geom_front().is_some_and(|geom| {
            self.windows
                .get(&wid)
                .and_then(|ws| ws.settings())
                .and_then(|s| crate::settings::widget_hit_left(s, &geom, idx))
                .is_some_and(|left| cx >= left)
        });
        if in_widget && !editing {
            self.settings_activate();
        }
    }

    /// Mid-drag of the colour wheel's disk/slider (armed by [`Self::settings_click`]):
    /// map the pointer through the SAME [`crate::settings::wheel_geom`] +
    /// [`crate::settings::disk_hs_at`]/[`crate::settings::slider_v_at`] the press
    /// hit-test used and scrub h/s or v (both clamp, so a drag past the rim/track
    /// pins rather than jumps). Returns whether a wheel drag consumed the motion.
    fn settings_wheel_drag_motion(&mut self, wid: WindowId, x: f64, y: f64) -> bool {
        let drag = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.settings())
            .and_then(|s| s.wheel.as_ref())
            .and_then(|w| w.drag);
        let Some(drag) = drag else { return false };
        // Held but unresolvable geometry (mid-resize): still swallow the motion —
        // the overlay is modal while the wheel is up.
        let Some(geom) = self.settings_geom_front() else {
            return true;
        };
        let Some(wg) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.settings())
            .and_then(|s| crate::settings::wheel_geom(s, &geom))
        else {
            return true;
        };
        let (cx, cy) = self.settings_card_point(wid, x, y);
        if let Some(st) = self.windows.get_mut(&wid).and_then(|ws| ws.settings_mut()) {
            match drag {
                crate::settings::WheelDrag::Disk => {
                    let (h, s) = crate::settings::disk_hs_at(&wg, cx, cy);
                    st.wheel_set_hs(h, s);
                }
                crate::settings::WheelDrag::Slider => {
                    st.wheel_set_v(crate::settings::slider_v_at(&wg, cx));
                }
            }
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        true
    }

    /// `CursorMoved` -> remember the cell under the pointer; mid-drag, grow the
    /// text selection to that cell (and, when motion tracking is on, report the
    /// move to the app instead).
    /// Show the "pointer" cursor while Cmd-hovering a link, else the default. Only
    /// touches the OS cursor on a state CHANGE (not every mouse move). Updated on
    /// both pointer motion and Cmd press/release so the affordance tracks the key.
    pub(crate) fn update_hover_cursor(&mut self, wid: WindowId) {
        // While a modal overlay is open the pointer is ITS (the About dialog runs its
        // own link/I-beam cursor): a Cmd press must not resolve a terminal link
        // hidden UNDER the card and flip the cursor out from under the modal.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.overlay.is_some())
        {
            return;
        }
        let mod_held = self
            .windows
            .get(&wid)
            .is_some_and(|ws| link_modifier_held(ws.mods));
        let over_link = mod_held && self.link_under_pointer(wid).is_some();
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if over_link != ws.hover_pointer || ws.native_text_cursor {
            ws.hover_pointer = over_link;
            ws.native_text_cursor = false;
            if let Some(w) = &ws.os_window {
                w.set_cursor(if over_link {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                });
            }
        }
    }

    /// Resolve a native editor source-body hit through the exact compiled node
    /// and geometry used for paint. The byte is `None` only when the pointer is
    /// in an as-yet-unmaterialized visible row; the I-beam still belongs to the
    /// editor in that case.
    fn native_editor_pointer_target(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(crate::tab_model::ViewId, Option<usize>, usize)> {
        let (_, active_view) = self.active_native_view(wid)?;
        let (artifact, x, y) = self.retained_native_leaf_at_pointer(wid, x, y)?;
        if artifact.view != active_view
            || self.native_runtime.view_generation(artifact.view) != Some(artifact.generation)
            || self.native_runtime.app(artifact.instance)?.kind()
                != crate::native_app::AppKind::Editor
        {
            return None;
        }
        let node = artifact.compiled.paint.iter().rev().find(|node| {
            node.rect.contains(x, y)
                && matches!(&node.content, crate::native_ui::UiContent::TextViewport(_))
        })?;
        let crate::native_ui::UiContent::TextViewport(spec) = &node.content else {
            return None;
        };
        let geometry = crate::native_ui::text_viewport_geometry(node.rect);
        if x < geometry.text_x
            || x >= node.rect.right()
            || y < geometry.body_y
            || y >= geometry.body_y + geometry.body_h
        {
            return None;
        }
        let visible_lines = (geometry.body_h / geometry.line_h).ceil().max(1.0) as usize;
        let byte = crate::native_ui::text_viewport_byte_at(spec, node.rect, x, y);
        Some((artifact.view, byte, visible_lines))
    }

    /// Apply one source-addressable pointer position to the active editor view.
    /// Document bytes stay in `DocumentStore`; this mutates only the view-local
    /// primary selection and viewport anchor.
    fn native_editor_pointer_select(
        &mut self,
        wid: WindowId,
        expected_view: crate::tab_model::ViewId,
        byte: usize,
        extend: bool,
        visible_lines: usize,
    ) -> bool {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return false;
        };
        if view != expected_view {
            return false;
        }
        let Some(document) = self.native_runtime.document_id(instance) else {
            return false;
        };
        let Some(snapshot) = self.document_store.snapshot(document) else {
            return false;
        };
        let Some(crate::native_app::AppViewState::Editor(state)) =
            self.native_runtime.view_state_mut(view)
        else {
            return false;
        };
        let Some(buffer) = state.buffer.as_mut() else {
            return false;
        };
        let changed = buffer.pointer_select(&snapshot.text, byte, extend, visible_lines);
        if changed {
            state.common.presentation_revision =
                state.common.presentation_revision.saturating_add(1);
            self.invalidate_native_ui_cache(wid);
            self.request_redraw_all_windows();
        }
        changed
    }

    pub(crate) fn on_cursor_moved(&mut self, wid: WindowId, x: f64, y: f64) {
        // Remember the raw pixel position so a follow-up button press can tell
        // whether it landed in the tab strip (intercepted before cell mapping).
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_cursor_px = (x, y);
        }
        if self.palette_claims_pointer(wid) {
            self.palette_pointer_motion(wid, x, y);
            return;
        }
        // SETTINGS COLOUR-WHEEL DRAG: while the popover's disk or value slider is
        // held, motion scrubs the working colour continuously — the overlay is
        // modal, so the gesture stops here (no hover/selection path below runs).
        if self.settings_wheel_drag_motion(wid, x, y) {
            return;
        }
        // ABOUT MODAL: while the dialog is open, motion belongs to it — grow an
        // in-flight text-selection drag, else track its hover cursor (pointer over
        // the site link, I-beam over selectable text). The modal swallows the motion
        // (no grid hover, terminal selection, or PTY mouse report underneath) — but
        // the cell caches still refresh: the first click after a KEYBOARD close
        // (Esc/Enter, no intervening motion) must not act on the pre-open cell.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.about().is_some())
        {
            self.refresh_mouse_cell(wid, x, y);
            let _ = self.on_about_motion(wid, x, y);
            return;
        }
        // Native tabs own the full content region below host tab chrome. Track
        // hover in the view-local controller so the same typed tree drives the
        // visual wash, pointer cursor, semantics, and later press activation;
        // never leak motion into the parked PTY beneath the app.
        if let Some((_, view)) = self.active_native_view(wid) {
            if self
                .active_visible_leaf_plan(wid)
                .is_some_and(|plan| plan.leaves.len() > 1)
            {
                self.refresh_mouse_cell(wid, x, y);
            }
            let dragging_editor = self
                .native_runtime
                .view_state(view)
                .and_then(|state| state.common().pressed.as_ref())
                .is_some_and(|key| key.as_str() == "editor/buffer");
            let editor_target = self.native_editor_pointer_target(wid, x, y);
            if dragging_editor && let Some((target_view, Some(byte), visible_lines)) = editor_target
            {
                let _ =
                    self.native_editor_pointer_select(wid, target_view, byte, true, visible_lines);
            }
            let native_hover = if self.strip_col_at(wid, x, y).is_some() {
                None
            } else {
                self.retained_native_leaf_at_pointer(wid, x, y).and_then(
                    |(artifact, local_x, local_y)| {
                        let hit = artifact.compiled.hit_test(local_x, local_y)?.clone();
                        let is_text = artifact.compiled.semantic(&hit.key).is_some_and(|node| {
                            node.role == crate::native_ui::SemanticRole::TextField
                        });
                        Some((artifact.view, artifact.generation, hit.key, is_text))
                    },
                )
            };
            let native_hover = native_hover.filter(|(target, generation, _, _)| {
                self.native_runtime.view_generation(*target) == Some(*generation)
            });
            let text_field_hovered = native_hover
                .as_ref()
                .is_some_and(|(_, _, _, is_text)| *is_text);
            // Hover is view-owned even in a native/native split. Clear every
            // formerly hovered sibling that is no longer under the pointer and
            // dirty only the views whose visual state actually changed.
            let visible_native_views = self
                .active_visible_leaf_plan(wid)
                .map(|plan| {
                    plan.leaves
                        .into_iter()
                        .filter_map(|leaf| {
                            matches!(
                                self.view_store.get(leaf.view),
                                Some(crate::tab_model::View::Native(_))
                            )
                            .then_some(leaf.view)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut hover_changed = Vec::new();
            for target in visible_native_views {
                let desired = native_hover
                    .as_ref()
                    .filter(|(hit_view, _, _, _)| *hit_view == target)
                    .map(|(_, _, key, _)| key.clone());
                if let Some(state) = self.native_runtime.view_state_mut(target)
                    && state.common().hovered != desired
                {
                    state.common_mut().hovered = desired;
                    hover_changed.push(target);
                }
            }
            let text_cursor = editor_target.is_some() || text_field_hovered;
            let pointer = !text_field_hovered
                && native_hover
                    .as_ref()
                    .is_some_and(|(_, _, key, _)| key.as_str() != "editor/buffer");
            if let Some(ws) = self.windows.get_mut(&wid)
                && (ws.hover_pointer != pointer || ws.native_text_cursor != text_cursor)
            {
                ws.hover_pointer = pointer;
                ws.native_text_cursor = text_cursor;
                if let Some(window) = &ws.os_window {
                    window.set_cursor(if text_cursor {
                        CursorIcon::Text
                    } else if pointer {
                        CursorIcon::Pointer
                    } else {
                        CursorIcon::Default
                    });
                }
            }
            for target in hover_changed {
                self.invalidate_native_view_cache(
                    wid,
                    target,
                    crate::native_app::DamageRegion::All,
                );
                self.request_redraw_all_windows();
            }
            return;
        }
        // While the pointer is over the tab strip, it is NOT over the terminal grid:
        // show the default cursor and do not report a mouse-move to any pane's app
        // (the strip is GUI chrome). A no-op when the strip is disabled.
        if self.strip_col_at(wid, x, y).is_some() {
            if let Some(ws) = self.windows.get_mut(&wid)
                && (ws.hover_pointer || ws.native_text_cursor)
            {
                ws.hover_pointer = false;
                ws.native_text_cursor = false;
                if let Some(w) = &ws.os_window {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            // A selection drag held up INTO the tab strip is still past the TOP grid edge
            // — the strip occupies exactly the pixel band (`y < pad + strip_px`) that
            // triggers top-edge autoscroll, so without this the gesture would be dead
            // whenever the strip is shown (the Linux/Windows default). No-op unless a
            // selection drag is active; arms `next_autoscroll`, and the repeat tick grows
            // the selection from the last in-grid cell. (macOS strip default is 0 → never
            // reached.)
            self.selection_autoscroll(wid, y);
            return;
        }
        // The window cell is cached by the helper; the seam below consumes pane-local.
        let (_, (lr, lc)) = self.refresh_mouse_cell(wid, x, y);
        // SPLIT-PANE DIVIDER DRAG: while a divider is held, motion resizes the split
        // (relayout + repaint) and short-circuits the selection / mouse-report path —
        // the drag is GUI chrome, not terminal input. A no-op when none is held.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.divider_drag.is_some())
        {
            self.drag_divider(wid);
            return;
        }
        self.update_hover_cursor(wid);
        // Which half of the cell the pointer is in: the right half includes
        // the hovered cell, the left half stops before it. Remembered so a
        // shift-click press (which has no pixel position of its own) can
        // anchor by the half that was pressed. Subtract the `pad` inset first so
        // the half-split lines up with the (padded) cell, matching `pixel_to_cell`.
        let (cw, ch) = self.win_cell_size(wid);
        let cw = cw.max(1);
        let ch = ch.max(1);
        // W1: strip the leading remainder bands first (window→frame), THEN the
        // `pad` inset, so the half-split lines up with the on-glass (banded,
        // padded) cell — matching `pixel_to_cell`.
        let (fx, fy) = self.window_to_frame(wid, x, y);
        let win_pad = self.win_pad(wid);
        let gx = (fx - win_pad as f64).max(0.0) as usize;
        let side = if (gx % cw) * 2 >= cw {
            SelectionSide::Right
        } else {
            SelectionSide::Left
        };
        // Sub-cell pixel offset of the pointer inside its cell, measured from the
        // real winit cursor (band-, pad- and strip-stripped) so a DEC 1016 (SGR-pixel)
        // report carries a GENUINE sub-cell coordinate, not a cell-origin one. The
        // strip occupies the top rows, so subtract its pixel height from `y` before
        // taking the per-cell remainder (matches `pixel_to_term_cell`). Ignored by
        // every cell-coordinate encoding — see [`crate::input::PixelOffset`].
        let strip_px = self.tab_strip_rows as usize * ch;
        // The chrome headroom sits above the pad on the y-axis (x carries none),
        // matching `pixel_to_term_cell`'s `pad_top + head` inset.
        let gy = (fy - (self.win_pad_top(wid) + self.win_head(wid)) as f64).max(0.0) as usize;
        let gy = gy.saturating_sub(strip_px);
        let px_off = crate::input::PixelOffset {
            x: (gx % cw) as u16,
            y: (gy % ch) as u16,
        };
        // Phase 0.5: the cell-half (`side`) is GUI-derived (it needs the pixel x),
        // then handed to the seam as DATA. The seam runs the `self.selecting` local
        // drag and the tracking-ON motion report under ONE mode read. `buttons == 3`
        // is the no-button hover code (kills c: a controller drag arrives as
        // `MouseMove { buttons != 3 }` in a batch). The seam also updates
        // last_mouse_cell/last_mouse_side, so both sources keep that state in sync.
        let mods = self.mouse_modifiers(wid);
        // The X10 button code of the held button (Left=0/Middle=1/Right=2), or `3`
        // (no button held) for a true hover. `encode_mouse_motion` ORs in the 32
        // motion bit, so a drag in 1002/1003 reports the held button correctly and
        // a button-less hover still reports 3 (which 1002 drops, as it should).
        let buttons = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.held_mouse_button)
            .map_or(3u8, |b| b.code());
        // Remember the sub-cell offset so a follow-up button press / wheel notch
        // (winit delivers no pixel position on those) reports the same pixel the
        // pointer last hovered, under DEC 1016.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_mouse_px_off = px_off;
        }
        // SELECTION AUTOSCROLL: a drag dragged past the top/bottom grid edge scrolls
        // the scrollback so the selection extends into off-screen content. Done
        // BEFORE the MouseMove drag below so `drag_selection` (which maps the row
        // through the now-updated `display_offset`) grows the selection to the
        // freshly-revealed edge row. A no-op when no selection drag is active or the
        // pointer is inside the grid. `row`/`col` are already clamped to the grid by
        // `pixel_to_cell`, so the edge row is 0 (top) or rows-1 (bottom).
        self.selection_autoscroll(wid, y);
        self.input(
            wid,
            InputEvent::MouseMove {
                buttons,
                // PANE-LOCAL cells, CLAMPED to the focused pane's grid: the seam writes
                // these into `last_mouse_cell`, drives `drag_selection`, AND reports them
                // to the PTY — all of which must be in the FOCUSED pane's grid, not window
                // coordinates, or a non-top-left split selects/reports the wrong cells; the
                // clamp keeps a divider-crossing drag inside the pane. (ro,co)==(0,0) for a
                // single pane / top-left pane → byte-identical to before.
                row: lr,
                col: lc,
                mods,
                side,
                px_off,
            },
            Source::Human,
        );
    }

    /// Mid-drag: grow the selection to the hovered viewport cell — by cell for
    /// simple/block drags, by whole words/lines when the drag began as a
    /// double/triple click (the gesture origin stays fully selected whichever
    /// direction the drag goes).
    pub(crate) fn drag_selection(&mut self, wid: WindowId, row: u16, col: u16) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(fws) = self.windows.get_mut(&wid) else {
            return;
        };
        let sel_row = {
            let mut term = term_lock(&term);
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            match fws.gesture {
                None => {
                    term.text_selection_mut()
                        .update_selection(sel_row, col, fws.last_mouse_side);
                }
                // Triple-click drag: whole rows from the origin line to the
                // hovered line. Rebuilt from the origin each move so the
                // anchor sides stay inclusive in either drag direction.
                Some(g) if g.kind == SelectionType::Lines => {
                    let max_col = term.cols().saturating_sub(1);
                    let sel = term.text_selection_mut();
                    if sel_row < g.row {
                        sel.start_selection(
                            g.row,
                            max_col,
                            SelectionSide::Right,
                            SelectionType::Lines,
                        );
                        sel.update_selection(sel_row, 0, SelectionSide::Left);
                    } else {
                        sel.start_selection(g.row, 0, SelectionSide::Left, SelectionType::Lines);
                        sel.update_selection(sel_row, max_col, SelectionSide::Right);
                    }
                }
                // Double-click drag: snap the moving end to the hovered word
                // (or bare cell on whitespace); the origin word stays fully
                // selected by anchoring at its far boundary.
                Some(g) => {
                    let (ws, we) = control::word_cols(&term, sel_row, col).unwrap_or((col, col));
                    let sel = term.text_selection_mut();
                    if (sel_row, col) < (g.row, g.start_col) {
                        sel.start_selection(
                            g.row,
                            g.end_col,
                            SelectionSide::Right,
                            SelectionType::Semantic,
                        );
                        sel.update_selection(sel_row, ws, SelectionSide::Left);
                    } else {
                        sel.start_selection(
                            g.row,
                            g.start_col,
                            SelectionSide::Left,
                            SelectionType::Semantic,
                        );
                        sel.update_selection(sel_row, we, SelectionSide::Right);
                    }
                }
            }
            sel_row
        };
        if (sel_row, col) != fws.sel_press_cell {
            fws.sel_dragged = true;
        }
        if let Some(w) = &fws.os_window {
            w.request_redraw();
        }
    }

    /// While a left-drag selection is in flight, AUTOSCROLL the scrollback when the
    /// pointer is dragged PAST the top/bottom viewport edge, so the selection can
    /// extend into content that is currently off-screen (the standard text-editor
    /// "drag to the edge to keep selecting" gesture). Returns `true` iff the viewport
    /// actually moved (the caller then re-grows the selection to the freshly-revealed
    /// edge row and repaints).
    ///
    /// A NO-OP unless a selection drag is active (`selecting`) — a plain hover past
    /// the edge never scrolls. The line count + direction come from the pure
    /// [`crate::app_render::selection_autoscroll_lines`] (so the edge math is
    /// unit-testable); `scroll_display` clamps at the history ends, so dragging past
    /// the oldest/newest line is harmless.
    pub(crate) fn selection_autoscroll(&mut self, wid: WindowId, y: f64) -> bool {
        let (selecting, rows) = match self.windows.get(&wid) {
            Some(ws) => (ws.selecting, ws.rows),
            None => return false,
        };
        // Not dragging a selection → disarm the repeat timer and bail.
        if !selecting {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.next_autoscroll = None;
            }
            return false;
        }
        let ch = self.win_cell_size(wid).1.max(1);
        // The chrome headroom stacks above the pad on the y-axis, so the grid's
        // top edge is `head + pad_top + strip_px` — fold it into the pad inset the
        // pure edge math subtracts (head == 0 && pad_top == pad is byte-identical).
        let pad = self.win_pad_top(wid) + self.win_head(wid);
        let strip_px = self.tab_strip_rows as usize * ch;
        // W1: window→frame first, so the grid's top/bottom edges account for the
        // leading remainder band like every other pointer consumer.
        let (_, y) = self.window_to_frame(wid, 0.0, y);
        let lines = crate::app_render::selection_autoscroll_lines(y, pad, strip_px, ch, rows);
        if lines == 0 {
            // Pointer is back inside the grid → stop auto-scrolling.
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.next_autoscroll = None;
            }
            return false;
        }
        let term = match self.front_terminal(wid) {
            Some(terminal) => terminal.term.clone(),
            None => return false,
        };
        let moved = {
            let mut term = term_lock(&term);
            let before = term.grid().display_offset();
            term.scroll_display(lines);
            term.grid().display_offset() != before
        };
        // Held past an edge: arm the repeat so the scroll continues even with the
        // pointer still (`about_to_wait` folds this deadline, `new_events` re-fires it).
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.next_autoscroll = Some(Instant::now() + crate::SELECTION_AUTOSCROLL_INTERVAL);
        }
        if moved && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
            w.request_redraw();
        }
        moved
    }

    /// Repeat-timer tick (`new_events`): while a selection drag is held past a grid
    /// edge, scroll one step and grow the selection to the freshly-revealed edge cell —
    /// even though the pointer has not moved. Re-uses the held pointer's last pixel-y
    /// and cell; `selection_autoscroll` re-arms `next_autoscroll` while still past the
    /// edge and disarms it once the pointer re-enters the grid.
    pub(crate) fn tick_selection_autoscroll(&mut self, wid: WindowId) {
        let Some((last_y, (lr, lc))) = self
            .windows
            .get(&wid)
            .map(|ws| (ws.last_cursor_px.1, ws.last_mouse_cell))
        else {
            return;
        };
        if self.selection_autoscroll(wid, last_y) {
            // Map the held pointer cell through the NEW display_offset so the selection
            // grows to the row just scrolled into view (same as the CursorMoved path).
            self.drag_selection(wid, lr, lc);
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) {
                w.request_redraw();
            }
        }
    }

    /// Left press with mouse tracking OFF — the selection-gesture dispatcher.
    ///
    /// Shift with an existing selection extends it to the pressed cell;
    /// otherwise the multi-click count picks the gesture: 1 starts a simple
    /// drag (rectangular block with alt/option held), 2 selects the word under
    /// the press, 3 selects the whole line. Word/line selections stay
    /// draggable until release (extending by whole words/lines).
    /// Shift-click: extend the existing selection (GUI affordance) and reset the
    /// multi-click streak (this press is not part of a double-click). The actual
    /// selection mutation reuses [`Self::extend_selection_to`]. Stays in the human
    /// handler — it is keyed on `self.mods`, which a controller never sets (the
    /// controller analogue is the `select extend` verb).
    pub(crate) fn shift_extend_press(&mut self, wid: WindowId) {
        let Some((row, col)) = self.windows.get(&wid).map(|ws| ws.last_mouse_cell) else {
            return;
        };
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let sel_row = i32::from(row) - term_lock(&term).grid().display_offset() as i32;
        let now = Instant::now();
        self.extend_selection_to(wid, sel_row, col);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_press = Some((now, (sel_row, col)));
            ws.click_count = 1;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
    }

    /// Advance the MULTI_CLICK_MS streak FSM and RETURN the resulting click_count
    /// (1 = single, 2 = double, 3 = triple; a fourth rapid click wraps to 1). The
    /// human handler owns this streak state (`last_press`/`click_count`); a
    /// controller passes an authoritative count without mutating it (A.2.2). The
    /// gesture DISPATCH on the returned count now lives in the seam
    /// (`seam_left_press`), shared by both sources.
    pub(crate) fn advance_click_streak(&mut self, wid: WindowId) -> u8 {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return 1;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return 1;
        };
        let (row, col) = ws.last_mouse_cell;
        let sel_row = i32::from(row) - term_lock(&term).grid().display_offset() as i32;
        let now = Instant::now();
        ws.click_count = match ws.last_press {
            Some((t, cell))
                if cell == (sel_row, col)
                    && now.duration_since(t).as_millis() <= MULTI_CLICK_MS =>
            {
                ws.click_count % 3 + 1
            }
            _ => 1,
        };
        ws.last_press = Some((now, (sel_row, col)));
        ws.click_count
    }

    /// Shift-click: extend an EXISTING non-empty selection so the pressed cell
    /// becomes its new endpoint (side by cell half), then complete it again.
    /// Returns false (no-op) when there is nothing to extend.
    pub(crate) fn extend_selection_to(&mut self, wid: WindowId, sel_row: i32, col: u16) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(terminal) = ws.front_terminal() else {
            return false;
        };
        let mut term = term_lock(&terminal.term);
        let sel = term.text_selection_mut();
        if !sel.has_selection() || sel.is_empty() {
            return false;
        }
        sel.extend_selection(sel_row, col, ws.last_mouse_side);
        sel.complete_selection();
        true
    }

    /// Double-click: word-select the pressed cell (builtin smart rules — URLs,
    /// paths, words; just the cell on whitespace), completed immediately, and
    /// arm the gesture so a drag before release extends by whole words.
    pub(crate) fn select_word_click(&mut self, wid: WindowId, sel_row: i32, col: u16) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let (start_col, end_col) = {
            let mut term = term_lock(&term);
            control::select_word(&mut term, sel_row, col)
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.gesture = Some(GestureOrigin {
                row: sel_row,
                start_col,
                end_col,
                kind: SelectionType::Semantic,
            });
        }
        self.arm_gesture_drag(wid, sel_row, col);
    }

    /// Triple-click: select the full line under the press, completed
    /// immediately, and arm the gesture so a drag extends by whole lines.
    pub(crate) fn select_line_click(&mut self, wid: WindowId, sel_row: i32, col: u16) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let end_col = {
            let mut term = term_lock(&term);
            control::select_line(&mut term, sel_row);
            term.cols().saturating_sub(1)
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.gesture = Some(GestureOrigin {
                row: sel_row,
                start_col: 0,
                end_col,
                kind: SelectionType::Lines,
            });
        }
        self.arm_gesture_drag(wid, sel_row, col);
    }

    /// Keep a completed double/triple-click selection draggable while the
    /// button stays down: `sel_dragged` is pre-set so the release completes
    /// the selection instead of treating it as a deselecting plain click.
    pub(crate) fn arm_gesture_drag(&mut self, wid: WindowId, sel_row: i32, col: u16) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        ws.selecting = true;
        ws.sel_dragged = true;
        ws.sel_press_cell = (sel_row, col);
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Single press with mouse tracking OFF: start a text selection of `kind`
    /// (`Simple`, or `Block` for alt-drag) at the cell under the pointer,
    /// mapped to live-screen selection coords (viewport row minus
    /// `display_offset`, so a scrolled-back press lands in scrollback).
    pub(crate) fn begin_selection(&mut self, wid: WindowId, kind: SelectionType) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let (row, col) = ws.last_mouse_cell;
        let sel_row = {
            let mut term = term_lock(&term);
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            term.text_selection_mut()
                .start_selection(sel_row, col, SelectionSide::Left, kind);
            sel_row
        };
        ws.selecting = true;
        ws.sel_dragged = false;
        ws.sel_press_cell = (sel_row, col);
        ws.gesture = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Left release ending a drag: complete the selection — unless the pointer
    /// never left the press cell, in which case a plain click deselects.
    ///
    /// COPY-ON-SELECT: when `copy_on_select` is enabled (config, default off) and
    /// the release actually COMPLETED a selection (a real drag, not a deselecting
    /// click), the selected text is copied to the system clipboard right here — no
    /// explicit Cmd-C needed. The highlight is left intact (`copy_selection` does
    /// not clear it), so Cmd-C still works on the same selection afterwards.
    ///
    /// Returns whether the copy-on-select path FIRED (an opted-in completed drag) —
    /// the auto-copy trigger, independent of whether `pbcopy` itself succeeded — so
    /// the firing CONDITION is unit-testable without touching the system clipboard.
    ///
    /// `suppress_copy_on_select` is the EXFIL FENCE: when a scoped-edge (`non-Owner`)
    /// `mouse` verb injected the gesture, the control layer stamps this `true` on the
    /// release event so BOTH the system-clipboard `pbcopy` AND the X11 PRIMARY
    /// auto-own are skipped — a `WriteInput` edge may select (viewport nav) but must
    /// not exfiltrate on-screen text. A human gesture and an Owner-scoped controller
    /// gesture pass `false`, so their copy-on-select / PRIMARY behaviour is unchanged.
    /// The selection ITSELF is still completed regardless (it is not exfil).
    pub(crate) fn finish_selection(
        &mut self,
        wid: WindowId,
        suppress_copy_on_select: bool,
    ) -> bool {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return false;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let completed = ws.sel_dragged;
        {
            let mut term = term_lock(&term);
            let sel = term.text_selection_mut();
            if completed {
                sel.complete_selection();
            } else {
                sel.clear();
            }
        }
        ws.selecting = false;
        ws.gesture = None;
        // Button released: stop any selection autoscroll repeat immediately (→ pure
        // `Wait`) rather than waiting for the next tick to notice `!selecting`.
        ws.next_autoscroll = None;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        // A completed drag-select auto-copies when the user opted in. Done AFTER
        // the borrow above ends (it re-locks the term to stringify the selection)
        // and only for a real selection — a plain click that cleared never copies.
        // A scoped-edge gesture (`suppress_copy_on_select`) NEVER auto-copies: the
        // selection is made but the clipboard side-effect (the exfil) is fenced.
        let fired = completed && self.copy_on_select && !suppress_copy_on_select;
        if fired {
            self.copy_selection();
        }
        // X11 convention: a completed selection ALWAYS owns the PRIMARY selection
        // (independent of copy-on-select), so middle-click-paste works in other apps
        // without clobbering the explicit-copy CLIPBOARD. Compiled only where PRIMARY
        // exists (mirrors `primary_set`'s own cfg): elsewhere the lock-held
        // `selection_to_string` extraction would only feed a no-op, and a large
        // scrollback selection makes that a multi-ms stall on every release.
        // PRIMARY is ALSO a clipboard exfil channel, so the scoped-edge fence
        // (`suppress_copy_on_select`) skips it too — closing the same tunnel here.
        #[cfg(target_os = "linux")]
        if completed
            && !suppress_copy_on_select
            && let Some(text) = term_lock(&term).selection_to_string()
            && !text.is_empty()
        {
            crate::control::primary_set(&text);
        }
        fired
    }

    /// The URL under the pointer, if any: an (authorized) OSC 8 hyperlink on the
    /// cell wins; else a plain-text `http(s)://` URL detected in the row. Used by
    /// Cmd-click (open) and Cmd-hover (pointer cursor).
    pub(crate) fn link_under_pointer(&self, wid: WindowId) -> Option<String> {
        let ws = self.windows.get(&wid)?;
        let (row, col) = ws.last_mouse_cell;
        let term = term_lock(&ws.front_terminal()?.term);
        term.hyperlink_at(row, col).map(str::to_owned).or_else(|| {
            plain_url_at(&term.render_row(row as usize), col as usize).map(|(u, _, _)| u)
        })
    }

    /// Cmd-click: if there is a link under the pointer with a safe scheme, open it
    /// via the OS and report `true`. The `is_safe_url` allowlist is the security
    /// boundary — a hostile program's link can never make `open` launch an app or
    /// touch the filesystem (covers both OSC 8 and auto-detected plain-text URLs).
    pub(crate) fn open_link_under_pointer(&self, wid: WindowId) -> bool {
        let Some(url) = self.link_under_pointer(wid) else {
            return false;
        };
        if !is_safe_url(&url) {
            return false;
        }
        open_url_external(&url);
        true
    }

    /// `MouseInput` -> when no app is tracking the mouse, left presses run the
    /// selection gestures (drag select, double-click word, triple-click line,
    /// shift-click extend, alt-drag block; a plain left click deselects); when
    /// tracking is on, encode the press/release for the cell under the pointer
    /// and write it to the PTY.
    pub(crate) fn on_mouse_input(
        &mut self,
        wid: WindowId,
        state: ElementState,
        button: WinitMouseButton,
    ) {
        // GUI-ONLY prefix (gesture-state owner = App; a controller can't trigger
        // these): Cmd-click link-open, shift-extend, and the MULTI_CLICK_MS streak
        // FSM that yields the authoritative `click_count`. These stay in the
        // handler; the seam consumes `click_count`/`side` as DATA.
        let pressed = state == ElementState::Pressed;
        if self.palette_claims_pointer(wid) {
            let (x, y) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            if button == WinitMouseButton::Left {
                if pressed {
                    self.palette_pointer_press(wid, x, y);
                } else {
                    self.palette_pointer_release(wid, x, y);
                }
            }
            if !pressed {
                self.settle_pointer_drags(wid);
            }
            return;
        }
        let Some(mods_state) = self.windows.get(&wid).map(|ws| ws.mods) else {
            return;
        };
        // Mixed tabs use the canonical tree before either content-specific
        // boundary. A divider consumes the press; otherwise a press in a sibling
        // focuses that stable view first, so the matching keyboard/pointer router
        // can never target the formerly focused leaf.
        if pressed
            && button == WinitMouseButton::Left
            && self.active_tab_has_native(wid)
            && self
                .windows
                .get(&wid)
                .is_none_or(|window| window.tab_set.active().is_none_or(|tab| tab.root.len() > 1))
        {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            if self.strip_col_at(wid, px, py).is_none()
                && (self.begin_divider_drag(wid) || self.focus_pane_under_pointer(wid))
            {
                return;
            }
        }
        // NATIVE TAB POINTER BOUNDARY: canonical tab chrome remains host-owned,
        // while every point below it is hit-tested against the exact semantic
        // tree used for paint and control inspection. Even a miss is swallowed
        // so a click can never reach the parked PTY underneath the app.
        if self.active_native_view(wid).is_some() {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if !pressed && button == WinitMouseButton::Left {
                let Some((_, view)) = self.active_native_view(wid) else {
                    return;
                };
                let armed = self
                    .native_runtime
                    .view_state(view)
                    .and_then(|state| state.common().pressed.clone());
                let release = self.retained_native_leaf_at_pointer(wid, px, py).and_then(
                    |(artifact, x, y)| {
                        if artifact.view != view
                            || self.native_runtime.view_generation(artifact.view)
                                != Some(artifact.generation)
                        {
                            return None;
                        }
                        let hit = artifact.compiled.hit_test(x, y)?.clone();
                        let value = artifact
                            .compiled
                            .text_field_byte_at(&hit.key, x)
                            .map(|byte| crate::native_app::SemanticInput::TextPosition {
                                byte,
                                extend: true,
                            })
                            .or_else(|| {
                                artifact
                                    .compiled
                                    .slider_value_at(&hit.key, x)
                                    .map(crate::native_app::SemanticInput::Number)
                            });
                        Some((artifact.view, artifact.generation, hit, value))
                    },
                );
                if let Some(state) = self.native_runtime.view_state_mut(view) {
                    state.common_mut().pressed = None;
                }
                self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
                self.request_redraw_all_windows();
                if let (Some(armed), Some((release_view, generation, release, value))) =
                    (armed, release)
                    && armed == release.key
                    && self
                        .active_native_view(wid)
                        .is_some_and(|(_, live)| live == release_view)
                    && self.native_runtime.view_generation(release_view) == Some(generation)
                {
                    let _ = self.dispatch_native_event(
                        wid,
                        crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                            id: release.action,
                            value,
                        }),
                    );
                }
                return;
            }
            if pressed && button == WinitMouseButton::Left {
                if let Some(col) = self.strip_col_at(wid, px, py) {
                    self.handle_tab_strip_click(wid, col);
                    return;
                }
                let editor_target = self.native_editor_pointer_target(wid, px, py);
                let press = self.retained_native_leaf_at_pointer(wid, px, py).and_then(
                    |(artifact, x, y)| {
                        let (_, active_view) = self.active_native_view(wid)?;
                        if artifact.view != active_view
                            || self.native_runtime.view_generation(artifact.view)
                                != Some(artifact.generation)
                        {
                            return None;
                        }
                        let hit = artifact.compiled.hit_test(x, y)?;
                        Some((
                            artifact.view,
                            artifact.generation,
                            hit.key.clone(),
                            hit.action.clone(),
                            artifact.compiled.text_field_byte_at(&hit.key, x),
                        ))
                    },
                );
                if let Some((target_view, generation, key, action, text_position)) = press {
                    if self.native_runtime.view_generation(target_view) == Some(generation)
                        && let Some(state) = self.native_runtime.view_state_mut(target_view)
                    {
                        let common = state.common_mut();
                        common.hovered = Some(key.clone());
                        common.pressed = Some(key.clone());
                        common.last_focus = Some(key.clone());
                        common.focus_visible = false;
                    }
                    self.invalidate_native_view_cache(
                        wid,
                        target_view,
                        crate::native_app::DamageRegion::All,
                    );
                    self.request_redraw_all_windows();
                    if let Some(byte) = text_position
                        && self.native_runtime.view_generation(target_view) == Some(generation)
                    {
                        let _ = self.dispatch_native_view_event(
                            wid,
                            target_view,
                            crate::native_app::AppEvent::Action(
                                crate::native_app::ActionInvocation {
                                    id: action,
                                    value: Some(crate::native_app::SemanticInput::TextPosition {
                                        byte,
                                        extend: mods_state.shift_key(),
                                    }),
                                },
                            ),
                        );
                    }
                    if let Some((target_view, Some(byte), visible_lines)) = editor_target {
                        let _ = self.native_editor_pointer_select(
                            wid,
                            target_view,
                            byte,
                            mods_state.shift_key(),
                            visible_lines,
                        );
                    }
                } else if let Some((_, view)) = self.active_native_view(wid)
                    && let Some(state) = self.native_runtime.view_state_mut(view)
                {
                    state.common_mut().pressed = None;
                    self.invalidate_native_view_cache(
                        wid,
                        view,
                        crate::native_app::DamageRegion::All,
                    );
                }
            }
            return;
        }
        // SETTINGS MODAL: while the overlay is open, a left press drives the panel
        // ([`Self::settings_click`]: select on first click, activate on the selected
        // row's widget region, commit/dismiss the open popup menu); EVERY mouse gesture
        // is then swallowed (no tab-strip switch, no divider drag, no pane focus, no
        // selection, no PTY mouse report) so the panel is truly modal. Checked first,
        // before any other mouse layer.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.settings().is_some())
        {
            if button == WinitMouseButton::Left {
                if pressed {
                    let (px, py) = self
                        .windows
                        .get(&wid)
                        .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
                    self.settings_click(wid, px, py);
                } else {
                    // Release ends an in-flight colour-wheel scrub (the working
                    // colour keeps its last dragged value; ↵ commits, Esc discards).
                    self.settings_wheel_drag_end();
                    // A swallowed left RELEASE still settles an in-flight drag, or a
                    // divider/selection drag begun before the panel opened would keep
                    // dragging on every pointer motion under (and after) the modal.
                    self.settle_pointer_drags(wid);
                }
            }
            return;
        }
        // ABOUT MODAL: the native-window-styled About dialog. A left press on the
        // title-bar close dot / OK closes it, on the byline's site link opens the
        // browser, and anywhere else on the card anchors a TEXT-SELECTION drag (the
        // release settles it — `App::on_about_press`/`on_about_release`). Every
        // mouse gesture is swallowed while it is open — exactly like Settings above —
        // so the dialog is truly modal. A swallowed left RELEASE still settles any
        // in-flight TERMINAL drag (belt to `about_enter`'s braces): a divider/selection
        // drag whose release the modal ate must not keep dragging on pointer motion.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.about().is_some())
        {
            if button == WinitMouseButton::Left {
                if pressed {
                    self.on_about_press(wid);
                } else {
                    self.on_about_release(wid);
                    self.settle_pointer_drags(wid);
                }
            }
            return;
        }
        // SOFTWARE UPDATE MODAL: the own-rendered update dialog. A left press on the close
        // dot / Close closes it, on Check runs a fresh check, on Install & Relaunch applies
        // the staged build. Every gesture is swallowed while open (truly modal), and a
        // swallowed left RELEASE still settles any in-flight drag (as with About above).
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.update_screen().is_some())
        {
            if button == WinitMouseButton::Left {
                if pressed {
                    let (px, py) = self
                        .windows
                        .get(&wid)
                        .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
                    if let Some(hit) = self.update_hit_at(wid, px, py) {
                        self.update_screen_click(wid, hit);
                    }
                } else {
                    self.settle_pointer_drags(wid);
                }
            }
            return;
        }
        // The update nudge is the version-menu ⬆️ (macOS) / the LEADING `↻` icon in the
        // tab strip (off macOS — handled by the tab-strip click path below via
        // `TabHit::Update`), not a floating overlay, so there is no separate mouse gate
        // here.
        // TRANSIENT "Update ready" NOTICE: the fading top-centre pill is clickable — a
        // left press on it APPLIES the update in one gesture (details-overlay fallback
        // when nothing is actually staged) + dismisses the pill; see
        // `App::apply_update_or_details`. Checked here (no modal is open, else the pill
        // is hidden under it) BEFORE the mouse-report path so the click doesn't also
        // reach the program running in the terminal.
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self.notice_click(wid, px, py) {
                return;
            }
        }
        // MIDDLE-CLICK PASTE (X11 PRIMARY): when no app is tracking the mouse, a
        // middle press pastes the PRIMARY selection (the X convention) through the
        // same seam as Ctrl+Shift+V; when tracking is ON it falls through to the
        // mouse-report encoding below so TUIs still receive the button. X11 only.
        #[cfg(target_os = "linux")]
        if pressed && button == WinitMouseButton::Middle {
            let tracking = self
                .front_terminal(wid)
                .map(|terminal| terminal.term.clone())
                .is_some_and(|t| term_lock(&t).mouse_tracking_enabled());
            if !tracking {
                if let Some(text) = crate::control::primary_get() {
                    self.input(wid, InputEvent::Paste(text), Source::Human);
                }
                return;
            }
        }
        // TAB STRIP: a left press in the strip region (top `tab_strip_rows` rows)
        // switches / closes / opens a tab and stops there — it never reaches the
        // terminal selection / pane-focus path. A no-op when the strip is disabled
        // or the press is in the terminal region.
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if let Some(col) = self.strip_col_at(wid, px, py) {
                self.handle_tab_strip_click(wid, col);
                return;
            }
        }
        // FIND-BAR TOGGLES: a left press on the find bar's `Aa` / `.*` indicators flips
        // match-case / regex (same as ^S/^R) and stops there. Runs AFTER the strip block
        // (so a strip click can't reach it) and is a no-op off the indicators or when not
        // searching, so ordinary terminal / selection clicks are unaffected. The
        // pixel-band gate ([`Self::find_bar_pixel_hit`]) rejects clicks the clamping
        // `pixel_to_cell` would otherwise snap onto the bar row from the surrounding pad.
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if self.find_bar_pixel_hit(wid, px, py) {
                let (row, col) = self.pixel_to_cell(wid, px, py);
                if self.find_bar_click(wid, row, col) {
                    return;
                }
            }
        }
        // SPLIT-PANE DIVIDER DRAG: a left press ON a divider grabs it to resize the
        // split (and stops there — no focus change, no selection). Release ends the
        // drag. Checked BEFORE pane-focus so a press on the gap between panes resizes
        // rather than mis-focusing. A no-op on the single-pane path.
        if button == WinitMouseButton::Left {
            if pressed {
                if self.begin_divider_drag(wid) {
                    return;
                }
            } else if self.finish_divider_drag(wid) {
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.held_mouse_button = None;
                }
                return;
            }
        }
        // SPLIT PANES: a left press in a DIFFERENT pane focuses it (and stops there
        // — it does not also start a selection in the old pane). A no-op on the
        // single-pane path (the hit-test always returns the only/focused pane).
        if pressed && button == WinitMouseButton::Left && self.focus_pane_under_pointer(wid) {
            return;
        }
        let mut click_count: u8 = 1;
        if button == WinitMouseButton::Left {
            let Some(term) = self
                .front_terminal(wid)
                .map(|terminal| terminal.term.clone())
            else {
                return;
            };
            let tracking = term_lock(&term).mouse_tracking_enabled();
            let option_held = mods_state.alt_key();
            // A left press starts a LOCAL selection when nothing tracks the mouse OR
            // (audit M6) Option is held to bypass a tracking app — iTerm2's gesture, so
            // a PID can be copied out of htop. The GUI-only prefix (link-open,
            // shift-extend, the click-streak FSM) runs for BOTH cases.
            if pressed && press_starts_selection(tracking, option_held) {
                // Cmd-click (macOS) / Ctrl-click (Linux) an OSC 8 hyperlink opens it
                // (safe schemes only) instead of starting a selection. GUI-only —
                // never reaches the seam.
                if link_modifier_held(mods_state) && self.open_link_under_pointer(wid) {
                    return;
                }
                // Shift-click extends an existing selection (GUI affordance keyed on
                // self.mods); it returns here without reaching the seam, like today.
                if mods_state.shift_key() {
                    self.shift_extend_press(wid);
                    return;
                }
                // Advance the streak and capture the count for the seam's gesture.
                click_count = self.advance_click_streak(wid);
                // M6 BYPASS: tracking is ON, so the seam would REPORT this press to the
                // app. Run the local selection gesture HERE and return without reporting
                // — press/release stay symmetric for the app. Option is spent on the
                // bypass, so the kind is Simple (`press_selection_kind`), never the
                // local Option+drag Block.
                if tracking {
                    let (row, col) = self
                        .windows
                        .get(&wid)
                        .map_or((0, 0), |ws| ws.last_mouse_cell);
                    let block = press_selection_kind(option_held, tracking) == SelectionType::Block;
                    self.seam_left_press(wid, row, col, click_count, block);
                    return;
                }
                // tracking-OFF: fall through to the seam exactly as today (it returns
                // Egress::TrackingOff and runs the same seam_left_press on the press).
            }
            // M6: the matching RELEASE of a bypass selection settles it LOCALLY and does
            // NOT leak a release report. The tracking-OFF release is still settled via
            // the seam's TrackingOff branch; this also catches a selection the app began
            // tracking AFTER a tracking-off press started it (mid-drag mode flip).
            // The `tracking` requirement is intentionally DROPPED: an Option-bypass (M6)
            // press starts a LOCAL selection WITHOUT setting the Left reported bit, so if
            // tracking flips ON->OFF mid-drag (the app exits, or issues CSI ?1000l/?1002l/
            // ?1003l) the release would otherwise be double-gated out — the old `tracking`
            // check skipped it here, and the generic `!was_reported` guard below returns
            // before the seam's TrackingOff branch can settle it — leaving `selecting`
            // stuck so every bare hover kept growing the selection. Settling unconditionally
            // is symmetric with the M6 press and stays correct for the normal tracking-OFF
            // release (which previously settled via the seam's TrackingOff branch with the
            // identical outcome; a tracking-OFF release never reports).
            if !pressed && self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
                // Direct winit release path = a REAL human gesture, so copy-on-select
                // is not suppressed (false); the scoped-edge fence only applies to
                // control-verb-injected gestures via the `App::input` seam.
                self.finish_selection(wid, false);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.held_mouse_button = None;
                    // Also clear the LEFT (selection) button's reported bit — this branch
                    // returns before the generic press/release bookkeeping below, so the
                    // bit set by the tracking-OFF press would otherwise stay stuck and a
                    // later non-reporting press could leak an orphan release (`1` = Left).
                    ws.reported_buttons &= !1u8;
                }
                return;
            }
        }
        let Some(button) = winit_mouse_button(button) else {
            return;
        };
        // Track the held button so a subsequent motion report (tracking ON) carries
        // it instead of the hover code. Set on press, cleared on release; harmless
        // when tracking is OFF (the motion then takes the local selection path and
        // never reads this).
        // Per-button "press was reported" bit, so a release pairs with its OWN press
        // (a two-button chord under mouse tracking keeps both releases). `&7` keeps the
        // shift in range; terminal mouse buttons are codes 0..=2.
        let bit = 1u8 << (button.code() & 7);
        let was_reported = self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.reported_buttons & bit != 0);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.held_mouse_button = if pressed { Some(button) } else { None };
            if pressed {
                ws.reported_buttons |= bit;
            } else {
                ws.reported_buttons &= !bit;
            }
        }
        // A RELEASE whose PRESS never produced a report must not emit an orphan release
        // to a mouse-tracking app — press/release must stay paired. The split-pane
        // focusing press (above) returns early WITHOUT setting the bit or reporting, so
        // its release would otherwise leak a lone button-up. Skip a release whose press
        // bit was never set; a genuine (or chorded) press always reports its release.
        if !pressed && !was_reported {
            return;
        }
        let (row, col) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| ws.last_mouse_cell);
        let mods = self.mouse_modifiers(wid);
        let side = self
            .windows
            .get(&wid)
            .map_or(SelectionSide::Left, |ws| ws.last_mouse_side);
        // The sub-cell pixel offset of the last pointer move (a press carries no
        // pixel of its own) so a DEC 1016 press/release lands on the genuine pixel.
        let px_off = self
            .windows
            .get(&wid)
            .map_or(crate::input::PixelOffset::CELL_ORIGIN, |ws| {
                ws.last_mouse_px_off
            });
        // Snapshot the block-select intent (held Alt/Option) HERE, at build time,
        // into event DATA — so the seam's selection-type decision is source-blind
        // (it reads `block`, never `self.mods`). A controller sends `block=1` for
        // the same effect; a human's later-released Alt can't retroactively change
        // this press's type.
        let block = mods_state.alt_key();
        // Phase 0.5: the seam reads mouse_tracking_enabled() ONCE under one lock
        // (closing the old two-lock window) and either emits the press/release
        // report (tracking ON, real `mods` — kills a) or runs the local selection
        // gesture (tracking OFF), dispatching on `click_count` (kills b) at `side`
        // (kills i) with type from `block`. Both sources share that machinery.
        self.input(
            wid,
            InputEvent::MouseButton {
                button,
                pressed,
                row,
                col,
                mods,
                click_count,
                side,
                block,
                // A real human gesture NEVER suppresses copy-on-select — the exfil
                // fence targets only scoped-edge injected gestures (stamped by the
                // `mouse` control verb). Human copy-on-select is unaffected.
                suppress_copy_on_select: false,
                px_off,
            },
            Source::Human,
        );
    }

    /// `MouseWheel` -> when an app is tracking the mouse, report wheel up/down at
    /// the cell under the pointer; otherwise scroll the scrollback viewport (the
    /// everyday "scroll up to see history" gesture).
    pub(crate) fn on_mouse_wheel(&mut self, wid: WindowId, delta: MouseScrollDelta) {
        // The modal claim is decided before normalization. Horizontal/sub-line gestures
        // still return below without ever reaching native/terminal scroll consumers.
        let palette = self.palette_claims_pointer(wid);
        // Lines to move per event: whole lines drained from the per-window
        // residual — one per classic ±1 LineDelta notch, banked fractions for
        // precision-touchpad LineDelta and trackpad PixelDelta.
        let (dir_up, lines) = match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                // Ignore a predominantly-horizontal notch (a horizontal wheel or a
                // tilt-wheel): a horizontal gesture must NOT scroll the viewport
                // vertically. Without this, `y == 0.0` fell through to dir_up=false
                // + `.max(1)` and scrolled DOWN one line on every horizontal swipe.
                if y == 0.0 || y.abs() <= x.abs() {
                    return;
                }
                // Fractional deltas (Windows precision touchpads / free-spinning
                // wheels emit many |y| << 1 events) bank into the residual and
                // emit only WHOLE lines — the old `.round().max(1)` forced a full
                // line per micro-event, scrolling several times too fast. A
                // classic ±1-per-notch wheel still moves exactly one line/notch.
                let Some(ws) = self.windows.get_mut(&wid) else {
                    return;
                };
                match bank_scroll_lines(&mut ws.scroll_residual, f64::from(y)) {
                    Some(r) => r,
                    None => return, // sub-line motion banked; nothing to emit yet
                }
            }
            MouseScrollDelta::PixelDelta(p) => {
                // Same guard for trackpad pixel deltas: bail when the vertical
                // component is negligible or dominated by the horizontal one, so a
                // horizontal two-finger swipe is a no-op instead of a phantom
                // scroll-down. Vertical-dominant events keep the prior `.max(1)`
                // one-line-minimum behavior unchanged.
                if p.y.abs() < f64::EPSILON || p.y.abs() <= p.x.abs() {
                    return;
                }
                let ch = self.win_cell_size(wid).1.max(1) as f64;
                // Accumulate the signed sub-line delta in the per-window residual
                // and emit only WHOLE lines, carrying the fraction forward — so
                // slow, precise trackpad scrolling moves pixel-by-pixel instead of
                // snapping ≥1 line per event and dropping the remainder.
                let Some(ws) = self.windows.get_mut(&wid) else {
                    return;
                };
                match bank_scroll_lines(&mut ws.scroll_residual, p.y / ch) {
                    Some(r) => r,
                    None => return, // sub-line motion banked; nothing to emit yet
                }
            }
        };
        if palette {
            self.palette_pointer_wheel(
                wid,
                if dir_up {
                    -(lines as isize)
                } else {
                    lines as isize
                },
            );
            return;
        }
        if self.active_native_view(wid).is_some() {
            let signed = if dir_up { -lines } else { lines };
            let _ =
                self.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(signed));
            return;
        }
        let (row, col) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| ws.last_mouse_cell);
        let mods = self.mouse_modifiers(wid);
        let px_off = self
            .windows
            .get(&wid)
            .map_or(crate::input::PixelOffset::CELL_ORIGIN, |ws| {
                ws.last_mouse_px_off
            });
        // Phase 0.5: the seam decides tracking-ON (N reports / N lines — kills e)
        // vs tracking-OFF (scroll the viewport `lines`) under one mode read.
        self.input(
            wid,
            InputEvent::Wheel {
                dir_up,
                lines,
                row,
                col,
                mods,
                px_off,
            },
            Source::Human,
        );
    }

    /// Snap the viewport back to the live bottom (called on keyboard input, the
    /// standard "start typing and jump to the prompt" behavior).
    pub(crate) fn snap_to_bottom(&mut self, wid: WindowId) {
        let Some(term) = self
            .front_terminal(wid)
            .map(|terminal| terminal.term.clone())
        else {
            return;
        };
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        // M1: typing snaps INSTANTLY (the brief's "keyboard input still snaps"
        // clause) — cancel any in-flight wheel glide so its eased tail cannot
        // scroll the viewport back away from the prompt. M1b: drop any banked
        // sub-row residual and any elastic-overscroll bounce too, so the
        // jump-to-live is whole-row.
        ws.scroll_glide = None;
        ws.overscroll = None;
        ws.scroll_frac_px = 0;
        let scrolled = {
            let mut term = term_lock(&term);
            if term.grid().display_offset() != 0 {
                term.scroll_to_bottom();
                true
            } else {
                false
            }
        };
        if scrolled && let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Edit ▸ Select All: select the entire visible screen as whole lines (a
    /// `Lines` selection from the top row to the bottom row, full width), then
    /// repaint so the highlight shows. Mirrors a triple-click line selection
    /// dragged top-to-bottom; the snap-to-bottom first makes 0..rows stable
    /// selection coordinates (matching `search_recompute`). Copy (Cmd-C) then
    /// works on the whole screen exactly as on a mouse selection.
    pub(crate) fn select_all(&mut self) {
        // A window-level command (menu Select All): targets the frontmost window.
        let Some(wid) = self.frontmost_window else {
            return;
        };
        self.snap_to_bottom(wid);
        let Some(ws) = self.front() else { return };
        let Some(terminal) = ws.front_terminal() else {
            return;
        };
        let last = i32::from(ws.rows.saturating_sub(1));
        let max_col = ws.cols.saturating_sub(1);
        {
            let mut term = term_lock(&terminal.term);
            let sel = term.text_selection_mut();
            sel.start_selection(0, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(last, max_col, SelectionSide::Right);
            sel.expand_lines(max_col);
            sel.complete_selection();
        }
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }
}

/// Open an ALREADY-ALLOWLISTED URL via the OS default handler — macOS `open`,
/// Linux `xdg-open`, Windows `ShellExecuteW`. Shared by the terminal's
/// Cmd-click link open and the About dialog's site link; callers gate through
/// `is_safe_url` FIRST (this helper trusts its input). Best-effort: a failed
/// spawn (`xdg-open` absent, etc.) degrades to a silent no-op.
pub(crate) fn open_url_external(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(windows)]
    open_url_windows(url);
}

/// Windows arm of the Ctrl-click link open: hand the (allowlisted) URL to the
/// shell's default handler via `ShellExecuteW` — never through `cmd.exe /C
/// start`, whose metacharacter parsing (`&`, `^`, …) would reopen the injection
/// hole the `is_safe_url` allowlist closes. shell32 is on the approved tiny-FFI
/// list for exactly this use (no kernel32/std alternative opens a URL by its
/// registered handler). Failure (return <= 32) degrades to a silent no-op,
/// matching the Linux `xdg-open` arm.
#[cfg(windows)]
fn open_url_windows(url: &str) {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;
    }
    const SW_SHOWNORMAL: i32 = 1;
    let verb: [u16; 5] = [
        u16::from(b'o'),
        u16::from(b'p'),
        u16::from(b'e'),
        u16::from(b'n'),
        0,
    ];
    // `is_safe_url` already rejected control bytes, so the URL cannot embed a
    // NUL that would truncate the wide string.
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the
    // call; the null hwnd/parameters/directory are documented as optional.
    let _ = unsafe {
        ShellExecuteW(
            0,
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
}

/// Bank a signed scroll delta (in LINES) into the per-window residual and drain
/// the whole lines: `Some((dir_up, n))` with `n >= 1`, or `None` while the
/// accumulated motion is still sub-line. A direction flip forfeits the old
/// remainder first, so a reversal responds immediately instead of paying off
/// leftover opposite-direction motion.
fn bank_scroll_lines(residual: &mut f64, delta: f64) -> Option<(bool, i32)> {
    if *residual != 0.0 && (*residual > 0.0) != (delta > 0.0) {
        *residual = 0.0;
    }
    *residual += delta;
    let whole = residual.trunc();
    *residual -= whole;
    let n = whole.abs() as i32;
    (n > 0).then_some((whole > 0.0, n))
}

#[cfg(test)]
mod tests {
    use super::{bank_scroll_lines, press_selection_kind, press_starts_selection};
    use aterm_core::selection::SelectionType;

    fn native_palette_row_center(
        app: &crate::App,
        wid: crate::WindowId,
        slot: usize,
    ) -> (f64, f64) {
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let head = app.win_head(wid);
        let window = &app.windows[&wid];
        let scale = window.scale.max(f64::EPSILON);
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32 / scale as f32,
            ch: ch as f32 / scale as f32,
            font_px: 13.0,
            cols: window.cols as usize,
            panel_rows: window.overlay_rows(),
        };
        let (rx, ry, rw, rh) =
            crate::palette::palette_row_rect(window.palette().expect("open palette"), &geom, slot)
                .expect("visible palette row");
        let (ox, oy) = app.frame_origin(wid);
        (
            ox as f64 + pad as f64 + f64::from(rx + rw * 0.5) * scale,
            oy as f64
                + (head + usize::from(app.tab_strip_rows) * ch) as f64
                + f64::from(ry + rh * 0.5) * scale,
        )
    }

    fn native_content_to_window(
        app: &mut crate::App,
        wid: crate::WindowId,
        x: f32,
        y: f32,
    ) -> (f64, f64) {
        let view = prepare_presented_native(app, wid);
        let artifact = app
            .retained_native_leaf_artifact(wid, view, true)
            .expect("presented native semantic artifact");
        (
            artifact.device_x as f64 + f64::from(x) * artifact.scale,
            artifact.device_y as f64 + f64::from(y) * artifact.scale,
        )
    }

    /// Tests must drive the same retained, successfully presented artifact as
    /// production pointer routing. Preparing a speculative tree is deliberately
    /// insufficient: the `presented` bit is the glass-commit boundary.
    fn prepare_presented_native(
        app: &mut crate::App,
        wid: crate::WindowId,
    ) -> crate::tab_model::ViewId {
        let (_, view) = app.active_native_view(wid).expect("active native view");
        assert!(app.prepare_native_input_scratch(wid));
        app.windows
            .get_mut(&wid)
            .and_then(|window| window.leaf_render_cache.get_mut(&view))
            .and_then(|cache| cache.native.as_mut())
            .expect("retained native raster")
            .presented = true;
        assert!(
            app.retained_native_leaf_artifact(wid, view, true).is_some(),
            "prepared native artifact must satisfy the shipping glass guard"
        );
        view
    }

    fn presented_native_ui(
        app: &mut crate::App,
        wid: crate::WindowId,
    ) -> crate::native_ui::CompiledUi {
        let view = prepare_presented_native(app, wid);
        app.retained_native_leaf_artifact(wid, view, true)
            .expect("presented native semantic artifact")
            .compiled
            .clone()
    }

    fn native_text_boundary_point(
        app: &mut crate::App,
        wid: crate::WindowId,
        key: &crate::native_ui::UiKey,
        byte: usize,
    ) -> (f64, f64) {
        let compiled = presented_native_ui(app, wid);
        let paint = compiled
            .paint
            .iter()
            .find(|node| &node.key == key)
            .expect("painted text field");
        let crate::native_ui::UiContent::TextField(control) = &paint.content else {
            panic!("target is a text field");
        };
        let text = control.spec.input.as_ref().map_or_else(
            || match &control.value {
                crate::native_ui::SemanticValue::Text(value) => value.as_str(),
                _ => "",
            },
            |input| input.text.as_str(),
        );
        let geometry = crate::native_ui::text_field_geometry(control, paint.rect, 13.0);
        let x = crate::native_ui::text_field_x_for_byte(text, &geometry, byte, 13.0);
        assert_eq!(
            compiled.text_field_byte_at(key, x),
            Some(byte),
            "test point must invert through shipping hit geometry"
        );
        native_content_to_window(app, wid, x, paint.rect.y + paint.rect.height * 0.5)
    }

    #[test]
    fn palette_over_native_tab_swallows_pointer_before_underlying_hit_test() {
        use crate::native_app::AppViewState;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = crate::native_text_input::TextInputState::new("écho".to_string());
        settings.search_input.set_selection(2, 2);
        settings.set_search("écho".to_string());
        assert!(app.prepare_native_input_scratch(wid));
        app.windows
            .get_mut(&wid)
            .unwrap()
            .leaf_render_cache
            .get_mut(&view)
            .unwrap()
            .native
            .as_mut()
            .unwrap()
            .presented = true;
        let artifact = app
            .retained_native_leaf_artifact(wid, view, true)
            .expect("presented Settings UI");
        let hit = artifact
            .compiled
            .hits
            .iter()
            .find(|hit| hit.key.as_str() == "settings/search")
            .expect("actionable Settings search field");
        let scale = artifact.scale;
        let x = artifact.device_x as f64 + f64::from(hit.rect.x + hit.rect.width * 0.5) * scale;
        let y = artifact.device_y as f64 + f64::from(hit.rect.y + hit.rect.height * 0.5) * scale;

        // Sanity: without the modal, this exact point belongs to the underlying control.
        let point = app.native_content_point(wid, x, y).unwrap();
        assert!(artifact.compiled.hit_test(point.0, point.1).is_some());
        app.on_cursor_moved(wid, x, y);
        let before_focus = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone());
        let before_selection = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Settings(settings) => settings.search_input.selection().clone(),
            _ => unreachable!(),
        };
        assert!(
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().hovered.as_ref())
                .is_some(),
            "underlying hover is live before the modal opens"
        );

        app.palette_enter();
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let common = app.native_runtime.view_state(view).unwrap().common();
        assert!(common.hovered.is_none());
        assert!(common.pressed.is_none());
        assert_eq!(
            common.last_focus, before_focus,
            "click never reached Settings"
        );
        let AppViewState::Settings(settings) = app.native_runtime.view_state(view).unwrap() else {
            unreachable!();
        };
        assert_eq!(settings.search_input.selection(), &before_selection);
        assert!(app.windows[&wid].palette().is_some(), "modal remains open");
        assert!(!app.windows[&wid].native_text_cursor);
        assert!(!app.windows[&wid].hover_pointer);

        app.palette_exit();
        assert!(
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().hovered.as_ref())
                .is_some(),
            "closing immediately restores hover/cursor ownership to revealed content"
        );
    }

    #[test]
    fn settings_text_pointer_click_drag_and_shift_click_use_shipping_reducer_path() {
        use crate::native_app::AppViewState;
        use crate::native_text_input::TextInputState;
        use crate::native_ui::UiKey;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};
        use winit::keyboard::ModifiersState;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let suffix = "Aé👩‍💻末";
        let text = format!("{}{}", "long-prefix-".repeat(400), suffix);
        let suffix_start = text.len() - suffix.len();
        let woman_start = suffix_start + "Aé".len();
        let woman_end = woman_start + "👩‍💻".len();
        let e_start = suffix_start + "A".len();
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = TextInputState::new(text.clone());
        settings.set_search(text);
        settings.common.last_focus = Some(UiKey::new("settings/search"));

        let key = UiKey::new("settings/search");
        let (press_x, press_y) = native_text_boundary_point(&mut app, wid, &key, woman_start);
        app.on_cursor_moved(wid, press_x, press_y);
        assert!(app.windows[&wid].native_text_cursor);
        assert!(!app.windows[&wid].hover_pointer);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(
            settings.search_input.selection().range(),
            woman_start..woman_start
        );
        assert_eq!(settings.common.pressed.as_ref(), Some(&key));

        // Release at a different boundary in the same painted key. The press
        // establishes the reducer-owned anchor; release extends it.
        let (release_x, release_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
        app.on_cursor_moved(wid, release_x, release_y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(settings.search_input.selected_text(), "👩‍💻");
        assert!(settings.common.pressed.is_none());

        // A plain click collapses again, even after a drag selection.
        let (click_x, click_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
        app.on_cursor_moved(wid, click_x, click_y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (click_x, click_y) = native_text_boundary_point(&mut app, wid, &key, woman_end);
        app.on_cursor_moved(wid, click_x, click_y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(
            settings.search_input.selection().range(),
            woman_end..woman_end
        );

        // Shift-press preserves that caret as the anchor and extends over whole
        // multibyte graphemes. Release uses the same-key guard as buttons.
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::SHIFT;
        let (shift_x, shift_y) = native_text_boundary_point(&mut app, wid, &key, e_start);
        app.on_cursor_moved(wid, shift_x, shift_y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (shift_x, shift_y) = native_text_boundary_point(&mut app, wid, &key, e_start);
        app.on_cursor_moved(wid, shift_x, shift_y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(settings.search_input.selection().anchor, woman_end);
        assert_eq!(settings.search_input.selection().head, e_start);
        assert_eq!(settings.search_input.selected_text(), "é👩‍💻");
    }

    #[test]
    fn settings_color_field_padding_and_swatch_clamp_to_committed_edges() {
        use crate::native_app::AppViewState;
        use crate::native_text_input::TextInputState;
        use crate::native_ui::UiKey;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let field_key = crate::prefs::EDIT_FOREGROUND;
        let value = "#123456";
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = TextInputState::new("foreground".to_string());
        settings.set_search("foreground".to_string());
        // Global search is the result surface itself and never reserves a
        // renderer-preview disclosure slice. Its top-ranked foreground result
        // therefore remains at offset zero.
        settings.page_scroll = 0;
        settings
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == field_key)
            .expect("foreground preference")
            .seed = Some(value.to_string());

        let key = UiKey::new(format!("settings/control/{field_key}"));
        let edge_point = |app: &mut App, trailing: bool| {
            let compiled = presented_native_ui(app, wid);
            let paint = compiled
                .paint
                .iter()
                .find(|node| node.key == key)
                .expect("painted foreground field");
            let x = if trailing {
                paint.rect.right() - 1.0
            } else {
                paint.rect.x + 1.0
            };
            assert_eq!(
                compiled.text_field_byte_at(&key, x),
                Some(if trailing { value.len() } else { 0 })
            );
            native_content_to_window(app, wid, x, paint.rect.y + paint.rect.height * 0.5)
        };

        let (x, y) = edge_point(&mut app, true);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (x, y) = edge_point(&mut app, true);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(settings.editing_field.as_deref(), Some(field_key));
        assert_eq!(
            settings.field_inputs[field_key].selection().range(),
            value.len()..value.len(),
            "swatch click places an end caret"
        );

        let (x, y) = edge_point(&mut app, false);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (x, y) = edge_point(&mut app, false);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(
            settings.field_inputs[field_key].selection().range(),
            0..0,
            "leading padding click places a start caret"
        );

        // Drag from padding to swatch selects the whole committed value.
        let (x, y) = edge_point(&mut app, false);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (x, y) = edge_point(&mut app, true);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            unreachable!();
        };
        assert_eq!(settings.field_inputs[field_key].selected_text(), value);
    }

    #[test]
    fn settings_text_release_rejects_a_detached_native_view() {
        use crate::native_app::AppViewState;
        use crate::native_text_input::TextInputState;
        use crate::native_ui::UiKey;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state_mut(view) else {
            panic!("Settings state");
        };
        settings.search_input = TextInputState::new("écho".to_string());
        settings.set_search("écho".to_string());
        settings.common.last_focus = Some(UiKey::new("settings/search"));

        let key = UiKey::new("settings/search");
        let (x, y) = native_text_boundary_point(&mut app, wid, &key, 2);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let (release_x, release_y) = native_text_boundary_point(&mut app, wid, &key, 5);
        app.on_cursor_moved(wid, release_x, release_y);

        // The tab reference deliberately outlives its runtime view generation.
        // Release must not redirect the captured text action anywhere else.
        let detached = app
            .native_runtime
            .take_view_state(view)
            .expect("detach live Settings view");
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let AppViewState::Settings(settings) = detached else {
            unreachable!();
        };
        assert_eq!(settings.search_input.selection().range(), 2..2);
        assert_eq!(settings.search_input.value(), "écho");
    }

    #[test]
    fn text_field_pointer_payload_does_not_replace_button_slider_or_document_paths() {
        use crate::native_app::{AppKind, AppViewState};
        use crate::native_ui::{UiContent, UiKey};
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, settings_view) = app.active_native_view(wid).expect("Settings active");

        // Ordinary buttons still arm on press and activate only on a matched
        // release; no text position exists for their painted node.
        let compiled = presented_native_ui(&mut app, wid);
        let route_button = compiled
            .hits
            .iter()
            .find(|hit| hit.action.as_str() == "settings/route/appearance")
            .expect("visible Appearance route button");
        let route_key = route_button.key.clone();
        assert_eq!(
            compiled.text_field_byte_at(&route_key, route_button.rect.x),
            None
        );
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            route_button.rect.x + route_button.rect.width * 0.5,
            route_button.rect.y + route_button.rect.height * 0.5,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
        else {
            panic!("Settings state");
        };
        assert_eq!(settings.route, crate::native_settings::SettingsRoute::Home);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
        else {
            unreachable!();
        };
        assert_eq!(
            settings.route,
            crate::native_settings::SettingsRoute::Appearance
        );

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let Some(AppViewState::Settings(settings)) =
            app.native_runtime.view_state_mut(settings_view)
        else {
            panic!("same singleton Settings view");
        };
        settings
            .legacy
            .fields
            .iter_mut()
            .find(|field| field.key == crate::prefs::EDIT_FONT_PX)
            .expect("font size field")
            .seed = Some("14".to_string());
        settings.search_input =
            crate::native_text_input::TextInputState::new("font size".to_string());
        settings.set_search("font size".to_string());
        // Short compact search results disclose their live preview first; the
        // numeric control is on the next bounded, user-reachable slice.
        settings.page_scroll = 1;
        let slider_key = UiKey::new(format!("settings/control/{}", crate::prefs::EDIT_FONT_PX));
        let compiled = presented_native_ui(&mut app, wid);
        let slider = compiled
            .paint
            .iter()
            .find(|node| node.key == slider_key)
            .expect("painted font slider");
        assert!(matches!(slider.content, UiContent::Slider(_)));
        let geometry = crate::native_ui::slider_geometry(slider.rect);
        let slider_x = geometry.track_x + (geometry.track_right - geometry.track_x) * 0.75;
        assert!(compiled.slider_value_at(&slider_key, slider_x).is_some());
        assert_eq!(compiled.text_field_byte_at(&slider_key, slider_x), None);
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            slider_x,
            slider.rect.y + slider.rect.height * 0.5,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(settings_view)
        else {
            unreachable!();
        };
        assert_ne!(
            settings.editing_field.as_deref(),
            Some(crate::prefs::EDIT_FONT_PX),
            "slider remains numeric rather than becoming a text editor"
        );

        // A real editor viewport still follows its source-addressable document
        // selection path. It never receives the Settings-only semantic payload.
        let dir = std::env::temp_dir().join(format!(
            "aterm-pointer-negative-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pointer.txt");
        std::fs::write(&path, "alpha βeta\nsecond\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy());
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (_, editor_view) = app.active_native_view(wid).expect("Editor active");
        let compiled = presented_native_ui(&mut app, wid);
        let editor_key = UiKey::new("editor/buffer");
        let editor = compiled
            .paint
            .iter()
            .find(|node| node.key == editor_key)
            .expect("painted editor viewport");
        let UiContent::TextViewport(spec) = &editor.content else {
            panic!("editor body remains a text viewport");
        };
        let geometry = crate::native_ui::text_viewport_geometry(editor.rect);
        let content_x = geometry.text_x + 36.0;
        let content_y = geometry.body_y + geometry.line_h * 0.5;
        let expected =
            crate::native_ui::text_viewport_byte_at(spec, editor.rect, content_x, content_y)
                .expect("materialized source byte");
        assert_eq!(compiled.text_field_byte_at(&editor_key, content_x), None);
        let (x, y) = native_content_to_window(&mut app, wid, content_x, content_y);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Editor(editor)) = app.native_runtime.view_state(editor_view) else {
            panic!("Editor state");
        };
        assert_eq!(
            editor.buffer.as_ref().unwrap().primary_selection().head,
            expected
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn native_palette_row_hover_and_left_release_activate_the_painted_target() {
        use crate::native_app::AppViewState;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        assert!(app.prepare_native_input_scratch(wid));
        app.palette_enter();
        for c in "Go to Modified".chars() {
            app.palette_filter_push(c);
        }

        // Reverse the native overlay placement exactly as the production hit path does and
        // land at the centre of the shared painted/hit rectangle for its sole result.
        let palette = app.windows[&wid].palette().unwrap();
        assert_eq!(palette.filtered().len(), 1, "unique route command fixture");
        let (x, y) = native_palette_row_center(&app, wid, 0);

        app.on_cursor_moved(wid, x, y);
        assert!(app.windows[&wid].palette().unwrap().pointer_over_row());
        assert!(
            app.windows[&wid].hover_pointer,
            "row advertises pointer affordance"
        );
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        assert!(app.windows[&wid].palette().is_some(), "press only arms");
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);

        assert!(
            app.windows[&wid].palette().is_none(),
            "matched release activates"
        );
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(
            settings.route,
            crate::native_settings::SettingsRoute::Modified
        );
    }

    #[test]
    fn wheel_scrolled_native_palette_hit_activates_the_newly_painted_row() {
        use crate::native_app::AppViewState;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton, MouseScrollDelta};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        assert!(app.prepare_native_input_scratch(wid));
        app.palette_enter();
        for c in "Settings:".chars() {
            app.palette_filter_push(c);
        }
        let palette = app.windows[&wid].palette().unwrap();
        let count = palette.filtered().len();
        let body = palette.scroll_extent().2;
        assert!(count > body, "Settings commands exceed the visible band");
        let about_index = palette
            .controls_lines()
            .iter()
            .filter(|line| line.starts_with("menu row "))
            .position(|line| line.contains("action=settings/route/about"))
            .expect("About route command");
        let wanted_scroll = about_index.saturating_sub(body - 1);
        assert!(wanted_scroll > 0, "About route exercises wheel scrolling");
        let about_slot = about_index - wanted_scroll;
        let before_page_scroll = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Settings(settings) => settings.page_scroll,
            _ => panic!("Settings state"),
        };

        // Keep the pointer on the last visible slot. Scrolling to the bottom replaces the
        // command under that exact painted rectangle, and the post-scroll re-hit selects it.
        let (x, y) = native_palette_row_center(&app, wid, about_slot);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_wheel(
            wid,
            MouseScrollDelta::LineDelta(0.0, -(wanted_scroll as f32)),
        );
        let palette = app.windows[&wid].palette().unwrap();
        assert_eq!(palette.scroll_extent().0, wanted_scroll);
        assert!(matches!(
            palette.selected_target(),
            Some(crate::palette::PaletteTarget::Native(target))
                if target.action.as_str() == "settings/route/about"
        ));
        assert_eq!(
            match app.native_runtime.view_state(view).unwrap() {
                AppViewState::Settings(settings) => settings.page_scroll,
                _ => unreachable!(),
            },
            before_page_scroll,
            "modal wheel never reaches the native Settings scroller"
        );

        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Settings(settings)) = app.native_runtime.view_state(view) else {
            panic!("Settings state");
        };
        assert_eq!(settings.route, crate::native_settings::SettingsRoute::About);
    }

    #[test]
    fn pointer_release_rejects_a_native_command_after_its_view_generation_dies() {
        use crate::native_app::AppViewState;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        assert!(app.prepare_native_input_scratch(wid));
        app.palette_enter();
        for c in "Settings: Search".chars() {
            app.palette_filter_push(c);
        }
        assert_eq!(app.windows[&wid].palette().unwrap().filtered().len(), 1);
        let (x, y) = native_palette_row_center(&app, wid, 0);
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);

        // Detach advances the lifecycle generation after the painted command was armed.
        // The active tab's stale reference remains solely to exercise dispatch rejection.
        let detached = app
            .native_runtime
            .take_view_state(view)
            .expect("detach live Settings view");
        let before_focus = detached.common().last_focus.clone();
        assert_eq!(app.native_runtime.view_generation(view), None);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);

        assert!(app.windows[&wid].palette().is_none(), "release was settled");
        let AppViewState::Settings(settings) = detached else {
            panic!("Settings state");
        };
        assert_eq!(
            settings.common.last_focus, before_focus,
            "stale generation receives no search action"
        );
    }

    #[test]
    fn click_below_terminal_palette_is_swallowed_without_starting_selection() {
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.palette_enter();
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid);
        let head = app.win_head(wid);
        let panel_rows = app.windows[&wid].overlay_rows();
        let (ox, oy) = app.frame_origin(wid);
        let x = ox as f64 + (pad + cw * 3) as f64;
        let y = oy as f64 + (pad + head + ch * (panel_rows + 2)) as f64;
        assert_eq!(app.palette_row_at_pointer(wid, x, y), None);
        let before_cell = app.windows[&wid].last_mouse_cell;

        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);

        let window = &app.windows[&wid];
        assert!(
            window.palette().is_some(),
            "outside click does not dismiss modal"
        );
        assert!(
            !window.selecting,
            "terminal selection gesture never started"
        );
        assert_eq!(
            window.last_mouse_cell, before_cell,
            "terminal pointer seam was bypassed"
        );
        assert!(
            !crate::term_lock(&window.front_terminal().expect("front terminal").term)
                .text_selection()
                .has_selection()
        );
    }

    #[test]
    fn palette_wheel_never_reaches_terminal_scrollback() {
        use crate::{App, WindowId};
        use winit::event::MouseScrollDelta;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        {
            let mut terminal = crate::term_lock(&term);
            for line in 0..100 {
                terminal.process(format!("history {line}\r\n").as_bytes());
            }
            assert!(terminal.grid().scrollback_lines() > 0);
            assert_eq!(terminal.grid().display_offset(), 0);
        }

        app.palette_enter();
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "modal wheel cannot arm terminal smooth scrolling"
        );

        // A fractional palette gesture is discarded on close rather than draining into
        // the terminal's next fractional event.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.75));
        app.palette_exit();
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 0.5));
        assert!(app.windows[&wid].scroll_glide.is_none());
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);

        // Negative control: a whole wheel after closing reaches the terminal path.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        assert!(
            app.windows[&wid].scroll_glide.is_some()
                || crate::term_lock(&term).grid().display_offset() > 0
        );
    }

    #[test]
    fn editor_pointer_selection_updates_only_the_exact_active_native_view() {
        use crate::native_app::{AppKind, AppViewState};
        use crate::{App, WindowId};

        let dir = std::env::temp_dir().join(format!(
            "aterm-editor-pointer-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pointer.md");
        std::fs::write(&path, "alpha\nβeta\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy());

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Editor, &uri).unwrap();
        let (_, view) = app.active_native_view(wid).expect("editor active");

        assert!(app.native_editor_pointer_select(wid, view, 5, false, 12));
        assert!(app.native_editor_pointer_select(wid, view, 1, true, 12));
        let selection = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Editor(state) => {
                state.buffer.as_ref().unwrap().primary_selection().clone()
            }
            _ => panic!("editor view changed kind"),
        };
        assert_eq!(selection.anchor, 5);
        assert_eq!(selection.head, 1);

        // A tab switch makes the captured view stale. The same pointer target
        // must not mutate the parked editor or leak into the terminal tab.
        app.switch_tab_in(wid, 0);
        assert!(!app.native_editor_pointer_select(wid, view, 0, false, 12));
        let after = match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Editor(state) => {
                state.buffer.as_ref().unwrap().primary_selection().clone()
            }
            _ => panic!("editor view changed kind"),
        };
        assert_eq!(after, selection);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// REGRESSION (v0.55): a pointer click on a popup-chip Enum row COMMITTED
    /// the next option through the cycle path instead of opening the picker —
    /// `reduce_action` had no value-less choice-surface guard, so the chip's
    /// `settings/set/*` activation fell through to `semantic_value_for_field`
    /// → `cycle_choice`, and every click silently rewrote the config. Pin the
    /// FULL production pointer pipeline (cursor move → press → release →
    /// retained hit-test → dispatch): a click must open the ChoicePicker as a
    /// pull-down decision surface with NO config edit, and dismissing it via
    /// Done must land back on the UNCHANGED committed value. (The picker's
    /// option-activation commit itself is pinned at the reducer seam by
    /// `pointer_activation_opens_choice_surface_then_selection_patches`.)
    #[test]
    fn enum_chip_click_opens_the_choice_picker_and_commits_nothing() {
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        let key = crate::native_ui::UiKey::new(format!(
            "settings/control/{}",
            crate::prefs::EDIT_CURSOR_STYLE
        ));
        // The page window is virtualized: scroll until the enum chip paints.
        let mut compiled = presented_native_ui(&mut app, wid);
        for _ in 0..40 {
            if compiled.paint.iter().any(|node| node.key == key) {
                break;
            }
            app.dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(1))
                .unwrap();
            compiled = presented_native_ui(&mut app, wid);
        }
        let chip = compiled
            .paint
            .iter()
            .find(|node| node.key == key)
            .expect("enum control painted");
        let UiContent::Button(control) = &chip.content else {
            panic!("popup-chip enum row paints a Button, got another control");
        };
        let committed = control.spec.label.clone();
        let chip_rect = chip.rect;

        let click = |app: &mut App, x: f64, y: f64| {
            app.on_cursor_moved(wid, x, y);
            app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
            app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        };

        // ONE click on the chip: the pull-down list opens — it must not commit.
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            chip_rect.x + chip_rect.width / 2.0,
            chip_rect.y + chip_rect.height / 2.0,
        );
        click(&mut app, x, y);
        let open = presented_native_ui(&mut app, wid);
        assert!(
            open.paint.iter().any(|node| {
                node.key.as_str()
                    == format!("settings/choice-picker/{}", crate::prefs::EDIT_CURSOR_STYLE)
            }),
            "a click on a popup-chip enum row opens the ChoicePicker"
        );
        assert!(
            open.paint.iter().any(|node| {
                node.key.as_str()
                    == format!("settings/choice/{}/0", crate::prefs::EDIT_CURSOR_STYLE)
            }),
            "the pull-down lists selectable options"
        );

        // Dismiss via Done: the chip returns showing the UNCHANGED committed
        // value. Under the v0.55 cycle regression a single click had already
        // advanced it (with no picker ever opening).
        let done = open
            .paint
            .iter()
            .find(|node| node.key.as_str() == "settings/choice-close")
            .expect("picker paints its Done control");
        let (dx, dy) = native_content_to_window(
            &mut app,
            wid,
            done.rect.x + done.rect.width / 2.0,
            done.rect.y + done.rect.height / 2.0,
        );
        click(&mut app, dx, dy);
        let closed = presented_native_ui(&mut app, wid);
        assert!(
            !closed
                .paint
                .iter()
                .any(|node| node.key.as_str().starts_with("settings/choice-picker/")),
            "Done dismisses the pull-down"
        );
        let chip = closed
            .paint
            .iter()
            .find(|node| node.key == key)
            .expect("enum control repaints after dismissal");
        let UiContent::Button(control) = &chip.content else {
            panic!("popup-chip enum row paints a Button, got another control");
        };
        assert_eq!(
            control.spec.label, committed,
            "opening + dismissing the pull-down never cycles the committed value"
        );
    }

    #[test]
    fn markdown_pointer_selects_the_exact_authored_block_through_semantic_hit_testing() {
        use crate::native_app::{AppKind, AppViewState};
        use crate::native_ui::UiContent;
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton};

        let dir = std::env::temp_dir().join(format!(
            "aterm-markdown-pointer-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pointer.md");
        let source = "# **Heading**\n\nBody [link](https://example.com).\n";
        std::fs::write(&path, source).unwrap();
        let uri = format!("file://{}", path.to_string_lossy());
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.open_document_tab(AppKind::Markdown, &uri).unwrap();
        let (_, view) = app.active_native_view(wid).expect("Markdown active");
        let compiled = presented_native_ui(&mut app, wid);
        let block = compiled
            .paint
            .iter()
            .find(|node| {
                matches!(&node.content, UiContent::MarkdownBlock(_))
                    && node.key.as_str() == "markdown/block/0"
            })
            .expect("first Markdown block painted");
        let expected = match &block.content {
            UiContent::MarkdownBlock(spec) => spec.source.clone(),
            _ => unreachable!(),
        };
        let (x, y) = native_content_to_window(
            &mut app,
            wid,
            block.rect.x + block.rect.width / 2.0,
            block.rect.y + block.rect.height / 2.0,
        );
        app.on_cursor_moved(wid, x, y);
        app.on_mouse_input(wid, ElementState::Pressed, MouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, MouseButton::Left);
        let Some(AppViewState::Markdown(state)) = app.native_runtime.view_state(view) else {
            panic!("Markdown view changed kind");
        };
        assert_eq!(state.selection, Some(expected.clone()));
        assert_eq!(&source[expected], "# **Heading**\n");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// REGRESSION (audit): a sidebar SEARCH click during a live free-form edit
    /// must cancel the edit BEFORE focusing the search — the one arm of the
    /// edit+search dual-state fix that shipped without a test. A surviving
    /// buffer would swallow every "query" keystroke (both key paths check
    /// `editing` before `searching`) and ↵ would commit it against the row —
    /// with the whole suite green. Pins the mutual exclusion through the REAL
    /// click path (`settings_click` → `SidebarHit::Search`).
    #[test]
    fn search_click_during_edit_cancels_the_edit() {
        use crate::{App, WindowId};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // `settings_panel_rows` mins against the composed frame height; a headless
        // window never rendered, so seed the frame rows the live path would have.
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.input_scratch.cells = vec![Vec::new(); 24];
        }
        app.settings_enter();
        // Begin a live free-form edit (font_family) and type into it.
        let idx = app
            .front()
            .and_then(|ws| ws.settings())
            .and_then(|s| {
                s.fields
                    .iter()
                    .position(|f| f.key == crate::prefs::EDIT_FONT_FAMILY)
            })
            .expect("font_family row");
        app.settings_select(idx);
        app.settings_edit_begin();
        app.settings_edit_push('M');
        assert_eq!(
            app.front()
                .and_then(|ws| ws.settings())
                .and_then(|s| s.editing.clone()),
            Some("M".to_string()),
            "a free-form edit is live"
        );
        // Click the sidebar SEARCH field (rows 1-2, x inside the sidebar) through
        // the same pixel path a real press takes.
        let (cw, ch) = app.cell_size();
        let pad = app.backend.pad() as f64;
        let (x, y) = (pad + 2.0 * cw as f64, pad + 1.5 * ch as f64);
        {
            // Sanity: the point resolves the Search row, not a category/margin.
            let ws = app.windows.get(&wid).expect("window 0");
            let row = ((y - pad) / ch as f64) as usize;
            assert_eq!(
                crate::settings::sidebar_hit(row, ws.settings_panel_rows()),
                Some(crate::settings::SidebarHit::Search),
            );
        }
        app.settings_click(wid, x, y);
        let s = app
            .front()
            .and_then(|ws| ws.settings())
            .expect("settings open");
        assert_eq!(
            s.editing, None,
            "the search click abandons the pending edit"
        );
        assert!(
            s.searching,
            "…and focuses the search bar — never both at once"
        );
    }

    #[test]
    fn settings_click_rejects_all_four_card_exterior_bands() {
        use crate::{App, WindowId};
        use winit::dpi::PhysicalSize;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(4);
        app.backend.set_head(3);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.metrics.pad = 12;
            ws.metrics.pad_top = 4;
            ws.input_scratch.cells = vec![Vec::new(); 24];
        }
        app.settings_enter();

        let (cw, ch) = app.win_cell_size(wid);
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (ws.rows as usize, ws.cols as usize)
        };
        app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
            (cols * cw + 24 + cw.saturating_sub(1).max(2)) as u32,
            (rows * ch + 4 + 12 + 3 + ch.saturating_sub(1).max(2)) as u32,
        ));
        let (origin_x, origin_y) = app.frame_origin(wid);
        assert!(
            origin_x > 0 && origin_y > 0,
            "fixture needs remainder bands"
        );
        let panel_rows = app.windows[&wid].settings_panel_rows();
        let left = (origin_x + 12) as f64;
        let top = (origin_y + 4 + 3) as f64;
        let right = left + (cols * cw) as f64;
        let bottom = top + (panel_rows * ch) as f64;
        let points = [
            ("left", left - 0.5, top + 1.5 * ch as f64),
            ("right", right + 0.5, top + 1.5 * ch as f64),
            ("top", left + 2.0 * cw as f64, top - 0.5),
            ("bottom", left + 2.0 * cw as f64, bottom + 0.5),
        ];
        let before = app.windows[&wid].settings().unwrap().fingerprint();
        for (band, x, y) in points {
            assert_eq!(
                app.settings_card_point_if_inside(wid, x, y),
                None,
                "{band} band is outside the canonical card"
            );
            assert_eq!(
                app.settings_row_at(wid, x, y),
                None,
                "{band} band cannot alias a settings row"
            );
            app.settings_click(wid, x, y);
            assert_eq!(
                app.windows[&wid].settings().unwrap().fingerprint(),
                before,
                "{band} band cannot focus sidebar or activate a widget"
            );
        }
    }

    /// The retired compatibility card still shares production pointer seams.
    /// A nonzero compositor remainder plus asymmetric top padding/headroom must
    /// be removed exactly once: click selection and colour-wheel drag see the
    /// same card-local point the painter sees.
    #[test]
    fn settings_pointer_is_frame_local_once_and_drag_matches_press_geometry() {
        use crate::{App, WindowId};
        use winit::dpi::PhysicalSize;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(4);
        app.backend.set_head(3);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.metrics.pad = 12;
            ws.metrics.pad_top = 4;
            ws.input_scratch.cells = vec![Vec::new(); 24];
        }
        app.settings_enter();

        let (cw, ch) = app.win_cell_size(wid);
        let (rows, cols) = {
            let ws = &app.windows[&wid];
            (ws.rows as usize, ws.cols as usize)
        };
        // A strict sub-cell remainder produces a nonzero leading frame band.
        let rw = cw.saturating_sub(1).max(2);
        let rh = ch.saturating_sub(1).max(2);
        app.windows.get_mut(&wid).unwrap().win_px = Some(PhysicalSize::new(
            (cols * cw + 24 + rw) as u32,
            (rows * ch + 4 + 12 + 3 + rh) as u32,
        ));
        let (ox, oy) = app.frame_origin(wid);
        assert!(ox > 0 && oy > 0, "fixture needs both frame remainder bands");

        let panel_rows = app.windows[&wid].settings_panel_rows();
        let pane = crate::settings::pane_geom_cells(cols, panel_rows);
        let card_x = pane.sidebar_w_cells * cw as f32 + cw as f32 * 0.5;
        let raw_x = ox as f64 + 12.0 + f64::from(card_x);
        let current = app.windows[&wid]
            .settings()
            .and_then(crate::settings::SettingsState::action_target);
        let (raw_y, wanted) = (0..panel_rows)
            .find_map(|row| {
                let y = oy as f64 + 4.0 + 3.0 + (row * ch + ch / 2) as f64;
                app.settings_row_at(wid, raw_x, y)
                    .filter(|idx| Some(*idx) != current)
                    .map(|idx| (y, idx))
            })
            .expect("a second painted Settings control");
        assert_eq!(
            app.settings_card_point(wid, raw_x, raw_y),
            (card_x, (raw_y - oy as f64 - 4.0 - 3.0) as f32),
            "frame remainder and asymmetric top origin are each stripped once"
        );
        app.settings_click(wid, raw_x, raw_y);
        assert_eq!(
            app.windows[&wid]
                .settings()
                .and_then(crate::settings::SettingsState::action_target),
            Some(wanted),
            "click uses raw window coordinates when resolving the row"
        );

        let color = app.windows[&wid]
            .settings()
            .unwrap()
            .fields
            .iter()
            .position(|field| field.key == crate::prefs::EDIT_FOREGROUND)
            .expect("foreground colour row");
        app.settings_select(color);
        app.settings_wheel_open();
        app.settings_wheel_press_disk(0.0, 0.0);
        let geom = app.settings_geom_front().expect("settings geometry");
        let wheel_geom = crate::settings::wheel_geom(app.windows[&wid].settings().unwrap(), &geom)
            .expect("wheel geometry");
        let target_cx = wheel_geom.disk_cx + wheel_geom.disk_r * 0.55;
        let target_cy = wheel_geom.disk_cy;
        let expected = crate::settings::disk_hs_at(&wheel_geom, target_cx, target_cy);
        let drag_x = ox as f64 + 12.0 + f64::from(target_cx);
        let drag_y = oy as f64 + 4.0 + 3.0 + f64::from(target_cy);
        assert!(app.settings_wheel_drag_motion(wid, drag_x, drag_y));
        let wheel = app.windows[&wid]
            .settings()
            .and_then(|settings| settings.wheel.as_ref())
            .expect("wheel remains open");
        assert!((wheel.h - expected.0).abs() < 1.0e-6);
        assert!((wheel.s - expected.1).abs() < 1.0e-6);
    }

    /// Classic wheel notches (±1 or ±3 lines per event) pass through whole and
    /// leave no residual — behavior identical to the pre-banking code.
    #[test]
    fn whole_wheel_notches_pass_through_unbanked() {
        let mut r = 0.0;
        assert_eq!(bank_scroll_lines(&mut r, 1.0), Some((true, 1)));
        assert_eq!(r, 0.0);
        assert_eq!(bank_scroll_lines(&mut r, -3.0), Some((false, 3)));
        assert_eq!(r, 0.0);
    }

    /// Fractional LineDelta (Windows precision touchpads emit many |y| << 1
    /// events) banks until a whole line accumulates instead of the old
    /// `.round().max(1)` forcing one full line per micro-event.
    #[test]
    fn fractional_deltas_bank_until_a_whole_line() {
        let mut r = 0.0;
        for _ in 0..4 {
            assert_eq!(
                bank_scroll_lines(&mut r, 0.2),
                None,
                "sub-line: nothing emitted"
            );
        }
        assert_eq!(bank_scroll_lines(&mut r, 0.2), Some((true, 1)));
        assert!(r.abs() < 1e-9, "the whole line drained the bank: {r}");
        // Same going down.
        assert_eq!(bank_scroll_lines(&mut r, -0.6), None);
        assert_eq!(bank_scroll_lines(&mut r, -0.6), Some((false, 1)));
    }

    /// A direction flip forfeits the banked remainder, so reversing never has
    /// to pay off leftover motion from the old direction first.
    #[test]
    fn direction_flip_forfeits_the_banked_remainder() {
        let mut r = 0.0;
        assert_eq!(bank_scroll_lines(&mut r, 0.9), None);
        assert_eq!(
            bank_scroll_lines(&mut r, -0.5),
            None,
            "the +0.9 bank must not absorb the reversal into a net +0.4"
        );
        assert_eq!(
            r, -0.5,
            "the reversal starts a fresh bank in its own direction"
        );
        assert_eq!(bank_scroll_lines(&mut r, -0.5), Some((false, 1)));
    }

    /// Audit M6 — a left press starts a local selection when nothing is tracking, OR
    /// when Option is held to bypass a tracking app; a plain press while tracking
    /// keeps reporting to the app.
    #[test]
    fn option_press_bypasses_tracking_for_local_selection() {
        assert!(
            press_starts_selection(false, false),
            "no tracking: always local"
        );
        assert!(press_starts_selection(false, true));
        assert!(
            !press_starts_selection(true, false),
            "tracking + plain press keeps reporting to the app"
        );
        assert!(
            press_starts_selection(true, true),
            "Option overrides tracking (the bypass gesture)"
        );
    }

    /// Audit M6 — Option+drag is the rectangular block selection only when the press
    /// was local anyway; while Option buys the tracking bypass it selects normally.
    #[test]
    fn option_drag_kind_depends_on_who_owns_the_mouse() {
        assert_eq!(
            press_selection_kind(true, false),
            SelectionType::Block,
            "local Option+drag stays the rectangular block selection"
        );
        assert_eq!(
            press_selection_kind(true, true),
            SelectionType::Simple,
            "bypass Option+drag selects normally (the modifier bought the bypass)"
        );
        assert_eq!(press_selection_kind(false, false), SelectionType::Simple);
        assert_eq!(press_selection_kind(false, true), SelectionType::Simple);
    }

    /// W1 regression (kill the compositor stretch): the pointer geometry mirrors
    /// the band placement. A window exactly grid-fit + 7px shifts the frame — and
    /// thus every pixel→cell mapping — by the leading 3px band (`band_offset`
    /// splits 7 as 3/4); headless (no `win_px`) and an exact grid fit stay at
    /// origin 0. The fixture uses independent 2px top / 12px bottom padding.
    #[test]
    fn pointer_geometry_tracks_the_band_offset() {
        use crate::{App, WindowId};
        use winit::dpi::PhysicalSize;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.backend.set_pad(12);
        app.backend.set_pad_top(2);
        app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
        app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
        // Headless: the raw window size is unknown -> identity mapping.
        assert_eq!(
            app.frame_origin(wid),
            (0, 0),
            "headless must be the identity"
        );

        let (cw, ch) = app.cell_size();
        let pad = app.backend.pad();
        let pad_top = app.backend.pad_top();
        let (rows, cols) = {
            let ws = app.windows.get(&wid).expect("seeded window");
            (ws.rows as usize, ws.cols as usize)
        };
        let fit = PhysicalSize::new(
            (cols * cw + 2 * pad) as u32,
            (rows * ch + pad_top + pad) as u32,
        );
        // Exact grid fit: zero remainder, origin stays 0 (byte-identical).
        app.windows.get_mut(&wid).expect("window").win_px = Some(fit);
        assert_eq!(
            app.frame_origin(wid),
            (0, 0),
            "exact fit must be the identity"
        );
        let (px, py) = ((pad + 5 * cw) as f64 + 1.0, (pad_top + 2 * ch) as f64 + 1.0);
        let at_fit = app.pixel_to_cell(wid, px, py);

        // Grid fit + a SUB-CELL remainder per axis (strictly < the cell, so it
        // centres into a leading band instead of being absorbed as another whole
        // column/row — the readback pin's window). Derive the remainder from the
        // ACTUAL cell size so the test survives font/size default changes: a
        // hardcoded +7 silently became a whole extra column once the cell width
        // dropped to 7px (Monaco/Menlo at the 12px default).
        let (rw, rh) = (cw.saturating_sub(1).max(1), ch.saturating_sub(1).max(1));
        app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
            fit.width + rw as u32,
            fit.height + rh as u32,
        ));
        // pad_split centres: the leading band is the low half of the remainder.
        let (ox, oy) = (rw / 2, rh / 2);
        assert_eq!(
            app.frame_origin(wid),
            (ox as i64, oy as i64),
            "a sub-cell remainder centres into a leading band"
        );
        assert_eq!(
            app.pixel_to_cell(wid, px + ox as f64, py + oy as f64),
            at_fit,
            "a click over the same on-glass cell must map to the same cell"
        );
        // And a click IN the leading band clamps to the frame edge (col 0), the
        // same semantics a click in the pad border always had.
        let (_, col) = app.pixel_to_cell(wid, 1.0, py + oy as f64);
        assert_eq!(col, 0, "a band click clamps to the leading cell");

        // A transient surface smaller than the composed frame is a centred
        // SOURCE crop. Preserve its signed origin so window pixel 0 maps to the
        // actual positive frame coordinate instead of incorrectly clamping at 0.
        app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
            fit.width.saturating_sub(3).max(1),
            fit.height.saturating_sub(5).max(1),
        ));
        let cropped_origin = app.frame_origin(wid);
        assert!(cropped_origin.0 < 0 && cropped_origin.1 < 0);
        assert_eq!(
            app.window_to_frame(wid, 0.0, 0.0),
            (-cropped_origin.0 as f64, -cropped_origin.1 as f64),
            "pointer transform must retain the centred source crop"
        );
    }

    /// BUG 18 regression: an Option-bypass (M6) LOCAL selection started while a
    /// foreground app is mouse-tracking must SETTLE on release even when tracking
    /// flips ON->OFF mid-drag (the program exits / issues CSI ?1000l). Before the
    /// fix the release was double-gated out (the M6 settle branch required live
    /// tracking, and the generic `!was_reported` guard returned first), so
    /// `selecting` stayed stuck and every subsequent bare hover grew the selection.
    #[test]
    fn release_settles_local_selection_after_tracking_flips_off_mid_drag() {
        use crate::{App, WindowId, term_lock};
        use winit::event::{ElementState, MouseButton as WinitMouseButton};
        use winit::keyboard::ModifiersState;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        // Foreground app turns mouse tracking ON.
        term_lock(&term).process(b"\x1b[?1000h");
        assert!(term_lock(&term).mouse_tracking_enabled());
        // Hold Option/Alt so the left press takes the M6 BYPASS (a local selection
        // with NO reported bit) despite the app tracking the mouse.
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::ALT;
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows.get(&wid).unwrap().selecting,
            "the Option-bypass press starts a local selection"
        );
        // Tracking flips OFF mid-gesture (the program exited / sent CSI ?1000l).
        term_lock(&term).process(b"\x1b[?1000l");
        assert!(!term_lock(&term).mouse_tracking_enabled());
        // The release must settle the in-flight LOCAL selection regardless of the
        // live tracking mode, and clear the Left reported bit.
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            !app.windows.get(&wid).unwrap().selecting,
            "release settles the selection (selecting cleared) even though tracking flipped off"
        );
        assert_eq!(
            app.windows.get(&wid).unwrap().reported_buttons & 1,
            0,
            "the Left (selection) reported bit is cleared on settle"
        );
    }
}
