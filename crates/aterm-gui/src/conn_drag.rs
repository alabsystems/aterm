// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Drag-to-connect (design §3.1–§3.3): the PURE core of the connector gesture —
//! a state machine over `{armed, dragging, over-target}` plus the screen-space
//! window-frame registry the cross-window resolution reads. No App, no winit,
//! no AppKit in here, so every disambiguation rule (§3.1's press-release =
//! menu vs press-move = drag), the self-drop refusal, and the platform scoping
//! are unit-testable with synthetic geometry.
//!
//! Ownership split (mirrors `conn_card.rs` / `app_conn_card.rs`):
//! - THIS module: the gesture facts and transitions. [`ConnDragState::track`]
//!   applies the §3.1 movement threshold; [`ConnDragState::set_over`] is the
//!   ONE authority that refuses an illegal target (self-connection — installed
//!   at arm time, so no later step can re-admit it); [`ConnDragState::release`]
//!   turns the final state into the committed outcome (menu / drop / cancel).
//!   Nothing here mints: a `Drop` outcome only names the pair the §3.3 confirm
//!   card is then opened over.
//! - `app_conn_drag.rs`: the App glue — arming from the strip funnel, cursor
//!   tracking, target resolution against live windows, the drop-target
//!   highlight pushes, and the source-window wire card.
//!
//! PLATFORM SCOPE (§3.2): cross-window drag needs global coordinates. The
//! [`FrameRegistry`] is built from `winit`'s `inner_position`/`inner_size` —
//! on macOS / X11 / Windows every mapped window joins; on WAYLAND
//! `inner_position` errors by protocol (the compositor owns global space), so
//! no frame joins and [`FrameRegistry::to_screen`] returns `None` — the drag
//! is then structurally same-window-only there (the runtime detection IS the
//! registry: no cfg fork, no second code path). The Connections menu and the
//! `connect` verb remain the cross-window path on Wayland.

use aterm_session::SessionId;

use crate::WindowId;

/// §3.1 "small movement threshold", in the in-grid strip's physical pixels:
/// press + release with less total travel than this opens the Connections
/// menu; crossing it commits the gesture to a connection drag. (The native
/// macOS strip applies its own point-space twin before posting
/// `Wake::ConnDragBegin` — see `toolbar.rs`.)
pub(crate) const CONN_DRAG_THRESHOLD_PX: f64 = 6.0;

/// The session under the drag cursor, resolved to drop grain (§3.3): `T` is
/// the session of the pane or tab chip under the point.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct ConnDropTarget {
    /// The window hosting the target — the confirm card opens HERE (§3.3:
    /// "focus T's window first").
    pub(crate) window: WindowId,
    /// The target window's tab index carrying the drop-target highlight (the
    /// chip under the cursor, or the active tab for a pane hit). Presentation
    /// only — `session` is the authority the drop acts on.
    pub(crate) chip: Option<usize>,
    /// T itself.
    pub(crate) session: SessionId,
}

/// The live connector gesture: one per process (a mouse has one button down).
///
/// scope-waiver: this names a POINTER fact, not a multiplying enforcer. The
/// slot is `App::conn_drag: Option<ConnDragState>` — one `App` per process by
/// construction of the binary, and `Option` cannot hold two — so no instance
/// count over this type could falsify the sentence. A second live gesture
/// would draw a second wire, not raise a second budget over one retina, which
/// is the hazard class this census exists for; and `Scope` has no `Process`
/// variant to pin an ownership chain against.
#[derive(Clone, Debug)]
pub(crate) struct ConnDragState {
    /// The window whose connector was pressed — local cursor coordinates and
    /// the wire rendering are THIS window's (§3.2: the wire renders only on
    /// the source window's own surface).
    pub(crate) src_window: WindowId,
    /// The pressed tab's index at arm time — the release-in-place menu opens
    /// for this tab (§3.1).
    pub(crate) src_tab: usize,
    /// S — the session whose connector was grabbed.
    pub(crate) src_session: SessionId,
    /// True for a drag originating on the NATIVE macOS strip: `origin`/`cursor`
    /// then hold winit-space SCREEN coordinates (posted by the AppKit tracking
    /// wakes) rather than source-window-local ones, and the in-grid wire is
    /// not drawn (the source window has no in-grid strip to anchor it — the
    /// pushed chip highlights + the cursor carry the signal, §3.2).
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
    /// The screen-space frame registry snapshotted when the drag began (empty
    /// while merely armed). Windows do not move mid-drag — the hand doing the
    /// moving is on the connector — so one snapshot per gesture is exact
    /// enough and keeps per-motion cost to arithmetic.
    pub(crate) frames: FrameRegistry,
}

