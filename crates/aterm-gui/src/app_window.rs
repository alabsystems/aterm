// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window lifecycle: logical + OS window create/attach/close/focus orchestration
//! (`create_window_logical`/`create_window_internal`/`attach_os_window`,
//! `close_window`/`close_window_logical`/`escalate_pending_close`, focus
//! bookkeeping), the `front`/`front_mut` accessors, and `apply_title`. The native
//! window chrome (colour-space/appearance, background, toolbar, menu) is reached
//! through the platform [`crate::platform::AppRt`] seam. A verbatim inherent-impl
//! split of `App`.

use std::sync::Arc;
use std::time::Instant;

use winit::dpi::{LogicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app_config::resolve_force_scale;
use crate::platform::AppRt;
use crate::spawn::spawn_session;
use crate::{
    App, Backend, BackendSlot, CloseOutcome, FONT_PX, FONT_PX_MAX, FONT_PX_MIN, PresentTarget,
    Session, TabIndex, WindowId, WindowState, pane, seed_cell_px,
};

/// A screen's usable placement rectangle in LOGICAL POINTS, top-left origin with
/// y growing DOWN — the space [`Window::outer_position`] reports once converted to
/// logical, so an anchor position, a window size, and this rectangle are directly
/// comparable. On macOS it excludes the menu bar and the Dock
/// ([`AppRt::window_work_area_pts`]); elsewhere it is the full monitor rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkAreaPts {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

/// How far down-and-right of the window it was opened from a new window is placed,
/// in LOGICAL POINTS. Sized to clear a standard titlebar, so the previous window
/// keeps a graspable strip of its own frame (and its traffic lights) visible —
/// which is the whole point: a new window must never sit exactly on top of the one
/// the user was just looking at. Roughly what AppKit's own `cascadeTopLeftFromPoint:`
/// steps by, and what Terminal/Finder cascade by.
const CASCADE_STEP_PTS: f64 = 26.0;

/// The smallest slice of a cascaded frame that must stay inside the work area, in
/// points: enough of its top-left corner to see it and grab it.
///
/// A cascaded window is deliberately ALLOWED to hang past the right and bottom
/// edges — macOS places windows that way, and permitting it is what keeps the
/// cascade meaningful for a parent that already fills the screen. Wrapping on "the
/// whole frame no longer fits" would send a work-area-sized window straight back to
/// the work area's origin: exactly on top of the window it was supposed to clear.
const CASCADE_MIN_VISIBLE_PTS: f64 = 160.0;

/// How long after an attach the [`crate::InitialFrameSettle`] may still correct
/// the frame. Long enough for the map + first configure burst AND the
/// activation-echo configure that trails it (observed well under 500 ms on
/// GNOME); short enough that a real user gesture is essentially impossible to
/// collide with — past it, the compositor's size is the user's truth.
#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(2000);
/// Corrective size requests one attach may spend (stale-echo + scale-change can
/// each need one; see [`crate::InitialFrameSettle::corrections`]).
#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_MAX_CORRECTIONS: u8 = 3;

/// Where a freshly-created window goes so it does not eclipse `anchor` (the outer
/// top-left, in points, of the window it was opened from).
///
/// A diagonal step down-right, wrapped PER AXIS back to the work area's edge once
/// the step would push the frame's grabbable corner ([`CASCADE_MIN_VISIBLE_PTS`])
/// off that edge: the cascade marches down-right, returns to the top of the screen
/// when it runs out of height (keeping its column, so the run continues rightwards)
/// and to the left when it runs out of width. Same shape as AppKit's own cascade,
/// and it can never march unboundedly off-screen. The final clamp is the fail-safe
/// for an anchor that is itself off the work area (a window dragged nearly
/// off-screen, or left behind by a display change).
///
/// `None` for degenerate geometry (a non-finite coordinate, a negative frame, or an
/// empty work area — e.g. a monitor winit reports as 0×0); the caller then leaves
/// the OS placement alone rather than moving the window somewhere nonsensical.
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
    // A frame smaller than the grabbable minimum must be kept whole, not have a
    // phantom 160 pt demanded of it.
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

/// Which existing window a new one cascades off, given the MRU focus stack and the
/// ids of the windows that actually own an OS window (ASCENDING — `App.windows` is
/// a `BTreeMap`, so its key order already is).
///
/// Most-recently-focused first, because that is the window the user was in; the
/// highest attached id otherwise, which is the most recently CREATED window and
/// therefore where an unfocused burst (session restore) last placed a frame.
/// Excluding `new` is load-bearing, not defensive: `frontmost_window` — and, once
/// the OS focuses it, `focus_order` — name the new window itself, and cascading a
/// window off its own position would step it away from nothing on every attach.
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
///
/// The close-warning is safety-critical and outranks the temporary find status;
/// find in turn outranks the continuously refreshed canonical Title/Description.
/// Canonical composition still advances its cache under either override so the
/// newest Smart Title is ready when the temporary owner leaves.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WindowTitleAuthority {
    CloseWarning,
    Search,
    Canonical,
}

#[must_use]
pub(crate) const fn window_title_authority(
    close_warning_armed: bool,
    search_active: bool,
) -> WindowTitleAuthority {
    if close_warning_armed {
        WindowTitleAuthority::CloseWarning
    } else if search_active {
        WindowTitleAuthority::Search
    } else {
        WindowTitleAuthority::Canonical
    }
}

