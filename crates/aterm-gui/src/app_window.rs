// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window lifecycle (`create`/`attach`/`close`/`focus`, `front`/`front_mut`, and `apply_title`)
//! routed through the platform [`crate::platform::AppRt`] seam.

use std::sync::Arc;
use std::time::Instant;

use aterm_window::dpi::{LogicalPosition, PhysicalSize};
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

/// A screen's usable placement rectangle in logical points, top-left origin, y.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkAreaPts {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

const CASCADE_STEP_PTS: f64 = 26.0;

const CASCADE_MIN_VISIBLE_PTS: f64 = 160.0;

#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(2000);
#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_MAX_CORRECTIONS: u8 = 3;

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
    let monitors: Vec<MonitorRectPx> = window
        .available_monitors()
        .map(|m| {
            let pos = m.position();
            let size = m.size();
            MonitorRectPx {
                x: f64::from(pos.x),
                y: f64::from(pos.y),
                w: f64::from(size.width),
                h: f64::from(size.height),
                scale: m.scale_factor(),
            }
        })
        .collect();
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
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        ws.restore_placement = Some(placement);
        true
    }
}

impl crate::WindowState {
    /// Take a restored window's carried frame before its first attach sizes it.
    pub(crate) fn take_carried_frame(&mut self, inner_px: Option<PhysicalSize<u32>>) {
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
fn present_target_install_outcome(
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

    /// The frontmost logical window's state.
    pub(crate) fn front(&self) -> Option<&WindowState> {
        self.frontmost_window
            .and_then(|id| self.windows.get(&id))
            .map(Box::as_ref)
    }

    /// The window-chrome theme from config `window_theme`.
    pub(crate) fn window_theme_for_chrome(&self) -> crate::app_config::WindowTheme {
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

    fn early_reveal_backdrop(&self) -> u32 {
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

    /// The theme the window's chrome painters (tab band strip tones, native pages' role palette).
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

    pub(crate) fn front_mut(&mut self) -> Option<&mut WindowState> {
        self.frontmost_window
            .and_then(move |id| self.windows.get_mut(&id))
            .map(Box::as_mut)
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

    /// Install an already-spawned `session` as the sole tab of a fresh window `wid` and make it.
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

    /// `install_window_state` with an explicit registry lineage parent for the window's first.
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
        if session.master >= 0 {
            Self::register_session(&self.store, &session, lineage_parent);
        }
        let layout = pane::PaneTree::new(sid);
        let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
            .expect("tab/view identity space");
        self.pool_insert(session);
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
        self.windows.insert(wid, Box::new(ws));
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

    /// Full window creation IN the turn, the restore's (one window per restore slice).
    #[allow(
        clippy::too_many_arguments,
        reason = "the window's birth facts — cwd, the seamless adoptee, the connection \
                  lineage and an update's carried frame and place — \
                  are each a per-open datum, as in `create_window_logical`"
    )]
    pub(crate) fn create_window_internal_connected(
        &mut self,
        el: &ActiveEventLoop,
        cwd_override: Option<&str>,
        adopt: Option<crate::spawn::Adopted>,
        lineage_parent: Option<aterm_session::SessionId>,
        observe: Option<&aterm_session::SessionId>,
        carried: Option<CarriedFrame>,
        placement: ((Option<i32>, Option<i32>), Option<bool>),
    ) -> Option<WindowId> {
        let ((rows, cols), carried_inner_px) = self.new_window_frame(carried);
        let wid =
            self.create_window_logical(rows, cols, cwd_override, adopt, lineage_parent, observe)?;
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.take_carried_frame(carried_inner_px);
        }
        #[cfg(any(windows, target_os = "linux"))]
        self.stage_restore_placement(wid, placement.0, placement.1);
        #[cfg(not(any(windows, target_os = "linux")))]
        let _ = placement;
        if !self.headless && !self.attach_os_window(el, wid) {
            self.close_window_logical(wid);
            return None;
        }
        self.sync_active_session();
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

    /// The size window `wid` is attached.
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
        let (cell_w, cell_h) = self.win_cell_size(wid);
        let remainder = |got: u32, frame: u32, cell: usize| {
            got.checked_sub(frame)
                .is_some_and(|extra| (extra as usize) < cell)
        };
        if remainder(carried.width, exact.width, cell_w)
            && remainder(carried.height, exact.height, cell_h)
            && self.grid_dims_for(wid, carried) == (rows, cols)
        {
            return carried;
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

    /// Window `wid`'s head at its attach for a `head_pts` titlebar band at `scale`.
    pub(crate) fn attach_deck_head(
        &mut self,
        wid: WindowId,
        head_pts: f64,
        scale: f64,
        (rows, cols): (u16, u16),
        carried: Option<PhysicalSize<u32>>,
    ) -> usize {
        use crate::stats_plan::DeckForm;
        let frame_under = |app: &mut Self, form: DeckForm| {
            if let Some(ws) = app.windows.get_mut(&wid) {
                ws.deck_form = form;
            }
            let head = app.deck_head_px(wid, head_pts, scale, 0);
            app.backend.set_head(head);
            app.window_frame_px_for(wid, rows, cols)
        };
        let frame_h = match carried {
            None => frame_under(self, DeckForm::Folded).height,
            Some(carried) => {
                let (_, cell_h) = self.win_cell_size(wid);
                let full = frame_under(self, DeckForm::Full);
                let was_full = carried
                    .height
                    .checked_sub(full.height)
                    .is_some_and(|extra| (extra as usize) < cell_h);
                if !was_full {
                    frame_under(self, DeckForm::Folded);
                }
                carried.height
            }
        };
        let head = self.deck_head_px(wid, head_pts, scale, frame_h);
        self.backend.set_head(head);
        head
    }

    /// Create the OS window and present surface for logical window `wid` and attach them.
    #[must_use]
    pub(crate) fn attach_os_window(&mut self, el: &ActiveEventLoop, wid: WindowId) -> bool {
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        let pending_join = self.backend.is_pending();
        let startup_attach_entry = Instant::now();
        let mut size = if pending_join {
            let (cw, ch) = seed_cell_px(self.font_px);
            let pad = self.cfg_pad_for_scale(1.0);
            let vertical_pad = pad.saturating_add(self.cfg_pad_top_for_scale(1.0));
            let total_rows = rows.saturating_add(self.chrome_rows(wid));
            PhysicalSize::new(
                (cols as usize * cw + pad.saturating_mul(2)) as u32,
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
        let attrs = if reveal_after_chrome || defer_reveal {
            attrs.with_visible(false)
        } else {
            attrs
        };
        let startup_before_window_create = Instant::now();
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                crate::logging::stderr_line!("aterm-gui: OS window creation failed: {e}");
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
        }
        if self._status_item.is_none()
            && let Some(proxy) = self.proxy.clone()
        {
            self.install_status_item(&proxy);
        }
        if self._reduce_motion.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            self.system_reduce_motion = self.apprt.reduce_motion();
            crate::native_appearance::install_preferences(
                self.apprt.native_appearance_preferences(),
            );
            self._reduce_motion = self.apprt.observe_reduce_motion(proxy);
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
            let bootstrap_restore = self
                .pending_restore
                .as_ref()
                .and_then(|m| m.windows.first());
            let restore_maximized = bootstrap_restore.and_then(|wl| wl.maximized) == Some(true);
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
                    PhysicalSize::new(width as u32, height as u32)
                };
                let bars = self.stats_bars_form();
                let (folded, full) = (
                    crate::stats_plan::DeckForm::Folded,
                    crate::stats_plan::DeckForm::Full,
                );
                let head_in = |form| band + crate::stats_deck::deck_px(bars, form, scale, cell_h);
                let carried_h = self
                    .windows
                    .get(&wid)
                    .and_then(|ws| ws.carried_inner_px)
                    .map(|c| c.height);
                let (prev, frame_h) = match carried_h {
                    Some(h) => {
                        let was_full = h
                            .checked_sub(frame(head_in(full)).height)
                            .is_some_and(|extra| (extra as usize) < cell_h);
                        (if was_full { full } else { folded }, h)
                    }
                    None => (folded, frame(head_in(folded)).height),
                };
                let avail = crate::stats_plan::avail_pt(f64::from(frame_h), band as f64, scale);
                let deck = crate::stats_plan::deck_form_for(avail, prev, self.deck_size_pref());
                let exact = frame(head_in(deck));
                let _ = window.request_inner_size(exact);
                if let Some((x, y)) =
                    bootstrap_restore.and_then(|wl| Some((wl.outer_x?, wl.outer_y?)))
                    && restored_position_on_screen(&window, x, y)
                {
                    window.set_outer_position(aterm_window::dpi::PhysicalPosition::new(x, y));
                }
                let backdrop = self.early_reveal_backdrop();
                #[cfg(windows)]
                crate::platform_win::note_caption_surface(&window, self.caption_surface_tone(wid));
                self.apprt.window_set_background_color(&window, backdrop);
                self.apprt
                    .window_set_appearance(&window, self.window_theme_for_chrome());
                window.set_visible(true);
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
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.head_pts = head_pts;
            ws.last_windowed_band_pts = band_memory_pts;
        }
        let scale = resolve_force_scale().unwrap_or_else(|| window.scale_factor());
        if let Some(ws) = self.windows.get_mut(&wid) {
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
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.head_pts = synthetic as f64 / if scale > 0.0 { scale } else { 1.0 };
            }
        }
        let head_pts = self.windows.get(&wid).map_or(0.0, |ws| ws.head_pts);
        let carried = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.carried_inner_px.take());
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.metrics = crate::MetricsView::applied(self.font_px, pad, pad_top, ws.metrics.head);
        }
        let head = self.attach_deck_head(wid, head_pts, scale, (rows, cols), carried);
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.metrics = crate::MetricsView::applied(self.font_px, pad, pad_top, head);
        }
        size = self.attach_frame_px(wid, rows, cols, carried);
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
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.initial_frame_settle = Some(crate::InitialFrameSettle {
                rows,
                cols,
                deadline: Instant::now() + INITIAL_FRAME_SETTLE_WINDOW,
                corrections: INITIAL_FRAME_SETTLE_MAX_CORRECTIONS,
                corrected: false,
            });
        }
        {
            let (cw, ch) = self.cell_size();
            window.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
            #[cfg(target_os = "linux")]
            window.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
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
        #[cfg(any(windows, target_os = "linux"))]
        if let Some(placement) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.restore_placement.take())
            && early_reveal_size.is_none()
        {
            if let Some((x, y)) = placement.outer
                && restored_position_on_screen(&window, x, y)
            {
                window.set_outer_position(aterm_window::dpi::PhysicalPosition::new(x, y));
            }
            #[cfg(windows)]
            if placement.maximized {
                window.set_maximized(true);
            }
        }
        if reveal_after_chrome {
            // reveal site (carried pixels first, never a bg-only frame).
            if !defer_reveal && early_reveal_size.is_none() {
                window.set_visible(true);
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
                    let present_installed = if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.os_window = Some(window);
                        ws.pending_reveal = defer_reveal
                            .then(|| Instant::now() + std::time::Duration::from_millis(1500));
                        ws.win_px = Some(raw_px);
                        #[cfg(a11y_tree)]
                        {
                            ws.a11y = a11y_adapter;
                        }
                        ws.capture_acquire_armed = false;
                        ws.gpu_acquire_wait = crate::GpuAcquireWait::default();
                        ws.present = Some(PresentTarget::Gpu {
                            gpu_surface: surf,
                            window_gpu,
                        });
                        true
                    } else {
                        false
                    };
                    if present_installed {
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
                        drop(window);
                        return false;
                    }
                    crate::logging::stderr_line!(
                        "aterm-gui: GPU surface creation failed: {e}; falling back to the CPU renderer"
                    );
                    aterm_log::warn!(
                        "GPU surface creation failed: {e}; falling back to the CPU renderer"
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
                drop(window);
                return false;
            }
        };
        let startup_after_surface_create = Instant::now();
        self.winit_to_window.insert(window.id(), wid);
        let present_installed = if let Some(ws) = self.windows.get_mut(&wid) {
            ws.os_window = Some(window);
            ws.pending_reveal =
                defer_reveal.then(|| Instant::now() + std::time::Duration::from_millis(1500));
            ws.win_px = Some(raw_px);
            #[cfg(a11y_tree)]
            {
                ws.a11y = a11y_adapter;
            }
            ws.present = Some(PresentTarget::Cpu { surface });
            true
        } else {
            false
        };
        if present_installed {
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
        if let Some(focused) = os_focus
            && self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.focused != focused)
        {
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
        if Instant::now() > settle.deadline || settle.corrections == 0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.initial_frame_settle = None;
            }
            return false;
        }
        self.apply_window_scale(wid);
        let intended = self.window_frame_px_for(wid, settle.rows, settle.cols);
        let grid_now = self.grid_dims_for(wid, size);
        if grid_now == (settle.rows, settle.cols) {
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
                && let Some(ws) = self.windows.get_mut(&wid)
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
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        if let Some(s) = ws.initial_frame_settle.as_mut() {
            s.corrections -= 1;
            s.corrected = true;
        }
        let os_window = ws.os_window.clone();
        let granted = os_window
            .as_ref()
            .and_then(|w| w.request_inner_size(intended));
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
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(applied) = granted {
            ws.pending_resize = None;
            ws.next_resize_settle = None;
            ws.win_px = Some(applied);
            self.sync_surface_to_window(wid);
            self.on_resize(wid, applied);
        } else {
            let fallback = (Instant::now() + Self::SETTLE_VETO_FALLBACK).min(settle_deadline);
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
            // Distinct branch.
            return CloseOutcome::Stay; // stale/unknown id → no-op
        }
        #[cfg(target_os = "macos")]
        self.withdraw_askpass_window(wid);
        #[cfg(target_os = "macos")]
        self.withdraw_secrets_sheet_window(wid);
        let deferred_attribution = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.pending_close_attribution.take());
        let _deferred_close = deferred_attribution
            .map(|(reason, actor)| crate::session_store::CloseAttribution::enter(reason, actor));
        let _ = self.video_abort_window_close(wid);
        if self.exits_app_when_closing(wid) {
            self.retire_spare("closed", None);
        } else {
            self.spare_poke();
        }
        let Some(ws) = self.windows.get(&wid) else {
            return CloseOutcome::Stay;
        };
        let exits_app = self.exits_app_when_closing(wid);
        if self.config.restore_session_or_default() && exits_app {
            self.quit_capture = Some(self.capture_restore_manifest());
        }
        let ids = self.window_terminal_sessions(wid);
        let view_ids: Vec<crate::tab_model::ViewId> = ws
            .tab_set
            .tabs()
            .iter()
            .flat_map(|tab| tab.root.leaves())
            .collect();
        let plan = crate::session_log::ClosedKeep::plan(&self.pool.drops_of(&ids));
        for (id, planned) in ids.into_iter().zip(plan) {
            if self.detach_session_view(id) {
                let keep = (!exits_app)
                    .then(|| planned.unwrap_or(crate::session_log::ClosedKeep::Skeleton));
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
        self.windows.remove(&wid);
        self.find_origins.retain(|(_, entry), _| *entry != wid);
        self._toolbars.remove(&wid);
        #[cfg(target_os = "macos")]
        if self.paste_confirm.as_ref().is_some_and(|c| c.wid == wid) {
            self.paste_confirm = None;
        }
        if self.paste_banner.as_ref().is_some_and(|p| p.wid == wid) {
            self.paste_banner = None;
        }
        if self.close_banner.as_ref().is_some_and(|p| p.wid == wid) {
            self.close_banner = None;
        }
        self.close_banner_queue.retain(|q| q.wid != wid);
        self.ask_next_queued_close();
        self.winit_to_window.retain(|_, &mut v| v != wid);
        self.focus_order.retain(|w| *w != wid);
        if self.pending_deminiaturize_focus == Some(wid) {
            self.pending_deminiaturize_focus = None;
        }
        if self.frontmost_window == Some(wid) {
            self.frontmost_window = self.next_frontmost_after_close();
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
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::mutation(
            crate::DeferredHandoffMutation::CloseWindow(wid),
        )) {
            return;
        }
        match self.prepare_window_native_shutdown(wid, crate::native_app::CloseScope::Window) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                aterm_log::warn!("window native close barrier: {error}");
                return;
            }
        }
        match self.prepare_window_document_shutdown(wid) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                aterm_log::warn!("window document close barrier: {error}");
                return;
            }
        }
        if matches!(self.close_window_logical(wid), CloseOutcome::Exit) {
            el.exit();
        }
    }

    /// Re-read the frame window `wid` must draw itself ([`crate::client_frame`]) from its OS window
    #[cfg(target_os = "linux")]
    pub(crate) fn sync_client_frame(&mut self, wid: WindowId) -> bool {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
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

    /// Re-resolve the strip's rows ([`Self::strip_rows_for`] of the live config) after window.
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
            if let Some(ws) = self.windows.get_mut(&wid) {
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
            if let Some(ws) = self.windows.get_mut(&w) {
                ws.pending_close = false;
            }
            self.close_window(el, w);
        }
    }

    /// Whether session `id` is running a foreground job (not the idle shell).
    pub(crate) fn session_foreground_busy(&self, id: u64) -> bool {
        self.pool.get(id).is_some_and(Self::session_busy)
    }

    fn session_busy(s: &crate::Session) -> bool {
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
                    // refusal is never reported against a later `tab` or `invoke`.
                    self.pending_action_refusal = Some(WIRE_CLOSE_REFUSED.to_string());
                }
                return !busy;
            }
            CloseConfirm::Interactive => {}
        }
        let Some(prompt) = self.destructive_prompt(wid, exits_app, busy, replay) else {
            // is busy; Windows: also a single idle tab): the close needs no confirm.
            return true;
        };
        if cfg!(windows) {
            return self.present_close_banner(wid, prompt, replay);
        }
        if let Some(proceed) = self
            .apprt
            .confirm(prompt.title, prompt.body, prompt.proceed)
        {
            return proceed;
        }
        if !busy {
            return true;
        }
        self.present_close_banner(wid, prompt, replay)
    }

    fn destructive_prompt(
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
            None => prompt,
        }
    }

    /// Park a destructive question on the in-window banner over `wid` and refuse the gesture.
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
        self.pending_action_refusal = Some(pending.wire_refusal());
        self.stash_deferred_close_attribution(wid);
        self.close_banner = Some(pending);
        self.raise_close_question(wid, by_person);
        false
    }

    fn raise_close_question(&mut self, wid: WindowId, focus: bool) {
        let Some((w, focused)) = self
            .windows
            .get(&wid)
            .and_then(|ws| Some((ws.os_window.clone()?, ws.focused)))
        else {
            return;
        };
        if w.is_minimized() == Some(true) {
            // asynchronous: the `Occluded(false)` arm finishes the raise.
            if focus {
                self.pending_deminiaturize_focus = Some(wid);
            }
            w.set_minimized(false);
        }
        if focus && !focused {
            w.focus_window();
            w.request_user_attention(Some(aterm_window::window::UserAttentionType::Critical));
        }
        w.request_redraw();
    }

    /// Ask the first question that waited behind the one just answered or dropped.
    pub(crate) fn ask_next_queued_close(&mut self) {
        while self.close_banner.is_none() && !self.close_banner_queue.is_empty() {
            let mut next = self.close_banner_queue.remove(0);
            if !self.windows.contains_key(&next.wid) {
                continue;
            }
            if let crate::close_confirm::CloseReplay::Window(wid) = next.replay {
                let exits_app = self.exits_app_when_closing(wid);
                let busy = self.window_has_foreground_job(wid);
                let asks = self
                    .destructive_prompt(wid, exits_app, busy, next.replay)
                    .filter(|_| cfg!(windows) || busy);
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
            let (wid, focus) = (next.wid, !next.wire_may_answer());
            self.close_banner = Some(next);
            self.raise_close_question(wid, focus);
        }
    }

    /// Answer the in-window close/quit banner.
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
            // will not run; a later close must not inherit this gesture's name.
            if let Some(ws) = self.windows.get_mut(&pending.wid) {
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
                self.close_banner_queue.clear();
                self.on_quit_requested(el);
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
                .and_then(|ws| ws.pending_close_attribution.take()),
            crate::close_confirm::CloseReplay::Window(_)
            | crate::close_confirm::CloseReplay::Quit => None,
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
        }
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
            None if cfg!(not(target_os = "macos")) => {
                Some(crate::close_confirm::CONTROLS_CLOSED.to_string())
            }
            None => None,
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

    fn exits_app_when_closing(&self, wid: WindowId) -> bool {
        !self.windows.keys().any(|w| *w != wid)
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
            Ok(false) => return QuitBarrier::Refused,
            Err(error) => {
                aterm_log::warn!("quit native close barrier: {error}");
                return QuitBarrier::Refused;
            }
        }
        match self.prepare_quit_document_shutdown() {
            Ok(true) => QuitBarrier::Ready,
            Ok(false) if !self.quit_document_shutdown_blocked() => QuitBarrier::Saving,
            Ok(false) => {
                self.cancel_failed_quit_document_shutdown();
                QuitBarrier::Refused
            }
            Err(error) => {
                aterm_log::warn!("quit document close barrier: {error}");
                self.cancel_failed_quit_document_shutdown();
                QuitBarrier::Refused
            }
        }
    }

    /// A Quit whose barriers were ready when it deferred behind an update handoff.
    #[cfg(unix)]
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
        self.on_quit_requested(el);
    }

    /// A headless instance was sent a terminating signal (`owned_endpoint::arm_orderly_quit`).
    #[cfg(unix)]
    pub(crate) fn on_exit_signal(&mut self, el: &ActiveEventLoop, sig: i32) {
        if !self.headless {
            aterm_log::warn!("signal {sig} reached a windowed instance's quit path; ending now");
            crate::owned_endpoint::die_of_quit_signal();
            return;
        }
        self.on_quit_requested(el);
    }

    /// Stateful counterpart of AppKit's bare `applicationShouldTerminate:` callback.
    #[cfg(unix)]
    pub(crate) fn on_native_terminate_requested(&mut self, el: &ActiveEventLoop, generation: u64) {
        if !crate::menu::native_termination_is_current(generation) {
            return;
        }
        if self.defer_pending_update_handoff_teardown(
            crate::DeferredHandoffTeardown::NativeTerminate { generation },
        ) {
            return;
        }
        let anchor = self
            .frontmost_window
            .filter(|window| self.windows.contains_key(window))
            .or_else(|| self.windows.keys().next().copied());
        if let Some(window) = anchor {
            let busy = self.any_foreground_job();
            if !self.confirm_destructive_gesture(
                window,
                true,
                busy,
                crate::close_confirm::CloseReplay::Quit,
            ) {
                let _ = crate::menu::cancel_native_termination(generation);
                return;
            }
        }

        match self.quit_barriers() {
            QuitBarrier::Ready => {}
            QuitBarrier::Saving => return,
            QuitBarrier::Refused => {
                let _ = crate::menu::cancel_native_termination(generation);
                return;
            }
        }
        if !crate::menu::complete_native_termination(generation) {
            return;
        }
        let _ = self.video_abort_app_shutdown();
        el.exit();
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
            let _ = crate::menu::complete_current_native_termination();
            let _ = self.video_abort_app_shutdown();
            el.exit();
            return;
        }
        for window in windows {
            if matches!(self.close_window_logical(window), CloseOutcome::Exit) {
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
        self.publish_active_terminal_title(id, title);
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
            let Some(ws) = self.windows.get_mut(&id) else {
                self.window_title_scratch = scratch;
                return;
            };
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
        let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.clone()) else {
            // Distinct branch.
            return; // headless / not yet attached: nothing to manipulate
        };
        match op {
            W::DeIconify => w.set_minimized(false),
            W::Iconify => w.set_minimized(true),
            W::RaiseWindow => w.focus_window(),
            W::RefreshWindow => w.request_redraw(),
            W::ResizeWindowPixels { height, width } => {
                #[cfg(target_os = "linux")]
                if let Some(ws) = self.windows.get_mut(&wid) {
                    // settle must not re-assert the attach grid over it.
                    ws.initial_frame_settle = None;
                }
                let _ =
                    w.request_inner_size(PhysicalSize::new(u32::from(width), u32::from(height)));
            }
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

    fn publish_active_terminal_title(&mut self, id: WindowId, title: &str) {
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
                if let Some(ws) = self.windows.get_mut(&id) {
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
            // Distinct branch.
            return; // headless, or no OS window yet — nothing to reflect onto
        }
        let progress = self
            .front_terminal(wid)
            .and_then(|terminal| crate::term_lock(&terminal.term).taskbar_progress());
        taskbar::set_progress(hwnd, progress);
        self.sync_taskbar_extras(wid, hwnd);
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
        register_tab: unsafe extern "system" fn(*mut ITaskbarList3, isize, isize) -> i32,
        unregister_tab: unsafe extern "system" fn(*mut ITaskbarList3, isize) -> i32,
        set_tab_order: unsafe extern "system" fn(*mut ITaskbarList3, isize, isize) -> i32,
        set_tab_active: unsafe extern "system" fn(*mut ITaskbarList3, isize, isize, u32) -> i32,
        thumb_bar_add_buttons:
            unsafe extern "system" fn(*mut ITaskbarList3, isize, u32, *const c_void) -> i32,
        thumb_bar_update_buttons:
            unsafe extern "system" fn(*mut ITaskbarList3, isize, u32, *const c_void) -> i32,
        thumb_bar_set_image_list:
            unsafe extern "system" fn(*mut ITaskbarList3, isize, isize) -> i32,
        set_overlay_icon:
            unsafe extern "system" fn(*mut ITaskbarList3, isize, isize, *const u16) -> i32,
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
            // SAFETY: standard COM bring-up.
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
                // Distinct branch.
                return; // unchanged since the last frame — do not touch COM
            }
            let Some(obj) = c.object() else {
                // Distinct branch.
                return; // no taskbar interface available
            };
            // SAFETY: `obj` is a live `ITaskbarList3` created on this thread and `hwnd` a live.
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

    /// Show (`icon` non-zero, with its accessible `description`, NUL-terminated) or clear.
    pub(crate) fn set_overlay_icon(hwnd: isize, icon: isize, description: &[u16]) -> bool {
        CACHE.with(|c| {
            let Some(obj) = c.borrow_mut().object() else {
                return false;
            };
            let description = overlay_description(icon, description);
            // SAFETY: `obj` is a live `ITaskbarList3` created on this thread, `hwnd` a live.
            unsafe { ((*(*obj).vtbl).set_overlay_icon)(obj, hwnd, icon, description) >= 0 }
        })
    }

    fn overlay_description(icon: isize, description: &[u16]) -> *const u16 {
        if icon == 0 || description.last() != Some(&0) {
            std::ptr::null()
        } else {
            description.as_ptr()
        }
    }

    /// Drop the cache entry for a closing window's `hwnd`.
    pub(crate) fn forget(hwnd: isize) {
        crate::taskbar_badge_win::forget(hwnd);
        CACHE.with(|c| {
            c.borrow_mut().applied.remove(&hwnd);
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_show_passes_its_text_and_a_clear_passes_none() {
            let text = crate::win_ffi::wide("A tab rang the bell");
            assert_eq!(overlay_description(0x7010, &text), text.as_ptr());
            assert!(overlay_description(0, &text).is_null(), "a clear");
            assert!(overlay_description(0, &[]).is_null());
            let unterminated: Vec<u16> = "A tab".encode_utf16().collect();
            assert!(overlay_description(0x7010, &unterminated).is_null());
        }

        #[test]
        fn progress_maps_to_the_taskbar_flag_and_value() {
            assert_eq!(encode(None), (TBPF_NOPROGRESS, 0));
            assert_eq!(encode(Some(TaskbarProgress::Hidden)), (TBPF_NOPROGRESS, 0));
            assert_eq!(
                encode(Some(TaskbarProgress::Indeterminate)),
                (TBPF_INDETERMINATE, 0)
            );
            assert_eq!(encode(Some(TaskbarProgress::Normal(42))), (TBPF_NORMAL, 42));
            assert_eq!(encode(Some(TaskbarProgress::Error(7))), (TBPF_ERROR, 7));
            assert_eq!(encode(Some(TaskbarProgress::Paused(99))), (TBPF_PAUSED, 99));
        }

        #[test]
        fn the_vtable_slots_are_the_headers() {
            let slot = |offset: usize| offset / std::mem::size_of::<usize>();
            assert_eq!(
                slot(std::mem::offset_of!(ITaskbarList3Vtbl, set_progress_value)),
                9
            );
            assert_eq!(
                slot(std::mem::offset_of!(ITaskbarList3Vtbl, set_progress_state)),
                10
            );
            assert_eq!(
                slot(std::mem::offset_of!(ITaskbarList3Vtbl, set_overlay_icon)),
                18
            );
        }
    }
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
        use aterm_window::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get(),
            _ => return None,
        };
        // SAFETY: out-params are stack locals of the documented layouts, `length` pre-set.
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
mod tests {
    use super::*;

    use crate::feed_session_for_test as feed;

    #[test]
    fn the_os_caption_drops_a_leading_spinner_frame() {
        assert_eq!(
            os_window_title("\u{25d0} Aterm OSX shortcuts and font — Running claude"),
            "Aterm OSX shortcuts and font — Running claude"
        );
        assert_eq!(os_window_title("\u{280b} cargo build"), "cargo build");
        assert_eq!(os_window_title("cargo build"), "cargo build");
        assert_eq!(os_window_title("\u{25d0}"), "\u{25d0}");
        assert_eq!(os_window_title("\u{25d0}x"), "\u{25d0}x");
    }

    #[test]
    fn a_running_block_arms_every_close_gate_without_a_child_process() {
        let app = crate::App::headless_for_test();
        let wid = WindowId(0);
        assert!(
            !app.session_foreground_busy(0),
            "a fresh stub session is idle"
        );
        assert!(!app.window_has_foreground_job(wid));
        assert!(!app.any_foreground_job());

        feed(
            &app,
            0,
            b"\x1b]133;A\x07PS> \x1b]133;B\x07Start-Sleep 60\n\x1b]133;C\x07",
        );
        assert!(
            app.session_foreground_busy(0),
            "133;C opened, no 133;D: the shell said a command is running"
        );
        assert!(
            app.window_has_foreground_job(wid),
            "the window close gate (✕, Close Tab, ctl close) sees it"
        );
        assert!(app.any_foreground_job(), "the whole-app Quit gate sees it");
        assert!(
            !app.session_foreground_busy(99),
            "an unknown session id is idle, never a wedge"
        );

        feed(&app, 0, b"\x1b]133;D;0\x07");
        assert!(!app.session_foreground_busy(0), "133;D: the job is over");
        assert!(!app.window_has_foreground_job(wid));
        assert!(!app.any_foreground_job());
    }

    #[test]
    fn a_co_viewed_running_block_exempts_the_window_gate_but_not_quit() {
        let mut app = crate::App::headless_for_test();
        let wid = WindowId(0);
        feed(
            &app,
            0,
            b"\x1b]133;A\x07$ \x1b]133;B\x07while :; do :; done\n\x1b]133;C\x07",
        );
        assert!(
            app.window_has_foreground_job(wid),
            "precondition: busy, sole viewer"
        );

        app.pool.attach(0); // a second window now shows session 0
        assert_eq!(app.pool.views(0), Some(2));
        assert!(
            !app.window_has_foreground_job(wid),
            "closing this window only detaches a view: the job survives elsewhere"
        );
        assert!(
            app.any_foreground_job(),
            "Quit would hang up every view, so the job still counts there"
        );

        assert!(
            !app.pool.detach(0),
            "back to one view; the session lives on"
        );
        assert!(
            app.window_has_foreground_job(wid),
            "the sole viewer again: this window's close would hang the job up"
        );
    }

    #[test]
    fn early_reveal_backdrop_is_the_colour_the_first_frame_paints() {
        let mut app = crate::App::headless_for_test();
        for appearance in [
            aterm_types::Appearance::Light,
            aterm_types::Appearance::Dark,
        ] {
            app.sync_app_theme_to_appearance(appearance);
            let backdrop = app.early_reveal_backdrop();
            let tc = app
                .config
                .applied_terminal_config_for_with_assets(appearance, &app.config_assets.themes);
            let cell = tc.default_background;
            assert_eq!(
                [cell.r, cell.g, cell.b],
                crate::settings::u32_rgb(backdrop),
                "{appearance:?}: an unstyled cell of the first frame must paint the erase colour"
            );
            assert_eq!(
                crate::app_render::present_band_bg(aterm_core::render::COLOR_UNSET, app.theme.bg),
                backdrop,
                "{appearance:?}: the bands around the grid must paint the erase colour"
            );
        }
        app.theme.bg = 0xFF11_1318;
        assert_eq!(
            app.early_reveal_backdrop(),
            0x11_1318,
            "the brush is seeded 24-bit: a stray alpha byte is masked off"
        );
    }

    const SCREEN: WorkAreaPts = WorkAreaPts {
        x: 0.0,
        y: 25.0,
        w: 1440.0,
        h: 875.0,
    };

    #[test]
    fn a_new_window_never_lands_exactly_on_the_window_it_was_opened_from() {
        let anchor = (400.0, 200.0);
        let placed = cascade_top_left_pts(anchor, CASCADE_STEP_PTS, (900.0, 600.0), SCREEN)
            .expect("ordinary geometry places the window");
        assert_ne!(
            placed, anchor,
            "the regression: winit centres every new window, so a same-sized window \
             lands pixel-exactly on its parent"
        );
        assert_eq!(
            placed,
            (anchor.0 + CASCADE_STEP_PTS, anchor.1 + CASCADE_STEP_PTS),
            "an ordinary cascade is one diagonal step down-right"
        );
        assert!(placed.0 > anchor.0 && placed.1 > anchor.1);
    }

    #[test]
    fn the_cascade_wraps_per_axis_instead_of_marching_off_screen() {
        let window = (900.0, 600.0);
        let low = (300.0, 720.0);
        let placed = cascade_top_left_pts(low, CASCADE_STEP_PTS, window, SCREEN)
            .expect("ordinary geometry places the window");
        assert_eq!(placed, (low.0 + CASCADE_STEP_PTS, SCREEN.y));

        let far_right = (1260.0, 100.0);
        let placed = cascade_top_left_pts(far_right, CASCADE_STEP_PTS, window, SCREEN)
            .expect("ordinary geometry places the window");
        assert_eq!(placed, (SCREEN.x, far_right.1 + CASCADE_STEP_PTS));

        let corner = (1260.0, 720.0);
        assert_eq!(
            cascade_top_left_pts(corner, CASCADE_STEP_PTS, window, SCREEN),
            Some((SCREEN.x, SCREEN.y))
        );
    }

    #[test]
    fn a_screen_filling_parent_is_still_stepped_off_not_wrapped_back_onto() {
        let filled = (SCREEN.x, SCREEN.y);
        let frame = (SCREEN.w, SCREEN.h);
        let placed = cascade_top_left_pts(filled, CASCADE_STEP_PTS, frame, SCREEN)
            .expect("a screen-filling window is still placed");
        assert_eq!(
            placed,
            (SCREEN.x + CASCADE_STEP_PTS, SCREEN.y + CASCADE_STEP_PTS)
        );
        assert_ne!(placed, filled, "the child must not land on its parent");
    }

    #[test]
    fn every_placement_keeps_a_grabbable_corner_on_screen() {
        let inside = |(x, y): (f64, f64), window: (f64, f64)| {
            let keep_x = window.0.min(CASCADE_MIN_VISIBLE_PTS);
            let keep_y = window.1.min(CASCADE_MIN_VISIBLE_PTS);
            x >= SCREEN.x
                && y >= SCREEN.y
                && x + keep_x <= SCREEN.x + SCREEN.w
                && y + keep_y <= SCREEN.y + SCREEN.h
        };

        let huge = (2000.0, 1400.0);
        let placed = cascade_top_left_pts((100.0, 100.0), CASCADE_STEP_PTS, huge, SCREEN)
            .expect("an oversized window is still placed");
        assert!(inside(placed, huge), "{placed:?} left the usable screen");

        let window = (900.0, 600.0);
        let placed = cascade_top_left_pts((5000.0, 5000.0), CASCADE_STEP_PTS, window, SCREEN)
            .expect("an off-screen anchor is still placed");
        assert!(inside(placed, window), "{placed:?} followed the anchor off");

        let tiny = (90.0, 40.0);
        let placed = cascade_top_left_pts((1420.0, 880.0), CASCADE_STEP_PTS, tiny, SCREEN)
            .expect("a tiny window is still placed");
        assert!(inside(placed, tiny), "{placed:?} left the usable screen");
        assert!(placed.0 + tiny.0 <= SCREEN.x + SCREEN.w);
        assert!(placed.1 + tiny.1 <= SCREEN.y + SCREEN.h);
    }

    #[test]
    fn degenerate_geometry_leaves_the_os_placement_alone() {
        let empty = WorkAreaPts {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        assert_eq!(
            cascade_top_left_pts((10.0, 10.0), CASCADE_STEP_PTS, (900.0, 600.0), empty),
            None,
            "a 0x0 monitor must not be treated as a placement target"
        );
        assert_eq!(
            cascade_top_left_pts((f64::NAN, 10.0), CASCADE_STEP_PTS, (900.0, 600.0), SCREEN),
            None,
            "a non-finite coordinate must not reach the clamp"
        );
        assert_eq!(
            cascade_top_left_pts(
                (10.0, 10.0),
                CASCADE_STEP_PTS,
                (f64::INFINITY, 600.0),
                SCREEN
            ),
            None,
        );
    }

    #[test]
    fn the_cascade_anchors_on_the_window_the_user_was_in_never_on_the_new_one() {
        let (a, b, new) = (WindowId(0), WindowId(1), WindowId(2));
        let attached = [a, b, new];
        assert_eq!(
            cascade_anchor_of(new, &[a, b], &attached),
            Some(b),
            "the most recently focused OTHER window is the anchor"
        );
        assert_eq!(cascade_anchor_of(new, &[a, b, new], &attached), Some(b));
        assert_eq!(cascade_anchor_of(new, &[], &attached), Some(b));
        assert_eq!(cascade_anchor_of(new, &[b, a], &[a, new]), Some(a));
        assert_eq!(cascade_anchor_of(new, &[], &[new]), None);
        assert_eq!(cascade_anchor_of(new, &[new], &[]), None);
    }

    #[test]
    fn first_successful_target_offers_milestones_even_after_early_backend_finalize() {
        let app = App::headless_for_test();
        assert!(
            !app.backend.is_pending(),
            "regression precondition: an early Wake already finalized the backend"
        );
        let now = Instant::now();
        let milestones = crate::metrics::StartupAttachMilestones::new([now; 7]);
        let (attached, publication) = present_target_install_outcome(false, milestones);
        assert!(
            !attached && publication.is_none(),
            "negative control: a stale wid must fail even after surface creation"
        );
        let (attached, publication) = present_target_install_outcome(true, milestones);
        assert!(
            attached && publication.is_some(),
            "a successfully installed target must offer its complete milestones"
        );
    }

    #[test]
    fn automatic_window_chrome_remains_system_owned_independent_of_terminal_palette() {
        let mut app = App::headless_for_test();
        app.window_theme = crate::app_config::WindowTheme::Auto;
        app.theme.bg = 0x000000;
        assert_eq!(
            app.window_theme_for_chrome(),
            crate::app_config::WindowTheme::Auto
        );
        app.theme.bg = 0xffffff;
        assert_eq!(
            app.window_theme_for_chrome(),
            crate::app_config::WindowTheme::Auto,
            "terminal colors must not silently replace system-following chrome"
        );

        app.window_theme = crate::app_config::WindowTheme::Dark;
        assert_eq!(
            app.window_theme_for_chrome(),
            crate::app_config::WindowTheme::Dark,
            "an explicit chrome override remains explicit"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_csd_chrome_resolves_auto_from_terminal_theme_darkness() {
        let mut app = App::headless_for_test();
        app.window_theme = crate::app_config::WindowTheme::Auto;
        app.theme.bg = 0x000000;
        assert_eq!(
            app.chrome_theme_for_apprt(),
            crate::app_config::WindowTheme::Dark,
            "a dark terminal theme must not sit under a light-grey CSD slab"
        );
        app.theme.bg = 0xffffff;
        assert_eq!(
            app.chrome_theme_for_apprt(),
            crate::app_config::WindowTheme::Light,
            "a light terminal theme resolves the light CSD variant"
        );
        app.window_theme = crate::app_config::WindowTheme::Light;
        app.theme.bg = 0x000000;
        assert_eq!(
            app.chrome_theme_for_apprt(),
            crate::app_config::WindowTheme::Light,
            "an explicit chrome override remains the operator's word"
        );
    }

    #[test]
    fn chrome_painters_resolve_window_theme_to_a_real_palette() {
        let mut app = App::headless_for_test();
        app.window_theme = crate::app_config::WindowTheme::Auto;
        let terminal = app.theme;
        assert!(
            crate::tab_bar::theme_is_dark(terminal.bg),
            "precondition: the default terminal theme is dark"
        );
        let auto = app.chrome_palette_theme();
        assert_eq!(
            (auto.fg, auto.bg, auto.cursor, auto.selection),
            (
                terminal.fg,
                terminal.bg,
                terminal.cursor,
                terminal.selection
            ),
            "Auto keeps chrome an extension of the terminal theme"
        );

        app.window_theme = crate::app_config::WindowTheme::Dark;
        let matching = app.chrome_palette_theme();
        assert_eq!(
            (matching.fg, matching.bg),
            (terminal.fg, terminal.bg),
            "forcing the terminal theme's own side keeps theme-derived chrome"
        );

        app.window_theme = crate::app_config::WindowTheme::Light;
        let forced = app.chrome_palette_theme();
        assert_eq!(
            forced.bg,
            crate::native_appearance::FORCED_LIGHT_CHROME_BG,
            "window_theme=light on a dark terminal theme paints the authored light chrome"
        );
        assert_eq!(forced.fg, crate::native_appearance::FORCED_LIGHT_CHROME_FG);
        assert!(
            !crate::tab_bar::theme_is_dark(forced.bg),
            "the forced palette classifies as the forced side"
        );
        assert_eq!(
            forced.selection, terminal.selection,
            "the accent identity is carried from the terminal theme"
        );
        assert_eq!(
            app.chrome_theme_for_apprt(),
            crate::app_config::WindowTheme::Light
        );
    }

    #[test]
    fn native_pages_draw_from_the_theme_before_color_overrides() {
        let mut app = App::headless_for_test();
        let base = app.theme;
        app.config.foreground = Some("#FFAA00".to_string());
        app.config.background = Some("#202040".to_string());
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        assert_eq!(app.theme.fg, 0x00FF_AA00, "the terminal takes the override");
        let page = app.native_page_palette_theme();
        assert_eq!(
            (page.fg, page.bg),
            (base.fg, base.bg),
            "the pages keep the theme's own colors"
        );
        app.window_theme = crate::app_config::WindowTheme::Light;
        assert_eq!(
            app.native_page_palette_theme().bg,
            crate::native_appearance::FORCED_LIGHT_CHROME_BG
        );
    }

    #[test]
    fn the_page_palette_follows_the_installed_resolution_not_the_live_config() {
        let mut app = App::headless_for_test();
        app.config.foreground = Some("#FFAA00".to_string());
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        let (terminal, page) = (app.theme, app.native_page_palette_theme());
        app.config.theme = Some("Nord".to_string());
        assert_eq!(app.theme, terminal, "nothing installed yet");
        assert_eq!(
            app.native_page_palette_theme(),
            page,
            "the page waits for the same install the terminal waits for"
        );
        app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
        let nord = aterm_render::Theme::from(&aterm_types::scheme::builtin("Nord").expect("Nord"));
        assert_eq!(app.theme.fg, 0x00FF_AA00, "the terminal keeps its override");
        assert_eq!(
            app.native_page_palette_theme(),
            crate::native_appearance::resolve_chrome_palette(app.window_theme, nord),
            "the page moves to the new theme with the install"
        );
    }

    #[test]
    fn native_pages_follow_the_theme_trial_with_terminal_color_overrides() {
        for foreground in [None, Some("#FFAA00".to_string())] {
            let mut app = App::headless_for_test();
            app.config = crate::app_config::Config {
                theme: Some("Default".to_string()),
                foreground: foreground.clone(),
                ..crate::app_config::Config::default()
            };
            app.install_resolved_colors(crate::app_colorscheme::ThemeChange::ConfigReload);
            let saved_page = app.native_page_palette_theme();
            let trial_page = crate::app_config::Config {
                theme: Some("Nord".to_string()),
                ..crate::app_config::Config::default()
            }
            .theme_for_with_assets(app.os_appearance, &app.config_assets.themes);
            assert_ne!(saved_page.bg, trial_page.bg, "the trial changes the page");

            app.begin_theme_preview(
                "Nord",
                crate::control_theme::PreviewHold::Settings { keeping: false },
            )
            .expect("the built-in trial is available");
            let shown = app.native_page_palette_theme();
            assert_eq!(
                (shown.fg, shown.bg, shown.cursor, shown.selection),
                (
                    trial_page.fg,
                    trial_page.bg,
                    trial_page.cursor,
                    trial_page.selection
                ),
                "the page draws the trial before terminal overrides: {foreground:?}"
            );
            if foreground.is_some() {
                assert_eq!(app.theme.fg, 0x00FF_AA00, "the terminal keeps its override");
                assert_ne!(
                    shown.fg, app.theme.fg,
                    "the page does not inherit that override"
                );
            }

            app.window_theme = crate::app_config::WindowTheme::Light;
            assert_eq!(
                app.native_page_palette_theme().bg,
                crate::native_appearance::FORCED_LIGHT_CHROME_BG,
                "forcing the window light still overrides the trial's dark side"
            );
            app.window_theme = crate::app_config::WindowTheme::Auto;
            app.revert_theme_preview(Instant::now());
            let reverted = app.native_page_palette_theme();
            assert_eq!(
                (
                    reverted.fg,
                    reverted.bg,
                    reverted.cursor,
                    reverted.selection
                ),
                (
                    saved_page.fg,
                    saved_page.bg,
                    saved_page.cursor,
                    saved_page.selection
                ),
                "revert restores the saved page palette: {foreground:?}"
            );
            assert!(app.theme_preview.is_none());
            assert_eq!(
                app.config.foreground, foreground,
                "a trial never writes config"
            );
            assert_eq!(app.config.theme.as_deref(), Some("Default"));
        }
    }

    #[test]
    fn forcing_a_side_moves_the_pages_across_the_former_classifier_band() {
        let mut app = App::headless_for_test();
        for gray in [0x76_u32, 0x80, 0x93, 0x96, 0x97, 0xA0, 0xA6, 0xAD] {
            let bg = (gray << 16) | (gray << 8) | gray;
            app.theme.bg = bg;
            for (window_theme, want_dark) in [
                (crate::app_config::WindowTheme::Light, false),
                (crate::app_config::WindowTheme::Dark, true),
            ] {
                app.window_theme = window_theme;
                let chrome = app.chrome_palette_theme();
                let side = [
                    ((chrome.bg >> 16) & 0xff) as u8,
                    ((chrome.bg >> 8) & 0xff) as u8,
                    (chrome.bg & 0xff) as u8,
                ];
                assert_eq!(
                    crate::native_appearance::surface_is_dark(side),
                    want_dark,
                    "#{gray:02X} under {window_theme:?}: the ROLE pipeline must build \
                     the forced side, whether the theme passed through or the \
                     authored palette replaced it"
                );
                assert_eq!(
                    crate::tab_bar::theme_is_dark(chrome.bg),
                    want_dark,
                    "#{gray:02X} under {window_theme:?}: and the strip must agree"
                );
            }
        }
    }

    #[test]
    fn window_created_after_config_asset_publication_inherits_custom_art() {
        let mut app = App::headless_for_test();
        let rgba: Arc<[u8]> = Arc::from([0xff, 0x22, 0xaa, 0xff]);
        let assets = Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_style: crate::app_config::TrailStyleCache::empty(),
            wallpaper: crate::app_config::WallpaperAsset::None,
            kitty_sprite: crate::app_config::KittySpriteAsset::Ready {
                source_id: Arc::from("test.png"),
                w: 1,
                h: 1,
                rgba: Arc::clone(&rgba),
                fp: 7,
            },
            themes: crate::app_config::ThemeCatalog::empty(),
            sparkle_spec_consumers: Default::default(),
        });
        assert_eq!(app.publish_config_assets(assets), 1);
        let sid = app.next_session_id;
        let wid = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        assert!(
            app.windows[&wid].fx.decos().has_custom_kitty_sprite(),
            "a post-publication window must not silently fall back to built-in art"
        );
        assert!(Arc::ptr_eq(
            app.windows[&wid].fx.decos().kitty_sprite_rgba().unwrap(),
            &rgba
        ));
    }

    #[test]
    fn every_font_size_draws_at_its_window_scale() {
        assert_eq!(crate::font_px_for_scale(FONT_PX, 1.0), FONT_PX);
        for scale in [1.25, 1.5, 2.0, 3.0] {
            assert_eq!(
                crate::font_px_for_scale(FONT_PX, scale),
                (FONT_PX * scale as f32).round(),
                "the default at {scale}"
            );
        }
        assert_eq!(crate::font_px_for_scale(13.0, 2.0), 26.0);
        assert!(
            crate::font_px_for_scale(FONT_PX + 1.0, 2.0) > crate::font_px_for_scale(FONT_PX, 2.0),
            "one step up from this platform's default grows the text at 2×"
        );
        assert_eq!(
            crate::font_px_for_scale(12.5, 1.0),
            12.5,
            "1× keeps a fraction"
        );
        for scale in [1.25, 2.0] {
            for size in 8..36 {
                let (a, b) = (size as f32, (size + 1) as f32);
                assert!(
                    crate::font_px_for_scale(b, scale) > crate::font_px_for_scale(a, scale),
                    "a step up from {a} at {scale} grows the text"
                );
            }
        }
        assert_eq!(crate::font_px_for_scale(150.0, 2.0), FONT_PX_MAX);
        assert_eq!(crate::font_px_for_scale(f32::NAN, 2.0), FONT_PX);
        assert_eq!(font_size_of_device_px(26.0, 2.0), 13.0);
        assert_eq!(font_size_of_device_px(15.0, 1.25), 12.0);
        assert_eq!(font_size_of_device_px(13.0, 1.0), 13.0);
    }

    #[test]
    fn native_chrome_title_never_overwrites_parked_terminal_identity() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let local_id = app
            .store
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .snapshot()
            .first()
            .expect("headless app registers its terminal")
            .local_id;
        app.publish_active_terminal_title(wid, "~/project");
        assert_eq!(
            app.store
                .read()
                .unwrap_or_else(|poison| poison.into_inner())
                .by_local(local_id)
                .map(|session| session.title.clone()),
            Some("~/project".to_string())
        );

        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        assert!(app.active_native_view(wid).is_some());
        app.publish_active_terminal_title(wid, "Settings");
        assert_eq!(
            app.store
                .read()
                .unwrap_or_else(|poison| poison.into_inner())
                .by_local(local_id)
                .map(|session| session.title.clone()),
            Some("~/project".to_string()),
            "native presentation title must not corrupt parked terminal metadata"
        );
    }

    #[test]
    fn window_title_identity_matches_user_osc_cwd_fallback_precedence() {
        let app = App::headless_for_test();
        let wid = WindowId(0);
        let session = app.pool.get(0).expect("session 0");
        let mut slot = String::new();

        assert_eq!(app.window_title_identity(wid, "", &mut slot), Some(0));
        assert_eq!(slot, "aterm");

        crate::term_lock(&session.term)
            .process(b"\x1b]7;file://localhost/aterm-proof/window-title\x07");
        let _ = session
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set(
                "description",
                Some("Runs the focused release checks".to_string()),
            );
        let session_id = app.window_title_identity(wid, "", &mut slot);
        assert_eq!(session_id, Some(0));
        assert_eq!(
            slot, "/aterm-proof/window-title",
            "OSC-7 cwd is the title, and an authored description is not joined into it"
        );
        assert_eq!(
            app.chrome_labels.present(
                session_id,
                &slot,
                crate::chrome_label::ChromeSurface::WindowTitle,
            ),
            "/aterm-proof/window-title",
            "the window title is the identity title alone"
        );

        app.window_title_identity(wid, "vim src/main.rs", &mut slot);
        assert_eq!(slot, "vim src/main.rs", "a live OSC 0/2 title outranks cwd");
        let _ = session
            .ctx
            .meta
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set("title", Some("release builder".to_string()));
        app.window_title_identity(wid, "vim src/main.rs", &mut slot);
        assert_eq!(
            slot, "release builder",
            "the operator title is the top rung"
        );
    }

    #[test]
    fn window_title_cwd_fallback_never_waits_for_the_terminal_parser() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.windows
            .get_mut(&wid)
            .expect("window 0")
            .tab_title_cache
            .insert(0, "~/cached-worktree".to_string());
        let term = app.pool.get(0).expect("session 0").term.clone();
        let _parser_guard = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut slot = String::new();
        app.window_title_identity(wid, "", &mut slot);
        assert_eq!(
            slot, "~/cached-worktree",
            "a contended parser keeps the last stable label instead of blocking or flickering"
        );
    }

    #[test]
    fn spinner_phase_only_store_titles_are_coalesced() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let local_id = app
            .store
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .snapshot()
            .first()
            .expect("headless app registers its terminal")
            .local_id;

        let stored_title = |app: &App| {
            app.store
                .read()
                .unwrap_or_else(|poison| poison.into_inner())
                .by_local(local_id)
                .map(|session| session.title.clone())
        };

        app.publish_active_terminal_title(wid, "⠋ aterm");
        assert_eq!(stored_title(&app), Some("⠋ aterm".to_string()));

        app.publish_active_terminal_title(wid, "⠙ aterm");
        app.publish_active_terminal_title(wid, "⠹ aterm");
        assert_eq!(stored_title(&app), Some("⠋ aterm".to_string()));

        app.publish_active_terminal_title(wid, "⠸ project");
        assert_eq!(stored_title(&app), Some("⠸ project".to_string()));
        app.publish_active_terminal_title(wid, "project");
        assert_eq!(stored_title(&app), Some("project".to_string()));
    }

    #[test]
    fn heterogeneous_window_teardown_enumerates_every_terminal_view() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, native_view) = app.active_native_view(wid).expect("Settings focused");
        let (mixed_session, mixed_view) =
            app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);

        let legacy_projection = app.windows[&wid]
            .layouts
            .iter()
            .flat_map(crate::pane::PaneTree::sessions)
            .collect::<Vec<_>>();
        assert!(
            !legacy_projection.contains(&mixed_session),
            "negative control: all-terminal layouts omit a heterogeneous leaf"
        );
        let canonical = app.window_terminal_sessions(wid);
        assert!(
            canonical.contains(&0),
            "ordinary terminal tab remains owned"
        );
        assert!(
            canonical.contains(&mixed_session),
            "job safety and teardown enumerate the mixed terminal leaf"
        );
        assert!(app.pool.get(mixed_session).is_some());

        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        assert!(app.pool.get(0).is_none());
        assert!(app.pool.get(mixed_session).is_none());
        assert!(app.view_store.get(native_view).is_none());
        assert!(app.view_store.get(mixed_view).is_none());
    }

    #[test]
    fn close_safety_counts_all_same_session_views_inside_the_window() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let second_view = app
            .view_store
            .insert_terminal(0)
            .expect("duplicate terminal view identity");
        app.pool.attach(0);
        assert!(
            app.windows
                .get_mut(&wid)
                .and_then(|window| window.tab_set.active_mut())
                .is_some_and(
                    |tab| tab.split_focused(crate::tab_model::SplitAxis::Horizontal, second_view,)
                )
        );
        app.sync_window(wid);

        assert_eq!(app.pool.views(0), Some(2));
        assert_eq!(
            app.window_terminal_sessions(wid)
                .into_iter()
                .filter(|session| *session == 0)
                .count(),
            2
        );
        assert!(
            app.window_drops_last_session_view(wid, 0),
            "both global views disappear with this one window"
        );
        assert_ne!(
            app.pool.views(0),
            Some(1),
            "negative control: the retired views==1 rule would miss this close"
        );
    }

    fn desk() -> Vec<MonitorRectPx> {
        vec![
            MonitorRectPx {
                x: 0.0,
                y: 0.0,
                w: 1920.0,
                h: 1080.0,
                scale: 1.0,
            },
            MonitorRectPx {
                x: 1920.0,
                y: 0.0,
                w: 2560.0,
                h: 1440.0,
                scale: 2.0,
            },
        ]
    }

    #[test]
    fn restored_positions_on_live_monitors_are_kept() {
        assert!(restored_origin_visible((100.0, 100.0), &desk()));
        assert!(restored_origin_visible((2400.0, 300.0), &desk()));
        assert!(restored_origin_visible((0.0, 0.0), &desk()));
        assert!(restored_origin_visible((1920.0, 0.0), &desk()));
    }

    #[test]
    fn restored_positions_on_a_departed_monitor_are_refused() {
        let undocked = vec![MonitorRectPx {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
            scale: 1.0,
        }];
        assert!(restored_origin_visible((2400.0, 300.0), &desk()));
        assert!(!restored_origin_visible((2400.0, 300.0), &undocked));
        assert!(!restored_origin_visible((-32000.0, -32000.0), &desk()));
        assert!(!restored_origin_visible((0.0, 5000.0), &desk()));
    }

    #[test]
    fn restored_positions_need_a_grabbable_corner_not_just_intersection() {
        assert!(!restored_origin_visible((1920.0 - 10.0, 100.0), &desk()));
        assert!(!restored_origin_visible((100.0, 1080.0 - 10.0), &desk()));
        assert!(!restored_origin_visible(
            (1920.0 + 2560.0 - 200.0, 100.0),
            &desk()
        ));
        assert!(!restored_origin_visible((f64::NAN, 10.0), &desk()));
        assert!(!restored_origin_visible((10.0, 10.0), &[]));
        assert!(!restored_origin_visible(
            (10.0, 10.0),
            &[MonitorRectPx {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                scale: 1.0,
            }]
        ));
    }

    fn arm_settle(app: &mut App, rows: u16, cols: u16, budget: u8) -> PhysicalSize<u32> {
        let wid = WindowId(0);
        app.windows
            .get_mut(&wid)
            .expect("headless window 0")
            .initial_frame_settle = Some(crate::InitialFrameSettle {
            rows,
            cols,
            deadline: Instant::now() + std::time::Duration::from_secs(60),
            corrections: budget,
            corrected: false,
        });
        app.window_frame_px_for(wid, rows, cols)
    }

    fn settle_state(app: &App) -> Option<crate::InitialFrameSettle> {
        app.windows
            .get(&WindowId(0))
            .and_then(|ws| ws.initial_frame_settle)
    }

    #[test]
    fn initial_frame_settle_waits_out_the_csd_header_echo_then_disarms() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);

        app.settle_initial_frame(wid, intended);
        let s = settle_state(&app).expect("an initially-correct frame must stay armed");
        assert!(!s.corrected, "no correction was needed yet");
        assert_eq!(s.corrections, 3);

        let stolen = PhysicalSize::new(intended.width, intended.height - 35);
        app.settle_initial_frame(wid, stolen);
        let s = settle_state(&app).expect("a spent correction stays armed for its own echo");
        assert!(s.corrected);
        assert_eq!(
            s.corrections, 2,
            "the mismatch spends exactly one correction"
        );

        app.settle_initial_frame(wid, intended);
        assert!(
            settle_state(&app).is_none(),
            "intended size after a correction = settled: disarm"
        );
    }

    #[test]
    fn initial_frame_settle_accepts_a_band_absorbed_fractional_scale_residue() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);
        let (cell_w, _) = app.win_cell_size(wid);

        let residue = PhysicalSize::new(intended.width + 1, intended.height);
        assert_eq!(
            app.grid_dims_for(wid, residue),
            (24, 80),
            "negative control: a 1 px residue must NOT move the grid"
        );
        app.settle_initial_frame(wid, residue);
        let s = settle_state(&app).expect("still armed — the lossy echo may yet arrive");
        assert_eq!(
            s.corrections, 3,
            "a band-absorbed residue must spend NO correction (pre-fix: spent one, \
             then two more, on every fractional-scale launch)"
        );
        assert!(!s.corrected);

        let short = PhysicalSize::new(
            intended.width - u32::try_from(cell_w).expect("cell width fits u32"),
            intended.height,
        );
        assert_ne!(
            app.grid_dims_for(wid, short),
            (24, 80),
            "negative control: a whole cell must move the grid"
        );
        app.settle_initial_frame(wid, short);
        assert_eq!(
            settle_state(&app).expect("armed").corrections,
            2,
            "a real grid change still spends exactly one correction"
        );
    }

    #[test]
    fn initial_frame_settle_budget_bounds_the_fight() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 1);
        let insisted = PhysicalSize::new(1920, 1080);

        app.settle_initial_frame(wid, insisted);
        assert_eq!(
            settle_state(&app)
                .expect("one correction spent")
                .corrections,
            0
        );

        app.settle_initial_frame(wid, insisted);
        assert!(
            settle_state(&app).is_none(),
            "budget exhausted: the insisted size is accepted for good"
        );
        assert_ne!(
            intended, insisted,
            "negative control: the fight was real, not a matching no-op"
        );
    }

    #[test]
    fn initial_frame_settle_deadline_expires_without_correcting() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .initial_frame_settle
            .as_mut()
            .unwrap()
            .deadline = Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .expect("the machine has been up a second");

        app.settle_initial_frame(wid, PhysicalSize::new(intended.width, intended.height - 35));
        assert!(
            settle_state(&app).is_none(),
            "expiry disarms instead of spending a correction"
        );
    }

    #[test]
    fn initial_frame_settle_keeps_the_csd_echo_off_the_engine_and_pty() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let rows = |app: &App| {
            (
                term.lock().expect("term").rows(),
                app.windows.get(&wid).expect("window 0").rows,
            )
        };
        let echo = PhysicalSize::new(intended.width, intended.height - 35);
        let (echo_rows, echo_cols) = app.grid_dims_for(wid, echo);
        assert!(
            echo_rows < 24 && echo_cols == 80,
            "negative control: the echo is a real grid change: {echo_rows}x{echo_cols}"
        );

        let mut seen = Vec::new();
        app.apply_window_resized_grid(wid, intended); // first configure: right
        seen.push(rows(&app));
        app.apply_window_resized_grid(wid, echo); // activation echo
        seen.push(rows(&app));
        app.apply_window_resized_grid(wid, intended); // the correction lands
        seen.push(rows(&app));
        app.flush_pending_resize(wid); // the trailing settle, if still armed
        seen.push(rows(&app));
        assert_eq!(
            seen,
            [(24, 24); 4],
            "the engine (and so the PTY winsize) must stay at the attach grid \
             through the echo — pre-fix it read 24 -> 21 -> 24"
        );
        assert!(
            settle_state(&app).is_none(),
            "the corrected frame settles the arm"
        );

        let intended = arm_settle(&mut app, 24, 80, 3);
        app.apply_window_resized_grid(wid, intended);
        app.apply_window_resized_grid(wid, echo);
        app.apply_window_resized_grid(wid, echo);
        assert_eq!(rows(&app), (24, 24), "a repeated echo is vetoed too");
        assert_eq!(
            settle_state(&app).expect("still correcting").corrections,
            1,
            "each vetoed echo spent one correction"
        );
        app.apply_window_resized_grid(wid, intended);
        assert_eq!(rows(&app), (24, 24));
        assert!(settle_state(&app).is_none());
    }

    #[test]
    fn a_refused_initial_frame_correction_still_lands_the_real_size() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);
        let echo = PhysicalSize::new(intended.width, intended.height - 35);
        let (echo_rows, _) = app.grid_dims_for(wid, echo);
        assert!(
            echo_rows < 24,
            "PRECONDITION: the echo is a real grid change"
        );
        app.apply_window_resized_grid(wid, intended);
        app.apply_window_resized_grid(wid, echo);
        assert_eq!(
            app.windows.get(&wid).expect("window 0").pending_resize,
            Some(echo),
            "the vetoed size is parked, not dropped"
        );
        assert!(
            app.windows
                .get(&wid)
                .expect("window 0")
                .next_resize_settle
                .is_some(),
            "and its trailing settle is armed"
        );
        app.flush_pending_resize(wid);
        assert_eq!(
            app.windows.get(&wid).expect("window 0").rows,
            echo_rows,
            "no correction arrived: the window's real size is the truth"
        );
    }

    #[test]
    fn a_vetoed_async_echo_is_parked_past_a_configure_round_trip() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);
        let echo = PhysicalSize::new(intended.width, intended.height - 35);
        app.apply_window_resized_grid(wid, intended);
        let before = Instant::now();
        app.apply_window_resized_grid(wid, echo);
        let parked = app
            .windows
            .get(&wid)
            .expect("window 0")
            .next_resize_settle
            .expect("the vetoed echo is parked");
        assert!(
            parked >= before + App::SETTLE_VETO_FALLBACK,
            "the fallback must outlast a configure round trip, not one throttle \
             window (pre-fix: now + {:?})",
            crate::RESIZE_THROTTLE
        );
        assert!(
            App::SETTLE_VETO_FALLBACK > crate::RESIZE_THROTTLE,
            "negative control: the park is longer than the ordinary throttle"
        );

        let intended = arm_settle(&mut app, 24, 80, 3);
        let deadline = Instant::now() + std::time::Duration::from_millis(20);
        app.windows
            .get_mut(&wid)
            .and_then(|ws| ws.initial_frame_settle.as_mut())
            .expect("armed")
            .deadline = deadline;
        app.apply_window_resized_grid(wid, intended);
        app.apply_window_resized_grid(wid, echo);
        assert_eq!(
            app.windows.get(&wid).expect("window 0").next_resize_settle,
            Some(deadline),
            "the park never outlives the settle"
        );
    }

    #[test]
    fn a_synchronous_settle_grant_applies_the_granted_size_in_place_of_the_echo() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);
        let echo = PhysicalSize::new(intended.width, intended.height - 35);
        let term = app
            .front_terminal(wid)
            .expect("front terminal")
            .term
            .clone();
        let rows = |app: &App| {
            (
                term.lock().expect("term").rows(),
                app.windows.get(&wid).expect("window 0").rows,
            )
        };
        app.apply_window_resized_grid(wid, intended);
        assert_eq!(rows(&app), (24, 24));
        let deadline = settle_state(&app).expect("armed").deadline;
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.pending_resize = Some(echo);
            ws.next_resize_settle = Some(Instant::now());
        }

        app.settle_take_over(wid, echo, Some(intended), deadline);
        let ws = app.windows.get(&wid).expect("window 0");
        assert_eq!(
            ws.pending_resize, None,
            "the grant cancels any parked reflow"
        );
        assert_eq!(ws.next_resize_settle, None);
        assert_eq!(ws.win_px, Some(intended), "the applied size is recorded");
        assert_eq!(
            rows(&app),
            (24, 24),
            "the engine never sees the echo on a synchronous grant"
        );

        let tiled = PhysicalSize::new(intended.width, intended.height + 200);
        app.settle_take_over(wid, echo, Some(tiled), deadline);
        assert_eq!(app.windows.get(&wid).expect("window 0").win_px, Some(tiled));
        assert_eq!(
            rows(&app).1,
            app.grid_dims_for(wid, tiled).0,
            "negative control: the granted size really drives the grid"
        );
        assert_ne!(rows(&app), (24, 24));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_driven_grid_resize_disarms_the_initial_frame_settle() {
        let mut app = App::headless_for_test();
        arm_settle(&mut app, 24, 80, 3);
        app.apply_grid_resize(30, 100);
        assert!(
            settle_state(&app).is_none(),
            "apply_grid_resize must clear the settle before requesting its own size"
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn xtwinops_manipulations_post_a_window_op_wake_and_reports_post_nothing() {
        use aterm_types::WindowOperation as W;
        let manipulations = [
            W::DeIconify,
            W::Iconify,
            W::RaiseWindow,
            W::LowerWindow,
            W::RefreshWindow,
            W::ResizeWindowPixels {
                height: 300,
                width: 500,
            },
            W::ResizeWindowCells { rows: 30, cols: 90 },
            W::RestoreMaximized,
            W::MaximizeWindow,
            W::MaximizeVertically,
            W::MaximizeHorizontally,
            W::UndoFullscreen,
            W::EnterFullscreen,
            W::ToggleFullscreen,
        ];
        for op in manipulations {
            match crate::spawn::window_op_wake(op.clone(), 7, WindowId(3)) {
                Some(crate::Wake::WindowOp {
                    session,
                    window,
                    op: routed,
                }) => {
                    assert_eq!(session, 7, "{op:?} must stamp the requesting session");
                    assert_eq!(window, WindowId(3), "{op:?} must stamp the owner window");
                    assert_eq!(routed, op, "the operation must survive the post unchanged");
                }
                other => panic!("{op:?} must post a WindowOp wake, got {other:?}"),
            }
        }
        let silent = [
            W::MoveWindow { x: 10, y: 10 },
            W::ReportWindowState,
            W::ReportWindowPosition,
            W::ReportTextAreaPosition,
            W::ReportTextAreaSizePixels,
            W::ReportWindowSizePixels,
            W::ReportScreenSizePixels,
            W::ReportCellSizePixels,
            W::ReportTextAreaSizeCells,
            W::ReportScreenSizeCells,
            W::ReportIconLabel,
            W::ReportWindowTitle,
            W::PushTitle {
                icon: true,
                window: true,
            },
            W::PopTitle {
                icon: true,
                window: true,
            },
        ];
        for op in silent {
            assert!(
                crate::spawn::window_op_wake(op.clone(), 7, WindowId(3)).is_none(),
                "{op:?} must not ride the wake"
            );
        }
    }

    #[test]
    fn a_cell_resize_is_honored_only_for_the_focused_session() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let focused = app.split_active_stub_tab(wid);
        assert_ne!(focused, 0, "the split spawns and focuses a second session");
        let dims = |app: &App| {
            let ws = &app.windows[&wid];
            (ws.rows, ws.cols)
        };
        let before = dims(&app);

        app.on_window_op(
            wid,
            0,
            aterm_types::WindowOperation::ResizeWindowCells { rows: 30, cols: 90 },
        );
        assert_eq!(
            dims(&app),
            before,
            "a background pane must not re-grid the window"
        );

        app.on_window_op(
            wid,
            focused,
            aterm_types::WindowOperation::ResizeWindowCells { rows: 30, cols: 90 },
        );
        assert_eq!(dims(&app), (30, 90), "the focused session re-grids");
    }

    #[test]
    fn a_window_op_re_resolves_the_stale_spawn_stamp() {
        let mut app = App::headless_for_test();
        let sid = app.next_session_id;
        let wid_b = app.insert_logical_window(crate::stub_session(sid), 24, 80);

        assert_eq!(app.window_op_target(sid, WindowId(0)), Some(wid_b));
        assert_eq!(app.window_op_target(sid, WindowId(77)), Some(wid_b));
        assert_eq!(app.window_op_target(9999, WindowId(0)), Some(WindowId(0)));
        assert_eq!(app.window_op_target(9999, WindowId(77)), None);

        app.on_window_op(
            WindowId(0),
            sid,
            aterm_types::WindowOperation::ResizeWindowCells { rows: 30, cols: 90 },
        );
        let dims = |app: &App, wid: WindowId| {
            let ws = &app.windows[&wid];
            (ws.rows, ws.cols)
        };
        assert_eq!(
            dims(&app, wid_b),
            (30, 90),
            "the operation lands on the window the session lives in"
        );
        assert_eq!(
            dims(&app, WindowId(0)),
            (24, 80),
            "the stamped window is untouched"
        );

        app.on_window_op(
            WindowId(77),
            9999,
            aterm_types::WindowOperation::ResizeWindowCells { rows: 50, cols: 50 },
        );
        assert_eq!(dims(&app, wid_b), (30, 90));
        assert_eq!(dims(&app, WindowId(0)), (24, 80));
    }
}

