// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Window lifecycle: logical and OS window create/attach/close/focus, the
//! `front`/`front_mut` accessors and `apply_title`. Native chrome is reached
//! through the platform [`crate::platform::AppRt`] seam. An inherent-impl split
//! of `App`.

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

/// A screen's usable placement rectangle in logical points, top-left origin, y
/// down (the space of [`Window::outer_position`] in logical units). On macOS it
/// excludes the menu bar and Dock ([`AppRt::window_work_area_pts`]); elsewhere it
/// is the full monitor rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkAreaPts {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
}

/// Offset down-right from its parent at which a new window is placed, in logical
/// points. Clears a standard titlebar so the parent's frame stays graspable;
/// about AppKit's `cascadeTopLeftFromPoint:` step.
const CASCADE_STEP_PTS: f64 = 26.0;

/// The smallest slice of a cascaded frame that must stay inside the work area, in
/// points: enough of its top-left corner to see and grab it.
///
/// A cascaded window may hang past the right and bottom edges, as on macOS;
/// wrapping when the whole frame no longer fits would put a work-area-sized
/// window back at the origin, on top of its parent.
const CASCADE_MIN_VISIBLE_PTS: f64 = 160.0;

/// How long after an attach [`crate::InitialFrameSettle`] may still correct the
/// frame: covers the map, first configure burst and trailing activation
/// configure; after it, the compositor's size is the user's.
#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_WINDOW: std::time::Duration = std::time::Duration::from_millis(2000);
/// Corrective size requests one attach may spend (stale-echo + scale-change can
/// each need one; see [`crate::InitialFrameSettle::corrections`]).
#[cfg(target_os = "linux")]
const INITIAL_FRAME_SETTLE_MAX_CORRECTIONS: u8 = 3;

/// Where a freshly-created window goes so it does not eclipse `anchor` (the outer
/// top-left, in points, of the window it was opened from).
///
/// A diagonal step down-right, wrapped per axis back to the work area's edge once
/// the grabbable corner ([`CASCADE_MIN_VISIBLE_PTS`]) would leave it, like
/// AppKit's cascade. The final clamp handles an anchor that is itself off the
/// work area.
///
/// `None` for degenerate geometry (non-finite coordinate, negative frame, empty
/// work area); the caller then leaves the OS placement alone.
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

/// One live monitor's rectangle in physical px (global desktop coordinates, the
/// space of `outer_position`/`set_outer_position` and persisted `outer_x`/`outer_y`)
/// plus its scale factor, to convert [`CASCADE_MIN_VISIBLE_PTS`] per monitor.
/// Physical because mixed-DPI logical rectangles overlap and gap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MonitorRectPx {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) w: f64,
    pub(crate) h: f64,
    pub(crate) scale: f64,
}

/// Whether a persisted window origin still lands on an existing monitor.
///
/// Restore overrides the cascade, so it runs the cascade clamp's test itself:
/// the origin is accepted when some monitor contains the top-left corner with at
/// least [`CASCADE_MIN_VISIBLE_PTS`] (in px at that monitor's scale) to its right
/// and below. A rejected origin falls back to the cascade; clamping instead would
/// invent a position that can land on top of another window.
///
/// Degenerate rectangles (0×0, non-finite) are skipped rather than matched.
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
        // Corner inside the monitor with a grabbable slice to its right and below;
        // a monitor smaller than the grab minimum accepts any corner inside it.
        let keep_x = keep.min(m.w);
        let keep_y = keep.min(m.h);
        origin.0 >= m.x
            && origin.0 <= m.x + m.w - keep_x
            && origin.1 >= m.y
            && origin.1 <= m.y + m.h - keep_y
    })
}

/// [`restored_origin_visible`] against the live monitor set; `true` means apply
/// the persisted coordinate. An empty monitor enumeration cannot validate
/// anything, so it answers `true`.
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

/// A restored window's FRAME, as its layout row carried it — an update's (P6
/// finding 4) or a cold restore's: the grid the window is created at, and the
/// raw inner size it had, which its first attach takes when that size holds the
/// grid ([`App::attach_frame_px`], [`crate::WindowState::take_carried_frame`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CarriedFrame {
    pub(crate) rows: u16,
    pub(crate) cols: u16,
    pub(crate) inner_px: PhysicalSize<u32>,
}

impl CarriedFrame {
    /// The frame `layout` carries, when it carries one: a row that names its
    /// window's raw inner size ([`crate::restore::WindowLayout::inner_px`]) —
    /// every row of an update's layout or a quit's, but a maximized window's
    /// and a quit's full-screen one — is recreated at its own grid, and at that
    /// size when it still holds the grid. Every other row — an older build's,
    /// those two — gives `None`, and the window inherits the front window's
    /// grid as it always has. The grid keeps the launch clamps window 0's does
    /// (`main_entry`); a clamped grid is one the carried size no longer holds,
    /// so it then comes up at that grid's exact frame.
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

/// Where a restored window goes before it is first shown (Windows): the outer
/// position its layout row persisted, validated against the live monitors when
/// the attach applies it, and whether it was maximized. Staged on the window
/// ([`App::stage_restore_placement`]) and taken by its first
/// [`App::attach_os_window`], which places the window while it is still hidden.
#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RestorePlacement {
    pub(crate) outer: Option<(i32, i32)>,
    pub(crate) maximized: bool,
}

#[cfg(windows)]
impl App {
    /// Stage window `wid`'s persisted place (`outer`, and `maximized` when
    /// `Some(true)`; `None` never forces a change) for its first attach, which
    /// applies it before the reveal. `true` when there was a place to stage: the
    /// caller then leaves it to the attach rather than moving the window on
    /// glass afterwards.
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
    /// Take a restored window's CARRIED FRAME before its first attach sizes
    /// it: the raw inner size it had — an update's parent's, or a cold
    /// restore's quit ([`App::attach_frame_px`] takes it when it holds the
    /// carried grid). Presence reserves no row (it owns none since the owner
    /// surfaces, 2026-10-16): an older parent's carried `presence_row` is read
    /// by nobody, so its size holds a grid one row shorter than this build's
    /// and is refused, and the window comes up at its grid's own frame — one
    /// re-grid, once, on the update off that build.
    pub(crate) fn take_carried_frame(&mut self, inner_px: Option<PhysicalSize<u32>>) {
        self.carried_inner_px = inner_px;
    }
}

/// Which existing window a new one cascades off, given the MRU focus stack and the
/// ids of windows that own an OS window (ascending, as `App.windows` is a
/// `BTreeMap`).
///
/// Most-recently-focused first; otherwise the highest attached id (the most
/// recently created). `new` is excluded because `frontmost_window` and
/// `focus_order` can already name the new window itself.
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
/// The temporary find status outranks the canonical Title/Description. The
/// canonical cache keeps advancing under the override so it is current when the
/// override leaves.
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

/// The text the OS shows for a window's canonical title (caption, taskbar,
/// Alt+Tab): `title` without a leading busy-spinner frame
/// ([`crate::chrome_label::without_spinner_frame`]). Phase-only title changes are
/// coalesced (`toolbar::busy_spinner_phase_only_change`), so the OS label would
/// otherwise hold one frame of the animation for as long as the agent works.
#[must_use]
pub(crate) fn os_window_title(title: &str) -> &str {
    crate::chrome_label::without_spinner_frame(title)
}

/// Push a session's cwd onto `slot` in `~`-abbreviated tab-label form; `false`
/// leaves `slot` untouched. Same result as [`crate::app_tabs::home_abbreviated`]
/// but in place, so the per-redraw title path does not allocate.
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

/// Decide the `attach_os_window` outcome. A stale or missing logical window fails
/// closed: no startup milestones, no present target. Every successful install
/// offers its timeline to the process-wide first-write slot, which picks the
/// startup surface.
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

/// How [`App::confirm_destructive_close`] answers a close that would exit a window
/// or the app.
///
/// The native confirm dialog is modal on the main thread, which a control-socket
/// verb is waiting on, so wire paths set a non-interactive policy for their close;
/// the dialog is only for Cmd-W, Cmd-Q, the close button and the strip's ✕.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum CloseConfirm {
    /// A human gesture: the native dialog, or the in-window confirm banner where
    /// there is none.
    #[default]
    Interactive,
    /// Proceed with no dialog, for a close that is already an explicit
    /// instruction: the operator's Stop row, and the replay of a close the human
    /// confirmed on the in-window banner ([`App::replay_confirmed_close`]).
    Programmatic,
    /// The `close` and `tab close` verbs: no dialog; an idle close proceeds and a
    /// close that would hang up a running job is refused: a busy last tab with
    /// [`WIRE_CLOSE_REFUSED`], a busy pane in a window that stays open with
    /// `app_tabs::WIRE_PANE_CLOSE_REFUSED`, a busy tab among several with
    /// [`WIRE_TAB_CLOSE_REFUSED`].
    WireRefuseBusy,
}

/// The wire refusal of a busy last-tab close, as the `close` catalog entry
/// promises. Both `close_session_by_id` and [`App::confirm_destructive_close`]
/// answer with it.
pub(crate) const WIRE_CLOSE_REFUSED: &str =
    "close refused (a running job armed the last-tab confirm)";

/// The wire refusal of a busy `tab close` aimed at a tab that is not its
/// window's last. Shares its opening words with `app_tabs::WIRE_PANE_CLOSE_REFUSED`
/// so a driver can match every busy refusal, but names the tab.
pub(crate) const WIRE_TAB_CLOSE_REFUSED: &str = "close refused (a running job in that tab)";

/// What the Quit barriers answer at a Quit edge ([`App::quit_barriers`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitBarrier {
    /// Nothing holds the Quit: the app may exit now.
    Ready,
    /// Documents are saving; the Quit stays armed and ends when they land.
    Saving,
    /// A barrier refused the Quit, and it is disarmed.
    Refused,
}