/// What a completed (released) gesture settles to. Consumed by the App layer;
/// nothing mints in any arm — `Drop` feeds `App::open_confirm_card` (§3.3:
/// nothing mints before confirm).
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

    /// Begin a NATIVE-strip drag already past AppKit-side threshold (the
    /// `Wake::ConnDragBegin` entry): born dragging, screen-space coordinates.
    /// The release-in-place menu path never reaches the App for a native
    /// press — the strip opens its own `NSMenu` in-process.
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

    /// Track the cursor to `to` (the space `origin` is in). Applies the §3.1
    /// movement threshold; returns `true` exactly when THIS call commits the
    /// gesture from armed to dragging (the caller then snapshots the frame
    /// registry and begins target resolution).
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

    /// Whether `target` is legal for THIS drag: anything but S itself. The
    /// refusal is structural from arm time — `set_over` consults it, so an
    /// illegal target can never even become the highlighted candidate (§3.3
    /// self-drop refused; the confirm card re-refuses defensively).
    pub(crate) fn accepts(&self, target: &ConnDropTarget) -> bool {
        target.session != self.src_session
    }

    /// Install the resolved target under the cursor (`None` = over nothing).
    /// Illegal targets are refused here — the one write authority — and store
    /// as `None`. Returns whether the stored value CHANGED (the caller then
    /// pushes highlight updates to the affected windows).
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
    /// Client-area top-left in screen space (`inner_position`) — local cursor
    /// coordinates are client-relative, so THIS (not `outer_position`) is the
    /// translation the screen mapping needs.
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

/// The window-frame registry (§3.2): every mapped window whose position the
/// backend can actually report. On Wayland that is NONE (see the module doc),
/// which scopes the drag to the source window with no platform fork here.
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

    /// Map `window`-local client coordinates to screen space, or `None` when
    /// that window has no reported frame (Wayland; headless) — the caller then
    /// stays same-window-scoped.
    pub(crate) fn to_screen(&self, window: WindowId, local: (f64, f64)) -> Option<(f64, f64)> {
        let f = self.frame_of(window)?;
        Some((f.origin.0 + local.0, f.origin.1 + local.1))
    }

    /// The window under screen point `p`, with `p` re-expressed in that
    /// window's local client coordinates. winit exposes no z-order, so overlap
    /// is resolved by the caller-supplied `prefer` ranking (the focused window
    /// first, then the drag source — the two the gesture can actually be
    /// over); windows outside that ranking tie-break in registry order, an
    /// honest approximation recorded in the §3.2 notes.
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

    /// §3.1 disambiguation: a press stays ARMED within the threshold — the
    /// release then opens the menu for the pressed tab — and travel past the
    /// threshold commits to a drag exactly once.
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

    /// A native-strip drag is born dragging (AppKit applied its own point
    /// threshold before posting), so a wake-driven release with no target is a
    /// cancel, never a menu-open (the native strip opens its menu itself).
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

    /// WAYLAND SCOPING (§3.2): with no frame reported for the source window —
    /// exactly what `inner_position`'s protocol error produces there — the
    /// screen mapping refuses, so target resolution stays same-window-only.
    /// No cfg fork: the registry's emptiness IS the runtime detection.
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