/// Push a session's reported cwd onto `slot` in the `~`-abbreviated tab-label
/// form, reporting whether anything was written (`false` leaves `slot` untouched,
/// so the caller can fall through to the next rung). Byte-for-byte the result of
/// [`crate::app_tabs::home_abbreviated`], but pushed IN PLACE — the window-title
/// path recomposes on every redraw and must not allocate a `String` per frame.
/// Mirrors the tab strip's own in-place cwd rung (`refill_strip_titles`).
fn push_home_abbreviated_cwd(slot: &mut String, cwd: Option<&str>) -> bool {
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

/// Decide the `attach_os_window` outcome at its real installation seam. A
/// stale/missing logical window must fail closed: it cannot offer startup
/// milestones or tell the caller that a present target exists. Every successful
/// installation offers its complete timeline to the process-wide first-write
/// slot; that slot, rather than the backend's Pending/Ready state, decides which
/// successful surface is the startup surface.
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

impl App {
    /// Coalesce a destructive request behind the one live overlap handoff and
    /// nonblockingly ask its worker to abort.  Returning `true` is a hard
    /// structural barrier: the caller must not detach documents, windows, or PTYs.
    /// The worker emits completion only after the child process group is reaped;
    /// `finish_update_handoff` then rolls back overlap and replays this exact intent.
    pub(crate) fn defer_pending_update_handoff_teardown(
        &mut self,
        intent: crate::DeferredHandoffTeardown,
    ) -> bool {
        if self.pending_update_handoff.is_none() {
            return false;
        }
        // This is commit revocation, not just a wake hint. Some native/control
        // close events bypass ordinary keyboard activity accounting, and cancel
        // can race a worker that already emitted ProofReady. Advancing the epoch
        // makes that proof inadmissible; the explicit teardown predicate in the
        // proof reducer remains fail-closed even if the counter saturated.
        self.note_update_handoff_activity();
        let pending = self
            .pending_update_handoff
            .as_mut()
            .expect("pending handoff checked immediately above");
        pending.teardown.merge(intent);
        // Full means cancellation is already queued; Disconnected is handled by
        // the existing emergency-reaper path. Neither case authorizes teardown.
        let _ = pending.cancel.try_send(());
        true
    }

    /// The frontmost logical window's state (immutable). Transitional — every
    /// caller is single-window today; later steps route by an explicit WindowId.
    pub(crate) fn front(&self) -> Option<&WindowState> {
        self.frontmost_window.and_then(|id| self.windows.get(&id))
    }

    /// The window-CHROME theme to apply from config `window_theme`.
    /// `Auto` remains `Auto` on every platform so AppKit/winit/DWM can follow the
    /// live system appearance; Light and Dark are explicit overrides. Terminal
    /// palette selection is deliberately independent.
    pub(crate) fn window_theme_for_chrome(&self) -> crate::app_config::WindowTheme {
        self.window_theme
    }

    pub(crate) fn front_mut(&mut self) -> Option<&mut WindowState> {
        self.frontmost_window
            .and_then(move |id| self.windows.get_mut(&id))
    }

    /// LOGICAL window creation (NO winit): mint a fresh [`WindowId`], spawn a new
    /// single-tab session at `rows`×`cols`, register it, and install a fresh
    /// [`WindowState`] as the new frontmost window. Returns the new id, or `None`
    /// if the spawn failed (in which case NO window is minted — we never leave a
    /// broken, session-less window behind). This is the fully-testable seam the
    /// multi-window conformance test drives; `create_window_internal` wraps it with
    /// the winit surface attach.
    pub(crate) fn create_window_logical(
        &mut self,
        rows: u16,
        cols: u16,
        cwd_override: Option<&str>,
        adopt: Option<crate::spawn::Adopted>,
    ) -> Option<WindowId> {
        // Mint the window id FIRST so the spawned session's `Wake`s are stamped with
        // the window that will own them (Output/Exit/Bell route back to THIS window).
        let wid = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let sid = self.next_session_id;
        // A real run always has a proxy; only `headless_for_test` lacks one (and it
        // never calls this — it installs stub sessions directly). Guard, don't panic.
        let proxy = self.proxy.clone()?;
        // `cwd_override` (RESTORE-1: the persisted first-leaf cwd) wins; otherwise
        // inherit the cwd of the (still-current) frontmost window's focused pane, so a
        // new window opens where the user is — frontmost_window is repointed to `wid`
        // only later, in install_window_state.
        let cwd = cwd_override
            .map(str::to_owned)
            .or_else(|| self.frontmost_window.and_then(|w| self.focused_pane_cwd(w)));
        let session = match spawn_session(
            sid,
            wid,
            rows,
            cols,
            &self.session_factory,
            &proxy,
            cwd.as_deref(),
            adopt, // SEAMLESS: `Some` re-adopts this window's handed-off first-leaf shell
        ) {
            Ok(s) => s,
            Err(e) => {
                // Spawn failed: do NOT mint a broken (session-less) window. The id is
                // burned (never reused), which is fine — ids are monotonic, not dense.
                eprintln!("aterm-gui: could not open a new window: {e}");
                return None;
            }
        };
        self.next_session_id += 1;
        self.install_window_state(wid, session, rows, cols);
        Some(wid)
    }

    /// Install an already-spawned `session` as the sole tab of a fresh window `wid`
    /// and make it frontmost. Factored out of `create_window_logical` so the spawn
    /// (real PTY) and the pure windows/pool/frontmost bookkeeping are separable:
    /// the unit test drives THIS with a stub `Session`, exercising the real
    /// frontmost/windows/pool state transitions with no PTY.
    pub(crate) fn install_window_state(
        &mut self,
        wid: WindowId,
        session: Session,
        rows: u16,
        cols: u16,
    ) {
        let sid = session.id;
        // Clone the mirror Arcs BEFORE moving the session into the pool (the pool
        // then OWNS it; these are the window's active-tab mirror, source-of-truth in
        // the pool).
        let (term, master, sink) = (
            session.term.clone(),
            session.master,
            session.ctx.sink.clone(),
        );
        // P1.1: register in the process-wide registry. A new window's first tab has
        // no parent (it is a fresh root, like session 0). Test-only inert sessions
        // (`master == -1`) are not registered, avoiding phantom control targets.
        if session.master >= 0 {
            Self::register_session(&self.store, &session, None);
        }
        let layout = pane::PaneTree::new(sid);
        let tab = crate::register_terminal_tab(&mut self.tab_ids, &mut self.view_store, &layout)
            .expect("tab/view identity space");
        self.pool.insert(session);
        let metrics = self.unattached_window_metrics();
        let ws = WindowState::new_terminal(
            term,
            master,
            sink,
            sid,
            rows,
            cols,
            metrics,
            TabIndex::new(0, 1),
            vec![layout],
            crate::tab_model::TabSet::new(tab),
        );
        self.windows.insert(wid, ws);
        self.install_window_config_assets(wid);
        // The new window becomes frontmost (the standard "open and focus" behavior).
        self.frontmost_window = Some(wid);
        // The status-item glance lists windows now, so a create moves it
        // (fingerprint-gated; runs after every structural mutation above).
        self.refresh_operator_status_item();
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after create_window_logical",
        );
    }

    /// Test-only window creation that drives the SAME wid/session-id minting +
    /// `install_window_state` bookkeeping as [`Self::create_window_logical`], but
    /// takes a pre-built (stub) `Session` instead of spawning a real PTY — so the
    /// multi-window state-transition test exercises the real frontmost/windows/pool
    /// transitions with no event loop and no shell. `session.id` MUST equal the
    /// caller's `self.next_session_id` so the pool/window ids stay consistent (the
    /// test builds it that way). Returns the freshly-minted, strictly-increasing id.
    ///
    /// SPEC (TRUST_VACUITY_GATE §2.3 / finding 3): this real seam IS the
    /// `WindowRouting.CreateWindow` action — minting the next monotonic id, bumping
    /// `win_count`, and re-pointing `frontmost`. The `#[refines]` makes `window_routing`
    /// an ACTIVELY-BOUND machine in the gate (so it is coverage-gated, no longer a
    /// report-only model), and the gate now also RUNS its Tier-1 conformance
    /// (`run_window_routing_conformance`) — the "already green" claim is no longer a
    /// conflation of two disconnected tests. PROJECTION
    /// (`aterm_gui::App::project_window_routing`): `App` → `<<win_count, frontmost,
    /// next_id, exited>>` (the load-bearing +1 remap is in `window_routing_conformance::project`).
    #[cfg(test)]
    #[cfg_attr(
        test,
        aterm_spec::refines(
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

    /// Full window creation: the logical seam + (when not headless) the winit OS
    /// window attach. The new window inherits the front window's grid size (or an
    /// 80×24 default if somehow no window exists). Under headless the window stays
    /// logical-only (no OS surface); a headless 2nd window is refused EARLIER, at the
    /// `Wake::CreateWindow` arm, so this stays logical-only there only defensively.
    /// `cwd_override` (RESTORE-1) starts the first session in a specific directory
    /// instead of inheriting the frontmost pane's; `None` for every user-driven open.
    pub(crate) fn create_window_internal(
        &mut self,
        el: &ActiveEventLoop,
        cwd_override: Option<&str>,
        adopt: Option<crate::spawn::Adopted>,
    ) -> Option<WindowId> {
        let (rows, cols) = self.front().map_or((80, 24), |ws| (ws.rows, ws.cols));
        let wid = self.create_window_logical(rows, cols, cwd_override, adopt)?;
        if !self.headless && !self.attach_os_window(el, wid) {
            // GPU surface failed: roll back the just-created window + its fresh
            // session rather than leave a present-less black window.
            self.close_window_logical(wid);
            return None;
        }
        // The new window is now frontmost: re-point the GLOBAL control/notify handle
        // at its session. `install_window_state` set `frontmost_window` but does NOT
        // sync the global handle, and the OS `Focused(true)` that would normally do so
        // is a no-op here (its `frontmost != Some(wid)` guard is already satisfied) —
        // so without this the control socket keeps targeting the PREVIOUS window's
        // session for the new window's whole life. Mirrors every other new-frontmost
        // path (Cmd-Shift-O, detach-to-new-window, open_tab_in). On the attach-failure
        // path above, `close_window_logical` already re-synced the surviving front.
        self.sync_active_session();
        Some(wid)
    }

    /// The SINGLE join point for a windowed cold launch's deferred backend build
    /// (#7 tail): take the `Pending` handle, join it, and run the setup `main`
    /// used to run right after its early join — the `metrics` backend tag, the
    /// 1× pad seed (re-applied at the window's real scale by the caller), the
    /// GPU bloom config, and the text shaping + font-feature warnings.
    /// Idempotent: a `Ready` slot returns immediately (every non-first attach,
    /// and always in headless).
    ///
    /// A backend-build panic is a launch-fatal loss of the only renderer: exit
    /// with a one-line failure — the same outcome class as the old pre-`run_app`
    /// `.expect` — never a half-attached window.
    pub(crate) fn finalize_backend(&mut self) {
        let BackendSlot::Pending(handle) = &mut self.backend else {
            return;
        };
        let handle = handle.take().expect("finalize_backend re-entered mid-join");
        let (backend, use_gpu) = handle.join().unwrap_or_else(|_| {
            eprintln!("aterm-gui: backend-build thread panicked; no renderer — exiting");
            std::process::exit(1);
        });
        assert!(
            backend.admitted_font_sources_sealed(),
            "backend worker published an unsealed font generation"
        );
        self.backend = BackendSlot::Ready(backend);
        self.use_gpu = use_gpu;
        crate::metrics::set_backend_gpu(use_gpu);
        // Seed the interior padding at 1×, exactly as the old pre-`run_app` join
        // did; `attach_os_window` re-applies it at the window's real scale.
        // Config-resolved so a set `window_padding` shapes the first frame too.
        self.backend.set_pad(self.cfg_pad_for_scale(1.0));
        // Seed the chrome headroom at 0 (no NSWindow measured yet);
        // `attach_os_window` re-applies the real titlebar band post-attach.
        self.backend.set_head(0);
        // GPU cursor-comet BLOOM settings from config (GPU backend only; the
        // CPU/software path has no bloom). Set before the first present so the
        // per-window bloom target is built alongside the offscreen.
        let gpu_post_fx = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::GpuPostFx);
        if let Backend::Gpu(g) = self.backend.ready_mut() {
            g.set_bloom(gpu_post_fx && self.config.cursor_trail_bloom_or_default());
            g.set_bloom_params(
                self.config.cursor_trail_bloom_strength_or_default(),
                self.config.cursor_trail_bloom_radius_or_default(),
            );
            // Heat shimmer above burning cells (the bloom's parity class —
            // GPU only, like the bloom; the CPU path has no shimmer).
            g.set_shimmer(gpu_post_fx && self.config.cursor_fire_shimmer_or_default());
            // M3 phase B: the EDR cursor glow opt-in (default off). Set BEFORE any
            // window surface exists so an opted-in first window gets the
            // Rgba16Float extended-linear swapchain at attach.
            g.set_hdr_glow(self.config.hdr_glow_or_default());
            // SDR twin: the swapchain-side crown budget for non-HDR desktops.
            g.set_sdr_glow_boost(self.config.cursor_glow_sdr_boost_or_default());
        }
        // The build worker already applied and sealed the complete font
        // generation before publishing this backend. Re-pin only memory-backed
        // shaping/typography/render knobs here; reopening configured font paths
        // on the event-loop thread would violate the worker-only seal contract
        // and serialize hundreds of MB of fallback parsing before first paint.
        self.pin_backend_render_config_core();
        // Once-only font-feature diagnostics — now that the backend carries the
        // shaping AND the resolved font config, so the font probe is accurate.
        self.warn_font_feature_issues();
        // NOT `sync_chrome_fonts()` here. It hands the terminal face to the chrome
        // rasterizer so Settings/About/Palette render in the user's font, and it
        // costs ~295 ms (a ChromeFace parse of the resolved face plus a semantic
        // surface fork) — measured on the event-loop thread, INSIDE the window-attach
        // bracket, i.e. squarely between launch and the first frame. Nothing the
        // first terminal frame draws reads those faces: every `tray_raster`
        // consumer is an overlay (Settings, About, Palette, the modal trays) that
        // only exists after a deliberate user action. So it runs on the
        // first-present hook in `app_render` instead, where it is off time-to-glass
        // and still far earlier than any overlay can be opened.
    }

    /// The window a NEW window should be offset from: the most recently FOCUSED
    /// other window that still owns an OS window, falling back to the highest-id
    /// other attached window when no focus history exists (a restore loop creating
    /// several windows back-to-back, or a control-driven open before any
    /// `Focused(true)` arrives).
    ///
    /// `frontmost_window` is deliberately NOT the source: every creation path
    /// re-points it at the brand-new window BEFORE the OS surface attaches, so it
    /// would name the very window being placed. `focus_order`'s last entry is still
    /// the window the user was actually in — the new one is not in it yet, since
    /// nothing has focused it. `None` means this is the only window on glass:
    /// nothing to avoid, so the OS's own (centred) placement stands.
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

    /// The usable placement rectangle of the screen `window` occupies, in points:
    /// the real work area when the platform can measure one (macOS: menu bar and
    /// Dock excluded), else the winit monitor rectangle — the FULL display. Off
    /// macOS that fallback only makes the cascade wrap a little later than it
    /// ideally would; it never places a window wrongly.
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

    /// Offset a freshly created 2nd..Nth window down-and-right of the window it was
    /// opened from, so that window stays visible behind it.
    ///
    /// winit gives a window it creates with no explicit position the OS default
    /// placement, and on macOS that is `NSWindow::center` — so a new window, which
    /// inherits the front window's grid and therefore its exact size, lands
    /// pixel-perfectly ON TOP of it and Cmd-N reads as "nothing happened". Every
    /// new-window path funnels through `attach_os_window`, so placing it there
    /// covers Cmd-N, Move Tab to New Window, Open Session in New Window, and the
    /// restore loop's extra windows alike.
    ///
    /// Called AFTER the real frame size is known (the frame is what decides where
    /// the cascade must wrap) and BEFORE the surface exists, so no present has
    /// happened yet and the window never visibly hops. Best-effort in every
    /// direction — no other window (the first window), an unreadable position, or
    /// degenerate screen geometry all simply leave the OS placement in force.
    /// Callers carrying an EXPLICIT position (session restore, the seamless-update
    /// window carry) apply theirs after the attach and deliberately win over this.
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
        // Points, not pixels: the anchor and the new window can sit on displays with
        // different scale factors, and winit's physical coordinates are each scaled
        // by their own display's factor — only the logical space is common.
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
        // Where a window lands is otherwise invisible to everything but the eye:
        // no control verb reports a frame's screen origin, and the OS placement it
        // replaces leaves no trace. One `$ATERM_LOG=debug` record (coordinates and
        // screen metrics only — never window content) makes the decision auditable.
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

    /// Create the OS window + present surface for logical window `wid` and attach
    /// them to its [`WindowState`]. Factored out of `resumed` so it serves BOTH the
    /// first window (at `resumed`) and every Cmd-N 2nd..Nth window (at
    /// `create_window_internal`). Sizes the OS window from the window's stored grid,
    /// installs the macOS menu (FIRST window only), and builds the GPU or CPU present
    /// target. NEVER called in headless (no OS window is ever created there). A
    /// missing `wid` (stale) is a silent no-op on the present-target writes.
    /// Returns `true` iff the OS window was created AND a present target installed.
    /// `false` means an OS-window, GPU-swapchain, or CPU-surface creation failure: the
    /// just-created OS window (if any) is dropped rather than installed present-less
    /// (which would show a permanently black window), and the caller rolls back the
    /// logical window (or, for the first window, exits). Every failure path is
    /// fail-SOFT — declining just this window — never a process-wide panic that would
    /// also destroy every other live window/session.
    #[must_use]
    pub(crate) fn attach_os_window(&mut self, el: &ActiveEventLoop, wid: WindowId) -> bool {
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        // (#7 tail) FIRST window while the backend build is still in flight:
        // create the OS window BEFORE joining, so the AppKit window-server round
        // trip overlaps the GPU device/pipeline/font tail instead of being
        // serialized behind it. Its initial size comes from ESTIMATED cell
        // metrics (`seed_cell_px` at the old 1× pad seed) and it is created
        // HIDDEN; the join + the existing `request_inner_size` correction below
        // fix the size before the first show, so the seed is never on glass (no
        // size jump on 1× displays, no unthemed flash). Every non-first attach
        // (Cmd-N) sees a Ready slot — byte-identical behavior to before.
        let pending_join = self.backend.is_pending();
        // Startup-only drill-down. Every successful installation offers these
        // stamps to a process-wide OnceLock. The first successful surface wins;
        // later Cmd-N windows cannot replace it. Do NOT key this to `pending_join`:
        // an early proxy Wake may legitimately finalize the backend before the
        // first `resumed` attach reaches this function.
        let startup_attach_entry = Instant::now();
        // The window holds the terminal grid PLUS the tab-strip rows at the top PLUS
        // independent configured top and base bottom padding. `window_frame_px`
        // folds in both; with both zero this is the original `rows * ch`. Chrome headroom
        // is deliberately 0 here — no NSWindow exists yet to
        // measure a titlebar band; the post-attach `set_head` + size recompute
        // below fold it in before the window is ever shown.
        let mut size = if pending_join {
            let (cw, ch) = seed_cell_px(self.font_px);
            let pad = self.cfg_pad_for_scale(1.0);
            let vertical_pad = pad.saturating_add(self.cfg_pad_top_for_scale(1.0));
            let total_rows = rows.saturating_add(self.tab_strip_rows);
            PhysicalSize::new(
                (cols as usize * cw + pad.saturating_mul(2)) as u32,
                (total_rows as usize * ch + vertical_pad) as u32,
            )
        } else {
            self.window_frame_px(rows, cols)
        };
        let attrs = Window::default_attributes()
            .with_title("aterm")
            .with_inner_size(size);
        // LINUX WINDOW IDENTITY + FLOOR. `with_name` is the Wayland `app_id`
        // AND the X11 `WM_CLASS` (one shared attribute serves both backends):
        // without it the compositor sees an EMPTY app id — GNOME shows the
        // generic gear icon, refuses to group aterm windows, and can never
        // match a future `aterm.desktop` (icon/pinning/dock identity all key on
        // this string equalling the desktop-file basename). Keep it `"aterm"`
        // and lowercase-stable across releases for exactly that reason. The min
        // size is the smallest LEGAL grid — the 20-col × 5-row clamp floor at
        // the 12 px base font plus strip row and padding, in LOGICAL px so
        // HiDPI scales it — a floor `resize`/WM hints cannot collapse to a
        // sub-cell sliver. macOS/Windows attrs are byte-identical to before.
        #[cfg(target_os = "linux")]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attrs
                .with_name("aterm", "aterm")
                .with_min_inner_size(winit::dpi::LogicalSize::new(164.0, 98.0))
        };
        // a11y: AccessKit must attach BEFORE the window is first shown, so create it
        // hidden and reveal it right after the adapter is built (feature-gated; the
        // default build keeps winit's visible-by-default).
        #[cfg(feature = "a11y-accesskit")]
        let attrs = attrs.with_visible(false);
        // OVERLAP HANDOFF: EVERY window of a handoff boot is created hidden and
        // revealed only after its first REAL content present (the post-present
        // hook in `app_render`), so the parked parent's frozen frame is never
        // covered by a themed-background-only flash — the carried pixels are the
        // first thing on glass. Bounded: `about_to_wait` force-reveals at the
        // deadline if presents keep dropping while hidden.
        let defer_reveal = self.handoff_ready.is_some();
        // Deferred-join first window: created hidden, shown only after the joined
        // backend's real metrics have resized it (below). A no-op when the a11y
        // feature already forced hidden creation.
        let attrs = if pending_join || defer_reveal {
            attrs.with_visible(false)
        } else {
            attrs
        };
        let startup_before_window_create = Instant::now();
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                // Fail soft: decline this one window so the caller rolls back (or, for
                // the first window, exits) — never unwind the whole process.
                eprintln!("aterm-gui: OS window creation failed: {e}");
                return false;
            }
        };
        let startup_after_window_create = Instant::now();
        // P2: attach this window's AccessKit adapter (events arrive as `Wake::Accessibility`
        // via the proxy), then SHOW the window. Stored into `ws` at the os_window assignment
        // below. `update_if_active` (in app_settings) pushes the Settings tree on change.
        #[cfg(feature = "a11y-accesskit")]
        let a11y_adapter = self
            .proxy
            .clone()
            .map(|proxy| accesskit_winit::Adapter::with_event_loop_proxy(el, &window, proxy));
        // Deferred-join first window: keep it hidden past the adapter attach; the
        // post-join size correction below is the single show site (the adapter
        // still attaches strictly before the first show, as AccessKit requires).
        #[cfg(feature = "a11y-accesskit")]
        if !pending_join && !defer_reveal {
            window.set_visible(true);
        }
        // Native macOS menu bar (menu.rs): build + install NSApp.mainMenu now the
        // FIRST window exists, so aterm presents as a real Mac app. There is ONE
        // shared NSApp.mainMenu, so window 2..N must NOT reinstall it (that would
        // drop the first install's retained action target and rebuild the bar). The
        // `_menu.is_none()` guard makes the install fire exactly once. Skipped under
        // `--headless` (this fn is never reached there); a no-op off macOS. The
        // returned action target is RETAINED in `self` (AppKit holds a menu item's
        // target only weakly) for the run loop's life.
        if self._menu.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            self._menu = self.apprt.install_menu(proxy);
            // Defer AppKit `terminate:` quits that BYPASS the menu (Dock-icon Quit,
            // AppleScript `quit`, logout/restart/shutdown) onto the typed App lane,
            // which runs the same confirmation + document barrier as Cmd-Q.
            // Registered once alongside the menu (a no-op off macOS).
            self.apprt.install_quit_confirm();
        }
        // The menu-bar OPERATOR status item (status_item.rs): created once with
        // the FIRST window, alive for the process lifetime (the handle retains
        // both the NSStatusItem — releasing one removes it from the bar — and
        // its weakly-referenced action target). Same one-shot/headless/off-macOS
        // contract as the menu install above.
        if self._status_item.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            let glance = self.operator_fleet_glance();
            self._status_item = self.apprt.install_status_item(proxy, &glance);
            self.operator_status_fingerprint = Some(glance.fingerprint());
            self.operator_local_id = glance.operator_session;
            // Seed the sibling-instances section so the FIRST menu open has
            // fleet rows (menu opens kick refreshes thereafter).
            if self._status_item.is_some() {
                crate::fleet_watch::request_scan(proxy.clone());
            }
        }
        // W11 MotionPolicy: seed the OS "Reduce Motion" flag at ATTACH and subscribe
        // to its change notification once (the observer target is retained for the
        // process life, like `_menu`). Windows has a real attach-time query but no
        // live observer, so it re-samples on each attach. Linux/other platforms
        // currently return `false` / `None`, leaving explicit config in control.
        if self._reduce_motion.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            self.system_reduce_motion = self.apprt.reduce_motion();
            crate::native_appearance::install_preferences(
                self.apprt.native_appearance_preferences(),
            );
            self._reduce_motion = self.apprt.observe_reduce_motion(proxy);
        }
        // IME-1: opt into IME so the window receives `WindowEvent::Ime`
        // (Preedit/Commit) for CJK/dead-key/Option composition. Never enabled
        // before, so composition input was impossible.
        window.set_ime_allowed(true);
        // OS color-scheme source: seed every session of this window with the REAL
        // desktop light/dark appearance (winit's `Window::theme()`, cross-platform).
        // The engine REPORTS the scheme to apps (DEC 2031 + DSR ?996n); this is what
        // actually feeds it the host value at startup. Live OS toggles are handled by
        // `WindowEvent::ThemeChanged`. `None` (indeterminate OS) maps to the engine's
        // own `Dark` default, so this is a no-op there.
        let os_appearance = crate::app_colorscheme::theme_to_appearance(window.theme());
        self.apply_os_color_scheme(wid, os_appearance);
        // Also switch aterm's OWN rendered theme to the matching side of a
        // `dark:…,light:…` split `theme` config (a no-op for a single theme, or when
        // the OS appearance equals the engine default already in effect).
        self.sync_app_theme_to_appearance(os_appearance);
        // (#7 tail) THE join: the OS window now exists (its creation overlapped the
        // build tail); everything below — the HiDPI font derivation, the pad, the
        // corrected window size, the present target — needs the real backend. A
        // build panic exits inside `finalize_backend` (launch-fatal, same outcome
        // as the old pre-`run_app` `.expect`).
        let startup_before_backend_finalize = Instant::now();
        if pending_join {
            self.finalize_backend();
        }
        let startup_after_backend_finalize = Instant::now();
        // FIRE-INTO-CHROME (order matters): extend the content view under the
        // titlebar FIRST (fullSizeContentView), THEN measure the band via
        // `contentLayoutRect` — the ONE truth for how much chrome sits above the
        // usable content. Measuring pre-mask (`frame - contentRectForFrameRect`)
        // UNDER-counts by the unified toolbar's height (32 vs 46 pt observed), and a
        // band that disagrees with the post-mask resize law costs a grid row and
        // mis-pads the top. `contentLayoutRect` is recomputed synchronously by the
        // style-mask change, so the measure is valid immediately — no need to wait
        // for the grown bounds to land. Stored in POINTS so scale changes recompute
        // the px headroom without an AppKit round-trip. Both calls are no-ops off
        // macOS.
        self.apprt.window_set_fullsize_content(&window);
        // Native macOS window toolbar (toolbar.rs): a unified-style NSToolbar with
        // a "+" New Tab button, so the window presents as a real Mac app.
        // Installed HERE — before the titlebar-band measure below (the toolbar
        // GROWS `contentLayoutRect`'s chrome band, 32→46 pt observed; measuring
        // first under-counts and costs a grid row) and before the GPU/CPU present
        // split so BOTH backends get it. The "+" reuses File ▸ New Tab (posts the
        // same `Wake::MenuAction { NewTab }`). The retained backing objects are
        // kept in `self._toolbars` keyed by window (AppKit holds the toolbar's
        // delegate + the item's target only WEAKLY). A no-op off macOS; never
        // reached under `--headless`. Cloning the proxy avoids borrowing `self`
        // immutably (proxy) and mutably (`_toolbars`) at once.
        if let Some(proxy) = self.proxy.clone()
            && let Some(handle) = self.apprt.install_toolbar(&window, &proxy, wid)
        {
            // Pin the strip's appearance to the THEME's darkness so the tab
            // labels' semantic colours resolve against their actual backdrop (the
            // theme-coloured titlebar), not the OS/window appearance — a dark
            // theme under a light OS otherwise renders inactive labels as
            // translucent black on near-black. Re-synced on every theme change
            // (apply_theme_live + the reload rebuild branch).
            crate::toolbar::set_strip_dark(&handle, crate::tab_bar::theme_is_dark(self.theme.bg));
            // Pin the user's selected-tab color override (config `active_tab_color`)
            // at install too, so a fresh window's strip matches the live config.
            crate::toolbar::set_active_tab_color(&handle, self.config.active_tab_color_rgb());
            self._toolbars.insert(wid, handle);
        }
        // Measure the band AFTER the mask + toolbar: `contentLayoutRect` now
        // reflects the full chrome (titlebar + unified toolbar) above the usable
        // content — the ONE truth the resize law re-derives against.
        let head_pts = self.apprt.titlebar_band_pts(&window);
        // Seed the last-good-windowed band memory from the same sample (via the
        // one decision law — only a sane decorated windowed measurement seeds).
        // Without this, a fullscreen entered before any windowed resize would
        // leave the memory at 0 and a bad exit sample would have nothing to
        // restore. The applied `head_pts` stays the verbatim measurement — the
        // deliberate post-chrome ordering above makes attach the trusted read.
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
        // HiDPI / Retina auto-scale. aterm rasterizes glyphs at `font_px` PHYSICAL
        // pixels and works in physical units throughout, so on a 2× Retina display
        // the built-in 12 px default renders at ~6 LOGICAL points — crisp but tiny.
        // The display scale factor is only knowable once the window exists, so apply
        // it HERE: when the size is the DEFAULT (no `$ATERM_FONT_PX`, no
        // `config.font_px`), scale it to `round(FONT_PX × scale)`. An EXPLICIT size is
        // honored verbatim — never double-scaled.
        //
        // W12: SELECT this window's size on the SHARED renderer via the LIGHT
        // `activate_px` — NOT a rebuild. The old path swapped the whole CPU backend
        // (or re-faced the GPU one) whenever a window landed on a different-scale
        // display, tearing down EVERY other window's warm glyph atlas and re-parsing
        // the font on the critical path to this window's first pixel. The light
        // switch keeps the shared faces + every size's glyphs resident (they coexist
        // by `px_q`); other windows simply re-activate their own size on their next
        // redraw. This is the same mechanism `apply_window_scale` uses per frame, so
        // attach and steady-state agree.
        //
        // An explicit render-scale override ($ATERM_FORCE_SCALE / --scale) wins over
        // the window's real scale_factor(), driving BOTH the auto-scaled font and the
        // interior padding so a forced scale renders identically to that real DPI.
        let scale = resolve_force_scale().unwrap_or_else(|| window.scale_factor());
        // Record THIS window's scale so `redraw_window` can re-select the shared
        // backend to it per frame (per-window DPI); the block below activates it for
        // this window's first paint + the geometry-dependent sizing that follows.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scale = scale;
        }
        if let Some(scaled) = hidpi_target_font_px(self.font_px_explicit, scale) {
            // Light switch on a real change; a no-op guard keeps the common case
            // (every 2nd..Nth window on the SAME display) free. The faces / shaping /
            // fallback config on the shared renderer are unchanged by a size switch,
            // so none of the old rebuild's re-pinning is needed. Cmd-0 resets to this
            // scaled default rather than the tiny FONT_PX base.
            if (scaled - self.font_px).abs() >= 0.5 {
                self.backend.activate_px(scaled);
            }
            self.font_px = scaled;
            self.default_font_px = scaled;
        }
        // Apply the interior padding at the window's REAL scale and recompute `size`
        // so the window — and the GPU swapchain configured from it below — fits the
        // grid PLUS this border (and the new cell metrics the size activation above
        // selected) PLUS the tab strip.
        let pad = self.cfg_pad_for_scale(scale);
        self.backend.set_pad(pad);
        // TOP-only tighter interior pad (the ~46 pt chrome band already separates
        // the grid from the window top). The frame shrinks by exactly the removed
        // top pixels; the bottom remains the base pad. Apply it after `set_pad`:
        // an actual base-pad change deliberately starts a new symmetric padding
        // regime, while a redundant same-value `set_pad` is a complete no-op.
        let pad_top = self.cfg_pad_top_for_scale(scale);
        self.backend.set_pad_top(pad_top);
        // FIRE-INTO-CHROME: the titlebar band in device px.
        let head_pts = self.windows.get(&wid).map_or(0.0, |ws| ws.head_pts);
        let head = (head_pts * scale).round() as usize;
        self.backend.set_head(head);
        // W12: record THIS window's resolved metrics from its OWN scale — the
        // per-window source of truth `on_scale_factor_changed` keeps current. Uses the
        // live `font_px` (which the explicit-font / force-scale paths above pin
        // verbatim) so the record never diverges from what the window renders.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.metrics = crate::MetricsView::applied(self.font_px, pad, pad_top, head);
        }
        size = self.window_frame_px(rows, cols);
        let _ = window.request_inner_size(size);
        // LINUX INITIAL-FRAME SETTLE: the request above is pre-map on Wayland —
        // no CSD frame exists yet and `scale_factor()` may still be the assumed
        // 1.0 — so the compositor's first configure round can hand back a frame
        // that is NOT `rows`×`cols` (GNOME subtracts the 35 px CSD header from a
        // geometry echo; a HiDPI output re-derives the font under the same
        // logical size). Arm one bounded post-map re-request of THIS grid; see
        // [`crate::InitialFrameSettle`] / [`Self::settle_initial_frame`].
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
        // W1 (kill the compositor stretch): hint the WM to resize in whole-cell
        // steps, so an interactive edge drag lands on an exact grid fit and the
        // remainder bands stay at the base pad. macOS honours this during live
        // resizes only — zoom/tiling/maximize still produce arbitrary sizes,
        // which the per-edge padding bands absorb (the actual root fix). The
        // increments are re-derived on every font/DPI rebuild (`rebuild_backend`).
        {
            let (cw, ch) = self.cell_size();
            window.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
        }
        // Don't eclipse the window this one was opened from: step the frame down and
        // right so the previous window stays visible behind it. Placed HERE — after
        // the size correction above (the real frame decides where the cascade wraps)
        // and before any present — so the move is never on glass. A no-op for the
        // first window, and overridden by the explicit positions the restore /
        // seamless-update carries apply after the attach returns.
        self.cascade_window_placement(wid, &window);
        // Paint the window background the terminal's theme background colour so the
        // transparent titlebar — and the bare single-tab compact bar — reads as a
        // seamless extension of the terminal body instead of a distinct lighter chrome
        // strip (Ghostty's "transparent" titlebar look). Runs for BOTH backends: the
        // GPU arm `return`s below, so this must precede the split. No-op off macOS
        // (the Linux apprt's `window_set_background_color` does nothing).
        self.apprt
            .window_set_background_color(&window, self.theme.bg);
        // Apply the configured window-chrome appearance (config `window_theme`) at
        // attach, BEFORE the GPU/CPU split so BOTH present paths get it — the GPU
        // arm `return`s below. On Windows this is winit `set_theme` →
        // DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE), so the titlebar
        // matches from the first frame instead of waiting for a config reload; on
        // macOS it also matches the NSWindow colour space to the device-RGB content
        // (dropping CoreAnimation's per-frame gamut conversion — see the apprt
        // method's docs).
        self.apprt
            .window_set_appearance(&window, self.window_theme_for_chrome());
        // (#7 tail) Reveal the deferred-join first window only NOW: the joined
        // backend's real metrics resized it above and the themed background is
        // set, so the seeded size / unthemed chrome are never on glass. (Under
        // a11y the adapter attached strictly before this first show.)
        if pending_join {
            // Handoff boots stay hidden here — the post-present hook is the one
            // reveal site (carried pixels first, never a bg-only frame).
            if !defer_reveal {
                window.set_visible(true);
            }
            // First paint must never wait on the OS to volunteer a WM_PAINT: request
            // it explicitly so the just-revealed window presents its first real frame
            // on the next loop turn (the class-brush themed erase covers the gap; see
            // `AppRtWindows::window_set_background_color`). Harmless everywhere — a
            // duplicate redraw of a fresh window coalesces.
            window.request_redraw();
        }
        // W1: the surface (GPU swapchain or CPU softbuffer) is sized to the RAW
        // window pixels — what the WM actually granted, not the grid-quantized
        // request — so the compositor never rescales the presented frame. Read
        // AFTER the deferred-join reveal above, so `inner_size()` reflects the
        // really-granted size. The `Resized` event that may still follow re-syncs
        // both `win_px` and the swapchain; a zero inner size (not yet laid out)
        // falls back to the requested frame size.
        let inner = window.inner_size();
        let raw_px = if inner.width == 0 || inner.height == 0 {
            size
        } else {
            inner
        };
        let startup_before_surface_create = Instant::now();
        if self.backend.is_gpu() {
            // GPU mode: a wgpu swapchain on the SAME instance/adapter as the
            // offscreen renderer. The offscreen frame is blitted into it and
            // presented on the GPU — no softbuffer surface is created.
            let (w_px, h_px) = (raw_px.width, raw_px.height);
            let surface_result =
                self.backend
                    .gpu_mut()
                    .unwrap()
                    .create_window_surface(window.clone(), w_px, h_px);
            let startup_after_surface_create = Instant::now();
            match surface_result {
                Ok(surf) => {
                    // M3 (colour-managed present): the wgpu surface creation just
                    // attached a CAMetalLayer to this view; tag it so ColorSync
                    // INTERPRETS the presented bytes — the configured space
                    // (default `srgb`) for an SDR swapchain, or (phase B) the
                    // extended-linear-sRGB tag an HDR (Rgba16Float) swapchain
                    // REQUIRES (`resolve_surface_colorspace`, proven precedence).
                    // Interpretation only — the bytes (and the readback/
                    // introspection parity) are untouched. No-op off macOS.
                    let hdr = surf.is_hdr();
                    let surface_colorspace =
                        crate::platform::resolve_surface_colorspace(self.window_colorspace, hdr);
                    let effective_colorspace = self
                        .apprt
                        .window_set_surface_colorspace(&window, surface_colorspace);
                    // LIVE-RESIZE ANCHOR: the same freshly-attached CAMetalLayer
                    // keeps CoreAnimation's default `contentsGravity` (`resize`),
                    // which STRETCHES the last presented frame onto the new bounds
                    // for every drag step we have not repainted yet — text smears,
                    // then snaps back. Pin it to `topLeft` so an un-repainted frame
                    // stays at 1:1 where it was drawn. Once per window, here,
                    // because the layer does not exist before the surface does.
                    self.apprt.window_anchor_surface_top_left(&window);
                    // M5 TRUE VIBRANCY: with the wgpu CAMetalLayer now attached,
                    // install the NSVisualEffectView backdrop + flip the window /
                    // Metal-layer opacity when `background_opacity < 1.0`, so this
                    // window's translucent (PostMultiplied) present composites over
                    // live blur. No-op at the solid default / off macOS. Windows
                    // opened AFTER a reload read the same cached knobs.
                    self.apprt.window_set_vibrancy(
                        &window,
                        self.render_knobs.background_material,
                        aterm_render::vibrancy::is_translucent(
                            self.render_knobs.background_opacity,
                        ),
                        self.theme.bg,
                    );
                    // M3 phase B: seed this window's per-screen EDR headroom for
                    // the aurora boost (re-queried on monitor changes in
                    // `refresh_frame_interval`). Only worth a query on an HDR
                    // swapchain; the renderer-side sanitizer makes the unset
                    // default (0.0 → headroom 0) provably inert.
                    let mut window_gpu = aterm_gpu::WindowGpu::new();
                    // Snapshot/video PNGs are unprofiled sRGB. Freeze the ACTUAL
                    // compositor interpretation alongside the surface instead of
                    // guessing it from Bgra8 (which can also be tagged P3).
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
                        #[cfg(feature = "a11y-accesskit")]
                        {
                            ws.a11y = a11y_adapter;
                        }
                        ws.present = Some(PresentTarget::Gpu {
                            gpu_surface: surf,
                            window_gpu,
                        });
                        true
                    } else {
                        false
                    };
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
                    // The window's GPU swapchain failed even though the offscreen
                    // device initialized (e.g. an adapter that is offscreen-capable
                    // but not present-capable — some Linux/Vulkan setups). With GPU
                    // now the default renderer, a hard exit here would crash launch
                    // on such a box. If ANOTHER window already presents on the GPU,
                    // keep the hard rollback — downgrading the shared backend would
                    // corrupt those live GPU surfaces. Otherwise this is the
                    // first/only window: downgrade the whole app to the CPU
                    // softbuffer renderer and fall through to the CPU present path
                    // below (reusing the just-created `window`).
                    let other_gpu = self
                        .windows
                        .values()
                        .any(|w| matches!(w.present, Some(PresentTarget::Gpu { .. })));
                    if other_gpu {
                        eprintln!("aterm-gui: GPU surface creation failed: {e}");
                        drop(window);
                        return false;
                    }
                    eprintln!(
                        "aterm-gui: GPU surface creation failed: {e}; falling back to the CPU renderer"
                    );
                    match self
                        .backend
                        .cpu_renderer_from_admitted(self.font_px, self.theme)
                    {
                        Ok(cpu) => {
                            self.backend = BackendSlot::Ready(Backend::Cpu(cpu));
                            self.use_gpu = false;
                            crate::metrics::set_backend_gpu(false);
                            self.backend.set_pad(self.cfg_pad_for_scale(scale));
                            self.backend.set_pad_top(self.cfg_pad_top_for_scale(scale));
                            self.backend.set_head(head);
                            self.pin_backend_render_config_core();
                            self.warn_font_feature_issues();
                            self.sync_chrome_fonts();
                            // Do NOT return — fall through to the softbuffer path.
                        }
                        Err(error) => {
                            eprintln!("aterm-gui: resident CPU font fallback failed: {error}");
                            drop(window);
                            return false;
                        }
                    }
                }
            }
        }
        // CPU (softbuffer) present path. Mirror the GPU arm's fail-soft rollback: a
        // surface-creation failure drops the just-created window and returns false so
        // the caller declines just this window, instead of `.expect()` unwinding the
        // whole process and taking every other window/session down with it.
        let context = match softbuffer::Context::new(window.clone()) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("aterm-gui: softbuffer context creation failed: {e}");
                drop(window);
                return false;
            }
        };
        let surface = match softbuffer::Surface::new(&context, window.clone()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("aterm-gui: softbuffer surface creation failed: {e}");
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
            #[cfg(feature = "a11y-accesskit")]
            {
                ws.a11y = a11y_adapter;
            }
            ws.present = Some(PresentTarget::Cpu {
                surface,
                _context: context,
            });
            true
        } else {
            false
        };
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

    /// LINUX INITIAL-FRAME SETTLE (see [`crate::InitialFrameSettle`]): called on
    /// every `WindowEvent::Resized` AFTER the normal reflow has applied `size`.
    /// While armed and inside the deadline, a `size` that is NOT the attach's
    /// intended `rows`×`cols` frame — re-derived at the window's CURRENT scale,
    /// so a post-map `ScaleFactorChanged` folds into the same target — spends
    /// one corrective `request_inner_size`. A matching `size` after a correction
    /// disarms (settled); a matching `size` BEFORE any correction keeps the arm,
    /// because GNOME's activation configure can echo the pre-CSD-frame geometry
    /// (header subtracted, −35 px) AFTER an initially-correct resize. winit
    /// itself drops the request under a non-stateless configure
    /// (maximized/fullscreen/tiled), so a tiling compositor is never fought.
    #[cfg(target_os = "linux")]
    pub(crate) fn settle_initial_frame(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        let Some(settle) = self.windows.get(&wid).and_then(|ws| ws.initial_frame_settle) else {
            return;
        };
        if Instant::now() > settle.deadline || settle.corrections == 0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.initial_frame_settle = None;
            }
            return;
        }
        // Re-tune the shared backend to THIS window's live scale before deriving
        // the target frame — a `ScaleFactorChanged` may have landed since attach
        // and `window_frame_px` reads the backend's current cell metrics.
        self.apply_window_scale(wid);
        let intended = self.window_frame_px(settle.rows, settle.cols);
        if size == intended {
            if settle.corrected && let Some(ws) = self.windows.get_mut(&wid) {
                ws.initial_frame_settle = None;
            }
            return;
        }
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if let Some(s) = ws.initial_frame_settle.as_mut() {
            s.corrections -= 1;
            s.corrected = true;
        }
        let Some(w) = ws.os_window.clone() else {
            return;
        };
        // winit's Wayland backend applies this request SYNCHRONOUSLY and
        // returns `Some` — NO `Resized` event will follow (the event-driven
        // shape the macOS/X11 backends have). Left unhandled, the app keeps
        // the echo's size while the compositor holds the corrected one, and
        // the two never reconverge (observed: a 24-row engine cropped into a
        // 329 px surface). So feed the applied size through the same resize
        // path an event would have taken, and cancel the throttled reflow of
        // the very size being corrected away — its trailing settle would
        // otherwise re-apply the stale grid on top of this resync. `None`
        // (X11/async) keeps the event path authoritative.
        if let Some(applied) = w.request_inner_size(intended) {
            ws.pending_resize = None;
            ws.next_resize_settle = None;
            ws.win_px = Some(applied);
            self.sync_surface_to_window(wid);
            self.on_resize(wid, applied);
            w.request_redraw();
        }
    }

    /// LOGICAL window teardown (NO winit/`el`): close window `wid` — drop every one
    /// of its tabs' PANES' views (the last view closes the PTY master via
    /// `Session::drop`), remove the window (dropping its present target →
    /// surface/`Arc<Window>`; the SHARED GPU device on the `Backend` is NEVER
    /// dropped — the S6 invariant), clear its winit mapping, and re-point
    /// `frontmost_window` to a surviving window if it named the closed one. Returns
    /// whether the APP should now exit ([`CloseOutcome::Exit`] iff no windows
    /// remain). A stale/unknown `wid` is a silent `Stay`.
    ///
    /// SPEC (TRUST_VACUITY_GATE §2.3 / finding 3): this real production seam IS the
    /// `WindowRouting.CloseWindow` action — decrement `win_count`, exit-iff-empty, and
    /// the nondeterministic frontmost re-point. The `#[refines]` (paired with the
    /// `CreateWindow` anchor) makes `window_routing` actively-bound + coverage-gated;
    /// its Tier-1 conformance is run by the gate (`run_window_routing_conformance`).
    /// PROJECTION `aterm_gui::App::project_window_routing` (the `window_routing_conformance::project`).
    #[cfg_attr(
        test,
        aterm_spec::refines(
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
        // The recording owns this WindowState's tap and a pre-created output
        // directory. Abort while the tap is still reachable, before structural
        // removal can strand the client until its old deadline.
        let _ = self.video_abort_window_close(wid);
        let Some(ws) = self.windows.get(&wid) else {
            return CloseOutcome::Stay;
        };
        // RESTORE-1: this close is about to end the app (the last window is
        // going away) — capture the layout manifest NOW, while the window's tabs/panes
        // and the pool's sessions are still intact (everything below tears them down
        // before the `CloseOutcome::Exit` reaches `el.exit()`). The post-loop writer in
        // `main` persists it (and is what gates headless runs off the user's manifest —
        // this stash is memory-only).
        if self.config.restore_session_or_default() && self.exits_app_when_closing(wid) {
            self.quit_capture = Some(self.capture_restore_manifest());
        }
        // Snapshot EVERY canonical terminal-view edge before mutating. The old
        // all-terminal `layouts` projection deliberately omits heterogeneous tabs;
        // using it here would leak a mixed tab's live Session/PTY on window close.
        // Preserve duplicate session ids because each terminal View owns one pool
        // attachment and therefore requires one matching detach.
        let ids = self.window_terminal_sessions(wid);
        let view_ids: Vec<crate::tab_model::ViewId> = ws
            .tab_set
            .tabs()
            .iter()
            .flat_map(|tab| tab.root.leaves())
            .collect();
        // Drop every pane's view. DETACH the pool view FIRST (which drops the Session
        // iff it was the last view, closing its PTY master), and deregister from the
        // process-wide registry ONLY when that detach actually dropped the session —
        // a shared (Cmd-Shift-O) session still viewed in ANOTHER window keeps its
        // single store entry while a view remains. A genuinely-closed id then
        // fail-closes a later @<selector>. EACH pane is detached (not once per tab)
        // so a split-tab window releases every pane's PTY.
        for id in ids {
            if self.detach_session_view(id)
                && let Some(sid) = self
                    .store
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .deregister_local(id)
            {
                crate::proxy::unpublish_session(&sid);
            }
        }
        for view in view_ids {
            self.remove_view_link(view);
        }
        // Forget this window's taskbar-progress change-detection entry BEFORE its HWND
        // is dropped, so a later window reusing the same HWND value is not falsely
        // skipped (its button vanishes with the window, so no explicit clear is needed).
        #[cfg(windows)]
        {
            let hwnd = self.window_hwnd(wid);
            if hwnd != 0 {
                taskbar::forget(hwnd);
            }
        }
        // Drop the WindowState (its PresentTarget → GpuSurface/softbuffer Surface +
        // Arc<Window>; the shared GPU DEVICE on the Backend is untouched).
        self.windows.remove(&wid);
        // Release this window's retained native toolbar backing objects (no-op off
        // macOS / when none was installed) so they don't outlive the window.
        self._toolbars.remove(&wid);
        // A multi-line-paste confirmation sheet hanging off this window dies with it,
        // and AppKit posts no answer for a sheet torn down that way — so retire the
        // entry (and with it the key interceptor) HERE. Without this the app would keep
        // a filter alive for a window that no longer exists and refuse the next paste
        // confirmation as "already outstanding" until the self-healing attachment check
        // caught it.
        #[cfg(target_os = "macos")]
        if self.paste_confirm.as_ref().is_some_and(|c| c.wid == wid) {
            self.paste_confirm = None;
        }
        // Clear the winit→logical mapping for this window (its OS id is gone).
        self.winit_to_window.retain(|_, &mut v| v != wid);
        // Drop the closed window from the focus-order stack so it can never be picked
        // as a survivor below.
        self.focus_order.retain(|w| *w != wid);
        // A pending menu-driven raise of this window dies with it.
        if self.pending_deminiaturize_focus == Some(wid) {
            self.pending_deminiaturize_focus = None;
        }
        // Re-point frontmost if it named the just-closed window: the most-recently
        // focused SURVIVOR (matching the window the OS raises), with a deterministic
        // lowest-live-id fallback. See `next_frontmost_after_close`.
        if self.frontmost_window == Some(wid) {
            self.frontmost_window = self.next_frontmost_after_close();
        }
        if !self.windows.is_empty() {
            debug_assert!(
                self.structural_invariants_ok(),
                "window/session structural invariants violated after close_window_logical",
            );
            // A survivor became (or stayed) frontmost: re-mirror the control socket /
            // notify target onto its active tab, exactly like a tab/focus switch.
            self.sync_active_session();
            debug_assert!(
                self.structural_invariants_ok(),
                "window/session structural invariants violated after re-mirror",
            );
        }
        // Glance refresh AFTER the store deregistration and the map removal,
        // so the menu shows post-close truth (fires on the last close too —
        // harmless when the loop is about to exit).
        self.refresh_operator_status_item();
        if self.windows.is_empty() {
            CloseOutcome::Exit
        } else {
            CloseOutcome::Stay
        }
    }

    /// Record that `wid` gained OS focus — move it to the most-recent end of the
    /// focus-order (MRU) stack consulted when the frontmost window closes. Removing
    /// any prior occurrence before pushing keeps the stack deduped and bounded by the
    /// live-window count. Called only from `WindowEvent::Focused(true)`, so headless
    /// (no OS focus events) leaves `focus_order` empty and the re-point falls back to
    /// the lowest live id — byte-identical to the pre-MRU behavior.
    pub(crate) fn note_window_focused(&mut self, wid: WindowId) {
        self.focus_order.retain(|w| *w != wid);
        self.focus_order.push(wid);
        // The glance marks the frontmost window, and a same-window re-focus is
        // the case the `frontmost_window` guard in the Focused(true) arm skips
        // — this seam sees every focus gain (fingerprint-gated, cheap).
        self.refresh_operator_status_item();
    }

    /// The window to make frontmost when the current front window closes: the
    /// most-recently-FOCUSED window that still exists (matching the window macOS
    /// raises — usually NOT the lowest id), falling back to the lowest live
    /// `WindowId` when no focus history applies (headless, or a window never
    /// focused). The fallback keeps the choice DETERMINISTIC where there is no OS
    /// focus to honor — the behavior the headless multi-window tests pin. Returns
    /// `None` only when no window remains.
    pub(crate) fn next_frontmost_after_close(&self) -> Option<WindowId> {
        self.focus_order
            .iter()
            .rev()
            .find(|w| self.windows.contains_key(w))
            .copied()
            .or_else(|| self.windows.keys().next().copied())
    }

    /// Close window `wid` and exit the app IFF it was the LAST window (the
    /// `ExitIffEmpty` invariant). The single routing point for every close path
    /// (CloseRequested, last-tab Cmd-W/CloseTab, a last-tab `Wake::Exit`).
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
                // Fail closed: structural teardown must never become the error
                // recovery for a document-persistence failure.
                aterm_log::warn!("window document close barrier: {error}");
                return;
            }
        }
        if self.exits_app_when_closing(wid) && self.apply_deferred_native_update_on_clean_quit() {
            return;
        }
        if matches!(self.close_window_logical(wid), CloseOutcome::Exit) {
            el.exit();
        }
    }

    /// Escalate any window whose LAST-tab close set `pending_close`: close it (the
    /// close paths have no `ActiveEventLoop`, so they flag instead). The flag is set
    /// on the FRONTMOST window by keyboard/menu Cmd-W and on the CLICKED window by a
    /// tab-strip close — either may differ from the event-stamped window — so SCAN
    /// for it rather than assume the event window. At most one is set per action;
    /// clearing it first guards against a re-trigger if the close somehow no-ops.
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
            // The destructive-close confirm already ran at the tab-close gate
            // (`window_exit_close_allowed`, inside `close_tab_at`/`close_active_tab`)
            // BEFORE `pending_close` was set, so this escalation must NOT re-prompt —
            // it just tears the window down (exiting the app iff it was the last
            // window). Re-confirming here would pop a SECOND dialog for one gesture.
            self.close_window(el, w);
        }
    }

    /// M2 quit-safety: whether session `id`'s PTY has a foreground JOB (not the idle
    /// shell). See [`crate::quit_safety::foreground_is_job`]. Unknown id → idle.
    fn session_foreground_busy(&self, id: u64) -> bool {
        let Some(s) = self.pool.get(id) else {
            return false;
        };
        let fg = crate::quit_safety::foreground_pgrp(s.master);
        crate::quit_safety::foreground_is_job(fg, s.pid)
    }

    /// M2: any pane (across every tab) of window `wid` has a foreground job that
    /// CLOSING this window would actually SIGHUP. A Cmd-Shift-O co-viewed session has
    /// one PTY but is shown in more than one window (pool view-count > 1); closing this
    /// window only detaches its view and the shell keeps running elsewhere — so it must
    /// NOT arm the close confirm. Only a session whose LAST viewer is this window
    /// (`views == 1`) is at risk, so only those count toward "busy".
    fn window_has_foreground_job(&self, wid: WindowId) -> bool {
        let mut ids = self.window_terminal_sessions(wid);
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().any(|id| {
            self.window_drops_last_session_view(wid, id) && self.session_foreground_busy(id)
        })
    }

    /// Closing `wid` removes all of its canonical view edges at once. A shared
    /// session is therefore safe only when at least one pool view remains outside
    /// this window—not merely when its global refcount happens to exceed one.
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

    /// M2: any live session in the whole app has a foreground job running.
    fn any_foreground_job(&self) -> bool {
        self.pool.iter().any(|s| {
            crate::quit_safety::foreground_is_job(
                crate::quit_safety::foreground_pgrp(s.master),
                s.pid,
            )
        })
    }

    /// Arm the ~2 s close/quit confirm window on `wid` and show the warning in its
    /// titlebar (`apply_title` suppresses overwriting it while armed; `new_events`
    /// restores the live title at expiry).
    fn arm_close_warning(&mut self, wid: WindowId) {
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.close_warning_until =
                Some(Instant::now() + crate::quit_safety::CLOSE_CONFIRM_WINDOW);
            if let Some(w) = ws.os_window.as_ref() {
                w.set_title(crate::quit_safety::CLOSE_WARNING_TITLE);
            }
        }
    }

    /// Confirm a destructive close/quit gesture for window `wid`, returning `true` to
    /// PROCEED. `exits_app` = the gesture quits the WHOLE program (Cmd-Q / app-menu
    /// Quit, or a close that removes the last window); `busy` = closing now would
    /// SIGHUP a foreground job.
    ///
    /// On macOS this shows the native confirmation dialog (`NSAlert`) — ALWAYS for a
    /// whole-app quit (the iTerm-style "are you sure you want to quit?" the user
    /// expects from ⌘Q) and, for a window/tab close that leaves the app running, only
    /// while a job is running. Off macOS (no native dialog) it falls back to the ~2 s
    /// titlebar-warning confirm: the gesture is refused once (arming the warning) and
    /// proceeds on a repeat within the window.
    ///
    /// NON-INTERACTIVE closes always PROCEED without a dialog: `headless` (no window to
    /// confirm against) and a control-socket-driven close (`close_confirm_suppressed`,
    /// e.g. the `tab close` verb) are DELIBERATE programmatic instructions, not stray
    /// user gestures — blocking them on a human click would wedge the UI thread and the
    /// client's reply. This means the M2 busy-job guard is intentionally not enforced
    /// for programmatic/headless closes; it exists to catch accidental UI gestures.
    fn confirm_destructive_close(&mut self, wid: WindowId, exits_app: bool, busy: bool) -> bool {
        if self.headless || self.close_confirm_suppressed {
            return true;
        }
        let Some(prompt) = crate::quit_safety::confirm_prompt(exits_app, busy) else {
            // App keeps running and nothing is busy: a plain close needs no confirm.
            return true;
        };
        if let Some(proceed) = self
            .apprt
            .confirm(prompt.title, prompt.body, prompt.proceed)
        {
            return proceed;
        }
        // No native dialog on this platform (off macOS): the in-window
        // titlebar-warning fallback. `close_decision` gates on `busy` only, so an idle
        // whole-app quit still closes immediately (as before); a busy gesture is
        // refused once and confirmed by a repeat within the armed window.
        let armed = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.close_warning_until)
            .is_some_and(|t| Instant::now() < t);
        match crate::quit_safety::close_decision(busy, armed) {
            crate::quit_safety::CloseDecision::Close => true,
            crate::quit_safety::CloseDecision::Warn => {
                self.arm_close_warning(wid);
                false
            }
        }
    }

    /// Quit-safety for a TAB close that would EXIT the window. The tab-close gestures
    /// (Cmd-W, Close Tab, the strip/native ✕, the `tab close` verb) all funnel through
    /// `close_active_tab`/`close_tab_at`, which — when the close drops the window's LAST
    /// tab (and, for a pane-close, its last pane) — escalate to closing the window (and
    /// exiting the app if it was the last window). Those paths must enforce the SAME
    /// confirmation the red traffic light ([`Self::on_close_requested`]) and Cmd-Q
    /// ([`Self::on_quit_requested`]) enforce, so a stray last-tab Cmd-W can't SIGHUP an
    /// in-flight build/AI run — or quit the whole app — without asking.
    ///
    /// Given `exits_window` (the caller computed that this close closes the window),
    /// return `true` to PROCEED or `false` to CANCEL via
    /// [`Self::confirm_destructive_close`] — the native quit/close dialog when the close
    /// exits the last window or a job is running. A non-exiting close (a split-pane
    /// collapse, or one tab among several) always returns `true`.
    pub(crate) fn window_exit_close_allowed(&mut self, wid: WindowId, exits_window: bool) -> bool {
        if !exits_window {
            return true;
        }
        // Closing this window quits the whole app only when no other mixed-tab host
        // survives.
        let exits_app = self.exits_app_when_closing(wid);
        let busy = self.window_has_foreground_job(wid);
        self.confirm_destructive_close(wid, exits_app, busy)
    }

    /// Whether closing window `wid` quits the app: no other mixed-tab host survives.
    fn exits_app_when_closing(&self, wid: WindowId) -> bool {
        !self.windows.keys().any(|w| *w != wid)
    }

    /// Quit-safety: a CloseRequested (red traffic light / Window ▸ Close). Confirms via
    /// [`Self::confirm_destructive_close`] — the native "quit aterm?" dialog when this is
    /// the LAST window (closing it exits the app), or the "a process is running" dialog
    /// when a pane of `wid` has a foreground job. An idle non-last-window close proceeds
    /// immediately.
    pub(crate) fn on_close_requested(&mut self, el: &ActiveEventLoop, wid: WindowId) {
        // The red traffic light closes this window and quits when no other window
        // survives.
        let exits_app = self.exits_app_when_closing(wid);
        let busy = self.window_has_foreground_job(wid);
        if self.confirm_destructive_close(wid, exits_app, busy) {
            self.close_window(el, wid);
        }
    }

    /// Quit-safety for whole-app Quit (Cmd-Q / app menu): ALWAYS shows the native
    /// "are you sure you want to quit aterm?" confirmation (iTerm-style), and exits the
    /// whole program only when the user confirms. A running foreground job in any window
    /// enriches the dialog copy. Off macOS (no native dialog) it falls back to the
    /// titlebar-warning confirm, which is busy-gated.
    ///
    /// This covers Cmd-Q and the menu-bar "Quit aterm" item (both route through our
    /// custom `menuAction:` selector). The Dock-icon "Quit", AppleScript `quit`, and
    /// logout/restart/shutdown send AppKit `terminate:` straight to `NSApp` instead —
    /// that hook vetoes and posts [`crate::Wake::NativeTerminateRequested`] through
    /// [`crate::platform::AppRt::install_quit_confirm`], returning here on the typed
    /// event-loop lane before any process exit can occur.
    pub(crate) fn on_quit_requested(&mut self, el: &ActiveEventLoop) {
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested)
        {
            return;
        }
        // The window the confirm is anchored to: the frontmost, or ANY survivor when
        // none is frontmost (e.g. before the first OS focus). Resolving a window even
        // with no frontmost means a Cmd-Q still prompts instead of exiting outright;
        // only a truly window-less app (headless) exits immediately.
        let anchor = self
            .frontmost_window
            .filter(|w| self.windows.contains_key(w))
            .or_else(|| self.windows.keys().next().copied());
        // Cmd-Q / app-menu Quit ALWAYS confirms (it exits the whole program); `busy`
        // — any job in any window — enriches the dialog copy. A truly windowless
        // process has nowhere to anchor a dialog, but still runs the store-owned
        // document barrier (a dirty suspended document must not be skipped).
        if let Some(wid) = anchor {
            let busy = self.any_foreground_job();
            if !self.confirm_destructive_close(wid, true, busy) {
                return;
            }
        }
        match self.prepare_quit_native_shutdown() {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                aterm_log::warn!("quit native close barrier: {error}");
                return;
            }
        }
        match self.prepare_quit_document_shutdown() {
            Ok(true) => {}
            Ok(false) => {
                if self.quit_document_shutdown_blocked() {
                    self.cancel_failed_quit_document_shutdown();
                }
                return;
            }
            Err(error) => {
                aterm_log::warn!("quit document close barrier: {error}");
                self.cancel_failed_quit_document_shutdown();
                return;
            }
        }
        if self.apply_deferred_native_update_on_clean_quit() {
            return;
        }
        let _ = self.video_abort_app_shutdown();
        el.exit();
    }

    /// Stateful counterpart of AppKit's bare `applicationShouldTerminate:`
    /// callback. The callback always defers the first request and posts this typed
    /// generation; only this event-loop lane may confirm, save documents, apply a
    /// deferred update, and exit.
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
            if !self.confirm_destructive_close(window, true, busy) {
                let _ = crate::menu::cancel_native_termination(generation);
                return;
            }
        }

        match self.prepare_quit_native_shutdown() {
            Ok(true) => {}
            Ok(false) => {
                let _ = crate::menu::cancel_native_termination(generation);
                return;
            }
            Err(error) => {
                aterm_log::warn!("native terminate app-close barrier: {error}");
                let _ = crate::menu::cancel_native_termination(generation);
                return;
            }
        }

        match self.prepare_quit_document_shutdown() {
            Ok(true) => {}
            Ok(false) => {
                if self.quit_document_shutdown_blocked() {
                    self.cancel_failed_quit_document_shutdown();
                    let _ = crate::menu::cancel_native_termination(generation);
                }
                return;
            }
            Err(error) => {
                aterm_log::warn!("native terminate document barrier: {error}");
                self.cancel_failed_quit_document_shutdown();
                let _ = crate::menu::cancel_native_termination(generation);
                return;
            }
        }
        // Keep AppKit's generation Pending while the asynchronous update child
        // owns this quit. A duplicate Dock/Cmd-Q terminate must remain deferred;
        // completing early would let AppKit kill the parked parent before Commit.
        if self.apply_deferred_native_update_on_clean_quit() {
            return;
        }
        if !crate::menu::complete_native_termination(generation) {
            return;
        }
        let _ = self.video_abort_app_shutdown();
        el.exit();
    }

    /// Re-evaluate document-owned close plans after an asynchronous atomic-save
    /// completion.  Whole-app Quit has priority; otherwise each now-ready window
    /// has already detached all of its document edges as one batch and may be
    /// structurally removed without re-entering the barrier.
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
            if self.apply_deferred_native_update_on_clean_quit() {
                return;
            }
            // Ordinary quit is now irreversible. Async handoff success exits via
            // `_exit` and intentionally never reaches this completion boundary.
            let _ = crate::menu::complete_current_native_termination();
            let _ = self.video_abort_app_shutdown();
            el.exit();
            return;
        }
        for window in windows {
            if self.exits_app_when_closing(window)
                && self.apply_deferred_native_update_on_clean_quit()
            {
                return;
            }
            if matches!(self.close_window_logical(window), CloseOutcome::Exit) {
                el.exit();
                return;
            }
        }
    }

    /// Resolve the stable identity used by native window chrome. Terminal windows
    /// share the tab-label precedence exactly: operator title, live OSC 0/2 title,
    /// reported OSC-7 cwd, tab presentation, then `"aterm"`. The terminal fallback
    /// read is deliberately nonblocking because this runs on the event-loop thread;
    /// contention keeps the last tab-title cache value (or the presentation fallback)
    /// until the next output wake/redraw instead of parking behind a busy parser.
    ///
    /// Resolves IN PLACE into `slot` (which arrives holding anything and leaves
    /// holding the resolved raw title) rather than returning an owned `String`:
    /// [`Self::apply_title`] runs on every redraw of the window, so the steady
    /// state must not allocate. Only the rare operator-title rung, which owns its
    /// `String` in the session meta, still copies.
    fn window_title_identity(
        &self,
        id: WindowId,
        live_title: &str,
        slot: &mut String,
    ) -> (Option<u64>, Option<String>) {
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
            return (None, None);
        };
        let Some(session) = self.pool.get(session_id) else {
            slot.clear();
            slot.push_str(presentation_fallback);
            return (Some(session_id), None);
        };
        let (user_title, authored_description) = {
            let meta = session
                .ctx
                .meta
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                meta.presentation_value("title"),
                meta.presentation_value("description"),
            )
        };
        slot.clear();
        if let Some(user_title) = user_title {
            slot.push_str(&user_title);
        } else if !live_title.is_empty() {
            slot.push_str(live_title);
        } else {
            // Same rungs as before, written into the resident slot: reported cwd
            // (`~`-abbreviated), else the last stable tab-title cache value when the
            // parser holds the lock, else the presentation fallback.
            let filled = match session.term.try_lock() {
                Ok(term) => push_home_abbreviated_cwd(slot, term.current_working_directory()),
                Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                    let term = poisoned.into_inner();
                    push_home_abbreviated_cwd(slot, term.current_working_directory())
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
        (Some(session_id), authored_description)
    }

    /// Reflect the focused terminal's stable identity in the window chrome, falling
    /// back to `"aterm"` when nothing has set one. Calls `set_title` only on an actual
    /// semantic change (a cheap String compare plus leading-busy-spinner
    /// coalescing), so it is safe to call every frame — even on the redraw
    /// early-out path, where a title-only change still updates the titlebar
    /// without a pixel repaint.
    ///
    /// IME-1: while a composition is in flight, the marked preedit text is shown
    /// as `title [‹preedit›]` — the minimal inline indicator that an
    /// IME/dead-key composition is active and what it currently holds. Because
    /// this runs on the early-out path too, the indicator follows the
    /// composition without forcing a full pixel repaint.
    ///
    /// TABS: with more than one in-window tab, a ` — [active/total]` indicator is
    /// appended (e.g. `aterm — [2/3]`) so the (visual-tab-bar-less) tab state is
    /// visible in the window chrome. A single tab shows no indicator, so a
    /// one-session window's title is byte-identical to before. (The count is the
    /// number of TABS, not panes — a split tab is still one tab in the indicator.)
    pub(crate) fn apply_title(&mut self, id: WindowId, window: &Window, title: &str) {
        // TASKBAR PROGRESS (Windows, ConEmu OSC 9;4): mirror this window's focused
        // session's engine `taskbar_progress()` onto its taskbar button. `apply_title`
        // runs every frame for the redrawn window (including the "nothing changed"
        // early-out path), so this follows OSC-driven progress + tab switches within a
        // frame; `sync_taskbar_progress` is change-gated so it only calls into COM when
        // the state actually differs. A no-op on every other platform.
        #[cfg(windows)]
        self.sync_taskbar_progress(id);
        // Keep the registry's title for the FOCUSED pane's session fresh
        // (best-effort), so a cross-session `sessions` read reflects the live window
        // title. Gate on the per-window `(session, title)` cache: take the process-
        // wide store WRITE lock (contended with the control thread) ONLY when the
        // active session or its title actually changed since the last publish — a
        // steady screen no longer grabs the exclusive lock every redraw. Resolve the
        // active session via the TARGET window's active tab focus → pool.
        self.publish_active_terminal_title(id, title);
        // Resolve + compose through a RESIDENT slot: this runs on every redraw of
        // this window (including the no-change early-out), so the identity ladder
        // and the label composition must reuse one warmed buffer instead of
        // allocating two `String`s per frame. Taken out of `self` for the call so
        // `&self.config`/`&self.title_summaries` stay borrowable, and put back
        // (with its capacity) before every exit.
        let mut scratch = std::mem::take(&mut self.title_compose_scratch);
        let (focused_session, authored_description) =
            self.window_title_identity(id, title, &mut scratch);
        // Composes IN PLACE: `scratch` arrives holding the resolved raw title and
        // leaves holding the presented label — a cache hit `clone_from`s into the
        // slot's existing capacity, so a steady frame allocates nothing.
        self.title_summaries.compose_label_into(
            focused_session,
            authored_description.as_deref(),
            self.config.window_title_format_or_default(),
            &self.config,
            " — ",
            &mut scratch,
        );
        let preedit = self.windows.get(&id).map_or("", |ws| ws.preedit.as_str());
        // No "[active/total]" tab counter in the title: the visible tab strip already
        // shows the tabs, so a title-bar counter is redundant clutter (and macOS apps
        // like Ghostty/Terminal don't do it). The title is just the program/cwd title.
        // Temporary native-title owners form one explicit precedence stack:
        // close-warning > find status > canonical Smart Title. Keep tracking
        // `current_title` under either override, but suppress canonical writes so
        // neither temporary owner is erased by a live title/summary refresh.
        let (warning_armed, search_active) = self.windows.get(&id).map_or((false, false), |ws| {
            (
                ws.close_warning_until.is_some_and(|t| Instant::now() < t),
                ws.search.is_some(),
            )
        });
        let title_authority = window_title_authority(warning_armed, search_active);
        let title_changed = if preedit.is_empty() {
            // Common path (no active IME composition): compare the cached title
            // against the composed scratch directly, avoiding the per-frame
            // String allocation that an unconditional `format!`/`to_string` paid.
            let Some(ws) = self.windows.get_mut(&id) else {
                // Disjoint field, so restoring the slot here is fine even though
                // `self.windows` was just borrowed.
                self.title_compose_scratch = scratch;
                return;
            };
            if ws.current_title != scratch
                && !crate::toolbar::busy_spinner_phase_only_change(&ws.current_title, &scratch)
            {
                if title_authority == WindowTitleAuthority::Canonical {
                    window.set_title(&scratch);
                }
                ws.current_title.clear();
                ws.current_title.push_str(&scratch);
                true
            } else {
                false
            }
        } else {
            // Rare IME-preedit path: only here do we allocate the formatted title.
            let desired = format!("{scratch} [‹{preedit}›]");
            let Some(ws) = self.windows.get_mut(&id) else {
                self.title_compose_scratch = scratch;
                return;
            };
            if desired != ws.current_title
                && !crate::toolbar::busy_spinner_phase_only_change(&ws.current_title, &desired)
            {
                if title_authority == WindowTitleAuthority::Canonical {
                    window.set_title(&desired);
                }
                ws.current_title.clear();
                ws.current_title.push_str(&desired);
                true
            } else {
                false
            }
        };
        // Hand the warmed buffer back before any further `&mut self` work, so the
        // next frame's resolve/compose reuses this capacity.
        self.title_compose_scratch = scratch;
        // LIVE TAB TITLES: the native tab strip labels each tab with its session's title
        // (the cwd / running command the shell integration sets via OSC 0/2). That title
        // changes constantly (every `cd`, every command) but `refresh_window_tabs` only
        // ran on STRUCTURAL tab changes (`sync_window`), so the strip labels froze at
        // tab-creation time. Refresh the strip whenever the active tab's title changes
        // (it re-reads EVERY tab), so the tabs track the live cwd like Ghostty/iTerm.
        // Cheap + gated: only on a semantic title change (not another frame of the
        // conventional leading busy spinner), and a no-op off macOS / with no native
        // strip. A cwd-ONLY label drift (the titleless cwd-as-default-label fallback —
        // `tab_titles`) never trips this gate; the `Wake::Output` title-epoch path owns
        // that refresh.
        if title_changed {
            self.refresh_window_tabs(id);
        }
    }

    /// Publish an OSC-derived title only when the canonical active tab is a
    /// terminal. Native presentations own OS chrome while active, but they must
    /// never rename the parked terminal in `SessionStore` (which is also the
    /// control-socket identity visible to other processes).
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

    /// Reflect window `wid`'s focused-session ConEmu taskbar progress (OSC 9;4,
    /// parsed by the engine into [`aterm_core::terminal::Terminal::taskbar_progress`])
    /// onto its Windows taskbar button via `ITaskbarList3::SetProgressState` /
    /// `SetProgressValue` (see [`taskbar`]). The optional front-terminal capability
    /// follows tab/pane focus, so this naturally follows the active terminal tab;
    /// [`TaskbarProgress::Hidden`] (and no window/HWND) clears the button. Change-gated
    /// inside [`taskbar::set_progress`], so the per-frame call is a cheap map lookup
    /// until the state actually changes. Windows-only; the caller is `cfg`-gated.
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
    }
}

