// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drag-to-connect (`docs/design/SESSION_CONNECTIONS.md` §3.1–§3.3): the pure core of
//! the connector gesture, a state machine over `{armed, dragging, over-target}` plus
//! the screen-space window-frame registry that cross-window resolution reads. No App,
//! winit or AppKit here, so the press-release = menu vs press-move = drag rule, the
//! self-drop refusal and platform scoping are unit-testable with synthetic geometry.
//!
//! - This module: [`ConnDragState::track`] applies the movement threshold;
//!   [`ConnDragState::set_over`] is the one authority that refuses an illegal target
//!   (self-connection, fixed at arm time); [`ConnDragState::release`] yields the
//!   outcome (menu / drop / cancel). Nothing mints here: `Drop` names the pair the
//!   §3.3 confirm card opens over.
//! - `app_conn_drag.rs`: App glue (arming from the strip funnel, cursor tracking,
//!   target resolution against live windows, drop-target highlights, the wire card).
//!
//! Platform scope (§3.2): cross-window drag needs global coordinates. The
//! [`FrameRegistry`] is built from winit's `inner_position`/`inner_size`; on macOS,
//! X11 and Windows every mapped window joins, while on Wayland `inner_position` errors
//! by protocol, so no frame joins and [`FrameRegistry::to_screen`] returns `None`. The
//! drag is then same-window only with no cfg fork; the Connections menu and the
//! `connect` verb stay the cross-window path there.

use aterm_session::SessionId;

use crate::WindowId;

/// Movement threshold (§3.1) in the in-grid strip's physical pixels: less total
/// travel opens the Connections menu on release, more commits to a drag. The native
/// macOS strip applies its own point-space threshold before posting
/// `Wake::ConnDragBegin` (see `toolbar.rs`).
pub(crate) const CONN_DRAG_THRESHOLD_PX: f64 = 6.0;

/// The session under the drag cursor, resolved to drop grain (§3.3): `T` is
/// the session of the pane or tab chip under the point.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ConnDropTarget {
    /// The window hosting the target — the confirm card opens HERE (§3.3:
    /// "focus T's window first").
    pub(crate) window: WindowId,
    /// Tab index in the target window carrying the drop highlight (the chip under the
    /// cursor, or the active tab for a pane hit). Presentation only; `session` is what
    /// the drop acts on.
    pub(crate) chip: Option<usize>,
    /// T itself.
    pub(crate) session: SessionId,
}

/// The live connector gesture: one per process (a mouse has one button down).
///
/// scope-waiver: a pointer fact, not a multiplying enforcer; the slot is
/// `App::conn_drag: Option<ConnDragState>`, and a second gesture would draw a second
/// wire, not raise a second budget.
#[derive(Clone, Debug)]
pub(crate) struct ConnDragState {
    /// Window whose connector was pressed; local cursor coordinates and the wire are
    /// this window's (§3.2: the wire renders only on the source surface).
    pub(crate) src_window: WindowId,
    /// The pressed tab's index at arm time — the release-in-place menu opens
    /// for this tab (§3.1).
    pub(crate) src_tab: usize,
    /// S — the session whose connector was grabbed.
    pub(crate) src_session: SessionId,
    /// True for a drag from the native macOS strip: `origin`/`cursor` hold winit-space
    /// screen coordinates (posted by AppKit tracking wakes), and no in-grid wire is
    /// drawn; chip highlights and the cursor carry the signal (§3.2).
    pub(crate) native: bool,
    /// Where the press landed (local px; screen px when `native`).
    pub(crate) origin: (f64, f64),
    /// The latest tracked cursor position (same space as `origin`).
    pub(crate) cursor: (f64, f64),
    /// Whether the gesture crossed the §3.1 threshold: `false` = still armed
    /// (release opens the menu), `true` = a connection drag (release drops).
    pub(crate) dragging: bool,
    /// The legal target currently under the cursor, if any. Written ONLY by
    /// [`Self::set_over`], which refuses self-connection.
    pub(crate) over: Option<ConnDropTarget>,
    /// Screen-space frame registry snapshotted when the drag began (empty while
    /// armed). Windows do not move mid-drag, so one snapshot per gesture suffices and
    /// per-motion cost stays arithmetic.
    pub(crate) frames: FrameRegistry,
}

