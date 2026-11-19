// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window lifecycle (`create`/`attach`/`close`/`focus`, `front`/`front_mut`, and `apply_title`)
//! routed through the platform [`crate::platform::AppRt`] seam.

use std::sync::Arc;
use std::time::Instant;

use aterm_window::dpi::{LogicalPosition, PhysicalPosition, PhysicalSize, Position};
use aterm_window::event_loop::ActiveEventLoop;
use aterm_window::window::Window;

#[cfg(windows)]
use crate::GpuBackend;
use crate::app_config::resolve_force_scale;
use crate::platform::AppRt;
use crate::present::CpuPresenter as _;
use crate::spawn::spawn_session;
use crate::{
    App, Backend, BackendSlot, CloseOutcome, FONT_PX, FONT_PX_MAX, FONT_PX_MIN, PresentTarget,
    Session, TabIndex, WindowId, WindowState, pane, seed_cell_px,
};

/// Reveal a restored macOS window without lending it the keyboard before focus is settled.
pub(crate) fn reveal_os_window(window: &Window, passive: bool) {
    #[cfg(target_os = "macos")]
    if passive {
        use aterm_window::platform::macos::WindowExtMacOS as _;
        window.order_front();
        return;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = passive;
    window.set_visible(true);
}

/// A screen's usable placement rectangle in logical points (top-left origin, y-down).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkAreaPts {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

const CASCADE_STEP_PTS: f64 = 26.0;

const CASCADE_MIN_VISIBLE_PTS: f64 = 160.0;

/// Subscribe before reading either accessibility value, keeping the observer alive through
/// the snapshot. A change posted just before registration must reach the initial reading.
fn observe_initial_accessibility<O>(
    observe: impl FnOnce() -> O,
    read: impl FnOnce() -> (bool, crate::native_appearance::AppearancePreferences),
) -> (O, bool, crate::native_appearance::AppearancePreferences) {
    let observer = observe();
    let (reduce_motion, appearance) = read();
    (observer, reduce_motion, appearance)
}

#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(2000);
#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_MAX_CORRECTIONS: u8 = 3;

#[cfg(test)]
thread_local! {
    /// The instant a test holds the initial-frame settle's clock at on this thread.
    static SETTLE_NOW_FOR_TEST: std::cell::Cell<Option<Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Now, as the initial-frame settle reads it: its expiry and its veto's park.
#[cfg(any(target_os = "linux", test))]
fn settle_now() -> Instant {
    #[cfg(test)]
    if let Some(now) = SETTLE_NOW_FOR_TEST.with(std::cell::Cell::get) {
        return now;
    }
    Instant::now()
}

#[must_use]
fn cascade_top_left_pts(
    anchor: (f64, f64),
    step: f64,
    window: (f64, f64),
    area: WorkAreaPts,
) -> Option<(f64, f64)> {
    let finite = [
        anchor.0, anchor.1, step, window.0, window.1, area.x, area.y, area.w, area.h,
    ]
    .into_iter()
    .all(f64::is_finite);
    if !finite || area.w <= 0.0 || area.h <= 0.0 || window.0 < 0.0 || window.1 < 0.0 {
        return None;
    }
    let keep_x = window.0.min(CASCADE_MIN_VISIBLE_PTS);
    let keep_y = window.1.min(CASCADE_MIN_VISIBLE_PTS);
    let mut x = anchor.0 + step;
    let mut y = anchor.1 + step;
    if x + keep_x > area.x + area.w {
        x = area.x;
    }
    if y + keep_y > area.y + area.h {
        y = area.y;
    }
    Some((
        x.clamp(area.x, (area.x + area.w - keep_x).max(area.x)),
        y.clamp(area.y, (area.y + area.h - keep_y).max(area.y)),
    ))
}

/// One live monitor's rectangle in physical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MonitorRectPx {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
    pub(crate) scale: f64,
}

#[must_use]
fn restored_origin_visible(origin: (f64, f64), monitors: &[MonitorRectPx]) -> bool {
    if !origin.0.is_finite() || !origin.1.is_finite() {
        return false;
    }
    monitors.iter().any(|m| {
        let finite = [m.x, m.y, m.w, m.h, m.scale]
            .into_iter()
            .all(f64::is_finite);
        if !finite || m.w <= 0.0 || m.h <= 0.0 || m.scale <= 0.0 {
            return false;
        }
        let keep = CASCADE_MIN_VISIBLE_PTS * m.scale;
        let keep_x = keep.min(m.w);
        let keep_y = keep.min(m.h);
        origin.0 >= m.x
            && origin.0 <= m.x + m.w - keep_x
            && origin.1 >= m.y
            && origin.1 <= m.y + m.h - keep_y
    })
}

/// [`restored_origin_visible`] against the live monitor set.
#[must_use]
pub(crate) fn restored_position_on_screen(window: &Window, x: i32, y: i32) -> bool {
    let monitors = monitors_px(window);
    if monitors.is_empty() {
        return true;
    }
    restored_origin_visible((f64::from(x), f64::from(y)), &monitors)
}

/// A restored window's frame, as its layout row carried it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedFrame {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) inner_px: PhysicalSize<u32>,
}

impl CarriedFrame {
    /// The frame `layout` carries.
    #[must_use]
    pub(crate) fn of(layout: &crate::restore::WindowLayout) -> Option<Self> {
        let [width, height] = layout.inner_px?;
        Some(Self {
            rows: layout.rows.clamp(5, 300),
            cols: layout.cols.clamp(20, 500),
            inner_px: PhysicalSize::new(width, height),
        })
    }

    /// The grid and frame a cold restore creates its first window at from that window's
    /// `layout`, where the command line's `--lines` and `--columns` (`lines`, `columns`) name the
    /// grid over the layout's: the frame is `layout`'s, as the windows after it carry theirs
    /// ([`Self::of`]), unless the command line named the lines or columns, which size it
    /// instead. `None` for a maximized window, which the OS sizes.
    #[must_use]
    pub(crate) fn cold_first_seed(
        layout: &crate::restore::WindowLayout,
        lines: Option<u16>,
        columns: Option<u16>,
    ) -> Option<ColdFirstSeed> {
        if layout.maximized == Some(true) {
            return None;
        }
        let grid = (
            lines.unwrap_or(layout.rows).clamp(5, 300),
            columns.unwrap_or(layout.cols).clamp(20, 500),
        );
        let frame = (lines.is_none() && columns.is_none())
            .then(|| Self::of(layout))
            .flatten()
            .map(|frame| frame.inner_px);
        Some(ColdFirstSeed { grid, frame })
    }
}

/// What a cold restore creates its first window at ([`CarriedFrame::cold_first_seed`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ColdFirstSeed {
    /// Rows and columns.
    pub(crate) grid: (u16, u16),
    /// The frame it carries, or `None` to size it to `grid`.
    pub(crate) frame: Option<PhysicalSize<u32>>,
}

/// The frame a window attaches at when it carries `carried` and its grid's own frame is
/// `exact`: `carried` where it holds the `grid` (rows, cols) with less than a cell to spare each
/// way, or, where the deck moved under it for something other than its rows (`moved`,
/// `CarriedDeck::keeps_frame`), where it holds the grid's columns with less than a cell to
/// spare, and the grid re-grids to it once (`attach_os_window`); else `None`, and the window
/// takes `exact`. `grid_of` is the grid a frame holds. [`App::attach_frame_px`] and the Windows
/// early reveal both size by it, so the window shown early is the window attached.
#[must_use]
pub(crate) fn kept_carried_frame(
    exact: PhysicalSize<u32>,
    carried: PhysicalSize<u32>,
    (cell_w, cell_h): (usize, usize),
    grid: (u16, u16),
    moved: bool,
    grid_of: impl Fn(PhysicalSize<u32>) -> (u16, u16),
) -> Option<PhysicalSize<u32>> {
    let under_a_cell = |got: u32, frame: u32, cell: usize| {
        got.checked_sub(frame)
            .is_some_and(|extra| (extra as usize) < cell)
    };
    if !under_a_cell(carried.width, exact.width, cell_w) {
        return None;
    }
    let held = grid_of(carried);
    let holds = under_a_cell(carried.height, exact.height, cell_h) && held == grid;
    (holds || moved && held.1 == grid.1).then_some(carried)
}

/// Where a restored window goes before it is first shown (Windows, and X11's position).
#[cfg(any(windows, target_os = "linux"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RestorePlacement {
    pub(crate) outer: Option<(i32, i32)>,
    pub(crate) maximized: bool,
}

#[cfg(any(windows, target_os = "linux"))]
impl App {
    /// Stage window `wid`'s persisted place.
    pub(crate) fn stage_restore_placement(
        &mut self,
        wid: WindowId,
        outer: (Option<i32>, Option<i32>),
        maximized: Option<bool>,
    ) -> bool {
        let placement = RestorePlacement {
            outer: outer.0.zip(outer.1),
            maximized: maximized == Some(true),
        };
        if placement.outer.is_none() && !placement.maximized {
            return false;
        }
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let ws = &mut **route_guard_ws;
        ws.restore_placement = Some(placement);
        true
    }
}

impl crate::WindowState {
    /// Take a restored window's carried frame before its first attach sizes it.
    pub(crate) const fn take_carried_frame(&mut self, inner_px: Option<PhysicalSize<u32>>) {
        self.carried_inner_px = inner_px;
    }
}

#[must_use]
fn cascade_anchor_of(
    new: WindowId,
    focus_order: &[WindowId],
    attached: &[WindowId],
) -> Option<WindowId> {
    focus_order
        .iter()
        .rev()
        .find(|wid| **wid != new && attached.contains(wid))
        .or_else(|| attached.iter().rev().find(|wid| **wid != new))
        .copied()
}

/// Authority currently owning the native window title.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowTitleAuthority {
    Search,
    Canonical,
}

#[must_use]
pub(crate) const fn window_title_authority(search_active: bool) -> WindowTitleAuthority {
    if search_active {
        WindowTitleAuthority::Search
    } else {
        WindowTitleAuthority::Canonical
    }
}

/// The text the OS shows for a window's canonical title (caption, taskbar, Alt+Tab).
#[must_use]
pub(crate) fn os_window_title(title: &str) -> &str {
    crate::chrome_label::without_spinner_frame(title)
}

/// Push a session's cwd onto `slot` in `~`-abbreviated tab-label form.
pub(crate) fn push_home_abbreviated_cwd(slot: &mut String, cwd: Option<&str>) -> bool {
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) else {
        return false;
    };
    match crate::app_tabs::cached_home()
        .and_then(|home| crate::app_tabs::home_relative_suffix(cwd, home))
    {
        Some(rest) => {
            slot.push('~');
            slot.push_str(rest);
        }
        None => slot.push_str(cwd),
    }
    true
}

#[must_use]
const fn present_target_install_outcome(
    present_installed: bool,
    milestones: crate::metrics::StartupAttachMilestones,
) -> (bool, Option<crate::metrics::StartupAttachMilestones>) {
    if !present_installed {
        return (false, None);
    }
    (true, Some(milestones))
}

/// How [`App::confirm_destructive_close`] answers a close that would exit a window or the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum CloseConfirm {
    /// A human gesture: the native dialog, or the in-window confirm banner where there is none.
    #[default]
    Interactive,
    /// Proceed with no dialog, for a close that is already an explicit instruction.
    Programmatic,
    /// The `close` and `tab close` verbs.
    WireRefuseBusy,
}

/// The wire refusal of a busy last-tab close, as the `close` catalog entry promises.
pub(crate) const WIRE_CLOSE_REFUSED: &str =
    "close refused (a running job armed the last-tab confirm)";

/// The wire refusal of a busy `tab close` aimed at a tab that is not its window's last.
pub(crate) const WIRE_TAB_CLOSE_REFUSED: &str = "close refused (a running job in that tab)";

/// What the Quit barriers answer at a Quit edge ([`App::quit_barriers`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitBarrier {
    /// Nothing holds the Quit.
    Ready,
    /// Documents are saving.
    Saving,
    /// A barrier refused the Quit, and it is disarmed.
    Refused,
}

/// What a window close ends, as its close gate weighs it: the terminals the
/// window holds, those of them running a job only this window shows (with which
/// job, [`SessionJob`]), and whether it is the last window. An armed document
/// close remembers it from when the close was asked
/// ([`App::window_close_consent_holds`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WindowCloseConsent {
    sessions: std::collections::BTreeSet<u64>,
    busy: std::collections::BTreeMap<u64, SessionJob>,
    exits_app: bool,
}

/// Which job a busy terminal runs, as far as aterm can tell one job from the
/// next: the PTY's foreground process group (unix) and how many commands the
/// shell's integration has reported finished. A job that ends and another that
/// starts moves one of them; where neither moves (no process group and no shell
/// integration), the two are not told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionJob {
    foreground_group: i32,
    finished_commands: u64,
}

impl App {
    /// Coalesce a destructive request behind the live overlap handoff and ask its worker to abort.
    #[cfg(unix)]
    pub(crate) fn defer_pending_update_handoff_teardown(
        &mut self,
        intent: crate::DeferredHandoffTeardown,
    ) -> bool {
        if self.pending_update_handoff.is_none() {
            #[cfg(any(target_os = "macos", all(test, unix)))]
            if self.update_handoff_prelaunch.is_some() {
                self.note_update_handoff_activity();
                self.stand_down_prelaunched_successor(
                    crate::app_update_handoff::HandoffStandDown {
                        outcome: crate::UpdateHandoffOutcome::ActivityRevoked,
                        detail: "a destructive intent arrived while the successor held its claim"
                            .to_string(),
                    },
                );
            }
            return false;
        }
        self.note_update_handoff_activity();
        let pending = self
            .pending_update_handoff
            .as_mut()
            .expect("pending handoff checked immediately above");
        pending.teardown.merge(intent);
        let _ = pending.cancel.try_send(());
        true
    }

    /// No overlap handoff exists off unix, so nothing is ever deferred behind one.
    #[cfg(not(unix))]
    pub(crate) fn defer_pending_update_handoff_teardown(
        &mut self,
        _intent: crate::DeferredHandoffTeardown,
    ) -> bool {
        false
    }