#[cfg(test)]
mod close_confirm_tests {
    use super::*;
    use crate::close_confirm::CloseReplay;
    use crate::quit_safety::ConfirmPrompt;

    fn busy_quit_prompt() -> ConfirmPrompt {
        crate::quit_safety::confirm_prompt(true, true, true).expect("a busy quit always prompts")
    }

    fn busy_close_prompt() -> ConfirmPrompt {
        crate::quit_safety::confirm_prompt(false, true, true).expect("a busy close prompts")
    }

    #[test]
    fn presenting_refuses_now_and_the_answer_replays_or_drops() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        assert!(!app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid)));
        assert!(app.close_banner.as_ref().is_some_and(|p| p.wid == wid));
        assert!(
            app.windows[&wid].pending_close_attribution.is_some(),
            "the gesture's exit-ledger attribution rides to the replay"
        );
        let refusal = app
            .pending_action_refusal
            .take()
            .expect("the parked question is reported to the wire");
        assert_eq!(refusal, "a quit is waiting for the person at the window");
        assert!(!app.present_close_banner(wid, busy_close_prompt(), CloseReplay::Quit));
        assert_eq!(
            app.close_banner.as_ref().map(|p| p.replay),
            Some(CloseReplay::Window(wid))
        );
        assert!(
            app.pending_action_refusal
                .take()
                .is_some_and(|why| why.starts_with("a quit ")),
            "the refusal names the question that stands, not the one dropped"
        );
        assert_eq!(app.answer_close_banner(false), None);
        assert!(app.close_banner.is_none());
        assert!(app.windows.contains_key(&wid), "cancel closes nothing");
        assert!(app.windows[&wid].pending_close_attribution.is_none());
        assert!(!app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Quit));
        assert_eq!(app.answer_close_banner(true), Some(CloseReplay::Quit));
        assert!(app.close_banner.is_none());
        assert_eq!(app.answer_close_banner(true), None);
    }

    #[test]
    fn controls_front_reports_the_pending_confirm() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let lines = app.read_aux_controls(crate::app_introspect::AuxTarget::Front);
        if cfg!(not(target_os = "macos")) {
            assert_eq!(
                app.close_confirm_controls_line().as_deref(),
                Some("confirm open=false")
            );
            assert_eq!(lines, ["overlay open=false", "confirm open=false"]);
        } else {
            assert_eq!(app.close_confirm_controls_line(), None);
            assert_eq!(lines, ["overlay open=false"]);
        }
        {
            let _closing = crate::session_store::CloseAttribution::enter(
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Ctl,
            );
            app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
        }
        let line = app
            .close_confirm_controls_line()
            .expect("a pending confirm is reported on every host");
        assert!(
            line.starts_with("confirm open=true kind=quit window=0 by=ctl "),
            "{line}"
        );
        assert!(line.contains("answer=\"confirm yes\""), "{line}");
        let lines = app.read_aux_controls(crate::app_introspect::AuxTarget::Front);
        assert_eq!(lines.last(), Some(&line), "{lines:?}");
        assert_eq!(app.answer_close_banner(false), None);
        app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
        let line = app
            .close_confirm_controls_line()
            .expect("a pending confirm is reported on every host");
        assert!(line.contains(" by=human "), "{line}");
        assert!(line.contains("answer=\"-\""), "{line}");
        assert_eq!(app.answer_close_banner(false), None);
        assert_eq!(
            app.close_confirm_controls_line().is_some(),
            cfg!(not(target_os = "macos")),
            "closed again"
        );
    }

    fn raise_from_the_wire(app: &mut App, wid: WindowId, prompt: ConfirmPrompt) {
        let proceeded = app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
            Ok(app
                .present_close_banner(wid, prompt, CloseReplay::Window(wid))
                .to_string())
        });
        assert_eq!(proceeded.as_deref(), Ok("false"), "the gesture waits");
        assert!(
            app.close_banner
                .as_ref()
                .is_some_and(crate::close_confirm::PendingClose::wire_may_answer),
            "a question raised under `invoke` is the wire's"
        );
    }

    #[test]
    fn the_wire_answers_its_own_question_whatever_window_is_in_front() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        assert_eq!(
            app.answer_close_confirm_from_wire(true),
            Err("nothing is waiting for an answer".to_string())
        );
        raise_from_the_wire(&mut app, wid, busy_quit_prompt());
        let sid = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        app.frontmost_window = Some(other);
        assert_eq!(
            app.answer_close_confirm_from_wire(false),
            Ok("answered=no kind=quit".to_string())
        );
        assert!(app.close_banner.is_none());
        assert!(app.windows.contains_key(&wid), "`no` closes nothing");
        assert!(app.windows[&wid].pending_close_attribution.is_none());

        raise_from_the_wire(&mut app, wid, busy_quit_prompt());
        assert_eq!(
            app.answer_close_confirm_from_wire(true),
            Ok("answered=yes kind=quit".to_string())
        );
        assert!(app.close_banner.is_none());
        assert!(
            app.windows[&wid].pending_close_attribution.is_some(),
            "`yes` leaves the close's attribution for the replay to journal"
        );
        assert_eq!(
            app.answer_close_confirm_from_wire(false),
            Err("nothing is waiting for an answer".to_string()),
            "answered once"
        );
    }

    #[test]
    fn the_wire_cannot_answer_a_question_the_person_asked() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(!app.present_close_banner(wid, busy_close_prompt(), CloseReplay::Window(wid)));
        for proceed in [true, false] {
            assert_eq!(
                app.answer_close_confirm_from_wire(proceed),
                Err("a window close is waiting for the person at the window".to_string())
            );
            assert!(app.close_banner.is_some(), "the person's question stands");
        }
    }

    fn named_key(key: aterm_window::keyboard::NamedKey) -> aterm_window::event::KeyEvent {
        aterm_window::event::KeyEvent::synthetic_for_test(
            aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::Enter),
            aterm_window::keyboard::Key::Named(key),
            key.to_text().map(aterm_window::keyboard::SmolStr::new),
            aterm_window::keyboard::KeyLocation::Standard,
            aterm_window::event::ElementState::Pressed,
            false,
        )
    }

    fn char_key(c: &str) -> aterm_window::event::KeyEvent {
        aterm_window::event::KeyEvent::synthetic_for_test(
            aterm_window::keyboard::PhysicalKey::Code(aterm_window::keyboard::KeyCode::KeyA),
            aterm_window::keyboard::Key::Character(aterm_window::keyboard::SmolStr::new(c)),
            Some(aterm_window::keyboard::SmolStr::new(c)),
            aterm_window::keyboard::KeyLocation::Standard,
            aterm_window::event::ElementState::Pressed,
            false,
        )
    }

    #[test]
    fn the_keyboard_answers_a_standing_question_and_nothing_leaks_under_it() {
        use aterm_window::keyboard::NamedKey;
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1));
        let mut app = App::headless_for_test_with_sink(std::sync::Arc::clone(&sink));
        app.headless = false;
        let wid = WindowId(0);
        let settled = |sink: &aterm_session::sink::SinkWriter| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while sink.ordered_egress_count() > 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        let before = sink.input_epoch();
        app.on_key(wid, char_key("a"));
        settled(&sink);
        assert_ne!(sink.input_epoch(), before, "an unguarded key writes");

        app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
        let before = sink.input_epoch();
        app.on_key(wid, char_key("a"));
        app.on_key(wid, named_key(NamedKey::Tab));
        settled(&sink);
        assert_eq!(
            sink.input_epoch(),
            before,
            "nothing reaches the shell under it"
        );
        assert!(
            app.close_banner.is_some(),
            "and nothing but an answer answers it"
        );

        app.on_key(wid, named_key(NamedKey::Escape));
        settled(&sink);
        assert!(app.close_banner.is_none(), "Escape cancels");
        assert!(app.windows.contains_key(&wid), "…and closes nothing");
        assert!(app.windows[&wid].pending_close_attribution.is_none());
        assert_eq!(sink.input_epoch(), before, "the answer is not typed either");

        app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
        app.on_key(wid, named_key(NamedKey::Enter));
        settled(&sink);
        assert!(app.close_banner.is_none(), "Enter answers");
        assert!(
            app.windows[&wid].pending_close_attribution.is_some(),
            "…yes: the attribution waits for the replay"
        );
        assert_eq!(sink.input_epoch(), before, "the Enter is not typed");

        let sid = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        app.present_close_banner(other, busy_quit_prompt(), CloseReplay::Window(other));
        app.on_key(wid, char_key("b"));
        settled(&sink);
        assert_ne!(sink.input_epoch(), before, "a key in another window passes");
        assert!(app.close_banner.is_some(), "…and answers nothing");
    }

    #[test]
    fn a_closing_window_drops_its_question() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        assert!(app.close_banner.is_none());
    }

    #[test]
    fn an_interactive_busy_close_parks_the_banner_instead_of_a_dialog() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let wid = WindowId(0);
        if cfg!(not(target_os = "macos")) {
            app.close_confirm = CloseConfirm::Interactive;
            assert!(!app.confirm_destructive_close(wid, true, true));
            let pending = app.close_banner.as_ref().expect("the question is parked");
            assert_eq!(pending.wid, wid);
            assert_eq!(pending.replay, CloseReplay::Window(wid));
            assert_eq!(
                crate::close_confirm::kind(&pending.prompt),
                "quit",
                "a last-window close quits the app, and its prompt says so"
            );
            app.close_banner = None;
            assert!(!app.confirm_destructive_gesture(wid, true, true, CloseReplay::Quit));
            assert_eq!(
                app.close_banner.as_ref().map(|p| p.replay),
                Some(CloseReplay::Quit)
            );
        }
        app.close_banner = None;
        app.close_confirm = CloseConfirm::WireRefuseBusy;
        assert!(!app.confirm_destructive_close(wid, true, true));
        assert!(app.close_banner.is_none());
        app.close_confirm = CloseConfirm::Programmatic;
        assert!(app.confirm_destructive_close(wid, true, true));
        assert!(app.close_banner.is_none());
        app.close_confirm = CloseConfirm::Interactive;
        assert!(app.confirm_destructive_close(wid, false, false));
        assert!(app.close_banner.is_none());
    }

    #[test]
    fn a_second_window_close_waits_its_turn_and_is_asked_next() {
        let mut app = App::headless_for_test();
        app.headless = false;
        let a = WindowId(0);
        let sid = app.next_session_id;
        let b = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        let sid = app.next_session_id;
        app.push_stub_tab(b, crate::stub_session(sid));
        assert!(!app.present_close_banner(a, busy_close_prompt(), CloseReplay::Window(a)));
        assert!(!app.present_close_banner(b, busy_close_prompt(), CloseReplay::Window(b)));
        assert_eq!(
            app.close_banner.as_ref().map(|p| p.wid),
            Some(a),
            "one question shows at a time"
        );
        assert_eq!(app.close_banner_queue.len(), 1, "the second waits its turn");
        assert!(
            app.windows[&b].pending_close_attribution.is_some(),
            "the waiting gesture's attribution rides to its replay"
        );
        let line = app
            .close_confirm_controls_line()
            .expect("a question stands");
        assert!(line.ends_with(" waiting=1"), "{line}");
        app.present_close_banner(b, busy_close_prompt(), CloseReplay::Window(b));
        app.present_close_banner(a, busy_close_prompt(), CloseReplay::Window(a));
        assert_eq!(app.close_banner_queue.len(), 1);
        {
            let _closing = crate::session_store::CloseAttribution::enter(
                crate::session_store::ExitReason::CtlClose,
                crate::session_store::ExitActor::Ctl,
            );
            let sid = app.next_session_id;
            let c = app.insert_logical_window(crate::stub_session(sid), 24, 80);
            assert!(!app.present_close_banner(c, busy_close_prompt(), CloseReplay::Window(c)));
            assert!(
                app.pending_action_refusal
                    .take()
                    .is_some_and(|why| why.starts_with("a window close ")),
                "the wire is told what stands"
            );
            assert_eq!(app.close_banner_queue.len(), 1);
            assert_eq!(app.close_window_logical(c), CloseOutcome::Stay);
        }
        assert_eq!(app.answer_close_banner(true), Some(CloseReplay::Window(a)));
        assert!(
            app.close_banner.is_none(),
            "asked after the replay, not before"
        );
        assert_eq!(app.close_window_logical(a), CloseOutcome::Stay);
        if cfg!(windows) {
            let pending = app
                .close_banner
                .as_ref()
                .expect("B's question is asked next");
            assert_eq!(pending.wid, b);
            assert_eq!(
                crate::close_confirm::kind(&pending.prompt),
                "quit",
                "B is the last window now, so its close quits and its question says so"
            );
            assert!(app.close_banner_queue.is_empty());
            assert!(
                !app.close_confirm_controls_line()
                    .unwrap()
                    .contains("waiting=")
            );
        } else {
            assert!(app.close_banner.is_none());
            assert!(app.close_banner_queue.is_empty());
        }

        let mut app = App::headless_for_test();
        app.headless = false;
        let sid = app.next_session_id;
        let b = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        let sid = app.next_session_id;
        app.push_stub_tab(b, crate::stub_session(sid));
        app.present_close_banner(a, busy_quit_prompt(), CloseReplay::Quit);
        app.present_close_banner(b, busy_close_prompt(), CloseReplay::Window(b));
        assert_eq!(app.answer_close_banner(false), None);
        if cfg!(not(windows)) {
            // Linux re-asks a window close only while a job runs.
            return;
        }
        let pending = app
            .close_banner
            .as_ref()
            .expect("the waiting question asks");
        assert_eq!(pending.wid, b);
        assert!(app.windows.contains_key(&a), "the cancel closed nothing");
        assert_eq!(app.close_window_logical(b), CloseOutcome::Stay);
        assert!(app.close_banner.is_none());
    }
}