impl App {
    /// Coalesce a destructive request behind the live overlap handoff and ask its
    /// worker to abort without blocking. `true` is a hard barrier: the caller must
    /// not detach documents, windows or PTYs. The worker completes only after the
    /// child process group is reaped; `finish_update_handoff` then rolls back
    /// overlap and replays this intent.
    #[cfg(unix)]
    pub(crate) fn defer_pending_update_handoff_teardown(
        &mut self,
        intent: crate::DeferredHandoffTeardown,
    ) -> bool {
        if self.pending_update_handoff.is_none() {
            // A held successor with nothing parked holds no descriptor, so it is
            // no barrier and the intent proceeds now. The typed stand-down also
            // shuts the main-thread park gate, which a raw cancel poke would
            // leave armed.
            //
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
        // Commit revocation, not only a wake hint: some close events bypass
        // keyboard activity accounting and cancel can race a worker that already
        // emitted ProofReady. Advancing the epoch makes that proof inadmissible;
        // the proof reducer's teardown predicate stays fail-closed on saturation.
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

    /// No overlap handoff exists off unix, so nothing is ever deferred behind one.
    #[cfg(not(unix))]
    pub(crate) fn defer_pending_update_handoff_teardown(
        &mut self,
        _intent: crate::DeferredHandoffTeardown,
    ) -> bool {
        false
    }

    /// The frontmost logical window's state (immutable). Transitional — every
    /// caller is single-window today; later steps route by an explicit WindowId.
    pub(crate) fn front(&self) -> Option<&WindowState> {
        self.frontmost_window
            .and_then(|id| self.windows.get(&id))
            .map(Box::as_ref)
    }

    /// The window-chrome theme from config `window_theme`. `Auto` stays `Auto` so
    /// the platform follows the live system appearance; Light/Dark override.
    /// Independent of the terminal palette.
    pub(crate) fn window_theme_for_chrome(&self) -> crate::app_config::WindowTheme {
        self.window_theme
    }

    /// The cell-geometry fingerprint keying the per-scale cell-metrics cache
    /// (`restore::load_cell_metrics`/`store_cell_metrics`): the canonicalized font
    /// family (`font_family_request`) plus `line_height`. Not keyed on `font_px`
    /// (stored per entry and checked by the reader), padding/strip rows
    /// (recomputed at lookup) or bold/italic/fallback families (they never set
    /// the cell). A font file swapped under an unchanged name is caught by the
    /// post-join mismatch check in `attach_os_window`.
    ///
    /// An UNCONFIGURED font keys on the build's default primary identity where
    /// it has one (`bundled:Iosevka Term Semibold` on Windows,
    /// `bundled:JetBrains Mono` on Linux; empty on macOS, where the key is
    /// unchanged), so the release that changed that default reads as a cache
    /// miss rather than early-revealing the previous default's metrics — as
    /// does the line height, part of the key (`lh=1.1` on Windows since
    /// 2026-10-16).
    fn cell_metrics_font_key(&self) -> String {
        // The face the window draws, the theme's typography included: a theme
        // that names a family or a line height keys its own metrics.
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

    /// The resolved colour the warm-launch early reveal shows before the backend
    /// joins, and which the first frame then paints over it: the renderer theme
    /// background, 24-bit.
    ///
    /// Three consumers must agree: the Windows class brush
    /// (`window_set_background_color`), every unstyled cell of the first frame
    /// (the engine projection `App::install_colors` installs), and the bands around the grid
    /// (`present_band_bg`, which uses the theme until OSC 11 sets one).
    /// `early_reveal_backdrop_is_the_colour_the_first_frame_paints` pins this;
    /// `attach_os_window` checks it per launch. It is not a persisted "last
    /// background", which would be stale after OSC 11 or a theme change.
    ///
    /// On an HDR desktop the 8-bit GDI erase and the scRGB `Rgba16Float` swapchain
    /// compose to the same linear value (DWM scales the sRGB surface by
    /// `SDRwhite / 80`, as the present blit does). GDI `BitBlt`/`CopyFromScreen`
    /// captures misreport float surfaces; measure with an FP16 capture or
    /// `aterm-ctl video`.
    fn early_reveal_backdrop(&self) -> u32 {
        self.theme.bg & 0x00FF_FFFF
    }

    /// The chrome appearance handed to
    /// [`crate::platform::AppRt::window_set_appearance`].
    ///
    /// macOS/Windows pass the policy through: `Auto` follows the OS appearance
    /// (`automatic_window_chrome_remains_system_owned_independent_of_terminal_palette`)
    /// and the titlebar matches the terminal because those platforms paint it the
    /// theme's background.
    ///
    /// Linux's window-system chrome (`_GTK_THEME_VARIANT` on X11; on Wayland the
    /// compositor's own frame, or aterm's header where it leaves the window
    /// undecorated, `crate::client_frame`) only has a light/dark variant, and
    /// winit's Wayland backend never delivers `ThemeChanged`. So on Linux `Auto` resolves to the active terminal
    /// theme's darkness ([`crate::tab_bar::theme_is_dark`]); explicit Light/Dark
    /// still override. `apply_theme_live` re-pushes it on Linux.
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

    /// The theme the window's chrome painters (tab band strip tones, native pages'
    /// role palette) draw from; the terminal grid always uses the terminal theme.
    ///
    ///  * `Auto` — the terminal theme (on Linux this matches what
    ///    [`Self::chrome_theme_for_apprt`] resolves the CSD to);
    ///  * `Light`/`Dark` matching the terminal theme's darkness — the terminal
    ///    theme;
    ///  * `Light`/`Dark` against it — the authored forced palette
    ///    ([`crate::native_appearance::forced_chrome_theme`]), accent carried from
    ///    the terminal theme.
    ///
    /// The same on every platform, so a forced Window appearance turns the
    /// Settings pages and the in-window chrome along with the title bar the OS
    /// paints ([`crate::platform::AppRt::window_set_appearance`]). The strip,
    /// the native pages and the Linux CSD classify the background with one
    /// predicate ([`crate::tab_bar::theme_is_dark`], also used by the role
    /// pipeline), so they always pick the same side.
    pub(crate) fn chrome_palette_theme(&self) -> aterm_render::Theme {
        crate::native_appearance::resolve_chrome_palette(self.window_theme, self.theme)
    }

    /// The palette native pages (Settings, documents) draw from: the chrome
    /// palette of the color theme itself, before the per-key color overrides,
    /// so a Foreground or Background picked for the terminal does not recolor
    /// the app's own pages. The terminal theme still reaches the pages' live
    /// previews.
    ///
    /// Read off the installed resolution ([`Self::install_colors`] is the one
    /// install path, for a config or catalog reload, an OS flip and a theme
    /// trial alike), so a paint costs a copy and the page and the terminal
    /// theme always come from the same install.
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

    /// Logical window creation (no winit): mint a fresh [`WindowId`], spawn a
    /// single-tab session at `rows`×`cols`, register it and install a fresh
    /// [`WindowState`] as frontmost. `None` if the spawn failed, in which case no
    /// window is minted. `create_window_internal_connected` wraps it with the
    /// surface attach.
    #[allow(
        clippy::too_many_arguments,
        reason = "the window's birth facts — geometry, cwd, the seamless adoptee, the \
                  connection lineage and the agent identity — are each a per-open datum; \
                  a wrapper struct would relocate the list, not simplify it"
    )]
    pub(crate) fn create_window_logical(
        &mut self,
        rows: u16,
        cols: u16,
        cwd_override: Option<&str>,
        adopt: Option<crate::spawn::Adopted>,
        // Session-connection birth facts, `(None, None)` for an ordinary open:
        // `lineage_parent` records the origin as the first session's registry
        // parent (a controlled window is a child of its origin), and `observe`
        // injects the controller hint env (`ATERM_OBSERVE_SESSION_ID`).
        lineage_parent: Option<aterm_session::SessionId>,
        observe: Option<&aterm_session::SessionId>,
        // The agent identity the first session spawns under (`spawn … place=window
        // identity=<name>`); `None` for ordinary opens and restores.
        identity: Option<&str>,
    ) -> Option<WindowId> {
        // Mint the window id FIRST so the spawned session's `Wake`s are stamped with
        // the window that will own them (Output/Exit/Bell route back to THIS window).
        let wid = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let sid = self.next_session_id;
        // A real run always has a proxy; only `headless_for_test` lacks one (and it
        // never calls this — it installs stub sessions directly). Guard, don't panic.
        let proxy = self.proxy.clone()?;
        // `cwd_override` (the persisted first-leaf cwd) wins; otherwise inherit the
        // frontmost window's focused pane cwd. `frontmost_window` moves to `wid`
        // only later, in `install_window_state_with_parent`.
        let cwd = cwd_override
            .map(str::to_owned)
            .or_else(|| self.frontmost_window.and_then(|w| self.focused_pane_cwd(w)));
        // A fresh shell under an identity with a theme of its own starts on that
        // theme's side (`COLORFGBG`); an adopted one keeps the env it has.
        #[cfg(windows)]
        let shell_factory =
            (adopt.is_none() && identity.is_some()).then(|| self.shell_factory(None, identity));
        #[cfg(windows)]
        let factory = shell_factory.as_ref().unwrap_or(&self.session_factory);
        #[cfg(not(windows))]
        let factory = &self.session_factory;
        let session = match spawn_session(
            sid,
            wid,
            rows,
            cols,
            // `wid` is not in `self.windows` yet, so this uses `win_cell_size`'s
            // unknown-window fallback (the shared backend); `apply_window_scale`'s
            // `sync_cell_pixel_size` corrects it after attach.
            self.spawn_cell_px(wid),
            factory,
            &proxy,
            cwd.as_deref(),
            observe,
            identity,
            None,  // no worker's task
            adopt, // SEAMLESS: `Some` re-adopts this window's handed-off first-leaf shell
        ) {
            Ok(s) => s,
            Err(e) => {
                // Spawn failed: do NOT mint a broken (session-less) window. The id is
                // burned (never reused), which is fine — ids are monotonic, not dense.
                crate::logging::stderr_line!("aterm-gui: could not open a new window: {e}");
                self.post_message(crate::message_reporters::new_window_failed(&e.to_string()));
                return None;
            }
        };
        self.next_session_id += 1;
        self.install_window_state_with_parent(wid, session, rows, cols, lineage_parent);
        Some(wid)
    }

    /// Install an already-spawned `session` as the sole tab of a fresh window `wid`
    /// and make it frontmost. Separate from the spawn so tests can drive the
    /// windows/pool/frontmost transitions with a stub `Session`.
    // Test-only wrapper with no lineage parent.
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

    /// `install_window_state` with an explicit registry lineage parent for the
    /// window's first session: a connected controlled window records
    /// `parent = origin`; every other window is a fresh root (`None`).
    pub(crate) fn install_window_state_with_parent(
        &mut self,
        wid: WindowId,
        session: Session,
        rows: u16,
        cols: u16,
        lineage_parent: Option<aterm_session::SessionId>,
    ) {
        let sid = session.id;
        // Clone the active-tab mirror Arcs before the pool takes the session.
        let (term, master, sink, ui_waiting) = (
            session.term.clone(),
            session.master,
            session.ctx.sink.clone(),
            session.ctx.ui_waiting.clone(),
        );
        // Register in the process-wide registry. A new window's first tab has no
        // parent unless a connected spawn named its origin. Inert test sessions
        // (`master == -1`) are not registered.
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
        // The new window becomes frontmost, and the global control/notify handle
        // follows it here, in the same seam that moves the front, so the control
        // socket never keeps driving the previous window's session
        // (`active_handle_conformance`).
        self.frontmost_window = Some(wid);
        self.sync_active_session();
        // The status-item glance lists windows now, so a create moves it
        // (fingerprint-gated; runs after every structural mutation above).
        self.mark_status_dirty();
        debug_assert!(
            self.structural_invariants_ok(),
            "window/session structural invariants violated after create_window_logical: {}",
            self.structural_invariant_violation().unwrap_or_default(),
        );
    }

    /// Test-only window creation with the same id minting and
    /// `install_window_state` bookkeeping as [`Self::create_window_logical`], but
    /// taking a stub `Session` instead of spawning a PTY. `session.id` must equal
    /// `self.next_session_id`. Returns the new, strictly increasing id.
    ///
    /// Refines `WindowRouting.CreateWindow`: mint the next id, bump `win_count`,
    /// re-point `frontmost`. Projection: `window_routing_conformance::project`
    /// (lib.rs) → `<<win_count, frontmost, next_id, exited>>`, with the +1 remap.
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

    /// Full window creation IN THE TURN, the restore's (one window per restore
    /// slice): the logical seam, its first session spawned here, plus, when not
    /// headless, the winit OS window attach. The new window inherits the front
    /// window's grid (80×24 if no window exists). `cwd_override` starts the first
    /// session in a given directory; `adopt` re-adopts a handed-off shell.
    /// `lineage_parent` and `observe` are a connected window's birth facts
    /// (`None` here: a connected window, like every window a person opens, is
    /// spawned off the loop, `App::open_window_off_loop`).
    ///
    /// `carried` is a restored window's frame ([`CarriedFrame`], an update's or
    /// a cold restore's): the window is created at its grid instead of the
    /// front window's, and first attached at its raw inner size when that holds
    /// the grid. `None` for every other open. `placement` is its row's persisted
    /// outer position and maximized state: Windows places the window there while
    /// it is still hidden ([`Self::stage_restore_placement`]); elsewhere the
    /// caller moves it after the attach.
    #[allow(
        clippy::too_many_arguments,
        reason = "the window's birth facts — cwd, the seamless adoptee, the connection \
                  lineage, the agent identity and an update's carried frame and place — \
                  are each a per-open datum, as in `create_window_logical`"
    )]
    pub(crate) fn create_window_internal_connected(
        &mut self,
        el: &ActiveEventLoop,
        cwd_override: Option<&str>,
        adopt: Option<crate::spawn::Adopted>,
        lineage_parent: Option<aterm_session::SessionId>,
        observe: Option<&aterm_session::SessionId>,
        identity: Option<&str>,
        carried: Option<CarriedFrame>,
        placement: ((Option<i32>, Option<i32>), Option<bool>),
    ) -> Option<WindowId> {
        let ((rows, cols), carried_inner_px) = self.new_window_frame(carried);
        let wid = self.create_window_logical(
            rows,
            cols,
            cwd_override,
            adopt,
            lineage_parent,
            observe,
            identity,
        )?;
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.take_carried_frame(carried_inner_px);
        }
        #[cfg(windows)]
        self.stage_restore_placement(wid, placement.0, placement.1);
        #[cfg(not(windows))]
        let _ = placement;
        if !self.headless && !self.attach_os_window(el, wid) {
            // GPU surface failed: roll back the just-created window + its fresh
            // session rather than leave a present-less black window.
            self.close_window_logical(wid);
            return None;
        }
        // Sync again now that the OS surface exists so the title lands
        // immediately. On attach failure `close_window_logical` already re-synced
        // the surviving front.
        self.sync_active_session();
        Some(wid)
    }

    /// The single join point for a windowed cold launch's deferred backend build:
    /// take the `Pending` handle, join it, and run the post-join setup (`metrics`
    /// backend tag, 1× pad seed, GPU bloom config, shaping and font-feature
    /// warnings). A `Ready` slot returns immediately.
    ///
    /// A backend-build panic loses the only renderer: exit with a one-line
    /// failure, never a half-attached window.
    pub(crate) fn finalize_backend(&mut self) {
        let BackendSlot::Pending(handle) = &mut self.backend else {
            return;
        };
        let handle = handle.take().expect("finalize_backend re-entered mid-join");
        let (backend, use_gpu, font_family_rejected) = handle.join().unwrap_or_else(|_| {
            crate::logging::stderr_line!(
                "aterm-gui: backend-build thread panicked; no renderer — exiting"
            );
            // Not `exit(3)`: the OS window and GPU driver are live, and running the
            // driver's static destructors under its own threads aborts. See
            // `crate::exit_without_process_teardown`.
            crate::exit_without_process_teardown(1);
        });
        assert!(
            backend.admitted_font_sources_sealed(),
            "backend worker published an unsealed font generation"
        );
        self.backend = BackendSlot::Ready(backend);
        self.use_gpu = use_gpu;
        // Warm miss: the worker built this backend at the launch hint's size
        // and the authenticated carry selected another. Switch now, before
        // anything measures a cell, via the light zoom path (`activate_px`).
        if let Some(px) = self.warm_miss_px.take() {
            self.backend.activate_px(px);
        }
        // The worker resolved the configured family. A family it rejected was
        // seeded optimistically in `main_entry` and must not stay the rebuild
        // source; `None` names the built-in candidates the renderer uses. The
        // refusal is reported here, at the join.
        if let Some(warning) = font_family_rejected {
            crate::say_font_family_rejected(&warning);
            self.font_family = None;
        }
        crate::metrics::set_backend_gpu(use_gpu);
        // Seed interior padding at 1× (config-resolved); `attach_os_window`
        // re-applies it at the window's real scale.
        self.backend.set_pad(self.cfg_pad_for_scale(1.0));
        // Chrome headroom starts at 0; `attach_os_window` applies the titlebar band.
        self.backend.set_head(0);
        // GPU cursor-comet bloom from config, set before the first present so the
        // per-window bloom target is built with the offscreen. GPU only.
        self.pin_gpu_effect_config();
        // The worker applied and sealed the font generation. Re-pin only
        // memory-backed shaping/typography/render knobs; reopening font paths here
        // would break the worker-only seal and delay first paint.
        self.pin_backend_render_config_core();
        // Once-only font-feature diagnostics — now that the backend carries the
        // shaping AND the resolved font config, so the font probe is accurate.
        self.warn_font_feature_issues();
        // Not `sync_chrome_fonts()`: it parses the face for the chrome rasterizer
        // (hundreds of ms) and only overlays opened by the user read it, so it runs
        // on the first-present hook in `app_render`, off time-to-glass.

        // The join is the first moment real font metrics exist. The boot session
        // was spawned before it with the engine's 8x16 placeholder, so push every
        // window's real cell box now; a shell's first OSC 1337 image or DEC 1016
        // report is then answered from the font. `apply_window_scale` re-pushes if
        // the display resolves another scale (the memo keys on the cell size).
        //
        // The XTWINOPS pixel reports (`CSI 14 t`, `CSI 16 t`) answer only from a
        // host-reported cell box and stay silent until one arrives. See
        // `Terminal::host_cell_pixel_size` and `handler_window`.
        let windows: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in windows {
            self.sync_cell_pixel_size(wid);
        }
    }

    /// Pin the GPU-only effect/swapchain knobs (bloom, shimmer, backdrop margins,
    /// EDR opt-in and its SDR twin) from config onto a freshly built GPU backend.
    /// Shared by [`Self::finalize_backend`] and
    /// [`Self::retry_attach_on_opaque_swapchain`]. No-op on the CPU backend.
    pub(crate) fn pin_gpu_effect_config(&mut self) {
        let gpu_post_fx = self
            .serious_mode_policy()
            .allows(crate::motion::SeriousEffect::GpuPostFx);
        // Per-style bloom radius (rainbow kitty 1.8, else 2.2; the knob
        // overrides), resolved before the `&mut` backend borrow.
        let bloom_radius = self
            .config
            .cursor_trail_bloom_radius_or_default(self.trail_presentation());
        if let Backend::Gpu(g) = self.backend.ready_mut() {
            g.set_bloom(gpu_post_fx && self.config.cursor_trail_bloom_or_default());
            g.set_bloom_params(
                self.config.cursor_trail_bloom_strength_or_default(),
                bloom_radius,
            );
            // Heat shimmer above burning cells (the bloom's parity class —
            // GPU only, like the bloom; the CPU path has no shimmer).
            g.set_shimmer(gpu_post_fx && self.config.cursor_fire_shimmer_or_default());
            // Windows Mica/Acrylic: mirror the material knob before any surface
            // exists so the first window is configured PreMultiplied at attach.
            // Inert unless the context was built on the DComp visual path.
            g.set_backdrop_margins(
                self.render_knobs.background_material
                    != crate::app_config::BackgroundMaterial::None,
            );
            // The EDR cursor glow opt-in (default off), set before any surface
            // exists so an opted-in first window gets the Rgba16Float swapchain.
            // After the backdrop mirror: on a visual instance the renderer forces
            // HDR off (the swapchains are exclusive) and logs it once.
            g.set_hdr_glow(self.config.hdr_glow_or_default());
            // SDR twin: the swapchain-side crown budget for non-HDR desktops.
            g.set_sdr_glow_boost(self.config.cursor_glow_sdr_boost_or_default());
        }
    }

    /// Windows fail-soft: a window's GPU swapchain attach failed with
    /// `first_error`. Unless [`aterm_gpu::surface_attach_fallback`] says
    /// `RebuildOpaque` (first window on a DirectComposition visual instance with
    /// DComp unavailable), return the error so the caller declines the window or
    /// downgrades to CPU. Otherwise withdraw the visual latch, build a GPU
    /// renderer on a fresh instance (presentation is instance-level), re-pin
    /// every knob through the join's seams, and retry the attach once. The
    /// window keeps `WS_EX_NOREDIRECTIONBITMAP`, harmless to a flip-model HWND
    /// swapchain. A failed rebuild leaves the backend untouched and returns
    /// `first_error`; a failed retry returns its own error with the opaque
    /// backend installed. The caller's CPU arm works either way.
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
        // This runs during the first attach, before anything is on glass and with
        // stderr possibly discarded: queue the message for the band and latch the
        // cause so `aterm-ctl chrome` reads `client=opaque(dcomp_unavailable)`.
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
        // Replacing the renderer destroys any virtual tap (none can be live at
        // a first attach, but the rebuild path's contract is kept whole).
        let _ = self.video_abort_backend_rebuild();
        // Carries the pad, top pad and head the attach applied at this window's
        // scale.
        self.backend
            .swap_renderer(Backend::Gpu(GpuBackend::new(rebuilt)));
        // `use_gpu` / the metrics backend tag are unchanged: GPU stays GPU.
        self.reset_window_raster_caches();
        // The deferred join's two knob seams, on the fresh renderer.
        self.pin_gpu_effect_config();
        self.pin_backend_render_config_core();
        self.backend
            .gpu_mut()
            .expect("the backend was just replaced with a GPU renderer")
            .create_window_surface(window.clone(), w_px, h_px)
    }

    /// The window a new window is offset from: the most recently focused other
    /// window that owns an OS window, else the highest-id other attached window.
    ///
    /// Not `frontmost_window`: creation re-points it at the new window before the
    /// surface attaches. `None` means this is the only window, so the OS
    /// placement stands.
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
    /// the real work area where measurable (macOS), else the winit monitor
    /// rectangle, which only makes the cascade wrap slightly later.
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

    /// Offset a freshly created 2nd..Nth window down-right of the window it was
    /// opened from, so that window stays visible behind it.
    ///
    /// The OS default placement (macOS: `NSWindow::center`) would put a same-size
    /// window exactly on top of its parent. Every new-window path goes through
    /// `attach_os_window`, so placing it here covers them all.
    ///
    /// Called after the frame size is known and before the surface exists, so the
    /// window never visibly hops. Best-effort: no other window, an unreadable
    /// position or degenerate geometry leave the OS placement. Callers with an
    /// explicit position (restore, update carry) apply it afterwards and win.
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
        // Points, not pixels: the windows can sit on displays with different
        // scale factors; only the logical space is common.
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
        // Coordinates and screen metrics only, never window content.
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

    /// The grid a new window is created at, and the raw inner size its first
    /// attach may size to ([`Self::attach_frame_px`]): a restored window's
    /// carried frame ([`CarriedFrame`]), else the front window's grid (or an
    /// 80×24 default if somehow no window exists) and no size.
    pub(crate) fn new_window_frame(
        &self,
        carried: Option<CarriedFrame>,
    ) -> ((u16, u16), Option<PhysicalSize<u32>>) {
        match carried {
            Some(frame) => ((frame.rows, frame.cols), Some(frame.inner_px)),
            None => (self.front().map_or((24, 80), |ws| (ws.rows, ws.cols)), None),
        }
    }

    /// The size window `wid` is attached at: the exact frame of its
    /// `rows`×`cols` grid, or `carried` — a restored window's raw inner size
    /// (`WindowState::carried_inner_px`: an update successor's window 0's
    /// from [`crate::session_store::WindowCarry::inner_px`], every further
    /// window's from [`crate::restore::WindowLayout::inner_px`], an update's
    /// or a cold restore's) — when that size holds exactly this grid at this
    /// window's metrics: the launch's font and the window's scale.
    ///
    /// A font zoom re-grids inside the window it has, so a zoomed parent's
    /// window is its grid's frame plus a remainder of less than one cell per
    /// axis. Sizing the successor to the bare frame dropped the remainder, and
    /// the zoom back out then fitted a row and a column fewer than it had
    /// (P6, finding 4). The check keeps the window's grid the carried one:
    /// a size outside `[frame, frame + one cell)` on either axis (another
    /// display scale, another chrome band, a cold launch at the configured
    /// font after a zoomed quit, a hostile manifest) is not the window's
    /// remainder, and the exact frame is used as before.
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

    /// Create the OS window and present surface for logical window `wid` and
    /// attach them to its [`WindowState`]; used for the first window (at `resumed`)
    /// and every later one (`create_window_internal_connected`, a window landing
    /// off the loop, a detached tab's). Sizes the OS window from the
    /// stored grid, installs the macOS menu (first window only) and builds the GPU
    /// or CPU present target. Never called headless. A stale `wid` is a no-op.
    /// Returns `true` iff the OS window was created and a present target installed.
    /// On failure the just-created OS window is dropped (never installed
    /// present-less) and the caller rolls back the logical window, or exits for
    /// the first. Failures decline only this window, never panic.
    #[must_use]
    pub(crate) fn attach_os_window(&mut self, el: &ActiveEventLoop, wid: WindowId) -> bool {
        let (rows, cols) = self
            .windows
            .get(&wid)
            .map_or((0, 0), |ws| (ws.rows, ws.cols));
        // First window while the backend build is in flight: create the OS window
        // before joining so the window-server round trip overlaps the GPU/font
        // build. It is created hidden at estimated cell metrics (`seed_cell_px`);
        // the join and the `request_inner_size` correction below fix the size
        // before it is shown. Later attaches see a Ready slot.
        let pending_join = self.backend.is_pending();
        // Startup drill-down: every successful install offers these stamps to a
        // process-wide OnceLock and the first wins. Not keyed to `pending_join`,
        // since an early proxy Wake may finalize the backend before this attach.
        let startup_attach_entry = Instant::now();
        // The window holds the grid, the tab-strip rows and configured top/bottom
        // padding (`window_frame_px_for`). Chrome headroom is 0 here; the
        // post-attach `set_head` and size recompute add it before first show.
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
        // Windows: point the window at the icon compiled into this exe. DWM reads
        // the window's icon via WM_GETICON / the class GCLP_HICON, and winit
        // registers its class with no icon, so without WM_SETICON the caption and
        // Alt-Tab show the generic exe glyph.
        //
        // Both icons are the .ico's frames of exactly SM_CXSMICON / SM_CXICON at
        // the system DPI (`win_icon`); a window that opens on a display of another
        // DPI, or moves to one, is sent that display's below and on
        // `ScaleFactorChanged`.
        //
        // Failure is silent: `from_resource` fails only when the build had no
        // resource compiler (build.rs already warned), and this is a GUI-subsystem
        // binary with no useful stderr.
        //
        // The class is aterm's own, `aterm` (the Linux `WM_CLASS` too), not winit's
        // shared "Window Class", so window managers, FancyZones rules, AutoHotkey
        // (`ahk_class aterm`) and accessibility tools can name aterm's windows.
        // Nothing looks windows up by the old name (the installer and uninstaller
        // enumerate by process id).
        #[cfg(windows)]
        let attrs = {
            use aterm_window::platform::windows::WindowAttributesExtWindows as _;
            let (small, big) = crate::win_icon::window_icons(crate::win_icon::system_dpi());
            let attrs = attrs
                .with_window_icon(small)
                .with_taskbar_icon(big)
                .with_class_name("aterm");
            // A window presented through the DirectComposition visual swapchain is
            // created without the GDI redirection bitmap
            // (`WS_EX_NOREDIRECTIONBITMAP`); DWM would composite it, filled with
            // the class brush, under the visual tree and hide the Mica/Acrylic
            // backdrop. Keyed to the request latch because the GPU context may
            // still be building; the build thread's failure arm withdraws it.
            // Scoped to the backdrop opt-in so default windows are unchanged.
            if aterm_gpu::visual_swapchain_requested() {
                attrs.with_no_redirection_bitmap(true)
            } else {
                attrs
            }
        };
        // Linux identity and minimum size. `with_name` is the Wayland `app_id` and
        // X11 `WM_CLASS`; it must equal the `aterm.desktop` basename for icon,
        // grouping and launcher matching, so keep it `"aterm"`. The minimum is
        // the smallest legal grid (20 cols × 5 rows at the 12 px base font plus
        // strip row and padding), in logical px.
        //
        // On X11 the window also carries the app icon (`_NET_WM_ICON`), which panels
        // and window switchers that read the window property show; Wayland has no
        // window icon, and its compositor takes the icon from the desktop entry the
        // app_id names. The first window presents the launcher's startup token
        // (`startup_token`), which ends the launch's busy cursor or spinner.
        #[cfg(target_os = "linux")]
        let attrs = {
            use aterm_window::platform::wayland::{
                ActiveEventLoopExtWayland as _, WindowAttributesExtWayland,
            };
            let wayland = el.is_wayland();
            let attrs = attrs
                .with_name("aterm", "aterm")
                .with_min_inner_size(aterm_window::dpi::LogicalSize::new(164.0, 98.0));
            let attrs = if wayland {
                attrs
            } else {
                attrs.with_window_icon(crate::desktop_files::window_icon())
            };
            match crate::startup_token::take_for_first_window(wayland) {
                Some(token) => attrs.with_activation_token(token),
                None => attrs,
            }
        };
        // Overlap handoff: every window of a handoff boot is created hidden and
        // revealed after its first real content present (`app_render`), so the
        // parked parent's frozen frame is never covered by a background-only
        // flash. `about_to_wait` force-reveals at a deadline.
        let defer_reveal = self.handoff_ready.is_some();
        // Deferred-join first window: created hidden, shown once the joined
        // backend's metrics have resized it. Windows creates EVERY window hidden
        // and shows it below once it has its size, place and chrome: a window
        // created visible appears at the OS default spot with the OS's own
        // caption (a light one on a light desktop) and is then resized,
        // cascaded and re-tinted on glass.
        let reveal_after_chrome = pending_join || cfg!(windows);
        let attrs = if reveal_after_chrome || defer_reveal {
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
                crate::logging::stderr_line!("aterm-gui: OS window creation failed: {e}");
                return false;
            }
        };
        let startup_after_window_create = Instant::now();
        // Opened on a display whose DPI is not the system's: that display's icons.
        #[cfg(windows)]
        crate::win_icon::fit_new_window(&window);
        // Meta mode's Option is an Alt from the window's first key event: without it
        // AppKit composes Option+a into "å" and starts a dead key on Option+e, which
        // the encoder never sees as Meta. A new view's Option composes, so the window
        // is told now and its record follows (`sync_option_as_alt` keeps both after).
        let option_is_alt = self.option_is_alt(wid);
        crate::platform::set_option_as_alt(&window, option_is_alt);
        if let Some(ws) = self.windows.get(&wid) {
            ws.option_as_alt.set(option_is_alt);
        }
        // Attach this real window's native accessibility adapter; its events arrive as
        // `Wake::Accessibility`.
        #[cfg(a11y_tree)]
        let a11y_adapter = self
            .proxy
            .clone()
            .and_then(|proxy| crate::a11y_adapter::Adapter::new(&window, proxy));
        // A non-deferred window with a Ready backend is on glass here (off
        // Windows). Offer the process-wide first-reveal stamp (first write wins).
        if !reveal_after_chrome && !defer_reveal {
            crate::metrics::mark_first_window_visible();
        }
        // Install the shared NSApp.mainMenu once, with the first window;
        // reinstalling would drop the retained action target. The target is kept
        // in `self` because AppKit holds menu targets weakly. No-op off macOS.
        if self._menu.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            // Its items and the toolbar's buttons mark what they post, so keys
            // typed before the loop dispatches it wait for it (`tab_spawn`).
            crate::tab_spawn::serve_native_menu_marks(&self.tab_spawns.menu);
            self._menu = self.apprt.install_menu(proxy);
            // Route AppKit `terminate:` quits that bypass the menu (Dock Quit,
            // AppleScript, logout) through the App lane's Cmd-Q confirmation and
            // document barrier. Registered once; no-op off macOS.
            self.apprt.install_quit_confirm();
        }
        // The menu-bar status item (status_item.rs), created once with the
        // first window and retained for the process life (releasing the
        // NSStatusItem removes it), its sibling rows seeded. No-op off macOS.
        if self._status_item.is_none()
            && let Some(proxy) = self.proxy.clone()
        {
            self.install_status_item(&proxy);
        }
        // Seed the OS "Reduce Motion" flag at attach and subscribe to its change
        // notification once (macOS). Windows and Linux (GNOME
        // `enable-animations`, `false` when unreadable) re-sample on each attach.
        if self._reduce_motion.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            self.system_reduce_motion = self.apprt.reduce_motion();
            crate::native_appearance::install_preferences(
                self.apprt.native_appearance_preferences(),
            );
            self._reduce_motion = self.apprt.observe_reduce_motion(proxy);
        }
        // Enable IME so the window receives `WindowEvent::Ime` for composition.
        window.set_ime_allowed(true);
        // Declare the field a terminal (Wayland `content_purpose.terminal`), so IMs
        // and on-screen keyboards drop autocapitalisation and prediction. Stored on
        // the window so every `text_input.enter` re-announces it. No-op on other
        // platforms.
        window.set_ime_purpose(aterm_window::window::ImePurpose::Terminal);
        // The desktop light/dark appearance: start its observer once, then
        // install what it reads now. It picks the side of a `dark:…,light:…`
        // `theme`, and the engine reports that palette's appearance to apps (DEC
        // 2031, DSR ?996n). Launch already built everything for the appearance
        // it read (`warm_config`), so this is a no-op unless the desktop flipped
        // since. Never the window's own theme off Windows: a forced
        // `window_theme` pins it on macOS, and Linux reports aterm's decoration.
        if self.os_appearance_observer.is_none()
            && let Some(proxy) = self.proxy.as_ref()
        {
            self.os_appearance_observer = self.os_appearance_source.observe(proxy);
        }
        if let Some(os_appearance) = self.os_appearance_source.at_attach(|| window.theme()) {
            self.sync_app_theme_to_appearance(os_appearance);
        }
        // Warm-launch early reveal (Windows only). A hidden top-level HWND has no
        // window or taskbar button, and the backend join takes a noticeable part of
        // startup there. When a previous run persisted this scale's real cell
        // metrics (`restore::load_cell_metrics`), the window is revealed at its
        // exact final size, position and theme before the join; content then
        // arrives with no geometry change (checked post-join below). The seeded
        // `seed_cell_px` size is only a heuristic, so without cached metrics the
        // window stays hidden. The titlebar band is 0 on Windows (the caption is
        // outside the client area).
        //
        // Not done on macOS: the join there is already negligible and the exact
        // resize has to precede the show on either path, so revealing earlier
        // gains nothing.
        //
        // Skipped (window stays hidden until ready) for:
        //   * cold launch — no persisted metrics for this scale/font;
        //   * overlap handoff (`defer_reveal`) — the parked parent's frame must
        //     not be covered;
        //   * seamless position carry — applied by `resumed` after this returns;
        //   * a maximized restore — the post-join resize works on a normal frame.
        // A restore with a plain persisted position is applied here, pre-reveal.
        #[cfg(windows)]
        let mut early_reveal_size: Option<PhysicalSize<u32>> = None;
        #[cfg(not(windows))]
        let early_reveal_size: Option<PhysicalSize<u32>> = None;
        // The colour the early reveal erased to, for the post-join colour check
        // (the geometry check's twin). `None` on every non-early path.
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
            // The same scale resolution the post-join sizing uses (force-scale
            // override included), so the two frame derivations cannot diverge.
            let scale = resolve_force_scale().unwrap_or_else(|| window.scale_factor());
            // Predict the font px the post-join block settles on (the size at this
            // scale); deterministic, so a cache hit means the joined backend
            // activates the cached size.
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
                // The exact frame from cached cell metrics, since the live backend
                // has not joined; drift is caught by the mismatch log below.
                let pad = self.cfg_pad_for_scale(scale);
                let pad_top = self.cfg_pad_top_for_scale(scale);
                // The measured OS band (0 on Windows) plus the synthetic tab band,
                // which is reserved from the post-join frame too; mispredicting it
                // would log a stale reveal and resize on glass.
                let head = (self.apprt.titlebar_band_pts(&window) * scale)
                    .round()
                    .max(0.0) as usize
                    + self.synthetic_strip_head_px(scale, cell_h);
                let total_rows = rows.saturating_add(self.chrome_rows(wid)) as usize;
                // The shared frame law (`frame_size_px` + `visible_frame_height`),
                // so this prediction and the post-join `window_frame_px_for` differ
                // only in where the cell box came from.
                let (width, raw_height) = aterm_render::frame_size_px(
                    total_rows,
                    cols as usize,
                    cell_w,
                    cell_h,
                    pad,
                    head,
                );
                let height = crate::visible_frame_height(raw_height, pad, pad_top);
                let exact = PhysicalSize::new(width as u32, height as u32);
                // Still hidden: the correction from the seeded size to the
                // exact frame is never on glass.
                let _ = window.request_inner_size(exact);
                // Bootstrap restore position, applied pre-reveal and validated
                // against the live monitors, so the window does not hop later.
                if let Some((x, y)) =
                    bootstrap_restore.and_then(|wl| Some((wl.outer_x?, wl.outer_y?)))
                    && restored_position_on_screen(&window, x, y)
                {
                    window.set_outer_position(aterm_window::dpi::PhysicalPosition::new(x, y));
                }
                // The backend-independent chrome calls, hoisted above the join
                // (re-applied idempotently after it): the themed class brush, so
                // the client area erases to the first frame's colour
                // (`early_reveal_backdrop`), and the caption dark-mode resolution.
                let backdrop = self.early_reveal_backdrop();
                // The caption's surface (the tab band), so it is tinted right
                // before the window is first on glass.
                #[cfg(windows)]
                crate::platform_win::note_caption_surface(&window, self.caption_surface_tone(wid));
                self.apprt.window_set_background_color(&window, backdrop);
                self.apprt
                    .window_set_appearance(&window, self.window_theme_for_chrome());
                // Reveal.
                window.set_visible(true);
                crate::metrics::mark_first_window_visible();
                // The event loop is about to block in the join, so paint the themed
                // erase synchronously; otherwise the window is unpainted until then.
                self.apprt.window_flush_backdrop(&window);
                early_reveal_size = Some(exact);
                early_reveal_bg = Some(backdrop);
            }
        }
        // The backend join: the OS window exists (its creation overlapped the
        // build); the HiDPI font derivation, pad, corrected size and present
        // target below need the real backend. A build panic exits inside
        // `finalize_backend`. On macOS the worker normally finishes before this
        // point, so the wait is negligible.
        let startup_before_backend_finalize = Instant::now();
        if pending_join {
            self.finalize_backend();
        }
        let startup_after_backend_finalize = Instant::now();
        // Extend the content view under the titlebar first (fullSizeContentView),
        // then measure the band via `contentLayoutRect`, which the style-mask
        // change recomputes synchronously. Measuring before the mask under-counts
        // the unified toolbar and costs a grid row. Stored in points so scale
        // changes recompute px headroom. No-ops off macOS.
        self.apprt.window_set_fullsize_content(&window);
        // Native macOS toolbar (toolbar.rs) with a "+" New Tab button (posts
        // `Wake::MenuAction { NewTab }`). Installed before the band measure, since
        // the toolbar grows `contentLayoutRect`'s band, and before the GPU/CPU
        // split so both backends get it. Backing objects live in
        // `self._toolbars` because AppKit holds the delegate and target weakly.
        // No-op off macOS. The proxy clone avoids a simultaneous borrow of `self`.
        if let Some(proxy) = self.proxy.clone()
            && let Some(handle) = self.apprt.install_toolbar(&window, &proxy, wid)
        {
            // Pin the strip appearance to the theme's darkness so tab label
            // colours resolve against the theme-coloured titlebar, not the OS
            // appearance. Re-synced on theme change.
            crate::toolbar::set_strip_dark(&handle, crate::tab_bar::theme_is_dark(self.theme.bg));
            // Pin the authored selected-tab colour and accent (theme or config)
            // at install too, so a fresh window's strip matches the live config.
            let (authored, strip_accent) = self.native_strip_chrome();
            crate::toolbar::set_active_tab_color(&handle, authored.active_tab);
            crate::toolbar::set_strip_accent(&handle, strip_accent);
            self._toolbars.insert(wid, handle);
        }
        // Measure the band after the mask and toolbar: `contentLayoutRect` now
        // covers the full chrome above the content.
        let head_pts = self.apprt.titlebar_band_pts(&window);
        // Seed the last-good windowed band memory from this sample (via the
        // decision law), so a fullscreen entered before any windowed resize has a
        // value to restore. The applied `head_pts` stays the raw measurement.
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
        // HiDPI scale. Glyphs are rasterized at `font_px` device pixels, so the
        // font size (px at 1×, whatever named it) draws at `round(size × scale)`
        // once the window's scale is known ([`crate::font_px_for_scale`]).
        //
        // The size is selected on the shared renderer via the light `activate_px`,
        // not a rebuild, so other windows keep their glyph atlases; this is the
        // same mechanism `apply_window_scale` uses per frame. `--scale` overrides
        // the window's scale factor for both font and padding.
        let scale = resolve_force_scale().unwrap_or_else(|| window.scale_factor());
        // Record this window's scale so `redraw_window` re-selects the shared
        // backend to it per frame.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.scale = scale;
        }
        // A zoom an older outgoing build carried in device px becomes sizes at
        // this, the first, window's scale.
        if let Some((px, reset)) = self.font_size_from_device.take() {
            self.font_size = font_size_of_device_px(px, scale);
            self.default_font_size = font_size_of_device_px(reset, scale);
        }
        let scaled = self.attach_font_px(scale);
        // Switch only on a real change. Faces and shaping config are unchanged
        // by a size switch.
        if (scaled - self.font_px).abs() >= 0.5 {
            self.backend.activate_px(scaled);
        }
        self.font_px = scaled;
        // Apply the padding at the real scale and recompute `size` so the window
        // and its swapchain fit grid, border, new cell metrics and tab strip.
        let pad = self.cfg_pad_for_scale(scale);
        self.backend.set_pad(pad);
        // Tighter top pad (the chrome band already separates the grid from the
        // window top); the bottom keeps the base pad. Applied after `set_pad`,
        // since a real base-pad change starts a new symmetric padding regime.
        let pad_top = self.cfg_pad_top_for_scale(scale);
        self.backend.set_pad_top(pad_top);
        // Windows/Linux: replace the measured band (0) with the synthetic tab band.
        // Derived here because its residue depends on `cell_h`, which is settled
        // only after the size activation above. Not compiled on macOS, whose
        // measured band must stand; no-op when the strip is off or `compact`.
        #[cfg(any(windows, target_os = "linux"))]
        {
            let cell_h = self.backend.cell_geometry(self.font_px).1;
            let synthetic = self.synthetic_strip_head_px(scale, cell_h);
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.head_pts = synthetic as f64 / if scale > 0.0 { scale } else { 1.0 };
            }
        }
        // FIRE-INTO-CHROME: the titlebar band in device px.
        let head_pts = self.windows.get(&wid).map_or(0.0, |ws| ws.head_pts);
        // …plus the stats Deck under it (`crate::stats_deck`), on by default.
        let head = crate::stats_deck::chrome_head_px(head_pts, scale, self.stats_bars_form());
        self.backend.set_head(head);
        // Record this window's resolved metrics from its own scale (kept current by
        // `on_scale_factor_changed`), using the live `font_px`.
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.metrics = crate::MetricsView::applied(self.font_px, pad, pad_top, head);
        }
        // A restored window (an update successor's, or a cold restore's) comes
        // up at the raw inner size it had, not just its grid's frame, when that
        // size holds the grid (P6 finding 4).
        let carried = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.carried_inner_px.take());
        size = self.attach_frame_px(wid, rows, cols, carried);
        let _ = window.request_inner_size(size);
        // The warm-launch check: the frame revealed before the join must equal the
        // one the joined backend derived, so the `request_inner_size` above was a
        // no-op. A mismatch (e.g. a font file changed under an unchanged
        // fingerprint) is the early path's visible failure mode, so it logs; the
        // store below re-persists the corrected metrics.
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
        // The colour check: the theme background resolved now must equal the one
        // the pre-join brush was seeded from; a mismatch would be a visible colour
        // step at content arrival, so it logs at warn.
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
        // Linux initial-frame settle: on Wayland the request above is pre-map (no
        // CSD frame, `scale_factor()` possibly 1.0), so the first configure can
        // return a frame that is not `rows`×`cols`. Arm one bounded post-map
        // re-request of this grid; see [`crate::InitialFrameSettle`] and
        // [`Self::settle_initial_frame`].
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
        // Hint the WM to resize in whole-cell steps so edge drags land on an exact
        // grid fit. macOS honours it only during live resize; other sizes are
        // absorbed by the per-edge padding bands. Re-derived on every font/DPI
        // rebuild (`rebuild_backend`).
        {
            let (cw, ch) = self.cell_size();
            window.set_resize_increments(Some(PhysicalSize::new(cw as u32, ch as u32)));
            // Put the increment base on the same whole-cell frame lattice: winit's
            // Wayland backend snaps resizes to `min + k·increment`, and an
            // incongruent min made a vertical drag shave a column. See
            // `whole_cell_min_size`. Linux-only.
            #[cfg(target_os = "linux")]
            window.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
            // Persist this scale's real cell metrics for the next launch's early
            // reveal. Only at the launch-default font size (0.5 px tolerance), so a
            // zoomed Cmd-N attach never stores transient metrics. All platforms
            // record; only the lookup is Windows-gated. Off-thread, and unchanged
            // metrics write nothing.
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
        // Step the frame down-right so the parent window stays visible. After the
        // size correction (the frame decides where the cascade wraps) and before
        // any present. No-op for the first window; restore/update positions
        // applied after the attach (on Windows, the staged `RestorePlacement`
        // below, before the reveal) win.
        self.cascade_window_placement(wid, &window);
        // The caption's surface (the tab band) before the window is revealed.
        #[cfg(windows)]
        crate::platform_win::note_caption_surface(&window, self.caption_surface_tone(wid));
        // Paint the window background the theme background so the transparent
        // titlebar reads as part of the terminal body. Before the GPU/CPU split,
        // since the GPU arm returns. No-op off macOS.
        self.apprt
            .window_set_background_color(&window, self.theme.bg);
        // Apply the configured chrome appearance (`window_theme`) at attach, before
        // the GPU/CPU split. On Windows this sets DWMWA_USE_IMMERSIVE_DARK_MODE so
        // the titlebar matches from the first frame; on macOS it also matches the
        // NSWindow colour space to the content (see the apprt method).
        self.apprt
            .window_set_appearance(&window, self.chrome_theme_for_apprt());
        // Windows: a restored window takes its persisted place while still hidden
        // (`RestorePlacement`, staged by the restore) — position first, validated
        // against the live monitors, then maximized LAST, since winit clears its
        // MAXIMIZED flag on every move and resize. aterm-window keeps a hidden
        // window's maximize for its reveal, so it appears there, maximized when it
        // was, instead of at the cascade or the OS default and then hopping and
        // playing the maximize animation on glass. An early-revealed window
        // already took its position before the join (and is never maximized).
        #[cfg(windows)]
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
            if placement.maximized {
                window.set_maximized(true);
            }
        }
        // Reveal the deferred-join first window now (and on Windows every window,
        // created hidden above), unless the early path already showed it
        // (`early_reveal_size`). By now it has its real size, place and themed
        // background.
        if reveal_after_chrome {
            // Handoff boots stay hidden here — the post-present hook is the one
            // reveal site (carried pixels first, never a bg-only frame).
            if !defer_reveal && early_reveal_size.is_none() {
                window.set_visible(true);
                crate::metrics::mark_first_window_visible();
                // The surface is built synchronously below, before the loop turns:
                // put the themed erase on glass now, not an unpainted client area.
                #[cfg(windows)]
                self.apprt.window_flush_backdrop(&window);
            }
            // Request the first redraw explicitly rather than waiting for WM_PAINT;
            // the themed class-brush erase covers the gap. A duplicate coalesces.
            window.request_redraw();
        }
        // Size the surface to the raw window pixels the WM granted, not the grid
        // request, so the compositor never rescales the frame. Read after the
        // reveal so `inner_size()` is the granted size; a later `Resized`
        // re-syncs. A zero size falls back to the requested frame.
        let inner = window.inner_size();
        let raw_px = if inner.width == 0 || inner.height == 0 {
            size
        } else {
            inner
        };
        let startup_before_surface_create = Instant::now();
        if self.backend.is_gpu() {
            // GPU mode: a wgpu swapchain on the offscreen renderer's instance and
            // adapter; the offscreen frame is blitted into it. No softbuffer.
            let (w_px, h_px) = (raw_px.width, raw_px.height);
            let surface_result =
                self.backend
                    .gpu_mut()
                    .unwrap()
                    .create_window_surface(window.clone(), w_px, h_px);
            // Windows fail-soft: a first-window attach failure on a DComp visual
            // instance rebuilds the GPU stack on the opaque HWND swapchain and
            // retries once. Other outcomes pass through.
            #[cfg(windows)]
            let surface_result = match surface_result {
                Ok(surf) => Ok(surf),
                Err(first_error) => {
                    self.retry_attach_on_opaque_swapchain(first_error, &window, (w_px, h_px))
                }
            };
            // Both creation paths register before installation and first present.
            // A worker spawn failure takes the attach-error fallback, never a
            // synchronous drawable acquire on the event loop.
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
                    // Tag the new CAMetalLayer's colour space so ColorSync interprets
                    // the presented bytes: the configured space (default `srgb`) for
                    // SDR, or extended-linear sRGB for an Rgba16Float swapchain
                    // (`resolve_surface_colorspace`). The bytes are unchanged. No-op
                    // off macOS.
                    let hdr = surf.is_hdr();
                    let surface_colorspace =
                        crate::platform::resolve_surface_colorspace(self.window_colorspace, hdr);
                    let effective_colorspace = self
                        .apprt
                        .window_set_surface_colorspace(&window, surface_colorspace);
                    // Pin the CAMetalLayer's `contentsGravity` to `topLeft` so an
                    // un-repainted frame stays 1:1 during live resize instead of
                    // being stretched. Once per window, after the layer exists.
                    self.apprt.window_anchor_surface_top_left(&window);
                    // With `background_opacity < 1.0`, install the
                    // NSVisualEffectView backdrop and make the window and layer
                    // non-opaque so the translucent present composites over live
                    // blur. No-op at the solid default or off macOS.
                    self.apprt.window_set_vibrancy(
                        &window,
                        self.render_knobs.background_material,
                        aterm_render::vibrancy::is_translucent(
                            self.render_knobs.background_opacity,
                        ),
                        self.theme.bg,
                    );
                    // Seed this window's per-screen EDR headroom (re-queried on
                    // monitor changes in `refresh_frame_interval`); only queried on
                    // an HDR swapchain, and the unset default is inert.
                    let mut window_gpu = aterm_gpu::WindowGpu::new();
                    // Snapshot/video PNGs are unprofiled sRGB; record the actual
                    // compositor interpretation with the surface rather than
                    // guessing it from Bgra8.
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
                        // A configure that beat the attach already settled the
                        // decoration mode; later ones arrive as `Resized`.
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
                    // The window's swapchain failed although the offscreen device
                    // initialized (e.g. an adapter that cannot present). If another
                    // window already presents on the GPU, roll back hard, since a
                    // downgrade would break its surface. Otherwise downgrade the app
                    // to the CPU renderer and fall through to the CPU path with this
                    // `window`.
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
                    // The file log's copy: it already holds the worker's "GPU rendering
                    // on …", which this run then did not do.
                    aterm_log::warn!(
                        "GPU surface creation failed: {e}; falling back to the CPU renderer"
                    );
                    match self
                        .backend
                        .cpu_renderer_from_admitted(self.font_px, self.theme)
                    {
                        Ok(cpu) => {
                            // Carries the pad, top pad and head applied above at
                            // this window's scale.
                            self.backend.swap_renderer(Backend::Cpu(cpu));
                            self.use_gpu = false;
                            crate::metrics::set_backend_gpu(false);
                            self.pin_backend_render_config_core();
                            self.warn_font_feature_issues();
                            self.sync_chrome_fonts();
                            // Do NOT return — fall through to the softbuffer path.
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
        // CPU present path (CoreGraphics on macOS, MIT-SHM / wl_shm on Linux,
        // `softbuffer` on Windows; see `crate::present`). A surface failure drops the window and returns false
        // so only this window is declined.
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
            // As on the GPU path: the decoration mode a configure already settled.
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

    /// Read the OS window's key state at attach and hand it to
    /// [`Self::adopt_initial_focus`], since a window can come up behind another
    /// app without any focus event.
    fn adopt_attached_window_focus(&mut self, wid: WindowId) {
        let focused = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.os_window.as_ref())
            .map(|window| window.has_focus());
        self.adopt_initial_focus(wid, focused);
    }

    /// Attach-time focus adoption (`docs/DESIGN-host-boundary-2026-08-30.md` §7
    /// decision 8). `os_focus` is the OS window's `has_focus()` at attach; `None`
    /// (no OS surface) keeps the controller-declared focus. Otherwise the window
    /// takes the OS answer before its first frame through [`Self::on_focus`]. On
    /// backends that send no creation-time focus event this is the only
    /// correction a window born behind another app gets.
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

    /// The grid half of a `WindowEvent::Resized`: the settle's veto first, then
    /// the throttled apply, in one testable unit so a compositor echo never
    /// reaches the PTY.
    ///
    /// The reflow is scoped to the active tab via `resize_live_drag`, which
    /// `on_resize_throttled` -> ... -> `resize_panes` reads; it is set and
    /// cleared inline. The settle's own correction runs with it clear (all tabs).
    pub(crate) fn apply_window_resized_grid(&mut self, wid: WindowId, size: PhysicalSize<u32>) {
        #[cfg(any(target_os = "linux", test))]
        if self.settle_initial_frame(wid, size) {
            return;
        }
        self.resize_live_drag = true;
        self.on_resize_throttled(wid, size);
        self.resize_live_drag = false;
    }

    /// Linux initial-frame settle (see [`crate::InitialFrameSettle`]): called on
    /// every `WindowEvent::Resized` before the reflow applies `size`; returns
    /// whether it took the size over (the caller must then not apply it). While
    /// armed and before the deadline, a `size` whose grid is not the intended
    /// `rows`×`cols` at the current scale spends one corrective
    /// `request_inner_size`. A match after a correction disarms; a match before
    /// any correction keeps the arm, because GNOME's activation configure can
    /// echo the pre-CSD geometry later. winit drops the request under a
    /// non-stateless configure (maximized/fullscreen/tiled).
    ///
    /// It runs before the apply so a size about to be corrected is never applied:
    /// shrinking and restoring the PTY sends a net-zero SIGWINCH pair, and an
    /// alternate-screen app loses its top row and may not repaint. On a
    /// synchronous grant the corrected size is applied instead; on an
    /// asynchronous one the vetoed size is parked in `pending_resize`, superseded
    /// by the correction's `Resized`, or applied after
    /// [`App::SETTLE_VETO_FALLBACK`] if the WM refuses.
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
        // Re-tune the shared backend to this window's live scale first, since
        // `window_frame_px_for` reads its cell metrics.
        self.apply_window_scale(wid);
        let intended = self.window_frame_px_for(wid, settle.rows, settle.cols);
        // Compare grids, not pixels. At a fractional scale winit sends Wayland a
        // whole-number logical size, so the physical frame can come back a pixel
        // off (e.g. 1157 for 1156 at 1.5×) while the grid is exact; `pad_split`
        // puts the stray px in the right band. Pixel equality would spend the
        // whole correction budget on an unreachable size. The CSD header
        // subtraction this settle exists for still changes the grid.
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
        // winit's Wayland backend applies this request synchronously and returns
        // `Some`, with no `Resized` to follow. `None` means X11/async or no OS
        // window. [`App::settle_take_over`] handles both.
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

    /// How long a size the settle vetoed on an asynchronous backend stays parked
    /// before the fallback applies the window's real size. It must outlast the
    /// correction's configure round trip, or the fallback and the correction
    /// produce the net-zero SIGWINCH pair the veto exists to prevent. Capped by
    /// the settle's deadline.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) const SETTLE_VETO_FALLBACK: std::time::Duration =
        std::time::Duration::from_millis(250);

    /// The settle's take-over of a vetoed `Resized` size, given what the
    /// corrective `request_inner_size` answered (`granted`). Separate from
    /// [`App::settle_initial_frame`] so the synchronous branch is testable.
    ///
    /// `Some(applied)` (Wayland): the request was applied synchronously and no
    /// `Resized` follows, so the applied size goes through the resize path in
    /// place of the echo and any pending throttled reflow is cancelled. If the
    /// compositor refused, `applied` is the current size.
    ///
    /// `None` (X11/async, or no OS window): the correction arrives as a later
    /// `Resized`. The vetoed size is parked in the trailing settle instead; the
    /// correction supersedes it, and a refused request still lands the real size
    /// after [`App::SETTLE_VETO_FALLBACK`] or at the settle's deadline.
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

    /// Logical window teardown (no winit): drop the views of every pane in every
    /// tab of `wid` (the last view closes the PTY master via `Session::drop`),
    /// remove the window (the shared GPU device on `Backend` is never dropped),
    /// clear its winit mapping and re-point `frontmost_window` if needed. Returns
    /// [`CloseOutcome::Exit`] iff no windows remain; a stale `wid` is `Stay`.
    ///
    /// Refines `WindowRouting.CloseWindow`: decrement `win_count`, exit iff empty,
    /// nondeterministic frontmost re-point. Projection:
    /// `window_routing_conformance::project` (lib.rs).
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
        #[cfg(target_os = "macos")]
        self.withdraw_askpass_window(wid);
        // A single-tab close is deferred, so the gesture's `CloseAttribution`
        // scope has dropped by now; re-enter the attribution stashed on the window
        // so `deregister_local_as` records `ctl-close`/`ui-close`. `enter` is
        // outermost-wins, so a live scope takes precedence. The stash is taken
        // either way.
        let deferred_attribution = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.pending_close_attribution.take());
        let _deferred_close = deferred_attribution
            .map(|(reason, actor)| crate::session_store::CloseAttribution::enter(reason, actor));
        // Abort a recording that owns this window's tap while the tap is still
        // reachable, so its client is not stranded until the deadline.
        let _ = self.video_abort_window_close(wid);
        // The last window closing ends the warm spare with it (`crate::spare`);
        // any other close moves where a spare should be.
        if self.exits_app_when_closing(wid) {
            self.retire_spare("closed", None);
        } else {
            self.spare_poke();
        }
        let Some(ws) = self.windows.get(&wid) else {
            return CloseOutcome::Stay;
        };
        // If this close ends the app, capture the layout manifest now while tabs,
        // panes and sessions are intact. The post-loop writer in `main` persists
        // it; this stash is memory-only.
        let exits_app = self.exits_app_when_closing(wid);
        if self.config.restore_session_or_default() && exits_app {
            self.quit_capture = Some(self.capture_restore_manifest());
        }
        // Snapshot every terminal-view edge before mutating. The `layouts`
        // projection omits heterogeneous tabs and would leak a mixed tab's
        // session. Duplicate ids are kept: each view needs one matching detach.
        let ids = self.window_terminal_sessions(wid);
        let view_ids: Vec<crate::tab_model::ViewId> = ws
            .tab_set
            .tabs()
            .iter()
            .flat_map(|tab| tab.root.leaves())
            .collect();
        // Detach each pane's pool view (dropping the Session and its PTY iff it was
        // the last view), and deregister from the registry only when the detach
        // dropped the session; a session shared with another window keeps its
        // entry.
        // THE CLOSED-SESSION INDEX: a close that ends the app keeps nothing
        // (nobody is left to read it), and a window of many tabs builds the
        // skeletons of its last closes only, the ones the index will keep,
        // counted by the closes this batch makes, not by its views.
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
        // Forget this window's taskbar-progress entry before its HWND drops, so a
        // later window reusing the HWND value is not skipped.
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
        // Parked find origins are keyed by (session, window) and otherwise pruned
        // only per session; drop this window's entries, since `WindowId`s are
        // never reused.
        self.find_origins.retain(|(_, entry), _| *entry != wid);
        // Release this window's retained native toolbar backing objects (no-op off
        // macOS / when none was installed) so they don't outlive the window.
        self._toolbars.remove(&wid);
        // A paste-confirmation sheet on this window dies with it and AppKit posts
        // no answer, so retire the entry and its key interceptor here.
        #[cfg(target_os = "macos")]
        if self.paste_confirm.as_ref().is_some_and(|c| c.wid == wid) {
            self.paste_confirm = None;
        }
        // The in-window paste banner (Linux) targeted this window's PTY; dropping
        // it is the fail-closed cancel and frees the one-at-a-time slot.
        if self.paste_banner.as_ref().is_some_and(|p| p.wid == wid) {
            self.paste_banner = None;
        }
        // The close/quit banner (Windows) asked about this very window, so it is
        // moot; dropping it frees the slot, and so is a question that waited over
        // it. The next one that waited asks now, in words that see this close
        // (this window is already out of `windows`).
        if self.close_banner.as_ref().is_some_and(|p| p.wid == wid) {
            self.close_banner = None;
        }
        self.close_banner_queue.retain(|q| q.wid != wid);
        self.ask_next_queued_close();
        // Clear the winit→logical mapping for this window (its OS id is gone).
        self.winit_to_window.retain(|_, &mut v| v != wid);
        // Drop the closed window from the focus-order stack so it can never be picked
        // as a survivor below.
        self.focus_order.retain(|w| *w != wid);
        // A pending menu-driven raise of this window dies with it.
        if self.pending_deminiaturize_focus == Some(wid) {
            self.pending_deminiaturize_focus = None;
        }
        // Re-point frontmost to the most recently focused survivor, else the
        // lowest live id. See `next_frontmost_after_close`.
        if self.frontmost_window == Some(wid) {
            self.frontmost_window = self.next_frontmost_after_close();
        }
        if !self.windows.is_empty() {
            debug_assert!(
                self.structural_invariants_ok(),
                "window/session structural invariants violated after close_window_logical: {}",
                self.structural_invariant_violation().unwrap_or_default(),
            );
            // A survivor became (or stayed) frontmost: re-mirror the control socket /
            // notify target onto its active tab, exactly like a tab/focus switch.
            self.sync_active_session();
            debug_assert!(
                self.structural_invariants_ok(),
                "window/session structural invariants violated after re-mirror: {}",
                self.structural_invariant_violation().unwrap_or_default(),
            );
        }
        // Refresh the status item after deregistration so it shows post-close
        // state.
        self.mark_status_dirty();
        if self.windows.is_empty() {
            CloseOutcome::Exit
        } else {
            CloseOutcome::Stay
        }
    }

    /// Record that `wid` gained OS focus: move it to the most-recent end of the
    /// deduped MRU stack consulted when the front window closes. Called only from
    /// `WindowEvent::Focused(true)`, so headless leaves it empty.
    pub(crate) fn note_window_focused(&mut self, wid: WindowId) {
        self.focus_order.retain(|w| *w != wid);
        self.focus_order.push(wid);
        // The status item marks the frontmost window; this sees every focus gain,
        // including same-window re-focus (fingerprint-gated).
        self.mark_status_dirty();
    }

    /// The window to make frontmost when the front window closes: the most
    /// recently focused surviving window (what macOS raises), else the lowest
    /// live `WindowId`, which keeps headless deterministic. `None` only when no
    /// window remains.
    pub(crate) fn next_frontmost_after_close(&self) -> Option<WindowId> {
        self.focus_order
            .iter()
            .rev()
            .find(|w| self.windows.contains_key(w))
            .copied()
            .or_else(|| self.windows.keys().next().copied())
    }

    /// Close window `wid` and exit the app iff it was the last window. The routing
    /// point for every close path (CloseRequested, last-tab Cmd-W/CloseTab, a
    /// last-tab `Wake::Exit`).
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
        if matches!(self.close_window_logical(wid), CloseOutcome::Exit) {
            el.exit();
        }
    }

    /// Re-read the frame window `wid` must draw itself ([`crate::client_frame`]) from
    /// its OS window: at attach, and on every `Resized`, which is how the fork reports
    /// a change of decoration mode, of the compositor's capabilities or of the
    /// maximized, fullscreen or tiled state. The strip lays its buttons out from it on
    /// the next splice (the slot and the frame are strip cache-key and fingerprint
    /// terms); losing the frame drops what only it could hold.
    ///
    /// `true` when the strip's rows moved with it ([`Self::sync_strip_header_row`]):
    /// `wid`'s grid is then the caller's to fit.
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

    /// The strip rows for the `configured` `tab_strip_rows`: one at least while any
    /// window draws its own frame and needs its header
    /// ([`crate::client_frame::strip_rows_with_header`]). The config reload and
    /// [`Self::sync_strip_header_row`] both resolve through it, so the header's row
    /// comes and goes with the frames that need it.
    pub(crate) fn strip_rows_for(&self, configured: u16) -> u16 {
        let header = self.windows.values().any(|ws| {
            ws.client_frame
                .is_some_and(crate::client_frame::ClientFrame::has_header)
        });
        crate::client_frame::strip_rows_with_header(configured, header)
    }

    /// Re-resolve the strip's rows ([`Self::strip_rows_for`] of the live config) after
    /// window `settling`'s frame changed; `true` when they moved. Every other window
    /// re-grids as for a Settings edit of the rows; `settling`'s grid is its caller's
    /// to fit (the `Resized` handler's own fit, or at attach
    /// [`Self::apply_window_resized_grid`]). Fitting it there runs the initial-frame
    /// settle first, so a header row that arrives with the first configure is added
    /// to the window rather than taken from the terminal, and the PTY sees one size,
    /// not the net-zero pair the settle exists to prevent. It resolves the strip
    /// from the live config, so a Settings edit's fall still waiting the regrow
    /// floor ([`Self::set_tab_strip_rows`]) is settled here: taken with the frame's
    /// own change, or dropped when the header keeps the row.
    #[cfg(any(test, target_os = "linux"))]
    pub(crate) fn sync_strip_header_row(&mut self, settling: WindowId) -> bool {
        self.strip_fall = None;
        let rows = self.strip_rows_for(crate::app_config::resolve_tab_strip_rows(&self.config));
        if rows == self.tab_strip_rows {
            return false;
        }
        // A rise is noted like the reload's (`set_tab_strip_rows`), so no band
        // shrink re-grids inside it.
        if aterm_messages::chrome::is_rise(self.tab_strip_rows, rows) {
            self.note_chrome_rise_everywhere(Instant::now());
        }
        self.tab_strip_rows = rows;
        self.regrid_for_chrome_rows_but(settling);
        true
    }

    /// [`Self::sync_client_frame`] at attach, where no `Resized` fit follows: a header
    /// row it added (or dropped) is fitted here as a `Resized` of the window's size
    /// would be, the initial-frame settle first.
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

    /// Run a press on a window's own close button (`WindowState::caption_close`,
    /// set where the mouse handler has no `ActiveEventLoop`) as the window system's
    /// close: `CloseRequested`'s attribution and path, its confirmation included.
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

    /// Close any window whose last-tab close set `pending_close` (those paths have
    /// no `ActiveEventLoop`). The flag may be on the frontmost or the clicked
    /// window, so scan for it. It is cleared first to avoid a re-trigger.
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
            // (`window_exit_close_allowed`), so do not prompt again.
            self.close_window(el, w);
        }
    }

    /// Whether session `id` is running a foreground job (not the idle shell). See
    /// [`Self::session_busy`]. Unknown id → idle. Also used by the `close` verb's
    /// per-pane guard (`App::pane_close_hangs_up_a_job`).
    pub(crate) fn session_foreground_busy(&self, id: u64) -> bool {
        self.pool.get(id).is_some_and(Self::session_busy)
    }

    /// The busy predicate every close path reads (ctl `close`/`tab close`, the
    /// close button, Close Tab, Quit): [`crate::quit_safety::foreground_busy`] over
    /// the shell-integration state (OSC 133;C open without 133;D) or the PTY's
    /// verdict (`tcgetpgrp` on unix; on Windows a child-process walk counting only
    /// children started since the shell last went idle,
    /// [`crate::quit_safety::ShellWord`]).
    ///
    /// The engine is locked, not `try_lock`ed: a close is a one-off on the main
    /// thread and a contended guess would either kill a job or wedge the close.
    /// Poison is recovered like every other main-thread engine read.
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

    /// Whether closing window `wid` would SIGHUP a foreground job. A session also
    /// viewed in another window only loses a view, so only sessions whose last
    /// viewer is this window count.
    fn window_has_foreground_job(&self, wid: WindowId) -> bool {
        let mut ids = self.window_terminal_sessions(wid);
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter().any(|id| {
            self.window_drops_last_session_view(wid, id) && self.session_foreground_busy(id)
        })
    }

    /// Closing `wid` removes all of its view edges at once, so a shared session is
    /// safe only when a pool view remains outside this window.
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

    /// Whether any live session has a foreground job: the app Quit's `busy`, read
    /// through the same [`Self::session_busy`] as the window close.
    fn any_foreground_job(&self) -> bool {
        self.pool.iter().any(Self::session_busy)
    }

    /// Confirm a destructive close/quit gesture for window `wid`, returning `true` to
    /// proceed. `exits_app`: the gesture quits the whole program (Cmd-Q, app-menu
    /// Quit, or closing the last window); `busy`: closing now would SIGHUP a
    /// foreground job.
    ///
    /// macOS shows the native `NSAlert`: always for a whole-app quit, and for a
    /// window/tab close only while a job runs. Linux asks in the window, through
    /// the same banner as Windows (below), and only while a job runs: an idle
    /// close or quit is instant there, as in GNOME Terminal.
    ///
    /// Non-interactive closes never reach a dialog: headless always proceeds, and a
    /// control-socket close answers by its [`CloseConfirm`] policy. The `close` and
    /// `tab close` verbs proceed when idle and are refused when a job runs (the
    /// refusal reaches the wire through `pending_action_refusal`); only an explicit
    /// instruction ([`CloseConfirm::Programmatic`]) proceeds regardless. A wire close
    /// never blocks on a human click.
    ///
    /// Windows and Linux ask through the in-window banner, not a dialog: a
    /// task-modal dialog would park the event loop and every control verb with it,
    /// and a titlebar hint is invisible on a tiling or titlebar-less session. The
    /// gesture is refused now (`false`) and its question parked on `close_banner`
    /// (see [`crate::close_confirm`]); Enter replays it under
    /// [`CloseConfirm::Programmatic`], so it is never asked twice.
    ///
    /// The gesture here is a window close (a last-tab close, Alt+F4, the caption ✕),
    /// so its replay is [`crate::close_confirm::CloseReplay::Window`]; Quit names its
    /// own replay through [`Self::confirm_destructive_gesture`].
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

    /// [`Self::confirm_destructive_close`] with the gesture's REPLAY named: what
    /// Enter re-runs once the banner is answered (a window, tab or pane close, or
    /// the whole-app quit whose barriers a window close does not run). macOS
    /// answers synchronously through the alert.
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
                    // Every consumer of this channel clears it before its own action
                    // (`apply_tab_cmd_in`, `invoke_menu_action_by_name`), so this
                    // refusal is never reported against a later `tab` or `invoke`.
                    self.pending_action_refusal = Some(WIRE_CLOSE_REFUSED.to_string());
                }
                return !busy;
            }
            CloseConfirm::Interactive => {}
        }
        let Some(prompt) = self.destructive_prompt(wid, exits_app, busy, replay) else {
            // Nothing worth interrupting for (macOS: app keeps running and nothing
            // is busy; Windows: also a single idle tab): the close needs no confirm.
            return true;
        };
        // Windows: park the question on the in-window banner and refuse for now,
        // never `apprt.confirm` (see the doc above and `close_confirm`). `cfg!`
        // rather than `#[cfg]` keeps both arms compiled on every host.
        if cfg!(windows) {
            return self.present_close_banner(wid, prompt, replay);
        }
        if let Some(proceed) = self
            .apprt
            .confirm(prompt.title, prompt.body, prompt.proceed)
        {
            return proceed;
        }
        // No native dialog (Linux): only a gesture that would end a job asks, and
        // it asks in the window. The always-ask quit above is macOS's ⌘Q rule.
        if !busy {
            return true;
        }
        self.present_close_banner(wid, prompt, replay)
    }

    /// The question a destructive gesture over `wid` asks, or `None` when it is
    /// worth no interruption. `replay` names the gesture: a tab or pane close
    /// asks only while `busy`, in its own words; a window close or a quit asks by
    /// the platform's policy. Linux's banner additionally asks only while busy
    /// (see [`Self::confirm_destructive_gesture`]); this is the question, not
    /// that gate.
    fn destructive_prompt(
        &self,
        wid: WindowId,
        exits_app: bool,
        busy: bool,
        replay: crate::close_confirm::CloseReplay,
    ) -> Option<crate::quit_safety::ConfirmPrompt> {
        // Windows follows Windows Terminal's convention, not macOS's: prompt when
        // the gesture closes MULTIPLE tabs or a running job, never for a single
        // idle tab — see `quit_safety::confirm_prompt_windows`. The tab count is
        // the gesture's blast radius: every window's tabs for a whole-app quit,
        // the closing window's for a window close. A last-tab close
        // (`window_exit_close_allowed`) reaches here with the window already down
        // to one tab, so it stays instant while idle under both policies.
        // A quit with restore on loses the shells, not the layout: the copy says so.
        let reopens = self.config.restore_session_or_default();
        // A tab or pane close leaves its window standing: it asks only while a job
        // runs, in its own words, on every platform's policy.
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

    /// Park a destructive question on the in-window banner over `wid` and refuse
    /// the gesture for now (always `false`), so the event loop keeps turning
    /// (`close_confirm`). One question shows at a time. A person's gesture over
    /// ANOTHER window while one stands waits its turn (`close_banner_queue`, one
    /// per window) and is asked once the standing question is answered or
    /// dropped ([`Self::ask_next_queued_close`]): the taskbar's "Close all
    /// windows" closes every window at once, and a dropped gesture left the
    /// second window open after the first was answered. Every other second
    /// gesture (over the same window, or a control client's) is refused with
    /// the standing question named, as before.
    ///
    /// The gesture's exit-ledger attribution is stashed on the window now, while
    /// its `CloseAttribution` scope is open, because the replay runs later with no
    /// scope.
    ///
    /// The window holding the question is brought forward
    /// ([`Self::raise_close_question`]) whenever a person's gesture lands on it,
    /// first or later, so a question over a background or minimized window is
    /// seen.
    pub(crate) fn present_close_banner(
        &mut self,
        wid: WindowId,
        prompt: crate::quit_safety::ConfirmPrompt,
        replay: crate::close_confirm::CloseReplay,
    ) -> bool {
        // Who asked decides who may answer (`PendingClose::wire_may_answer`): read
        // the gesture's attribution scope (`ctl-close` for `invoke`, `ui-close` /
        // `window-close` for the human, none for Quit) while it is still open.
        let (_, asked_by) = crate::session_store::current_close_attribution();
        let pending = crate::close_confirm::PendingClose::new(wid, prompt, replay, asked_by);
        let by_person = !pending.wire_may_answer();
        if let Some(standing) = self.close_banner.as_ref() {
            let standing_wid = standing.wid;
            // The STANDING question is what a wire caller must answer.
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
            // A person whose gesture is held up is shown what holds it up.
            if by_person {
                self.raise_close_question(standing_wid, true);
            }
            return false;
        }
        // A gesture that mints a reply (`invoke CloseTab`, the `tab` verb) reports
        // the pending question and how to answer it. Wire consumers clear this
        // before their own action.
        self.pending_action_refusal = Some(pending.wire_refusal());
        self.stash_deferred_close_attribution(wid);
        self.close_banner = Some(pending);
        self.raise_close_question(wid, by_person);
        false
    }

    /// Bring the window holding the close question into view: restored when
    /// minimized, and, for a person (`focus`), raised and given the keyboard
    /// that answers it, with the taskbar button flashing when the OS refuses the
    /// focus change. A control client's question only un-minimizes and repaints:
    /// an agent's gesture never takes the person's focus.
    fn raise_close_question(&mut self, wid: WindowId, focus: bool) {
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
            // Windows skips this when the focus change took (the window is active).
            w.request_user_attention(Some(aterm_window::window::UserAttentionType::Critical));
        }
        w.request_redraw();
    }

    /// Ask the first question that waited behind the one just answered or
    /// dropped (`close_banner_queue`), over a window that still exists. A window
    /// close is asked again in the words that are true NOW: the first window's
    /// close may have made this one the last (its close now quits), or its job
    /// may have ended. One that no longer needs asking proceeds as its gesture
    /// would have (`Wake::ConfirmedClose`). A no-op while a question stands.
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

    /// Answer the in-window close/quit banner: `proceed` replays the parked gesture
    /// through [`crate::Wake::ConfirmedClose`] (the lane with the `ActiveEventLoop`
    /// the close needs); `!proceed` drops the question. Reached from Enter/Escape in
    /// `on_key`, a click on the band (`app_mouse`) and the wire's `confirm yes|no`
    /// ([`Self::answer_close_confirm_from_wire`]). `None` when no banner is up;
    /// otherwise the replay it posted.
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
            // The attribution `present_close_banner` stashed was for a replay that
            // will not run; a later close must not inherit this gesture's name.
            if let Some(ws) = self.windows.get_mut(&pending.wid) {
                ws.pending_close_attribution = None;
            }
            // A cancel answers this question only: one that waited asks now. A
            // `yes` asks it once its replay has run (`replay_confirmed_close`),
            // so the next question's words see that close.
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

    /// The `confirm yes|no` verb ([`crate::Wake::AnswerCloseConfirm`]): answer the
    /// standing question as Enter/Escape would, when the wire may. `Ok` is the text
    /// after `OK ` (`answered=yes kind=quit`), `Err` the refusal after `ERR `.
    ///
    /// There is one question per instance, so no aim is needed. A question a
    /// person asked at the window is refused here and left for them
    /// ([`crate::close_confirm::PendingClose::wire_may_answer`]).
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

    /// Serve [`crate::Wake::ConfirmedClose`]: run the gesture the banner stood for
    /// under [`CloseConfirm::Programmatic`], so [`Self::confirm_destructive_close`]
    /// does not ask again. The policy is restored afterwards so it covers only this
    /// replay. A window that closed meanwhile has nothing to replay.
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
                // A confirmed quit answers every close that waited behind it:
                // the windows go with the app.
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
        // The next question that waited, now that this close has run (a window
        // close already asked it from `close_window_logical`).
        self.ask_next_queued_close();
    }

    /// The tab or pane close a banner stood for, re-run under the caller's
    /// policy: `Some(window)` when the close emptied that window (it became the
    /// last while the question stood), which the caller then closes as the
    /// gesture would have. A tab or pane already gone replays nothing.
    ///
    /// The replay runs a turn after the gesture, with no attribution scope open,
    /// so it re-enters the one [`Self::present_close_banner`] stashed on the
    /// window, and takes it: a tab or pane close leaves the window standing, so
    /// [`Self::close_window_logical`] (the stash's other consumer) never runs to
    /// clear it. Left behind, the stash would name the window's NEXT scope-less
    /// close (its last shell exiting on its own read `ui-close by=human`), and a
    /// question `invoke CloseTab` raised would be journalled under the
    /// `ui-close by=human` scope `close_tab_at` opens for itself, not
    /// `ctl-close by=ctl`. If the replay empties the window, `close_tab_at`
    /// stashes this same attribution again for the teardown.
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

    /// The `controls front` line for the close/quit confirm: the pending question
    /// and the wire way to answer it, or `confirm open=false`. Reported for the
    /// instance regardless of which window the banner is over. ` waiting=<n>`
    /// ends it while `n` of a person's gestures over other windows wait their
    /// turn ([`Self::present_close_banner`]).
    ///
    /// `None` on macOS, which never parks a close question (its `NSAlert`
    /// answers inside the gesture).
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

    /// Quit-safety for a tab close that would exit the window. Tab-close gestures
    /// (Cmd-W, Close Tab, the strip/native ✕, `tab close`) go through
    /// `close_active_tab`/`close_tab_at`, which escalate to closing the window when
    /// they drop its last tab (or last pane), so they enforce the same confirm as
    /// the close button ([`Self::on_close_requested`]) and Cmd-Q
    /// ([`Self::on_quit_requested`]).
    ///
    /// Given `exits_window`, return `true` to proceed or `false` to cancel via
    /// [`Self::confirm_destructive_close`]. A non-exiting close always returns
    /// `true`: [`Self::view_close_allowed`] guards that one.
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

    /// Quit-safety for a tab or pane close that leaves window `wid` standing:
    /// Ctrl+Shift+W on a split or a non-last tab, the strip ✕, a middle click, the
    /// tab menu. `busy`: the close would hang up a job only this tab or pane holds
    /// (`pane_close_hangs_up_a_job` / `tab_close_hangs_up_a_job`, so a co-viewed
    /// session or a sibling pane's job never counts). A busy close asks first,
    /// through the same confirm as the window's last close, and `replay` is the
    /// close the answer re-runs; an idle one is instant. GNOME Terminal and
    /// Terminal.app ask the same question; before, one slip of Ctrl+Shift+W hung
    /// up vim, a build or an ssh session in a background tab.
    ///
    /// macOS keeps aterm's documented choice for a person at the window (Cmd-W
    /// closes such a tab or pane without a dialog: a human is watching); the wire
    /// policies still refuse or proceed there as everywhere.
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

    /// Whether closing window `wid` quits the app: no other mixed-tab host survives.
    fn exits_app_when_closing(&self, wid: WindowId) -> bool {
        !self.windows.keys().any(|w| *w != wid)
    }

    /// Quit-safety for CloseRequested (close button / Window ▸ Close): confirms via
    /// [`Self::confirm_destructive_close`] when this is the last window or a pane
    /// of `wid` has a foreground job; otherwise proceeds immediately.
    pub(crate) fn on_close_requested(&mut self, el: &ActiveEventLoop, wid: WindowId) {
        // The red traffic light closes this window and quits when no other window
        // survives.
        let exits_app = self.exits_app_when_closing(wid);
        let busy = self.window_has_foreground_job(wid);
        if self.confirm_destructive_close(wid, exits_app, busy) {
            self.close_window(el, wid);
        }
    }

    /// Quit-safety for whole-app Quit (Cmd-Q / app menu): always confirms on macOS
    /// and exits only when the user confirms; a running job in any window enriches
    /// the dialog copy. Elsewhere it uses the platform confirm, which is
    /// busy-gated.
    ///
    /// Dock Quit, AppleScript `quit` and logout send AppKit `terminate:` to `NSApp`
    /// directly; that hook vetoes and posts [`crate::Wake::NativeTerminateRequested`]
    /// ([`crate::platform::AppRt::install_quit_confirm`]), which returns here on the
    /// event-loop lane before any exit.
    pub(crate) fn on_quit_requested(&mut self, el: &ActiveEventLoop) {
        if self.defer_pending_update_handoff_teardown(crate::DeferredHandoffTeardown::QuitRequested)
        {
            return;
        }
        // Anchor the confirm to the frontmost window, else any survivor, so Cmd-Q
        // still prompts before the first OS focus; only a window-less app exits
        // immediately.
        let anchor = self
            .frontmost_window
            .filter(|w| self.windows.contains_key(w))
            .or_else(|| self.windows.keys().next().copied());
        // Quit always confirms; `busy` enriches the copy. A windowless process
        // still runs the store-owned document barrier.
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

    /// Both Quit barriers, in the order every Quit edge asks them: the native
    /// reducers, then the documents, whose saves this starts. A Quit left
    /// [`QuitBarrier::Saving`] keeps its document plan armed, and
    /// [`Self::escalate_document_shutdown`] ends the quit when the saves land;
    /// a [`QuitBarrier::Refused`] one has been disarmed.
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

    /// A Quit whose barriers were ready when it deferred behind an update
    /// handoff (`DeferredHandoffTeardown::CleanQuitReady`), replayed once the
    /// rollback has run. The app stayed interactive meanwhile, so a native view
    /// or a document may hold new unsaved work: both barriers are asked again,
    /// as at every Quit edge. The native barrier alone cannot see a document's
    /// edits, since the editor reducer answers `Ready` whatever its document
    /// holds. `true` means exit now; a Quit left saving ends in
    /// [`Self::escalate_document_shutdown`] when its saves land.
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

    /// The process that started this headless instance is gone
    /// (`crate::lifeline`): quit as Cmd-Q does. Headless has no confirm, so
    /// [`Self::on_quit_requested`] runs its barriers and `el.exit()`, and teardown
    /// unlinks the socket and hangs up every shell. A barrier holding the quit is
    /// left to finish; the watcher's backstop ends the process if it never does.
    #[cfg(unix)]
    pub(crate) fn on_lifeline_cut(&mut self, el: &ActiveEventLoop) {
        if !self.headless {
            // `cli::lifeline_request` refuses the flag on a window, so this is not
            // reached; were it, a window is a person's to close, not a harness's.
            aterm_log::warn!("lifeline cut in a windowed instance; leaving it open");
            return;
        }
        self.on_quit_requested(el);
    }

    /// A headless instance was sent a terminating signal
    /// (`owned_endpoint::arm_orderly_quit`): quit as the Quit verb does — its
    /// barriers, `el.exit()`, then the final exit's teardown (subscribers told
    /// `bye quit`, task files and the crash marker removed, every shell hung
    /// up), after which the process dies of `sig`. A barrier holding the quit is
    /// left to finish; the watcher's backstop ends the process if it never does.
    #[cfg(unix)]
    pub(crate) fn on_exit_signal(&mut self, el: &ActiveEventLoop, sig: i32) {
        if !self.headless {
            // Only a headless instance arms the orderly path; were a window's
            // reached, its quit would ask a person, so the signal ends it as
            // before.
            aterm_log::warn!("signal {sig} reached a windowed instance's quit path; ending now");
            crate::owned_endpoint::die_of_quit_signal();
            return;
        }
        self.on_quit_requested(el);
    }

    /// Stateful counterpart of AppKit's bare `applicationShouldTerminate:`
    /// callback. The callback always defers the first request and posts this typed
    /// generation; only this event-loop lane may confirm, save documents, apply a
    /// deferred update, and exit.
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

    /// Re-evaluate document-owned close plans after an asynchronous atomic-save
    /// completion. Whole-app Quit has priority; otherwise each ready window has
    /// already detached its document edges as one batch and can be removed
    /// without re-entering the barrier.
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
            // Ordinary quit is now irreversible. Async handoff success exits via
            // `_exit` and intentionally never reaches this completion boundary.
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

    /// Resolve the identity shown in native window chrome, with the tab-label
    /// precedence: operator title, live OSC 0/2 title, OSC-7 cwd, tab presentation,
    /// then `"aterm"`. The terminal read is nonblocking (event-loop thread);
    /// contention keeps the last cached title until the next redraw.
    ///
    /// Resolves in place into `slot` so the per-redraw [`Self::apply_title`] does
    /// not allocate. Returns the focused session, the label cache's identity.
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
            // The operator title is already in `slot`.
        } else if !live_title.is_empty() {
            slot.push_str(live_title);
        } else {
            // Reported cwd (`~`-abbreviated, native form such as `C:\Users\x`), else
            // the cached tab title when the lock is free, else the presentation
            // fallback. The `Cow` is bound to a local so it outlives the borrow;
            // off Windows the conversion is a pure borrow.
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

    /// Reflect the focused terminal's identity in the window chrome, falling back
    /// to `"aterm"`. Calls `set_title` only on a semantic change (string compare
    /// with busy-spinner coalescing), so it is safe every frame, including the
    /// redraw early-out path.
    ///
    /// IME-2: the caption deliberately shows NO composition state. An earlier
    /// cut appended `title [‹preedit›]` here as the only preedit indicator; the
    /// composition now paints INLINE where the user is looking (the grid caret
    /// via `RenderInput::overlay_ime_preedit`, or the find well / rename well
    /// when one owns it — see `App::preedit_owner`), and the
    /// caption suffix was DELETED rather than kept as a fallback: it rewrote
    /// the Alt-Tab/taskbar label on every composing keystroke, it went dark
    /// whenever a temporary title owner held the caption (find status), and a
    /// headless window has no OS caption for a "headless fallback" to reach.
    ///
    /// No tab counter: the tab strip shows the tabs.
    pub(crate) fn apply_title(&mut self, id: WindowId, window: &Window, title: &str) {
        // Windows taskbar progress (ConEmu OSC 9;4): mirror the focused session's
        // `taskbar_progress()` onto the taskbar button. Change-gated, so COM is
        // called only on a real change.
        #[cfg(windows)]
        self.sync_taskbar_progress(id);
        // Keep the registry title for the focused pane's session fresh, taking the
        // store write lock only when the per-window `(session, title)` cache says
        // the session or its title changed.
        self.publish_active_terminal_title(id, title);
        // Resolve and present through a resident buffer so the per-redraw path does
        // not allocate. Taken out of `self` so `&self.chrome_labels` stays
        // borrowable; put back before every exit.
        let mut scratch = std::mem::take(&mut self.window_title_scratch);
        let focused_session = self.window_title_identity(id, title, &mut scratch);
        // Presents in place: `scratch` holds the raw title in and the label out
        // (sanitized and capped for the titlebar); a cache hit reuses its capacity.
        self.chrome_labels.present_into(
            focused_session,
            crate::chrome_label::ChromeSurface::WindowTitle,
            &mut scratch,
        );
        // No tab counter in the title; the tab strip shows the tabs. The find
        // status, while open, takes precedence over the canonical title.
        // `current_title` keeps tracking under it, but canonical writes are
        // suppressed.
        let search_active = self.windows.get(&id).is_some_and(|ws| ws.search.is_some());
        let title_authority = window_title_authority(search_active);
        let title_changed = {
            // Compare against the presented scratch directly to avoid a per-frame
            // allocation.
            let Some(ws) = self.windows.get_mut(&id) else {
                // Disjoint field, so restoring the slot here is fine even though
                // `self.windows` was just borrowed.
                self.window_title_scratch = scratch;
                return;
            };
            if ws.current_title != scratch
                && !crate::toolbar::busy_spinner_phase_only_change(&ws.current_title, &scratch)
            {
                // The OS caption, taskbar button and Alt+Tab label show the
                // subject without a leading spinner frame: they keep one frame of
                // an animation the coalescing above never advances, and a frozen
                // `◐` reads as a stray glyph. The tab chip keeps its activity mark;
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
        // Hand the warmed buffer back before any further `&mut self` work, so the
        // next frame's resolve/present reuses this capacity.
        self.window_title_scratch = scratch;
        // Refresh the native tab strip when the active tab's title changes (it
        // re-reads every tab), so labels track the live cwd/command; structural
        // changes are handled by `sync_window`. Gated on a semantic change; no-op
        // off macOS. Cwd-only label drift is refreshed by the `Wake::Output`
        // title-epoch path.
        if title_changed {
            self.refresh_window_tabs(id);
        }
    }

    /// Apply one engine-authorized XTWINOPS window manipulation on the main thread,
    /// via [`crate::Wake::WindowOp`] posted by `spawn::configure_window_ops`
    /// (Linux).
    ///
    /// Security is the engine's: this is reachable only through the in-core
    /// `allow_window_ops` capability, window move is denied there, and resizes
    /// arrive clamped. This method adds routing:
    ///
    /// * the target window is re-resolved from the session (the spawn-stamped
    ///   `window` is a fallback), so a migrated tab manipulates its current window;
    /// * a cell resize applies only when the session is the target window's
    ///   focused one, through the `resize` verb's clamp+apply seam
    ///   (`echo_to_window: true`), so a background tab cannot re-grid a window;
    /// * `LowerWindow` and axis-only maximizes are ignored: winit has no such
    ///   affordance.
    ///
    /// winit calls are best-effort (the WM may clamp, defer or refuse); geometry
    /// changes flow back through `Resized`.
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
        // A cell resize needs no window handle: the clamp+apply seam resizes grid
        // and PTY and echoes the pixel size to any glass, as the `resize` verb
        // does. Handled before the glass gate, so it also works headless.
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
            return; // headless / not yet attached: nothing to manipulate
        };
        match op {
            W::DeIconify => w.set_minimized(false),
            W::Iconify => w.set_minimized(true),
            // xterm "raise" maps to requesting activation; winit has no raw
            // restack-above, and focus is the portable near-equivalent.
            W::RaiseWindow => w.focus_window(),
            W::RefreshWindow => w.request_redraw(),
            W::ResizeWindowPixels { height, width } => {
                // The engine already clamped both axes to its 200 px floor; the
                // platform's `Resized` then drives the grid.
                #[cfg(target_os = "linux")]
                if let Some(ws) = self.windows.get_mut(&wid) {
                    // A driven resize owns the geometry from here: the launch
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
            // No winit mapping (lower/axis-maximize). Reports never ride this wake
            // (`configure_window_ops` answers them `None`). `_` covers the
            // `#[non_exhaustive]` enum and the cell resize routed above.
            W::LowerWindow | W::MaximizeVertically | W::MaximizeHorizontally => {}
            _ => {}
        }
    }

    /// Target window for one XTWINOPS wake (as `on_bell`): the window whose tab set
    /// focuses this session now, else the spawn-stamped window while it is live.
    /// `None` drops the operation.
    #[cfg(any(target_os = "linux", test))]
    fn window_op_target(&self, session: u64, stamped: WindowId) -> Option<WindowId> {
        self.windows_with_focused_session(session)
            .first()
            .copied()
            .or_else(|| self.windows.contains_key(&stamped).then_some(stamped))
    }

    /// Publish an OSC-derived title only when the active tab is a terminal. Native
    /// presentations own OS chrome while active but must never rename the parked
    /// terminal in `SessionStore` (its control-socket identity).
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
    /// [`aterm_core::terminal::Terminal::taskbar_progress`]) onto its taskbar
    /// button via `ITaskbarList3` (see [`taskbar`]). Follows tab/pane focus;
    /// [`TaskbarProgress::Hidden`] or no HWND clears the button. Change-gated in
    /// [`taskbar::set_progress`]. Windows-only.
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
        // The bell's overlay badge and the jump list's Recent sampler ride this
        // same per-frame seam (`taskbar_badge_win`, `jumplist_recent`).
        self.sync_taskbar_extras(wid, hwnd);
    }
}

/// Hand-rolled COM FFI for the Windows taskbar progress indicator
/// (`ITaskbarList3`), behind ConEmu's `OSC 9;4`, and for the bell's overlay
/// badge (`SetOverlayIcon`, driven by `taskbar_badge_win`). Direct `ole32`
/// declarations and a by-hand vtable, like [`super::win32`]. One COM object is
/// created lazily on the UI/STA thread, cached in a `thread_local`, shared by
/// every window and leaked for the process life. Each window's last state is
/// cached so COM is reached only on change.
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

    /// The `ITaskbarList3` vtable in COM inheritance order (IUnknown → ITaskbarList
    /// → ITaskbarList2 → ITaskbarList3), truncated after `set_overlay_icon` (the
    /// bell's badge, `taskbar_badge_win`). The unused slots must stay for the
    /// offsets of the ones called; the order is `shobjidl_core.h`'s, checked
    /// against `windows` 0.62's `ITaskbarList3_Vtbl`.
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
            // SAFETY: `obj` is a live `ITaskbarList3` created on this thread and
            // `hwnd` a live window handle (0 rejected by the caller); both calls are
            // synchronous and take integers. The value is out of 100.
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

    /// Show (`icon` non-zero, with its accessible `description`, NUL-terminated)
    /// or clear (`icon` 0) the overlay badge on `hwnd`'s taskbar button
    /// (`SetOverlayIcon`). The taskbar keeps its own copy of the icon. Whether
    /// the taskbar took it; `false` with no taskbar interface. The badge's state
    /// lives in `taskbar_badge_win`, which calls this only on a change.
    pub(crate) fn set_overlay_icon(hwnd: isize, icon: isize, description: &[u16]) -> bool {
        CACHE.with(|c| {
            let Some(obj) = c.borrow_mut().object() else {
                return false;
            };
            let description = overlay_description(icon, description);
            // SAFETY: `obj` is a live `ITaskbarList3` created on this thread,
            // `hwnd` a live top-level window (0 rejected by the caller), `icon`
            // a live HICON or 0, and `description` null or a NUL-terminated
            // UTF-16 string that outlives this synchronous call.
            unsafe { ((*(*obj).vtbl).set_overlay_icon)(obj, hwnd, icon, description) >= 0 }
        })
    }

    /// The `pszDescription` [`set_overlay_icon`] hands the taskbar: null for a
    /// clear (`icon` 0) and for text that is not NUL-terminated (the shell
    /// reads a C string), else the text itself.
    fn overlay_description(icon: isize, description: &[u16]) -> *const u16 {
        if icon == 0 || description.last() != Some(&0) {
            std::ptr::null()
        } else {
            description.as_ptr()
        }
    }

    /// Drop the cache entry for a closing window's `hwnd`, so a later window
    /// reusing the value is not skipped, and clear its overlay badge. The button
    /// disappears with the window.
    pub(crate) fn forget(hwnd: isize) {
        crate::taskbar_badge_win::forget(hwnd);
        CACHE.with(|c| {
            c.borrow_mut().applied.remove(&hwnd);
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// What `SetOverlayIcon` reads as the badge's spoken text: the
        /// description on a show, nothing on a clear, and nothing for text
        /// without its NUL (the shell would read past it).
        #[test]
        fn a_show_passes_its_text_and_a_clear_passes_none() {
            let text = crate::win_ffi::wide("A tab rang the bell");
            assert_eq!(overlay_description(0x7010, &text), text.as_ptr());
            assert!(overlay_description(0, &text).is_null(), "a clear");
            assert!(overlay_description(0, &[]).is_null());
            let unterminated: Vec<u16> = "A tab".encode_utf16().collect();
            assert!(overlay_description(0x7010, &unterminated).is_null());
        }

        /// The ConEmu progress states map to the TBPFLAG and value the taskbar
        /// takes; nothing set clears the button.
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

        /// The vtable reaches `SetOverlayIcon` at slot 18 and the progress
        /// calls at 9 and 10, `shobjidl_core.h`'s order: IUnknown (3),
        /// ITaskbarList (5), ITaskbarList2 (1), then ITaskbarList3.
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

/// Win32 window-placement read (`GetWindowPlacement`, user32) for
/// maximized-state persistence.
///
/// winit's `is_maximized` reports only the current show state (wrong for a
/// window minimized from maximized) and exposes no `rcNormalPosition`, the rect
/// the OS restores down to. While maximized, `outer_position()` is the monitor
/// corner, which a restore must not persist. One placement call answers both.
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

    /// `WINDOWPLACEMENT` — 44 bytes on every Windows ABI aterm builds for
    /// (4×u32-ish header + two POINTs + one RECT); `length` must be pre-set.
    #[repr(C)]
    struct WindowPlacement {
        length: u32,
        flags: u32,
        show_cmd: u32,
        min_position: Point,
        max_position: Point,
        normal_position: Rect,
    }

    /// `SW_SHOWMAXIMIZED` / `SW_SHOWMINIMIZED` show commands.
    const SW_SHOWMINIMIZED: u32 = 2;
    const SW_SHOWMAXIMIZED: u32 = 3;
    /// `WPF_RESTORETOMAXIMIZED`: minimized now but returns to maximized, so it
    /// counts as maximized.
    const WPF_RESTORETOMAXIMIZED: u32 = 0x2;
    /// `SPI_GETWORKAREA`: the primary monitor's work area in screen coordinates,
    /// the origin of the workspace space `rcNormalPosition` uses.
    const SPI_GETWORKAREA: u32 = 0x0030;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetWindowPlacement(hwnd: isize, placement: *mut WindowPlacement) -> i32;
        // Signature matches `platform_win.rs`'s declaration of this import
        // (`*mut c_void`) to avoid `clashing_extern_declarations`.
        fn SystemParametersInfoW(
            action: u32,
            ui_param: u32,
            pv_param: *mut std::ffi::c_void,
            fw_ini: u32,
        ) -> i32;
    }

    /// `(maximized, normal-rect top-left in screen physical px)` for `window`, or
    /// `None` when the HWND is unreachable or the call fails (capture then uses
    /// `outer_position()`).
    ///
    /// `rcNormalPosition` is in workspace coordinates (screen shifted by the
    /// primary work-area origin); they coincide only with a bottom taskbar.
    /// `SPI_GETWORKAREA` supplies the offset; a failed read uses (0,0).
    pub(crate) fn read(window: &aterm_window::window::Window) -> Option<(bool, (i32, i32))> {
        use aterm_window::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = match window.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get(),
            _ => return None,
        };
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
    /// The device px a window attaching at `scale` draws the live font size
    /// at ([`crate::font_px_for_scale`]); a zoom still carried in an older
    /// build's device px is that px. Deterministic, so the early reveal's
    /// prediction and the post-join activation agree.
    fn attach_font_px(&self, scale: f64) -> f32 {
        match self.font_size_from_device {
            Some((px, _)) => px,
            None => crate::font_px_for_scale(self.font_size, scale),
        }
    }
}

/// The font size (px at 1×) a device px drew at on a display of `scale`: the
/// inverse of [`crate::font_px_for_scale`], for a zoom an older build carried in
/// device px, clamped to the font bounds.
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

    /// The OS caption never holds a frozen spinner frame: Claude Code's moon and
    /// Codex's braille dots are dropped with the blank after them, the subject
    /// stays, and a title that is only a frame, or a glyph not followed by a
    /// blank, is the title's own text.
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

    /// A cmdlet or script inside `pwsh.exe` spawns no child, so only the block
    /// state can mark the session busy. The stub PTY says nothing (`master`/`pid`
    /// = -1, as ConPTY), and the per-session, per-window and whole-app gates must
    /// all arm from OSC 133 alone and go idle on 133;D.
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

    /// A session another window also displays is not this window's to hang up, so
    /// it does not arm this window's close gate while its block executes; the
    /// whole-app Quit gate still counts it.
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

    /// The early reveal's class-brush erase and the first presented frame are one
    /// colour: the brush seeded from `early_reveal_backdrop`, every unstyled cell
    /// and the remainder bands all resolve to the same bytes, under either OS
    /// appearance (a `dark:…,light:…` split theme is where they could differ). A
    /// theme alpha byte never reaches the brush.
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

    /// A 1440×900-point work area whose origin is not (0, 0), as with the menu-bar
    /// band or a secondary display.
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
        // Parent and child share a size (the child inherits the grid), so the wrap
        // tests the grabbable corner, not the whole frame; otherwise the child
        // would land back at the origin on top of a filled parent.
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
        // Publication does not depend on `pending_join`: an early Wake may finalize
        // the backend before `resumed`, and the first surface still owns a complete
        // attach timeline.
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

    /// Linux `chrome_theme_for_apprt`: `Auto` resolves to the active terminal
    /// theme's darkness so the CSD header tracks the terminal body. The config
    /// value itself stays system-owned (test above), and explicit Light/Dark
    /// override.
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
        // An explicit config override outranks the palette on every platform.
        app.window_theme = crate::app_config::WindowTheme::Light;
        app.theme.bg = 0x000000;
        assert_eq!(
            app.chrome_theme_for_apprt(),
            crate::app_config::WindowTheme::Light,
            "an explicit chrome override remains the operator's word"
        );
    }

    /// `chrome_palette_theme`, on every platform: Auto is the terminal theme; an
    /// explicit side matching the terminal theme keeps it; an explicit side
    /// against it swaps in the authored forced palette with the accent carried
    /// over, so a forced Window appearance turns the Settings pages too.
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

        // Forcing the side the theme already occupies changes nothing.
        app.window_theme = crate::app_config::WindowTheme::Dark;
        let matching = app.chrome_palette_theme();
        assert_eq!(
            (matching.fg, matching.bg),
            (terminal.fg, terminal.bg),
            "forcing the terminal theme's own side keeps theme-derived chrome"
        );

        // Forcing AGAINST it swaps in the authored palette, accent carried.
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
        // And the CSD variant agrees with the painters' side, so the header,
        // band and pages can only move together.
        assert_eq!(
            app.chrome_theme_for_apprt(),
            crate::app_config::WindowTheme::Light
        );
    }

    /// A Foreground or Background picked for the terminal colors the terminal,
    /// not the app's own pages: native pages draw from the color theme as it
    /// is before the per-key overrides, while forcing a side still turns them.
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

    /// The page palette and the terminal theme come from one install: a config
    /// assigned but not yet installed moves neither, and the next install
    /// moves both together.
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

    /// A whole-app trial still colors Settings when the terminal has its own
    /// foreground. Revert restores the saved page palette, and a forced window
    /// appearance still takes precedence over the trial's side.
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

    /// Forcing a side always moves the chrome, including across both former
    /// classifier bands: `#767676`..`#969696`, where the old Rec. 601 luma rule
    /// and the WCAG pole disagree, and `#979797`..`#ADADAD`, where the old render
    /// rule did. `chrome_palette_theme` and `native_appearance::roles` read the
    /// same predicate, so the pages build the side the operator asked for.
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

    /// One font-size unit: every size — the default and one someone named —
    /// draws at `round(size × scale)` on a window of that scale, the size
    /// itself at 1×, so Settings' default size (12 px on macOS, 15 on Linux,
    /// 17 on Windows) is Automatic on every display and one step up from it is
    /// one step up in every window. Negative control: the old rule drew a named
    /// size at that many device px on a 2× display, so macOS's 13 drew at 13,
    /// half of Automatic's 24.
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
        // An older build's device-px zoom reads back as the size it drew.
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
        // The resolved title is written into a caller-owned slot (the per-frame
        // path reuses one buffer); the focused session is returned.
        let mut slot = String::new();

        // With neither OSC title nor cwd, native chrome uses the same final
        // presentation fallback as the tab label.
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

    /// A synthetic two-monitor desktop: a 1× 1920×1080 primary and a 2× 2560×1440
    /// external to its right (a docked laptop).
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
        // Interior points of both monitors, including one only reachable via the
        // second rect (undock this desk to the primary alone and it must flip).
        assert!(restored_origin_visible((100.0, 100.0), &desk()));
        assert!(restored_origin_visible((2400.0, 300.0), &desk()));
        // Origin exactly at a monitor corner is the cascade's own wrap target.
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
        // The window that lived on the (now absent) external: entirely outside
        // the surviving desktop — the exact "restores off-desktop" defect.
        assert!(restored_origin_visible((2400.0, 300.0), &desk()));
        assert!(!restored_origin_visible((2400.0, 300.0), &undocked));
        // Far off every edge, and the classic minimized-sentinel corner.
        assert!(!restored_origin_visible((-32000.0, -32000.0), &desk()));
        assert!(!restored_origin_visible((0.0, 5000.0), &desk()));
    }

    #[test]
    fn restored_positions_need_a_grabbable_corner_not_just_intersection() {
        // Inside the primary but with less than the 160-pt grab minimum to the
        // right: rejected, as in `cascade_top_left_pts`'s final clamp.
        assert!(!restored_origin_visible((1920.0 - 10.0, 100.0), &desk()));
        assert!(!restored_origin_visible((100.0, 1080.0 - 10.0), &desk()));
        // The 2× monitor's grab minimum is 320 PHYSICAL px: a point 200 px from
        // its right edge fails there even though 200 > 160.
        assert!(!restored_origin_visible(
            (1920.0 + 2560.0 - 200.0, 100.0),
            &desk()
        ));
        // Nonsense geometry answers false rather than guessing.
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

    /// Arm window 0's initial-frame settle with a far deadline and `budget`
    /// corrections, returning the intended frame for the given grid.
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

    /// The GNOME shape: the first configure lands the intended size (settle waits,
    /// the echo is still in flight), the activation echo returns a CSD-header-short
    /// frame (settle corrects), and the corrected size lands (settle disarms).
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

    /// Fractional scale (125%/150%): the Wayland size round-trips through a
    /// whole-number logical size, so the surface can come back a pixel off the
    /// law's frame (1157×720 for 1156×720 at 1.5×, 24×80). The grid is exact, so
    /// the settle counts it settled; a whole-cell residue is still corrected.
    #[test]
    fn initial_frame_settle_accepts_a_band_absorbed_fractional_scale_residue() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let intended = arm_settle(&mut app, 24, 80, 3);
        let (cell_w, _) = app.win_cell_size(wid);

        // +1 px of width: unrepresentable-frame residue, absorbed by the band.
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

        // A WHOLE cell of width is a genuine column loss: still corrected.
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

    /// An exhausted correction budget disarms WITHOUT another request — the
    /// compositor that keeps insisting (tiling, an early maximize) wins.
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

    /// Past the deadline the settle disarms untouched — whatever the window is
    /// by then is the user's truth, never re-asserted.
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

    /// A window attached with a running session gets the intended frame, then
    /// GNOME's activation configure echoes the pre-CSD geometry (−35 px: three
    /// rows of macOS's face, two of Linux's), then the correction lands. The echo
    /// must never reach the engine or PTY, or an alternate-screen app sees a
    /// net-zero SIGWINCH pair and never repaints its top row.
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
        // As many rows as 35 px holds of this platform's face.
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

        // A second stale echo between issuing and receiving the first correction
        // is vetoed the same way.
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

    /// The veto never strands the grid: when an X11 WM ignores the correction (no
    /// `Resized` follows), the vetoed size parked in the trailing settle applies
    /// the window's real size. This pins the fallback only; the ordering is pinned
    /// by `initial_frame_settle_keeps_the_csd_echo_off_the_engine_and_pty`.
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

    /// The async (X11) veto parks the echo long enough for an ordinary configure
    /// round trip, so the fallback does not apply the echo before the correction;
    /// the park is capped by the settle's deadline.
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

        // A settle about to expire caps the park at its deadline.
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

    /// The synchronous grant (Wayland: `request_inner_size` returns
    /// `Some(applied)` and no `Resized` follows). The applied size replaces the
    /// echo: the engine stays at the attach grid, any parked reflow is cancelled
    /// and the window's recorded size is the applied one. A refusing compositor
    /// answers its current size, which is applied.
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
        // A stale park from an earlier event must be cancelled by the grant.
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

        // A refusing compositor (a non-stateless configure) answers with the
        // window's CURRENT size; that is applied, not the intended one.
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

    /// XTWINOPS transport: exactly the manipulation set posts `Wake::WindowOp`,
    /// stamped with the session and its owner window, operation unchanged; reports
    /// and the denied `MoveWindow` post nothing, leaving the engine's fallbacks.
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
            // Denied in-core; the transport must stay inert even if it leaked.
            W::MoveWindow { x: 10, y: 10 },
            // Reports: an async wake cannot carry a synchronous reply, so the
            // engine's in-core fallbacks own every answer.
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
            // Title-stack ops are applied in-core, never by the host.
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

    /// Only the target window's focused session may re-grid it via
    /// `ResizeWindowCells`; a background pane's request is ignored.
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

        // The BACKGROUND pane asks: gated out, nothing moves.
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

        // The focused session's request routes through the resize clamp+apply seam.
        app.on_window_op(
            wid,
            focused,
            aterm_types::WindowOperation::ResizeWindowCells { rows: 30, cols: 90 },
        );
        assert_eq!(dims(&app), (30, 90), "the focused session re-grids");
    }

    /// The spawn-stamped window is only a fallback: a session whose tab moved
    /// manipulates its current window; a dead stamp with no session match drops
    /// the operation.
    #[test]
    fn a_window_op_re_resolves_the_stale_spawn_stamp() {
        let mut app = App::headless_for_test();
        let sid = app.next_session_id;
        let wid_b = app.insert_logical_window(crate::stub_session(sid), 24, 80);

        // The session resolves its REAL window over any stamp, live or dead.
        assert_eq!(app.window_op_target(sid, WindowId(0)), Some(wid_b));
        assert_eq!(app.window_op_target(sid, WindowId(77)), Some(wid_b));
        // No session match: a live stamp is the fallback, a dead one is nothing.
        assert_eq!(app.window_op_target(9999, WindowId(0)), Some(WindowId(0)));
        assert_eq!(app.window_op_target(9999, WindowId(77)), None);

        // The stale stamp (window A) must not stop B's focused session from
        // re-gridding B, nor let it touch A.
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

        // A dead stamp with no resolvable session is a silent drop.
        app.on_window_op(
            WindowId(77),
            9999,
            aterm_types::WindowOperation::ResizeWindowCells { rows: 50, cols: 50 },
        );
        assert_eq!(dims(&app, wid_b), (30, 90));
        assert_eq!(dims(&app, WindowId(0)), (24, 80));
    }
}

