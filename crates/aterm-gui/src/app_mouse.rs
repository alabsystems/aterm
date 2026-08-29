// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Pointer, selection, hover, and link handling: the mouse/cursor event handlers
//! plus the selection gesture state machine (click streaks, word/line/block
//! select, drag, copy), pane-under-pointer focus, pixel→cell mapping, and the
//! scroll snap. A verbatim inherent-impl split of `App`.

use std::cell::Cell;
use std::time::Instant;

use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_types::mouse::WheelDir;
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta};
use winit::window::CursorIcon;

use aterm_core::terminal::{CustodyTransition, Terminal};

use crate::app_render::{pixel_to_term_cell, strip_col_for_pixel};
use crate::input::{InputEvent, Source};
use crate::{
    App, GestureOrigin, MULTI_CLICK_MS, WindowId, control, is_safe_url, pane, plain_url_at,
    term_lock,
};

/// PRESS CUSTODY — the mouse half of the custody RECORD: a user gesture that left a
/// selection behind, or one that deliberately took it away.
///
/// Anchored here rather than on [`App::finish_selection`] because this is the
/// function that RECORDS the variant, and because `finish_selection` already carries
/// `SelectionCustody`'s same-named `UserClear` anchor (the macro's generated const is
/// `__aterm_spec_refines_{fn}_{action}` with no machine name in it, so two machines'
/// same-named actions cannot share one function).
///
/// `kept` is the discriminator and it is WINDOW-side: `ws.sel_dragged` says whether
/// the pointer ever left the press cell, and nothing inside `TextSelection` carries
/// that fact. A recorder placed in the selection primitive could see the state change
/// but not the intent behind it — and "a press and release inside one cell" is
/// exactly the intent that makes a `clear()` legitimate rather than a bug.
///
/// Neither variant touches the viewport, which is the model's own claim about them:
/// `UserSelect` and `UserClear` assign `selection` and leave `offset` and `owner`
/// alone. Tier-1 drives the real
/// `begin_selection` → `drag_selection` → `finish_selection` gesture in
/// [`crate::press_custody_conformance`].
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "PressCustody",
        action = "UserSelect",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
#[cfg_attr(
    test,
    aterm_spec::refines(
        machine = "PressCustody",
        action = "UserClear",
        project = "aterm_gui::press_custody_conformance::project_press_custody"
    )
)]
pub(crate) fn note_selection_custody(t: &mut Terminal, kept: bool) {
    t.note_custody(if kept {
        CustodyTransition::UserSelect
    } else {
        CustodyTransition::UserClear
    });
}

/// Map a winit mouse button to the engine's [`aterm_types::mouse::MouseButton`]
/// for an [`InputEvent::MouseButton`].
///
/// The THUMB pair (audit I8) maps too: `Back`/`Forward` are XButton1/XButton2 on
/// Windows and `BTN_SIDE`/`BTN_EXTRA` on Linux, and they are xterm's buttons 8/9,
/// so a TUI that asked for mouse tracking can finally see them. They flow through
/// the SAME seam as the other three — every pre-dispatch consumer in
/// `on_mouse_input` (strip, dividers, selection, right-click, modals) is gated on
/// a specific button, so nothing local claims them and, with tracking OFF, the
/// `TrackingOff` consumer is Left-gated and they stay inert.
///
/// `Other(_)` stays `None` and the handler early-returns: an unmapped device
/// button has NO xterm code, and inventing one (or folding it onto button 8)
/// would send a TUI a report for a press it cannot name.
pub(crate) fn winit_mouse_button(b: WinitMouseButton) -> Option<aterm_types::mouse::MouseButton> {
    use aterm_types::mouse::MouseButton;
    match b {
        WinitMouseButton::Left => Some(MouseButton::Left),
        WinitMouseButton::Middle => Some(MouseButton::Middle),
        WinitMouseButton::Right => Some(MouseButton::Right),
        WinitMouseButton::Back => Some(MouseButton::Back),
        WinitMouseButton::Forward => Some(MouseButton::Forward),
        WinitMouseButton::Other(_) => None,
    }
}

/// An in-flight DRAG-TO-REORDER on the in-grid tab strip: which STABLE tab is
/// held, and the strip column the gesture is anchored at. Stored per window in
/// [`crate::WindowState::strip_drag`]; see [`App::advance_strip_drag`] for the
/// rules.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct StripDrag {
    /// The tab the press grabbed. Identity, not index: a step relayouts the band,
    /// so the index the press saw names a different tab immediately afterwards.
    pub(crate) tab: crate::tab_model::TabId,
    /// The strip column the drag is anchored at — the press column, re-anchored
    /// to the pointer after every step it takes. A motion that has not left this
    /// column takes no step, so a press that never moved cannot reorder.
    pub(crate) origin_col: u16,
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

/// What a RIGHT press does at its pre-dispatch point in `on_mouse_input`
/// (audit I6). Pure so the decision table is unit-pinned; the side-effects
/// (copy/paste/report) stay in the handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RightPressPlan {
    /// The press landed on CHROME (the tab strip band): swallow it entirely —
    /// no copy, no paste, and crucially NO seam report. Before this arm the
    /// strip's gates were all Left-only, so a right press on a CHIP fell
    /// through to the seam and a tracking TUI received a press at whatever
    /// grid cell the clamp put under the strip — a bogus report for a click
    /// the user aimed at chrome (the audit's "reported at a bogus cell").
    Chrome,
    /// Tracking OFF, a selection exists: copy it to the system clipboard and
    /// clear it (conhost's QuickEdit half of the convention).
    Copy,
    /// Tracking OFF, no selection: paste the clipboard (the WT/conhost other
    /// half) — via `App::paste_clipboard_into`, NEVER a raw
    /// `self.input(…Paste…)`: the paste MUST pass `deliver_paste`'s
    /// pastejacking guard. (The Linux middle-click arm in `on_mouse_input`
    /// routes through the guard too, via `App::paste_primary_into` — the raw
    /// shape it once had is the audited defect; do not reintroduce it.)
    Paste,
    /// Fall through to the seam untouched: tracking is ON (the app owns the
    /// button — press/release report exactly as before this gesture existed),
    /// or the gesture is configured `off`.
    Seam,
}

/// Resolve a RIGHT press against the conhost/Windows-Terminal convention
/// (`right_click = "copy_paste"`): copy-if-selection-else-paste, tracking-OFF
/// only, never on chrome. THE ORDER IS THE CONTRACT: chrome wins first (the
/// bogus-cell fix applies even with the gesture `off` — a right press on the
/// strip is a press on chrome no matter what the grid gesture is configured
/// to); then a tracking app keeps the button (parity with the pre-gesture
/// behaviour, and the M6 Option-bypass already covers "select over a tracking
/// app" for the LEFT button); only then does the copy/paste split apply.
pub(crate) fn right_press_plan(
    gesture_on: bool,
    over_strip: bool,
    tracking: bool,
    has_selection: bool,
) -> RightPressPlan {
    if over_strip {
        return RightPressPlan::Chrome;
    }
    if !gesture_on || tracking {
        return RightPressPlan::Seam;
    }
    if has_selection {
        RightPressPlan::Copy
    } else {
        RightPressPlan::Paste
    }
}

/// How far outside the pet's drawn body (frame px, each side) a click still
/// counts as petting. A cat is not a checkbox: the target moves, so a few
/// pixels of grace keeps an honest aim from sliding off a paw mid-walk.
pub(crate) const PET_HIT_SLOP_PX: i32 = 4;

/// How far outside Robi's drawn body (frame px, each side) a click still
/// counts as a dismissal. The pet's grace, for the pet's reason: a strolling
/// robot is not a checkbox.
pub(crate) const ROBI_HIT_SLOP_PX: i32 = 4;