    /// The windows whose close a pending update handoff holds and replays
    /// once it stands down: each counts as gone when another close is weighed.
    #[cfg(unix)]
    pub(crate) fn windows_closing_behind_handoff(&self) -> Vec<WindowId> {
        match self
            .pending_update_handoff
            .as_ref()
            .map(|pending| &pending.teardown)
        {
            Some(crate::DeferredHandoffTeardown::Mutations(mutations)) => mutations
                .keys()
                .filter_map(|mutation| match mutation {
                    crate::DeferredHandoffMutation::CloseWindow(window) => Some(*window),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// No overlap handoff exists off unix, so no close waits behind one.
    #[cfg(not(unix))]
    pub(crate) fn windows_closing_behind_handoff(&self) -> Vec<WindowId> {
        Vec::new()
    }

    /// The frontmost logical window's state.
    pub(crate) fn front(&self) -> Option<&WindowState> {
        self.frontmost_window.and_then(|id| self.windows.get(&id))
    }

    /// The window-chrome theme from config `window_theme`.
    pub(crate) const fn window_theme_for_chrome(&self) -> crate::app_config::WindowTheme {
        self.window_theme
    }

    fn cell_metrics_font_key(&self) -> String {
        let view = self.font_view();
        format!(
            "{}|lh={}",
            view.font_family_request().unwrap_or_else(|| {
                aterm_render::bundled::default_primary_candidates()
                    .first()
                    .copied()
                    .unwrap_or_default()
                    .to_string()
            }),
            view.line_height_or_default(),
        )
    }

    const fn early_reveal_backdrop(&self) -> u32 {
        self.theme.bg & 0x00FF_FFFF
    }

    /// The chrome appearance handed to [`crate::platform::AppRt::window_set_appearance`].
    pub(crate) fn chrome_theme_for_apprt(&self) -> crate::app_config::WindowTheme {
        #[cfg(target_os = "linux")]
        if self.window_theme == crate::app_config::WindowTheme::Auto {
            return if crate::tab_bar::theme_is_dark(self.theme.bg) {
                crate::app_config::WindowTheme::Dark
            } else {
                crate::app_config::WindowTheme::Light
            };
        }
        self.window_theme_for_chrome()
    }

    /// The theme used by the window's chrome painters (tab band strip tones and native pages' role palette).
    pub(crate) fn chrome_palette_theme(&self) -> aterm_render::Theme {
        crate::native_appearance::resolve_chrome_palette(self.window_theme, self.theme)
    }

    /// The palette native pages (Settings, documents) draw.
    pub(crate) fn native_page_palette_theme(&self) -> aterm_render::Theme {
        crate::native_appearance::resolve_chrome_palette(
            self.window_theme,
            self.installed_colors.theme_before_overrides(),
        )
    }

    pub(crate) fn front_mut(&mut self) -> Option<crate::option_routing::WindowMutation<'_>> {
        self.windows.get_mut(&self.frontmost_window?)
    }

    /// Logical window creation (no winit).
    #[allow(
        clippy::too_many_arguments,
        reason = "the window's birth facts — geometry, cwd, the seamless adoptee, the \
                  connection lineage — are each a per-open datum; \
                  a wrapper struct would relocate the list, not simplify it"
    )]
    pub(crate) fn create_window_logical(
        &mut self,
        rows: u16,
        cols: u16,
        cwd_override: Option<&str>,
        kept: Option<aterm_session::SessionId>,
        adopt: Option<crate::spawn::Adopted>,
        lineage_parent: Option<aterm_session::SessionId>,
        observe: Option<&aterm_session::SessionId>,
    ) -> Option<WindowId> {
        let wid = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let sid = self.next_session_id;
        let proxy = self.proxy.clone()?;
        let cwd = cwd_override
            .map(str::to_owned)
            .or_else(|| self.frontmost_window.and_then(|w| self.focused_pane_cwd(w)));
        let factory = &self.session_factory;
        let session = match spawn_session(
            sid,
            wid,
            rows,
            cols,
            self.spawn_cell_px(wid),
            factory,
            &proxy,
            cwd.as_deref(),
            observe,
            None,  // no worker's task
            kept,  // COLD RESTORE: the first leaf's tab ID, kept
            adopt, // SEAMLESS: `Some` re-adopts this window's handed-off first-leaf shell
        ) {
            Ok(s) => s,
            Err(e) => {
                crate::logging::stderr_line!("aterm-gui: could not open a new window: {e}");
                self.post_message(crate::message_reporters::new_window_failed(&e.to_string()));
                return None;
            }
        };
        self.next_session_id += 1;
        self.install_window_state_with_parent(wid, session, rows, cols, lineage_parent);
        Some(wid)
    }

    /// Install an already-spawned `session` as the sole tab of a fresh window `wid` and focus it.
    #[cfg(test)]
    pub(crate) fn install_window_state(
        &mut self,
        wid: WindowId,
        session: Session,
        rows: u16,
        cols: u16,
    ) {
        self.install_window_state_with_parent(wid, session, rows, cols, None);
    }

    /// The display scale of a window made with no OS window, the scale its
    /// [`Self::unattached_window_metrics`] are at: a headless launch's own (`--scale`), since no
    /// screen ever measures one; windowed, 1 until [`Self::attach_os_window`] measures it.
    pub(crate) const fn unattached_window_scale(&self) -> f64 {
        if self.headless {
            self.headless_scale
        } else {
            1.0
        }
    }

    /// Installs `ws`, made with no OS window, as window `wid` at
    /// [`Self::unattached_window_scale`], so a headless window's overlay cards measure their
    /// points at the pixels its cells are drawn at.
    pub(crate) fn insert_unattached_window(&mut self, wid: WindowId, mut ws: WindowState) {
        ws.scale = self.unattached_window_scale();
        self.windows.insert(wid, Box::new(ws));
    }

    /// `install_window_state` with an explicit registry lineage parent for the window's first session.
    pub(crate) fn install_window_state_with_parent(
        &mut self,
        wid: WindowId,
        session: Session,
        rows: u16,
        cols: u16,
        lineage_parent: Option<aterm_session::SessionId>,
    ) {
        let sid = session.id;
        let (term, master, sink, ui_waiting) = (
            session.term.clone(),
            session.master,
            session.ctx.sink.clone(),
            session.ctx.ui_waiting.clone(),
        );
        let registered = session.master >= 0;
        if registered {
            Self::register_session(&self.store, &session, lineage_parent.clone());
        }
        let layout = pane::PaneTree::new(sid);
        let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
            .expect("tab/view identity space");
        self.pool_insert(session);
        if registered {
            self.note_tab_open(sid, wid, false, lineage_parent.as_ref(), "new-window");
        }
        let metrics = self.unattached_window_metrics();
        let ws = WindowState::new_terminal(
            term,
            master,
            sink,
            ui_waiting,
            sid,
            rows,
            cols,
            metrics,
            TabIndex::new(0, 1),
            vec![layout],
            crate::tab_model::TabSet::new(tab),
        );
        self.insert_unattached_window(wid, ws);
        self.install_window_config_assets(wid);
        self.frontmost_window = Some(wid);
        self.sync_active_session();
        self.mark_status_dirty();
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after create_window_logical: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
    }

    /// Test-only [`Self::create_window_logical`] over a stub `Session`.
    #[cfg(test)]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "window_routing",
            action = "CreateWindow",
            project = "aterm_gui::App::project_window_routing"
        )
    )]
    pub(crate) fn insert_logical_window(
        &mut self,
        session: Session,
        rows: u16,
        cols: u16,
    ) -> WindowId {
        debug_assert_eq!(
            session.id, self.next_session_id,
            "stub session id must match the minted session id",
        );
        let wid = WindowId(self.next_window_id);
        self.next_window_id += 1;
        self.next_session_id += 1;
        self.install_window_state(wid, session, rows, cols);
        wid
    }

    /// Full window creation within the current turn (one window per restore slice).
    #[allow(
        clippy::too_many_arguments,
        reason = "the window's birth facts — cwd, the seamless adoptee, the connection \
                  lineage, an update's carried frame and place and the row's dock record — \
                  are each a per-open datum, as in `create_window_logical`"
    )]
    pub(crate) fn create_window_internal_connected(
        &mut self,
        el: &ActiveEventLoop,
        cwd_override: Option<&str>,
        kept: Option<aterm_session::SessionId>,
        adopt: Option<crate::spawn::Adopted>,
        lineage_parent: Option<aterm_session::SessionId>,
        observe: Option<&aterm_session::SessionId>,
        carried: Option<CarriedFrame>,
        placement: ((Option<i32>, Option<i32>), Option<bool>),
        dock: DockRow,
    ) -> Option<WindowId> {
        let wid = self.create_restored_window_logical(carried, dock, |app, rows, cols| {
            app.create_window_logical(
                rows,
                cols,
                cwd_override,
                kept,
                adopt,
                lineage_parent,
                observe,
            )
        })?;
        #[cfg(any(windows, target_os = "linux"))]
        self.stage_restore_placement(wid, placement.0, placement.1);
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = placement;
        if !self.headless && !self.attach_restored_os_window(el, wid) {
            self.close_window_logical(wid);
            return None;
        }
        self.sync_active_session();
        Some(wid)
    }

    /// [`Self::create_window_internal_connected`]'s logical birth of a restored or carried
    /// window: its grid and frame from `carried` (else the front's grid), made by `spawn` at that
    /// grid with the front left as it was, then the carried frame and the row's dock record
    /// `dock` ([`Self::take_dock_record`]), all before its first attach. Only `spawn` (a shell's,
    /// which needs the event loop's proxy) is the caller's, so a fixture drives the rest.
    pub(crate) fn create_restored_window_logical(
        &mut self,
        carried: Option<CarriedFrame>,
        dock: DockRow,
        spawn: impl FnOnce(&mut Self, u16, u16) -> Option<WindowId>,
    ) -> Option<WindowId> {
        let ((rows, cols), carried_inner_px) = self.new_window_frame(carried);
        let previous_front = self.frontmost_window;
        let wid = spawn(self, rows, cols)?;
        self.frontmost_window = previous_front;
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.take_carried_frame(carried_inner_px);
        }
        self.take_dock_record(wid, dock);
        Some(wid)
    }

    /// The single join point for a windowed cold launch's deferred backend build.
    pub(crate) fn finalize_backend(&mut self) {
        let BackendSlot::Pending(handle) = &mut self.backend else {
            return;
        };
        let handle = handle.take().expect("finalize_backend re-entered mid-join");
        let (backend, use_gpu, font_family_rejected) = handle.join().unwrap_or_else(|_| {
            crate::logging::stderr_line!(
                "aterm-gui: backend-build thread panicked; no renderer — exiting"
            );
            crate::exit_without_process_teardown(1);
        });
        assert!(
            backend.admitted_font_sources_sealed(),
            "backend worker published an unsealed font generation"
        );
        self.backend = BackendSlot::Ready(backend);
        self.use_gpu = use_gpu;
        if let Some(px) = self.warm_miss_px.take() {
            self.backend.activate_px(px);
        }
        if let Some(warning) = font_family_rejected {
            crate::say_font_family_rejected(&warning);
            self.font_family = None;
        }
        crate::metrics::set_backend_gpu(use_gpu);
        self.backend.set_pad(self.cfg_pad_for_scale(1.0));
        self.backend.set_head(0);
        self.pin_gpu_effect_config();
        self.pin_backend_render_config_core();
        self.warn_font_feature_issues();

        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in windows {
            self.sync_cell_pixel_size(wid);
        }
    }

    /// Pin the GPU-only effect/swapchain knobs.
    pub(crate) fn pin_gpu_effect_config(&mut self) {
        let gpu_post_fx = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::GpuPostFx);
        let bloom_radius = self
            .config
            .cursor_trail_bloom_radius_or_default(self.trail_presentation());
        if let Backend::Gpu(g) = self.backend.ready_mut() {
            g.set_bloom(gpu_post_fx && self.config.cursor_trail_bloom_or_default());
            g.set_bloom_params(
                self.config.cursor_trail_bloom_strength_or_default(),
                bloom_radius,
            );
            g.set_shimmer(gpu_post_fx && self.config.cursor_fire_shimmer_or_default());
            g.set_backdrop_margins(
                self.render_knobs.background_material
                    != crate::app_config::BackgroundMaterial::None,
            );
            g.set_hdr_glow(self.config.hdr_glow_or_default());
            g.set_sdr_glow_boost(self.config.cursor_glow_sdr_boost_or_default());
            // Where a config apply would warm the effect pipelines, a window's
            // Metal arm builds what its frames can bind under these switches on
            // a worker from its mint, handing each over as it lands; a frame
            // that binds a row before then builds it. A headless instance warms
            // nothing: test suites start many at once, and their concurrent
            // reads and rewrites of the one Metal shader cache killed one by
            // SIGBUS (an FS pagein error in MTLCompilerFSCache).
            #[cfg(target_os = "macos")]
            if !self.headless && self.config.warms_effect_pipelines() {
                g.warm_effect_pipelines();
            }
        }
    }

    #[cfg(windows)]
    fn retry_attach_on_opaque_swapchain(
        &mut self,
        first_error: String,
        window: &Arc<Window>,
        (w_px, h_px): (u32, u32),
    ) -> Result<aterm_gpu::GpuSurface, String> {
        let other_gpu = self
            .windows
            .values()
            .any(|w| matches!(w.present, Some(PresentTarget::Gpu { .. })));
        if aterm_gpu::surface_attach_fallback(aterm_gpu::visual_swapchain_active(), other_gpu)
            != aterm_gpu::SurfaceAttachFallback::RebuildOpaque
        {
            return Err(first_error);
        }
        crate::logging::stderr_line!(
            "aterm-gui: backdrop requested but DirectComposition is unavailable ({first_error}); \
             falling back to the opaque swapchain — background_material styles the caption \
             only this run"
        );
        crate::message_inbox::queue_message(crate::message_reporters::backdrop_declined(
            "background_material is styling the title bar only",
            "this display stack refused the DirectComposition backdrop swapchain, so the \
             terminal body stays opaque",
        ));
        crate::platform_win::note_client_backdrop_declined(
            crate::platform_win::ClientBackdropDecline::DcompUnavailable,
        );
        aterm_gpu::withdraw_visual_swapchain();
        let rebuilt = match self
            .backend
            .gpu_ref()
            .map(|g| g.rebuild_on_fresh_context(self.font_px))
        {
            Some(Ok(g)) => g,
            Some(Err(err)) => {
                crate::logging::stderr_line!("aterm-gui: opaque swapchain rebuild failed: {err}");
                return Err(first_error);
            }
            None => return Err(first_error),
        };
        let _ = self.video_abort_backend_rebuild();
        self.backend
            .swap_renderer(Backend::Gpu(GpuBackend::new(rebuilt)));
        self.reset_window_raster_caches();
        self.pin_gpu_effect_config();
        self.pin_backend_render_config_core();
        self.backend
            .gpu_mut()
            .expect("the backend was just replaced with a GPU renderer")
            .create_window_surface(window.clone(), w_px, h_px)
    }

    fn cascade_anchor_window(&self, new: WindowId) -> Option<Arc<Window>> {
        let attached: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.os_window.is_some())
            .map(|(wid, _)| *wid)
            .collect();
        let anchor = cascade_anchor_of(new, &self.focus_order, &attached)?;
        self.windows.get(&anchor)?.os_window.clone()
    }

    fn work_area_pts(&self, window: &Window) -> Option<WorkAreaPts> {
        if let Some(area) = self.apprt.window_work_area_pts(window) {
            return Some(area);
        }
        let monitor = window.current_monitor()?;
        let scale = monitor.scale_factor();
        let origin = monitor.position().to_logical::<f64>(scale);
        let size = monitor.size().to_logical::<f64>(scale);
        Some(WorkAreaPts {
            x: origin.x,
            y: origin.y,
            w: size.width,
            h: size.height,
        })
    }

    fn cascade_window_placement(&self, new: WindowId, window: &Window) {
        let Some(anchor) = self.cascade_anchor_window(new) else {
            return;
        };
        let Ok(anchor_origin) = anchor.outer_position() else {
            return;
        };
        let Some(area) = self.work_area_pts(&anchor) else {
            return;
        };
        let anchor_pts = anchor_origin.to_logical::<f64>(anchor.scale_factor());
        let frame_pts = window.outer_size().to_logical::<f64>(window.scale_factor());
        let Some((x, y)) = cascade_top_left_pts(
            (anchor_pts.x, anchor_pts.y),
            CASCADE_STEP_PTS,
            (frame_pts.width, frame_pts.height),
            area,
        ) else {
            return;
        };
        aterm_log::debug!(
            "window {} placed at ({x:.0},{y:.0}) pt, cascaded from ({:.0},{:.0}) — frame {:.0}x{:.0} in work area {:.0}x{:.0}+{:.0}+{:.0}",
            new.0,
            anchor_pts.x,
            anchor_pts.y,
            frame_pts.width,
            frame_pts.height,
            area.w,
            area.h,
            area.x,
            area.y
        );
        window.set_outer_position(LogicalPosition::new(x, y));
    }

    /// The grid a new window is created at, and the raw inner size its first attach may size.
    pub(crate) fn new_window_frame(
        &self,
        carried: Option<CarriedFrame>,
    ) -> ((u16, u16), Option<PhysicalSize<u32>>) {
        match carried {
            Some(frame) => ((frame.rows, frame.cols), Some(frame.inner_px)),
            None => (self.front().map_or((24, 80), |ws| (ws.rows, ws.cols)), None),
        }
    }

    /// The size at which window `wid` is attached.
    pub(crate) fn attach_frame_px(
        &self,
        wid: WindowId,
        rows: u16,
        cols: u16,
        carried: Option<PhysicalSize<u32>>,
    ) -> PhysicalSize<u32> {
        let exact = self.window_frame_px_for(wid, rows, cols);
        let Some(carried) = carried else {
            return exact;
        };
        let moved = self.windows.get(&wid).is_some_and(|ws| {
            self.carried_deck_of(wid, ws.head_pts, ws.scale, (rows, cols), carried.height)
                .is_some_and(|was| was.keeps_frame(ws.deck_form))
        });
        let cells = self.win_cell_size(wid);
        if let Some(kept) = kept_carried_frame(exact, carried, cells, (rows, cols), moved, |px| {
            self.grid_dims_for(wid, px)
        }) {
            return kept;
        }
        aterm_log::info!(
            "restore: window {}'s carried size {}x{} does not hold the carried \
             {cols}x{rows} grid here (its frame is {}x{}); sized to the frame",
            wid.0,
            carried.width,
            carried.height,
            exact.width,
            exact.height
        );
        exact
    }

    /// Re-grid window `wid`'s tabs once to its attach frame `size` where that frame holds
    /// another grid than the window's: the Deck sized it in the form it no longer takes
    /// ([`Self::attach_frame_px`] keeps it), so the deck's head moved under it. Booked
    /// `site=deck`.
    pub(crate) fn regrid_to_attach_frame(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let Some((rows, cols)) = self.windows.get(&wid).map(|ws| (ws.rows, ws.cols)) else {
            return;
        };
        if size != self.window_frame_px_for(wid, rows, cols)
            && self.grid_dims_for(wid, size) != (rows, cols)
        {
            let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Deck);
            self.on_resize(wid, size);
        }
    }

    /// Window `wid`'s head at its attach for a `head_pts` titlebar band at `scale`, in the form
    /// the size setting gives ([`crate::stats_plan::deck_form_for`]), whatever form sized a
    /// carried frame: [`Self::attach_frame_px`] keeps a frame the Deck sized in the other form,
    /// and [`Self::regrid_to_attach_frame`] re-grids to it.
    pub(crate) fn attach_deck_head(&mut self, wid: WindowId, head_pts: f64, scale: f64) -> usize {
        let form = crate::stats_plan::deck_form_for(self.deck_size_pref());
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.deck_form = form;
        }
        let head = self.deck_head_px(wid, head_pts, scale, 0);
        self.backend.set_head(head);
        head
    }

    /// Attach an explicitly opened foreground window, superseding an older saved focus.
    pub(crate) fn attach_foreground_os_window(
        &mut self,
        el: &ActiveEventLoop,
        wid: WindowId,
    ) -> bool {
        self.note_restore_focus_intent(wid);
        self.attach_os_window(el, wid)
    }

    /// Attach a restored window without giving it the keyboard while its saved state is pending.
    pub(crate) fn attach_restored_os_window(
        &mut self,
        el: &ActiveEventLoop,
        wid: WindowId,
    ) -> bool {
        if let Some(mut route_guard_window) = self.windows.get_mut(&wid) {
            let window = &mut **route_guard_window;
            window.passive_reveal = true;
        }
        self.attach_os_window(el, wid)
    }

    /// Create the OS window and present surface for logical window `wid` and attach them,
    /// its dock seated first as its birth rule says ([`Self::seat_birth_dock`]): a window born
    /// docked is sized with the dock beside the grid it carries. A failed attach leaves no dock
    /// view behind, whatever rollback follows.
    #[must_use]
    pub(crate) fn attach_os_window(&mut self, el: &ActiveEventLoop, wid: WindowId) -> bool {
        let seated = self.seat_local_panel_at_birth(wid);
        let attached = self.attach_os_window_sized(el, wid);
        if seated && !attached {
            self.unseat_birth_dock(wid);
        }
        attached
    }

    /// [`Self::attach_os_window`] once the dock is seated.
    #[must_use]
    fn attach_os_window_sized(&mut self, el: &ActiveEventLoop, wid: WindowId) -> bool {
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        let side_inset_cells = self.windows.get(&wid).map_or(0, |ws| ws.dock.cells);
        let pending_join = self.backend.is_pending();
        let startup_attach_entry = Instant::now();
        let mut size = if pending_join {
            let (cw, ch) = seed_cell_px(self.font_px);
            let pad = self.cfg_pad_for_scale(1.0);
            let vertical_pad = pad.saturating_add(self.cfg_pad_top_for_scale(1.0));
            let total_rows = rows.saturating_add(self.chrome_rows(wid));
            PhysicalSize::new(
                (cols as usize * cw + pad.saturating_mul(2) + usize::from(side_inset_cells) * cw)
                    as u32,
                (total_rows as usize * ch + vertical_pad) as u32,
            )
        } else {
            self.window_frame_px_for(wid, rows, cols)
        };
        let attrs = Window::default_attributes()
            .with_title(
                crate::elevated_caption::caption("aterm", aterm_uds::elevation::is_elevated())
                    .into_owned(),
            )
            .with_inner_size(size);
        #[cfg(windows)]
        let attrs = {
            use aterm_window::platform::windows::WindowAttributesExtWindows as _;
            let (small, big) = crate::win_icon::window_icons(crate::win_icon::system_dpi());
            let attrs = attrs
                .with_window_icon(small)
                .with_taskbar_icon(big)
                .with_class_name("aterm");
            if aterm_gpu::visual_swapchain_requested() {
                attrs.with_no_redirection_bitmap(true)
            } else {
                attrs
            }
        };
        #[cfg(target_os = "linux")]
        let attrs = {
            use aterm_window::platform::wayland::{
                ActiveEventLoopExtWayland as _, WindowAttributesExtWayland,
            };
            use aterm_window::platform::x11::WindowAttributesExtX11 as _;
            let wayland = el.is_wayland();
            let attrs = attrs
                .with_name("aterm", "aterm")
                .with_min_inner_size(aterm_window::dpi::LogicalSize::new(164.0, 98.0));
            let attrs = if wayland {
                attrs
            } else {
                attrs
                    .with_window_icon(crate::desktop_files::window_icon())
                    .with_x11_background(self.early_reveal_backdrop())
            };
            match crate::startup_token::take_for_first_window(wayland) {
                Some(token) => attrs.with_activation_token(token),
                None => attrs,
            }
        };
        let defer_reveal = self.handoff_ready.is_some();
        if defer_reveal && let Some(mut window) = self.windows.get_mut(&wid) {
            window.passive_reveal = true;
        }
        let passive_reveal = self
            .windows
            .get(&wid)
            .is_some_and(|window| window.passive_reveal);
        #[cfg(target_os = "linux")]
        let placed_before_reveal =
            !aterm_window::platform::wayland::ActiveEventLoopExtWayland::is_wayland(el)
                && self
                    .windows
                    .get(&wid)
                    .is_some_and(|ws| ws.restore_placement.is_some());
        #[cfg(not(target_os = "linux"))]
        let placed_before_reveal = false;
        let reveal_after_chrome = pending_join || cfg!(windows) || placed_before_reveal;
        let attrs = if reveal_after_chrome || defer_reveal || passive_reveal {
            attrs.with_visible(false)
        } else {
            attrs
        };
        let startup_before_window_create = Instant::now();
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                crate::logging::stderr_line!("aterm-gui: OS window creation failed: {e}");
                aterm_log::warn!(
                    "OS window creation failed ({})",
                    aterm_log::words::Len(&e.to_string())
                );
                return false;
            }
        };
        let startup_after_window_create = Instant::now();
        #[cfg(windows)]
        crate::win_icon::fit_new_window(&window);
        let option_is_alt = self.option_is_alt(wid);
        crate::platform::set_option_as_alt(&window, option_is_alt);
        if let Some(ws) = self.windows.get(&wid) {
            ws.option_as_alt.set(option_is_alt);
        }
        #[cfg(a11y_tree)]
        let a11y_adapter = self
            .proxy
            .clone()
            .and_then(|proxy| crate::a11y_adapter::Adapter::new(&window, proxy));
        if !reveal_after_chrome && !defer_reveal {
            crate::metrics::mark_first_window_visible();
        }
        if self._menu.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            crate::tab_spawn::serve_native_menu_marks(&self.tab_spawns.menu);
            self._menu = self.apprt.install_menu(proxy);
            self.apprt.install_quit_confirm();
            // The machine title's reader starts once there is a bar to title (never headless).
            #[cfg(target_os = "macos")]
            if self._menu.is_some() {
                crate::machine_title::spawn(proxy.clone());
            }
        }
        if self._status_item.is_none()
            && let Some(proxy) = self.proxy.clone()
        {
            self.install_status_item(&proxy);
        }
        if self._reduce_motion.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            let (observer, reduce_motion, appearance) = observe_initial_accessibility(
                || self.apprt.observe_reduce_motion(proxy),
                || {
                    (
                        self.apprt.reduce_motion(),
                        self.apprt.native_appearance_preferences(),
                    )
                },
            );
            self._reduce_motion = observer;
            self.system_reduce_motion =
                reduce_motion || crate::native_appearance::dev_accessibility().reduce_motion;
            crate::native_appearance::install_preferences(appearance);
            // The accent macOS paints its controls in, read once a window stands; the system
            // colours' notification reads it again.
            #[cfg(target_os = "macos")]
            crate::native_appearance::install_control_accent(self.apprt.control_accent());
            // Keep the initial power sample after the same subscription.
            let _ = self.resample_low_power(std::time::Instant::now(), true);
        }
        window.set_ime_allowed(true);
        window.set_ime_purpose(aterm_window::window::ImePurpose::Terminal);
        if self.os_appearance_observer.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            self.os_appearance_observer = self.os_appearance_source.observe(proxy);
        }
        if let Some(os_appearance) = self.os_appearance_source.at_attach(|| window.theme()) {
            self.sync_app_theme_to_appearance(os_appearance);
        }
        #[cfg(windows)]
        let mut early_reveal_size: Option<PhysicalSize<u32>> = None;
        #[cfg(not(windows))]
        let early_reveal_size: Option<PhysicalSize<u32>> = None;
        #[cfg(windows)]
        let mut early_reveal_bg: Option<u32> = None;
        #[cfg(not(windows))]
        let early_reveal_bg: Option<u32> = None;
        #[cfg(windows)]
        if pending_join && !defer_reveal && self.seamless_position.is_none() {
            // Read out of the pending manifest by value: the screen fit below borrows the app
            // mutably.
            let (restore_maximized, bootstrap_origin) = self
                .pending_restore
                .as_ref()
                .and_then(|m| m.windows.first())
                .map_or((false, None), |wl| {
                    (wl.maximized == Some(true), wl.outer_x.zip(wl.outer_y))
                });
            let scale = resolve_force_scale().unwrap_or_else(|| window.scale_factor());
            let target_font_px = self.attach_font_px(scale);
            let cached = (!restore_maximized)
                .then(|| {
                    crate::restore::load_cell_metrics(
                        &self.cell_metrics_font_key(),
                        scale,
                        target_font_px,
                    )
                })
                .flatten();
            if let Some((cell_w, cell_h)) = cached {
                let pad = self.cfg_pad_for_scale(scale);
                let pad_top = self.cfg_pad_top_for_scale(scale);
                let band = (self.apprt.titlebar_band_pts(&window) * scale)
                    .round()
                    .max(0.0) as usize
                    + self.synthetic_strip_head_px(scale, cell_h);
                let total_rows = rows.saturating_add(self.chrome_rows(wid)) as usize;
                // The screen fit before the reveal sizes, places and shows the window: a
                // default dock its screen cannot hold is unseated here, not after the window
                // was shown and slid for it.
                let docked_w =
                    aterm_render::frame_size_px(total_rows, cols as usize, cell_w, cell_h, pad, 0)
                        .0
                        .saturating_add(usize::from(side_inset_cells) * cell_w);
                let saved = self.windows.get(&wid).and_then(|ws| ws.carried_inner_px);
                self.fit_birth_dock_at_attach(
                    wid,
                    &window,
                    (u32::try_from(docked_w).unwrap_or(u32::MAX), saved),
                    scale,
                );
                let side_inset_cells = self.windows.get(&wid).map_or(0, |ws| ws.dock.cells);
                let inset = usize::from(side_inset_cells) * cell_w;
                let frame = |head: usize| {
                    let (width, raw_height) = aterm_render::frame_size_px(
                        total_rows,
                        cols as usize,
                        cell_w,
                        cell_h,
                        pad,
                        head,
                    );
                    let height = crate::visible_frame_height(raw_height, pad, pad_top);
                    PhysicalSize::new((width + inset) as u32, height as u32)
                };
                let bars = self.window_bars(wid);
                // The whole reservation (the Deck's rows, and the Disk Custody and Network State
                // bands under them, `stats_deck::bands_px`), as `chrome_head_px` counts the
                // attach's head.
                let head_in =
                    |form| band + crate::stats_deck::reserved_deck_px(bars, form, scale, cell_h);
                let carried = self.windows.get(&wid).and_then(|ws| ws.carried_inner_px);
                let was = carried.and_then(|c| {
                    crate::stats_deck::carried_deck(c.height, cell_h, head_in, |head| {
                        frame(head).height
                    })
                });
                let deck = crate::stats_plan::deck_form_for(self.deck_size_pref());
                // Sized as `attach_frame_px` sizes it, so the window shown is the one attached.
                let head = head_in(deck);
                let moved = was.is_some_and(|was| was.keeps_frame(deck));
                let chrome = self.chrome_rows(wid);
                let grid_of = |px: PhysicalSize<u32>| {
                    let cols = aterm_render::pad_split(
                        (px.width as usize).saturating_sub(inset),
                        pad,
                        cell_w,
                    )
                    .cells;
                    let rows = crate::asymmetric_pad_cells(
                        (px.height as usize).saturating_sub(head),
                        pad_top,
                        pad,
                        cell_h,
                    );
                    (
                        u16::try_from(rows)
                            .unwrap_or(u16::MAX)
                            .saturating_sub(chrome)
                            .max(1),
                        u16::try_from(cols).unwrap_or(u16::MAX),
                    )
                };
                let exact = carried
                    .and_then(|carried| {
                        kept_carried_frame(
                            frame(head),
                            carried,
                            (cell_w, cell_h),
                            (rows, cols),
                            moved,
                            grid_of,
                        )
                    })
                    .unwrap_or_else(|| frame(head));
                let _ = window.request_inner_size(exact);
                if let Some((x, y)) = bootstrap_origin
                    && restored_position_on_screen(&window, x, y)
                {
                    // Before the join: the dock's width at the cached cells the frame was
                    // sized by, not the renderer's, which is still being built.
                    window.set_outer_position(self.restored_origin_among(
                        wid,
                        DesktopPlane::NATIVE,
                        &monitors_px(&window),
                        (window.outer_size().width, window.scale_factor()),
                        (x, y),
                        inset,
                    ));
                }
                let backdrop = self.early_reveal_backdrop();
                #[cfg(windows)]
                crate::platform_win::note_caption_surface(&window, self.caption_surface_tone(wid));
                self.apprt.window_set_background_color(&window, backdrop);
                self.apprt
                    .window_set_appearance(&window, self.window_theme_for_chrome());
                reveal_os_window(&window, passive_reveal);
                crate::metrics::mark_first_window_visible();
                self.apprt.window_flush_backdrop(&window);
                early_reveal_size = Some(exact);
                early_reveal_bg = Some(backdrop);
            }
        }
        let startup_before_backend_finalize = Instant::now();
        if pending_join {
            self.finalize_backend();
        }
        let startup_after_backend_finalize = Instant::now();
        self.apprt.window_set_fullsize_content(&window);
        if let Some(proxy) = self.proxy.clone()
            && let Some(handle) = self.apprt.install_toolbar(&window, &proxy, wid)
        {
            crate::toolbar::set_strip_dark(&handle, crate::tab_bar::theme_is_dark(self.theme.bg));
            let (authored, strip_accent) = self.native_strip_chrome();
            crate::toolbar::set_active_tab_color(&handle, authored.active_tab);
            crate::toolbar::set_strip_accent(&handle, strip_accent);
            self._toolbars.insert(wid, handle);
            self.publish_app_tiles_to(wid);
        }
        let head_pts = self.apprt.titlebar_band_pts(&window);
        let (_, band_memory_pts) = crate::app_config::titlebar_band_decision(
            head_pts,
            window.fullscreen().is_some(),
            window.is_decorated(),
            self.windows
                .get(&wid)
                .map_or(0.0, |ws| ws.last_windowed_band_pts),
        );
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.head_pts = head_pts;
            ws.last_windowed_band_pts = band_memory_pts;
        }
        let scale = resolve_force_scale().unwrap_or_else(|| window.scale_factor());
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.scale = scale;
        }
        if let Some((px, reset)) = self.font_size_from_device.take() {
            self.font_size = font_size_of_device_px(px, scale);
            self.default_font_size = font_size_of_device_px(reset, scale);
        }
        let scaled = self.attach_font_px(scale);
        if (scaled - self.font_px).abs() >= 0.5 {
            self.backend.activate_px(scaled);
        }
        self.font_px = scaled;
        let pad = self.cfg_pad_for_scale(scale);
        self.backend.set_pad(pad);
        let pad_top = self.cfg_pad_top_for_scale(scale);
        self.backend.set_pad_top(pad_top);
        #[cfg(any(windows, target_os = "linux"))]
        {
            let cell_h = self.backend.cell_geometry(self.font_px).1;
            let synthetic = self.synthetic_strip_head_px(scale, cell_h);
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.head_pts = synthetic as f64 / if scale > 0.0 { scale } else { 1.0 };
            }
        }
        let head_pts = self.windows.get(&wid).map_or(0.0, |ws| ws.head_pts);
        let carried = self
            .windows
            .get_mut(&wid)
            .as_deref_mut()
            .and_then(|ws| ws.carried_inner_px.take());
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.metrics = crate::MetricsView::applied(self.font_px, pad, pad_top, ws.metrics.head);
        }
        let head = self.attach_deck_head(wid, head_pts, scale);
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.metrics = crate::MetricsView::applied(self.font_px, pad, pad_top, head);
        }
        let docked_w = self.window_frame_px_for(wid, rows, cols).width;
        self.fit_birth_dock_at_attach(wid, &window, (docked_w, carried), scale);
        size = self.attach_frame_px(wid, rows, cols, carried);
        self.regrid_to_attach_frame(wid, size);
        let _ = window.request_inner_size(size);
        if let Some(early) = early_reveal_size {
            if early == size {
                aterm_log::debug!(
                    "L1 early reveal held: {}x{} confirmed post-join (zero resize on glass)",
                    size.width,
                    size.height
                );
            } else {
                aterm_log::warn!(
                    "L1 early reveal was STALE: revealed {}x{}, backend derived {}x{} — \
                     one visible resize this launch; re-persisting the real metrics",
                    early.width,
                    early.height,
                    size.width,
                    size.height
                );
            }
        }
        if let Some(early_bg) = early_reveal_bg {
            let now_bg = self.early_reveal_backdrop();
            if early_bg == now_bg {
                aterm_log::debug!(
                    "L1 early reveal backdrop held: #{:06x} erased pre-join, first frame paints the same",
                    now_bg
                );
            } else {
                aterm_log::warn!(
                    "L1 early reveal backdrop DRIFTED: erased #{:06x} pre-join, first frame paints #{:06x} — \
                     one visible colour step this launch",
                    early_bg,
                    now_bg
                );
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.initial_frame_settle = Some(crate::InitialFrameSettle {
                rows: ws.rows,
                cols: ws.cols,
                deadline: Instant::now() + INITIAL_FRAME_SETTLE_WINDOW,
                corrections: INITIAL_FRAME_SETTLE_MAX_CORRECTIONS,
                corrected: false,
            });
        }
        {
            let (cw, ch) = self.cell_size();
            window.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
            let least = self.least_window_size(wid);
            crate::app_config::tell_least_inner_size(&window, least);
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.least_held = Some(least);
            }
            if (self.font_size - self.default_font_size).abs() < 0.5 {
                crate::restore::store_cell_metrics(
                    self.cell_metrics_font_key(),
                    scale,
                    self.font_px,
                    cw,
                    ch,
                );
            }
        }
        self.cascade_window_placement(wid, &window);
        #[cfg(windows)]
        crate::platform_win::note_caption_surface(&window, self.caption_surface_tone(wid));
        self.apprt
            .window_set_background_color(&window, self.theme.bg);
        self.apprt
            .window_set_appearance(&window, self.chrome_theme_for_apprt());
        // The placement is taken in a statement of its own, so the window's mutation guard
        // drops before the body reads the app (`restored_origin`); in the `if let` scrutinee it
        // would live to the end of the body (E0502 on Windows and Linux).
        #[cfg(any(windows, target_os = "linux"))]
        let placement = self
            .windows
            .get_mut(&wid)
            .and_then(|mut ws| ws.restore_placement.take());
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(placement) = placement
            && early_reveal_size.is_none()
        {
            if let Some((x, y)) = placement.outer
                && restored_position_on_screen(&window, x, y)
            {
                window.set_outer_position(self.restored_origin(wid, &window, x, y));
            }
            #[cfg(windows)]
            if placement.maximized {
                window.set_maximized(true);
            }
        }
        if reveal_after_chrome || passive_reveal {
            // Handoff boots stay hidden here: the first successful present reveals them,
            // or the 1.5 s `pending_reveal` deadline when none has landed by then.
            if !defer_reveal && early_reveal_size.is_none() {
                reveal_os_window(&window, passive_reveal);
                crate::metrics::mark_first_window_visible();
                #[cfg(windows)]
                self.apprt.window_flush_backdrop(&window);
            }
            window.request_redraw();
        }
        let inner = window.inner_size();
        let raw_px = if inner.width == 0 || inner.height == 0 {
            size
        } else {
            inner
        };
        let startup_before_surface_create = Instant::now();
        if self.backend.is_gpu() {
            let (w_px, h_px) = (raw_px.width, raw_px.height);
            #[cfg(target_os = "linux")]
            let surface_result = self
                .backend
                .gpu_mut()
                .unwrap()
                .create_window_surface_with_sink(
                    window.clone(),
                    w_px,
                    h_px,
                    Box::new(crate::present::x11::X11FrameSink::new(window.clone())),
                );
            #[cfg(not(target_os = "linux"))]
            let surface_result =
                self.backend
                    .gpu_mut()
                    .unwrap()
                    .create_window_surface(window.clone(), w_px, h_px);
            #[cfg(windows)]
            let surface_result = match surface_result {
                Ok(surf) => Ok(surf),
                Err(first_error) => {
                    self.retry_attach_on_opaque_swapchain(first_error, &window, (w_px, h_px))
                }
            };
            let surface_result = surface_result.and_then(|mut surface| {
                let proxy = self.proxy.clone().ok_or_else(|| {
                    "GPU surface attach requires an event-loop completion proxy".to_string()
                })?;
                let acquire_id = surface.acquire_id();
                surface.set_acquire_ready_callback(Arc::new(move || {
                    let _ = proxy.send_event(crate::Wake::GpuSurfaceReady {
                        window: wid,
                        surface: acquire_id,
                    });
                }))?;
                Ok(surface)
            });
            let startup_after_surface_create = Instant::now();
            match surface_result {
                Ok(surf) => {
                    let hdr = surf.is_hdr();
                    let surface_colorspace =
                        crate::platform::resolve_surface_colorspace(self.window_colorspace, hdr);
                    let effective_colorspace = self
                        .apprt
                        .window_set_surface_colorspace(&window, surface_colorspace);
                    self.apprt.window_anchor_surface_top_left(&window);
                    self.apprt.window_set_vibrancy(
                        &window,
                        self.render_knobs.background_material,
                        aterm_render::vibrancy::is_translucent(
                            self.render_knobs.background_opacity,
                        ),
                        self.theme.bg,
                    );
                    let mut window_gpu = aterm_gpu::WindowGpu::new();
                    window_gpu.set_capture_color_space(
                        crate::platform::capture_space_after_surface_tag(
                            effective_colorspace,
                            aterm_gpu::video_tap::CaptureColorSpace::Unknown,
                        ),
                    );
                    if hdr {
                        window_gpu.set_edr_max(self.apprt.screen_edr_max(&window));
                        window_gpu.set_sdr_white_scale(self.apprt.screen_sdr_white_scale(&window));
                    }
                    self.winit_to_window.insert(window.id(), wid);
                    let present_installed =
                        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                            let ws = &mut **route_guard_ws;
                            ws.os_window = Some(window);
                            ws.pending_reveal = defer_reveal
                                .then(|| Instant::now() + std::time::Duration::from_millis(1500));
                            ws.set_window_pixels(raw_px);
                            #[cfg(a11y_tree)]
                            {
                                ws.a11y = a11y_adapter;
                            }
                            ws.capture_acquire_armed = false;
                            ws.gpu_acquire_wait = crate::GpuAcquireWait::default();
                            ws.replace_present(Some(PresentTarget::Gpu {
                                gpu_surface: surf,
                                window_gpu,
                            }));
                            true
                        } else {
                            false
                        };
                    if present_installed {
                        self.note_birth_dock_frame(wid, raw_px, (rows, cols));
                        self.adopt_attached_window_focus(wid);
                        #[cfg(target_os = "linux")]
                        self.fit_client_frame_at_attach(wid);
                    }
                    let (attached, startup_milestones) = present_target_install_outcome(
                        present_installed,
                        crate::metrics::StartupAttachMilestones::new([
                            startup_attach_entry,
                            startup_before_window_create,
                            startup_after_window_create,
                            startup_before_backend_finalize,
                            startup_after_backend_finalize,
                            startup_before_surface_create,
                            startup_after_surface_create,
                        ]),
                    );
                    if let Some(milestones) = startup_milestones {
                        crate::metrics::record_initial_attach_milestones(milestones);
                    }
                    return attached;
                }
                Err(e) => {
                    let other_gpu = self
                        .windows
                        .values()
                        .any(|w| matches!(w.present, Some(PresentTarget::Gpu { .. })));
                    if other_gpu {
                        crate::logging::stderr_line!("aterm-gui: GPU surface creation failed: {e}");
                        aterm_log::warn!(
                            "GPU surface creation failed ({})",
                            aterm_log::words::Len(&e)
                        );
                        drop(window);
                        return false;
                    }
                    crate::logging::stderr_line!(
                        "aterm-gui: GPU surface creation failed: {e}; falling back to the CPU renderer"
                    );
                    aterm_log::warn!(
                        "GPU surface creation failed ({}); falling back to the CPU renderer",
                        aterm_log::words::Len(&e)
                    );
                    match self
                        .backend
                        .cpu_renderer_from_admitted(self.font_px, self.theme)
                    {
                        Ok(cpu) => {
                            self.backend.swap_renderer(Backend::Cpu(cpu));
                            self.use_gpu = false;
                            crate::metrics::set_backend_gpu(false);
                            self.pin_backend_render_config_core();
                            self.warn_font_feature_issues();
                            self.sync_chrome_fonts();
                        }
                        Err(error) => {
                            crate::logging::stderr_line!(
                                "aterm-gui: resident CPU font fallback failed: {error}"
                            );
                            aterm_log::warn!(
                                "resident CPU font fallback failed ({})",
                                aterm_log::words::Len(&error)
                            );
                            drop(window);
                            return false;
                        }
                    }
                }
            }
        }
        let surface = match crate::present::CpuSurface::new(window.clone()) {
            Ok(s) => s,
            Err(e) => {
                crate::logging::stderr_line!("aterm-gui: CPU present surface creation failed: {e}");
                aterm_log::warn!(
                    "CPU present surface creation failed ({})",
                    aterm_log::words::Len(&e.to_string())
                );
                drop(window);
                return false;
            }
        };
        let startup_after_surface_create = Instant::now();
        self.winit_to_window.insert(window.id(), wid);
        let present_installed = if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.os_window = Some(window);
            ws.pending_reveal =
                defer_reveal.then(|| Instant::now() + std::time::Duration::from_millis(1500));
            ws.set_window_pixels(raw_px);
            #[cfg(a11y_tree)]
            {
                ws.a11y = a11y_adapter;
            }
            ws.replace_present(Some(PresentTarget::Cpu { surface }));
            true
        } else {
            false
        };
        if present_installed {
            self.note_birth_dock_frame(wid, raw_px, (rows, cols));
            self.adopt_attached_window_focus(wid);
            #[cfg(target_os = "linux")]
            self.fit_client_frame_at_attach(wid);
        }
        let (attached, startup_milestones) = present_target_install_outcome(
            present_installed,
            crate::metrics::StartupAttachMilestones::new([
                startup_attach_entry,
                startup_before_window_create,
                startup_after_window_create,
                startup_before_backend_finalize,
                startup_after_backend_finalize,
                startup_before_surface_create,
                startup_after_surface_create,
            ]),
        );
        if let Some(milestones) = startup_milestones {
            crate::metrics::record_initial_attach_milestones(milestones);
        }
        attached
    }

    fn adopt_attached_window_focus(&mut self, wid: WindowId) {
        let focused = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map(|window| window.has_focus());
        self.adopt_initial_focus(wid, focused);
    }

    /// Attach-time focus adoption.
    pub(crate) fn adopt_initial_focus(&mut self, wid: WindowId, os_focus: Option<bool>) {
        let Some(focused) = os_focus else {
            return;
        };
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let ws = &mut **route_guard_ws;
        // What the OS says of its key window is the look's input too (§14.11).
        ws.os_key = focused;
        if ws.focused != focused {
            drop(route_guard_ws);
            self.on_focus(wid, focused);
        }
    }

    /// The grid half of a `WindowEvent::Resized`.
    pub(crate) fn apply_window_resized_grid(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        #[cfg(any(target_os = "linux", test))]
        if self.settle_initial_frame(wid, size) {
            return;
        }
        self.resize_live_drag = true;
        self.on_resize_throttled(wid, size);
        self.resize_live_drag = false;
    }

    /// Linux initial-frame settle ([`crate::InitialFrameSettle`]).
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn settle_initial_frame(&mut self, wid: WindowId, size: PhysicalSize<u32>) -> bool {
        let Some(settle) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.initial_frame_settle)
        else {
            return false;
        };
        if settle_now() > settle.deadline || settle.corrections == 0 {
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.initial_frame_settle = None;
            }
            // The size the expired settle lets through is the OS's answer: a default dock it
            // refused to give the wider frame for goes inside instead.
            self.judge_birth_dock_frame(wid, size, settle);
            return false;
        }
        self.apply_window_scale(wid);
        let intended = self.window_frame_px_for(wid, settle.rows, settle.cols);
        let grid_now = self.grid_dims_for(wid, size);
        if grid_now == (settle.rows, settle.cols) {
            if settle.corrected {
                // Granted after a correction: the OS's own answer, not an echo of the request,
                // so the default dock stands for good.
                self.judge_birth_dock_frame(wid, size, settle);
            }
            if size != intended {
                aterm_log::debug!(
                    "initial-frame settle: {}x{} carries the intended {}x{} grid \
                     (frame law wanted {}x{}; the {}px residue is band-absorbed, \
                     unrepresentable at this scale) — settled",
                    size.width,
                    size.height,
                    settle.cols,
                    settle.rows,
                    intended.width,
                    intended.height,
                    (size.width as i64 - intended.width as i64).abs()
                        + (size.height as i64 - intended.height as i64).abs()
                );
            }
            if settle.corrected
                && let Some(mut ws) = self.windows.get_mut(&wid)
            {
                ws.initial_frame_settle = None;
            }
            return false;
        }
        aterm_log::debug!(
            "initial-frame settle: {}x{} grids {}x{}, attach asked for {}x{} — \
             re-requesting {}x{} ({} correction(s) left)",
            size.width,
            size.height,
            grid_now.1,
            grid_now.0,
            settle.cols,
            settle.rows,
            intended.width,
            intended.height,
            settle.corrections.saturating_sub(1)
        );
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let ws = &mut **route_guard_ws;
        if let Some(s) = ws.initial_frame_settle.as_mut() {
            s.corrections -= 1;
            s.corrected = true;
        }
        let os_window = ws.os_window.clone();
        let granted = os_window
            .as_ref()
            .and_then(|w| w.request_inner_size(intended));
        drop(route_guard_ws);
        self.settle_take_over(wid, size, granted, settle.deadline);
        if granted.is_some()
            && let Some(w) = os_window
        {
            w.request_redraw();
        }
        true
    }

    /// How long a size the settle vetoed on an asynchronous backend stays parked.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) const SETTLE_VETO_FALLBACK: std::time::Duration =
        std::time::Duration::from_millis(250);

    /// The settle's take-over of a vetoed `Resized` size.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn settle_take_over(
        &mut self,
        wid: WindowId,
        vetoed: PhysicalSize<u32>,
        granted: Option<PhysicalSize<u32>>,
        settle_deadline: Instant,
    ) {
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return;
        };
        let ws = &mut **route_guard_ws;
        if let Some(applied) = granted {
            ws.pending_resize = None;
            ws.next_resize_settle = None;
            ws.set_window_pixels(applied);
            drop(route_guard_ws);
            self.sync_surface_to_window(wid);
            self.on_resize(wid, applied);
        } else {
            let fallback = (settle_now() + Self::SETTLE_VETO_FALLBACK).min(settle_deadline);
            ws.pending_resize = Some(vetoed);
            ws.next_resize_settle = Some(fallback);
        }
    }

    /// Logical window teardown (no winit).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "window_routing",
            action = "CloseWindow",
            project = "aterm_gui::App::project_window_routing"
        )
    )]
    pub(crate) fn close_window_logical(&mut self, wid: WindowId) -> CloseOutcome {
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseWindow(wid),
        )) {
            return CloseOutcome::Stay;
        }
        if !self.windows.contains_key(&wid) {
            return CloseOutcome::Stay; // stale/unknown id → no-op
        }
        if self.exits_app_when_closing(wid)
            && !self.accounts_drained_before(crate::app_account_work::Continuation::Window(
                wid,
                Some(crate::session_store::current_close_attribution()),
            ))
        {
            return CloseOutcome::Stay;
        }
        #[cfg(target_os = "macos")]
        self.withdraw_askpass_window(wid);
        let deferred_attribution = self
            .windows
            .get_mut(&wid)
            .as_deref_mut()
            .and_then(|ws| ws.pending_close_attribution.take());
        let _deferred_close = deferred_attribution
            .map(|(reason, actor)| crate::session_store::CloseAttribution::enter(reason, actor));
        let _ = self.video_abort_window_close(wid);
        if self.exits_app_when_closing(wid) {
            self.retire_spare("closed", None);
        } else {
            self.spare_poke();
        }
        if !self.windows.contains_key(&wid) {
            return CloseOutcome::Stay;
        }
        let exits_app = self.exits_app_when_closing(wid);
        if self.config.restore_session_or_default() && exits_app {
            self.quit_capture = Some(self.capture_restore_manifest());
        }
        self.cancel_restore_window(wid, exits_app);
        // aterm quits with this window: a Mac Setup tab in it goes with the process, its ask never
        // declined, but one whose program the quit carries stays in the pool to the run's end,
        // which leaves it to the keeper (`App::carry_developer_tools_install_at_exit`).
        let carried = if exits_app {
            self.note_developer_tools_tabs_quit();
            self.carry_developer_tools_install_at_exit()
        } else {
            Vec::new()
        };
        let Some(ws) = self.windows.get(&wid) else {
            return CloseOutcome::Stay;
        };
        let mut ids = self.window_terminal_sessions(wid);
        ids.retain(|id| !carried.contains(id));
        let mut view_ids: Vec<crate::tab_model::ViewId> = Vec::new();
        for tab in ws.tab_set.tabs() {
            tab.root.visit(&mut |&view| view_ids.push(view));
        }
        if let Some(view) = ws.dock.view {
            view_ids.push(view);
        }
        let plan = crate::session_log::ClosedKeep::plan(&self.pool.drops_of(&ids));
        for (id, planned) in ids.into_iter().zip(plan) {
            if self.detach_session_view(id) {
                let keep = if exits_app {
                    crate::session_log::ClosedKeep::Summary
                } else {
                    planned.unwrap_or(crate::session_log::ClosedKeep::Skeleton)
                };
                self.retire_session_registration_keeping(id, keep);
            }
        }
        for view in view_ids {
            self.remove_view_link(view);
        }
        #[cfg(windows)]
        {
            let hwnd = self.window_hwnd(wid);
            if hwnd != 0 {
                taskbar::forget(hwnd);
            }
        }
        let closed_home = self.tab_home_window() == Some(wid);
        self.windows.remove(&wid);
        self.navigator_mark_structure();
        self.retire_window_document_admissions(wid);
        // A document close still armed for `wid` (a failed save left it, then a
        // tab move emptied the window) must not outlive it: its documents may be
        // open in another window now.
        self.disarm_window_document_shutdown(wid);
        self.find_origins.retain(|(_, entry), _| *entry != wid);
        self._toolbars.remove(&wid);
        #[cfg(target_os = "macos")]
        if self.paste_confirm.as_ref().is_some_and(|c| c.wid == wid) {
            self.paste_confirm = None;
        }
        if self.paste_banner.as_ref().is_some_and(|p| p.wid == wid) {
            self.paste_banner = None;
        }
        if self.close_banner.as_ref().is_some_and(|p| p.wid == wid)
            && let Some(pending) = self.close_banner.take()
        {
            pending.replay.cancel();
        }
        self.close_banner_queue.retain(|q| {
            if q.wid == wid {
                q.replay.cancel();
                false
            } else {
                true
            }
        });
        self.ask_next_queued_close();
        self.winit_to_window.retain(|_, &mut v| v != wid);
        self.focus_order.retain(|w| *w != wid);
        if self.pending_deminiaturize_focus == Some(wid) {
            self.pending_deminiaturize_focus = None;
        }
        if self.frontmost_window == Some(wid) {
            self.frontmost_window = self.next_frontmost_after_close();
        }
        if closed_home && let Some(home) = self.tab_home_window() {
            self.promote_tab_home_chrome(home);
        }
        if !self.windows.is_empty() {
            debug_assert!(
                self.structural_invariants_ok(),
                "window/session structural invariants violated after close_window_logical: {}",
                self.structural_invariant_violation().unwrap_or_default(),
            );
            self.sync_active_session();
            debug_assert!(
                self.structural_invariants_ok(),
                "window/session structural invariants violated after re-mirror: {}",
                self.structural_invariant_violation().unwrap_or_default(),
            );
        }
        self.mark_status_dirty();
        if self.windows.is_empty() {
            CloseOutcome::Exit
        } else {
            CloseOutcome::Stay
        }
    }

    /// Record that `wid` gained OS focus.
    pub(crate) fn note_window_focused(&mut self, wid: WindowId) {
        self.focus_order.retain(|w| *w != wid);
        self.focus_order.push(wid);
        self.mark_status_dirty();
    }

    /// The window to make frontmost when the front window closes.
    pub(crate) fn next_frontmost_after_close(&self) -> Option<WindowId> {
        self.focus_order
            .iter()
            .rev()
            .find(|w| self.windows.contains_key(w))
            .copied()
            .or_else(|| self.windows.keys().next().copied())
    }

    /// Close window `wid` and exit the app iff it was the last window.
    pub(crate) fn close_window(&mut self, el: &ActiveEventLoop, wid: WindowId) {
        if matches!(self.close_window_through_barriers(wid), CloseOutcome::Exit) {
            el.exit();
        }
    }

    /// [`Self::close_window`] up to the exit: the handoff deferral, then the
    /// native and document barriers, then the logical close, answering
    /// whether it closed the last window.
    pub(crate) fn close_window_through_barriers(&mut self, wid: WindowId) -> CloseOutcome {
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseWindow(wid),
        )) {
            return CloseOutcome::Stay;
        }
        match self.prepare_window_native_shutdown(wid, crate::native_app::CloseScope::Window) {
            Ok(true) => {}
            Ok(false) => {
                self.disarm_account_window_close(wid);
                return CloseOutcome::Stay;
            }
            Err(error) => {
                aterm_log::warn!("window native close barrier: {error}");
                self.disarm_account_window_close(wid);
                return CloseOutcome::Stay;
            }
        }
        match self.prepare_window_document_shutdown(wid) {
            Ok(true) => {}
            Ok(false) => return CloseOutcome::Stay,
            Err(error) => {
                aterm_log::warn!("window document close barrier: {error}");
                self.disarm_account_window_close(wid);
                return CloseOutcome::Stay;
            }
        }
        self.close_window_logical(wid)
    }

    /// Re-read the frame window `wid` must draw itself ([`crate::client_frame`]) from its OS window.
    #[cfg(target_os = "linux")]
    pub(crate) fn sync_client_frame(&mut self, wid: WindowId) -> bool {
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let ws = &mut **route_guard_ws;
        let frame = ws
            .os_window
            .as_deref()
            .and_then(crate::client_frame::ClientFrame::of_window);
        if ws.client_frame == frame {
            return false;
        }
        ws.client_frame = frame;
        if frame.is_none_or(|frame| !frame.edges) {
            ws.frame_edge = None;
        }
        if frame.is_none_or(|frame| !frame.header) {
            ws.caption_move = None;
            ws.strip_hover_caption = None;
        }
        drop(route_guard_ws);
        self.sync_strip_header_row(wid)
    }

    /// The strip rows for the `configured` `tab_strip_rows`.
    pub(crate) fn strip_rows_for(&self, configured: u16) -> u16 {
        let header = self.windows.values().any(|ws| {
            ws.client_frame
                .is_some_and(crate::client_frame::ClientFrame::has_header)
        });
        crate::client_frame::strip_rows_with_header(configured, header)
    }

    /// Re-resolve the strip's rows ([`Self::strip_rows_for`] of the live config) after a window frame change.
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn sync_strip_header_row(&mut self, settling: WindowId) -> bool {
        self.strip_fall = None;
        let rows = self.strip_rows_for(crate::app_config::resolve_tab_strip_rows(&self.config));
        if rows == self.tab_strip_rows {
            return false;
        }
        if aterm_messages::chrome::is_rise(self.tab_strip_rows, rows) {
            self.note_chrome_rise_everywhere(Instant::now());
        }
        self.tab_strip_rows = rows;
        self.regrid_for_chrome_rows_but(settling);
        true
    }

    #[cfg(target_os = "linux")]
    fn fit_client_frame_at_attach(&mut self, wid: WindowId) {
        if !self.sync_client_frame(wid) {
            return;
        }
        let size = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map(|w| w.inner_size());
        if let Some(size) = size {
            self.apply_window_resized_grid(wid, size);
        }
    }

    /// Run a press on a window's own close button.
    pub(crate) fn escalate_caption_close(&mut self, el: &ActiveEventLoop) {
        let flagged: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.caption_close)
            .map(|(w, _)| *w)
            .collect();
        for wid in flagged {
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.caption_close = false;
            }
            let _closing = crate::session_store::CloseAttribution::enter(
                crate::session_store::ExitReason::WindowClose,
                crate::session_store::ExitActor::Human,
            );
            self.on_close_requested(el, wid);
        }
    }

    /// Close any window whose last-tab close set `pending_close`.
    pub(crate) fn escalate_pending_close(&mut self, el: &ActiveEventLoop) {
        let to_close: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.pending_close)
            .map(|(w, _)| *w)
            .collect();
        for w in to_close {
            if let Some(mut route_guard_ws) = self.windows.get_mut(&w) {
                let ws = &mut **route_guard_ws;
                ws.pending_close = false;
            }
            self.close_window(el, w);
        }
    }

    /// Whether session `id` is running a foreground job (not the idle shell).
    pub(crate) fn session_foreground_busy(&self, id: u64) -> bool {
        self.pool.get(id).is_some_and(Self::session_busy)
    }

    /// Whether a command runs in `s`'s foreground, as its shell and its terminal's process group
    /// say.
    pub(crate) fn session_busy(s: &crate::Session) -> bool {
        let word = {
            let term = s
                .term
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            crate::quit_safety::shell_word(&term)
        };
        crate::quit_safety::foreground_busy(
            crate::quit_safety::foreground_pgrp(s.master),
            s.pid,
            word,
        )
    }

    fn window_has_foreground_job(&self, wid: WindowId) -> bool {
        let mut ids = self.window_terminal_sessions(wid);
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().any(|id| {
            self.window_drops_last_session_view(wid, id) && self.session_foreground_busy(id)
        })
    }

    /// What closing `wid` would end now.
    pub(crate) fn window_close_consent(&self, wid: WindowId) -> WindowCloseConsent {
        self.window_close_consent_after(wid, &[])
    }

    /// What closing `wid` would end once the windows in `gone` have closed too.
    fn window_close_consent_after(&self, wid: WindowId, gone: &[WindowId]) -> WindowCloseConsent {
        let local = self.window_terminal_sessions(wid);
        let sessions = local
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        let views_in = |window: WindowId, session: u64| {
            self.window_terminal_sessions(window)
                .into_iter()
                .filter(|candidate| *candidate == session)
                .count() as u32
        };
        let busy = sessions
            .iter()
            .copied()
            .filter(|id| {
                let local_views = local.iter().filter(|candidate| *candidate == id).count() as u32;
                let gone_views = gone
                    .iter()
                    .filter(|window| **window != wid)
                    .map(|window| views_in(*window, *id))
                    .sum::<u32>();
                let drops_last = self
                    .pool
                    .views(*id)
                    .is_some_and(|all_views| all_views.saturating_sub(gone_views) <= local_views);
                drops_last && self.session_foreground_busy(*id)
            })
            .filter_map(|id| Some((id, self.session_job(id)?)))
            .collect();
        WindowCloseConsent {
            sessions,
            busy,
            exits_app: !self
                .windows
                .keys()
                .any(|window| *window != wid && !gone.contains(window)),
        }
    }

    /// Whether closing `wid` now, once the windows in `gone` have closed,
    /// ends nothing beyond `asked`: no terminal the window gained, no job that
    /// was not running when the close was asked, and not the app unless the
    /// window was already the last one.
    pub(crate) fn window_close_consent_holds(
        &self,
        wid: WindowId,
        asked: &WindowCloseConsent,
        gone: &[WindowId],
    ) -> bool {
        let now = self.window_close_consent_after(wid, gone);
        now.sessions.is_subset(&asked.sessions)
            && now
                .busy
                .iter()
                .all(|(session, job)| asked.busy.get(session) == Some(job))
            && (!now.exits_app || asked.exits_app)
    }

    /// The job session `id` runs ([`SessionJob`]).
    fn session_job(&self, id: u64) -> Option<SessionJob> {
        let session = self.pool.get(id)?;
        let finished_commands = crate::term_lock(&session.term).completed_command_seq();
        Some(SessionJob {
            foreground_group: crate::quit_safety::foreground_pgrp(session.master),
            finished_commands,
        })
    }

    fn window_drops_last_session_view(&self, wid: WindowId, session: u64) -> bool {
        let local_views = self
            .window_terminal_sessions(wid)
            .into_iter()
            .filter(|candidate| *candidate == session)
            .count() as u32;
        local_views > 0
            && self
                .pool
                .views(session)
                .is_some_and(|all_views| all_views <= local_views)
    }

    fn any_foreground_job(&self) -> bool {
        self.pool.iter().any(Self::session_busy)
    }

    /// Confirm a destructive close/quit gesture for window `wid`.
    pub(crate) fn confirm_destructive_close(
        &mut self,
        wid: WindowId,
        exits_app: bool,
        busy: bool,
    ) -> bool {
        self.confirm_destructive_gesture(
            wid,
            exits_app,
            busy,
            crate::close_confirm::CloseReplay::Window(wid),
        )
    }

    /// [`Self::confirm_destructive_close`] with the gesture's replay named.
    pub(crate) fn confirm_destructive_gesture(
        &mut self,
        wid: WindowId,
        exits_app: bool,
        busy: bool,
        replay: crate::close_confirm::CloseReplay,
    ) -> bool {
        if self.headless {
            return true;
        }
        match self.close_confirm {
            CloseConfirm::Programmatic => return true,
            CloseConfirm::WireRefuseBusy => {
                if busy {
                    self.pending_action_refusal = Some(WIRE_CLOSE_REFUSED.to_string());
                }
                return !busy;
            }
            CloseConfirm::Interactive => {}
        }
        let Some(prompt) = self.destructive_prompt(wid, exits_app, busy, replay) else {
            return true;
        };
        if !cfg!(any(target_os = "macos", windows)) && !busy {
            return true;
        }
        // The question owns its replay until the shutdown barriers run, and its answer comes
        // back through the ordinary event loop: a Mac window shows it as a sheet, which runs no
        // nested modal loop (`close_sheet`).
        self.present_close_banner(wid, prompt, replay)
    }

    pub(crate) fn destructive_prompt(
        &self,
        wid: WindowId,
        exits_app: bool,
        busy: bool,
        replay: crate::close_confirm::CloseReplay,
    ) -> Option<crate::quit_safety::ConfirmPrompt> {
        let reopens = self.config.restore_session_or_default();
        let view_close = match replay {
            crate::close_confirm::CloseReplay::Tab { .. } => Some(false),
            crate::close_confirm::CloseReplay::Pane { .. } => Some(true),
            crate::close_confirm::CloseReplay::Window(_)
            | crate::close_confirm::CloseReplay::Quit => None,
            #[cfg(unix)]
            crate::close_confirm::CloseReplay::NativeTerminate { .. } => None,
        };
        #[cfg(windows)]
        let prompt = {
            let tabs_closing: usize = if exits_app {
                self.windows.values().map(|ws| ws.tab_set.len()).sum()
            } else {
                self.windows.get(&wid).map_or(1, |ws| ws.tab_set.len())
            };
            crate::quit_safety::confirm_prompt_windows(exits_app, tabs_closing, busy, reopens)
        };
        #[cfg(not(windows))]
        let prompt = {
            let _ = wid;
            crate::quit_safety::confirm_prompt(exits_app, busy, reopens)
        };
        match view_close {
            Some(pane) => busy.then(|| crate::quit_safety::busy_view_close_prompt(pane)),
            None if exits_app => {
                // A quit that leaves the Mac Setup tab's install running says so, not that its
                // process ends.
                let carried = self.developer_tools_custody_at_exit();
                if carried.is_empty() {
                    prompt
                } else {
                    let others_busy = busy
                        && self
                            .pool
                            .iter()
                            .any(|s| !carried.contains(&s.id) && Self::session_busy(s));
                    Some(crate::quit_safety::quit_prompt_carrying(
                        others_busy,
                        reopens,
                    ))
                }
            }
            None => prompt,
        }
    }

    /// Park a destructive question over `wid` and refuse the gesture.
    pub(crate) fn present_close_banner(
        &mut self,
        wid: WindowId,
        prompt: crate::quit_safety::ConfirmPrompt,
        replay: crate::close_confirm::CloseReplay,
    ) -> bool {
        let (_, asked_by) = crate::session_store::current_close_attribution();
        let pending = crate::close_confirm::PendingClose::new(wid, prompt, replay, asked_by);
        let by_person = !pending.wire_may_answer();
        if let Some(standing) = self.close_banner.as_ref() {
            let standing_wid = standing.wid;
            self.pending_action_refusal = Some(standing.wire_refusal());
            let waits = by_person
                && wid != standing_wid
                && self.windows.contains_key(&wid)
                && !self.close_banner_queue.iter().any(|q| q.wid == wid);
            if waits {
                aterm_log::info!(
                    "close confirm open over window {}; the {} gesture over window {} \
                     is asked next",
                    standing_wid.0,
                    crate::close_confirm::kind(&pending.prompt),
                    wid.0,
                );
                self.stash_deferred_close_attribution(wid);
                self.close_banner_queue.push(pending);
            } else {
                aterm_log::info!(
                    "close confirm already open over window {}; refusing the new {} gesture \
                     until it is answered",
                    standing_wid.0,
                    crate::close_confirm::kind(&pending.prompt),
                );
            }
            if by_person {
                self.raise_close_question(standing_wid, true);
            }
            return false;
        }
        let refusal = pending.wire_refusal();
        if !self.stand_close_question(pending, by_person) {
            return false;
        }
        self.pending_action_refusal = Some(refusal);
        self.stash_deferred_close_attribution(wid);
        false
    }

    /// Stand `pending` as the open question: its window raised (and focused when `focus`), and a
    /// Mac window showing it as a native sheet. `false`, with nothing standing, when a Mac
    /// window's key queue would not drain, so a key queued before the question could answer it.
    /// Stub and headless windows never enter AppKit, and draw the question in the window.
    fn stand_close_question(
        &mut self,
        pending: crate::close_confirm::PendingClose,
        focus: bool,
    ) -> bool {
        let wid = pending.wid;
        #[cfg(target_os = "macos")]
        let mut pending = pending;
        #[cfg(target_os = "macos")]
        let native = match self
            .windows
            .get(&wid)
            .and_then(|window| window.os_window.as_deref())
        {
            Some(window) => {
                let Some(fence) = crate::close_sheet::fence() else {
                    return false;
                };
                Some((crate::close_sheet::parent_of(window), fence))
            }
            None => None,
        };
        self.raise_close_question(wid, focus);
        #[cfg(target_os = "macos")]
        if let Some((parent, fence)) = native {
            let persons = !pending.wire_may_answer();
            pending.sheet = parent.zip(self.proxy.clone()).and_then(|(parent, proxy)| {
                crate::close_sheet::present(
                    parent,
                    &pending.prompt,
                    pending.serial(),
                    persons,
                    proxy,
                    fence,
                )
            });
            if pending.sheet.is_none() {
                aterm_log::warn!(
                    "close/quit question over window {}: no native sheet could be built; it \
                     stands in the window",
                    wid.0
                );
            }
        }
        self.close_banner = Some(pending);
        true
    }

    fn raise_close_question(&mut self, wid: WindowId, focus: bool) {
        if focus {
            self.note_restore_focus_intent(wid);
        }
        let Some((w, focused)) = self
            .windows
            .get(&wid)
            .and_then(|ws| Some((ws.os_window.clone()?, ws.focused)))
        else {
            return;
        };
        if w.is_minimized() == Some(true) {
            // `focus_window` no-ops on a minimized window and the restore is
            // asynchronous: the `Occluded(false)` arm finishes the raise.
            if focus {
                self.pending_deminiaturize_focus = Some(wid);
            }
            w.set_minimized(false);
        }
        if focus && !focused {
            w.focus_window();
        }
        w.request_redraw();
    }

    /// Ask the first question that waited behind the one just answered or dropped.
    pub(crate) fn ask_next_queued_close(&mut self) {
        while self.close_banner.is_none() && !self.close_banner_queue.is_empty() {
            let mut next = self.close_banner_queue.remove(0);
            if !self.windows.contains_key(&next.wid) {
                next.replay.cancel();
                continue;
            }
            if let crate::close_confirm::CloseReplay::Window(wid) = next.replay {
                let exits_app = self.exits_app_when_closing(wid);
                let busy = self.window_has_foreground_job(wid);
                let asks = self
                    .destructive_prompt(wid, exits_app, busy, next.replay)
                    .filter(|_| cfg!(any(windows, target_os = "macos")) || busy);
                let Some(prompt) = asks else {
                    if let Some(proxy) = self.proxy.as_ref() {
                        let _ = proxy.send_event(crate::Wake::ConfirmedClose {
                            replay: next.replay,
                        });
                    }
                    continue;
                };
                next.prompt = prompt;
            }
            let (replay, focus) = (next.replay, !next.wire_may_answer());
            if !self.stand_close_question(next, focus) {
                replay.cancel();
            }
        }
    }

    /// Answer the standing close/quit question; dropping it ends its sheet, if one shows it.
    pub(crate) fn answer_close_banner(
        &mut self,
        proceed: bool,
    ) -> Option<crate::close_confirm::CloseReplay> {
        let pending = self.close_banner.take()?;
        if let Some(w) = self
            .windows
            .get(&pending.wid)
            .and_then(|ws| ws.os_window.as_ref())
        {
            w.request_redraw();
        }
        if !proceed {
            pending.replay.cancel();
            self.resume_drained_accounts();
            // The attribution `present_close_banner` stashed was for a replay that
            // will not run; a later close must not inherit this gesture's name.
            if let Some(mut route_guard_ws) = self.windows.get_mut(&pending.wid) {
                let ws = &mut **route_guard_ws;
                ws.pending_close_attribution = None;
            }
            self.ask_next_queued_close();
            return None;
        }
        if let Some(proxy) = self.proxy.as_ref() {
            let _ = proxy.send_event(crate::Wake::ConfirmedClose {
                replay: pending.replay,
            });
        }
        Some(pending.replay)
    }

    /// The `confirm yes|no` verb ([`crate::Wake::AnswerCloseConfirm`]).
    pub(crate) fn answer_close_confirm_from_wire(
        &mut self,
        proceed: bool,
    ) -> Result<String, String> {
        let Some(pending) = self.close_banner.as_ref() else {
            return Err(crate::close_confirm::NOTHING_PENDING.to_string());
        };
        if !pending.wire_may_answer() {
            return Err(pending.wire_refusal());
        }
        let reply = pending.wire_answered(proceed);
        self.answer_close_banner(proceed);
        Ok(reply)
    }

    /// Serve [`crate::Wake::ConfirmedClose`].
    pub(crate) fn replay_confirmed_close(
        &mut self,
        el: &ActiveEventLoop,
        replay: crate::close_confirm::CloseReplay,
    ) {
        let policy = self.close_confirm;
        self.close_confirm = CloseConfirm::Programmatic;
        match replay {
            crate::close_confirm::CloseReplay::Window(wid) => {
                if self.windows.contains_key(&wid) {
                    self.close_window(el, wid);
                } else {
                    aterm_log::info!(
                        "confirmed close of window {} dropped: it closed while the question stood",
                        wid.0
                    );
                }
            }
            crate::close_confirm::CloseReplay::Quit => {
                for pending in &self.close_banner_queue {
                    pending.replay.cancel();
                }
                self.close_banner_queue.clear();
                self.on_quit_requested(el);
            }
            #[cfg(unix)]
            crate::close_confirm::CloseReplay::NativeTerminate { generation } => {
                self.resume_native_termination(el, generation, true);
            }
            crate::close_confirm::CloseReplay::Tab { .. }
            | crate::close_confirm::CloseReplay::Pane { .. } => {
                if let Some(wid) = self.replay_confirmed_view_close(replay) {
                    self.close_window(el, wid);
                }
            }
        }
        self.close_confirm = policy;
        self.ask_next_queued_close();
    }

    /// The tab or pane close a banner stood for, re-run under the caller's policy.
    pub(crate) fn replay_confirmed_view_close(
        &mut self,
        replay: crate::close_confirm::CloseReplay,
    ) -> Option<WindowId> {
        let asker = match replay {
            crate::close_confirm::CloseReplay::Tab { wid, .. }
            | crate::close_confirm::CloseReplay::Pane { wid, .. } => self
                .windows
                .get_mut(&wid)
                .as_deref_mut()
                .and_then(|ws| ws.pending_close_attribution.take()),
            crate::close_confirm::CloseReplay::Window(_)
            | crate::close_confirm::CloseReplay::Quit => None,
            #[cfg(unix)]
            crate::close_confirm::CloseReplay::NativeTerminate { .. } => None,
        };
        let _asked = asker
            .map(|(reason, actor)| crate::session_store::CloseAttribution::enter(reason, actor));
        match replay {
            crate::close_confirm::CloseReplay::Tab { wid, tab } => self
                .replay_deferred_handoff_tab_close(wid, tab)
                .then_some(wid),
            crate::close_confirm::CloseReplay::Pane { wid, tab, view } => {
                self.replay_deferred_handoff_view_close(wid, tab, view)
            }
            crate::close_confirm::CloseReplay::Window(_)
            | crate::close_confirm::CloseReplay::Quit => None,
            #[cfg(unix)]
            crate::close_confirm::CloseReplay::NativeTerminate { .. } => None,
        }
    }

    /// The close/quit question window `wid` draws as its band: one standing over it that no
    /// native sheet presents.
    pub(crate) fn close_band(&self, wid: WindowId) -> Option<&crate::close_confirm::PendingClose> {
        self.close_banner
            .as_ref()
            .filter(|pending| pending.wid == wid && pending.in_window())
    }

    /// The `controls front` line for the close/quit confirm.
    pub(crate) fn close_confirm_controls_line(&self) -> Option<String> {
        match self.close_banner.as_ref() {
            Some(pending) if !self.close_banner_queue.is_empty() => Some(format!(
                "{} waiting={}",
                pending.controls_line(),
                self.close_banner_queue.len()
            )),
            Some(pending) => Some(pending.controls_line()),
            None => Some(crate::close_confirm::CONTROLS_CLOSED.to_string()),
        }
    }

    /// Quit-safety for a tab close that would exit the window.
    pub(crate) fn window_exit_close_allowed(&mut self, wid: WindowId, exits_window: bool) -> bool {
        if !exits_window {
            return true;
        }
        let exits_app = self.exits_app_when_closing(wid);
        let busy = self.window_has_foreground_job(wid);
        self.confirm_destructive_close(wid, exits_app, busy)
    }

    /// Quit-safety for a tab or pane close that leaves window `wid` standing.
    pub(crate) fn view_close_allowed(
        &mut self,
        wid: WindowId,
        busy: bool,
        replay: crate::close_confirm::CloseReplay,
    ) -> bool {
        if !busy || (cfg!(target_os = "macos") && self.close_confirm == CloseConfirm::Interactive) {
            return true;
        }
        self.confirm_destructive_gesture(wid, false, true, replay)
    }

    /// Whether closing `wid` leaves no window: the windows a pending update
    /// handoff will close when it replays count as gone already.
    fn exits_app_when_closing(&self, wid: WindowId) -> bool {
        let closing = self.windows_closing_behind_handoff();
        !self
            .windows
            .keys()
            .any(|w| *w != wid && !closing.contains(w))
    }

    /// Quit-safety for CloseRequested (close button / Window ▸ Close).
    pub(crate) fn on_close_requested(&mut self, el: &ActiveEventLoop, wid: WindowId) {
        let exits_app = self.exits_app_when_closing(wid);
        let busy = self.window_has_foreground_job(wid);
        if self.confirm_destructive_close(wid, exits_app, busy) {
            self.close_window(el, wid);
        }
    }

    /// Quit-safety for whole-app Quit (Cmd-Q / app menu).
    pub(crate) fn on_quit_requested(&mut self, el: &ActiveEventLoop) {
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested)
        {
            return;
        }
        self.note_quit_by();
        let anchor = self
            .frontmost_window
            .filter(|w| self.windows.contains_key(w))
            .or_else(|| self.windows.keys().next().copied());
        if let Some(wid) = anchor {
            let busy = self.any_foreground_job();
            if !self.confirm_destructive_gesture(
                wid,
                true,
                busy,
                crate::close_confirm::CloseReplay::Quit,
            ) {
                self.resume_drained_accounts();
                return;
            }
        }
        if self.quit_barriers() != QuitBarrier::Ready {
            return;
        }
        let _ = self.video_abort_app_shutdown();
        el.exit();
    }

    /// Both Quit barriers, in the order every Quit edge asks them.
    pub(crate) fn quit_barriers(&mut self) -> QuitBarrier {
        match self.prepare_quit_native_shutdown() {
            Ok(true) => {}
            Ok(false) => {
                aterm_log::info!(
                    "quit waits: a tab holds unsaved work, and its Review and Discard are on it"
                );
                self.resume_drained_accounts();
                return QuitBarrier::Refused;
            }
            Err(error) => {
                aterm_log::warn!("quit native close barrier: {error}");
                self.resume_drained_accounts();
                return QuitBarrier::Refused;
            }
        }
        match self.prepare_quit_document_shutdown() {
            Ok(true) => {
                if self.accounts_drained_before(crate::app_account_work::Continuation::Quit) {
                    QuitBarrier::Ready
                } else {
                    QuitBarrier::Saving
                }
            }
            Ok(false) if !self.quit_document_shutdown_blocked() => QuitBarrier::Saving,
            Ok(false) => {
                self.cancel_failed_quit_document_shutdown();
                self.resume_drained_accounts();
                QuitBarrier::Refused
            }
            Err(error) => {
                aterm_log::warn!("quit document close barrier: {error}");
                self.cancel_failed_quit_document_shutdown();
                self.resume_drained_accounts();
                QuitBarrier::Refused
            }
        }
    }

    /// A Quit whose barriers were ready when it deferred behind an update handoff.
    pub(crate) fn replay_clean_quit_ready(&mut self) -> bool {
        match self.quit_barriers() {
            QuitBarrier::Ready => {
                let _ = crate::menu::complete_current_native_termination();
                let _ = self.video_abort_app_shutdown();
                true
            }
            QuitBarrier::Saving => false,
            QuitBarrier::Refused => {
                let _ = crate::menu::cancel_current_native_termination();
                false
            }
        }
    }

    /// The process that started this headless instance is gone (`crate::lifeline`).
    #[cfg(unix)]
    pub(crate) fn on_lifeline_cut(&mut self, el: &ActiveEventLoop) {
        if !self.headless {
            aterm_log::warn!("lifeline cut in a windowed instance; leaving it open");
            return;
        }
        self.on_unattended_quit(el, "the process that started this headless aterm is gone");
    }

    /// A headless instance was sent a terminating signal (`owned_endpoint::arm_orderly_quit`).
    #[cfg(unix)]
    pub(crate) fn on_exit_signal(&mut self, el: &ActiveEventLoop, sig: i32) {
        if !self.headless {
            aterm_log::warn!("signal {sig} reached a windowed instance's quit path; ending now");
            crate::owned_endpoint::die_of_quit_signal();
            return;
        }
        self.on_unattended_quit(el, "a terminating signal reached this headless aterm");
    }

    /// A Quit nobody at a window started (`cause`): no one can answer a tab's question, and the
    /// process ends at its backstop whatever is answered, so no tab's unsaved work holds it
    /// ([`App::prepare_quit_native_shutdown`]); documents still save on the way out.
    #[cfg(unix)]
    pub(crate) fn on_unattended_quit(&mut self, el: &ActiveEventLoop, cause: &'static str) {
        self.quit_unattended.get_or_insert(cause);
        // Nobody at a window asked: its tabs' closes name no actor.
        self.quit_by
            .get_or_insert(crate::session_store::ExitActor::Unknown);
        self.on_quit_requested(el);
    }

    /// Who asks for the quit now, once ([`crate::App::quit_by`]): the act under way's actor (a
    /// control client's `invoke` of Quit), else the person (⌘Q, the menu, the Dock, a logout).
    pub(crate) fn note_quit_by(&mut self) {
        self.quit_by.get_or_insert_with(|| {
            crate::tab_marks::acting()
                .map_or(crate::session_store::ExitActor::Human, |acting| acting.by)
        });
    }

    /// Stateful counterpart of AppKit's bare `applicationShouldTerminate:` callback.
    #[cfg(target_os = "macos")]
    pub(crate) fn on_native_terminate_requested(&mut self, el: &ActiveEventLoop, generation: u64) {
        self.resume_native_termination(el, generation, false);
    }

    /// Replay an accepted native request without asking again after an update handoff.
    #[cfg(unix)]
    pub(crate) fn resume_native_termination(
        &mut self,
        el: &ActiveEventLoop,
        generation: u64,
        confirmed: bool,
    ) {
        if !crate::menu::native_termination_is_current(generation) {
            return;
        }
        if self.defer_pending_update_handoff_teardown(
            crate::DeferredHandoffTeardown::NativeTerminate {
                generation,
                confirmed,
            },
        ) {
            return;
        }
        self.note_quit_by();
        let policy = self.close_confirm;
        if confirmed {
            self.close_confirm = CloseConfirm::Programmatic;
        }
        let ready = self.native_termination_ready(generation);
        self.close_confirm = policy;
        if !ready {
            return;
        }
        for pending in &self.close_banner_queue {
            pending.replay.cancel();
        }
        self.close_banner_queue.clear();
        let _ = self.video_abort_app_shutdown();
        el.exit();
    }

    /// Keep the native request's identity while its question is outstanding. A quit the system
    /// or System Settings' Quit & Reopen asked for ([`crate::system_quit::explicit`]) is asked
    /// nothing, busy tabs (the Mac Setup tab's installer among them) or not: the person's logout
    /// or Quit & Reopen is their answer, and a question of aterm's own over it would hold the
    /// quit, and every System Settings step that waits for it (`App::system_settings_may_ask`),
    /// until someone answered it. One the person's own quit already put up for this generation goes
    /// with its sheet. The barriers still run: a tab's unsaved work and the documents' saves hold
    /// such a quit as any other.
    #[cfg(unix)]
    pub(crate) fn native_termination_ready(&mut self, generation: u64) -> bool {
        if !crate::menu::native_termination_is_current(generation) {
            return false;
        }
        let explicit = crate::system_quit::explicit(generation);
        if explicit {
            self.withdraw_native_question(generation);
        }
        let anchor = self
            .frontmost_window
            .filter(|window| self.windows.contains_key(window))
            .or_else(|| self.windows.keys().next().copied())
            .filter(|_| !explicit);
        if let Some(window) = anchor {
            let busy = self.any_foreground_job();
            if !self.confirm_destructive_gesture(
                window,
                true,
                busy,
                crate::close_confirm::CloseReplay::NativeTerminate { generation },
            ) {
                let waiting = self
                    .close_banner
                    .iter()
                    .chain(self.close_banner_queue.iter())
                    .any(|pending| {
                        pending.replay
                            == crate::close_confirm::CloseReplay::NativeTerminate { generation }
                    });
                if !waiting {
                    let _ = crate::menu::cancel_native_termination(generation);
                }
                self.resume_drained_accounts();
                return false;
            }
        }

        match self.quit_barriers() {
            QuitBarrier::Ready => {}
            QuitBarrier::Saving => {
                self.account_quit_generation(generation);
                return false;
            }
            QuitBarrier::Refused => {
                let _ = crate::menu::cancel_native_termination(generation);
                if explicit {
                    // The quit that withdrew its question will not happen: a close question
                    // that waited behind that one is asked now.
                    self.ask_next_queued_close();
                }
                return false;
            }
        }
        crate::menu::complete_native_termination(generation)
    }

    /// Withdraw the question standing or waiting for the native quit of `generation`, the quit
    /// itself left pending: dropping it ends its sheet (`close_sheet::CloseSheet`'s drop), the
    /// window that showed it redraws without it, and the attribution its gesture stashed there
    /// goes, as for an answer that replays nothing (`Self::answer_close_banner`). A question
    /// queued behind it waits for the quit's barriers ([`Self::native_termination_ready`]).
    #[cfg(unix)]
    fn withdraw_native_question(&mut self, generation: u64) {
        let replay = crate::close_confirm::CloseReplay::NativeTerminate { generation };
        if self
            .close_banner
            .as_ref()
            .is_some_and(|pending| pending.replay == replay)
            && let Some(pending) = self.close_banner.take()
            && let Some(mut ws) = self.windows.get_mut(&pending.wid)
        {
            if let Some(w) = ws.os_window.as_ref() {
                w.request_redraw();
            }
            ws.pending_close_attribution = None;
        }
        self.close_banner_queue
            .retain(|pending| pending.replay != replay);
    }

    /// Re-evaluate document-owned close plans after an asynchronous atomic-save completion.
    pub(crate) fn escalate_document_shutdown(&mut self, el: &ActiveEventLoop) {
        let (quit, windows) = match self.take_ready_document_shutdowns() {
            Ok(ready) => ready,
            Err(error) => {
                aterm_log::warn!("document close escalation: {error}");
                return;
            }
        };
        if quit {
            if self.defer_pending_update_handoff_teardown(
                crate::DeferredHandoffTeardown::CleanQuitReady,
            ) {
                return;
            }
            if self.replay_clean_quit_ready() {
                el.exit();
            }
            return;
        }
        for window in windows {
            if matches!(
                self.close_window_through_barriers(window),
                CloseOutcome::Exit
            ) {
                el.exit();
                return;
            }
        }
    }

    fn window_title_identity(
        &self,
        id: WindowId,
        live_title: &str,
        slot: &mut String,
    ) -> Option<u64> {
        let focused_session = self.focused_session_id(id);
        let presentation_fallback = self
            .windows
            .get(&id)
            .and_then(|state| state.tab_set.active())
            .map(|tab| tab.presentation.title.as_str())
            .filter(|title| !title.is_empty())
            .unwrap_or("aterm");
        let Some(session_id) = focused_session else {
            let title = if live_title.is_empty() {
                presentation_fallback
            } else {
                live_title
            };
            slot.clear();
            slot.push_str(title);
            return None;
        };
        let Some(session) = self.pool.get(session_id) else {
            slot.clear();
            slot.push_str(presentation_fallback);
            return Some(session_id);
        };
        slot.clear();
        let user_titled = session
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .presentation_value_into("title", slot);
        if user_titled {
        } else if !live_title.is_empty() {
            slot.push_str(live_title);
        } else if let Some(line) = crate::app_take_over::task_line_of(&session.ctx) {
            // A title that says nothing leaves the strip's rungs: the task's first line comes
            // before the folder there ([`crate::app_tabs::resolved_terminal_title_rung`]), and a
            // busy terminal's arm below reads what the strip resolved, so both arms name it.
            slot.push_str(&line);
        } else {
            let filled = match session.term.try_lock() {
                Ok(term) => {
                    use crate::cwd_native::ReportedCwd as _;
                    let cwd = term.native_working_directory();
                    push_home_abbreviated_cwd(slot, cwd.as_deref())
                }
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    use crate::cwd_native::ReportedCwd as _;
                    let term = poisoned.into_inner();
                    let cwd = term.native_working_directory();
                    push_home_abbreviated_cwd(slot, cwd.as_deref())
                }
                Err(std::sync::TryLockError::WouldBlock) => match self
                    .windows
                    .get(&id)
                    .and_then(|state| state.tab_title_cache.get(&session_id))
                    .map(|cached| cached.title.as_str())
                    .filter(|title| !title.is_empty())
                {
                    Some(cached) => {
                        slot.push_str(cached);
                        true
                    }
                    None => false,
                },
            };
            if !filled {
                slot.push_str(presentation_fallback);
            }
        }
        Some(session_id)
    }

    /// Reflect the focused terminal's identity in the window chrome, falling back to `"aterm"`.
    pub(crate) fn apply_title(&mut self, id: WindowId, window: &Window, title: &str) {
        #[cfg(windows)]
        self.sync_taskbar_progress(id);
        let title = self.publish_active_terminal_title(id, title);
        let title: &str = &title;
        let mut scratch = std::mem::take(&mut self.window_title_scratch);
        let focused_session = self.window_title_identity(id, title, &mut scratch);
        self.chrome_labels.present_into(
            focused_session,
            crate::chrome_label::ChromeSurface::WindowTitle,
            &mut scratch,
        );
        let search_active = self.windows.get(&id).is_some_and(|ws| ws.search.is_some());
        let title_authority = window_title_authority(search_active);
        let title_changed = {
            let Some(mut route_guard_ws) = self.windows.get_mut(&id) else {
                self.window_title_scratch = scratch;
                return;
            };
            let ws = &mut **route_guard_ws;
            if ws.current_title != scratch
                && !crate::toolbar::busy_spinner_phase_only_change(&ws.current_title, &scratch)
            {
                // `current_title` keeps the frame for the strip and accessibility.
                if title_authority == WindowTitleAuthority::Canonical {
                    crate::elevated_caption::set_title(window, os_window_title(&scratch));
                }
                ws.current_title.clear();
                ws.current_title.push_str(&scratch);
                true
            } else {
                false
            }
        };
        self.window_title_scratch = scratch;
        if title_changed {
            self.refresh_window_tabs(id);
        }
    }

    /// Apply one engine-authorized xtwinops window manipulation on the main thread.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn on_window_op(
        &mut self,
        window: WindowId,
        session: u64,
        op: aterm_types::WindowOperation,
    ) {
        use aterm_types::WindowOperation as W;
        let Some(wid) = self.window_op_target(session, window) else {
            return;
        };
        if let W::ResizeWindowCells { rows, cols } = op {
            if self.focused_session_id(wid) == Some(session) {
                let _ = self.input(
                    wid,
                    crate::input::InputEvent::Resize {
                        rows,
                        cols,
                        echo_to_window: true,
                    },
                    crate::input::Source::Controller,
                );
            }
            return;
        }
        if let W::ResizeWindowPixels { height, width } = op {
            let asked = PhysicalSize::new(u32::from(width), u32::from(height));
            // A headless window takes a size only where there is one to take.
            if self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.os_window.is_some() || (width > 0 && height > 0))
            {
                let _ = self.resize_window_to_px(wid, asked);
            }
            return;
        }
        if matches!(op, W::RaiseWindow) {
            self.note_restore_focus_intent(wid);
        }
        let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) else {
            return; // headless / not yet attached: nothing to manipulate
        };
        match op {
            W::DeIconify => w.set_minimized(false),
            W::Iconify => w.set_minimized(true),
            W::RaiseWindow => w.focus_window(),
            W::RefreshWindow => w.request_redraw(),
            W::RestoreMaximized => w.set_maximized(false),
            W::MaximizeWindow => w.set_maximized(true),
            W::UndoFullscreen => w.set_fullscreen(None),
            W::EnterFullscreen => {
                w.set_fullscreen(Some(aterm_window::window::Fullscreen::Borderless(None)));
            }
            W::ToggleFullscreen => {
                if w.fullscreen().is_some() {
                    w.set_fullscreen(None);
                } else {
                    w.set_fullscreen(Some(aterm_window::window::Fullscreen::Borderless(None)));
                }
            }
            W::LowerWindow | W::MaximizeVertically | W::MaximizeHorizontally => {}
            _ => {}
        }
    }

    #[cfg(any(target_os = "linux", test))]
    fn window_op_target(&self, session: u64, stamped: WindowId) -> Option<WindowId> {
        self.windows_with_focused_session(session)
            .first()
            .copied()
            .or_else(|| self.windows.contains_key(&stamped).then_some(stamped))
    }

    /// Write window `id`'s title to its focused session's store handle and return it: `shown`, the shown engine's, or the live engine's while that pane is rewound, since the handle holds the live session's title.
    pub(crate) fn publish_active_terminal_title<'a>(
        &mut self,
        id: WindowId,
        shown: &'a str,
    ) -> std::borrow::Cow<'a, str> {
        let title = self
            .live_title_while_rewound(id)
            .map_or(std::borrow::Cow::Borrowed(shown), std::borrow::Cow::Owned);
        self.publish_terminal_title(id, &title);
        title
    }

    /// While window `id`'s focused pane is rewound, its live engine's title (the store's cached one while another thread holds that engine).
    fn live_title_while_rewound(&self, id: WindowId) -> Option<String> {
        let session = self.rewound_front(id)?;
        let live = self.pool.get(session)?.live_term();
        Some(match crate::term_try_lock(live) {
            Some(term) => term.title_arc().to_string(),
            None => self.windows.get(&id)?.store_title.1.clone(),
        })
    }

    fn publish_terminal_title(&mut self, id: WindowId, title: &str) {
        if self.active_native_view(id).is_some() {
            return;
        }
        if let Some(aid) = self
            .windows
            .get(&id)
            .and_then(|ws| ws.tab_set.active())
            .map(|tab| tab.focus)
            .and_then(|view| self.view_store.get(view).copied())
            .and_then(crate::tab_model::View::terminal_session)
        {
            let stale = self.windows.get(&id).is_some_and(|ws| {
                ws.store_title.0 != aid
                    || (ws.store_title.1 != title
                        && !crate::toolbar::busy_spinner_phase_only_change(
                            &ws.store_title.1,
                            title,
                        ))
            });
            if stale {
                if let Some(s) = self.pool.get(aid) {
                    self.store
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .set_title(s.id, title);
                }
                if let Some(mut route_guard_ws) = self.windows.get_mut(&id) {
                    let ws = &mut **route_guard_ws;
                    ws.store_title.0 = aid;
                    ws.store_title.1.clear();
                    ws.store_title.1.push_str(title);
                }
            }
        }
    }

    /// Reflect window `wid`'s focused-session ConEmu taskbar progress.
    #[cfg(windows)]
    pub(crate) fn sync_taskbar_progress(&self, wid: WindowId) {
        let hwnd = self.window_hwnd(wid);
        if hwnd == 0 {
            return; // headless, or no OS window yet — nothing to reflect onto
        }
        let progress = self
            .front_terminal(wid)
            .and_then(|terminal| crate::term_lock(&terminal.term).taskbar_progress());
        taskbar::set_progress(hwnd, progress);
        let focused = self
            .windows
            .get(&wid)
            .is_some_and(|ws| crate::os_window_focused(ws));
        self.sample_recent_dirs(hwnd, focused);
    }
}