/// What a released gesture settles to. Nothing mints in any arm; `Drop` feeds
/// `App::open_confirm_card` (§3.3).
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum ConnDragOutcome {
    /// Release within the threshold: the §3.1 press-release ⇒ the existing
    /// Connections menu, for the pressed tab.
    OpenMenu { window: WindowId, tab: usize },
    /// Release past the threshold over a legal target: open the confirm card
    /// primed S→T (both) in the target's window.
    Drop {
        src: SessionId,
        target: ConnDropTarget,
    },
    /// Release past the threshold over nothing legal: the drag dissolves.
    Cancel,
}

impl ConnDragState {
    /// Arm the gesture on a connector press (in-grid renderer). Not yet a
    /// drag: [`Self::track`] decides menu-vs-drag from the travel.
    pub(crate) fn arm(
        src_window: WindowId,
        src_tab: usize,
        src_session: SessionId,
        at: (f64, f64),
    ) -> Self {
        Self {
            src_window,
            src_tab,
            src_session,
            native: false,
            origin: at,
            cursor: at,
            dragging: false,
            over: None,
            frames: FrameRegistry::default(),
        }
    }

    /// Begin a native-strip drag already past AppKit's threshold (`Wake::ConnDragBegin`):
    /// born dragging, in screen coordinates. The strip opens its own `NSMenu` for a
    /// release in place, so that path never reaches the App.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn native_drag(
        src_window: WindowId,
        src_tab: usize,
        src_session: SessionId,
    ) -> Self {
        Self {
            src_window,
            src_tab,
            src_session,
            native: true,
            origin: (f64::NAN, f64::NAN),
            cursor: (f64::NAN, f64::NAN),
            dragging: true,
            over: None,
            frames: FrameRegistry::default(),
        }
    }

    /// Track the cursor to `to` (in `origin`'s space) and apply the §3.1 threshold.
    /// Returns `true` exactly when this call moves armed to dragging (the caller then
    /// snapshots the frame registry and starts target resolution).
    pub(crate) fn track(&mut self, to: (f64, f64)) -> bool {
        self.cursor = to;
        if self.dragging {
            return false;
        }
        let (dx, dy) = (to.0 - self.origin.0, to.1 - self.origin.1);
        if dx * dx + dy * dy > CONN_DRAG_THRESHOLD_PX * CONN_DRAG_THRESHOLD_PX {
            self.dragging = true;
            return true;
        }
        false
    }

    /// Whether `target` is legal for this drag: anything but the source session.
    /// `set_over` consults it, so an illegal target never becomes the highlighted
    /// candidate (the confirm card also refuses self-drop).
    pub(crate) fn accepts(&self, target: &ConnDropTarget) -> bool {
        target.session != self.src_session
    }

    /// Install the resolved target under the cursor (`None` = over nothing). Illegal
    /// targets store as `None`. Returns whether the stored value changed (the caller
    /// then pushes highlight updates).
    pub(crate) fn set_over(&mut self, target: Option<ConnDropTarget>) -> bool {
        let legal = target.filter(|t| self.accepts(t));
        if self.over == legal {
            return false;
        }
        self.over = legal;
        true
    }

    /// Settle the released gesture into its outcome (§3.1 / §3.3). Consumes
    /// the state: the gesture is over whatever happens next.
    pub(crate) fn release(self) -> ConnDragOutcome {
        if !self.dragging {
            return ConnDragOutcome::OpenMenu {
                window: self.src_window,
                tab: self.src_tab,
            };
        }
        match self.over {
            Some(target) => ConnDragOutcome::Drop {
                src: self.src_session,
                target,
            },
            None => ConnDragOutcome::Cancel,
        }
    }
}

/// One window's client-area rectangle in winit screen space (physical px,
/// top-left origin): `inner_position` + `inner_size`, captured at drag begin.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct WindowFrame {
    pub(crate) window: WindowId,
    /// Client-area top-left in screen space (`inner_position`, not `outer_position`,
    /// since local cursor coordinates are client-relative).
    pub(crate) origin: (f64, f64),
    /// Client-area size (`inner_size`).
    pub(crate) size: (f64, f64),
}