/// Whether a pointer at `(x, y)` (frame px) lands on the pet's drawn body
/// `rect` (`(x0, x1, y0, y1)`, right/bottom exclusive), padded by `slop` on
/// every side. Pure — the petting seam's hit test, unit-testable without a
/// window.
pub(crate) fn pet_rect_hit(rect: (i32, i32, i32, i32), x: f64, y: f64, slop: i32) -> bool {
    let (x0, x1, y0, y1) = rect;
    x >= f64::from(x0.saturating_sub(slop))
        && x < f64::from(x1.saturating_add(slop))
        && y >= f64::from(y0.saturating_sub(slop))
        && y < f64::from(y1.saturating_add(slop))
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

/// The window geometry EVERY pointer↔cell mapping needs, derived once.
///
/// All four fields are pure reads of window `wid`'s settled metrics, but `cell`
/// goes through `Backend::cell_geometry`, which for a variable primary face
/// re-parses the whole font file and re-applies its variation axes on each call
/// (`aterm_render`'s own comment there notes it runs ≥2×/frame plus once per
/// mouse-motion event). Every helper used to derive its own copy —
/// `pixel_to_cell` → `window_to_frame` → `frame_origin` alone did it twice — so
/// one `CursorMoved` re-derived identical geometry 4–8×.
///
/// Nothing can change it inside one event handler, so the motion path derives it
/// once and threads this `Copy` snapshot through the `*_with` seams. The
/// zero-argument wrappers stay for the ~80 cold call sites, and each of them now
/// derives exactly once as well.
#[derive(Clone, Copy)]
struct PointerGeometry {
    /// `(cell_w, cell_h)` in device px.
    cell: (usize, usize),
    pad: usize,
    pad_top: usize,
    head: usize,
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
        let transform = self.overlay_coordinate_transform(wid)?;
        let (frame_x, frame_y) = self.window_to_frame(wid, x, y);
        let local_x = (frame_x - transform.origin_x) / f64::from(transform.scale);
        let local_y = (frame_y - transform.origin_y) / f64::from(transform.scale);
        crate::palette::palette_row_hit(palette, &transform.geom, local_x as f32, local_y as f32)
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
        if self.active_tab_is_single_focused_pane(wid) {
            return (0, 0);
        }
        self.active_visible_leaf_plan(wid)
            // `LogicalRect` is `Copy`, so lift the four scalars out instead of
            // cloning the whole `VisibleLeaf` (and with it another `SplitPath`
            // `Vec`) just to read them.
            .and_then(|plan| plan.leaf(plan.focused).map(|leaf| leaf.rect))
            .map_or((0, 0), |rect| {
                (
                    rect.origin.y.round().max(0.0) as u16,
                    rect.origin.x.round().max(0.0) as u16,
                )
            })
    }

    /// `true` when window `wid`'s active tab is one single, focused pane — i.e.
    /// when [`Self::active_visible_leaf_plan`] would allocate a plan whose answer
    /// is already known.
    ///
    /// It is known exactly: `Tab::visible_plan`'s zoom branch requires
    /// `root.len() > 1`, so a one-leaf tree always takes `plan_into`, which gives
    /// that sole leaf the sanitized bounds verbatim — `(0, 0, cols.max(1),
    /// rows.max(1))` — and `contains(focus)` on a one-leaf tree means that leaf IS
    /// `plan.focused`, so `plan.leaf(plan.focused)` cannot miss and fall through to
    /// the no-plan default. Both `len` and `contains` are allocation-free walks
    /// over a single node (same precedent as `active_tab_contains_native`).
    fn active_tab_is_single_focused_pane(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() == 1 && tab.root.contains(tab.focus))
    }

    /// `plan.leaves.len() > 1` for window `wid`'s active tab, without building the
    /// plan: `Tab::visible_plan` plans every leaf except when zoom collapses a
    /// multi-leaf tab to its focused one, so the two forms agree on every input.
    fn active_tab_has_multiple_visible_panes(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active())
            .is_some_and(|tab| tab.root.len() > 1 && !tab.zoomed)
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
        if self.active_tab_is_single_focused_pane(wid) {
            // Exactly what the plan yields for a lone pane: its rect IS the
            // sanitized window bounds. The `.max(1)` is load-bearing — the bounds
            // are built from `rows.max(1)`/`cols.max(1)`, so a bare `(ws.rows,
            // ws.cols)` would disagree with today at `rows == 0`. The no-active-tab
            // default below stays unmaxed; that is a different case.
            return (0, 0, ws.rows.max(1), ws.cols.max(1));
        }
        self.active_visible_leaf_plan(wid)
            .and_then(|plan| plan.leaf(plan.focused).map(|leaf| leaf.rect))
            .map_or((0, 0, ws.rows, ws.cols), |rect| {
                (
                    rect.origin.y.round().max(0.0) as u16,
                    rect.origin.x.round().max(0.0) as u16,
                    (rect.size.height.round() as u16).max(1),
                    (rect.size.width.round() as u16).max(1),
                )
            })
    }

    /// `true` when the pointer's last WINDOW cell lies INSIDE the focused pane's
    /// rect — i.e. when `last_mouse_cell` is a translation of where the pointer
    /// is rather than a clamp of where it is not.
    ///
    /// [`Self::focused_pane_rect`] is used to CLAMP the pane-local cell into the
    /// focused pane, so a gesture that crosses a divider keeps addressing the
    /// pane it started in. The price of that clamp is that over a SIBLING pane
    /// the pane-local cell names an edge cell of the focused pane, columns or
    /// rows from the pointer. Routing a gesture wants the clamp; anything making
    /// a claim ABOUT WHERE THE POINTER IS has to ask this first. `true` on the
    /// single-pane path, where the focused rect is the whole grid and the cell
    /// mapping is already clamped into it.
    pub(crate) fn pointer_is_inside_focused_pane(&self, wid: WindowId) -> bool {
        let Some((wr, wc)) = self.windows.get(&wid).map(|ws| ws.last_mouse_window_cell) else {
            return false;
        };
        let (ro, co, prows, pcols) = self.focused_pane_rect(wid);
        (ro..ro.saturating_add(prows)).contains(&wr) && (co..co.saturating_add(pcols)).contains(&wc)
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

    /// SELECTION CUSTODY: settle an in-flight TEXT SELECTION drag only, for the
    /// paths that are about to drop `selecting`/`gesture` — window blur and a
    /// tab/pane switch.
    ///
    /// Those paths used to drop the gesture WITHOUT finishing it, leaving a
    /// zombie `InProgress` selection: still painted, but `extend_selection`
    /// refuses anything outside `Complete` and copy-on-select never fired, so the
    /// highlight on screen could neither be extended nor copied. Completing it
    /// turns it into a selection the user can actually act on.
    ///
    /// Deliberately NARROWER than [`Self::settle_pointer_drags`] in two ways,
    /// both load-bearing:
    ///
    /// - copy-on-select and PRIMARY are SUPPRESSED. Losing the window is not the
    ///   user lifting the button; auto-copying then would overwrite the clipboard
    ///   from an event the user did not cause.
    /// - the physical/mouse-tracking mirrors (`held_mouse_button`,
    ///   `reported_buttons`) are left ALONE. Unlike a modal — which swallows the
    ///   release outright — a focus flicker with the button still held returns
    ///   the eventual release to this window, where it must still pair and still
    ///   report to a mouse-tracking app. The divider drag is likewise left to the
    ///   caller, which drops it with the rest of its gesture state.
    pub(crate) fn settle_selection_gesture(&mut self, wid: WindowId) {
        if self.windows.get(&wid).is_some_and(|ws| ws.selecting) {
            self.finish_selection(wid, true);
        }
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

    /// Cmd-C: copy the selected text to the system clipboard (`pbcopy`).
    /// Returns whether anything was copied; the selection is NOT cleared (so a
    /// highlight survives the copy, and repeated copies work).
    ///
    /// Window-scoped. EVERY path that has an originating window MUST use this
    /// form rather than [`Self::copy_selection`]: a press is routed by its own
    /// `wid`, which can differ from `frontmost_window`, and a copy has to read the
    /// terminal the keystroke was addressed to. That is the hardcoded ⌘-C arm, the
    /// `Action::Copy` keybinding arm of `dispatch_action` (which is `ctrl+shift+c`
    /// / `ctrl+insert` — the PRIMARY copy chord off macOS), and the native-view
    /// path's `copy_native_selection(wid)`.
    ///
    /// Only `MenuAction::Copy` keeps the frontmost-window form, because a menu
    /// item genuinely carries no window of its own.
    pub(crate) fn copy_selection_in(&self, wid: WindowId) -> bool {
        let Some(text) = self.window_selection_text(wid) else {
            return false;
        };
        !text.is_empty() && control::pbcopy(&text)
    }

    /// The text of window `wid`'s live selection, resolved ACROSS PANES: the
    /// focused pane when it holds one, else the first visible pane in layout
    /// order that does.
    ///
    /// SELECTION CUSTODY: a selection in an unfocused split pane is alive, and
    /// since `push_pane_selection` it is also PAINTED, so refusing to copy it
    /// would leave the user looking at a highlight ⌘-C ignores. The two halves
    /// ship together deliberately: resolving the copy without the paint is the
    /// inverse hazard — copying text the user cannot see highlighted — which is
    /// why this landed with the projection and not before it.
    ///
    /// The focused pane keeps absolute priority, so a window whose focused pane
    /// has a selection behaves exactly as it always did; the fallback runs only
    /// where the focused pane holds NO selection at all — not merely where its
    /// selection resolves to empty text, which is a different and much larger set.
    pub(crate) fn window_selection_text(&self, wid: WindowId) -> Option<String> {
        // OWNERSHIP is decided by `has_selection()`, never by whether the selection
        // RESOLVES to text. `selection_to_string_capped` returns `None` when the
        // resolved text is empty, and every row is trailing-trimmed — so a real,
        // PAINTED selection over blank or trailing-whitespace cells resolves to
        // `None`. Gating the early return on the text would let that fall through to
        // a sibling and hand back a pane the user never touched: a silent
        // wrong-copy, fired automatically on every drag because `copy_on_select`
        // defaults on. A pane that holds a selection answers for this window, even
        // when the honest answer is "nothing to copy".
        if let Some(terminal) = self.front_terminal(wid) {
            let term = term_lock(&terminal.term);
            if term.text_selection().has_selection() {
                return term.selection_to_string();
            }
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        for leaf in &plan.leaves {
            let Some(crate::tab_model::View::Terminal(view)) =
                self.view_store.get(leaf.view).copied()
            else {
                continue;
            };
            let Some(session) = self.pool.get(view.session) else {
                continue;
            };
            // Same rule for the siblings: the first pane that HOLDS a selection is
            // the answer, so the scan cannot skip past a whitespace-only highlight
            // to a further pane's text.
            let term = term_lock(&session.term);
            if term.text_selection().has_selection() {
                return term.selection_to_string();
            }
        }
        None
    }

    /// [`Self::copy_selection_in`] against the frontmost window — the menu and
    /// command-registry entry point, which carries no window of its own.
    pub(crate) fn copy_selection(&self) -> bool {
        self.frontmost_window
            .is_some_and(|wid| self.copy_selection_in(wid))
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
        // One derivation, shared with the nested `window_to_frame`: this used to
        // resolve the cell geometry twice for the same answer.
        self.pixel_to_cell_with(wid, self.pointer_geometry(wid), x, y)
    }

    /// Window `wid`'s [`PointerGeometry`]. The one place the four reads happen.
    fn pointer_geometry(&self, wid: WindowId) -> PointerGeometry {
        PointerGeometry {
            cell: self.win_cell_size(wid),
            pad: self.win_pad(wid),
            pad_top: self.win_pad_top(wid),
            head: self.win_head(wid),
        }
    }

    /// [`Self::pixel_to_cell`] against geometry the caller already derived.
    fn pixel_to_cell_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> (u16, u16) {
        let (cw, ch) = geom.cell;
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        let (x, y) = self.window_to_frame_with(wid, geom, x, y);
        pixel_to_term_cell(
            x,
            y,
            cw,
            ch,
            rows,
            cols,
            self.chrome_rows(),
            geom.pad,
            geom.pad_top,
            geom.head,
        )
    }

    /// Refresh the remembered cells under the pointer — the raw WINDOW cell
    /// (click-to-focus hit-testing) and the FOCUSED-PANE-LOCAL cell (PTY mouse
    /// reports), the latter CLAMPED to the pane's own grid so a drag crossing a
    /// divider stays inside the focused pane's sub-rect. Returns
    /// `((window_row, window_col), (pane_row, pane_col))`. Shared by the normal
    /// motion path and the About-modal gate: the modal swallows motion, but must
    /// not leave a STALE cell for the first click after a keyboard close.
    fn refresh_mouse_cell(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> ((u16, u16), (u16, u16)) {
        let (row, col) = self.pixel_to_cell_with(wid, geom, x, y);
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
        // Both bail-outs below are geometry-free, so answer them BEFORE paying for
        // `pointer_geometry` — an unattached window keeps its `(0, 0)` for free,
        // exactly as when this function derived the geometry itself.
        if !self.windows.get(&wid).is_some_and(|ws| ws.win_px.is_some()) {
            return (0, 0);
        }
        self.frame_origin_with(wid, self.pointer_geometry(wid))
    }

    /// [`Self::frame_origin`] against geometry the caller already derived.
    fn frame_origin_with(&self, wid: WindowId, geom: PointerGeometry) -> (i64, i64) {
        let Some(ws) = self.windows.get(&wid) else {
            return (0, 0);
        };
        let Some(size) = ws.win_px else {
            return (0, 0);
        };
        let (cw, ch) = geom.cell;
        let (pad, pad_top, head) = (geom.pad, geom.pad_top, geom.head);
        let composed_rows = usize::from(ws.rows).saturating_add(usize::from(self.chrome_rows()));
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
            // Vertical placement is the platform policy (top-pinned on Linux):
            // the SAME `band_offset_y` the presenters place the frame with, so
            // pointer geometry and pixels can't disagree.
            aterm_render::band_offset_y(size.height as usize, frame_h),
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

    /// [`Self::window_to_frame`] against geometry the caller already derived.
    fn window_to_frame_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> (f64, f64) {
        let (ox, oy) = self.frame_origin_with(wid, geom);
        ((x - ox as f64).max(0.0), (y - oy as f64).max(0.0))
    }

    /// If pixel position `(x, y)` lands in window `wid`'s tab-strip region (the top
    /// `tab_strip_rows` pixel rows), return its strip COLUMN; otherwise `None` (the
    /// click is in the terminal region and maps to a cell as usual). Always `None`
    /// when the strip is disabled. Used by the mouse handlers to intercept strip
    /// clicks BEFORE the focused-pane cell mapping.
    pub(crate) fn strip_col_at(&self, wid: WindowId, x: f64, y: f64) -> Option<u16> {
        // Keep the disabled-strip bail BEFORE the derivation (it is the macOS
        // default, and this used to be answered without touching the font).
        if !self.tab_strip_enabled() {
            return None;
        }
        self.strip_col_at_with(wid, self.pointer_geometry(wid), x, y)
    }

    /// If pixel position `(x, y)` lands on one of window `wid`'s STATUS BAR rows
    /// (the chrome rows directly below the tab strip), which bar's lane. `None`
    /// when no bar is up or the point is elsewhere. The bars are chrome: a press
    /// on one opens the lane's deliberate surface (Settings ▸ Packages /
    /// Software Update) and never reaches the terminal underneath — there is no
    /// terminal underneath, the grid starts below them.
    pub(crate) fn status_bar_lane_at(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<crate::status_bars::Lane> {
        if self.status_bar_rows == 0 {
            return None;
        }
        let geom = self.pointer_geometry(wid);
        let (_, ch) = geom.cell;
        let (_, y) = self.window_to_frame_with(wid, geom, x, y);
        let gy = (y as usize).saturating_sub(geom.pad_top + geom.head);
        let strip_px = usize::from(self.tab_strip_rows) * ch.max(1);
        let bars_px = usize::from(self.status_bar_rows) * ch.max(1);
        if gy < strip_px || gy >= strip_px + bars_px {
            return None;
        }
        self.status_bars.lane_at((gy - strip_px) / ch.max(1))
    }

    /// [`Self::strip_col_at`] against geometry the caller already derived.
    fn strip_col_at_with(
        &self,
        wid: WindowId,
        geom: PointerGeometry,
        x: f64,
        y: f64,
    ) -> Option<u16> {
        if !self.tab_strip_enabled() {
            return None;
        }
        let (cw, ch) = geom.cell;
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols);
        let (x, y) = self.window_to_frame_with(wid, geom, x, y);
        strip_col_for_pixel(
            x,
            y,
            cw,
            ch,
            cols,
            self.tab_strip_rows,
            geom.pad,
            geom.pad_top,
            geom.head,
        )
    }

    /// C5 — window pixel `(x, y)` as a FRAME cell `(row, col)`: the coordinate
    /// system the tab-strip splice (and therefore [`crate::tab_menu::MenuRect`])
    /// works in, where row 0 is the strip's first row.
    ///
    /// Deliberately UNCLAMPED, for exactly the reason
    /// [`Self::find_bar_pixel_hit`] is: `pixel_to_cell` snaps a point in the
    /// surrounding pad onto the nearest real cell, so a press in the top border
    /// or past the right edge would resolve to a menu row it never touched. A
    /// point outside the grid interior returns `None` and the caller treats it
    /// as "off the card".
    fn frame_cell_at(&self, wid: WindowId, x: f64, y: f64) -> Option<(usize, usize)> {
        let (cw, ch) = self.win_cell_size(wid);
        let (cw, ch) = (cw.max(1), ch.max(1));
        let pad = self.win_pad(wid);
        let top = self.win_pad_top(wid) + self.win_head(wid);
        let cols = self.windows.get(&wid).map_or(0, |ws| ws.cols) as usize;
        let (ox, oy) = self.frame_origin(wid);
        let (fx, fy) = (x - ox as f64, y - oy as f64);
        if fy < top as f64 || fx < pad as f64 {
            return None;
        }
        let row = ((fy as usize).saturating_sub(top)) / ch;
        let col = ((fx as usize).saturating_sub(pad)) / cw;
        (col < cols).then_some((row, col))
    }

    /// C5 — the open menu's recorded rect plus the frame cell under `(x, y)`,
    /// when this window has a menu that has actually been PAINTED. The rect is
    /// the one the painter recorded, never a re-derivation, so a click can never
    /// resolve against a placement the glass does not show.
    fn tab_menu_probe(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(crate::tab_menu::MenuRect, usize, usize)> {
        let rect = self.windows.get(&wid)?.tab_menu_rect?;
        let (row, col) = self.frame_cell_at(wid, x, y)?;
        Some((rect, row, col))
    }

    /// C5 — the LIVE action row under `(x, y)`, or `None` on a header, a
    /// separator, a disabled row, a border, or anywhere off the card.
    fn tab_menu_action_at(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let (rect, row, col) = self.tab_menu_probe(wid, x, y)?;
        let ws = self.windows.get(&wid)?;
        let menu = ws.tab_menu.as_ref()?;
        crate::tab_menu::action_at(rect, &menu.entries, row, col)
    }

    /// C5 — whether `(x, y)` is anywhere ON the open card, border included. A
    /// press there is swallowed even when it activates nothing: the card is a
    /// surface, and a click on its frame must not reach the terminal beneath.
    fn tab_menu_contains(&self, wid: WindowId, x: f64, y: f64) -> bool {
        self.tab_menu_probe(wid, x, y)
            .is_some_and(|(rect, row, col)| rect.contains(row, col))
    }

    /// C5 — move the open menu's highlight to the row under the pointer (or
    /// clear it off the card). Change-gated: only a row CHANGE requests a
    /// repaint, so sweeping within one row costs nothing, exactly like
    /// [`Self::track_strip_hover`].
    fn track_tab_menu_hover(&mut self, wid: WindowId, x: f64, y: f64) {
        let hovered = self.tab_menu_action_at(wid, x, y);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let Some(menu) = ws.tab_menu.as_mut() else {
            return;
        };
        // A KEYBOARD-opened menu keeps the row its opener lit until the pointer
        // actually enters the card: a hand brushing the mouse must not silently
        // unselect the row a ⇧F10 user is about to press ↵ on. The first hover
        // ON a row hands ownership of the highlight to the pointer for good.
        if hovered.is_none() && menu.keyboard {
            return;
        }
        if hovered.is_some() {
            menu.keyboard = false;
        }
        if menu.highlight == hovered {
            return;
        }
        menu.highlight = hovered;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// PETTING THE PET (wave 1): if the last pointer position lands on the
    /// pet's drawn body (the rect the redraw stashed post-tick, padded by
    /// [`PET_HIT_SLOP_PX`]), stroke the cat and CONSUME the press. Returns
    /// whether it did — the caller stops routing on `true`.
    ///
    /// POLICY: chrome wins, like the tab strip. The pet is host chrome you
    /// can touch, not a cell you can select under: a press that pets never
    /// starts a selection and is never encoded for a mouse-tracking app
    /// (press/release stay symmetric — the swallowed press sets no reported
    /// bit, so the matching release is dropped by the orphan-release guard).
    ///
    /// KNOWN CAVEAT, accepted: `last_cursor_px` can be STALE on the first
    /// click after a tab switch (no `CursorMoved` has arrived on the new
    /// layout yet), so that one click is judged against where the pointer
    /// last was. The rect itself is fresh per frame.
    fn pet_press_at(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(rect) = ws.pet_hit_rect else {
            return false;
        };
        let (px, py) = ws.last_cursor_px;
        let (fx, fy) = self.window_to_frame(wid, px, py);
        if !pet_rect_hit(rect, fx, fy, PET_HIT_SLOP_PX) {
            return false;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            // Latch only (`kitty_pet::note_petted` — note, never act); the
            // redraw consumes it on the ground: a purr-flavored hold, a
            // heart per queued pet, and contentment toward the real purr.
            ws.cursor_pet.note_petted(Instant::now());
            // The pet may be settled with the frame lane released — the
            // latch re-arms `needs_frames`, but only a tick reads it, so
            // ask for the frame that runs one.
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
        }
        true
    }

    /// DISMISSING ROBI: if the last pointer position lands on Robi's drawn
    /// body (the rect the redraw stashed post-tick, padded by
    /// [`ROBI_HIT_SLOP_PX`]), send him away and CONSUME the press. Returns
    /// whether it did — the caller stops routing on `true`.
    ///
    /// "Away" means CONFIG-OFF: the press writes `robi = false` through the
    /// same versioned, compare-and-swap settings lane the Settings toggle
    /// uses ([`Self::queue_control_settings_field`]), so `aterm.toml` records
    /// the choice and he stays gone until the user re-enables `robi` in
    /// Settings. The live policy is deliberately NOT flipped here — the
    /// Serious Mode discipline: only the durable completion replaces
    /// `self.config`. What retires him NOW is the pending-dismissal latch
    /// (`App::robi_dismissal`), armed here and consulted by the render gate:
    /// every window retires him on its very next frame, and while the latch
    /// holds, the closed gate keeps `robi_hit_rect` cleared — the STRUCTURAL
    /// guarantee that a double-click cannot queue a second write (the spent
    /// rect alone never was one: any animating frame between the two presses
    /// re-stashed it). Stopping the show optimistically WITHOUT the latch
    /// would be worse, not better: the gate still reads the old config on
    /// the next frame and would re-birth him for one awkward fade-in.
    ///
    /// Nothing about the write is fire-and-forget: a synchronous refusal (no
    /// event-loop proxy, no persistence worker) is surfaced immediately as a
    /// config-notice banner with no latch armed, and the held reply receiver
    /// is settled by [`Self::poll_robi_dismissal`] — an async failure (an
    /// OCC conflict with an external `aterm.toml` edit, a rejection, an
    /// indeterminate persist) banners the same way and RELEASES the latch,
    /// so Robi walks back on: visible proof the click did not stick, with
    /// the banner saying why.
    ///
    /// A press on his TIP BUBBLE is not a dismissal: the bubble is the
    /// app-global transient notice (`TransientNotice::robi_tip`), and
    /// `notice_click` consumes a press on any visible card EARLIER in
    /// `on_mouse_input`'s chain — an ordering this seam depends on, pinned
    /// by `a_press_on_robis_tip_bubble_dismisses_the_bubble_not_the_robot`.
    ///
    /// Same chrome-wins policy and stale-`last_cursor_px` caveat as
    /// [`Self::pet_press_at`].
    fn robi_press_at(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let Some(rect) = ws.robi_hit_rect else {
            return false;
        };
        let (px, py) = ws.last_cursor_px;
        let (fx, fy) = self.window_to_frame(wid, px, py);
        if !pet_rect_hit(rect, fx, fy, ROBI_HIT_SLOP_PX) {
            return false;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.robi_hit_rect = None;
        }
        let (reply, outcome) = std::sync::mpsc::channel();
        self.queue_control_settings_field(
            crate::prefs::EDIT_ROBI.to_string(),
            Some("false".to_string()),
            reply,
        );
        match outcome.try_recv() {
            // Synchronous refusal: nothing is in flight, so there is no
            // latch to arm — he stays on glass and the banner says why.
            Ok(Err(error)) => {
                self.surface_native_config_lane_error(format!("Robi was not dismissed: {error}"));
            }
            // Already durable (never in practice — the lane is async): skip
            // straight to waiting for the config generation.
            Ok(Ok(_)) => {
                self.robi_dismissal = Some(crate::RobiDismissal::AwaitingConfig);
                self.request_redraw_all_windows();
            }
            // Queued: arm the latch, HOLDING the reply receiver so the
            // completion always has a reader, and repaint — the render gate
            // retires him on the NEXT frame in every window.
            Err(_) => {
                self.robi_dismissal = Some(crate::RobiDismissal::InFlight(outcome));
                self.request_redraw_all_windows();
            }
        }
        true
    }

    /// Settle ROBI's in-flight click-dismissal (`App::robi_dismissal`, armed
    /// by [`Self::robi_press_at`]). Called on every COMPOSED frame — before
    /// any route reads the render gate; a redraw that early-outs without
    /// drawing polls nothing — and again right at the settings lane's
    /// completion publish (`publish_native_config_origin` hooks the `robi`
    /// key), so neither branch waits on a lucky frame:
    ///
    /// * a FAILURE reply — OCC conflict, rejection, indeterminate persist —
    ///   releases the latch and banners through the lane's own surfacing
    ///   seam ([`Self::surface_native_config_lane_error`]): Robi walks back
    ///   on, and the click that did nothing is never silent;
    /// * a SUCCESS reply holds the latch (`AwaitingConfig`) until the new
    ///   generation replaces `self.config` — from then on `robi = false` is
    ///   the config itself and the latch is redundant, so it releases rather
    ///   than linger to override a later Settings re-enable.
    pub(crate) fn poll_robi_dismissal(&mut self) {
        if let Some(crate::RobiDismissal::InFlight(outcome)) = &self.robi_dismissal {
            match outcome.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Ok(Ok(_)) => self.robi_dismissal = Some(crate::RobiDismissal::AwaitingConfig),
                Ok(Err(error)) => {
                    self.robi_dismissal = None;
                    self.surface_native_config_lane_error(format!(
                        "Robi was not dismissed: {error}"
                    ));
                }
                // The lane dropped the request without ever replying — treat
                // it as the failure it is rather than latching him off on a
                // write nobody performed.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.robi_dismissal = None;
                    self.surface_native_config_lane_error(
                        "Robi was not dismissed: the settings lane dropped the request".to_string(),
                    );
                }
            }
        }
        if self.robi_dismissal.is_some() && !self.config.robi_or_default() {
            self.robi_dismissal = None;
        }
    }

    /// Track which in-grid tab-strip tab the pointer is over, so the `✕` can be a
    /// HOVER-ONLY affordance there exactly as it is on the native macOS strip. Runs
    /// on every `CursorMoved` — including motion that later paths consume — because
    /// leaving the strip must clear the reveal just as surely as entering it sets it.
    ///
    /// Cheap and change-gated: a no-op when the in-grid strip is disabled (the
    /// default), and it only requests a redraw when the hovered TAB changes, so
    /// sweeping the pointer across one tab costs nothing. The redraw is what
    /// re-runs `splice_tab_strip_with`, whose cache key carries the hover.
    fn track_strip_hover(&mut self, wid: WindowId, geom: PointerGeometry, x: f64, y: f64) {
        if !self.tab_strip_enabled() {
            return;
        }
        // DRAG-TO-REORDER rides the same per-motion hook: the pointer has moved,
        // and if a chip is held that motion may be a step. Runs BEFORE the hover
        // below because the hover is change-gated and returns early — a reorder
        // that left the pointer over the same INDEX must still repaint, which the
        // step does for itself.
        self.advance_strip_drag(wid, geom, x, y);
        // ONE hit test, TWO hover states. The `+` is not a tab, so an
        // `Option<usize>` cannot carry it — and for one release that meant the
        // strip's primary button had no pointer feedback at all. Resolved from
        // the SAME hit so the two can never both be lit (a column belongs to one
        // segment) and so the pointer leaving one clears the other.
        let hit = self.strip_col_at_with(wid, geom, x, y).and_then(|col| {
            let segs = self
                .windows
                .get(&wid)
                .map(|ws| ws.tab_segments.as_slice())?;
            crate::tab_bar::hit_test(segs, col)
        });
        let hovered = match hit {
            // The connector (status-mark cell) is part of its chip, so
            // hovering it must not drop the chip's hover state — that cell
            // hovered as Select before design §3.1 [v5] made it a hit.
            Some(
                crate::tab_bar::TabHit::Select(index) | crate::tab_bar::TabHit::Connector(index),
            ) => Some(index),
            // The `+` and the `↻` are not tabs; the pointer is in the strip but
            // on no tab, so nothing reveals a `✕`.
            _ => None,
        };
        let on_new_tab = matches!(hit, Some(crate::tab_bar::TabHit::NewTab));
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.strip_hover == hovered && ws.strip_hover_new_tab == on_new_tab {
            return;
        }
        ws.strip_hover = hovered;
        ws.strip_hover_new_tab = on_new_tab;
        if let Some(window) = &ws.os_window {
            window.request_redraw();
        }
    }

    /// THE POINTER LEFT THE WINDOW — clear the strip's hover state.
    ///
    /// [`Self::track_strip_hover`] is driven by `CursorMoved`, and a pointer that
    /// leaves the window stops sending those: whatever was lit at the last motion
    /// INSIDE stays lit while the pointer is in another app entirely. Measured on
    /// glass (2026-08-25, windowed, three tabs, font_px 13): parking the pointer on
    /// the `+` washed its card to `hover_bg`, and moving the pointer clean off the
    /// window left that wash on the surface — a control advertising a pointer that
    /// is not there, and a `✕` offering itself on a chip nobody is pointing at.
    ///
    /// One clear for BOTH hover states, and change-gated like the tracker: a leave
    /// with nothing lit costs nothing, and the redraw is what re-runs
    /// `splice_tab_strip_with` (whose cache key carries both flags).
    pub(crate) fn on_cursor_left(&mut self, wid: WindowId) {
        // The link caption is the same shape of stale claim: it names what a
        // click on the cell the pointer WAS over would open, and there is no
        // pointer over that cell any more.
        self.retire_link_target(wid);
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.strip_hover.is_none() && !ws.strip_hover_new_tab {
            return;
        }
        ws.strip_hover = None;
        ws.strip_hover_new_tab = false;
        if let Some(window) = &ws.os_window {
            window.request_redraw();
        }
    }

    /// DRAG A TAB TO REORDER IT — the in-grid strip's second pointer gesture,
    /// advanced from the same per-motion hook as the hover ([`Self::track_strip_hover`]).
    ///
    /// The gesture is armed by the press that SELECTS a chip (`handle_tab_strip_click`'s
    /// [`crate::tab_bar::TabHit::Select`] arm, which still switches on the press —
    /// Explorer, Edge and Windows Terminal all select first and reorder as the
    /// pointer moves) and disarmed by the release, by any other strip press
    /// ([`App::clear_strip_press`]) and by focus loss. Everything between is here.
    ///
    /// THE STEP RULE, and why it is a midpoint rather than a delta: the pointer
    /// steps the held tab one slot whenever it passes the MIDPOINT of the
    /// neighbouring segment, read out of the very `ws.tab_segments` the paint
    /// recorded — the painter-records / hit-test-reads discipline the rest of the
    /// strip already follows, so the chips can never swap somewhere the glass does
    /// not show them. Two properties fall out of that and are worth naming:
    ///
    /// * A PRESS THAT NEVER MOVED CANNOT REORDER. The press column lies inside the
    ///   held chip's own segment, and a neighbour's midpoint is strictly beyond
    ///   that segment's far edge, so no crossing exists until the pointer actually
    ///   travels. The `origin_col` guard states the same invariant explicitly
    ///   rather than leaving it to be re-derived (and it is re-anchored after each
    ///   step, so a settled pointer takes exactly one).
    /// * The step is SELF-STABILISING. `move_tab` keeps the same tab selected, so
    ///   after a step the pointer sits inside the moved chip's new share and the
    ///   next crossing needs fresh travel — including under the pressure layout,
    ///   where the active chip is the wide one and the geometry changes shape
    ///   around the move.
    ///
    /// Off the band (below the strip rows) the drag stays ARMED but takes no step:
    /// the chip neither tears out (explicitly out of scope — `detach_active_tab`
    /// exists for a later lane) nor snaps home, so sweeping back onto the strip
    /// resumes the same gesture.
    ///
    /// A single coalesced motion may cross more than one midpoint, so this steps
    /// until the pointer is over the held chip again, bounded by the tab count.
    fn advance_strip_drag(&mut self, wid: WindowId, geom: PointerGeometry, x: f64, y: f64) {
        let Some(drag) = self.windows.get(&wid).and_then(|ws| ws.strip_drag) else {
            return;
        };
        // A palette opened by KEYBOARD under a held button is still modal even
        // though the pointer was never claimed: stepping the held chip under it
        // would reorder the strip out of sight. Read-only on purpose —
        // `palette_claims_pointer` (the motion seam's gate) owns the hover/press
        // side effects, and the release still lands in `on_mouse_input`'s
        // hoisted disarm, so the gesture simply ends when the button lifts.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.palette().is_some())
        {
            return;
        }
        let Some(col) = self.strip_col_at_with(wid, geom, x, y) else {
            return;
        };
        if col == drag.origin_col {
            return;
        }
        let mut stepped = false;
        for _ in 0..self.windows.get(&wid).map_or(0, |ws| ws.tab_set.len()) {
            let Some((from, to)) = self.strip_drag_step(wid, drag.tab, col) else {
                break;
            };
            self.move_tab(wid, from, to);
            stepped = true;
        }
        if !stepped {
            return;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.strip_drag = Some(StripDrag {
                origin_col: col,
                ..drag
            });
            if let Some(window) = &ws.os_window {
                window.request_redraw();
            }
        }
    }

    /// One step of [`Self::advance_strip_drag`]: where the held `tab` currently
    /// sits and where a pointer at strip column `col` wants it, or `None` when the
    /// pointer has not passed a neighbour's midpoint (or the tab is gone, or it has
    /// no neighbour on that side — a lone tab always lands here).
    fn strip_drag_step(
        &self,
        wid: WindowId,
        tab: crate::tab_model::TabId,
        col: u16,
    ) -> Option<(usize, usize)> {
        let ws = self.windows.get(&wid)?;
        let from = ws.tab_set.tabs().iter().position(|t| t.id == tab)?;
        let segment = |index: usize| {
            ws.tab_segments
                .iter()
                .find(|seg| seg.kind == crate::tab_bar::TabHit::Select(index))
        };
        // Midpoint of a half-open `[start, end)` column range.
        let mid = |seg: &crate::tab_bar::TabSegment| {
            seg.start_col + seg.end_col.saturating_sub(seg.start_col) / 2
        };
        if let Some(next) = segment(from + 1)
            && col >= mid(next)
        {
            return Some((from, from + 1));
        }
        if let Some(prev) = from.checked_sub(1).and_then(segment)
            && col < mid(prev)
        {
            return Some((from, from - 1));
        }
        // PAST THE PAGE'S EDGE. The strip seats a PAGE of the tab list
        // (`tab_bar::strip_window`), so the held chip's neighbour is not always
        // ON the strip: at a page edge there is no next segment, and therefore
        // no midpoint for the rules above to pass. Stopping there would let a
        // paint decision bar a reorder — a tab could never be dragged out of
        // the page it started in, which the centred window used to hide by
        // travelling with the drag.
        //
        // The threshold is what the midpoint rule DEGENERATES to when the
        // neighbour is off-strip: the pointer has left the held chip on that
        // side. One such step turns the page, the held tab arrives on it, and
        // `advance_strip_drag`'s loop then settles the chip under the hand
        // exactly as it does for an ordinary step.
        let held = segment(from)?;
        let tabs = ws.tab_set.len();
        if from + 1 < tabs && segment(from + 1).is_none() && col >= held.end_col {
            return Some((from, from + 1));
        }
        if from > 0 && segment(from - 1).is_none() && col < held.start_col {
            return Some((from, from - 1));
        }
        None
    }

    /// A left click on the Cmd-F find panel. On the `Aa` (case) / `.*` (regex)
    /// indicators it fires the matching toggle — the SAME action as the ⌥⌘C / ⌥⌘R
    /// chords, so a mouse user can flip modes without the keyboard. Inside the query
    /// WELL it moves the caret to the character under the pointer, like any text field
    /// (past the text ⇒ the end). `row`/`col` are TERMINAL cell coordinates
    /// (`pixel_to_cell`), matching the geometry `splice_find_bar` recorded in
    /// [`crate::FindBarHit`].
    ///
    /// Returns `false` (unconsumed) when not searching or off the panel entirely, so
    /// ordinary terminal clicks fall through untouched. A click anywhere ELSE on the
    /// panel's band IS consumed: the band is chrome, and a drag started on it would
    /// otherwise select the panel's own text as if it were terminal output.
    pub(crate) fn find_bar_click(&mut self, wid: WindowId, row: u16, col: u16) -> bool {
        let Some(hit) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.find_bar_hit.clone())
        else {
            return false;
        };
        let row = row as usize;
        if !hit.band.contains(&row) {
            return false;
        }
        if row != hit.row {
            return true; // the panel's pad / hints rows: inert chrome, but consumed
        }
        let col = col as usize;
        if hit.case_cols.is_some_and(|r| r.contains(&col)) {
            self.search_toggle_case();
        } else if hit.regex_cols.is_some_and(|r| r.contains(&col)) {
            self.search_toggle_regex();
        } else if hit.field_cols.contains(&col) {
            // The well's first cell shows character `field_scroll`, so the click's
            // distance into it is the character index — the same mapping the paint used.
            self.search_click_caret_in(wid, hit.field_scroll + (col - hit.field_cols.start));
        }
        true
    }

    /// Pixel-precise gate for [`Self::find_bar_click`]: `true` only when window pixel
    /// `(x, y)` lands INSIDE the find bar's actual cell band, not merely on the row
    /// [`Self::pixel_to_cell`] would clamp it to. That clamp is the hazard: a click in
    /// the top `pad_top` border (or anywhere above the grid) snaps to row 0 —
    /// coincidentally the bar's row in the usual TOP placement — and a click in the
    /// bottom `pad` snaps to the last terminal row (the panel's last row when it floats
    /// to the bottom), so without this gate either would false-toggle a mode. The band is
    /// `[pad_top + head + frame_row*ch, + rows*ch)` on the frame's y-axis, where
    /// `frame_row = tab_strip_rows + hit.band.start` mirrors the strip prepend `splice_find_bar`
    /// applied; the x-check rejects clicks past the grid's right edge (which clamp onto
    /// the last column). `None` bar ⇒ `false`. Uses the SAME [`Self::window_to_frame`]
    /// band-strip as `pixel_to_cell`, so the two can't disagree in the settled state.
    pub(crate) fn find_bar_pixel_hit(&self, wid: WindowId, x: f64, y: f64) -> bool {
        let Some(band) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.find_bar_hit.as_ref())
            .map(|h| h.band.clone())
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
        let frame_row = usize::from(self.chrome_rows()) + band.start;
        let top = (pad_top + self.win_head(wid) + frame_row * ch) as f64;
        let bottom = top + (band.len() * ch) as f64;
        if fy < top || fy >= bottom || fx < pad as f64 {
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
        let Some(transform) = self.overlay_coordinate_transform(wid) else {
            return (-1.0, -1.0);
        };
        let (x, y) = self.window_to_frame(wid, x, y);
        (
            ((x - transform.origin_x) / f64::from(transform.scale)) as f32,
            ((y - transform.origin_y) / f64::from(transform.scale)) as f32,
        )
    }

    /// Resolve a raw window point only when it lies inside the exact Settings
    /// card rectangle the compositor publishes. Unlike `pixel_to_cell`, this is
    /// deliberately unclamped: the four remainder/padding bands surrounding the
    /// card are modal chrome, never aliases for sidebar rows or right-edge widgets.
    fn settings_card_point_if_inside(&self, wid: WindowId, x: f64, y: f64) -> Option<(f32, f32)> {
        let ws = self.windows.get(&wid)?;
        ws.settings()?;
        let transform = self.overlay_coordinate_transform(wid)?;
        let (origin_x, origin_y) = self.frame_origin(wid);
        let left = origin_x as f64 + transform.origin_x;
        let top = origin_y as f64 + transform.origin_y;
        let right =
            left + f64::from(ws.cols) * f64::from(transform.geom.cw) * f64::from(transform.scale);
        let bottom = top
            + ws.settings_panel_rows() as f64
                * f64::from(transform.geom.ch)
                * f64::from(transform.scale);
        (x >= left && x < right && y >= top && y < bottom).then_some((
            ((x - left) / f64::from(transform.scale)) as f32,
            ((y - top) / f64::from(transform.scale)) as f32,
        ))
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

    /// Resolve the OS cursor for the pointer's CURRENT location. Called on every
    /// `CursorMoved` (via the `_with` twin, which reuses the motion path's derived
    /// geometry) AND on every modifier change, so the link hand tracks a bare
    /// Cmd/Ctrl press. The modifier caller has no pointer event, which is why this
    /// must resolve BY LOCATION (`last_cursor_px` / `last_mouse_window_cell`): the
    /// old "link-or-default" swap was location-blind, and that was safe only while
    /// Default was the answer everywhere else — with the grid I-beam below it
    /// would paint an I-beam over the tab strip on a bare Ctrl tap.
    ///
    /// Resolution order: chrome (strip → the plain arrow) → split divider (the
    /// axis resize cursor, so the seam finally SAYS it is draggable before a drag
    /// begins) → link (the pointer hand, modifier held) → the grid itself: an
    /// I-beam, because the surface's primary gesture is text selection — reverted
    /// to the arrow while a VT mouse-tracking app owns the pointer (presses are
    /// REPORTED then, not selecting; the terminal is a control surface, not a
    /// text one, and the I-beam would promise the wrong gesture).
    pub(crate) fn update_hover_cursor(&mut self, wid: WindowId) {
        // Zero-argument wrapper for the cold caller (modifier changes); the
        // per-motion path threads its already-derived geometry instead, keeping
        // the one-derivation-per-event rule (`PointerGeometry`'s contract).
        // Bracketed like the motion path so a modifier tap that lands the
        // pointer on chrome (the strip arm below) retires a standing caption
        // AND repaints it away.
        let parked = self.park_link_target(wid);
        self.update_hover_cursor_with(wid, self.pointer_geometry(wid));
        self.settle_link_target(wid, parked);
    }

    /// TAKE the standing link hover without repainting, so the resolution about
    /// to run decides the whole answer. Every early return in
    /// [`Self::route_cursor_moved`] leaves it taken, which is the correct answer
    /// for all of them: chrome, a modal, or a native view owns the pointer, and
    /// a caption naming a grid link is then a caption about something no click
    /// can reach. Pairs with [`Self::settle_link_target`], which repaints.
    fn park_link_target(&mut self, wid: WindowId) -> Option<crate::link_target::LinkHover> {
        self.windows
            .get_mut(&wid)
            .and_then(|ws| ws.link_hover.take())
    }

    /// Close a [`Self::park_link_target`] bracket: repaint only if the pointer
    /// routing published something different from what it parked. A pointer
    /// resting still on one cell re-publishes the identical hover — the common
    /// case while a caption is up — and must not ask for a frame.
    fn settle_link_target(&mut self, wid: WindowId, parked: Option<crate::link_target::LinkHover>) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        if ws.link_hover == parked {
            return;
        }
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
    }

    /// Retire any standing caption, repainting it away. For the boundaries that
    /// end a hover outright rather than re-resolving it — the pointer leaving
    /// the window, the window losing focus.
    pub(crate) fn retire_link_target(&mut self, wid: WindowId) {
        let parked = self.park_link_target(wid);
        self.settle_link_target(wid, parked);
    }

    /// [`Self::update_hover_cursor`] against geometry the caller already derived.
    ///
    /// Publishes BOTH halves of one hover resolution: the OS cursor, and the
    /// cell whose destination the band discloses. They are written from the same
    /// probe on purpose — a cursor that promises a click and a caption that
    /// names its destination must never be able to disagree about which link is
    /// under the pointer.
    fn update_hover_cursor_with(&mut self, wid: WindowId, geom: PointerGeometry) {
        let resolved = self.resolve_hover_cursor(wid, geom);
        if let Some(ws) = self.windows.get_mut(&wid) {
            // A plain write: the park/settle bracket at the event boundary owns
            // the repaint decision, because only it can see what the value was
            // before the whole routing ran.
            ws.link_hover = resolved;
        }
    }

    /// Resolve the hover state, writing the cursor and RETURNING the cell whose
    /// hyperlink is to be disclosed (`None` from every arm where the grid is not
    /// the thing under the pointer).
    fn resolve_hover_cursor(
        &mut self,
        wid: WindowId,
        geom: PointerGeometry,
    ) -> Option<crate::link_target::LinkHover> {
        // While a modal overlay is open the pointer is ITS (the About dialog runs
        // its own link/I-beam cursor, the palette its row hand): a Cmd press must
        // not resolve a terminal link hidden UNDER the card and flip the cursor
        // out from under the modal.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.overlay.is_some())
        {
            return None;
        }
        // A native view owns its cursor through the motion path's native branch
        // (I-beam over text bodies, hand over controls): a modifier tap must not
        // stomp what that branch resolved — the old blind swap did exactly that,
        // clearing `native_text_cursor` on a Ctrl tap over an editor body.
        if self.active_native_view(wid).is_some() {
            return None;
        }
        // A held divider drag owns the resize cursor until release
        // (`begin_divider_drag` set it, `finish_divider_drag` restores): mid-drag
        // the pointer routinely leaves the seam, and re-resolving would flicker
        // the icon against the very gesture it advertises.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.divider_drag.is_some())
        {
            return None;
        }
        let (px, py) = self.windows.get(&wid).map(|ws| ws.last_cursor_px)?;
        // Chrome: over the tab strip the pointer is a button pointer, never an
        // I-beam (the strip is not selectable text) — the same answer the motion
        // path's own strip branch gives, repeated here for the pointer-eventless
        // caller. Same stale-first-event caveat as the pet seam: before the first
        // `CursorMoved`, `last_cursor_px` is the window origin.
        if self.strip_col_at_with(wid, geom, px, py).is_some() {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
            return None;
        }
        // Split dividers (hover half): the 1-cell seam is drawn, but until this
        // probe nothing SAID it was draggable — the resize cursor appeared only
        // once `begin_divider_drag` had already armed, i.e. you had to be
        // dragging to learn you could drag.
        if let Some(icon) = self.divider_cursor_under_pointer(wid) {
            self.set_hover_cursor(wid, icon, true, true);
            return None;
        }
        // The grid. ONE terminal lock answers everything this arm asks: the
        // hyperlink on the hovered cell, the plain-text URL run under it, and
        // the mouse mode.
        let mod_held = self
            .windows
            .get(&wid)
            .is_some_and(|ws| link_modifier_held(ws.mods));
        let (row, col) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| ws.last_mouse_cell);
        let window_row = self
            .windows
            .get(&wid)
            .map_or(0, |ws| ws.last_mouse_window_cell.0);
        // WHOSE CELL THIS IS — whether the cell about to be probed is the cell
        // the pointer is actually over. Two ways it is not, and a link read
        // through either would be a hand promising a click that opens nothing
        // and a caption naming a destination the pointer is not on, which is
        // the exact deception the band exists to end:
        //
        //   * a SIBLING PANE. The pane-local cell is CLAMPED into the focused
        //     pane, so over a sibling it names an edge cell of the other pane,
        //     and a press there focuses that sibling instead of opening
        //     anything.
        //   * a CHROME BAND, which COVERS the terminal row it took. A pointer
        //     resting on the find panel's well, on its toggles or on a banner
        //     is over chrome, and a press there is hit-tested against that
        //     chrome — never against the grid cell hidden beneath it. Asked of
        //     the frame's own register of claimed rows rather than of each band
        //     in turn, so a band added later needs no clause here.
        let cell_is_under_pointer = self.pointer_is_inside_focused_pane(wid)
            && !self.chrome_owns_terminal_row(wid, usize::from(window_row));
        let Some((term, session)) = self
            .front_terminal(wid)
            .map(|terminal| (terminal.term.clone(), terminal.session))
        else {
            self.set_hover_cursor(wid, CursorIcon::Text, false, true);
            return None;
        };
        let (linked, plain_url, tracking) = {
            let t = term_lock(&term);
            // Only WHETHER there is a link: the destination itself is re-read
            // from the grid at paint (`link_target`'s header), so copying the
            // string per motion event would buy a value that has to be
            // discarded anyway.
            //
            // VIEWPORT-keyed, like the plain-text probe beside it
            // (`render_row` resolves through `display_offset`): the pointer
            // stands on the row the frame DREW, and a screen-keyed probe
            // answers about a different line the moment the viewport is
            // scrolled back.
            let linked = cell_is_under_pointer && t.hyperlink_at_visible(row, col).is_some();
            // The plain-text probe renders a whole row, so it runs only for the
            // gesture that needs it. It also earns no caption: a detected URL
            // IS its own visible text, so there is nothing about its
            // destination that the screen is not already saying.
            let plain_url = mod_held
                && cell_is_under_pointer
                && !linked
                && plain_url_at(&t.render_row(row as usize), col as usize).is_some();
            (linked, plain_url, t.mouse_tracking_enabled())
        };
        if mod_held && (linked || plain_url) {
            self.set_hover_cursor(wid, CursorIcon::Pointer, true, false);
        } else if tracking {
            self.set_hover_cursor(wid, CursorIcon::Default, false, false);
        } else {
            self.set_hover_cursor(wid, CursorIcon::Text, false, true);
        }
        // DISCLOSE unconditionally, not only while the open modifier is down:
        // OSC 8 is the one link kind whose visible text and destination can be
        // unrelated, and the moment a person needs to know that is while they
        // are deciding whether the underlined word is worth a click — which is
        // before their hand reaches the modifier, not after.
        linked.then_some(crate::link_target::LinkHover {
            cell: (row, col),
            window_row,
            session,
        })
    }

    /// Write one resolved hover-cursor state, touching the OS cursor only on a
    /// CHANGE. The two existing `WindowState` bools double as the change
    /// detector, with one borrowed encoding: `(pointer, text) = (true, true)` —
    /// impossible for the plain states, which are mutually exclusive — means "a
    /// divider's resize cursor", and always re-issues `set_cursor`, because the
    /// pair cannot tell Col from Row and a hover sliding across a splits
    /// junction must not keep the wrong axis. Every existing reset site
    /// (`finish_divider_drag`, focus loss, the About boundary) clears both
    /// bools, so the encoding degrades to exactly the old semantics there.
    fn set_hover_cursor(&mut self, wid: WindowId, icon: CursorIcon, pointer: bool, text: bool) {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let divider = pointer && text;
        if !divider && ws.hover_pointer == pointer && ws.native_text_cursor == text {
            return;
        }
        ws.hover_pointer = pointer;
        ws.native_text_cursor = text;
        if let Some(w) = &ws.os_window {
            w.set_cursor(icon);
        }
    }

    /// The resize cursor for the pane divider under the pointer, if any — the
    /// HOVER half of the divider affordance: the same two probes
    /// [`Self::begin_divider_drag`] arms with (`pane::PaneTree::divider_at` for
    /// the terminal projection, the canonical plan's `divider_at` otherwise),
    /// minus the arming. `DividerHit.dir` exists for exactly this — its own doc
    /// says "Lets the GUI pick the resize cursor".
    fn divider_cursor_under_pointer(&self, wid: WindowId) -> Option<CursorIcon> {
        let ws = self.windows.get(&wid)?;
        let (wr, wc) = ws.last_mouse_window_cell;
        if let Some(tree) = self.active_tree(wid) {
            if tree.len() == 1 {
                return None;
            }
            let hit = tree.divider_at(wr, wc, ws.rows, ws.cols)?;
            return Some(match hit.dir {
                pane::SplitDir::Vertical => CursorIcon::ColResize,
                pane::SplitDir::Horizontal => CursorIcon::RowResize,
            });
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        let divider = plan.divider_at(crate::tab_model::LogicalPoint {
            x: f32::from(wc),
            y: f32::from(wr),
        })?;
        Some(match divider.axis {
            crate::tab_model::SplitAxis::Horizontal => CursorIcon::ColResize,
            crate::tab_model::SplitAxis::Vertical => CursorIcon::RowResize,
        })
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
        // ONE bracket around the WHOLE routing, rather than a retire-the-caption
        // line in each of the dozen branches below that return before the grid's
        // hover resolution runs. A branch that never publishes has answered
        // "nothing is under the pointer that a click could open", and that is
        // exactly what the parked-and-not-republished caption means — so the two
        // cannot drift as branches are added.
        let parked = self.park_link_target(wid);
        self.route_cursor_moved(wid, x, y);
        self.settle_link_target(wid, parked);
    }

    /// The pointer-motion routing itself: chrome, modals, native views, drags,
    /// then the grid. See [`Self::on_cursor_moved`] for the caption bracket that
    /// wraps it.
    fn route_cursor_moved(&mut self, wid: WindowId, x: f64, y: f64) {
        // Pointer motion is not typing. It stays dark, and as a newer user
        // boundary it closes an older swallowed key's licence even when
        // chrome/modal handling returns locally.
        self.clear_move_license(wid);
        // Remember the raw pixel position so a follow-up button press can tell
        // whether it landed in the tab strip (intercepted before cell mapping).
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.last_cursor_px = (x, y);
            // POINTER PURSUIT (wave 3): the brain is its own motion sensor,
            // but it only senses on a TICK — and with the frame lane
            // released, mouse motion alone never produced one, so the pet
            // could not see a toy waved at it until an unrelated repaint.
            // One requested frame per motion event (coalesced by the
            // windowing system) lets the brain sample the pointer; if the
            // motion is real its own heat re-arms `needs_frames` and the
            // effects cadence takes over from there. Gated on the pet
            // actually being on glass — `pet_hit_rect` is Some exactly when
            // a visible pet was drawn last frame — so a petless window pays
            // nothing.
            if ws.pet_hit_rect.is_some()
                && let Some(w) = ws.os_window.as_ref()
            {
                w.request_redraw();
            }
        }
        // DRAG-TO-CONNECT (design §3.1–§3.3): while the connector gesture is
        // armed or dragging from THIS window, motion belongs to it — the §3.1
        // threshold, target tracking, and the wire all read the stream here.
        // Checked before every other motion layer: the press that armed it was
        // already swallowed by the strip, so nothing below may see the drag.
        // (Native-origin drags track through `Wake::ConnDragTo`, not winit.)
        if self
            .conn_drag
            .as_ref()
            .is_some_and(|d| !d.native && d.src_window == wid)
        {
            self.conn_drag_motion(wid, x, y);
            return;
        }
        // Derive the window's pointer geometry ONCE for the whole event. Every
        // consumer below used to re-derive it (4× on the macOS default, 8–10× with
        // the in-grid strip on and a selection drag live), and each derivation
        // re-parses a variable primary face. Nothing between here and the last use
        // can move it: only `ws.metrics` / the backend's face carry it, and the one
        // handler-reachable mutation of either (`drag_divider` → `resize_panes`)
        // returns immediately after, before any later use.
        let geom = self.pointer_geometry(wid);
        self.track_strip_hover(wid, geom, x, y);
        if self.palette_claims_pointer(wid) {
            self.palette_pointer_motion(wid, x, y);
            return;
        }
        // C5 TAB CONTEXT MENU: while the card is up it owns the pointer, so
        // motion only moves its HIGHLIGHT and stops — no grid hover, no
        // selection drag, no PTY motion report under an open menu. Change-gated
        // (a sweep within one row costs nothing) and it clears the highlight
        // when the pointer leaves the card, which is what a real menu does.
        // Placed after the palette for the same reason `on_mouse_input`'s gate
        // is: the palette is the more modal of the two.
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            // …but the cell caches still refresh, exactly as the About modal
            // does: the first click after a KEYBOARD dismiss (Esc/↵, no
            // intervening motion) must not act on the pre-open cell.
            self.refresh_mouse_cell(wid, geom, x, y);
            self.track_tab_menu_hover(wid, x, y);
            return;
        }
        // The connection confirm/configure card and the session picker: the
        // same one-slot modal boundary.
        if self.conn_card_claims_pointer(wid) {
            self.conn_card_pointer_motion(wid, x, y);
            return;
        }
        if self.session_picker_claims_pointer(wid) {
            self.session_picker_pointer_motion(wid, x, y);
            return;
        }
        if self.connection_map_claims_pointer(wid) {
            self.connection_map_pointer_motion(wid, x, y);
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
            self.refresh_mouse_cell(wid, geom, x, y);
            let _ = self.on_about_motion(wid, x, y);
            return;
        }
        // Native tabs own the full content region below host tab chrome. Track
        // hover in the view-local controller so the same typed tree drives the
        // visual wash, pointer cursor, semantics, and later press activation;
        // never leak motion into the parked PTY beneath the app.
        if let Some((_, view)) = self.active_native_view(wid) {
            // The plan's leaf count WITHOUT building a plan per motion event:
            // `Tab::visible_plan` collapses to a single leaf exactly when the tab
            // is zoomed with more than one leaf, so `len() > 1 && !zoomed` equals
            // `plan.leaves.len() > 1` for every input (and a missing window or
            // active tab makes both sides false, matching the `is_some_and`).
            if self.active_tab_has_multiple_visible_panes(wid) {
                self.refresh_mouse_cell(wid, geom, x, y);
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
            let native_hover = if self.strip_col_at_with(wid, geom, x, y).is_some() {
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
        if self.strip_col_at_with(wid, geom, x, y).is_some() {
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
            self.selection_autoscroll_with(wid, geom, y);
            return;
        }
        // The window cell is cached by the helper; the seam below consumes pane-local.
        let (_, (lr, lc)) = self.refresh_mouse_cell(wid, geom, x, y);
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
        self.update_hover_cursor_with(wid, geom);
        // Which half of the cell the pointer is in: the right half includes
        // the hovered cell, the left half stops before it. Remembered so a
        // shift-click press (which has no pixel position of its own) can
        // anchor by the half that was pressed. Subtract the `pad` inset first so
        // the half-split lines up with the (padded) cell, matching `pixel_to_cell`.
        let (cw, ch) = geom.cell;
        let cw = cw.max(1);
        let ch = ch.max(1);
        // W1: strip the leading remainder bands first (window→frame), THEN the
        // `pad` inset, so the half-split lines up with the on-glass (banded,
        // padded) cell — matching `pixel_to_cell`.
        let (fx, fy) = self.window_to_frame_with(wid, geom, x, y);
        let gx = (fx - geom.pad as f64).max(0.0) as usize;
        let side = if (gx % cw) * 2 >= cw {
            SelectionSide::Right
        } else {
            SelectionSide::Left
        };
        // Sub-cell pixel offset of the pointer inside its cell, measured from the
        // real winit cursor (band-, pad- and strip-stripped) so a DEC 1016 (SGR-pixel)
        // report carries a GENUINE sub-cell coordinate, not a cell-origin one. The
        // chrome (strip + status bars) occupies the top rows, so subtract its pixel
        // height from `y` before taking the per-cell remainder (matches
        // `pixel_to_term_cell`). Ignored by every cell-coordinate encoding — see
        // [`crate::input::PixelOffset`].
        let strip_px = usize::from(self.chrome_rows()) * ch;
        // The chrome headroom sits above the pad on the y-axis (x carries none),
        // matching `pixel_to_term_cell`'s `pad_top + head` inset.
        let gy = (fy - (geom.pad_top + geom.head) as f64).max(0.0) as usize;
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
        self.selection_autoscroll_with(wid, geom, y);
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
            // SELECTION CUSTODY — a drag is a `UserSelect` PRODUCER, not merely an
            // extender. Every gesture arm below re-issues `start_selection` on each
            // pointer move with NO state test, and `start_selection` sets `InProgress`
            // unconditionally — so a drag turns `has_selection()` back on from off
            // whenever something cleared it mid-gesture. `Some(g)` guards a live
            // GESTURE, never a live SELECTION. This module argued the seam away twice
            // before recording it; the argument was false both times.
            let selection_before = term.text_selection().has_selection();
            let sel_row = i32::from(row) - term.grid().display_offset() as i32;
            // SELECTION CUSTODY Phase 2: re-derive the gesture ORIGIN from the LIVE
            // selection each move, instead of trusting `GestureOrigin.row`.
            //
            // A word/line drag rebuilds the whole selection from its origin on every
            // pointer move. `GestureOrigin.row` is a plain `i32` captured in
            // selection space at press time and stored in `WindowState` — nothing
            // ever compensates it. The ENGINE's anchors are compensated (`post_process`
            // runs `adjust_for_scroll` on every parser batch), so as soon as output
            // scrolls the grid mid-drag the two disagree and each move re-anchors the
            // selection at a row the origin word no longer occupies.
            //
            // This was masked before: the `i32::MAX` damage sentinel usually cleared
            // the selection out from under the drag first, so the mis-anchor rarely
            // survived long enough to be seen. Phase 4 removes that sentinel and
            // selections start surviving output — which would turn a hidden bug into a
            // live WRONG-COPY path. Fixing it is therefore a prerequisite, not polish.
            //
            // `start()` is the origin anchor: every arm below re-anchors with
            // `start_selection(origin)` and then `update_selection(moving end)`, so the
            // selection's start IS the origin, and it rides `adjust_for_scroll`. Only
            // fall back to the captured row when there is no live selection to read
            // (the first move after a press that the engine has since cleared).
            let origin_row = match term.text_selection().state() {
                aterm_core::selection::SelectionState::None => None,
                _ => Some(term.text_selection().start().row),
            };
            match fws.gesture {
                None => {
                    term.text_selection_mut()
                        .update_selection(sel_row, col, fws.last_mouse_side);
                }
                // Triple-click drag: whole rows from the origin line to the
                // hovered line. Rebuilt from the origin each move so the
                // anchor sides stay inclusive in either drag direction.
                Some(g) if g.kind == SelectionType::Lines => {
                    let g_row = origin_row.unwrap_or(g.row);
                    let max_col = term.cols().saturating_sub(1);
                    let sel = term.text_selection_mut();
                    if sel_row < g_row {
                        sel.start_selection(
                            g_row,
                            max_col,
                            SelectionSide::Right,
                            SelectionType::Lines,
                        );
                        sel.update_selection(sel_row, 0, SelectionSide::Left);
                    } else {
                        sel.start_selection(g_row, 0, SelectionSide::Left, SelectionType::Lines);
                        sel.update_selection(sel_row, max_col, SelectionSide::Right);
                    }
                }
                // Double-click drag: snap the moving end to the hovered word
                // (or bare cell on whitespace); the origin word stays fully
                // selected by anchoring at its far boundary.
                Some(g) => {
                    let g_row = origin_row.unwrap_or(g.row);
                    let (ws, we) = control::word_cols(&term, sel_row, col).unwrap_or((col, col));
                    let sel = term.text_selection_mut();
                    if (sel_row, col) < (g_row, g.start_col) {
                        sel.start_selection(
                            g_row,
                            g.end_col,
                            SelectionSide::Right,
                            SelectionType::Semantic,
                        );
                        sel.update_selection(sel_row, ws, SelectionSide::Left);
                    } else {
                        sel.start_selection(
                            g_row,
                            g.start_col,
                            SelectionSide::Left,
                            SelectionType::Semantic,
                        );
                        sel.update_selection(sel_row, we, SelectionSide::Right);
                    }
                }
            }
            // Record only the 0 -> 1 EDGE. A drag that merely widens a selection
            // already alive changes no projected variable, so filing `UserSelect` on
            // every pointer move would bury the answer the `custody` verb exists to
            // give under motion noise.
            if !selection_before && term.text_selection().has_selection() {
                note_selection_custody(&mut term, true);
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
        let geom = self.pointer_geometry(wid);
        self.selection_autoscroll_with(wid, geom, y)
    }

    /// [`Self::selection_autoscroll`] against geometry the caller already derived.
    fn selection_autoscroll_with(&mut self, wid: WindowId, geom: PointerGeometry, y: f64) -> bool {
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
        let ch = geom.cell.1.max(1);
        // The chrome headroom stacks above the pad on the y-axis, so the grid's
        // top edge is `head + pad_top + strip_px` — fold it into the pad inset the
        // pure edge math subtracts (head == 0 && pad_top == pad is byte-identical).
        let pad = geom.pad_top + geom.head;
        let strip_px = usize::from(self.chrome_rows()) * ch;
        // W1: window→frame first, so the grid's top/bottom edges account for the
        // leading remainder band like every other pointer consumer.
        let (_, y) = self.window_to_frame_with(wid, geom, 0.0, y);
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
            // PRESS CUSTODY: the one gesture that both raises the offset AND grows
            // the selection. `Terminal::scroll_display` records the `UserScroll`
            // itself, and only on a RISE, so the downward half of an autoscroll
            // (dragging back toward live) records nothing rather than claiming a
            // transition the model does not admit.
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
            let cols = control::select_word(&mut term, sel_row, col);
            // PRESS CUSTODY: the double-click PRESS is where the highlight comes into
            // existence. `arm_gesture_drag` below pre-sets `sel_dragged`, so the
            // RELEASE eventually records a `UserSelect` — but that is the end of the
            // gesture, and until it arrives the record named an event that did not
            // make this selection.
            note_selection_custody(&mut term, true);
            cols
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
            // PRESS CUSTODY: as for the double-click above — the press that made the
            // highlight records it, not only the release that ends the gesture.
            note_selection_custody(&mut term, true);
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
    ///
    /// PRESS CUSTODY: this is where a selection comes into EXISTENCE under the
    /// `has_selection()` projection the conformance uses — `start_selection` leaves
    /// `SelectionState::InProgress`, which is already "a selection exists" — so the
    /// record is stamped here as well as on the release. `finish_selection`'s
    /// complete arm then re-stamps the same `UserSelect`, which is idempotent in the
    /// model (`selection = 1` from `selection == 1`) and is the honest report: the
    /// release really is another user gesture that leaves a selection behind.
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
            // Inside the SAME guard as the mutation, so no PTY-reader output can
            // land between the selection appearing and the record naming it.
            note_selection_custody(&mut term, true);
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
    /// COPY-ON-SELECT: when `copy_on_select` is enabled (config; default ON off
    /// Linux, OFF on Linux where a selection owns PRIMARY instead — see
    /// [`crate::app_config::Config::copy_on_select_or_default`]) and the release
    /// actually COMPLETED a selection (a real drag, not a deselecting click), the
    /// selected text is copied to the system clipboard right here — no explicit
    /// Cmd-C needed. The highlight is left intact (`copy_selection` does not
    /// clear it), so Cmd-C still works on the same selection afterwards.
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
    /// SELECTION CUSTODY — the RELEASE is where a gesture becomes a selection (or
    /// stops being one), so this one seam carries FOUR `SelectionCustody` actions:
    ///
    /// * `SelectLow`, `SelectOldest`, `SelectHigh` — the `complete_selection()` arm.
    ///   The model splits the gesture by WHERE the interval landed (a scrollback pair,
    ///   a single row on the oldest retained line, a live-screen pair) because that is
    ///   what makes the eviction and damage branches reachable at all; the CODE has one
    ///   arm, and the row interval is whatever the drag left in `text_selection`. Three
    ///   anchors on one method is the macro's documented `write_all` shape.
    /// * `UserClear` — the `clear()` arm: a press and release inside one cell is a
    ///   deliberate deselect, and a deliberate deselect is always allowed.
    ///
    /// This is the CONSUMER of the gesture, not the recorder: `TextSelection` holds the
    /// anchors but cannot tell a completed drag from an abandoned one — `ws.sel_dragged`
    /// is read here and nowhere below. Tier-1 drives the real
    /// `begin_selection` → `drag_selection` → `finish_selection` gesture in
    /// [`crate::selection_custody_conformance`].
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "SelectLow",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "SelectOldest",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "SelectHigh",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "SelectionCustody",
            action = "UserClear",
            project = "aterm_gui::selection_custody_conformance::project_selection_custody"
        )
    )]
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
            // PRESS CUSTODY: `ws.sel_dragged` is the ONLY thing that separates
            // `UserSelect` from `UserClear` — nothing inside `TextSelection` carries
            // it, which is why the record is stamped at this seam and not down in
            // the selection primitive. Same guard as the mutation.
            note_selection_custody(&mut term, completed);
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
        //
        // Window-routed (`copy_selection_in(wid)`), NOT the frontmost-window form:
        // this function already resolved `term` from `wid`, and the PRIMARY branch
        // below stringifies that same `term`. Using `frontmost_window` for the
        // CLIPBOARD half made ONE completed gesture write its two channels from TWO
        // different terminals whenever the two diverge.
        let fired = completed && self.copy_on_select && !suppress_copy_on_select;
        if fired {
            self.copy_selection_in(wid);
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

    /// The URL a PRESS at the pointer would open, if any: an (authorized) OSC 8
    /// hyperlink on the cell wins; else a plain-text `http(s)://` URL detected
    /// in the row. `None` wherever a press does not reach the grid at all.
    pub(crate) fn link_under_pointer(&self, wid: WindowId) -> Option<String> {
        let ws = self.windows.get(&wid)?;
        let (row, col) = ws.last_mouse_cell;
        let window_row = usize::from(ws.last_mouse_window_cell.0);
        // A CHROME BAND COVERS THE ROW IT TOOK, and a press lands on what is
        // painted there — never on the grid cell hidden beneath it. The same
        // register the hover asks, so the hand, the caption and the click give
        // one answer: without it the find panel's own row opens whatever link
        // it happens to be covering, and the caption's `ctrl+click opens` is a
        // promise about a cell the person cannot see.
        if self.chrome_owns_terminal_row(wid, window_row) {
            return None;
        }
        let term = term_lock(&ws.front_terminal()?.term);
        // VIEWPORT rows for BOTH probes. `render_row` resolves through
        // `display_offset`, so a screen-keyed OSC 8 lookup beside it names a
        // scrolled-back row's neighbour while the plain-text arm names the row
        // the person is pointing at — and the press must open what the band and
        // the pointer hand agree on, whatever the viewport is showing.
        term.hyperlink_at_visible(row, col)
            .map(|url| url.to_string())
            .or_else(|| {
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
        self.clear_move_license(wid);
        // GUI-ONLY prefix (gesture-state owner = App; a controller can't trigger
        // these): Cmd-click link-open, shift-extend, and the MULTI_CLICK_MS streak
        // FSM that yields the authoritative `click_count`. These stay in the
        // handler; the seam consumes `click_count`/`side` as DATA.
        let pressed = state == ElementState::Pressed;
        // THE END OF A TAB DRAG. Lifting the button ends drag-to-reorder wherever
        // the pointer is, so this sits ABOVE every gate below — a release the
        // palette or the tab menu swallows must still disarm the chip, or the next
        // bare hover across the strip would keep shuffling tabs. Nothing else is
        // decided here: the tab is already where the drag left it (each step
        // committed through `move_tab`), so there is no drop to settle.
        if !pressed
            && button == WinitMouseButton::Left
            && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.strip_drag = None;
        }
        // DRAG-TO-CONNECT (design §3.1–§3.3): a live winit-origin connector
        // gesture owns the left button — the RELEASE is its commit (§3.1:
        // in-place ⇒ menu, past the threshold ⇒ drop/cancel), checked before
        // every modal/native boundary below so none of them can eat it. A left
        // PRESS while one is somehow still armed means the release was lost
        // (focus steal mid-gesture): dissolve defensively, then let the press
        // proceed normally. Native-origin drags settle via their own wakes.
        if button == WinitMouseButton::Left && self.conn_drag.as_ref().is_some_and(|d| !d.native) {
            if pressed {
                self.conn_drag_abort();
            } else {
                self.conn_drag_release();
                return;
            }
        }
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
        // C5 TAB CONTEXT MENU: while the card is up it is MODAL over the
        // pointer — every button, press and release, dies here. The three
        // outcomes are the ones every menu on this platform has:
        //   * a press on a LIVE action row runs it and dismisses;
        //   * a press anywhere else ON the card (border, header, separator,
        //     greyed row) is swallowed and the card stays — a real menu ignores
        //     its own inert rows rather than closing on them;
        //   * a press OFF the card dismisses it and is CONSUMED. Click-away
        //     costs a click, which is the Windows/AppKit rule: the gesture that
        //     closes a menu never also acts on what is underneath, or a hasty
        //     dismiss would switch a tab / start a selection / land in a TUI.
        // Only the PRESS is examined; the matching RELEASE is swallowed by the
        // same gate having never set a `reported_buttons` bit, so a tracking app
        // sees neither half and its press/release stream stays balanced (the
        // config-banner pattern, restated in `RightPressPlan::Chrome`).
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.tab_menu.is_some())
        {
            if pressed {
                let (px, py) = self
                    .windows
                    .get(&wid)
                    .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
                if button == WinitMouseButton::Left
                    && let Some(i) = self.tab_menu_action_at(wid, px, py)
                {
                    self.activate_tab_menu_entry(wid, i);
                } else if !self.tab_menu_contains(wid, px, py) {
                    self.close_tab_menu(wid);
                }
            } else {
                // A swallowed RELEASE still settles any drag that was in flight
                // when the menu popped — the same belt the Settings/About modals
                // wear, or a divider/selection drag would keep tracking motion
                // the gate above is no longer delivering.
                self.settle_pointer_drags(wid);
            }
            return;
        }
        // CONNECTION CONFIRM/CONFIGURE CARD: the same modal boundary — left
        // press/release drive the chips + Confirm/Cancel; every other gesture
        // is swallowed (§3.3: nothing under the card can see the click).
        if self.conn_card_claims_pointer(wid) {
            let (x, y) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            if button == WinitMouseButton::Left {
                if pressed {
                    self.conn_card_pointer_press(wid, x, y);
                } else {
                    self.conn_card_pointer_release(wid, x, y);
                }
            }
            if !pressed {
                self.settle_pointer_drags(wid);
            }
            return;
        }
        // SESSION PICKER: same boundary; left press/release choose a row.
        if self.session_picker_claims_pointer(wid) {
            let (x, y) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            if button == WinitMouseButton::Left {
                if pressed {
                    self.session_picker_pointer_press(wid, x, y);
                } else {
                    self.session_picker_pointer_release(wid, x, y);
                }
            }
            if !pressed {
                self.settle_pointer_drags(wid);
            }
            return;
        }
        // CONNECTION MAP: same boundary; left press/release activate a chip
        // (raise) or a flow row (the inline disconnect confirm two-step).
        if self.connection_map_claims_pointer(wid) {
            let (x, y) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |window| window.last_cursor_px);
            if button == WinitMouseButton::Left {
                if pressed {
                    self.connection_map_pointer_press(wid, x, y);
                } else {
                    self.connection_map_pointer_release(wid, x, y);
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
            && self.active_visible_content_route(wid)
                == Some(crate::VisibleContentRoute::Heterogeneous)
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
                            })
                            .or_else(|| {
                                // The Tab Color wheel: a release inside the disk
                                // carries the picked color as reducer-ready hex.
                                artifact.compiled.color_wheel_color_at(&hit.key, x, y).map(
                                    |[r, g, b]| {
                                        crate::native_app::SemanticInput::Text(format!(
                                            "#{r:02X}{g:02X}{b:02X}"
                                        ))
                                    },
                                )
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
                // Below the strip, on a native view: still a click away from an
                // open in-grid rename field, so it still commits. Same rule as the
                // terminal path — the field's exit must not depend on what kind of
                // content happens to be under the pointer.
                if self.inline_rename_edit(wid).is_some() {
                    self.clear_strip_press(wid);
                    self.settle_rename_edit(wid);
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
        // THE STATUS BARS are chrome rows: a press on one opens the lane's own
        // deliberate surface (the durable record the bar points at) and is
        // consumed — like the strip, and unlike the retired floating card, this
        // is a press on chrome, not on something the chrome is standing on. Only
        // the PRESS is swallowed; the orphan-release guard below drops the
        // matching release, so a tracking app sees a balanced stream.
        if pressed && button == WinitMouseButton::Left {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            if let Some(lane) = self.status_bar_lane_at(wid, px, py) {
                let route = match lane {
                    crate::status_bars::Lane::Toolchain => {
                        crate::native_settings::SettingsRoute::Packages
                    }
                    crate::status_bars::Lane::Update => {
                        crate::native_settings::SettingsRoute::SoftwareUpdate
                    }
                };
                let _ = self.open_settings_tab(route);
                return;
            }
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
        // CONFIG-WARNING BANNER: `splice_config_notice` OVERWRITES the top grid rows
        // IN PLACE — tab strip included, since it paints last — so a press inside its
        // band is a press on chrome, not on whatever the chrome is standing on. It was
        // the one occluding surface with no mouse gate (palette, Settings, About, the
        // Update overlay, the strip, the find bar and the pet all have one), so a click
        // on a warning about `columns/lines` fell through to `strip_col_at` — and a chip's
        // `x` under it CLOSES A TAB, PTYs and all, with no confirmation unless it is the
        // window's last. With the strip off it leaked into a selection, or into a
        // mouse-tracking TUI's stdin.
        //
        // Chrome wins, and the click also DISMISSES the banner — the gesture the user is
        // already making at a notice they have read. The band is the SAME geometry the
        // splice paints (`config_notice_tray_floor_y`, `0` = no banner), so the hit region
        // cannot drift from the pixels, and the no-banner path stays byte-identical.
        //
        // Only the PRESS is swallowed: never having set the `reported_buttons` bit, the
        // matching release is already dropped by the orphan-release guard below, so a
        // tracking app still sees a balanced button stream (it sees neither half).
        if pressed && button == WinitMouseButton::Left {
            let floor = self.config_notice_tray_floor_y(wid);
            if floor > 0 {
                let (px, py) = self
                    .windows
                    .get(&wid)
                    .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
                let (_, fy) = self.window_to_frame(wid, px, py);
                let top = (self.win_pad_top(wid) + self.win_head(wid)) as f64;
                if fy >= top && fy < f64::from(floor) {
                    // The multi-line-paste confirmation paints over the config
                    // notice when both bands are up, so it takes the click first:
                    // a press on a modal security question is its FAIL-CLOSED
                    // answer (cancel — the parked text is dropped), never a
                    // confirm. Only with no confirmation outstanding does the
                    // click dismiss the config-warning banner as before.
                    if self.paste_banner.as_ref().is_some_and(|p| p.wid == wid) {
                        self.answer_paste_banner(false);
                    } else {
                        self.config_notice = None;
                    }
                    self.request_redraw_all_windows();
                    return;
                }
            }
        }
        // MIDDLE-CLICK PASTE (X11 PRIMARY): when no app is tracking the mouse, a
        // middle press pastes the PRIMARY selection (the X convention) through
        // `App::paste_primary_into` — the same asynchronous pipeline + pastejacking
        // guard (`deliver_paste`) as Ctrl+Shift+V, with the own-selection fast path
        // delivered synchronously and a FOREIGN owner's blocking `ConvertSelection`
        // round-trip offloaded to a worker (audit: the old arm called the blocking
        // read directly in this winit handler — up to ~1 s of UI freeze on a hung
        // owner — and raw `self.input(…Paste…)` skipped the multi-line-paste
        // guard). When tracking is ON it falls through to the mouse-report
        // encoding below so TUIs still receive the button. X11 only.
        #[cfg(target_os = "linux")]
        if pressed && button == WinitMouseButton::Middle {
            let tracking = self
                .front_terminal(wid)
                .map(|terminal| terminal.term.clone())
                .is_some_and(|t| term_lock(&t).mouse_tracking_enabled());
            if !tracking {
                self.paste_primary_into(wid);
                return;
            }
        }
        // RIGHT-CLICK COPY/PASTE (audit I6 — the conhost/Windows Terminal
        // convention, `right_click = "copy_paste"`, the Windows default): a
        // right PRESS over the grid with VT mouse tracking OFF copies the
        // selection if one exists (and clears it, QuickEdit-style) — else
        // pastes. Tracking ON leaves the button to the seam so a TUI keeps its
        // press/release reports exactly as before. The decision table is
        // [`right_press_plan`]; two shapes here are deliberate:
        //   * only the PRESS is examined — the release falls through, where a
        //     REPORTED press's release still reports (pairing preserved) and a
        //     consumed/chrome press's release dies at the `was_reported` guard
        //     below (no orphan release, the config-banner pattern);
        //   * the paste goes through `paste_clipboard_into` -> `deliver_paste`
        //     (the pastejacking guard + the S9 CF_HDROP file fallback), NEVER
        //     `self.input(…Paste…)` directly — the middle-click arm above
        //     routes through the same guard via `paste_primary_into`.
        // Cross-platform by config (macOS/Linux default `off` — their hands
        // expect a context menu / middle-click paste), so no cfg here: the
        // platform split lives in `RightClickGesture::PLATFORM_DEFAULT`.
        //
        // C5 COMPLETION (WINDOWS) — the bare-band right press below DECIDED
        // the window's system menu but deliberately did not open it (see the
        // `Chrome` arm: DefWindowProc refuses menu mode while winit still
        // holds the press's mouse capture). The matching RELEASE completes
        // it: by the time the posted SC_KEYMENU is pumped, winit's
        // WM_RBUTTONUP handling has called `ReleaseCapture`, so the menu
        // tracks for a real hand exactly as it does for Alt+Space. The
        // release is CONSUMED with the popup — its press was chrome, never
        // reported, so letting it fall through would at best die at the
        // `was_reported` guard and at worst hand a tracking TUI half a pair.
        // A right PRESS finding the latch still armed means the release was
        // lost (focus steal mid-press, the `conn_drag` defensive rule):
        // disarm and let the press decide afresh.
        #[cfg(windows)]
        if button == WinitMouseButton::Right
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.band_menu_release_pending)
        {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.band_menu_release_pending = false;
            }
            if !pressed {
                if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    let _ = crate::platform_win::popup_system_menu(w);
                }
                return;
            }
        }
        if pressed && button == WinitMouseButton::Right {
            let (px, py) = self
                .windows
                .get(&wid)
                .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
            let over_strip = self.strip_col_at(wid, px, py).is_some();
            let gesture_on = self.config.right_click_or_default()
                == crate::app_config::RightClickGesture::CopyPaste;
            let term = self
                .front_terminal(wid)
                .map(|terminal| terminal.term.clone());
            // One lock window for both facts AND the selection text: deciding
            // on a cheap "is there a selection" probe and re-extracting later
            // would materialize the selection twice (multi-ms on a huge
            // scrollback sweep, see `finish_selection`'s PRIMARY note).
            let (tracking, selection) = term.as_ref().map_or((false, None), |t| {
                let tl = term_lock(t);
                (
                    tl.mouse_tracking_enabled(),
                    tl.selection_to_string().filter(|s| !s.is_empty()),
                )
            });
            match right_press_plan(gesture_on, over_strip, tracking, selection.is_some()) {
                RightPressPlan::Chrome => {
                    // C5: the press is on the strip band. If it landed on a
                    // CHIP, pop that tab's context menu — the first Windows
                    // reflex on a tab, and the only way the long-composed
                    // `session_chrome::compose_tab_menu` model reaches a human
                    // off macOS. On the BARE band — which is caption, and
                    // already drags the window and maximizes on a double-click —
                    // the reflex is the WINDOW's system menu, the third and last
                    // thing a right-press on a Windows caption does. It used to
                    // be swallowed silently, so the one place aterm's caption
                    // was not a caption was also the only place Move / Size /
                    // Close could not be reached with the mouse.
                    //
                    // WINDOWS AND LINUX — the two platforms whose tab chrome IS
                    // the in-grid strip. macOS stays out: its chips carry a
                    // REAL `NSMenu` on the native strip (`toolbar.rs`), popped
                    // by AppKit on the same model, and the in-grid strip is a
                    // non-default fallback there — a second, differently-drawn
                    // menu would be two answers to one gesture on one platform.
                    //
                    // Linux was held back as a scope decision, not a technical
                    // one, pending a Linux lane's review (audit-2 item 10 is
                    // that review). Its three named concerns each resolve in
                    // the code below, not by fiat: (1) the chip right-press was
                    // ALREADY app-owned here — `right_press_plan` returns
                    // `Chrome` for the strip regardless of the gesture setting,
                    // so `RightClickGesture::PLATFORM_DEFAULT = off` never
                    // reached this arm and the "pure swallow" was a dead end,
                    // not a preservation — Copy Session ID, Copy CWD and the
                    // identity card were simply unreachable by mouse, keyboard
                    // AND `aterm ctl` on the one platform whose chrome this is;
                    // (2) Menu / Shift+F10 ride the SAME `TabMenuChord` policy,
                    // whose one-key surrender (`tab_menu_chord = "off"`) hands
                    // both keys back on any platform, and whose kitty-protocol
                    // deference is unconditional; (3) the card's keyboard mode
                    // is the same tested state machine (`tab_menu_input_event`)
                    // Windows ships, with the same dismiss rules.
                    #[cfg(any(windows, target_os = "linux"))]
                    if let Some(col) = self.strip_col_at(wid, px, py) {
                        let segs = self
                            .windows
                            .get(&wid)
                            .map(|ws| ws.tab_segments.clone())
                            .unwrap_or_default();
                        match crate::tab_bar::hit_test(&segs, col) {
                            Some(
                                crate::tab_bar::TabHit::Select(index)
                                | crate::tab_bar::TabHit::Close(index),
                            ) => {
                                let _ = self.open_tab_context_menu(wid, index, col, false);
                            }
                            // The bare band (or the `+` / `↻`, which have no menu
                            // of their own): on WINDOWS the caption's system menu — the
                            // band IS caption there. Linux has no system menu to pop, so
                            // the bare band keeps its wave-3 pure swallow (and this arm
                            // must not name the cfg(windows) platform_win module).
                            //
                            // DECIDED here, OPENED on the release: the press only ARMS
                            // `band_menu_release_pending`. Popping on the DOWN is a
                            // proven dead end — winit's win32 backend holds `SetCapture`
                            // for the whole press, `DefWindowProc` will not enter its
                            // modal menu loop while the thread holds mouse capture, so
                            // the posted SC_KEYMENU opened only for a machine-speed
                            // click whose release was already queued; a HUMAN press
                            // (release 50–150 ms later) opened nothing (GUI_INMENUMODE
                            // polled at zero through a 2 s held press, and the same post
                            // with no button down opened the menu instantly). The
                            // matching release completes it below, after winit has
                            // released capture — the bdf7cf40 custody rule.
                            _ =>
                            {
                                #[cfg(windows)]
                                if let Some(ws) = self.windows.get_mut(&wid) {
                                    ws.band_menu_release_pending = true;
                                }
                            }
                        }
                    }
                    return;
                }
                RightPressPlan::Copy => {
                    if let (Some(text), Some(t)) = (selection, term.as_ref()) {
                        // Copy, then clear-and-repaint even if the clipboard is
                        // momentarily held by another process (conhost clears
                        // its QuickEdit selection unconditionally too): the
                        // gesture's visible promise is "the selection is taken",
                        // and leaving a stale highlight would read as a no-op.
                        let _ = control::pbcopy(&text);
                        term_lock(t).text_selection_mut().clear();
                        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
                        {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                RightPressPlan::Paste => {
                    self.paste_clipboard_into(wid);
                    return;
                }
                // Tracking ON or gesture off: fall through — the seam reports
                // (or, tracking OFF + `off`, returns TrackingOff whose consumer
                // is Left-gated: today's inert behaviour, by explicit choice).
                RightPressPlan::Seam => {}
            }
        }
        // DISMISSING ROBI: a left press on the robot's drawn body queues the
        // `robi = false` write and stops HERE (see [`Self::robi_press_at`]
        // for the persist policy and the latch). Chrome-wins like the petting
        // seam below, but ABOVE the tab strip and the find bar: both of those
        // are spliced CELLS, and Robi is stamped `FreeZ::OverText` across the
        // whole composed frame — spliced strip rows included. He now HANGS FROM
        // the strip rather than through it (`app_render`'s `bar_y`), so his body
        // is over the grid and only his grip touches the band's last pixel row —
        // but a press there is still a press on him, and it must dismiss him
        // rather than switch (or close!) the tab underneath. Still below the
        // modals and the notice card, which composite over the finished frame
        // at present time — and his own tip bubble IS that notice card,
        // consumed by `notice_click` above.
        if pressed && button == WinitMouseButton::Left && self.robi_press_at(wid) {
            return;
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
            // OUTSIDE the strip with an in-grid rename field open: this is the
            // "click away" every other exit already honours, so it COMMITS what was
            // typed. It deliberately does NOT swallow the click — clicking the grid
            // on macOS ends the edit AND lands in the terminal, and the two
            // platforms must not differ on that. The streak dies with it: a press
            // in the grid is not part of a chip double-click.
            if self.inline_rename_edit(wid).is_some() {
                self.clear_strip_press(wid);
                self.settle_rename_edit(wid);
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
        // PETTING THE PET: a left press on the pet's drawn body strokes the
        // cat and stops HERE — chrome-wins, like the tab strip above (see
        // [`Self::pet_press_at`] for the policy and the tab-switch caveat).
        // Below the modals, the strip and the find bar (all of which cover
        // the pet), above the divider/pane-focus/selection/report layers
        // (all of which the cat's body occludes).
        if pressed && button == WinitMouseButton::Left && self.pet_press_at(wid) {
            return;
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
        // (a two-button chord under mouse tracking keeps both releases). Keyed on the
        // DENSE `slot()`, never on `code()`: the thumb buttons' wire codes are 128/129,
        // so the old `1 << (code & 7)` would alias Back onto Left's bit and Forward
        // onto Middle's — a thumb release would clear a live drag's press bit and
        // orphan its release.
        let bit = 1u8 << (button.slot() & 7);
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

    /// Normalize ONE winit wheel delta into whole notches: the axis, its
    /// [`WheelDir`], and how many reports/lines it is worth. `None` while the
    /// gesture is still sub-notch (banked) or carries no axis at all.
    ///
    /// Lines to move per event: whole lines drained from the per-window
    /// residual — one per classic ±1 LineDelta notch, banked fractions for
    /// precision-touchpad LineDelta and trackpad PixelDelta.
    ///
    /// AXIS FIRST (audit I7). The dominant component picks the axis; the
    /// horizontal one is then banked in its OWN residual and travels as a
    /// `WheelDir::Left/Right`. It used to be dropped in `on_mouse_wheel` with an
    /// early return, which was the correct LOCAL answer (a grid has no
    /// horizontal viewport, and the guard fixed a phantom scroll-down) but also
    /// silently ate the gesture for a mouse-TRACKING app: Neovim, tmux and every
    /// other SGR consumer never saw buttons 66/67 from aterm. Not a Windows
    /// quirk — the `PixelDelta` twin dropped macOS trackpad swipes identically.
    ///
    /// Split out of `on_mouse_wheel` so the SIGN convention (which winit sign
    /// becomes which xterm button) is provable against the REAL per-window
    /// residual fields rather than against locals: a test that banks into its own
    /// `f64` cannot fail no matter what this code does.
    pub(crate) fn wheel_notches(
        &mut self,
        wid: WindowId,
        delta: MouseScrollDelta,
    ) -> Option<(WheelDir, i32)> {
        match delta {
            MouseScrollDelta::LineDelta(x, y) => {
                // STRICT dominance on both axes, so the accepted VERTICAL set is
                // bit-for-bit the old guard's (`y != 0 && |y| > |x|`) and a tie —
                // including the all-zero event, which used to fall through to
                // dir_up=false + `.max(1)` and scroll DOWN one line — still
                // carries no axis and dies here.
                let axis = wheel_axis(f64::from(x), f64::from(y), 0.0)?;
                let ws = self.windows.get_mut(&wid)?;
                match axis {
                    // Positive x means "the content moves right", i.e. the reader
                    // is walking LEFT (winit's documented sign, and the reason its
                    // Windows backend negates WM_MOUSEHWHEEL's tilt-right).
                    WheelAxis::Horizontal => {
                        // sub-notch motion banks and emits nothing yet
                        let (positive, n) =
                            bank_scroll_lines(&mut ws.scroll_residual_x, f64::from(x))?;
                        Some((horizontal_dir(positive), n))
                    }
                    // Fractional deltas (Windows precision touchpads / free-spinning
                    // wheels emit many |y| << 1 events) bank into the residual and
                    // emit only WHOLE lines — the old `.round().max(1)` forced a full
                    // line per micro-event, scrolling several times too fast. A
                    // classic ±1-per-notch wheel still moves exactly one line/notch.
                    WheelAxis::Vertical => {
                        let (up, n) = bank_scroll_lines(&mut ws.scroll_residual, f64::from(y))?;
                        Some((vertical_dir(up), n))
                    }
                }
            }
            MouseScrollDelta::PixelDelta(p) => {
                // Same axis split for trackpad pixel deltas, with the same EPSILON
                // floor the old vertical guard carried. The horizontal half is
                // measured in CELL WIDTHS so a swipe's notch count is the same unit
                // the vertical half uses (cell heights) — a REPORT count, not a
                // pixel count; the app does its own per-notch conversion.
                //
                // A cell is roughly twice as tall as it is wide, so the SAME pixel
                // distance is worth about 2x the reports sideways. That is the
                // intended unit, not an oversight: a column is the horizontal
                // quantum exactly as a row is the vertical one, and measuring a
                // sideways swipe in row HEIGHTS would be arbitrary. The volume it
                // permits is bounded where every source converges, by
                // `MAX_WHEEL_BURST` at the seam.
                let axis = wheel_axis(p.x, p.y, f64::EPSILON)?;
                let (cw, ch) = self.win_cell_size(wid);
                let (cw, ch) = (cw.max(1) as f64, ch.max(1) as f64);
                let ws = self.windows.get_mut(&wid)?;
                match axis {
                    WheelAxis::Horizontal => {
                        let (positive, n) = bank_scroll_lines(&mut ws.scroll_residual_x, p.x / cw)?;
                        Some((horizontal_dir(positive), n))
                    }
                    // Accumulate the signed sub-line delta in the per-window residual
                    // and emit only WHOLE lines, carrying the fraction forward — so
                    // slow, precise trackpad scrolling moves pixel-by-pixel instead of
                    // snapping ≥1 line per event and dropping the remainder.
                    WheelAxis::Vertical => {
                        let (up, n) = bank_scroll_lines(&mut ws.scroll_residual, p.y / ch)?;
                        Some((vertical_dir(up), n))
                    }
                }
            }
        }
    }

    /// `MouseWheel` -> when an app is tracking the mouse, report wheel up/down at
    /// the cell under the pointer; otherwise scroll the scrollback viewport (the
    /// everyday "scroll up to see history" gesture).
    pub(crate) fn on_mouse_wheel(&mut self, wid: WindowId, delta: MouseScrollDelta) {
        self.clear_move_license(wid);
        // The modal claim is decided before normalization. Sub-line gestures still
        // return below without ever reaching native/terminal scroll consumers, and
        // so do HORIZONTAL ones over the palette / a native view — the terminal
        // seam is the only consumer with an answer for that axis (audit I7).
        let palette = self.palette_claims_pointer(wid);
        let conn_card = self.conn_card_claims_pointer(wid);
        let session_picker = self.session_picker_claims_pointer(wid);
        let connection_map = self.connection_map_claims_pointer(wid);
        let Some((dir, lines)) = self.wheel_notches(wid, delta) else {
            return;
        };
        // CHROME AND NATIVE VIEWS ARE VERTICAL SURFACES. The palette card and a
        // Settings/editor/reader tab each scroll ONE list on ONE axis, so a
        // horizontal flick over them is nothing — and it must not fall through to
        // the terminal seam either, because the PTY under a native tab is PARKED
        // (the whole point of the native pointer boundary in `on_mouse_input`).
        // Over the GRID a horizontal flick DOES continue to the seam, which
        // reports it to a tracking app and yields zero local motion otherwise.
        //
        // HONEST SCOPE of "tracking-off is unchanged": byte-identical and
        // motion-identical, not side-effect-identical. A horizontal flick over
        // the grid now traverses `App::input` -> `input_to_session`, where the
        // old early-return stopped it — so it now also trips
        // `note_update_handoff_activity` (which revokes a pending automatic-update
        // handoff overlap a dropped gesture used to leave intact), the
        // `is_present_recovery_stimulus` retry rearm, and `note_alt_scroll` on the
        // alt screen. All three are "the user is active" signals and are the
        // RIGHT answer for a gesture the user really made — but they are new
        // observable effects, so they are named here rather than implied away.
        let vertical_up = dir.vertical_up();
        if vertical_up.is_none()
            && (palette
                || conn_card
                || session_picker
                || connection_map
                || self.active_native_view(wid).is_some())
        {
            return;
        }
        if let Some(up) = vertical_up {
            let signed = if up {
                -(lines as isize)
            } else {
                lines as isize
            };
            if palette {
                self.palette_pointer_wheel(wid, signed);
                return;
            }
            // The confirm/configure card has no scroll model: the modal still
            // swallows the wheel (never a scroll-through layer).
            if conn_card {
                return;
            }
            if session_picker {
                self.session_picker_pointer_wheel(wid, signed);
                return;
            }
            if connection_map {
                self.connection_map_pointer_wheel(wid, signed);
                return;
            }
            if self.active_native_view(wid).is_some() {
                let signed = if up { -lines } else { lines };
                let _ = self
                    .dispatch_native_event(wid, crate::native_app::AppEvent::ScrollLines(signed));
                return;
            }
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
        // vs tracking-OFF (scroll the viewport `lines`) under one mode read. That
        // is also where the horizontal axis is resolved: REPORTED while an app
        // tracks the mouse, zero local motion otherwise. Reading the mode HERE
        // instead would split the decision across two lock windows and hand the
        // control `mouse` verb a different answer than a human hand — exactly the
        // source-blindness the seam exists to guarantee.
        self.input(
            wid,
            InputEvent::Wheel {
                dir,
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
    /// dragged top-to-bottom. Copy (Cmd-C) then works on the whole VISIBLE
    /// screen exactly as on a mouse selection.
    ///
    /// SELECTION CUSTODY Phase 2 deleted the snap-to-bottom this used to do
    /// first, so the rows it writes are NOT `0..rows`: they are
    /// `-off..=rows-1-off` for the live `display_offset` (negative rows are
    /// scrollback), which collapses to `0..rows` only when the viewport is
    /// already at live. Any second caller must derive its arithmetic the same
    /// way — see the body for why.
    pub(crate) fn select_all(&mut self) {
        // A window-level command (menu Select All): targets the frontmost window.
        let Some(ws) = self.front() else { return };
        let Some(terminal) = ws.front_terminal() else {
            return;
        };
        let last = i32::from(ws.rows.saturating_sub(1));
        let max_col = ws.cols.saturating_sub(1);
        {
            let mut term = term_lock(&terminal.term);
            // SELECTION CUSTODY Phase 2: no snap-to-bottom first.
            //
            // This used to jump the viewport to live "so 0..rows are stable
            // selection coordinates", and then select those rows — which meant
            // ⌘-A while reading history selected the screen the user had just been
            // moved AWAY from, and silently destroyed their place to do it. Select
            // All should select what you are looking at.
            //
            // The coordinates are made stable the honest way instead: read the live
            // `display_offset` inside the lock this function already takes and map
            // the visible rows through it, exactly as the mouse path does
            // (`sel_row = row - display_offset`). Visible rows `0..=last` are
            // selection rows `-off..=last-off`; negative rows are scrollback, which
            // is precisely where the user is reading.
            let off = term.grid().display_offset() as i32;
            let sel = term.text_selection_mut();
            sel.start_selection(-off, 0, SelectionSide::Left, SelectionType::Lines);
            sel.update_selection(last - off, max_col, SelectionSide::Right);
            sel.expand_lines(max_col);
            sel.complete_selection();
            // PRESS CUSTODY: ⌘-A reaches the `TextSelection` primitive directly —
            // there is no mouse-down before it and no `finish_selection` after it —
            // so without this the projected `selection` went 0 -> 1 with nothing
            // recorded, and the verb named whatever came before. Same guard as the
            // mutation.
            note_selection_custody(&mut term, true);
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

/// Which axis ONE wheel event belongs to, or `None` when it carries no axis at
/// all (audit I7). Pure, so the classification table is unit-pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WheelAxis {
    Horizontal,
    Vertical,
}

/// Classify a raw `(x, y)` wheel delta. `floor` is the magnitude below which a
/// component counts as no motion at all (0 for `LineDelta`, `f64::EPSILON` for
/// `PixelDelta`, matching the guards each arm carried before the axis existed).
///
/// STRICT dominance both ways, and that is deliberate: it makes the accepted
/// VERTICAL set bit-for-bit identical to the old `y == 0.0 || y.abs() <= x.abs()`
/// early-return, so nothing about tracking-off scrolling moved. A diagonal TIE
/// (|x| == |y|, the all-zero event included) still belongs to NEITHER axis and is
/// dropped — a 45° trackpad drift has no defensible answer, and the old code's
/// answer to the 0/0 case was a phantom scroll-DOWN.
pub(crate) fn wheel_axis(x: f64, y: f64, floor: f64) -> Option<WheelAxis> {
    let (ax, ay) = (x.abs(), y.abs());
    if ax > ay && ax >= floor && ax > 0.0 {
        return Some(WheelAxis::Horizontal);
    }
    if ay > ax && ay >= floor && ay > 0.0 {
        return Some(WheelAxis::Vertical);
    }
    None
}

/// A banked HORIZONTAL notch's [`WheelDir`]. `positive` is winit's sign, where
/// "+x" means the content moves right — i.e. the reader walks LEFT (xterm's
/// button 6). Named rather than inlined so the sign convention is stated once.
fn horizontal_dir(positive: bool) -> WheelDir {
    if positive {
        WheelDir::Left
    } else {
        WheelDir::Right
    }
}

/// A banked VERTICAL notch's [`WheelDir`] (+y = wheel up = older content).
fn vertical_dir(up: bool) -> WheelDir {
    if up { WheelDir::Up } else { WheelDir::Down }
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
    use super::{WheelDir, bank_scroll_lines, press_selection_kind, press_starts_selection};
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
                + (head + usize::from(app.chrome_rows()) * ch) as f64
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
        // A short compact search gives its contextual live preview the first
        // bounded slice and the matching native field the next one — at the 12 px
        // `FONT_PX` of the macOS arm. On Windows `FONT_PX` is 16 and on Linux 15
        // (the logical-unit split documented at the constant: a 1/96-in logical
        // px there, so those are 12 pt / 11.25 pt), which makes the headless
        // window taller in device px, and the compact page then fits the preview
        // AND the field in ONE bounded slice — the field is on slice 0 and the
        // next slice paints nothing. Staging only: the properties under test
        // (leading-pad caret, swatch-edge caret, pad→swatch drag selection) are
        // asserted identically on all three, and the slice the field lands on is
        // a function of viewport height, not of the behaviour being pinned.
        #[cfg(target_os = "macos")]
        let field_slice = 1;
        #[cfg(not(target_os = "macos"))]
        let field_slice = 0;
        settings.page_scroll = field_slice;
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
        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/compact-navigation"),
                value: None,
            }),
        )
        .unwrap();
        let Some(AppViewState::Settings(settings)) =
            app.native_runtime.view_state_mut(settings_view)
        else {
            panic!("Settings state");
        };
        // Short landscape category sheets intentionally show one complete row
        // per virtual page. Put the pointer fixture on Modified's exact page
        // instead of assuming the second route is simultaneously visible.
        settings.page_scroll = crate::native_settings::SettingsRoute::ALL
            .iter()
            .position(|route| *route == crate::native_settings::SettingsRoute::Modified)
            .expect("Modified route index");

        // Ordinary buttons still arm on press and activate only on a matched
        // release; no text position exists for their painted node.
        let compiled = presented_native_ui(&mut app, wid);
        let route_button = compiled
            .hits
            .iter()
            .find(|hit| hit.action.as_str() == "settings/route/modified")
            .expect("visible Modified route button");
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
            crate::native_settings::SettingsRoute::Modified
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
        // A hand-rolled `format!("file://{}")` URI is MALFORMED on Windows (drive
        // letter + backslashes after the authority slot), so these tests never ran
        // there. Build the URI the way the shipping path does.
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
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
        // A hand-rolled `format!("file://{}")` URI is MALFORMED on Windows (drive
        // letter + backslashes after the authority slot), so these tests never ran
        // there. Build the URI the way the shipping path does.
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();

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
        // A hand-rolled `format!("file://{}")` URI is MALFORMED on Windows (drive
        // letter + backslashes after the authority slot), so these tests never ran
        // there. Build the URI the way the shipping path does.
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();
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
        assert!(origin_x > 0, "fixture needs a horizontal remainder band");
        if cfg!(target_os = "linux") {
            // The vertical axis is top-pinned there: all slack below the frame.
            assert_eq!(origin_y, 0, "Linux pins the frame top");
        } else {
            assert!(origin_y > 0, "fixture needs a vertical remainder band");
        }
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
        assert!(ox > 0, "fixture needs the horizontal remainder band");
        if cfg!(target_os = "linux") {
            // Top-pinned vertical placement: the origin is 0 by policy, and the
            // origin-stripping seam is still exercised through the X axis.
            assert_eq!(oy, 0, "Linux pins the frame top");
        } else {
            assert!(oy > 0, "fixture needs the vertical remainder band");
        }

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

    /// Audit I7 — the axis table. The VERTICAL column is the regression fence:
    /// it must accept exactly what the old `y == 0.0 || y.abs() <= x.abs()`
    /// early-return accepted, no more, so tracking-off scrolling is untouched.
    /// The horizontal column is the new capability, and the tie/zero rows are
    /// the phantom-scroll-down cases that must stay dropped.
    #[test]
    fn wheel_axis_matches_the_old_vertical_guard_and_adds_the_horizontal_one() {
        use super::{WheelAxis, wheel_axis};
        // Vertical dominance (either sign) — accepted before and now.
        assert_eq!(wheel_axis(0.0, 1.0, 0.0), Some(WheelAxis::Vertical));
        assert_eq!(wheel_axis(0.0, -1.0, 0.0), Some(WheelAxis::Vertical));
        assert_eq!(wheel_axis(0.4, -1.0, 0.0), Some(WheelAxis::Vertical));
        // Horizontal dominance — dropped before, reported now.
        assert_eq!(wheel_axis(1.0, 0.0, 0.0), Some(WheelAxis::Horizontal));
        assert_eq!(wheel_axis(-1.0, 0.4, 0.0), Some(WheelAxis::Horizontal));
        // No axis: the all-zero event (which used to scroll DOWN one line) and
        // a perfect diagonal, which has no defensible answer either way.
        assert_eq!(wheel_axis(0.0, 0.0, 0.0), None);
        assert_eq!(wheel_axis(1.0, 1.0, 0.0), None);
        assert_eq!(wheel_axis(-2.0, 2.0, 0.0), None);
        // The PixelDelta floor rejects sub-EPSILON motion on BOTH axes, matching
        // the `p.y.abs() < f64::EPSILON` guard the vertical arm always had.
        assert_eq!(wheel_axis(0.0, f64::EPSILON / 2.0, f64::EPSILON), None);
        assert_eq!(wheel_axis(f64::EPSILON / 2.0, 0.0, f64::EPSILON), None);
    }

    /// Audit I7, on the REAL handler. `App::wheel_notches` (the normalizer
    /// `on_mouse_wheel` runs before anything else) must bank the two axes into
    /// the two SEPARATE per-window fields — `scroll_residual_x` for horizontal,
    /// `scroll_residual` for vertical — so a sideways flick can never pay off,
    /// or cancel, pending vertical motion.
    ///
    /// The previous version of this test banked into two LOCAL `f64`s and so
    /// could not fail whatever the handler did; it proved a property of
    /// `bank_scroll_lines` under a name that claimed a property of the handler.
    #[test]
    fn the_two_wheel_axes_bank_into_separate_window_fields() {
        use crate::{App, WindowId};
        use winit::event::MouseScrollDelta;

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // `LineDelta` is f32, so the banked f64 carries the f32 rounding of 0.6.
        let near = |got: f64, want: f64, what: &str| {
            assert!((got - want).abs() < 1e-6, "{what}: got {got}, want ~{want}");
        };
        let residuals = |app: &App| {
            let ws = &app.windows[&wid];
            (ws.scroll_residual_x, ws.scroll_residual)
        };
        assert_eq!(residuals(&app), (0.0, 0.0), "both banks start empty");

        // A sub-notch flick on each axis: neither emits, and each lands in its
        // OWN field. If the handler shared one residual these two 0.6s would
        // already have made a whole notch.
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.6, 0.0)),
            None
        );
        near(residuals(&app).0, 0.6, "x banked in the x field");
        assert_eq!(residuals(&app).1, 0.0, "the y field is untouched by x");
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.0, 0.6)),
            None
        );
        near(residuals(&app).0, 0.6, "the x bank survived a y event");
        near(residuals(&app).1, 0.6, "y banked in the y field");

        // Each axis needs its OWN second half to emit; neither borrows the other.
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(0.0, 0.6)),
            Some((WheelDir::Up, 1)),
            "the vertical bank pays off from vertical motion alone"
        );
        near(
            residuals(&app).0,
            0.6,
            "the vertical payoff must not drain the horizontal bank",
        );
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(-0.6, 0.0)),
            None,
            "a horizontal reversal forfeits only the horizontal bank"
        );
        near(
            residuals(&app).0,
            -0.6,
            "the reversal restarted the x bank in its own direction",
        );
    }

    /// Audit I7 — the horizontal axis END TO END, in BOTH directions, through
    /// the real `App` handler, with the exact xterm bytes each direction emits.
    ///
    /// This is the sign convention the whole widening rests on: winit documents
    /// "+x = the content moves right", i.e. the reader walks LEFT, which is
    /// xterm button 66; -x is button 67. Getting it backwards would silently
    /// invert every horizontal scroll in Neovim/tmux, and nothing else in the
    /// suite drives the handler off the vertical axis.
    #[test]
    fn horizontal_flicks_report_both_xterm_buttons_in_the_right_direction() {
        use aterm_core::terminal::Terminal;
        use winit::dpi::PhysicalPosition;
        use winit::event::MouseScrollDelta;

        use crate::{App, WindowId};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);

        // Classic tilt-wheel notches, both signs.
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(1.0, 0.0)),
            Some((WheelDir::Left, 1)),
            "+x (content moves right) = the reader walks LEFT"
        );
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(-1.0, 0.0)),
            Some((WheelDir::Right, 1)),
            "-x = the reader walks RIGHT (winit's Windows backend negates WM_MOUSEHWHEEL)"
        );
        // A multi-notch flick carries its count.
        assert_eq!(
            app.wheel_notches(wid, MouseScrollDelta::LineDelta(-3.0, 0.0)),
            Some((WheelDir::Right, 3))
        );

        // Trackpad pixel deltas take the same signs, measured in CELL WIDTHS.
        let cw = app.win_cell_size(wid).0.max(1) as f64;
        assert_eq!(
            app.wheel_notches(
                wid,
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(cw, 0.0))
            ),
            Some((WheelDir::Left, 1)),
            "+x pixels agree with +x lines"
        );
        assert_eq!(
            app.wheel_notches(
                wid,
                MouseScrollDelta::PixelDelta(PhysicalPosition::new(-cw, 0.0))
            ),
            Some((WheelDir::Right, 1))
        );

        // THE BYTES. `Terminal::encode_mouse_wheel` is the seam's SOLE wheel
        // byte producer, so pinning it for the two dirs above pins what a
        // tracking app actually receives from a left/right flick.
        let mut term = Terminal::new(24, 80);
        term.process(b"\x1b[?1000h");
        term.process(b"\x1b[?1006h");
        // Coordinates are 0-based going in and 1-based on the wire.
        assert_eq!(
            term.encode_mouse_wheel(WheelDir::Left, 0, 0, 0).as_deref(),
            Some(b"\x1b[<66;1;1M".as_slice()),
            "walking left is xterm button 6"
        );
        assert_eq!(
            term.encode_mouse_wheel(WheelDir::Right, 0, 0, 0).as_deref(),
            Some(b"\x1b[<67;1;1M".as_slice()),
            "walking right is xterm button 7"
        );
        // …and the vertical pair keeps its codes, so the four are distinct.
        assert_eq!(
            term.encode_mouse_wheel(WheelDir::Up, 0, 0, 0).as_deref(),
            Some(b"\x1b[<64;1;1M".as_slice())
        );
        assert_eq!(
            term.encode_mouse_wheel(WheelDir::Down, 0, 0, 0).as_deref(),
            Some(b"\x1b[<65;1;1M".as_slice())
        );
    }

    /// The horizontal axis has NO local viewport, on the grid or over chrome.
    /// With tracking off a sideways flick over the grid must move nothing (the
    /// old early-return's behaviour, preserved by `WheelPlan::Fallback`'s
    /// `wheel_lines: 0`), and over the palette / a native tab it must be
    /// swallowed before it can reach the parked PTY at all.
    #[test]
    fn horizontal_flicks_move_no_local_viewport_anywhere() {
        use winit::event::MouseScrollDelta;

        use crate::native_app::AppViewState;
        use crate::{App, WindowId};

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
        }

        // Over the GRID, tracking off: the flick reaches the seam and yields
        // zero rows of motion and no glide.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(3.0, 0.0));
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(-3.0, 0.0));
        assert_eq!(crate::term_lock(&term).grid().display_offset(), 0);
        assert!(
            app.windows[&wid].scroll_glide.is_none(),
            "a horizontal flick never arms the vertical smooth scroll"
        );
        // Negative control: the VERTICAL twin of the same gesture does move.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, 3.0));
        assert!(
            app.windows[&wid].scroll_glide.is_some()
                || crate::term_lock(&term).grid().display_offset() > 0,
            "negative control: the vertical axis still scrolls"
        );

        // Over a NATIVE tab: swallowed before `dispatch_native_event`. The About
        // route carries a page-scroll limit without needing a render pass
        // (`special_page_scroll_limit`), so the vertical negative control below
        // really can move.
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_, view) = app.active_native_view(wid).expect("Settings active");
        let page_scroll = |app: &App| match app.native_runtime.view_state(view).unwrap() {
            AppViewState::Settings(settings) => settings.page_scroll,
            _ => panic!("Settings state"),
        };
        let before = page_scroll(&app);
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(5.0, 0.0));
        assert_eq!(
            page_scroll(&app),
            before,
            "a horizontal flick never scrolls a native view"
        );
        // Negative control: the vertical twin does reach the native scroller.
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(0.0, -5.0));
        assert_ne!(
            page_scroll(&app),
            before,
            "negative control: the vertical axis still reaches the native scroller"
        );

        // Over the PALETTE: swallowed before `palette_pointer_wheel`.
        let mut app = App::headless_for_test();
        app.palette_enter();
        let scroll_of = |app: &App| app.windows[&wid].palette().unwrap().scroll_extent().0;
        let before = scroll_of(&app);
        app.on_mouse_wheel(wid, MouseScrollDelta::LineDelta(5.0, 0.0));
        assert_eq!(
            scroll_of(&app),
            before,
            "a horizontal flick never scrolls the palette card"
        );
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

    /// Audit I6 — THE RIGHT-CLICK DECISION TABLE (the conhost/WT convention):
    /// chrome swallows first (even with the gesture off — the bogus-cell fix is
    /// unconditional), a tracking app keeps the button, and only then does
    /// copy-if-selection-else-paste split. All 16 rows, so the precedence can
    /// never silently reorder.
    #[test]
    fn right_press_plan_matches_the_windows_convention() {
        use super::{RightPressPlan, right_press_plan};
        for gesture_on in [false, true] {
            for has_selection in [false, true] {
                for tracking in [false, true] {
                    // Chrome (the strip band) wins over EVERYTHING: no paste on
                    // a chip, and no report at the clamped cell under the band.
                    assert_eq!(
                        right_press_plan(gesture_on, true, tracking, has_selection),
                        RightPressPlan::Chrome,
                        "chrome must swallow (gesture_on={gesture_on}, tracking={tracking})"
                    );
                }
                // Tracking ON over the grid: the app owns the button — the press
                // reports exactly as it did before the gesture existed,
                // selection or not, config on or off.
                assert_eq!(
                    right_press_plan(gesture_on, false, true, has_selection),
                    RightPressPlan::Seam,
                    "tracking keeps the button (gesture_on={gesture_on})"
                );
            }
        }
        // The gesture itself, tracking OFF over the grid.
        assert_eq!(
            right_press_plan(true, false, false, true),
            RightPressPlan::Copy,
            "selection exists: copy (and clear) it"
        );
        assert_eq!(
            right_press_plan(true, false, false, false),
            RightPressPlan::Paste,
            "no selection: paste"
        );
        // `right_click = "off"`: the seam keeps the press — today's inert
        // tracking-OFF behaviour, by explicit config choice.
        assert_eq!(
            right_press_plan(false, false, false, true),
            RightPressPlan::Seam
        );
        assert_eq!(
            right_press_plan(false, false, false, false),
            RightPressPlan::Seam
        );
    }

    /// W1 regression (kill the compositor stretch): the pointer geometry mirrors
    /// the band placement — horizontally the centred `band_offset` (grid-fit +
    /// 7px shifts the frame by the leading 3px band), vertically the platform
    /// `band_offset_y` (top-pinned on Linux, centred elsewhere); headless (no
    /// `win_px`) and an exact grid fit stay at origin 0. The fixture uses
    /// independent 2px top / 12px bottom padding.
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
        // Horizontally the leading band is the low half of the remainder
        // (centred); vertically it follows the PLATFORM policy — top-pinned on
        // Linux (the whole remainder lands under the frame, keeping the chrome
        // band glued to the titlebar), centred elsewhere. Both come from the
        // same `band_offset`/`band_offset_y` pair the presenters place with.
        let (ox, oy) = (rw / 2, if cfg!(target_os = "linux") { 0 } else { rh / 2 });
        assert_eq!(
            app.frame_origin(wid),
            (ox as i64, oy as i64),
            "a sub-cell remainder must land exactly where the presenter's bands do"
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

        // A transient surface smaller than the composed frame is a SOURCE crop:
        // centred horizontally everywhere; vertically top-pinned on Linux (the
        // crop falls off the bottom, the band stays glued to the titlebar) and
        // centred elsewhere. Preserve the signed origin so window pixel 0 maps
        // to the actual frame coordinate instead of incorrectly clamping at 0.
        app.windows.get_mut(&wid).expect("window").win_px = Some(PhysicalSize::new(
            fit.width.saturating_sub(3).max(1),
            fit.height.saturating_sub(5).max(1),
        ));
        let cropped_origin = app.frame_origin(wid);
        assert!(cropped_origin.0 < 0);
        if cfg!(target_os = "linux") {
            assert_eq!(cropped_origin.1, 0, "a top-pinned crop keeps the frame top");
        } else {
            assert!(cropped_origin.1 < 0);
        }
        assert_eq!(
            app.window_to_frame(wid, 0.0, 0.0),
            (-cropped_origin.0 as f64, -cropped_origin.1 as f64),
            "pointer transform must retain the source-crop origin"
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

    /// Audit I8, on the REAL handler. A THUMB button must get its own
    /// `reported_buttons` slot, so pressing it mid-drag cannot clear the Left
    /// button's bit and orphan that drag's release.
    ///
    /// This is the exact aliasing the `slot()` fix exists to kill: `Back.code()`
    /// is 128 and `128 & 7 == 0 == Left.code() & 7`, so the old
    /// `1 << (code & 7)` bookkeeping folded Back onto Left and Forward onto
    /// Middle. Nothing drove `on_mouse_input` with a thumb button before, so the
    /// whole `winit_mouse_button(Back)` -> bitset -> press/release pairing chain
    /// was unexercised end to end.
    #[test]
    fn a_thumb_press_never_clears_a_live_drags_reported_bit() {
        use aterm_types::mouse::MouseButton;
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        // A foreground app is tracking, so every button press reports and sets
        // its bit (no local selection gesture claims the Left press).
        term_lock(&term).process(b"\x1b[?1000h");
        assert!(term_lock(&term).mouse_tracking_enabled());
        let bits = |app: &App| app.windows[&wid].reported_buttons;
        let bit = |b: MouseButton| 1u8 << (b.slot() & 7);
        // The aliasing this test fences: the OLD `1 << (code & 7)` gave Back and
        // Left the same bit.
        assert_eq!(
            MouseButton::Back.code() & 7,
            MouseButton::Left.code() & 7,
            "the wire codes really do alias under `& 7` — that is why slot() exists"
        );
        assert_ne!(bit(MouseButton::Back), bit(MouseButton::Left));
        assert_ne!(bit(MouseButton::Forward), bit(MouseButton::Middle));

        // Left goes down and stays down (a live drag).
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert_ne!(
            bits(&app) & bit(MouseButton::Left),
            0,
            "Left press reported"
        );
        // A thumb click lands and lifts entirely inside that drag.
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Back);
        assert_ne!(
            bits(&app) & bit(MouseButton::Back),
            0,
            "Back press reported"
        );
        assert_ne!(
            bits(&app) & bit(MouseButton::Left),
            0,
            "the thumb press must not disturb the live Left drag"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Back);
        assert_eq!(bits(&app) & bit(MouseButton::Back), 0, "Back bit cleared");
        assert_ne!(
            bits(&app) & bit(MouseButton::Left),
            0,
            "the thumb RELEASE must not clear the live drag's press bit — the \
             orphaned-release bug"
        );
        // The drag's own release still pairs.
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(bits(&app), 0, "every bit paired off");

        // Forward gets a third distinct slot, and `Other(_)` is dropped outright
        // (no bogus report for a device button aterm has no xterm code for).
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Forward);
        assert_eq!(bits(&app), bit(MouseButton::Forward));
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Other(9));
        assert_eq!(
            bits(&app),
            bit(MouseButton::Forward),
            "an unmapped device button touches no bit"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Forward);
        assert_eq!(bits(&app), 0);
        assert_eq!(super::winit_mouse_button(WinitMouseButton::Other(9)), None);
    }

    /// PETTING (wave 1): the hit test is pure and pads by the slop on every
    /// side, with the body's own edges staying right/bottom-exclusive.
    #[test]
    fn pet_rect_hit_pads_by_the_slop_on_every_side() {
        use super::{PET_HIT_SLOP_PX, pet_rect_hit};
        let r = (10, 20, 30, 40);
        assert!(pet_rect_hit(r, 10.0, 30.0, 0), "top-left corner is inside");
        assert!(!pet_rect_hit(r, 20.0, 30.0, 0), "right edge is exclusive");
        assert!(!pet_rect_hit(r, 10.0, 40.0, 0), "bottom edge is exclusive");
        assert!(
            pet_rect_hit(r, 6.0, 26.0, PET_HIT_SLOP_PX),
            "the slop reaches out past the body"
        );
        assert!(
            pet_rect_hit(r, 23.9, 43.9, PET_HIT_SLOP_PX),
            "on every side"
        );
        assert!(
            !pet_rect_hit(r, 5.9, 30.0, PET_HIT_SLOP_PX),
            "and no further"
        );
    }

    /// PETTING (wave 1), the chrome-wins policy end to end: a left press
    /// inside the stashed pet rect strokes the cat and is CONSUMED — it
    /// never starts a selection — while the same press outside the rect
    /// still runs the ordinary selection gesture.
    #[test]
    fn a_click_on_the_pet_pets_and_never_starts_a_selection() {
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Stash a drawn-pet rect the way the redraw does (frame px,
        // right/bottom exclusive).
        app.windows.get_mut(&wid).unwrap().pet_hit_rect = Some((100, 200, 100, 160));
        app.on_cursor_moved(wid, 150.0, 130.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        {
            let ws = app.windows.get(&wid).unwrap();
            assert_eq!(
                ws.cursor_pet.pending_pets(),
                1,
                "the press latched a pet (note, never act)"
            );
            assert!(!ws.selecting, "and was consumed before the selection layer");
        }
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            !app.windows.get(&wid).unwrap().selecting,
            "the orphan release is dropped (press/release stay paired)"
        );
        // The control: the same gesture outside the padded rect selects.
        app.on_cursor_moved(wid, 420.0, 300.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        {
            let ws = app.windows.get(&wid).unwrap();
            assert_eq!(
                ws.cursor_pet.pending_pets(),
                1,
                "no second pet off the body"
            );
            assert!(ws.selecting, "a plain terminal press still selects");
        }
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    }

    /// THE CONFIG-WARNING BANNER IS CHROME, and chrome wins the press.
    ///
    /// `splice_config_notice` overwrites the top grid rows IN PLACE and paints
    /// LAST, so it covers the tab strip as well — yet it was the one occluding
    /// surface with no mouse gate. A press on a warning about `columns/lines`
    /// therefore fell through to whatever it was standing on: a strip chip's `x`
    /// (which closes a tab outright), a selection, or a mouse-tracking app's
    /// stdin. It must dismiss the banner and stop there, while the same press
    /// BELOW the band keeps behaving exactly as it always did.
    #[test]
    fn a_click_on_the_config_warning_banner_dismisses_it_and_stops_there() {
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // Size the row buffer the way a redraw does: the banner's floor is clamped
        // by `input_scratch`'s row count, so an unsized buffer claims no band at all.
        {
            let terminal = app
                .front_terminal(wid)
                .expect("front terminal")
                .term
                .clone();
            let ws = app.windows.get_mut(&wid).expect("headless window");
            let mut term = crate::term_lock(&terminal);
            term.cell_frame_into(&mut ws.input_scratch, 24, 80);
        }
        let warning = || {
            crate::config_notice::ConfigNotice::new(
                vec!["columns/lines applies on next launch".to_string()],
                std::time::Instant::now(),
            )
        };
        app.config_notice = warning();
        let floor = app.config_notice_tray_floor_y(wid);
        assert!(floor > 0, "PRECONDITION: the banner must own a band");
        let (ox, oy) = app.frame_origin(wid);
        let x = ox as f64 + app.win_pad(wid) as f64 + 2.0;
        let band_top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;

        app.on_cursor_moved(wid, x, oy as f64 + band_top + 2.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.config_notice.is_none(),
            "the press on the banner dismissed it"
        );
        assert!(
            !app.windows.get(&wid).unwrap().selecting,
            "and was consumed above the selection layer"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            !app.windows.get(&wid).unwrap().selecting,
            "the orphan release is dropped (press/release stay paired)"
        );

        // The control: a press one row BELOW the painted band is not the banner's,
        // so it reaches the terminal and leaves the notice up.
        app.config_notice = warning();
        app.on_cursor_moved(wid, x, oy as f64 + f64::from(floor) + 2.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.config_notice.is_some(),
            "a press under the band must not dismiss what it never touched"
        );
        assert!(
            app.windows.get(&wid).unwrap().selecting,
            "and still starts an ordinary selection"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    }

    /// PETTING (wave 1): a cleared stash (pet not drawn) can never eat a
    /// click — the guard for stale rects after a style switch or fade-out.
    #[test]
    fn no_rect_no_pet_the_click_falls_through() {
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert_eq!(app.windows.get(&wid).unwrap().pet_hit_rect, None);
        app.on_cursor_moved(wid, 150.0, 130.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(ws.cursor_pet.pending_pets(), 0, "nothing to pet");
        assert!(ws.selecting, "the press reached the selection layer");
    }

    /// DISMISSING ROBI, chrome-wins end to end: a left press inside the
    /// stashed body rect is CONSUMED (never a selection), spends the rect
    /// immediately, and routes the `robi = false` intent into the versioned
    /// settings lane — which the headless harness (no event-loop proxy)
    /// refuses synchronously, so the refusal surfacing as a config-notice
    /// banner is the proof the persist path was really invoked (and a
    /// refusal arms NO latch: he stays, the banner says why). The same press
    /// outside the padded rect still runs the ordinary selection gesture.
    #[test]
    fn a_click_on_robi_dismisses_him_and_never_selects() {
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows.get_mut(&wid).unwrap().robi_hit_rect = Some((100, 200, 100, 160));
        app.on_cursor_moved(wid, 150.0, 130.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        {
            let ws = app.windows.get(&wid).unwrap();
            assert!(!ws.selecting, "consumed before the selection layer");
            assert_eq!(ws.robi_hit_rect, None, "the rect is spent by the press");
        }
        assert!(
            app.config_notice.is_some(),
            "the persist attempt reached the settings lane (headless refusal is surfaced)"
        );
        assert!(
            app.robi_dismissal.is_none(),
            "a synchronous refusal arms no latch — nothing is in flight"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            !app.windows.get(&wid).unwrap().selecting,
            "the orphan release is dropped (press/release stay paired)"
        );
        // The control: the same gesture outside the padded rect selects (the
        // notice claims no band here — headless leaves `input_scratch` unsized).
        app.windows.get_mut(&wid).unwrap().robi_hit_rect = Some((100, 200, 100, 160));
        app.on_cursor_moved(wid, 420.0, 300.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows.get(&wid).unwrap().selecting,
            "a plain terminal press still selects"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    }

    /// DISMISSING ROBI: a cleared stash (Robi not drawn) can never eat a
    /// click — the guard for stale rects after the gate closes.
    #[test]
    fn no_rect_no_robi_the_click_falls_through() {
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert_eq!(app.windows.get(&wid).unwrap().robi_hit_rect, None);
        app.on_cursor_moved(wid, 150.0, 130.0);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        let ws = app.windows.get(&wid).unwrap();
        assert!(ws.selecting, "the press reached the selection layer");
        assert!(
            app.config_notice.is_none(),
            "and no dismissal was ever attempted"
        );
    }

    /// DISMISSING ROBI, the latch's settlement law (`poll_robi_dismissal`):
    /// a FAILED lane completion releases the latch AND banners (a click that
    /// did nothing is never silent — Robi walks back on as visible proof); a
    /// SUCCESSFUL one holds it until `robi = false` is the live config (else
    /// the gate would read the old `robi = true` in the completion-to-
    /// generation gap and re-birth him), then releases so it can never
    /// override a later Settings re-enable.
    #[test]
    fn the_dismissal_latch_settles_by_lane_outcome() {
        use crate::{App, RobiDismissal};

        let mut app = App::headless_for_test();
        // The latch's precondition: a clicked Robi was an ENABLED Robi (the
        // shipped default is off, so the enable is explicit here — without
        // it `poll` reads `robi_or_default() == false` and releases early).
        app.config.robi = Some(true);
        // Failure completion (the OCC-conflict shape): banner + release.
        let (tx, rx) = std::sync::mpsc::channel();
        app.robi_dismissal = Some(RobiDismissal::InFlight(rx));
        tx.send(Err(
            "save conflict for robi at config revision 7".to_string()
        ))
        .unwrap();
        app.poll_robi_dismissal();
        assert!(
            app.robi_dismissal.is_none(),
            "a failed write releases the latch — Robi returns"
        );
        assert!(
            app.config_notice.is_some(),
            "…and the failure is surfaced, never silent"
        );
        // A still-pending write keeps the latch armed…
        app.config_notice = None;
        let (tx, rx) = std::sync::mpsc::channel();
        app.robi_dismissal = Some(RobiDismissal::InFlight(rx));
        app.poll_robi_dismissal();
        assert!(
            matches!(app.robi_dismissal, Some(RobiDismissal::InFlight(_))),
            "an empty channel settles nothing"
        );
        // …a success holds it (config still says `robi = true`)…
        tx.send(Ok("saved: robi = false".to_string())).unwrap();
        app.poll_robi_dismissal();
        assert!(
            matches!(app.robi_dismissal, Some(RobiDismissal::AwaitingConfig)),
            "success waits for the generation, not the reply"
        );
        assert!(app.config_notice.is_none(), "success needs no banner");
        // …and it releases the moment the dismissal IS the live config.
        app.config.robi = Some(false);
        app.poll_robi_dismissal();
        assert!(
            app.robi_dismissal.is_none(),
            "the landed config owns the gate"
        );
    }

    /// DISMISSING ROBI vs HIS OWN BUBBLE: the tip bubble is the app-global
    /// transient notice, and `notice_click` runs EARLIER in `on_mouse_input`
    /// than `robi_press_at` — so a press on the bubble dismisses the bubble
    /// and never costs the robot, even where the card overlaps his padded
    /// body (it sits right over his head). This pins the one ordering fact
    /// unique to Robi; reorder the chrome chain and this test names the
    /// regression: a bubble tap would write `robi = false`.
    #[test]
    fn a_press_on_robis_tip_bubble_dismisses_the_bubble_not_the_robot() {
        use crate::{App, WindowId};
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // A tip mid-hold (past the entrance ramp, so it is clickable),
        // anchored the way the redraw anchors it: over the speaker.
        let spawned = std::time::Instant::now() - std::time::Duration::from_secs(2);
        app.notice = Some(crate::notice::TransientNotice::robi_tip(
            aterm_effects::robi::ROBI_TIPS[0].text,
            Some((160.0, 140.0)),
            spawned,
        ));
        // On glass in this window (what the splice records when it paints).
        app.windows.get_mut(&wid).unwrap().notice_card = Some(crate::SettingsCard {
            rgba: Vec::new(),
            pw: 0,
            ph: 0,
            dx: 0,
            dy: 0,
            fp: 0,
            geom: 0,
        });
        // Resolve the card's live rect through the SAME seams the click path
        // reads, and aim for its center.
        let now = std::time::Instant::now();
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f32;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f32;
        let geom = crate::settings::SettingsGeom {
            cw: cw as f32,
            ch: ch as f32,
            font_px: app.win_font_px(wid),
            cols: app.windows.get(&wid).unwrap().cols as usize,
            panel_rows: 0,
        };
        let motion = app
            .motion_policy(true)
            .amplitude(crate::motion::MotionEffect::NoticePill);
        let (rx, ry, rw, rh) = crate::notice::notice_rect(
            app.notice.as_ref().unwrap(),
            &geom,
            now,
            motion,
            app.notice_clear_rows(),
        );
        assert!(
            rw > 0.0 && rh > 0.0,
            "the card must be drawable to be clickable"
        );
        let (click_x, click_y) = (
            f64::from(pad + rx + rw * 0.5),
            f64::from(top + ry + rh * 0.5),
        );
        // Robi's stashed body DIRECTLY under that same point: if the bubble
        // did not win by order, this press would dismiss him.
        let body = (
            click_x as i32 - 20,
            click_x as i32 + 20,
            click_y as i32 - 20,
            click_y as i32 + 20,
        );
        app.windows.get_mut(&wid).unwrap().robi_hit_rect = Some(body);
        app.on_cursor_moved(wid, click_x, click_y);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(app.notice.is_none(), "the press dismissed the bubble");
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(
            ws.robi_hit_rect,
            Some(body),
            "…and never reached the robot (the rect is not spent)"
        );
        assert!(
            app.robi_dismissal.is_none(),
            "no `robi = false` write was queued"
        );
        assert!(!ws.selecting, "and no selection started");
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    }

    /// C5 — the RIGHT-press gesture, end to end through the real handler.
    ///
    /// Three things are pinned here and nowhere else: a right press on a chip
    /// POPS the composed menu; that press never reaches the grid (the
    /// bogus-cell fall-through the plan's `Chrome` arm exists to stop, now with
    /// something behind it); and a left press AWAY from the card dismisses it
    /// without also acting on whatever was underneath.
    ///
    /// The in-grid-strip platforms, mirroring the opener's own gate: macOS
    /// chips carry a real `NSMenu` instead (see the `Chrome` arm).
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_right_press_on_a_chip_pops_the_menu_and_never_reaches_the_grid() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        // Lay the strip out so `tab_segments` is real (the hit-test reads it).
        app.splice_tab_strip_with(wid, 1);
        let chip = app.windows[&wid]
            .tab_segments
            .iter()
            .find_map(|seg| match seg.kind {
                crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 1),
                _ => None,
            })
            .expect("the lone tab has a chip");
        let chip_pt = (
            pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        assert!(
            app.strip_col_at(wid, chip_pt.0, chip_pt.1).is_some(),
            "fixture point must be ON the strip"
        );

        app.on_cursor_moved(wid, chip_pt.0, chip_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
        let menu = app.windows[&wid]
            .tab_menu
            .as_ref()
            .expect("the right press popped the tab's menu");
        assert!(
            menu.entries
                .iter()
                .any(|e| matches!(e, crate::session_chrome::TabMenuEntry::Action { .. })),
            "…carrying the composed model's action rows"
        );
        // THE bogus-cell witness: `reported_buttons` is set by (and only by)
        // the seam, at the end of `on_mouse_input`. A zero here is the proof
        // that the press never reached the grid — the audit's "reported into
        // the terminal at a bogus cell under a tracking TUI".
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "a press on chrome is never reported to the terminal"
        );

        // A left press on the `+` dismisses the card and is CONSUMED: the
        // dismissing gesture must not also do what it was pointing at (here,
        // open a tab) — the click-away rule.
        let plus = app.windows[&wid]
            .tab_segments
            .iter()
            .find_map(|seg| (seg.kind == crate::tab_bar::TabHit::NewTab).then_some(seg.start_col))
            .expect("a one-tab strip still offers the + segment");
        let tabs_before = app.windows[&wid].tab_set.len();
        let plus_pt = (
            pad + f64::from(plus) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        app.on_cursor_moved(wid, plus_pt.0, plus_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].tab_menu.is_none(),
            "click-away dismissed the card"
        );
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            tabs_before,
            "…and the dismissing press did NOT also open a tab"
        );
        assert_eq!(app.windows[&wid].reported_buttons, 0);
    }

    /// C5, the BARE band — the system menu's press/release custody, the
    /// ordering the fix lives or dies on: the right PRESS on the bare band
    /// must NOT pop the menu (winit still holds that press's mouse capture,
    /// and DefWindowProc discards a menu entered under capture — the
    /// machine-click-only bug), only ARM `band_menu_release_pending`; the
    /// matching RELEASE spends the latch, and the popup post sits behind that
    /// spend (`on_mouse_input`'s completion block — headless windows have no
    /// HWND, so the latch transition is the unit-observable half; the glass
    /// probe photographs the #32768 menu itself). Windows-only like the latch:
    /// Linux's bare band keeps its pure swallow.
    #[cfg(windows)]
    #[test]
    fn the_bare_band_system_menu_arms_on_press_and_fires_on_release() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        // A strip column PAST every segment: `hit_test` answers None there —
        // the bare band, the caption's naked stretch.
        let bare = app.windows[&wid]
            .tab_segments
            .iter()
            .map(|seg| seg.end_col)
            .max()
            .expect("the strip laid out segments")
            + 2;
        let bare_pt = (
            pad + f64::from(bare) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        assert!(
            app.strip_col_at(wid, bare_pt.0, bare_pt.1).is_some(),
            "fixture point must be ON the strip"
        );
        assert!(
            crate::tab_bar::hit_test(&app.windows[&wid].tab_segments, bare).is_none(),
            "fixture point must be the BARE band, not a chip or control"
        );

        app.on_cursor_moved(wid, bare_pt.0, bare_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
        assert!(
            app.windows[&wid].band_menu_release_pending,
            "the PRESS armed the latch instead of popping — a popup posted \
             here is discarded under winit's mouse capture"
        );
        assert!(
            app.windows[&wid].tab_menu.is_none(),
            "the bare band pops the SYSTEM menu, never the tab card"
        );
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "a press on chrome is never reported to the terminal"
        );

        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
        assert!(
            !app.windows[&wid].band_menu_release_pending,
            "the RELEASE spent the latch — the popup post lives behind this \
             spend, after winit's ReleaseCapture"
        );
        assert_eq!(
            app.windows[&wid].reported_buttons, 0,
            "…and the consumed release leaked nothing to the terminal"
        );

        // CONTRAST: a right press on a CHIP arms nothing — the chip owns the
        // tab card, and the system menu must not shadow it.
        let chip = app.windows[&wid]
            .tab_segments
            .iter()
            .find_map(|seg| match seg.kind {
                crate::tab_bar::TabHit::Select(0) => Some(seg.start_col + 1),
                _ => None,
            })
            .expect("the lone tab has a chip");
        let chip_pt = (
            pad + f64::from(chip) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        app.on_cursor_moved(wid, chip_pt.0, chip_pt.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Right);
        assert!(
            !app.windows[&wid].band_menu_release_pending,
            "a chip press must not arm the system menu"
        );
        assert!(
            app.windows[&wid].tab_menu.is_some(),
            "…because it pops the tab card"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Right);
    }

    /// THE `+` HOVER, END TO END. The strip's primary affordance had no pointer
    /// feedback: `strip_hover` is an `Option<usize>` of TAB indices and the `+`
    /// is not a tab, so the per-motion hit test threw the `NewTab` arm away and
    /// hovering the button changed exactly zero pixels. Three links are pinned
    /// here, because breaking any one of them puts that silence back:
    /// the motion hook records the button (and clears it on leaving), the strip
    /// CACHE KEY carries the flag (or the repaint is a hit and the wash never
    /// reaches the rows), and the painted row actually moves.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn hovering_the_new_tab_button_records_it_and_repaints_the_strip() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let seg = |want_plus: bool| {
            app.windows[&wid]
                .tab_segments
                .iter()
                .find(|s| matches!(s.kind, crate::tab_bar::TabHit::NewTab) == want_plus)
                .copied()
                .expect("the strip laid out both a chip and a +")
        };
        let plus = seg(true);
        let chip = seg(false);
        let point = |col: u16| {
            (
                pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
                top + ch as f64 * 0.5,
            )
        };
        let button_col = plus.start_col + 1;
        // Reads the LIVE painted row, so it takes `app` as an argument rather
        // than capturing it — the pointer events below need `&mut app`.
        let bg_of =
            |app: &crate::App, col: u16| app.windows[&wid].cached_strip_rows[0][col as usize].bg;
        let resting = bg_of(&app, button_col);

        // (1) the motion hook records the button…
        let p = point(button_col);
        app.on_cursor_moved(wid, p.0, p.1);
        assert!(
            app.windows[&wid].strip_hover_new_tab,
            "the pointer is on the `+`"
        );
        assert_eq!(
            app.windows[&wid].strip_hover, None,
            "…and the `+` is not a tab, so no chip is hovered"
        );

        // (2) …the cache key carries it, so the next splice REBUILDS…
        // (3) …and the rebuilt row shows the wash.
        app.splice_tab_strip_with(wid, 1);
        let hot = bg_of(&app, button_col);
        assert_ne!(
            resting, hot,
            "the repaint must reach the rows — a cache key without the flag \
             would hit and paint the resting button forever"
        );
        assert_eq!(
            hot,
            crate::tab_bar::strip_hover_bg_for_test(app.chrome_palette_theme()),
            "the button takes the band's own hover material"
        );

        // Leaving for a CHIP clears the button and lights the tab instead.
        let q = point(chip.start_col + 1);
        app.on_cursor_moved(wid, q.0, q.1);
        assert!(
            !app.windows[&wid].strip_hover_new_tab,
            "leaving the `+` clears its wash"
        );
        assert_eq!(app.windows[&wid].strip_hover, Some(0));
        app.splice_tab_strip_with(wid, 1);
        assert_eq!(
            bg_of(&app, button_col),
            resting,
            "…and the button is back to resting"
        );
    }

    /// THE POINTER LEAVING THE WINDOW CLEARS THE STRIP'S HOVER. `CursorMoved` is
    /// the only thing that drives [`App::track_strip_hover`], so a pointer that
    /// walks off the window simply stops reporting and whatever was lit at the
    /// last motion INSIDE stays lit. Measured on glass (2026-08-25, windowed,
    /// three tabs, font_px 13): the `+` kept its `hover_bg` wash while the pointer
    /// sat 236 px outside the window's right edge. Both hover states are pinned
    /// here — a chip's (which reveals a `✕` on a tab nobody is pointing at) and
    /// the `+`'s — plus the redraw that carries the clear to the rows.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn the_pointer_leaving_the_window_clears_both_strip_hovers() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        app.splice_tab_strip_with(wid, 1);
        let seg = |want_plus: bool| {
            app.windows[&wid]
                .tab_segments
                .iter()
                .find(|s| matches!(s.kind, crate::tab_bar::TabHit::NewTab) == want_plus)
                .copied()
                .expect("the strip laid out both a chip and a +")
        };
        let plus = seg(true);
        let chip = seg(false);
        let point = |col: u16| {
            (
                pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
                top + ch as f64 * 0.5,
            )
        };
        let button_col = plus.start_col + 1;
        let bg_of =
            |app: &crate::App, col: u16| app.windows[&wid].cached_strip_rows[0][col as usize].bg;
        let resting_button = bg_of(&app, button_col);
        let resting_chip = bg_of(&app, chip.start_col + 1);

        // The `+` lit, then the pointer leaves.
        let p = point(button_col);
        app.on_cursor_moved(wid, p.0, p.1);
        assert!(app.windows[&wid].strip_hover_new_tab);
        app.splice_tab_strip_with(wid, 1);
        assert_ne!(resting_button, bg_of(&app, button_col), "the `+` is lit");
        app.on_cursor_left(wid);
        assert!(
            !app.windows[&wid].strip_hover_new_tab,
            "leaving the window clears the `+` wash"
        );
        app.splice_tab_strip_with(wid, 1);
        assert_eq!(
            bg_of(&app, button_col),
            resting_button,
            "…and the clear reaches the painted row"
        );

        // A CHIP lit, then the pointer leaves. The harness window has ONE tab and
        // that tab is the SELECTED one, whose card and `✕` are the same painted
        // either way ([`crate::tab_bar::strip_cell`]: hover never overrides the
        // selection, and the chip-card band keeps the selected `✕` resident), so
        // the chip's half is pinned at the STATE — which is what feeds the wash
        // and the hover-only `✕` on every unselected chip.
        let _ = resting_chip;
        let q = point(chip.start_col + 1);
        app.on_cursor_moved(wid, q.0, q.1);
        assert_eq!(
            app.windows[&wid].strip_hover,
            Some(0),
            "the chip is hovered"
        );
        app.on_cursor_left(wid);
        assert_eq!(
            app.windows[&wid].strip_hover, None,
            "leaving the window clears the chip's wash and its `✕`"
        );
    }

    /// I10/I11: the hover cursor resolves BY LOCATION. Over plain grid cells the
    /// window advertises the I-BEAM state (`native_text_cursor`, the surface's
    /// primary gesture being text selection); while a VT mouse-tracking app owns
    /// the pointer it reverts to the arrow (both bools clear — presses are
    /// REPORTED then, not selecting); over a split divider it advertises the
    /// RESIZE state (the borrowed `(pointer, text) = (true, true)` encoding); and
    /// the modifier-change caller has no pointer event, so a bare Ctrl tap with
    /// the pointer parked over the TAB STRIP must paint no I-beam. Headless
    /// windows have no OS cursor, so the assertions read the two `WindowState`
    /// bools the `set_cursor` write is keyed from.
    #[test]
    fn hover_cursor_resolves_grid_tracking_divider_and_strip_by_location() {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let strip_px = app.tab_strip_rows as f64 * ch as f64;
        // A fixture point WELL inside the grid: below any strip band, ~10 cols in.
        let grid = (pad + cw as f64 * 10.5, pad + strip_px + ch as f64 * 5.5);
        assert!(
            app.strip_col_at(wid, grid.0, grid.1).is_none(),
            "fixture point must be off the strip"
        );
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_cursor_px = grid;
            ws.last_mouse_window_cell = (5, 10);
        }

        // Plain grid, tracking off: the I-beam state.
        app.update_hover_cursor(wid);
        let state = |app: &crate::App| {
            let ws = &app.windows[&wid];
            (ws.hover_pointer, ws.native_text_cursor)
        };
        assert_eq!(state(&app), (false, true), "grid = I-beam");

        // A VT mouse-tracking app owns the pointer: revert to the arrow.
        let term = app.front_terminal(wid).expect("terminal").term.clone();
        crate::term_lock(&term).process(b"\x1b[?1000h");
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, false), "tracking = arrow");
        crate::term_lock(&term).process(b"\x1b[?1000l");
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, true), "tracking off = I-beam again");

        // Split 1|2 (divider at window col 40): hovering the seam advertises the
        // resize cursor BEFORE any drag — the whole point of I11.
        let _sid = app.split_active_stub_tab(wid);
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (5, 40);
        }
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (true, true), "divider = resize encoding");
        // Back off the seam: the grid I-beam returns.
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.last_mouse_window_cell = (5, 10);
        }
        app.update_hover_cursor(wid);
        assert_eq!(state(&app), (false, true), "off the seam = I-beam");

        // THE TRAP the audit named: `on_modifiers_changed` calls the resolver
        // with no pointer event. With the pointer parked over the tab strip, a
        // bare Ctrl tap must resolve the STRIP (arrow), not paint an I-beam over
        // chrome. `headless_for_test` seeds `tab_strip_rows: 0` (the macOS
        // default), so enable the one-row strip the Windows/Linux default ships.
        {
            app.tab_strip_rows = 1;
            let strip_px = ch as f64;
            let strip_pt = (pad + cw as f64 * 2.5, pad + strip_px * 0.5);
            assert!(
                app.strip_col_at(wid, strip_pt.0, strip_pt.1).is_some(),
                "fixture point must be ON the strip"
            );
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.last_cursor_px = strip_pt;
            }
            app.on_modifiers_changed(wid, winit::keyboard::ModifiersState::CONTROL);
            assert_eq!(
                state(&app),
                (false, false),
                "a Ctrl tap over the strip paints chrome's arrow, not an I-beam"
            );
            app.on_modifiers_changed(wid, winit::keyboard::ModifiersState::empty());
        }
    }

    // ------------------------------------------------------------------
    // DRAG A TAB TO REORDER IT — the strip's held-chip gesture, driven end to
    // end through the real handlers (`on_mouse_input` press → `on_cursor_moved`
    // motion → release), so these pin the mapping from PIXELS to a reorder and
    // not merely the FSM. Everything here runs against the real laid-out
    // `ws.tab_segments`.
    //
    // Three tabs on the harness's 80-column strip share ~25 cells each, which is
    // comfortably above `PREFERRED_MIN_TAB_COLS`, so the layout is
    // selection-independent (see `layout_segments`): a step moves the chips and
    // nothing else, which is what lets these tests read the spans once.
    // ------------------------------------------------------------------

    /// The window's canonical tab order, by stable id.
    fn tab_order(app: &crate::App, wid: crate::WindowId) -> Vec<crate::tab_model::TabId> {
        app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|tab| tab.id)
            .collect()
    }

    /// The half-open column span of chip `index` on the laid-out strip.
    fn chip_span(app: &crate::App, wid: crate::WindowId, index: usize) -> (u16, u16) {
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == crate::tab_bar::TabHit::Select(index))
            .expect("the tab has a laid-out chip");
        (seg.start_col, seg.end_col)
    }

    /// The window pixel at the centre of strip column `col`.
    fn strip_point(app: &crate::App, wid: crate::WindowId, col: u16) -> (f64, f64) {
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let top = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        let point = (
            pad + f64::from(col) * cw as f64 + cw as f64 * 0.5,
            top + ch as f64 * 0.5,
        );
        assert_eq!(
            app.strip_col_at(wid, point.0, point.1),
            Some(col),
            "fixture point must land on strip column {col}"
        );
        point
    }

    /// A three-tab window with the one-row in-grid strip laid out.
    fn app_with_three_chips() -> (crate::App, crate::WindowId, Vec<crate::tab_model::TabId>) {
        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        let order = tab_order(&app, wid);
        assert_eq!(order.len(), 3, "three chips to shuffle");
        (app, wid, order)
    }

    /// THE GESTURE: press a chip, sweep past the next chip's midpoint, release —
    /// the tabs swap, and the tab you dragged is still the one selected.
    #[test]
    fn dragging_a_chip_past_its_neighbour_reorders_the_strip() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let grab = strip_point(&app, wid, chip_span(&app, wid, 0).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "the press SELECTS and nothing more — reordering is the motion's job"
        );
        assert_eq!(app.windows[&wid].tabs.active, 0, "…and it selected chip 0");

        // Past the neighbour's midpoint: the crossing that steps.
        let (start, end) = chip_span(&app, wid, 1);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

        assert_eq!(
            tab_order(&app, wid),
            vec![before[1], before[0], before[2]],
            "the dragged chip swapped with the one it crossed"
        );
        assert_eq!(
            app.windows[&wid].tabs.active, 1,
            "a reorder must not silently switch tabs: the dragged tab is still selected"
        );
        assert!(
            app.windows[&wid].strip_drag.is_none(),
            "the release disarmed the gesture"
        );
    }

    /// The same gesture LEFTWARD, and across TWO chips in one sweep: a coalesced
    /// motion that crosses two midpoints takes two steps, not one.
    #[test]
    fn a_sweep_across_two_chips_walks_the_tab_two_places() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let grab = strip_point(&app, wid, chip_span(&app, wid, 2).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);

        // One motion event, landing on the FIRST chip's leading half.
        let over = strip_point(&app, wid, chip_span(&app, wid, 0).0);
        app.on_cursor_moved(wid, over.0, over.1);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

        assert_eq!(
            tab_order(&app, wid),
            vec![before[2], before[0], before[1]],
            "the dragged chip walked all the way to the front"
        );
        assert_eq!(app.windows[&wid].tabs.active, 0);
    }

    /// A PRESS WITH NO MOTION MUST NOT REORDER — and neither does a sweep that
    /// stays inside the held chip's own share. The midpoint rule is what makes
    /// that structural: a neighbour's midpoint is strictly beyond the held
    /// segment's far edge, so there is no crossing until the pointer travels.
    #[test]
    fn a_press_that_never_crosses_a_midpoint_never_reorders() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let (start, end) = chip_span(&app, wid, 0);
        let grab = strip_point(&app, wid, start + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].strip_drag.is_some(),
            "the chip IS held — this test is about what a held chip does NOT do"
        );

        // A hand resting on the mouse: sub-cell jitter that never leaves the
        // pressed column.
        app.on_cursor_moved(wid, grab.0 + 0.25, grab.1 + 0.25);
        assert_eq!(tab_order(&app, wid), before, "jitter is not a drag");

        // And a full sweep to the far end of the chip's OWN share.
        let inside = strip_point(&app, wid, end - 1);
        app.on_cursor_moved(wid, inside.0, inside.1);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "travel within the chip's own share crosses nothing"
        );
    }

    /// THE RELEASE ENDS IT. After the button is up, sweeping the pointer across
    /// the whole strip is a bare hover — it reveals `✕`s and reorders nothing.
    #[test]
    fn a_bare_hover_after_the_release_never_reorders() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let grab = strip_point(&app, wid, chip_span(&app, wid, 0).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

        for index in [1usize, 2, 1, 0] {
            let (start, end) = chip_span(&app, wid, index);
            let point = strip_point(&app, wid, start + (end - start) / 2);
            app.on_cursor_moved(wid, point.0, point.1);
        }
        assert_eq!(
            tab_order(&app, wid),
            before,
            "hovering the strip with no button held moves nothing"
        );
    }

    /// FOCUS LOSS DROPS IT TOO — an alt-tab mid-drag can leave the release
    /// undelivered, and a wedged gesture would make every later hover shuffle.
    #[test]
    fn focus_loss_drops_a_held_chip() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let grab = strip_point(&app, wid, chip_span(&app, wid, 0).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        app.on_focus(wid, false);
        assert!(app.windows[&wid].strip_drag.is_none());

        let (start, end) = chip_span(&app, wid, 1);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        assert_eq!(tab_order(&app, wid), before);
    }

    /// THE BAND'S OWN PRESS-DRAG IS UNTOUCHED: a press on bare strip background
    /// is the window-move / maximize gesture (W2), not a grab on a chip, so it
    /// arms no reorder — and sweeping from there moves no tab.
    #[test]
    fn a_press_on_the_bare_band_arms_no_reorder() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        // Past the trailing `+`, short of the strip's right edge: genuinely bare.
        let band = app.windows[&wid]
            .tab_segments
            .iter()
            .map(|seg| seg.end_col)
            .max()
            .expect("a laid-out strip");
        assert!(
            crate::tab_bar::hit_test(&app.windows[&wid].tab_segments, band).is_none(),
            "fixture column must be bare band"
        );
        let point = strip_point(&app, wid, band);
        app.on_cursor_moved(wid, point.0, point.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].strip_drag.is_none(),
            "the band's press-drag is a WINDOW move, never a tab grab"
        );

        let (start, end) = chip_span(&app, wid, 0);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(tab_order(&app, wid), before);
    }

    /// ANY OTHER STRIP PRESS DROPS THE HELD CHIP. The one that can actually
    /// arrive with the left button still down is the RIGHT press that pops the
    /// tab's context menu (`open_tab_context_menu` → `clear_strip_press`): the
    /// card owns the pointer from then on, and motion under it must not keep
    /// shuffling the strip it is anchored to.
    #[test]
    fn opening_the_context_menu_drops_a_held_chip() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let (grab_start, _) = chip_span(&app, wid, 0);
        let grab = strip_point(&app, wid, grab_start + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(app.windows[&wid].strip_drag.is_some(), "the chip is held");

        assert!(
            app.open_tab_context_menu(wid, 0, grab_start + 1, false),
            "the chip has a composed menu"
        );
        assert!(
            app.windows[&wid].strip_drag.is_none(),
            "popping the card dropped the grab"
        );

        let (start, end) = chip_span(&app, wid, 1);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "motion under the open card reorders nothing"
        );
    }

    /// A stale press (past the double-click window) on the chip whose inline
    /// RENAME editor is open must not arm a drag: that press is "keep editing"
    /// (the editor stays), and a drag would reshuffle the strip out from under
    /// the live field. Only the repeat press was excluded before this pin.
    #[test]
    fn a_press_on_the_chip_being_renamed_arms_no_drag() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        assert!(
            app.begin_session_rename(wid, before[0]),
            "fixture guard: the editor opened on chip 0"
        );
        let grab = strip_point(&app, wid, chip_span(&app, wid, 0).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].strip_drag.is_none(),
            "a press on the edited chip keeps editing — it never arms a drag"
        );
        assert!(
            app.inline_rename_edit(wid).is_some(),
            "…and the editor is still open"
        );
        let (start, end) = chip_span(&app, wid, 1);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "a sweep past a midpoint moves nothing under the field"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
    }

    /// A palette opened by KEYBOARD while a chip is held: the modal owns the
    /// window even though it never claimed the pointer, so the held chip
    /// freezes — and the release still disarms through the hoisted disarm.
    #[test]
    fn a_palette_opened_mid_drag_freezes_the_held_chip() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let (mut app, wid, before) = app_with_three_chips();
        let grab = strip_point(&app, wid, chip_span(&app, wid, 0).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert!(app.windows[&wid].strip_drag.is_some(), "the chip is held");

        app.toggle_palette();
        assert!(
            app.windows[&wid].palette().is_some(),
            "fixture guard: the palette really opened"
        );
        let (start, end) = chip_span(&app, wid, 1);
        let over = strip_point(&app, wid, start + (end - start) / 2);
        app.on_cursor_moved(wid, over.0, over.1);
        assert_eq!(
            tab_order(&app, wid),
            before,
            "motion under the open palette reorders nothing"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert!(
            app.windows[&wid].strip_drag.is_none(),
            "the release disarmed through the hoisted disarm"
        );
    }

    /// A LONE CHIP HAS NOWHERE TO GO: one tab, dragged the length of its own
    /// share, is a no-op rather than a panic or a phantom move.
    #[test]
    fn a_lone_chip_has_nowhere_to_go() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        app.splice_tab_strip(wid);
        let before = tab_order(&app, wid);
        assert_eq!(before.len(), 1);

        let (start, end) = chip_span(&app, wid, 0);
        let grab = strip_point(&app, wid, start);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        for col in start..end {
            let point = strip_point(&app, wid, col);
            app.on_cursor_moved(wid, point.0, point.1);
        }
        // The anchor is the witness: it is re-anchored by (and only by) a step,
        // so a lone chip that never stepped still carries its press column. This
        // catches a neighbour lookup that falls back to the held segment — where
        // `move_tab`'s own range guard would silently absorb the bogus move.
        assert_eq!(
            app.windows[&wid].strip_drag.map(|drag| drag.origin_col),
            Some(start),
            "a lone chip has no neighbour to cross, so it never took a step"
        );
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);
        assert_eq!(tab_order(&app, wid), before);
        assert_eq!(app.windows[&wid].tab_set.len(), 1);
    }

    /// THE PRESSURE REGIME (the equal share falls under the legibility floor):
    /// eight tabs on the 80-column harness strip cannot all have a legible chip,
    /// so `layout_segments` stops compressing and seats a PAGE of four
    /// (`tab_bar::strip_window`). The march below models the real frame loop
    /// (`move_tab` requests a redraw; the repaint re-records `ws.tab_segments`)
    /// by re-splicing after every motion, and walks the held chip from the front
    /// of the strip to the back one neighbour at a time — the self-stabilisation
    /// `advance_strip_drag`'s doc analyses, finally pinned by a test.
    ///
    /// WHAT CHANGED WITH THE PAGE, and why this test had to be re-derived rather
    /// than relaxed. The window used to be centred on the selection, which made
    /// two things true that are no longer true and that this test was reading as
    /// its subject:
    ///
    /// - The held chip was WIDER than its neighbours (the active-priority
    ///   reserve). A page does not compress anybody, so its chips are equal —
    ///   the pressure signature below is now "the strip seats fewer chips than
    ///   it has tabs, all of one width", which is the same fact about the same
    ///   regime, read off the thing the regime actually does.
    /// - The next neighbour was ALWAYS seated, because the window followed the
    ///   held tab. On a paged strip the neighbour across a page edge has no
    ///   chip, so the march crosses that edge on `strip_drag_step`'s
    ///   off-strip rule — the pointer leaving the held chip — and the page turns
    ///   under it. The march therefore reaches for the neighbour's span only
    ///   while the neighbour is seated, and drives the pointer past the held
    ///   chip's own end otherwise.
    ///
    /// It starts at the first chip the PAGE seats rather than at tab 0: a tab
    /// the strip does not paint has no pixel to press, and the drag gesture is
    /// defined on the chips a hand can actually reach.
    #[test]
    fn under_pressure_the_wide_active_chip_marches_across_the_strip() {
        use winit::event::{ElementState, MouseButton as WinitMouseButton};

        let mut app = crate::App::headless_for_test();
        let wid = crate::WindowId(0);
        app.tab_strip_rows = 1;
        for _ in 0..7 {
            app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        }
        app.splice_tab_strip(wid);
        let before = tab_order(&app, wid);
        assert_eq!(before.len(), 8, "eight chips to force the pressure layout");

        // The first chip the WINDOW seats — the leftmost one a hand can reach.
        let seated = |app: &crate::App| -> Vec<usize> {
            app.windows[&wid]
                .tab_segments
                .iter()
                .filter_map(|seg| match seg.kind {
                    crate::tab_bar::TabHit::Select(index) => Some(index),
                    _ => None,
                })
                .collect()
        };
        let held = seated(&app)[0];
        assert!(
            seated(&app).len() < before.len(),
            "eight tabs on an 80-column strip must be more than it can seat"
        );

        // Grab it: the press selects it, and the band is re-recorded around the
        // page that selection is on.
        let grab = strip_point(&app, wid, chip_span(&app, wid, held).0 + 1);
        app.on_cursor_moved(wid, grab.0, grab.1);
        app.on_mouse_input(wid, ElementState::Pressed, WinitMouseButton::Left);
        assert_eq!(
            app.windows[&wid].tabs.active, held,
            "the press selected the chip under it"
        );
        app.splice_tab_strip(wid); // the repaint the selection requested

        // The PRESSURE SIGNATURE, on the freshly recorded segments: the strip
        // seats FEWER chips than it has tabs, and each of them is wider than an
        // equal split of the band would have been — the strip stopped
        // compressing and paged instead. If this fails, the harness is no longer
        // testing the regime it names.
        let width = |index: usize| {
            let (start, end) = chip_span(&app, wid, index);
            end - start
        };
        let widths: Vec<u16> = seated(&app).iter().map(|&i| width(i)).collect();
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "a seated page divides its band equally: {widths:?}"
        );
        assert!(
            widths[0] > 80 / before.len() as u16,
            "eight tabs on this strip must engage the pressure layout: a seated \
             chip ({} cols) has to beat the equal eighth it refused to shrink to",
            widths[0]
        );

        // March the held chip past its neighbour, over and over, to the far end
        // — re-splicing after every motion exactly as the runtime repaints after
        // one. Each motion advances it by AT LEAST one slot: crossing a PAGE
        // edge turns the page under the pointer, and `advance_strip_drag` then
        // keeps stepping until the held chip is back under the hand — several
        // slots for one motion, which is the self-stabilisation doing its job
        // rather than a runaway.
        let mut motions = 0;
        let mut last_col = None::<u16>;
        while app.windows[&wid].tabs.active + 1 < before.len() {
            motions += 1;
            assert!(
                motions <= before.len(),
                "the march must terminate: {:?}",
                tab_order(&app, wid)
            );
            let was = app.windows[&wid].tabs.active;
            // Past the NEIGHBOUR's midpoint while the neighbour is seated;
            // past the held chip's own END when the neighbour is over a page
            // edge and has no chip to have a midpoint (`strip_drag_step`'s
            // off-strip rule). Always a column to the RIGHT of where the last
            // motion ended: `advance_strip_drag` ignores a pointer that has not
            // moved, so a hand that stops moving stops the march, exactly as it
            // should.
            let col = if seated(&app).contains(&(was + 1)) {
                let (start, end) = chip_span(&app, wid, was + 1);
                start + (end - start) / 2
            } else {
                chip_span(&app, wid, was).1
            }
            .max(last_col.map_or(0, |c| c + 1));
            last_col = Some(col);
            let over = strip_point(&app, wid, col);
            app.on_cursor_moved(wid, over.0, over.1);
            let now = app.windows[&wid].tabs.active;
            assert!(
                now > was,
                "motion {motions}: the held chip advanced (was {was}, now {now})"
            );
            assert_eq!(
                tab_order(&app, wid)[now],
                before[held],
                "motion {motions}: the dragged tab is the one that moved, and it \
                 is still the selected one"
            );
            app.splice_tab_strip(wid);
            // The SAME pointer, re-delivered over the re-recorded band: a hand
            // that has not moved takes no further step, whatever the repaint did
            // underneath it — including the repaint that TURNED THE PAGE and put
            // four different chips under the same columns. That is
            // `advance_strip_drag`'s stationary-pointer guard, and it is what
            // keeps a paged strip from marching a tab away from a resting hand.
            app.on_cursor_moved(wid, over.0, over.1);
            assert_eq!(
                app.windows[&wid].tabs.active, now,
                "motion {motions}: a settled pointer must not oscillate after \
                 the repaint"
            );
            assert_eq!(tab_order(&app, wid)[now], before[held]);
        }
        app.on_mouse_input(wid, ElementState::Released, WinitMouseButton::Left);

        let mut expect: Vec<_> = before.clone();
        let moved = expect.remove(held);
        expect.push(moved);
        assert_eq!(
            tab_order(&app, wid),
            expect,
            "the chip marched from the front of the strip to the back"
        );
        assert_eq!(app.windows[&wid].tabs.active, before.len() - 1);
        assert!(app.windows[&wid].strip_drag.is_none());
    }

    /// SELECTION CUSTODY §3 row 24, on the REAL handler: a word/line drag must
    /// re-derive its ORIGIN from the LIVE selection on every move, not from the
    /// `GestureOrigin.row` captured at press time.
    ///
    /// The design calls this "Not optional under this design" because Phase 4 stopped
    /// output from clearing selections: `post_process` compensates the ENGINE anchors
    /// through `adjust_for_scroll`, nothing compensates `GestureOrigin.row`, so a drag
    /// that outlives one line of output re-anchors on a row the origin unit no longer
    /// occupies — a live wrong-copy path. Nothing called `drag_selection` from a test
    /// before this one, so reverting the re-derivation left the suite green.
    ///
    /// Content-level oracle deliberately: the assertion is on the TEXT the drag
    /// resolves to, which is what a ⌘-C would put on the clipboard.
    #[test]
    fn a_line_drag_re_anchors_on_its_origin_line_after_output_scrolls_the_grid() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(&numbered_screen("L"));

        // Triple-click row 5 → the "L05" line; the gesture origin is captured at 5.
        app.select_line_click(wid, 5, 0);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("L05"),
            "precondition: the triple-click selected the L05 line"
        );

        // Three lines of ordinary output scroll the grid. Phase 4 records no damage
        // for plain writes, so the selection SURVIVES and its anchors ride up to
        // row 2 — while `GestureOrigin.row` is still the stale 5.
        term_lock(&term).process(b"\r\nX\r\nY\r\nZ");
        assert_eq!(
            app.windows[&wid].gesture.map(|g| g.row),
            Some(5),
            "precondition: the captured gesture row is NOT compensated — that is the trap"
        );
        assert!(
            term_lock(&term).text_selection().has_selection(),
            "precondition: ordinary output must not clear the selection (Phase 4)"
        );

        // Drag down to viewport row 8 (now "L11").
        app.drag_selection(wid, 8, 0);

        let text = term_lock(&term)
            .selection_to_string()
            .expect("the drag resolves to text");
        assert_eq!(
            text.lines().next(),
            Some("L05"),
            "the drag must stay anchored on its ORIGIN line; anchoring on the stale \
             gesture row would start the copy at L08 — text the user never selected"
        );
        assert_eq!(
            text.lines().last(),
            Some("L11"),
            "…and reach the hovered line"
        );
    }

    /// The word (double-click) arm of the same rule — it re-derives the origin
    /// through the identical `origin_row` lookup, and a word drag is the shape the
    /// design's scenario names.
    #[test]
    fn a_word_drag_re_anchors_on_its_origin_word_after_output_scrolls_the_grid() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(&numbered_screen("w"));

        app.select_word_click(wid, 5, 0);
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("w05"),
            "precondition: the double-click selected the w05 word"
        );

        term_lock(&term).process(b"\r\nX\r\nY\r\nZ");
        app.drag_selection(wid, 8, 0);

        let text = term_lock(&term)
            .selection_to_string()
            .expect("the drag resolves to text");
        assert!(
            text.starts_with("w05"),
            "the word drag must keep its ORIGIN word fully selected after the scroll; \
             got {text:?}"
        );
    }

    /// The fallback half of the same code: with NO live selection to read, the drag
    /// falls back to the captured `GestureOrigin.row`. Pins that the re-derivation is
    /// a correction, not a replacement — a first move after the engine cleared the
    /// selection must still produce one.
    #[test]
    fn a_drag_with_no_live_selection_falls_back_to_the_captured_gesture_row() {
        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(&numbered_screen("L"));

        app.select_line_click(wid, 5, 0);
        // The engine drops the selection out from under the drag (an overlapping
        // damage band would do exactly this).
        term_lock(&term).text_selection_mut().clear();

        app.drag_selection(wid, 8, 0);
        let text = term_lock(&term)
            .selection_to_string()
            .expect("the fallback still builds a selection");
        assert_eq!(
            text.lines().next(),
            Some("L05"),
            "with nothing live to read, the captured gesture row is the origin"
        );
    }

    /// 24 rows of `<tag>NN`, cursor left on the last row so one more newline scrolls
    /// the whole screen — the fixture the drag-origin tests share.
    fn numbered_screen(tag: &str) -> Vec<u8> {
        let mut seed = Vec::new();
        for r in 0..24u16 {
            if r > 0 {
                seed.extend_from_slice(b"\r\n");
            }
            seed.extend_from_slice(format!("{tag}{r:02}").as_bytes());
        }
        seed
    }

    /// SELECTION CUSTODY §3 rows 21/22 — BLUR must SETTLE the in-flight selection,
    /// not merely drop the window-local gesture flags.
    ///
    /// `on_focus_loss_clears_stuck_pointer_gesture` asserts `!ws.selecting` and
    /// `ws.gesture.is_none()`, both of which were already true BEFORE this fix — the
    /// bug was that they were dropped WITHOUT finishing the selection, leaving a
    /// painted `InProgress` highlight that `extend_selection` refuses and that
    /// copy-on-select never fired for. `Terminal::text_selection().state()` is the
    /// only observable the fix changes, and nothing looked at it.
    #[test]
    fn blur_settles_the_in_flight_selection_into_a_completed_one() {
        use aterm_core::selection::SelectionState;

        use crate::{App, WindowId, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"hello world");

        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, 0, 5);
        assert_eq!(
            term_lock(&term).text_selection().state(),
            SelectionState::InProgress,
            "precondition: the sweep leaves the engine mid-gesture"
        );

        // Focus is stolen mid-sweep; the release never comes back.
        app.on_focus(wid, false);

        assert_eq!(
            term_lock(&term).text_selection().state(),
            SelectionState::Complete,
            "blur must COMPLETE the selection, not strand it InProgress"
        );
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("hello"),
            "…and the settled selection still names the swept text"
        );
        // A completed selection is one the user can act on: extending it works,
        // which is exactly what an InProgress zombie refuses.
        assert!(
            app.extend_selection_to(wid, 0, 10),
            "the settled selection is extendable — the InProgress zombie was not"
        );
    }

    /// The second call site of the same fix: a TAB SWITCH settles the drag on the
    /// tab being left. Same observable, different entry point — `sync_window`.
    #[test]
    fn a_tab_switch_settles_the_in_flight_selection_on_the_tab_it_leaves() {
        use aterm_core::selection::SelectionState;

        use crate::{App, WindowId, stub_session, term_lock};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        term_lock(&term).process(b"hello world");

        app.windows.get_mut(&wid).expect("window").last_mouse_cell = (0, 0);
        app.begin_selection(wid, SelectionType::Simple);
        app.drag_selection(wid, 0, 5);
        assert_eq!(
            term_lock(&term).text_selection().state(),
            SelectionState::InProgress
        );

        // The user opens/switches to another tab with the button still down.
        let sid = app.next_session_id;
        app.push_stub_tab(wid, stub_session(sid));

        assert_eq!(
            term_lock(&term).text_selection().state(),
            SelectionState::Complete,
            "the tab switch must settle the drag on the tab it leaves"
        );
        assert_eq!(
            term_lock(&term).selection_to_string().as_deref(),
            Some("hello")
        );
    }
}