/// Hand-rolled COM ffi for the Windows taskbar progress indicator (`ITaskbarList3`).
#[cfg(windows)]
pub(crate) mod taskbar {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::c_void;

    use aterm_types::TaskbarProgress;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    const CLSID_TASKBAR_LIST: Guid = Guid {
        data1: 0x56FD_F344,
        data2: 0xFD6D,
        data3: 0x11D0,
        data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
    };
    const IID_ITASKBAR_LIST3: Guid = Guid {
        data1: 0xEA1A_FB91,
        data2: 0x9E28,
        data3: 0x4B86,
        data4: [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF],
    };

    const CLSCTX_INPROC_SERVER: u32 = 0x1;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;

    const TBPF_NOPROGRESS: u32 = 0x0;
    const TBPF_INDETERMINATE: u32 = 0x1;
    const TBPF_NORMAL: u32 = 0x2;
    const TBPF_ERROR: u32 = 0x4;
    const TBPF_PAUSED: u32 = 0x8;

    #[repr(C)]
    struct ITaskbarList3Vtbl {
        query_interface:
            unsafe extern "system" fn(*mut ITaskbarList3, *const Guid, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut ITaskbarList3) -> u32,
        release: unsafe extern "system" fn(*mut ITaskbarList3) -> u32,
        hr_init: unsafe extern "system" fn(*mut ITaskbarList3) -> i32,
        add_tab: unsafe extern "system" fn(*mut ITaskbarList3, isize) -> i32,
        delete_tab: unsafe extern "system" fn(*mut ITaskbarList3, isize) -> i32,
        activate_tab: unsafe extern "system" fn(*mut ITaskbarList3, isize) -> i32,
        set_active_alt: unsafe extern "system" fn(*mut ITaskbarList3, isize) -> i32,
        mark_fullscreen_window: unsafe extern "system" fn(*mut ITaskbarList3, isize, i32) -> i32,
        set_progress_value: unsafe extern "system" fn(*mut ITaskbarList3, isize, u64, u64) -> i32,
        set_progress_state: unsafe extern "system" fn(*mut ITaskbarList3, isize, u32) -> i32,
    }