/// Hand-rolled COM FFI for the Windows taskbar progress indicator (`ITaskbarList3`),
/// the surface behind ConEmu's `OSC 9;4` progress protocol (winget / npm / PowerShell
/// `Write-Progress` emit it; the engine parses it into `Terminal::taskbar_progress`).
/// Same tiny-FFI house style as [`super::win32`]: direct `ole32` declarations and a
/// by-hand vtable, no new dependency (ole32 is a system DLL). The single COM object is
/// created lazily on the UI/STA thread, cached in a `thread_local`, and reused for
/// every window (the HWND is a per-call argument); it is deliberately leaked for the
/// process's life (a cheap shell singleton). Each window's last-applied state is cached
/// so the per-frame call only reaches COM on an actual change.
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

    /// `CLSID_TaskbarList` {56FDF344-FD6D-11D0-958A-006097C9A090}.
    const CLSID_TASKBAR_LIST: Guid = Guid {
        data1: 0x56FD_F344,
        data2: 0xFD6D,
        data3: 0x11D0,
        data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
    };
    /// `IID_ITaskbarList3` {EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF}.
    const IID_ITASKBAR_LIST3: Guid = Guid {
        data1: 0xEA1A_FB91,
        data2: 0x9E28,
        data3: 0x4B86,
        data4: [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF],
    };

    const CLSCTX_INPROC_SERVER: u32 = 0x1;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;

    // TBPFLAG (SetProgressState).
    const TBPF_NOPROGRESS: u32 = 0x0;
    const TBPF_INDETERMINATE: u32 = 0x1;
    const TBPF_NORMAL: u32 = 0x2;
    const TBPF_ERROR: u32 = 0x4;
    const TBPF_PAUSED: u32 = 0x8;

    /// The `ITaskbarList3` vtable, laid out in COM inheritance order (IUnknown →
    /// ITaskbarList → ITaskbarList2 → ITaskbarList3). Only the entries up to
    /// `set_progress_state` are declared — the later methods are unused, and we only
    /// ever index the pointers we name, so truncating the tail is sound. The unused
    /// leading slots MUST remain for correct offsets of the ones we call.
    #[repr(C)]
    #[allow(dead_code)] // layout-only slots: their offsets are load-bearing, not their use
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
        /// The COM object, created lazily on THIS (UI/STA) thread. A raw pointer, so it
        /// stays on the thread it was made on — exactly what `thread_local` guarantees.
        obj: Option<*mut ITaskbarList3>,
        /// Set once creation has been attempted, so a failure (no shell / denied) does
        /// not re-run `CoCreateInstance` every frame.
        tried_init: bool,
        /// Per-HWND last-applied `(flag<<32 | value)`, so the per-frame call reaches COM
        /// only on a real change. Evicted on window close via [`forget`].
        applied: HashMap<isize, u64>,
    }

    thread_local! {
        static CACHE: RefCell<Cache> =
            RefCell::new(Cache { obj: None, tried_init: false, applied: HashMap::new() });
    }

    impl Cache {
        /// The cached `ITaskbarList3`, creating it on first use. `None` if COM object
        /// creation or `HrInit` failed (and then never retried).
        fn object(&mut self) -> Option<*mut ITaskbarList3> {
            if let Some(o) = self.obj {
                return Some(o);
            }
            if self.tried_init {
                return None;
            }
            self.tried_init = true;
            // SAFETY: standard COM bring-up. `CoInitializeEx` is idempotent — winit's
            // `OleInitialize` on this thread usually ran already, and S_FALSE /
            // RPC_E_CHANGED_MODE are non-fatal, so the HRESULT is intentionally ignored.
            // `CoCreateInstance` writes the interface pointer into `ppv`; a failed HRESULT
            // or null pointer is rejected before any use. `HrInit` must precede any
            // progress call; on failure the object is released and dropped.
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

    /// Map an engine [`TaskbarProgress`] (or `None` = nothing set) to a `(TBPFLAG,
    /// completed/100)` pair. `Hidden`/`None`/`Indeterminate` carry no determinate value.
    fn encode(progress: Option<TaskbarProgress>) -> (u32, u64) {
        match progress {
            None | Some(TaskbarProgress::Hidden) => (TBPF_NOPROGRESS, 0),
            Some(TaskbarProgress::Indeterminate) => (TBPF_INDETERMINATE, 0),
            Some(TaskbarProgress::Normal(v)) => (TBPF_NORMAL, u64::from(v)),
            Some(TaskbarProgress::Error(v)) => (TBPF_ERROR, u64::from(v)),
            Some(TaskbarProgress::Paused(v)) => (TBPF_PAUSED, u64::from(v)),
            // `TaskbarProgress` is `#[non_exhaustive]`: a future state we don't model
            // maps to "no progress" (clear the button) rather than a wrong indicator.
            Some(_) => (TBPF_NOPROGRESS, 0),
        }
    }

    /// Apply `progress` to the taskbar button of `hwnd`. Change-gated per HWND: a call
    /// whose state matches the last one for this window is a pure map lookup.
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
            // SAFETY: `obj` is a live `ITaskbarList3` created on this thread; `hwnd` is a
            // live winit window handle (0 was rejected by the caller). Both calls are
            // synchronous and take only integers. State is set first, then (for a
            // determinate flag) the value, which the taskbar draws against total 100.
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

    /// Drop the change-detection cache entry for a closing window's `hwnd`, so a future
    /// window that reuses the same HWND value is not falsely skipped. The taskbar button
    /// itself disappears with the window, so no explicit clear is needed.
    pub(crate) fn forget(hwnd: isize) {
        CACHE.with(|c| {
            c.borrow_mut().applied.remove(&hwnd);
        });
    }
}

/// HiDPI auto-scale target for [`App::attach_os_window`]: the `font_px` the
/// default font size should render at on a display of `scale`, or `None` when the
/// auto-scale does not apply (an EXPLICIT `$ATERM_FONT_PX`/`config.font_px` is
/// honored verbatim, and a 1× display keeps the built-in default). Pure and
/// deterministic: the same `scale` always yields a bit-identical `f32`, so a
/// caller that stored a previous target into `font_px` can compare with `==` to
/// detect "backend already at the target size" and skip the (expensive,
/// first-pixel-blocking) font rebuild.
fn hidpi_target_font_px(font_px_explicit: bool, scale: f64) -> Option<f32> {
    (!font_px_explicit && scale > 1.0).then(|| {
        (FONT_PX * scale as f32)
            .round()
            .clamp(FONT_PX_MIN, FONT_PX_MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1440×900-point work area whose origin is NOT (0, 0) — the menu-bar band on
    /// the main display, and the general case on a secondary one. Placements that
    /// silently assume a zero origin fail against this.
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
        // Both offsets are strictly positive, so the parent keeps a visible strip
        // along its top AND its left edge — not just a sliver on one axis.
        assert!(placed.0 > anchor.0 && placed.1 > anchor.1);
    }

    #[test]
    fn the_cascade_wraps_per_axis_instead_of_marching_off_screen() {
        let window = (900.0, 600.0);
        // Bottom edge reached: y returns to the top of the work area, x keeps its
        // column so the run continues rightwards rather than restarting in place.
        let low = (300.0, 720.0);
        let placed = cascade_top_left_pts(low, CASCADE_STEP_PTS, window, SCREEN)
            .expect("ordinary geometry places the window");
        assert_eq!(placed, (low.0 + CASCADE_STEP_PTS, SCREEN.y));

        // Right edge reached: x returns to the left of the work area.
        let far_right = (1260.0, 100.0);
        let placed = cascade_top_left_pts(far_right, CASCADE_STEP_PTS, window, SCREEN)
            .expect("ordinary geometry places the window");
        assert_eq!(placed, (SCREEN.x, far_right.1 + CASCADE_STEP_PTS));

        // The bottom-right corner wraps both axes at once, back to the origin.
        let corner = (1260.0, 720.0);
        assert_eq!(
            cascade_top_left_pts(corner, CASCADE_STEP_PTS, window, SCREEN),
            Some((SCREEN.x, SCREEN.y))
        );
    }

    #[test]
    fn a_screen_filling_parent_is_still_stepped_off_not_wrapped_back_onto() {
        // The zoomed/maximized case, and the reason the wrap tests the grabbable
        // corner rather than the whole frame: the new window inherits the front
        // window's grid, so parent and child are the same size. Wrapping on "the
        // frame no longer fits" would put the child back at the work-area origin —
        // the exact eclipse this placement exists to prevent.
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

        // A window larger than the whole work area still gets a reachable top-left
        // (never pushed up under the menu bar), even though it cannot fit.
        let huge = (2000.0, 1400.0);
        let placed = cascade_top_left_pts((100.0, 100.0), CASCADE_STEP_PTS, huge, SCREEN)
            .expect("an oversized window is still placed");
        assert!(inside(placed, huge), "{placed:?} left the usable screen");

        // An anchor already off the work area (a window dragged nearly off-screen,
        // or left behind by a display change) is pulled back rather than followed.
        let window = (900.0, 600.0);
        let placed = cascade_top_left_pts((5000.0, 5000.0), CASCADE_STEP_PTS, window, SCREEN)
            .expect("an off-screen anchor is still placed");
        assert!(inside(placed, window), "{placed:?} followed the anchor off");

        // A frame narrower than the grabbable minimum is kept WHOLE on screen — the
        // minimum must never demand more than the window actually is.
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
        // The new window is frontmost — and may already have taken OS focus — before
        // its surface attaches. Anchoring on itself would place it nowhere.
        assert_eq!(cascade_anchor_of(new, &[a, b, new], &attached), Some(b));
        // No focus history at all (a restore loop): the newest OTHER window.
        assert_eq!(cascade_anchor_of(new, &[], &attached), Some(b));
        // A remembered window that is gone or never got a surface is skipped.
        assert_eq!(cascade_anchor_of(new, &[b, a], &[a, new]), Some(a));
        // The first window on glass has nothing to avoid.
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
        // `pending_join == false` used to suppress this publication candidate.
        // Its absence from this seam is the regression pin: an early Wake may
        // finalize the backend before `resumed`, but the first surface still owns
        // a complete attach timeline.
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

    #[test]
    fn window_created_after_config_asset_publication_inherits_custom_art() {
        let mut app = App::headless_for_test();
        let rgba: Arc<[u8]> = Arc::from([0xff, 0x22, 0xaa, 0xff]);
        let assets = Arc::new(crate::app_config::ConfigAssetCatalog {
            trail_packs: crate::app_config::TrailPackCatalog::empty(),
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
            app.windows[&wid].word_decos.has_custom_kitty_sprite(),
            "a post-publication window must not silently fall back to built-in art"
        );
        assert!(Arc::ptr_eq(
            app.windows[&wid].word_decos.kitty_sprite_rgba().unwrap(),
            &rgba
        ));
    }

    /// An explicit font size is never auto-scaled — on ANY display.
    #[test]
    fn explicit_font_px_never_scaled() {
        assert_eq!(hidpi_target_font_px(true, 1.0), None);
        assert_eq!(hidpi_target_font_px(true, 2.0), None);
        assert_eq!(hidpi_target_font_px(true, 3.0), None);
    }

    /// A 1× (or sub-1×) display keeps the built-in default untouched.
    #[test]
    fn one_x_display_keeps_default() {
        assert_eq!(hidpi_target_font_px(false, 1.0), None);
        assert_eq!(hidpi_target_font_px(false, 0.75), None);
    }

    /// The Retina target is `round(FONT_PX × scale)` clamped to the zoom bounds.
    #[test]
    fn retina_target_is_rounded_and_clamped() {
        assert_eq!(
            hidpi_target_font_px(false, 2.0),
            Some((FONT_PX * 2.0).round())
        );
        // Fractional (Linux-style 1.25×) scale rounds to a whole px.
        let frac = hidpi_target_font_px(false, 1.25).unwrap();
        assert_eq!(frac, frac.round());
        // An absurd scale clamps at the zoom ceiling rather than overflowing it.
        assert_eq!(hidpi_target_font_px(false, 1000.0), Some(FONT_PX_MAX));
    }

    /// The skip contract in `attach_os_window`: a 2nd window opening on the SAME
    /// display must compute a bit-identical target, so `scaled == self.font_px`
    /// (stored by the 1st window's rebuild) detects the already-correct backend
    /// and skips the redundant font re-read/re-parse. A DIFFERENT-scale display
    /// must yield a different target so the rebuild still fires there.
    #[test]
    fn same_display_target_is_bit_stable_across_windows() {
        let first = hidpi_target_font_px(false, 2.0).unwrap();
        let second = hidpi_target_font_px(false, 2.0).unwrap();
        assert!(second == first, "same scale must be bit-identical");
        let other_display = hidpi_target_font_px(false, 1.5).unwrap();
        assert!(
            other_display != first,
            "different scale must still trigger a rebuild"
        );
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
        // The resolved title is written into a caller-owned slot (the per-frame
        // path reuses one buffer); identity/description are still returned.
        let mut slot = String::new();

        // With neither OSC title nor cwd, native chrome uses the same final
        // presentation fallback as the tab label.
        assert_eq!(
            app.window_title_identity(wid, "", &mut slot),
            (Some(0), None)
        );
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
        let (session_id, description) = app.window_title_identity(wid, "", &mut slot);
        assert_eq!(session_id, Some(0));
        assert_eq!(slot, "/aterm-proof/window-title");
        assert_eq!(
            description.as_deref(),
            Some("Runs the focused release checks")
        );
        assert_eq!(
            app.title_summaries.compose(
                session_id,
                &slot,
                description.as_deref(),
                app.config.window_title_format_or_default(),
                &app.config,
                " — ",
            ),
            "/aterm-proof/window-title — Runs the focused release checks",
            "OSC-7 cwd remains the stable title while Description stays distinct"
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

        // Animation-only churn must not take the process-wide store write lock or
        // publish another externally-visible title.
        app.publish_active_terminal_title(wid, "⠙ aterm");
        app.publish_active_terminal_title(wid, "⠹ aterm");
        assert_eq!(stored_title(&app), Some("⠋ aterm".to_string()));

        // A changed semantic suffix and the settled, non-spinner title both remain
        // observable immediately.
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

    /// Arm window 0's initial-frame settle with a far deadline and `budget`
    /// corrections, returning the intended frame for the given grid.
    #[cfg(target_os = "linux")]
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
        app.window_frame_px(rows, cols)
    }

    #[cfg(target_os = "linux")]
    fn settle_state(app: &App) -> Option<crate::InitialFrameSettle> {
        app.windows
            .get(&WindowId(0))
            .and_then(|ws| ws.initial_frame_settle)
    }

    /// The GNOME shape: the first configure lands the intended size (settle
    /// must WAIT, not disarm — the lossy echo is still in flight), the
    /// activation echo then hands back a CSD-header-short frame (settle spends
    /// a correction), and the corrected size finally lands (settle disarms).
    #[test]
    #[cfg(target_os = "linux")]
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
        assert_eq!(s.corrections, 2, "the mismatch spends exactly one correction");

        app.settle_initial_frame(wid, intended);
        assert!(
            settle_state(&app).is_none(),
            "intended size after a correction = settled: disarm"
        );
    }

    /// An exhausted correction budget disarms WITHOUT another request — the
    /// compositor that keeps insisting (tiling, an early maximize) wins.
    #[test]
    #[cfg(target_os = "linux")]
    fn initial_frame_settle_budget_bounds_the_fight() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 1);
        let insisted = PhysicalSize::new(1920, 1080);

        app.settle_initial_frame(wid, insisted);
        assert_eq!(settle_state(&app).expect("one correction spent").corrections, 0);

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

    /// Past the deadline the settle disarms untouched — whatever the window is
    /// by then is the user's truth, never re-asserted.
    #[test]
    #[cfg(target_os = "linux")]
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
            .deadline = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(2));

        app.settle_initial_frame(wid, PhysicalSize::new(intended.width, intended.height - 35));
        assert!(
            settle_state(&app).is_none(),
            "expiry disarms instead of spending a correction"
        );
    }

    /// A control-socket grid resize OWNS the geometry: it disarms the settle so
    /// the attach grid is never re-asserted over a driven size.
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
}