impl WindowFrame {
    fn contains(&self, p: (f64, f64)) -> bool {
        p.0 >= self.origin.0
            && p.0 < self.origin.0 + self.size.0
            && p.1 >= self.origin.1
            && p.1 < self.origin.1 + self.size.1
    }
}

/// The window-frame registry (§3.2): every mapped window whose position the backend
/// reports. Empty on Wayland, which scopes the drag to the source window.
#[derive(Clone, Default, Debug)]
pub(crate) struct FrameRegistry {
    frames: Vec<WindowFrame>,
}

impl FrameRegistry {
    pub(crate) fn push(&mut self, frame: WindowFrame) {
        self.frames.push(frame);
    }

    fn frame_of(&self, window: WindowId) -> Option<&WindowFrame> {
        self.frames.iter().find(|f| f.window == window)
    }

    /// Map `window`-local client coordinates to screen space, or `None` when the
    /// window has no reported frame (Wayland, headless).
    pub(crate) fn to_screen(&self, window: WindowId, local: (f64, f64)) -> Option<(f64, f64)> {
        let f = self.frame_of(window)?;
        Some((f.origin.0 + local.0, f.origin.1 + local.1))
    }

    /// The window under screen point `p`, with `p` in that window's local client
    /// coordinates. winit exposes no z-order, so overlap is resolved by `prefer` (the
    /// focused window, then the drag source); other windows tie-break in registry
    /// order, an approximation.
    pub(crate) fn locate(
        &self,
        p: (f64, f64),
        prefer: &[WindowId],
    ) -> Option<(WindowId, (f64, f64))> {
        let f = prefer
            .iter()
            .find_map(|w| self.frame_of(*w).filter(|f| f.contains(p)))
            .or_else(|| self.frames.iter().find(|f| f.contains(p)))?;
        Some((f.window, (p.0 - f.origin.0, p.1 - f.origin.1)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u8) -> SessionId {
        SessionId::new(format!("s-{n:020x}"))
    }

    fn target(window: u64, chip: usize, session: &SessionId) -> ConnDropTarget {
        ConnDropTarget {
            window: WindowId(window),
            chip: Some(chip),
            session: session.clone(),
        }
    }

    /// §3.1: a press stays armed within the threshold (release opens the menu for the
    /// pressed tab), and travel past it commits to a drag exactly once.
    #[test]
    fn press_release_in_place_opens_the_menu_and_travel_commits_the_drag() {
        let s = sid(1);
        let mut d = ConnDragState::arm(WindowId(7), 2, s.clone(), (100.0, 10.0));
        // A hand wobble under the threshold is not a drag.
        assert!(!d.track((102.0, 11.0)));
        assert!(!d.dragging);
        assert_eq!(
            d.release(),
            ConnDragOutcome::OpenMenu {
                window: WindowId(7),
                tab: 2
            }
        );

        let mut d = ConnDragState::arm(WindowId(7), 2, s, (100.0, 10.0));
        assert!(!d.track((101.0, 10.0)), "still armed");
        assert!(d.track((110.0, 10.0)), "crossing commits");
        assert!(d.dragging);
        assert!(!d.track((120.0, 10.0)), "commits only ONCE");
        assert_eq!(d.cursor, (120.0, 10.0), "tracking follows regardless");
    }

    /// Self-drop is refused structurally: S can never become the stored
    /// target, so a release over it cancels rather than drops (§3.3).
    #[test]
    fn self_target_is_refused_and_release_over_it_cancels() {
        let s = sid(1);
        let mut d = ConnDragState::arm(WindowId(0), 0, s.clone(), (0.0, 0.0));
        assert!(d.track((50.0, 0.0)));
        let self_target = target(0, 0, &s);
        assert!(!d.accepts(&self_target));
        assert!(!d.set_over(Some(self_target)), "stored value never changed");
        assert!(d.over.is_none());
        assert_eq!(d.release(), ConnDragOutcome::Cancel);
    }

    /// A legal target is stored (with change detection for the highlight
    /// pushes) and the release drops S→T.
    #[test]
    fn legal_target_tracks_with_change_detection_and_release_drops() {
        let (s, t) = (sid(1), sid(2));
        let mut d = ConnDragState::arm(WindowId(0), 0, s.clone(), (0.0, 0.0));
        assert!(d.track((50.0, 0.0)));
        let over = target(3, 1, &t);
        assert!(d.set_over(Some(over.clone())), "first sighting changes");
        assert!(!d.set_over(Some(over.clone())), "same target: no change");
        assert!(d.set_over(None), "leaving changes back");
        assert!(d.set_over(Some(over.clone())));
        assert_eq!(
            d.release(),
            ConnDragOutcome::Drop {
                src: s,
                target: over
            }
        );
    }

    /// Release over nothing (or after the target left) cancels; nothing in
    /// this module ever mints.
    #[test]
    fn release_over_nothing_cancels() {
        let mut d = ConnDragState::arm(WindowId(0), 0, sid(1), (0.0, 0.0));
        assert!(d.track((0.0, 50.0)));
        assert_eq!(d.release(), ConnDragOutcome::Cancel);
    }

    /// A native-strip drag is born dragging, so a release with no target cancels and
    /// never opens a menu (the native strip opens its own).
    #[test]
    fn native_drag_is_born_dragging() {
        let d = ConnDragState::native_drag(WindowId(1), 0, sid(1));
        assert!(d.dragging);
        assert_eq!(d.release(), ConnDragOutcome::Cancel);
    }

    /// Registry hit-testing with synthetic frames: local→screen→foreign-local
    /// round-trips through the arithmetic the App layer uses.
    #[test]
    fn registry_maps_screen_points_to_foreign_windows() {
        let mut reg = FrameRegistry::default();
        reg.push(WindowFrame {
            window: WindowId(0),
            origin: (100.0, 100.0),
            size: (800.0, 600.0),
        });
        reg.push(WindowFrame {
            window: WindowId(1),
            origin: (1000.0, 50.0),
            size: (400.0, 300.0),
        });
        // A source-local point far right of window 0 lands in window 1.
        let screen = reg.to_screen(WindowId(0), (950.0, 20.0)).unwrap();
        assert_eq!(screen, (1050.0, 120.0));
        let (w, local) = reg.locate(screen, &[WindowId(0)]).unwrap();
        assert_eq!(w, WindowId(1));
        assert_eq!(local, (50.0, 70.0));
        // A point over no window resolves to nothing.
        assert!(reg.locate((0.0, 0.0), &[]).is_none());
        // Edges are half-open: the first in-frame pixel hits, one past does not.
        assert!(reg.locate((100.0, 100.0), &[]).is_some());
        assert!(reg.locate((900.0, 100.0), &[]).is_none());
    }

    /// Overlap resolves by the `prefer` ranking (focused first), because winit
    /// exposes no z-order to consult.
    #[test]
    fn registry_overlap_prefers_the_ranked_window() {
        let mut reg = FrameRegistry::default();
        for (id, origin) in [(0u64, (0.0, 0.0)), (1, (50.0, 0.0))] {
            reg.push(WindowFrame {
                window: WindowId(id),
                origin,
                size: (200.0, 200.0),
            });
        }
        let p = (100.0, 100.0); // inside both
        assert_eq!(reg.locate(p, &[WindowId(1)]).unwrap().0, WindowId(1));
        assert_eq!(reg.locate(p, &[WindowId(0)]).unwrap().0, WindowId(0));
        // No ranking: registry order (recorded approximation).
        assert_eq!(reg.locate(p, &[]).unwrap().0, WindowId(0));
    }

    /// Wayland scoping (§3.2): with no frame for the source window (what
    /// `inner_position`'s protocol error yields), screen mapping refuses and target
    /// resolution stays same-window only.
    #[test]
    fn missing_source_frame_scopes_the_drag_to_the_source_window() {
        let reg = FrameRegistry::default();
        assert!(reg.to_screen(WindowId(0), (10.0, 10.0)).is_none());
        // Even with OTHER windows present (a hypothetical partial report),
        // a source with no frame cannot produce a screen point to locate.
        let mut partial = FrameRegistry::default();
        partial.push(WindowFrame {
            window: WindowId(1),
            origin: (0.0, 0.0),
            size: (100.0, 100.0),
        });
        assert!(partial.to_screen(WindowId(0), (10.0, 10.0)).is_none());
    }
}