    #[repr(C)]
    struct ITaskbarList3 {
        vtbl: *const ITaskbarList3Vtbl,
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, co_init: u32) -> i32;
        fn CoCreateInstance(
            rclsid: *const Guid,
            outer: *mut c_void,
            cls_ctx: u32,
            riid: *const Guid,
            ppv: *mut *mut c_void,
        ) -> i32;
    }

    struct Cache {
        /// The COM object, created lazily on this (UI/sta) thread.
        obj: Option<*mut ITaskbarList3>,
        /// Set once creation has been attempted.
        tried_init: bool,
        /// Per-HWND last-applied `(flag<<32 | value)`.
        applied: HashMap<isize, u64>,
    }

    thread_local! {
        static CACHE: RefCell<Cache> =
            RefCell::new(Cache { obj: None, tried_init: false, applied: HashMap::new() });
    }

    impl Cache {
        fn object(&mut self) -> Option<*mut ITaskbarList3> {
            if let Some(o) = self.obj {
                return Some(o);
            }
            if self.tried_init {
                return None;
            }
            self.tried_init = true;
            // SAFETY: standard COM bring-up. `CoInitializeEx` is idempotent (S_FALSE
            // and RPC_E_CHANGED_MODE are non-fatal, so its HRESULT is ignored).
            // A failed `CoCreateInstance` HRESULT or null pointer is rejected before
            // use; `HrInit` precedes any progress call, and on failure the object
            // is released.
            unsafe {
                let _ = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
                let mut ppv: *mut c_void = std::ptr::null_mut();
                let hr = CoCreateInstance(
                    &CLSID_TASKBAR_LIST,
                    std::ptr::null_mut(),
                    CLSCTX_INPROC_SERVER,
                    &IID_ITASKBAR_LIST3,
                    &mut ppv,
                );
                if hr < 0 || ppv.is_null() {
                    return None;
                }
                let obj = ppv.cast::<ITaskbarList3>();
                let vtbl = &*(*obj).vtbl;
                if (vtbl.hr_init)(obj) < 0 {
                    (vtbl.release)(obj);
                    return None;
                }
                self.obj = Some(obj);
                Some(obj)
            }
        }
    }

    fn encode(progress: Option<TaskbarProgress>) -> (u32, u64) {
        match progress {
            None | Some(TaskbarProgress::Hidden) => (TBPF_NOPROGRESS, 0),
            Some(TaskbarProgress::Indeterminate) => (TBPF_INDETERMINATE, 0),
            Some(TaskbarProgress::Normal(v)) => (TBPF_NORMAL, u64::from(v)),
            Some(TaskbarProgress::Error(v)) => (TBPF_ERROR, u64::from(v)),
            Some(TaskbarProgress::Paused(v)) => (TBPF_PAUSED, u64::from(v)),
            Some(_) => (TBPF_NOPROGRESS, 0),
        }
    }

    /// Apply `progress` to the taskbar button of `hwnd`.
    pub(crate) fn set_progress(hwnd: isize, progress: Option<TaskbarProgress>) {
        let (flag, completed) = encode(progress);
        let encoded = (u64::from(flag) << 32) | completed;
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if c.applied.get(&hwnd) == Some(&encoded) {
                return; // unchanged since the last frame — do not touch COM
            }
            let Some(obj) = c.object() else {
                return; // no taskbar interface available
            };
            // SAFETY: `obj` is a live `ITaskbarList3` created on this thread and `hwnd` a live
            // window handle (0 rejected by the caller); both calls are synchronous and take
            // integers.
            unsafe {
                let vtbl = &*(*obj).vtbl;
                (vtbl.set_progress_state)(obj, hwnd, flag);
                if flag != TBPF_NOPROGRESS && flag != TBPF_INDETERMINATE {
                    (vtbl.set_progress_value)(obj, hwnd, completed, 100);
                }
            }
            c.applied.insert(hwnd, encoded);
        });
    }

    /// Drop the cache entry for a closing window's `hwnd`.
    pub(crate) fn forget(hwnd: isize) {
        CACHE.with(|c| {
            c.borrow_mut().applied.remove(&hwnd);
        });
        crate::jumplist_recent::forget_window(hwnd);
    }

    #[cfg(test)]
    mod tests;
}