/// The Windows close/quit confirm as an in-window banner (`close_confirm`): the
/// gesture is refused now and its question parked, without parking the loop. The
/// banner type compiles everywhere; the routing arm is pinned under
/// `cfg(windows)`.
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

    /// Presenting refuses the gesture and parks the question; a second gesture is
    /// refused and the first question stands; Escape drops it and nothing closes;
    /// Enter hands back the replay.
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
        // A reply-minting verb (`invoke CloseTab`) answers ERR with the pending
        // question. No wire scope was open, so the reply says a person asked.
        let refusal = app
            .pending_action_refusal
            .take()
            .expect("the parked question is reported to the wire");
        assert_eq!(refusal, "a quit is waiting for the person at the window");
        // A second gesture is refused, and the refusal names the standing question
        // (a quit), not the dropped window close.
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
        // Escape: dropped, nothing closes, the stash is cleared.
        assert_eq!(app.answer_close_banner(false), None);
        assert!(app.close_banner.is_none());
        assert!(app.windows.contains_key(&wid), "cancel closes nothing");
        assert!(app.windows[&wid].pending_close_attribution.is_none());
        // Enter: the replay is handed back (and posted, when there is a loop).
        assert!(!app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Quit));
        assert_eq!(app.answer_close_banner(true), Some(CloseReplay::Quit));
        assert!(app.close_banner.is_none());
        // No banner: answering is a no-op.
        assert_eq!(app.answer_close_banner(true), None);
    }

    /// `controls front` reports what stands and how to answer it. With nothing
    /// pending, Windows and Linux (which ask in the window) print
    /// `confirm open=false`; macOS (whose `NSAlert` asks inside the gesture)
    /// prints only the `overlay` line.
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
        // Raised on the wire: `invoke CloseTab` runs inside the caller's
        // `ctl-close` scope (`App::invoke_attributed`).
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
        // Raised by the person at the window (no wire scope): reported, with no
        // wire verb that answers it.
        app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
        let line = app
            .close_confirm_controls_line()
            .expect("a pending confirm is reported on every host");
        assert!(line.contains(" by=human "), "{line}");
        assert!(line.contains("answer=\"-\""), "{line}");
        // Answered: the report goes back to its closed shape.
        assert_eq!(app.answer_close_banner(false), None);
        assert_eq!(
            app.close_confirm_controls_line().is_some(),
            cfg!(not(target_os = "macos")),
            "closed again"
        );
    }

    /// Raise `prompt` over `wid` as `invoke CloseTab` does, through
    /// `App::invoke_attributed`, whose `ctl-close` scope makes the question the
    /// wire's to answer.
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

    /// The wire's `confirm yes|no` answers a question a control client raised,
    /// whichever window is in front. `no` drops everything including the stashed
    /// attribution; `yes` keeps the attribution for the replay; each reply names
    /// what it answered.
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
        // The person brings another window to the front.
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

    /// A question a person asked at the window (Ctrl-W, Alt+F4, the caption ✕,
    /// Quit) is refused to the wire, both ways, and stays up.
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

    /// The keyboard gate in `on_key`, with real winit key events: while a question
    /// stands over a window it owns that window's keyboard. Escape cancels, Enter
    /// answers yes (attribution kept), every other key is swallowed; a key in
    /// another window passes through. The witness is the sink's input-attempt
    /// epoch, which every PTY write moves.
    #[test]
    fn the_keyboard_answers_a_standing_question_and_nothing_leaks_under_it() {
        use aterm_window::keyboard::NamedKey;
        let sink = std::sync::Arc::new(aterm_session::sink::SinkWriter::new(-1));
        let mut app = App::headless_for_test_with_sink(std::sync::Arc::clone(&sink));
        app.headless = false;
        let wid = WindowId(0);
        // On Windows keys go through the session's ordered writer thread
        // (`paste_order::UI_WRITES_CAN_BLOCK`), so the epoch moves after `on_key`
        // returns; drain the writer before each read. A leaked key would still
        // move the epoch.
        let settled = |sink: &aterm_session::sink::SinkWriter| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while sink.ordered_egress_count() > 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        // Non-vacuity: with no question standing, a typed key reaches the sink.
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

        // A question over ANOTHER window leaves this one's keyboard alone.
        let sid = app.next_session_id;
        let other = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        app.present_close_banner(other, busy_quit_prompt(), CloseReplay::Window(other));
        app.on_key(wid, char_key("b"));
        settled(&sink);
        assert_ne!(sink.input_epoch(), before, "a key in another window passes");
        assert!(app.close_banner.is_some(), "…and answers nothing");
    }

    /// A window closing while its question stands takes the question with it — the
    /// close was the very thing being asked about.
    #[test]
    fn a_closing_window_drops_its_question() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.present_close_banner(wid, busy_quit_prompt(), CloseReplay::Window(wid));
        assert_eq!(app.close_window_logical(wid), CloseOutcome::Exit);
        assert!(app.close_banner.is_none());
    }

    /// Off macOS the interactive confirm uses the banner, never a dialog, and the
    /// loop is never parked. On every host the wire policies answer before any
    /// banner and a single idle tab closes unasked. The routing is a `cfg!` branch,
    /// so only the half that would open a real `NSAlert` is skipped on macOS.
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
            // The Quit action names its own replay.
            app.close_banner = None;
            assert!(!app.confirm_destructive_gesture(wid, true, true, CloseReplay::Quit));
            assert_eq!(
                app.close_banner.as_ref().map(|p| p.replay),
                Some(CloseReplay::Quit)
            );
        }
        // The wire policies never reach the banner.
        app.close_banner = None;
        app.close_confirm = CloseConfirm::WireRefuseBusy;
        assert!(!app.confirm_destructive_close(wid, true, true));
        assert!(app.close_banner.is_none());
        app.close_confirm = CloseConfirm::Programmatic;
        assert!(app.confirm_destructive_close(wid, true, true));
        assert!(app.close_banner.is_none());
        // A single idle tab: instant, unasked.
        app.close_confirm = CloseConfirm::Interactive;
        assert!(app.confirm_destructive_close(wid, false, false));
        assert!(app.close_banner.is_none());
    }

    /// The taskbar's "Close all windows": a person's close of a SECOND window
    /// while the first window's question stands waits its turn instead of being
    /// dropped (which left that window open once the first was answered). It is
    /// asked once the first is answered, in the words true then: the first
    /// window's close made this one the last, so on Windows its close now quits.
    /// A repeat over either window adds nothing, a control client's gesture is
    /// still refused, and a cancel asks the next question at once.
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
        // A repeat over either window adds nothing.
        app.present_close_banner(b, busy_close_prompt(), CloseReplay::Window(b));
        app.present_close_banner(a, busy_close_prompt(), CloseReplay::Window(a));
        assert_eq!(app.close_banner_queue.len(), 1);
        // A control client's gesture is refused, never queued.
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
        // `yes` over A: nothing is asked until its replay has run, which ends
        // in A's logical close.
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
            // Linux asks only while a job runs: B's idle tabs need no question
            // now, and its close proceeds as the gesture would have.
            assert!(app.close_banner.is_none());
            assert!(app.close_banner_queue.is_empty());
        }

        // A cancel asks the next question at once.
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
        // Its window closing drops it, and nothing else asks.
        assert_eq!(app.close_window_logical(b), CloseOutcome::Stay);
        assert!(app.close_banner.is_none());
    }
}