/// Win32 window-placement read (`GetWindowPlacement`, user32) for maximized-state persistence.
#[cfg(windows)]
pub(crate) mod placement {
    #[repr(C)]
    struct Point {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct Rect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct WindowPlacement {
        length: u32,
        flags: u32,
        show_cmd: u32,
        min_position: Point,
        max_position: Point,
        normal_position: Rect,
    }

    const SW_SHOWMINIMIZED: u32 = 2;
    const SW_SHOWMAXIMIZED: u32 = 3;
    const WPF_RESTORETOMAXIMIZED: u32 = 0x2;
    const SPI_GETWORKAREA: u32 = 0x0030;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowPlacement(hwnd: isize, placement: *mut WindowPlacement) -> i32;
        fn SystemParametersInfoW(
            action: u32,
            ui_param: u32,
            pv_param: *mut std::ffi::c_void,
            fw_ini: u32,
        ) -> i32;
    }

    /// `(maximized, normal-rect top-left in screen physical px)` for `window`.
    pub(crate) fn read(window: &aterm_window::window::Window) -> Option<(bool, (i32, i32))> {
        let hwnd = crate::platform_win::hwnd_of(window)?;
        // SAFETY: out-params are stack locals of the documented layouts, `length`
        // pre-set as `GetWindowPlacement` requires; no pointer outlives the call.
        unsafe {
            let mut wp = WindowPlacement {
                length: std::mem::size_of::<WindowPlacement>() as u32,
                flags: 0,
                show_cmd: 0,
                min_position: Point { x: 0, y: 0 },
                max_position: Point { x: 0, y: 0 },
                normal_position: Rect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
            };
            if GetWindowPlacement(hwnd, &mut wp) == 0 {
                return None;
            }
            let maximized = wp.show_cmd == SW_SHOWMAXIMIZED
                || (wp.show_cmd == SW_SHOWMINIMIZED && wp.flags & WPF_RESTORETOMAXIMIZED != 0);
            let mut work = Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            if SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                (&raw mut work).cast::<std::ffi::c_void>(),
                0,
            ) == 0
            {
                work.left = 0;
                work.top = 0;
            }
            Some((
                maximized,
                (
                    wp.normal_position.left + work.left,
                    wp.normal_position.top + work.top,
                ),
            ))
        }
    }
}

impl App {
    fn attach_font_px(&self, scale: f64) -> f32 {
        match self.font_size_from_device {
            Some((px, _)) => px,
            None => crate::font_px_for_scale(self.font_size, scale),
        }
    }
}

/// The font size (px at 1×) a device px drew at on a display of `scale`.
pub(crate) fn font_size_of_device_px(px: f32, scale: f64) -> f32 {
    let size = if scale.is_finite() && scale > 0.0 {
        px / scale as f32
    } else {
        px
    };
    if size.is_finite() {
        size.clamp(FONT_PX_MIN, FONT_PX_MAX)
    } else {
        FONT_PX
    }
}

#[cfg(test)]
#[path = "app_window_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_window_close_confirm_tests.rs"]
mod close_confirm_tests;

/// The navigator column's width in cells where it is shown.
pub(crate) const DOCK_CELLS: u16 = 30;

/// The columns a terminal keeps where a dock goes inside a window by default (DK-4): a window
/// that cannot keep them beside the column starts without it. A person's own Show is held only
/// to the window's least size (THE EXPLICIT-SHOW FLOOR, [`App::set_dock_at`]).
pub(crate) const DOCK_KEEPS_COLS: u16 = 80;

/// What a window's birth knows of where its dock would go: whether a saved layout row brought
/// it back, whether that row held it to its screen (maximized or full screen, so it cannot be
/// born wider), the origin the row saved, and the scale that origin's px were read at.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct DockBirth {
    pub(crate) restored: bool,
    pub(crate) held: bool,
    pub(crate) origin: Option<(i32, i32)>,
    pub(crate) scale: Option<f64>,
}

impl DockBirth {
    /// The birth a restored layout row `wl` gives its window.
    pub(crate) fn of_row(wl: &crate::restore::WindowLayout) -> Self {
        Self {
            restored: true,
            held: wl.maximized == Some(true) || wl.show.fullscreen == Some(true),
            origin: wl.outer_x.zip(wl.outer_y),
            scale: wl.scale.filter(|s| s.is_finite() && *s > 0.0),
        }
    }
}

/// What a window takes from the layout row it is born from, before its first attach
/// ([`App::take_dock_record`]): the row's dock record, whether the default wrote it, and its
/// birth.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct DockRow {
    /// `None` never decided, `Some(0)` hidden, `Some(k)` shown at `k` cells ([`Dock::pref`]).
    pub(crate) pref: Option<u16>,
    /// The shown record was the default's ([`Dock::by_default`]).
    pub(crate) by_default: bool,
    pub(crate) birth: DockBirth,
}

impl DockRow {
    /// THE RECORD A RESTORED ROW GIVES ITS WINDOW: `wl`'s dock record and who wrote it, and
    /// the birth it gives. Only a shown record is the default's: a row that says
    /// `dock_by_default` beside no shown dock (hand-edited) is read as its record alone.
    pub(crate) fn of(wl: &crate::restore::WindowLayout) -> Self {
        Self {
            pref: wl.dock,
            by_default: wl.dock_by_default && wl.dock.is_some_and(|k| k != 0),
            birth: DockBirth::of_row(wl),
        }
    }
}

/// A default docking a window could not be born with, owed inside its frame: after its restore
/// re-grid settles, or due at the next park.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Owed {
    #[default]
    None,
    AfterRestore(u16),
    Due(u16),
}

/// How a window is born with its dock ([`birth_dock`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BirthDock {
    /// No dock.
    Hidden,
    /// Born with the dock `k` cells wide beside its grid: the window is that much wider.
    Shown(u16),
    /// Born without it; the dock goes inside the frame once due, where the terminal keeps
    /// [`DOCK_KEEPS_COLS`] beside it.
    Inside(u16),
}

/// THE DOCK BIRTH RULE. `pref` is the window's record (`None` never decided, `Some(0)` its person
/// hid it, `Some(k)` shown, born at [`DOCK_CELLS`] whatever `k`), `by_default` whether a shown
/// record was the default's rather than its person's Show ([`Dock::by_default`]), `on` whether
/// windows dock at birth ([`docks_by_default`]), `held` whether the window is held to its screen,
/// `room` whether its screen holds it wider. The setting decides what the default decides: an
/// undecided window, and one the default docked. A person's Show or Hide is theirs, whatever the
/// setting says.
#[must_use]
pub(crate) const fn birth_dock(
    pref: Option<u16>,
    by_default: bool,
    on: bool,
    held: bool,
    room: bool,
) -> BirthDock {
    match pref {
        Some(0) => BirthDock::Hidden,
        // The record says shown; this build's dock is `DOCK_CELLS` wide, whatever width a row
        // carries (another build's, a corrupt or hand-edited file), so none is born a column
        // wider than its screen.
        Some(_) if !by_default => BirthDock::Shown(DOCK_CELLS),
        _ if !on => BirthDock::Hidden,
        Some(_) => BirthDock::Shown(DOCK_CELLS),
        None if held || !room => BirthDock::Inside(DOCK_CELLS),
        None => BirthDock::Shown(DOCK_CELLS),
    }
}

/// Whether windows are born with the dock: `[navigator] dock` where set (`setting`), else on
/// outside a scratch world and CI ([`aterm_types::world::MARKER`], `CI`, as `set` says). Never
/// headless.
#[must_use]
pub(crate) fn docks_by_default(
    setting: Option<bool>,
    headless: bool,
    set: &dyn Fn(&str) -> bool,
) -> bool {
    !headless && setting.unwrap_or_else(|| !set(aterm_types::world::MARKER) && !set("CI"))
}

/// The x a window born with a default dock is placed at: its saved `x`, less what the dock
/// pushed past the right edge of its screen (`screen` is the screen's left edge and width).
/// `outer_w` is the docked frame's width and `dock_w` the dock's, all in physical px. A frame
/// that already crossed the edge without the dock is not moved, and none goes left of the
/// screen.
#[must_use]
pub(crate) fn docked_origin_x(x: i32, outer_w: u32, dock_w: u32, screen: (i32, u32)) -> i32 {
    let right = i64::from(screen.0) + i64::from(screen.1);
    let docked_right = i64::from(x) + i64::from(outer_w);
    let undocked_right = docked_right - i64::from(dock_w);
    if undocked_right > right || docked_right <= right {
        return x;
    }
    let slid = (i64::from(x) - (docked_right - right)).max(i64::from(screen.0));
    i32::try_from(slid).unwrap_or(x)
}

/// How a window system lays its displays out, as it reports a display's rectangle and a
/// window's origin in physical px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DesktopPlane {
    /// One plane in points: a display's px are its points times its own scale, and a window's
    /// its points times the window's (macOS: `CGDisplayBounds`, `-[NSWindow frame]`). On
    /// displays of different scale the px rectangles overlap, so only points say which display
    /// a place is on.
    Points,
    /// One plane in physical px, shared by every display and window (Windows' per-monitor
    /// aware virtual screen, X11).
    Pixels,
}

impl DesktopPlane {
    /// The plane this build's window system lays its displays on.
    pub(crate) const NATIVE: Self = if cfg!(target_os = "macos") {
        Self::Points
    } else {
        Self::Pixels
    };

    /// `px`, read at scale `from`, as px at scale `to`: on a plane of points, through the
    /// point it stands for; on a plane of px, as it is.
    fn rescale(self, px: f64, from: f64, to: f64) -> f64 {
        match self {
            Self::Points => px / from.max(f64::EPSILON) * to,
            Self::Pixels => px,
        }
    }

    /// An origin `(x, y)` in px read at scale `scale`, as the place a window is put at: on a
    /// plane of points, the point it stands for, which the window system places whatever scale
    /// the window has when it is moved (a restored macOS window is born on the main display, at
    /// that display's scale, before it is moved); on a plane of px, the px.
    pub(crate) fn place(self, (x, y): (i32, i32), scale: f64) -> Position {
        match self {
            Self::Points => {
                let scale = scale.max(f64::EPSILON);
                LogicalPosition::new(f64::from(x) / scale, f64::from(y) / scale).into()
            }
            Self::Pixels => PhysicalPosition::new(x, y).into(),
        }
    }
}

/// The monitor of `monitors` whose rectangle holds `origin`, a window's px read at scale
/// `scale`, judged on `plane` in one unit: each monitor's rectangle at its own scale and the
/// origin at its own, so on displays of different scale it is the monitor the window is on.
fn monitor_holding(
    plane: DesktopPlane,
    monitors: &[MonitorRectPx],
    origin: (i32, i32),
    scale: f64,
) -> Option<MonitorRectPx> {
    let unit = |px: f64, scale: f64| plane.rescale(px, scale, 1.0);
    let (x, y) = (
        unit(f64::from(origin.0), scale),
        unit(f64::from(origin.1), scale),
    );
    monitors
        .iter()
        .find(|m| {
            let (left, top) = (unit(m.x, m.scale), unit(m.y, m.scale));
            let (w, h) = (unit(m.w, m.scale), unit(m.h, m.scale));
            x >= left && x < left + w && y >= top && y < top + h
        })
        .copied()
}

/// How near a frame's edge may lie to its screen's visible edge and still meet it, in points:
/// the gap macOS leaves round a tiled window under "Tiled windows have margins", with room for
/// the px rounding of a saved frame.
const TILE_EDGE_PTS: f64 = 16.0;

/// THE PLACED-FRAME RULE. macOS tells aterm nothing of a tile, so a frame (`frame`, its outer
/// rectangle in points, top-left origin) is judged placed rather than floating by where it
/// stands in its screen's visible frame (`visible`, less the menu bar and the Dock): its left
/// or right edge meets the visible frame's, and its top and bottom meet a tile row's (the
/// visible frame's top and bottom, a half or the whole screen; or its top or bottom and its
/// middle, a quarter), each within [`TILE_EDGE_PTS`]. A placed window is held as a maximized one
/// is: a default dock goes inside it and never grows it over its neighbour.
#[must_use]
pub(crate) fn frame_is_placed(frame: WorkAreaPts, visible: WorkAreaPts) -> bool {
    let finite = [
        frame.x, frame.y, frame.w, frame.h, visible.x, visible.y, visible.w, visible.h,
    ]
    .into_iter()
    .all(f64::is_finite);
    if !finite || frame.w <= 0.0 || frame.h <= 0.0 || visible.w <= 0.0 || visible.h <= 0.0 {
        return false;
    }
    let meets = |a: f64, b: f64| (a - b).abs() <= TILE_EDGE_PTS;
    let (top, bottom) = (visible.y, visible.y + visible.h);
    let middle = visible.y + visible.h / 2.0;
    let (frame_top, frame_bottom) = (frame.y, frame.y + frame.h);
    let side = meets(frame.x, visible.x) || meets(frame.x + frame.w, visible.x + visible.w);
    let row = (meets(frame_top, top) && meets(frame_bottom, bottom))
        || (meets(frame_top, top) && meets(frame_bottom, middle))
        || (meets(frame_top, middle) && meets(frame_bottom, bottom));
    side && row
}

/// What a default dock's birth fit reads of the window's screens, all read by the caller
/// ([`App::fit_birth_dock_on`]), so the judgment needs no OS window.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BirthScreens<'a> {
    pub(crate) plane: DesktopPlane,
    pub(crate) monitors: &'a [MonitorRectPx],
    /// Each screen's visible frame in points; empty where the window system names none, and
    /// then no saved frame is judged to stand in a tile ([`App::fit_birth_dock_on`]).
    pub(crate) visible: &'a [WorkAreaPts],
    /// The visible frame of the screen the window is on now, in points: the area judged where
    /// the saved origin lies on no screen.
    pub(crate) current: Option<WorkAreaPts>,
    /// The window's scale now.
    pub(crate) scale: f64,
}

/// Every live monitor of `window`'s display set, in physical px.
fn monitors_px(window: &Window) -> Vec<MonitorRectPx> {
    window
        .available_monitors()
        .map(|m| {
            let (pos, size) = (m.position(), m.size());
            MonitorRectPx {
                x: f64::from(pos.x),
                y: f64::from(pos.y),
                w: f64::from(size.width),
                h: f64::from(size.height),
                scale: m.scale_factor(),
            }
        })
        .collect()
}

/// The window's navigator host. Programs keep the committed geometry while a fall waits for
/// a client with area or a person's Hide waits for the chrome regrow floor.
#[derive(Default)]
pub(crate) struct Dock {
    pub(crate) cells: u16,
    /// The process roster has claimed this window's dock as the session list.
    pub(crate) roster_panel: bool,
    /// A larger client may now admit the preferred panel width.
    pub(crate) retry_roster_on_resize: bool,
    pub(crate) view: Option<crate::tab_model::ViewId>,
    pub(crate) preview: Option<crate::app_navigator::SessionPreview>,
    pub(crate) hidden_order: Vec<u64>,
    pub(crate) session_click: Option<(u64, std::time::Instant, (f64, f64))>,
    pub(crate) session_drag: Option<(u64, (f64, f64))>,
    /// Pending width and whether this came from an explicit Show or Hide.
    fall: Option<(u16, bool)>,
    /// Geometry of the last successful present, fencing hits while a new placement is owed.
    pub(crate) presented: Option<crate::app_native::NativeHostGeometry>,
    /// The window's dock record (`WindowLayout::dock`): `None` never decided, `Some(0)` hidden,
    /// `Some(k)` requests a width of `k` cells. The roster keeps this intent while available space suppresses the panel in this window.
    pub(crate) pref: Option<u16>,
    /// The shown record was written by the default (a birth docked by it, or its docking paid
    /// inside), not by a person's Show (`WindowLayout::dock_by_default`): `[navigator] dock`
    /// decides it at the next birth as it decides an undecided window ([`birth_dock`]). A dock
    /// act clears it; it goes with the record to a new window. Only beside a shown record.
    pub(crate) by_default: bool,
    /// What the window's birth knows of its place ([`DockBirth`]).
    pub(crate) birth: DockBirth,
    /// A default docking the window could not be born with ([`Owed`]).
    owed: Owed,
    /// The dock was seated at birth by the default, no record saying so: its frame may still be
    /// fitted to its screen ([`App::fit_birth_dock_to`]) and its origin slid
    /// ([`App::restored_origin`]).
    pub(crate) born_default: bool,
    /// The cells of a shown dock a metrics change set down because the client no longer held
    /// the explicit-Show floor beside it at the new cells ([`App::fit_dock_to_metrics`]): a
    /// later metrics change that finds room raises it again. A dock act clears it.
    pub(crate) set_down: Option<u16>,
}

impl Dock {
    pub(crate) fn wanted_cells(&self) -> u16 {
        self.fall.map_or(self.cells, |(cells, _)| cells)
    }

    fn preferred_roster_width(&self, default_on: bool) -> u16 {
        match self.pref {
            Some(width) if !self.by_default => width,
            _ if !default_on => 0,
            width => width.unwrap_or(DOCK_CELLS),
        }
    }

    /// A roster capture keeps the saved width across available space changes. A legacy host
    /// captures a shown width only if its current geometry agrees, so the next birth can fit
    /// a default dock rather than assuming a previously refused width will fit.
    pub(crate) fn written(&self) -> Option<u16> {
        self.pref
            .filter(|&k| self.roster_panel || k == 0 || k == self.cells)
    }

    /// Whether the record a capture writes ([`Self::written`], or the record as it came where
    /// `as_came`, a headless app's) is a shown one the default wrote.
    pub(crate) fn written_by_default(&self, as_came: bool) -> bool {
        let written = if as_came { self.pref } else { self.written() };
        self.by_default && written.is_some_and(|k| k != 0)
    }

    /// Take `row`'s record and birth: whatever the window inherited, since a row with no record
    /// is one never decided.
    pub(crate) const fn take_row(&mut self, row: DockRow) {
        self.pref = row.pref;
        self.by_default = row.by_default;
        self.birth = row.birth;
    }

    /// THE LAUNCH'S FIRST WINDOW takes the record and birth of `manifest`'s first row, at
    /// `carried_origin` (a handoff's carried place) where one came
    /// ([`crate::restore::bootstrap_dock`]): the launch's own write, which it makes before the
    /// window attaches and the DK-4 exit test drives.
    pub(crate) fn take_bootstrap(
        &mut self,
        manifest: Option<&crate::restore::RestoreManifest>,
        carried_origin: Option<(i32, i32)>,
    ) {
        self.take_row(crate::restore::bootstrap_dock(manifest, carried_origin));
    }

    /// The window's restore re-grid settled: a default docking owed after it is due now. This
    /// only marks it; the park pays it outside every resize entry point
    /// ([`App::settle_dock_fall`]), so none of it is booked `site=restore`.
    pub(crate) const fn restore_settled(&mut self) {
        if let Owed::AfterRestore(k) = self.owed {
            self.owed = Owed::Due(k);
        }
    }

    /// Whether a default docking is owed to this window, after its restore or now.
    #[cfg(test)]
    pub(crate) const fn owes_birth_dock(&self) -> bool {
        !matches!(self.owed, Owed::None)
    }
}

/// Why a dock act committed nothing ([`App::set_dock_at`]). The words are the refusal the act's
/// own surface reports: the menu act's and `open app navigator dock`'s reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DockRefusal {
    /// No such window stands.
    NoWindow,
    /// The window's client has no area now (a minimized Windows window reads 0x0), so no grid
    /// could be judged or kept.
    NoClient,
    /// The window holds `has` columns and the rise needs `needs`: its least terminal grid
    /// ([`App::least_window_size`]) beside the column.
    TooNarrow { has: u16, needs: u16 },
    /// The navigator's view could not be made.
    View(String),
}

impl std::fmt::Display for DockRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWindow => f.write_str("navigator not shown: no such window"),
            Self::NoClient => {
                f.write_str("navigator not shown: the window has no area now (it is minimized)")
            }
            Self::TooNarrow { has, needs } => write!(
                f,
                "navigator not shown: the window is {has} columns wide and needs {needs}"
            ),
            Self::View(why) => write!(f, "navigator not shown: {why}"),
        }
    }
}

impl App {
    /// Commit chrome inside the held window, booking every actual program resize once.
    pub(crate) fn set_dock(&mut self, wid: WindowId, cells: u16) -> Result<(), DockRefusal> {
        let now = std::time::Instant::now();
        self.claim_roster_dock(wid)?;
        self.refresh_local_panel_roster();
        self.set_dock_at(wid, cells, now)
    }

    /// View > Show Navigator's act on the front window: Show where the navigator is hidden or
    /// its Hide is waiting, else Hide. A refused Show (no room, no client) leaves its words in
    /// the act's refusal (`pending_action_refusal`), which `invoke` replies with.
    pub(crate) fn toggle_navigator_dock(&mut self) {
        let Some(wid) = self.frontmost_window else {
            return;
        };
        let Some(wanted) = self.windows.get(&wid).map(|ws| ws.dock.wanted_cells()) else {
            return;
        };
        let cells = if wanted == 0 { DOCK_CELLS } else { 0 };
        if let Err(refusal) = self.set_dock(wid, cells) {
            self.pending_action_refusal = Some(refusal.to_string());
        }
    }

    /// The refusal of a rise to `cells` in window `wid` holding `held`: THE EXPLICIT-SHOW FLOOR.
    /// A Show is held to the window's own least size ([`Self::least_window_size_with_inset`]):
    /// the terminal keeps at least the grid the smallest window has (164 points on the cell
    /// lattice, about 20 columns at the default font; a window holding Settings its least
    /// view), so no Show shrinks its programs to a column. Nothing grows the window to make
    /// room: the commit is inside the client the person holds.
    fn dock_rise_refusal(
        &self,
        wid: WindowId,
        cells: u16,
        held: PhysicalSize<u32>,
    ) -> Option<DockRefusal> {
        let needs = self
            .least_window_size_with_inset(wid, self.side_inset_px_of(wid, cells))
            .width;
        if held.width >= needs {
            return None;
        }
        let (cw, pad) = (self.win_cell_size(wid).0, self.win_pad(wid));
        let cols = |px: u32| {
            u16::try_from(aterm_render::pad_split(px as usize, pad, cw).cells).unwrap_or(u16::MAX)
        };
        Some(DockRefusal::TooNarrow {
            has: cols(held.width),
            needs: cols(needs),
        })
    }

    /// Whether window `wid` holds a client with area: a dock change is judged and booked
    /// against that client, and a minimized Windows window has none.
    fn dock_client_stands(&self, wid: WindowId) -> bool {
        self.held_inner_px(wid)
            .is_some_and(|held| held.width != 0 && held.height != 0)
    }

    /// Window `wid`'s dock is `cells` wide from `now`: committed, already so, or (a fall inside
    /// the regrow floor or in a window whose client has no area) waiting for
    /// [`Self::settle_dock_fall`]. `Err` committed and booked nothing.
    pub(crate) fn set_dock_at(
        &mut self,
        wid: WindowId,
        cells: u16,
        now: std::time::Instant,
    ) -> Result<(), DockRefusal> {
        self.apply_dock_at(wid, cells, now, true)
    }

    /// Reconcile the session panel's width while preserving a person's explicit choice.
    /// A default rise inside the held client leaves the terminal its preferred 80 columns.
    pub(crate) fn set_roster_dock_at(
        &mut self,
        wid: WindowId,
        cells: u16,
        now: std::time::Instant,
    ) -> Result<(), DockRefusal> {
        self.claim_roster_dock(wid)?;
        let dock = &self.windows[&wid].dock;
        let explicit = dock.pref.is_some() && !dock.by_default;
        if cells > dock.cells
            && !explicit
            && let Some(held) = self.held_inner_px(wid)
            && held.width != 0
            && held.height != 0
        {
            let has = self.grid_dims_for(wid, held).1.saturating_add(dock.cells);
            let needs = DOCK_KEEPS_COLS.saturating_add(cells);
            if has.saturating_sub(cells) < DOCK_KEEPS_COLS {
                return Err(DockRefusal::TooNarrow { has, needs });
            }
        }
        self.apply_dock_at(wid, cells, now, false)
    }

    fn claim_roster_dock(&mut self, wid: WindowId) -> Result<(), DockRefusal> {
        let reorient = {
            let Some(mut ws) = self.windows.get_mut(&wid) else {
                return Err(DockRefusal::NoWindow);
            };
            let reorient = !ws.dock.roster_panel && ws.dock.cells != 0;
            ws.dock.roster_panel = true;
            if reorient {
                ws.dock.presented = None;
                ws.dock_card = None;
                ws.invalidate_raster_caches();
                ws.request_redraw();
            }
            reorient
        };
        if reorient {
            self.refresh_local_panel_tab_chrome(wid);
            self.navigator_mark_structure();
            self.refresh_pointer_after_frame_placement(wid);
        }
        Ok(())
    }

    fn apply_dock_at(
        &mut self,
        wid: WindowId,
        cells: u16,
        now: std::time::Instant,
        remember: bool,
    ) -> Result<(), DockRefusal> {
        let (old, needs_view) = {
            let Some(mut ws) = self.windows.get_mut(&wid) else {
                return Err(DockRefusal::NoWindow);
            };
            ws.dock.fall = None;
            // A dock act supersedes a default docking still owed, and a dock a metrics change
            // set down.
            ws.dock.owed = Owed::None;
            ws.dock.set_down = None;
            let old = ws.dock.cells;
            if old == cells {
                if remember {
                    ws.dock.pref = Some(cells);
                    ws.dock.by_default = false;
                }
                return Ok(());
            }
            if remember
                && cells < old
                && !aterm_messages::chrome::fall_allowed(ws.regrow_floor.floor(), now)
            {
                ws.dock.fall = Some((cells, remember));
                return Ok(());
            }
            (old, cells != 0 && ws.dock.view.is_none())
        };
        let Some(held) = self.held_inner_px(wid) else {
            return Err(DockRefusal::NoWindow);
        };
        if held.width == 0 || held.height == 0 {
            // No client, no grid to book against: committing here would re-grid every program
            // to 1x1 under `site=dock` and again under `site=window` when the window came
            // back. A fall stays owed and is paid, once and under `site=dock`, when the client
            // has area again; a rise is refused, as one with no room is.
            if cells > old {
                return Err(DockRefusal::NoClient);
            }
            if let Some(mut ws) = self.windows.get_mut(&wid) {
                ws.dock.fall = Some((cells, remember));
            }
            return Ok(());
        }
        if cells > old
            && let Some(refusal) = self.dock_rise_refusal(wid, cells, held)
        {
            return Err(refusal);
        }
        let new_view = if needs_view {
            match self.create_dock_navigator(wid) {
                Ok(view) => Some(view),
                Err(error) => {
                    aterm_log::info!("navigator dock: {error}");
                    return Err(DockRefusal::View(error));
                }
            }
        } else {
            None
        };
        self.settle_native_pointer_press(wid);
        {
            let Some(mut ws) = self.windows.get_mut(&wid) else {
                return Err(DockRefusal::NoWindow);
            };
            if let Some(view) = new_view {
                ws.dock.view = Some(view);
            }
            ws.dock.cells = cells;
            // A person's choice commits with the grid. Automatic roster suppression keeps
            // the width the window will use when it is eligible again.
            if remember {
                ws.dock.pref = Some(cells);
                ws.dock.by_default = false;
            }
            if cells > old {
                ws.regrow_floor.rose(now);
            }
            ws.dock_card = None;
            ws.dock.presented = None;
            ws.invalidate_raster_caches();
            // The commit is inside the frame the initial-frame settle holds: the same pixels,
            // whose grid is now the attach grid less the columns the inset took. Left at the
            // attach grid, the settle reads the dock's own re-grid as a mis-sized first frame
            // and asks for a window wider by the inset, re-gridding every program again under
            // `site=window`. Rebased, it still corrects a late configure to the attach frame.
            #[cfg(any(target_os = "linux", test))]
            if let Some(settle) = ws.initial_frame_settle.as_mut() {
                match settle
                    .cols
                    .checked_add(old)
                    .and_then(|cols| cols.checked_sub(cells))
                {
                    Some(cols) if cols != 0 => settle.cols = cols,
                    _ => ws.initial_frame_settle = None,
                }
            }
        }
        {
            let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Dock);
            self.on_resize(wid, held);
        }
        self.refresh_pointer_after_frame_placement(wid);
        if (old == 0 || cells == 0)
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.dock.roster_panel)
        {
            self.refresh_local_panel_tab_chrome(wid);
        }
        self.navigator_mark_structure();
        self.push_least_window_size_inside(wid);
        if let Some(ws) = self.windows.get(&wid) {
            ws.request_redraw();
        }
        Ok(())
    }

    /// Hold window `wid`'s dock to THE EXPLICIT-SHOW FLOOR at metrics that just changed (a font,
    /// line-height or padding change, [`Self::rebuild_backend`]), inside the client `held` it
    /// keeps, before that change's one re-grid: the dock is a whole number of cells, so it grows
    /// with the cells, and a zoom would otherwise take the terminal beside it down to a column.
    /// A shown dock the floor no longer admits is set down, its record kept and the cells kept
    /// to raise ([`Dock::set_down`]); a set-down dock the floor admits again is raised. Either
    /// moves only the inset: the caller's re-grid books each program's one resize under the
    /// change's own site. A Hide still waiting for the regrow floor commits here, the inset
    /// gone already. Whether the inset moved.
    pub(crate) fn fit_dock_to_metrics(&mut self, wid: WindowId, held: PhysicalSize<u32>) -> bool {
        if held.width == 0 || held.height == 0 {
            return false;
        }
        let Some((cells, wanted, remember_hide, set_down, has_view)) =
            self.windows.get(&wid).map(|ws| {
                (
                    ws.dock.cells,
                    ws.dock.wanted_cells(),
                    ws.dock.fall == Some((0, true)),
                    ws.dock.set_down,
                    ws.dock.view.is_some(),
                )
            })
        else {
            return false;
        };
        if cells == 0 && set_down.is_some() && self.windows[&wid].dock.roster_panel {
            if self.windows[&wid].dock.preferred_roster_width(
                self.tab_home_window() == Some(wid) && self.docks_at_birth(),
            ) == 0
            {
                return false;
            }
        }
        let raise = match (cells, set_down) {
            (0, Some(k)) if self.dock_rise_refusal(wid, k, held).is_none() => k,
            (0, _) => return false,
            (cells, _) if self.dock_rise_refusal(wid, cells, held).is_none() => return false,
            _ => 0,
        };
        let new_view = if raise != 0 && !has_view {
            match self.create_dock_navigator(wid) {
                Ok(view) => Some(view),
                Err(error) => {
                    aterm_log::info!("navigator dock: not raised after a metrics change: {error}");
                    return false;
                }
            }
        } else {
            None
        };
        if raise == 0 {
            self.settle_native_pointer_press(wid);
        }
        let Some(mut route_guard_ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        let ws = &mut **route_guard_ws;
        if let Some(view) = new_view {
            ws.dock.view = Some(view);
        }
        ws.dock.cells = raise;
        ws.dock.fall = None;
        ws.dock.set_down = if raise == 0 && wanted != 0 {
            Some(cells)
        } else {
            None
        };
        if raise == 0 && remember_hide {
            ws.dock.pref = Some(0);
            ws.dock.by_default = false;
        }
        ws.dock_card = None;
        ws.dock.presented = None;
        ws.invalidate_raster_caches();
        drop(route_guard_ws);
        aterm_log::info!(
            "navigator dock: window {} {} after a metrics change",
            wid.0,
            if raise == 0 { "set down" } else { "raised" }
        );
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.dock.roster_panel)
        {
            self.refresh_local_panel_tab_chrome(wid);
        }
        self.navigator_mark_structure();
        true
    }

    /// A native tab strip advertises tab-position shortcuts only while the roster is hidden.
    fn refresh_local_panel_tab_chrome(&mut self, wid: WindowId) {
        if self._toolbars.contains_key(&wid) {
            self.refresh_tab_chrome_windows(vec![wid]);
        }
    }

    /// Pay pending falls from the loop's existing deadline fold; no periodic dock wake exists.
    /// A fall in a window whose client has no area waits for the client, not the clock.
    ///
    /// It also pays a default docking due inside a window ([`BirthDock::Inside`]): where the
    /// terminal keeps [`DOCK_KEEPS_COLS`] beside the column it commits it as a Show does, one
    /// resize per program under `site=dock`; else the window stays undocked and undecided. A
    /// restore still draining pays none: its final re-grid would plan around the commit.
    ///
    /// Nothing is paid inside another resize entry point: the outermost scope names the site
    /// (`SiteScope::enter`), and the settle is reached through `sync_messages` too, not only
    /// from the park, so a pay there would be booked under the caller's cause.
    pub(crate) fn settle_dock_fall(&mut self, now: std::time::Instant) -> bool {
        if crate::resize_ledger::SiteScope::current().is_some() {
            return false;
        }
        self.settle_dock_fall_unscoped(now)
    }

    /// [`Self::settle_dock_fall`]'s pay, whatever resize entry point the caller is inside.
    pub(crate) fn settle_dock_fall_unscoped(&mut self, now: std::time::Instant) -> bool {
        let births = self.settle_birth_docks(now);
        let ready: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&wid, ws)| {
                ws.dock
                    .fall
                    .filter(|(_, remember)| {
                        !remember
                            || aterm_messages::chrome::fall_allowed(ws.regrow_floor.floor(), now)
                    })
                    .map(|cells| (wid, cells))
            })
            .filter(|&(wid, _)| self.dock_client_stands(wid))
            .collect();
        let moved = !ready.is_empty();
        for (wid, (cells, remember)) in ready {
            if let Err(refusal) = self.apply_dock_at(wid, cells, now, remember) {
                aterm_log::info!("navigator dock: waiting hide: {refusal}");
            }
        }
        moved || births
    }

    /// Pay every default docking due ([`Owed::Due`]) in a window whose client has area, once no
    /// restore drains. Whether any window's dock changed.
    fn settle_birth_docks(&mut self, now: std::time::Instant) -> bool {
        if self.restore_drain.is_some() {
            return false;
        }
        let due: Vec<(WindowId, u16)> = self
            .windows
            .iter()
            .filter_map(|(&wid, ws)| match ws.dock.owed {
                Owed::Due(cells) => Some((wid, cells)),
                Owed::None | Owed::AfterRestore(_) => None,
            })
            .filter(|&(wid, _)| self.dock_client_stands(wid))
            .collect();
        let mut moved = false;
        for (wid, cells) in due {
            let roster = self.windows[&wid].dock.roster_panel;
            if roster {
                if self.windows[&wid].dock.preferred_roster_width(
                    self.tab_home_window() == Some(wid) && self.docks_at_birth(),
                ) == 0
                {
                    if let Some(mut ws) = self.windows.get_mut(&wid) {
                        ws.dock.owed = Owed::None;
                    }
                    continue;
                }
            }
            if roster {
                if self.set_roster_dock_at(wid, cells, now).is_ok() {
                    moved = true;
                } else if let Some(mut ws) = self.windows.get_mut(&wid) {
                    ws.dock.owed = Owed::None;
                    ws.dock.retry_roster_on_resize = true;
                }
            } else if self.windows[&wid].cols >= DOCK_KEEPS_COLS.saturating_add(cells) {
                match self.set_dock_at(wid, cells, now) {
                    Ok(()) => {
                        moved = true;
                        // The default docked it, not its person: the setting still decides it.
                        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                            let ws = &mut **route_guard_ws;
                            ws.dock.by_default = ws.dock.pref.is_some_and(|k| k != 0);
                        }
                    }
                    Err(refusal) => aterm_log::info!("navigator dock: default inside: {refusal}"),
                }
            } else if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.dock.owed = Owed::None;
            }
        }
        moved
    }

    /// When a waiting fall may commit. A fall waiting for its window's client names no time:
    /// the event that brings the client back runs the loop, and no deadline spins on it.
    pub(crate) fn dock_fall_due(&self) -> Option<std::time::Instant> {
        self.windows
            .iter()
            .filter(|(_, ws)| ws.dock.fall.is_some())
            .filter(|&(&wid, _)| self.dock_client_stands(wid))
            .filter_map(|(_, ws)| ws.regrow_floor.floor())
            .min()
    }

    pub(crate) fn dock_view(&self, wid: WindowId) -> Option<crate::tab_model::ViewId> {
        let ws = self.windows.get(&wid)?;
        (ws.dock.cells != 0).then_some(ws.dock.view).flatten()
    }

    /// Window `wid` takes the dock record and birth of its layout row ([`Dock::take_row`]).
    pub(crate) fn take_dock_record(&mut self, wid: WindowId, row: DockRow) {
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.dock.take_row(row);
        }
    }

    /// Whether a window born now is born with its dock ([`docks_by_default`]): the live
    /// `[navigator] dock`, so a config reload governs the windows born after it.
    pub(crate) fn docks_at_birth(&self) -> bool {
        docks_by_default(
            self.config.navigator_dock(),
            self.headless,
            &crate::converge_lane::env_set,
        )
    }

    /// The roster is process-wide, while visibility and preferred width belong to each window.
    fn seat_local_panel_at_birth(&mut self, wid: WindowId) -> bool {
        if self.headless {
            return false;
        }
        let Some(mut ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        ws.dock.roster_panel = true;
        drop(ws);
        self.navigator_mark_structure();
        self.refresh_local_panel_roster();
        self.local_panel_roster.request_layout();
        self.seat_birth_dock(wid)
    }

    pub(crate) fn reconcile_local_panel_layout(&mut self, now: std::time::Instant) {
        self.refresh_local_panel_roster();
        if !self.local_panel_roster.take_layout_request() {
            return;
        }
        let home = self.tab_home_window();
        let default_on = self.docks_at_birth();
        let changes: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&wid, ws)| {
                let dock = &ws.dock;
                if !dock.roster_panel {
                    return None;
                }
                // A person's pending Hide keeps its usual debounce.
                if dock.fall.is_some_and(|(_, remember)| remember) {
                    return None;
                }
                let width = dock.preferred_roster_width(default_on && home == Some(wid));
                if width == 0 {
                    return (dock.cells != 0
                        || dock.owed != Owed::None
                        || dock.set_down.is_some()
                        || dock.retry_roster_on_resize)
                        .then_some((wid, 0));
                }
                // A held restore pays its inset after the carried grid settles. A font change
                // that put the dock down likewise owns reseating it at metrics it can fit.
                if dock.owed != Owed::None || dock.set_down.is_some() {
                    return None;
                }
                (width != dock.wanted_cells()).then_some((wid, width))
            })
            .collect();
        for (wid, width) in changes {
            if let Some(mut ws) = self.windows.get_mut(&wid) {
                ws.dock.retry_roster_on_resize = false;
            }
            if let Err(refusal) = self.set_roster_dock_at(wid, width, now) {
                match refusal {
                    DockRefusal::TooNarrow { .. } | DockRefusal::NoClient => {
                        if let Some(mut ws) = self.windows.get_mut(&wid) {
                            ws.dock.retry_roster_on_resize = true;
                        }
                    }
                    _ => aterm_log::info!("local session panel: {refusal}"),
                }
            }
        }
    }

    /// Seat window `wid`'s dock as its birth rule says ([`birth_dock`]), before its first
    /// attach sizes it: shown, the window is born that much wider at the grid it carries, so no
    /// program sees a resize; owed inside, it is paid once due ([`Self::settle_dock_fall`]).
    /// Only a window with no OS window and no dock is seated. Whether a dock view was made
    /// ([`Self::unseat_birth_dock`] undoes it).
    pub(crate) fn seat_birth_dock(&mut self, wid: WindowId) -> bool {
        // A headless app is born with no dock and keeps its rows' records as they came.
        if self.headless {
            return false;
        }
        let Some((pref, by_default, birth)) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.os_window.is_none() && ws.dock.cells == 0 && ws.dock.view.is_none())
            .map(|ws| (ws.dock.pref, ws.dock.by_default, ws.dock.birth))
        else {
            return false;
        };
        // A restored auxiliary window honors explicit Show and Hide and retires
        // a default that previously opened its navigator.
        let home = self.tab_home_window() == Some(wid);
        if !home && !birth.restored {
            return false;
        }
        let on = home && self.docks_at_birth();
        let placement = birth_dock(pref, by_default, on, birth.held, true);
        let placement = match (placement, pref) {
            // The carried frame already includes a custom width. Judge it inside that
            // frame, including records from a display with more room than this one.
            (BirthDock::Shown(_), Some(width))
                if self.windows[&wid].dock.roster_panel && width != DOCK_CELLS =>
            {
                BirthDock::Inside(width)
            }
            (placement, _) => placement,
        };
        match placement {
            BirthDock::Hidden => {
                // The setting hid a shown record the default wrote: the window lives undocked,
                // so its record is undecided again. Kept, a capture would write "shown" beside
                // an undocked grid and frame;
                // a birth after the setting comes back on then decides it as a default one (the
                // screen fit, the slide, a held window's one `site=dock` pay). The width is
                // always `DOCK_CELLS`, so nothing is lost.
                if pref.is_some_and(|k| k != 0)
                    && let Some(mut route_guard_ws) = self.windows.get_mut(&wid)
                {
                    let ws = &mut **route_guard_ws;
                    ws.dock.pref = None;
                    ws.dock.by_default = false;
                }
                false
            }
            BirthDock::Shown(cells) => match self.create_dock_navigator(wid) {
                Ok(view) => {
                    if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                        let ws = &mut **route_guard_ws;
                        ws.dock.view = Some(view);
                        ws.dock.cells = cells;
                        ws.dock.born_default = pref.is_none();
                        // An undecided window the default docks keeps a record that says so.
                        ws.dock.by_default = by_default || pref.is_none();
                        ws.dock.pref = Some(cells);
                        ws.dock_card = None;
                        ws.dock.presented = None;
                        ws.invalidate_raster_caches();
                    }
                    self.navigator_mark_structure();
                    true
                }
                Err(error) => {
                    aterm_log::info!("navigator dock: not seated at birth: {error}");
                    false
                }
            },
            BirthDock::Inside(cells) => {
                if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                    let ws = &mut **route_guard_ws;
                    ws.dock.owed = if birth.restored {
                        Owed::AfterRestore(cells)
                    } else {
                        Owed::Due(cells)
                    };
                }
                false
            }
        }
    }

    /// Undo [`Self::seat_birth_dock`]'s view in window `wid`, which no OS window holds yet: its
    /// attach failed, or its screen cannot hold it wider.
    pub(crate) fn unseat_birth_dock(&mut self, wid: WindowId) {
        let view = self.windows.get_mut(&wid).and_then(|mut route_guard_ws| {
            let ws = &mut **route_guard_ws;
            ws.dock.cells = 0;
            ws.dock.born_default = false;
            ws.dock_card = None;
            ws.dock.presented = None;
            ws.invalidate_raster_caches();
            ws.dock.view.take()
        });
        if let Some(view) = view {
            self.remove_view_link(view);
            self.navigator_mark_structure();
        }
    }

    /// Fit a dock window `wid` was born with by default to its screen: where the docked frame
    /// (`docked_w_pts`, its outer width in points) is wider than the screen's area
    /// (`area_w_pts`), or the window stood placed in a tile of its screen (`placed`,
    /// [`frame_is_placed`]), the window is born without it and owes it inside, as a held window
    /// does. A dock its record asked for is kept. Whether it was unseated.
    pub(crate) fn fit_birth_dock_to(
        &mut self,
        wid: WindowId,
        docked_w_pts: f64,
        area_w_pts: Option<f64>,
        placed: bool,
    ) -> bool {
        let Some(restored) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.dock.born_default && ws.os_window.is_none())
            .map(|ws| ws.dock.birth.restored)
        else {
            return false;
        };
        if !placed && area_w_pts.is_none_or(|area_w_pts| docked_w_pts <= area_w_pts) {
            return false;
        }
        // Before its attach, a restored window's restore has still to close.
        self.owe_birth_dock_inside(wid, restored);
        true
    }

    /// [`Self::fit_birth_dock_to`] window `wid` among `screens`, for a docked frame
    /// `docked_w_pts` wide and the frame its row saved, `saved_px` (its outer size in px at the
    /// row's scale), where it saved one. The screen's area is that of the screen its saved
    /// origin lies on ([`monitor_holding`]), else of the screen the window is on now; the saved
    /// frame is placed where it stands in a tile of the visible frame its origin lies in, among
    /// the screens' visible frames the window system names ([`frame_is_placed`],
    /// [`BirthScreens::visible`]). Only macOS names them; elsewhere no frame is judged placed
    /// here: the screen the window is on now is no stand-in, since a Windows window moves
    /// between its early reveal's fit and its attach's, and the two would judge different
    /// screens, and a Linux tiler's answer is the initial-frame settle's
    /// (`judge_birth_dock_frame`). Whether the dock was unseated.
    pub(crate) fn fit_birth_dock_on(
        &mut self,
        wid: WindowId,
        screens: &BirthScreens<'_>,
        docked_w_pts: f64,
        saved_px: Option<(f64, f64)>,
    ) -> bool {
        let Some((origin, origin_scale)) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.dock.born_default)
            .map(|ws| {
                let scale = ws.dock.birth.scale.unwrap_or(screens.scale);
                (ws.dock.birth.origin, scale.max(f64::EPSILON))
            })
        else {
            return false;
        };
        let area_w_pts = origin
            .and_then(|origin| {
                monitor_holding(screens.plane, screens.monitors, origin, origin_scale)
            })
            .map(|m| m.w / m.scale.max(f64::EPSILON))
            .or_else(|| screens.current.map(|area| area.w));
        let placed = origin.zip(saved_px).is_some_and(|((x, y), (w, h))| {
            let frame = WorkAreaPts {
                x: f64::from(x) / origin_scale,
                y: f64::from(y) / origin_scale,
                w: w / origin_scale,
                h: h / origin_scale,
            };
            let holds = |v: &WorkAreaPts| {
                frame.x >= v.x && frame.x < v.x + v.w && frame.y >= v.y && frame.y < v.y + v.h
            };
            screens
                .visible
                .iter()
                .copied()
                .find(holds)
                .is_some_and(|visible| frame_is_placed(frame, visible))
        });
        let unseated = self.fit_birth_dock_to(wid, docked_w_pts, area_w_pts, placed);
        if unseated {
            if placed {
                aterm_log::info!(
                    "navigator dock: window {} starts without its dock: its saved frame stands \
                     in a tile of its screen",
                    wid.0
                );
            } else {
                aterm_log::info!(
                    "navigator dock: window {} starts without its dock: {docked_w_pts:.0} pt \
                     docked is wider than its screen's {:.0} pt",
                    wid.0,
                    area_w_pts.unwrap_or_default()
                );
            }
        }
        unseated
    }

    /// [`Self::fit_birth_dock_on`] window `wid`, about to attach in `window` at a docked inner
    /// frame `frame_w` px wide at display `scale`, its row's saved inner frame `saved` (the
    /// carried frame) where it has one: the screens, their visible frames and the border are
    /// read off the OS window here. The Windows early reveal fits it at its cached cell metrics,
    /// before it sizes, places and shows the window, so the window is revealed at the frame and
    /// origin it keeps; the attach fits it again at the joined renderer's, a no-op where the
    /// reveal already decided.
    fn fit_birth_dock_at_attach(
        &mut self,
        wid: WindowId,
        window: &Window,
        (frame_w, saved): (u32, Option<PhysicalSize<u32>>),
        scale: f64,
    ) {
        if !self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.dock.born_default)
        {
            return;
        }
        let (outer, inner) = (window.outer_size(), window.inner_size());
        let border = (
            outer.width.saturating_sub(inner.width),
            outer.height.saturating_sub(inner.height),
        );
        let docked_w_pts = f64::from(frame_w.saturating_add(border.0)) / scale.max(f64::EPSILON);
        let saved_px = saved.map(|size| {
            (
                f64::from(size.width.saturating_add(border.0)),
                f64::from(size.height.saturating_add(border.1)),
            )
        });
        let (monitors, visible) = (
            monitors_px(window),
            self.apprt.screens_work_area_pts(window),
        );
        let screens = BirthScreens {
            plane: DesktopPlane::NATIVE,
            monitors: &monitors,
            visible: &visible,
            current: self.work_area_pts(window),
            scale: window.scale_factor(),
        };
        self.fit_birth_dock_on(wid, &screens, docked_w_pts, saved_px);
    }

    /// Where window `wid`, attached in `window`, is placed for its saved origin `(x, y)`, px its
    /// row read at the row's scale: born with a default dock, slid left by what the dock pushed
    /// past the right edge of the screen that origin lies on ([`docked_origin_x`]); else at
    /// `(x, y)`. Either is the place the window system puts the window at
    /// ([`DesktopPlane::place`]): on macOS the point the row's px stand for, whatever display the
    /// window was born on.
    pub(crate) fn restored_origin(
        &self,
        wid: WindowId,
        window: &Window,
        x: i32,
        y: i32,
    ) -> Position {
        let window_scale = window.scale_factor();
        if !self.born_with_default_dock(wid) {
            return DesktopPlane::NATIVE.place((x, y), self.row_scale(wid, window_scale));
        }
        self.restored_origin_among(
            wid,
            DesktopPlane::NATIVE,
            &monitors_px(window),
            (window.outer_size().width, window_scale),
            (x, y),
            self.win_side_inset_px(wid),
        )
    }

    /// Whether window `wid` stands with a dock its birth seated by default
    /// ([`Dock::born_default`]).
    fn born_with_default_dock(&self, wid: WindowId) -> bool {
        self.windows
            .get(&wid)
            .is_some_and(|ws| ws.dock.born_default && ws.dock.cells != 0)
    }

    /// The scale window `wid`'s layout row read its px at ([`DockBirth::scale`]), else the
    /// window's, `window_scale`: a row from before the field, or a window no row brought back.
    fn row_scale(&self, wid: WindowId, window_scale: f64) -> f64 {
        self.windows
            .get(&wid)
            .and_then(|ws| ws.dock.birth.scale)
            .unwrap_or(window_scale)
    }

    /// [`Self::restored_origin`] among `monitors` laid on `plane`, for a frame `outer_w` px
    /// wide at the window's scale `window_scale` whose dock is `dock_w` px at that scale, all
    /// read by the caller: no renderer metric is read here, so the Windows early reveal, which
    /// runs before the backend joins (a `Pending` slot panics on any metric read), passes the
    /// dock's width at the cached cell metrics it sized its frame by. The saved origin's
    /// screen is judged at the scale the row read it at (else the window's,
    /// [`monitor_holding`]), and the slide is made in that row's px; the place returned is the
    /// point those px stand for on a plane of points, so a window born at another display's
    /// scale lands where the slide was judged ([`DesktopPlane::place`]).
    pub(crate) fn restored_origin_among(
        &self,
        wid: WindowId,
        plane: DesktopPlane,
        monitors: &[MonitorRectPx],
        (outer_w, window_scale): (u32, f64),
        (x, y): (i32, i32),
        dock_w: usize,
    ) -> Position {
        let row_scale = self.row_scale(wid, window_scale);
        let x = if self.born_with_default_dock(wid)
            && let Some(screen) = monitor_holding(plane, monitors, (x, y), row_scale)
        {
            let at_row = |px: f64, scale: f64| plane.rescale(px, scale, row_scale).round();
            docked_origin_x(
                x,
                at_row(f64::from(outer_w), window_scale) as u32,
                at_row(dock_w as f64, window_scale) as u32,
                (
                    at_row(screen.x, screen.scale) as i32,
                    at_row(screen.w, screen.scale) as u32,
                ),
            )
        } else {
            x
        };
        plane.place((x, y), row_scale)
    }

    /// THE OS'S ANSWER TO A DEFAULT DOCK (Linux): window `wid`, born wider by a dock its birth
    /// seated by default, is about to apply `size`, which its initial-frame `settle` (asking for
    /// the docked grid) lets through: expired, granted after a correction, or a vetoed size's
    /// fallback ([`Self::flush_pending_resize`]). Where the settle corrected at least once and
    /// `size` still holds fewer columns than the docked grid, the OS refused the wider frame (a
    /// tiler sizes a window to its tile, whatever it asked): the dock is unseated before the
    /// size is applied, so the tile grids the carried columns and no program sees a resize, and
    /// the dock is owed inside, paid as one `site=dock` resize where the terminal keeps
    /// [`DOCK_KEEPS_COLS`] beside it, as for a window that cannot be born wider. Otherwise the
    /// dock stands, judged for good: a size a person makes later never unseats it. An uncorrected
    /// settle proves no refusal, since an X11 window's first read echoes the size it asked for.
    /// Whether the dock was unseated.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn judge_birth_dock_frame(
        &mut self,
        wid: WindowId,
        size: PhysicalSize<u32>,
        settle: crate::InitialFrameSettle,
    ) -> bool {
        if !self.born_with_default_dock(wid) {
            return false;
        }
        if !settle.corrected || self.grid_dims_for(wid, size).1 >= settle.cols {
            if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
                let ws = &mut **route_guard_ws;
                ws.dock.born_default = false;
            }
            return false;
        }
        let after_restore = self.windows.get(&wid).is_some_and(|ws| {
            ws.dock.birth.restored
                && (self.restore_pass_queued() || ws.restore_regrid_owed.is_some())
        });
        self.owe_birth_dock_inside(wid, after_restore);
        aterm_log::info!(
            "navigator dock: window {} starts without its dock: the OS gave {}x{}, not the \
             docked frame of {} columns",
            wid.0,
            size.width,
            size.height,
            settle.cols
        );
        true
    }

    /// Window `wid`'s default dock is unseated and owed inside its frame: once its restore
    /// re-grid settles (`after_restore`), else at the next park. Its record stays undecided.
    fn owe_birth_dock_inside(&mut self, wid: WindowId, after_restore: bool) {
        let Some(cells) = self.windows.get(&wid).map(|ws| ws.dock.cells) else {
            return;
        };
        self.unseat_birth_dock(wid);
        if let Some(mut route_guard_ws) = self.windows.get_mut(&wid) {
            let ws = &mut **route_guard_ws;
            ws.dock.pref = None;
            ws.dock.by_default = false;
            ws.dock.owed = if after_restore {
                Owed::AfterRestore(cells)
            } else {
                Owed::Due(cells)
            };
        }
    }

    /// Diagnostic: window `wid`, born with a default dock at grid `grid`, attached at `raw_px`.
    /// Where the OS did not give the docked frame (a clamp, a tiler), say so once; on Linux the
    /// initial-frame settle judges its answer ([`Self::judge_birth_dock_frame`]).
    fn note_birth_dock_frame(&self, wid: WindowId, raw_px: PhysicalSize<u32>, grid: (u16, u16)) {
        if self
            .windows
            .get(&wid)
            .is_some_and(|ws| ws.dock.born_default)
            && self.grid_dims_for(wid, raw_px) != grid
        {
            aterm_log::warn!(
                "navigator dock: window {} was born docked at {}x{} but the OS gave {}x{}",
                wid.0,
                grid.1,
                grid.0,
                raw_px.width,
                raw_px.height
            );
        }
    }

    /// What the dock of window `wid` puts on glass: its card's serial and where it lies.
    pub(crate) fn dock_shape(&self, wid: WindowId) -> Option<crate::DockShape> {
        dock_shape_of(
            self.windows.get(&wid)?.dock_card.as_ref(),
            self.dock_geometry(wid),
        )
    }

    pub(crate) fn dock_geometry(
        &self,
        wid: WindowId,
    ) -> Option<crate::app_native::NativeHostGeometry> {
        self.dock_view(wid)?;
        let ws = self.windows.get(&wid)?;
        let (cw, ch) = self.win_cell_size(wid);
        let width = u32::try_from(usize::from(ws.dock.cells).checked_mul(cw)?).ok()?;
        let height = u32::try_from(
            usize::from(ws.rows)
                .checked_mul(ch)?
                .checked_add(self.win_pad(wid))?,
        )
        .ok()?;
        let (frame_x, frame_y) = self.frame_origin(wid);
        let x = frame_x.saturating_sub(i64::from(width)).min(0);
        let origin = (
            x,
            frame_y.checked_add(i64::try_from(self.native_content_origin_y(wid)).ok()?)?,
        );
        let scale = ws.scale.max(f64::EPSILON) as f32;
        Some(crate::app_native::NativeHostGeometry {
            origin,
            size: (width, height),
            viewport: crate::native_ui::LogicalRect::new(
                0.0,
                0.0,
                width as f32 / scale,
                height as f32 / scale,
            ),
        })
    }

    /// A geometry-only fixture for DK-1: production inset changes enter `set_dock`.
    #[cfg(test)]
    pub(crate) fn set_side_inset_for_test(&mut self, wid: WindowId, cells: u16) {
        if let Some(mut ws) = self.windows.get_mut(&wid) {
            ws.dock.cells = cells;
            ws.invalidate_raster_caches();
            ws.request_redraw();
        }
        self.refresh_pointer_after_frame_placement(wid);
        self.push_least_window_size(wid);
    }
}

/// What a dock `card` held at `host` puts on glass ([`crate::DockShape`]): the one rule the
/// unchanged-frame key and the CPU surface's retained copy compare.
pub(crate) fn dock_shape_of(
    card: Option<&crate::SettingsCard>,
    host: Option<crate::app_native::NativeHostGeometry>,
) -> Option<crate::DockShape> {
    let (card, host) = card.zip(host)?;
    Some((card.serial, host.origin, host.size))
}

#[cfg(test)]
#[path = "app_window_side_inset_tests.rs"]
mod side_inset_tests;

#[cfg(test)]
#[path = "app_window_dock_tests.rs"]
mod dock_tests;

#[cfg(test)]
#[path = "app_window_dock_birth_tests.rs"]
mod dock_birth_tests;
