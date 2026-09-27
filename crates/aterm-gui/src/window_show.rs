// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE WINDOW SHOW STATE ACROSS AN UPDATE (gap #29): full screen, minimized,
//! the window stack and the key window.
//!
//! Before this, a seamless update carried every window's grid, position and
//! tabs but none of how the window was SHOWN. The harm was concrete: a window
//! in native full screen came back as an ordinary window on the desktop — its
//! Space closed with the outgoing process and nothing re-entered it — and with
//! two or more windows the successor keyed whichever window it happened to
//! reveal LAST, so the next keystroke after Commit could land in a window the
//! user was not looking at. A minimized window popped back up and stayed up.
//!
//! The pieces, in the order an update meets them:
//!
//! * CAPTURE (outgoing, macOS only) — `App::capture_restore_manifest` records a
//!   [`WindowShow`] per window through [`captured_show`]. Full screen and
//!   minimized are read off the live window (winit's full-screen state and
//!   `isMiniaturized`); the stack and the key window are read off state the App
//!   ALREADY keeps — the focus MRU (`App::focus_order`) and `frontmost_window` —
//!   through [`stacking_ranks`], so no new AppKit query is asked. On macOS a
//!   window that gains focus is raised, so the order windows last gained focus
//!   IS their stacking order, for every window the user brought forward.
//! * COMMIT COMPARISON — the show state is LIVE, like the position, and
//!   `commit_layout_topology` normalizes it out, so zooming, minimizing or
//!   clicking another window while the successor boots can never revoke a
//!   healthy Commit.
//! * RESTORE (successor, and a cold launch that restores a quit's manifest) —
//!   `apply_restore_manifest` records which live window each manifest window
//!   became, [`plan`] turns the carried states into a [`ShowPlan`], and
//!   `App::settle_carried_window_show` applies it in two stages:
//!   1. THE STACK, once every carried window is on glass: minimize what was
//!      minimized, then raise the rest BACK TO FRONT with the key window last
//!      ([`ShowPlan::stacking_ops`]). The windows are CREATED in manifest order —
//!      window 0 is the bootstrap window whose session was spawned before the
//!      restore runs, and manifest order is what the layout digest and the
//!      Commit comparison are taken over — so the stack is re-established after
//!      creation rather than by creating in stack order. On the update lane this
//!      runs FROM THE PROOF PATH and nowhere else, in the same call that writes
//!      the adoption proof: so it is in place before the outgoing process
//!      activates the successor at Commit and AppKit keys the window the stack
//!      names, and a window it minimizes can never be one the proof still waits
//!      to see paint (a minimized window presents nothing; see
//!      `App::settle_carried_window_show`). Raising is winit's
//!      `set_visible(true)`, the same `makeKeyAndOrderFront:` the reveal
//!      already sends: it orders the window front within the app and never
//!      activates the app.
//!   2. FULL SCREEN, only after Commit AND only while aterm is the active app
//!      ([`ShowPlan::fullscreen_ops`], key window last so the user ends in its
//!      Space). Earlier is not safe: entering full screen resizes the window,
//!      and before Commit the successor shares every PTY with the parked
//!      outgoing process — a resize would SIGWINCH the programs of a process
//!      that may yet be told to resume. And while the user is in another app, a
//!      full-screen transition would pull them into aterm's new Space, so it
//!      waits for aterm to be in front again (a later update carries a window
//!      whose re-entry is still waiting as full screen, so the state is never
//!      lost by waiting). "In front" is read only off focus records a `Focused`
//!      event has confirmed (`CarriedShow::heard`): a window the restore just
//!      created starts out recorded as focused. At Commit the key window is
//!      made key once more, unless the user already typed into another carried
//!      window.
//!
//! FAIL SAFE BY CONSTRUCTION: a manifest whose windows carry no show state at
//! all (every non-macOS capture, a headless capture, every older manifest)
//! plans NOTHING, so those restores behave exactly as they did before this
//! module; a window that is gone or never got an OS window is skipped; a cold
//! launch never restores EVERY window minimized (the launch the user asked for
//! shows the frontmost one). The AppKit half — what the stack, the key and the
//! full-screen re-entry look like on glass — is UNMEASURED: it was written
//! against AppKit's documented behaviour and winit's macOS source, and no
//! windowed update has been driven to watch it. What IS pinned is the App's
//! half: the plan, the stages' order and gates, and the bookkeeping
//! (`window_show_conformance` replays it against `NativeUpdateWindowShow`).

use crate::WindowId;
use crate::restore::WindowShow;

/// Front-to-back stacking ranks for `live` windows (aligned index for index; 0
/// is the frontmost), from the focus MRU `focus_order` (most recent LAST).
///
/// Windows that gained focus rank by how recently they did. A window that
/// NEVER gained focus ranks IN FRONT of every one that did, newest first: its
/// reveal ordered it front within the app, and it can only still be unfocused
/// if the app has not been active since — so no focus gain has reordered the
/// others above it. (A restore seeds the MRU from the stack it applied, so the
/// windows an update restores while the user is in another app are not in this
/// class; see `App::settle_carried_window_show`.)
#[cfg(any(target_os = "macos", test))]
pub(crate) fn stacking_ranks<W: Copy + Ord>(live: &[W], focus_order: &[W]) -> Vec<u32> {
    let mut front_to_back: Vec<W> = live
        .iter()
        .copied()
        .filter(|window| !focus_order.contains(window))
        .collect();
    // Newest first: window ids are allocated in increasing order.
    front_to_back.sort_unstable_by(|a, b| b.cmp(a));
    for window in focus_order.iter().rev() {
        if live.contains(window) && !front_to_back.contains(window) {
            front_to_back.push(*window);
        }
    }
    live.iter()
        .map(|window| {
            let rank = front_to_back
                .iter()
                .position(|candidate| candidate == window)
                .unwrap_or(front_to_back.len());
            u32::try_from(rank).unwrap_or(u32::MAX)
        })
        .collect()
}

/// What the capture read off one live OS window.
#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShowProbe {
    /// winit's full-screen state for the window.
    pub fullscreen: bool,
    /// `isMiniaturized`; `None` when the platform would not say.
    pub minimized: Option<bool>,
}

/// One window's captured [`WindowShow`]. `probe` is `None` for a window with
/// no OS window (a headless instance, a window not attached yet) — nothing is
/// known about how it is shown, so nothing is recorded. `fullscreen_pending`
/// is a carried full-screen re-entry this process has not performed yet
/// (aterm has not been in front since the update): the window is recorded as
/// full screen, so a second update before the user returns does not forget it.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn captured_show(
    probe: Option<ShowProbe>,
    rank: u32,
    key: bool,
    fullscreen_pending: bool,
) -> WindowShow {
    let Some(probe) = probe else {
        return WindowShow::UNKNOWN;
    };
    let minimized = probe.minimized;
    WindowShow {
        // A window cannot be both: AppKit refuses to miniaturize a full-screen
        // window, and a pending re-entry is never planned for a minimized one.
        fullscreen: Some((probe.fullscreen || fullscreen_pending) && minimized != Some(true)),
        minimized,
        z_order: Some(rank),
        key: Some(key),
    }
}

/// Which restore is being planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShowLane {
    /// A seamless update's successor: reproduce the outgoing process exactly.
    Handoff,
    /// A cold launch restoring a quit's manifest: the same, except that it
    /// never leaves every window minimized.
    Cold,
}

/// What a restore does with the carried show states. Built by [`plan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShowPlan<W> {
    /// Windows to miniaturize, back to front.
    pub minimize: Vec<W>,
    /// Windows to raise, BACK TO FRONT, the key window LAST. Empty when the
    /// plan names a single window: its reveal already made it key and front.
    pub raise: Vec<W>,
    /// The window typing should go to: the carried key window when it is not
    /// minimized, else the frontmost window that is not.
    pub key: Option<W>,
    /// Windows to re-enter full screen, back to front, the key window LAST.
    pub fullscreen: Vec<W>,
    /// The focus MRU this stack implies, most recent LAST — what the restore
    /// seeds `App::focus_order` with, so the next capture reads the stack it
    /// applied even if the app never gains focus in between.
    pub focus_seed: Vec<W>,
}

impl<W> ShowPlan<W> {
    /// Whether the plan has nothing to DO. A key window alone is not work: with
    /// one window on screen the reveal already made it key, so a single-window
    /// update that was neither minimized nor in full screen — the common case —
    /// records no carry and makes no AppKit call beyond the ones it always made.
    pub(crate) fn is_empty(&self) -> bool {
        self.minimize.is_empty() && self.raise.is_empty() && self.fullscreen.is_empty()
    }
}

/// One window operation a restore performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShowOp<W> {
    /// Miniaturize into the Dock (winit `set_minimized(true)`).
    Minimize(W),
    /// Order front within the app and make it the app's key window, WITHOUT
    /// activating the app (winit `set_visible(true)`, `makeKeyAndOrderFront:`).
    Raise(W),
    /// Enter native full screen on its current display (winit
    /// `set_fullscreen(Borderless(None))`).
    EnterFullScreen(W),
}

impl<W: Copy> ShowPlan<W> {
    /// Stage 1, the stack: minimize first, so the key window the raises settle
    /// on is the last window AppKit was told to key, then raise back to front.
    pub(crate) fn stacking_ops(&self) -> Vec<ShowOp<W>> {
        self.minimize
            .iter()
            .map(|&window| ShowOp::Minimize(window))
            .chain(self.raise.iter().map(|&window| ShowOp::Raise(window)))
            .collect()
    }

    /// Stage 2, full screen: back to front, the key window last.
    pub(crate) fn fullscreen_ops(&self) -> Vec<ShowOp<W>> {
        self.fullscreen
            .iter()
            .map(|&window| ShowOp::EnterFullScreen(window))
            .collect()
    }
}

/// Plan a restore of `windows` — `(live window, carried show state)` in
/// manifest order.
///
/// Returns an EMPTY plan when no window carries any show state, so a manifest
/// from a platform or a build that does not capture it restores exactly as it
/// did before. The stack is the carried `z_order` (front to back; a window
/// without one goes behind every window with one; ties keep manifest order).
pub(crate) fn plan<W: Copy + Eq>(windows: &[(W, WindowShow)], lane: ShowLane) -> ShowPlan<W> {
    let empty = ShowPlan {
        minimize: Vec::new(),
        raise: Vec::new(),
        key: None,
        fullscreen: Vec::new(),
        focus_seed: Vec::new(),
    };
    if windows.iter().all(|(_, show)| show.is_unknown()) {
        return empty;
    }
    // Front to back: stable, so ties keep manifest order.
    let mut front_to_back: Vec<(W, WindowShow)> = windows.to_vec();
    front_to_back.sort_by_key(|(_, show)| show.z_order.unwrap_or(u32::MAX));
    let mut minimized: Vec<W> = front_to_back
        .iter()
        .filter(|(_, show)| show.minimized == Some(true))
        .map(|(window, _)| *window)
        .collect();
    // A cold launch the user started must show them a window.
    if lane == ShowLane::Cold && minimized.len() == front_to_back.len() {
        minimized.retain(|window| *window != front_to_back[0].0);
    }
    let visible: Vec<W> = front_to_back
        .iter()
        .map(|(window, _)| *window)
        .filter(|window| !minimized.contains(window))
        .collect();
    let key = windows
        .iter()
        .find(|(window, show)| show.key == Some(true) && visible.contains(window))
        .map(|(window, _)| *window)
        .or_else(|| visible.first().copied());
    // Back to front, the key window moved to the end.
    let back_to_front = |candidates: Vec<W>| -> Vec<W> {
        let mut ordered: Vec<W> = candidates.into_iter().rev().collect();
        if let Some(key) = key
            && let Some(at) = ordered.iter().position(|window| *window == key)
        {
            let key = ordered.remove(at);
            ordered.push(key);
        }
        ordered
    };
    let raise = if windows.len() > 1 {
        back_to_front(visible.clone())
    } else {
        Vec::new()
    };
    let fullscreen = back_to_front(
        front_to_back
            .iter()
            .filter(|(window, show)| show.fullscreen == Some(true) && visible.contains(window))
            .map(|(window, _)| *window)
            .collect(),
    );
    let minimize: Vec<W> = minimized.iter().rev().copied().collect();
    let mut focus_seed = minimize.clone();
    focus_seed.extend(back_to_front(visible));
    ShowPlan {
        minimize,
        raise,
        key,
        fullscreen,
        focus_seed,
    }
}

/// The show state a restore carried, from the moment its windows exist until
/// the last of it is applied (`App::carried_window_show`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CarriedShow {
    pub lane: ShowLane,
    pub plan: ShowPlan<WindowId>,
    /// Stage 1 is done: the stack is on glass.
    pub stacked: bool,
    /// This process owns every window: always on a cold launch, from Commit on
    /// the update lane. Full screen waits for it.
    pub committed: bool,
    /// The carried windows whose focus the App's record reports TRUTHFULLY —
    /// what "is aterm in front" is read from. A window the restore just created
    /// starts out recorded as focused (`WindowState` is built focused) until
    /// winit delivers the `Focused(false)` it queued at creation, so its record
    /// counts only once a `Focused` event has reached it. The bootstrap window
    /// counts from the start: it was created, and its focus reported, before
    /// the restore ran.
    pub heard: Vec<WindowId>,
}

impl CarriedShow {
    /// A fresh carry for `plan`, nothing applied yet; `bootstrap` is the window
    /// that existed before the restore (manifest window 0's live window).
    pub(crate) fn new(
        lane: ShowLane,
        plan: ShowPlan<WindowId>,
        bootstrap: Option<WindowId>,
    ) -> Self {
        Self {
            lane,
            plan,
            stacked: false,
            committed: lane == ShowLane::Cold,
            heard: bootstrap.into_iter().collect(),
        }
    }

    /// Whether `window`'s carried full-screen re-entry is still to come.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn fullscreen_pending(&self, window: WindowId) -> bool {
        self.plan.fullscreen.contains(&window)
    }
}

impl crate::App {
    /// Record, at the end of the restore rebuild, the live window each manifest
    /// window became and the show state it carried; `bootstrap` is the window
    /// that existed before the rebuild. Inert headless (no OS window to show)
    /// and for a manifest that carries no show state at all.
    pub(crate) fn record_carried_window_show(
        &mut self,
        windows: &[(WindowId, WindowShow)],
        bootstrap: Option<WindowId>,
    ) {
        if self.headless {
            return;
        }
        let lane = if self.handoff_ready.is_some() || self.incoming_handoff_pending {
            ShowLane::Handoff
        } else {
            ShowLane::Cold
        };
        let plan = plan(windows, lane);
        if plan.is_empty() {
            return;
        }
        self.carried_window_show = Some(CarriedShow::new(lane, plan, bootstrap));
    }

    /// A `Focused` event reached `window` (the `WindowEvent::Focused` arm, after
    /// `on_focus` recorded it): its focus record is truthful from now on, and a
    /// gain may be exactly what a waiting full screen needs.
    pub(crate) fn carried_window_show_focus_event(
        &mut self,
        window: WindowId,
    ) -> Vec<ShowOp<WindowId>> {
        let Some(carried) = self.carried_window_show.as_mut() else {
            return Vec::new();
        };
        if !carried.heard.contains(&window) {
            carried.heard.push(window);
        }
        self.settle_carried_window_show()
    }

    /// Advance the carried show state as far as it can go now, and return the
    /// operations performed (the seam the tests read). Cheap when nothing is
    /// carried; called from `about_to_wait`, at Commit and on every focus event.
    ///
    /// While this process still OWES the adoption proof (`handoff_ready`), the
    /// stack is not this call's to apply: it goes on only from the proof path
    /// ([`Self::settle_carried_window_show_at_proof`]). The proof waits for
    /// every OS window to have a frame on record (`last_present`), which the
    /// boot clears and re-earns more than once (the fallback-face convergence
    /// clears it after every present while the async parse is in flight), and
    /// a MINIMIZED window presents nothing: its acquire parks. A stack applied
    /// in that gap — every window revealed, one not painted — could minimize a
    /// window that then never painted again, leaving the proof unwritten until
    /// the parent's timeout rolled the update back. (Read off the present path;
    /// no windowed update has been driven to watch it.)
    pub(crate) fn settle_carried_window_show(&mut self) -> Vec<ShowOp<WindowId>> {
        let may_stack = self.handoff_ready.is_none();
        self.settle_carried_window_show_as(may_stack)
    }

    /// The proof path's settle (`maybe_signal_handoff_ready`, past its last
    /// gate: every OS window painted and revealed, the restore drained, the
    /// control service ready). The stack goes on here and the proof is written
    /// in the same call, so nothing it does can hold the proof back.
    pub(crate) fn settle_carried_window_show_at_proof(&mut self) -> Vec<ShowOp<WindowId>> {
        self.settle_carried_window_show_as(true)
    }

    /// [`Self::settle_carried_window_show`]'s body; `may_stack` is whether this
    /// caller may apply the stack.
    fn settle_carried_window_show_as(&mut self, may_stack: bool) -> Vec<ShowOp<WindowId>> {
        let Some(carried) = self.carried_window_show.as_mut() else {
            return Vec::new();
        };
        // A carried window closed in the meantime is simply not shown.
        let windows = &self.windows;
        let live = |window: &WindowId| windows.contains_key(window);
        carried.plan.minimize.retain(live);
        carried.plan.raise.retain(live);
        carried.plan.fullscreen.retain(live);
        carried.plan.focus_seed.retain(live);
        if carried.plan.key.is_some_and(|key| !live(&key)) {
            carried.plan.key = None;
        }
        let mut performed = Vec::new();
        if !carried.stacked {
            // The proof is owed and this is not its path: nothing else can be
            // due either (full screen waits for the stack and for Commit).
            if !may_stack {
                return performed;
            }
            // Raising a window still waiting for its first present would put a
            // frame on glass before the carried pixels (`pending_reveal`).
            let revealed = carried.plan.focus_seed.iter().all(|window| {
                windows
                    .get(window)
                    .is_some_and(|ws| ws.pending_reveal.is_none())
            });
            if !revealed {
                return performed;
            }
            carried.stacked = true;
            let ops = carried.plan.stacking_ops();
            let seed = carried.plan.focus_seed.clone();
            let key = carried.plan.key;
            let minimized = carried.plan.minimize.len();
            let fullscreen = carried.plan.fullscreen.len();
            let lane = carried.lane;
            for op in &ops {
                self.perform_show_op(*op);
            }
            performed.extend(ops);
            // The stack just applied IS the OS stack: seed the MRU with it (and
            // the front with the key window), so a capture before the app next
            // gains focus reads it, and a close re-points to the window AppKit
            // raises. A live focus gain re-pushes in the same order.
            for window in &seed {
                self.focus_order.retain(|candidate| candidate != window);
                self.focus_order.push(*window);
            }
            if let Some(key) = key
                && self.frontmost_window != Some(key)
            {
                self.frontmost_window = Some(key);
                self.sync_active_session();
            }
            aterm_log::info!(
                "window show: the carried stack of {} window(s) is restored ({minimized} \
                 minimized, key window {}); {}",
                seed.len(),
                key.map_or_else(|| "none".to_string(), |key| key.0.to_string()),
                match (fullscreen, lane) {
                    (0, _) => "no window was in full screen".to_string(),
                    (n, ShowLane::Handoff) => format!(
                        "{n} window(s) go back to full screen after Commit, once aterm is in front"
                    ),
                    (n, ShowLane::Cold) => {
                        format!("{n} window(s) go back to full screen once aterm is in front")
                    }
                }
            );
        }
        performed.extend(self.enter_carried_fullscreen_if_due());
        performed
    }

    /// The update has committed: this process owns every window now. Make the
    /// carried key window key again — unless the user typed into a different
    /// carried window while the update was finishing (`typed_into`), in which
    /// case that is where they are working and the key stays theirs — and let
    /// full screen proceed. Returns the operations performed. (Commit is the
    /// unix overlap handoff's; nothing else reaches it.)
    #[cfg(any(unix, test))]
    pub(crate) fn commit_carried_window_show(
        &mut self,
        typed_into: &[WindowId],
    ) -> Vec<ShowOp<WindowId>> {
        let Some(carried) = self.carried_window_show.as_mut() else {
            return Vec::new();
        };
        let first_commit = !carried.committed;
        carried.committed = true;
        // A stack that is not on glass yet raises the key window LAST when it
        // goes on, so only a stack already applied needs the key re-asserted —
        // and only a stack of more than one window (`raise` is empty for one:
        // there is no other window the keyboard could be on).
        let rekey = (first_commit && carried.stacked && !carried.plan.raise.is_empty())
            .then_some(carried.plan.key)
            .flatten();
        let mut performed = Vec::new();
        if let Some(key) = rekey {
            if let Some(elsewhere) = typed_into.iter().find(|window| **window != key) {
                aterm_log::info!(
                    "window show: typing reached window {} before Commit, so it keeps the \
                     keyboard rather than the carried key window {}",
                    elsewhere.0,
                    key.0
                );
            } else {
                self.perform_show_op(ShowOp::Raise(key));
                performed.push(ShowOp::Raise(key));
            }
        }
        performed.extend(self.settle_carried_window_show());
        performed
    }

    /// Stage 2: re-enter the carried full screens once it is safe — the stack
    /// is on glass, the process owns every window, and aterm is the active app.
    /// Clears the carry once nothing is left to do.
    fn enter_carried_fullscreen_if_due(&mut self) -> Vec<ShowOp<WindowId>> {
        let Some(carried) = self.carried_window_show.as_ref() else {
            return Vec::new();
        };
        if !(carried.stacked && carried.committed) {
            return Vec::new();
        }
        if carried.plan.fullscreen.is_empty() {
            self.carried_window_show = None;
            return Vec::new();
        }
        // "aterm is the active app": one of the restored windows holds the
        // keyboard, by a focus record that is truthful (`CarriedShow::heard`).
        // Every window of a windowed restore has an OS window (one whose attach
        // failed was closed), so that record is what AppKit reported.
        let in_front = carried.plan.focus_seed.iter().any(|window| {
            carried.heard.contains(window) && self.windows.get(window).is_some_and(|ws| ws.focused)
        });
        if !in_front {
            return Vec::new();
        }
        let ops = carried.plan.fullscreen_ops();
        self.carried_window_show = None;
        for op in &ops {
            self.perform_show_op(*op);
        }
        aterm_log::info!(
            "window show: {} carried window(s) going back to full screen",
            ops.len()
        );
        ops
    }

    /// Whether `window` still has a carried full-screen re-entry to come.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn carried_fullscreen_pending(&self, window: WindowId) -> bool {
        self.carried_window_show
            .as_ref()
            .is_some_and(|carried| carried.fullscreen_pending(window))
    }

    /// Perform one operation on the window's OS window. A window with none — a
    /// headless instance, or one whose attach failed — is skipped: this is the
    /// only place the show state reaches AppKit, and it goes through the
    /// window's own winit handle, never a process-global query.
    fn perform_show_op(&self, op: ShowOp<WindowId>) {
        let (ShowOp::Minimize(window) | ShowOp::Raise(window) | ShowOp::EnterFullScreen(window)) =
            op;
        let Some(os_window) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.os_window.as_ref())
        else {
            return;
        };
        match op {
            ShowOp::Minimize(_) => os_window.set_minimized(true),
            // The plan never raises or full-screens a window it minimized, but
            // the user can minimize one between the stack and Commit, and
            // `makeKeyAndOrderFront:` would pull it back out of the Dock: what
            // they did last wins.
            ShowOp::Raise(_) | ShowOp::EnterFullScreen(_)
                if os_window.is_minimized() == Some(true) => {}
            ShowOp::Raise(_) => os_window.set_visible(true),
            ShowOp::EnterFullScreen(_) => {
                os_window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(z_order: u32, key: bool) -> WindowShow {
        WindowShow {
            fullscreen: Some(false),
            minimized: Some(false),
            z_order: Some(z_order),
            key: Some(key),
        }
    }

    // ---- capture ----------------------------------------------------------

    /// The MRU is the stack: the most recently focused window is rank 0, and
    /// a window closed since it was focused is not ranked at all.
    #[test]
    fn stacking_ranks_follow_the_focus_mru() {
        // Focused 1, then 3, then 2: front to back is 2, 3, 1. Window 9 is
        // gone and must not take a rank.
        let ranks = stacking_ranks(&[1_u64, 2, 3], &[9, 1, 3, 2]);
        assert_eq!(ranks, vec![2, 0, 1]);
    }

    /// A window that never gained focus sits in front of every window that did
    /// (its reveal ordered it front and nothing has reordered since), newest
    /// first — the successor that restored while the user was elsewhere.
    #[test]
    fn never_focused_windows_rank_in_front_newest_first() {
        let ranks = stacking_ranks(&[1_u64, 2, 3, 4], &[2, 1]);
        // Never focused: 4 (newest), 3. Then the MRU: 1, 2.
        assert_eq!(ranks, vec![2, 3, 1, 0]);
        // No focus history at all (a headless-shaped App): newest in front.
        assert_eq!(stacking_ranks(&[5_u64, 7], &[]), vec![1, 0]);
    }

    /// No OS window, nothing known: the capture records nothing, which is what
    /// keeps a headless or non-macOS layout byte-identical to an older build's.
    #[test]
    fn a_window_with_no_os_window_captures_nothing() {
        assert_eq!(captured_show(None, 0, true, true), WindowShow::UNKNOWN);
    }

    #[test]
    fn capture_maps_every_probe_bit() {
        let probe = ShowProbe {
            fullscreen: true,
            minimized: Some(false),
        };
        assert_eq!(
            captured_show(Some(probe), 2, false, false),
            WindowShow {
                fullscreen: Some(true),
                minimized: Some(false),
                z_order: Some(2),
                key: Some(false),
            }
        );
        let minimized = ShowProbe {
            fullscreen: false,
            minimized: Some(true),
        };
        assert_eq!(
            captured_show(Some(minimized), 0, true, false),
            WindowShow {
                fullscreen: Some(false),
                minimized: Some(true),
                z_order: Some(0),
                key: Some(true),
            }
        );
    }

    /// A carried full screen this process has not re-entered yet (aterm has
    /// not been in front since the update) is still captured AS full screen,
    /// so a second update does not forget it.
    #[test]
    fn a_pending_full_screen_is_captured_as_full_screen() {
        let probe = ShowProbe {
            fullscreen: false,
            minimized: Some(false),
        };
        assert_eq!(
            captured_show(Some(probe), 0, true, true).fullscreen,
            Some(true)
        );
        // …but never over a window the user has since minimized.
        let minimized = ShowProbe {
            fullscreen: false,
            minimized: Some(true),
        };
        assert_eq!(
            captured_show(Some(minimized), 0, true, true).fullscreen,
            Some(false)
        );
    }

    // ---- plan -------------------------------------------------------------

    /// FAIL SAFE: a manifest that carries no show state plans nothing, on
    /// either lane — the restore behaves exactly as before this module.
    #[test]
    fn a_manifest_without_show_state_plans_nothing() {
        let windows = [(1_u64, WindowShow::UNKNOWN), (2, WindowShow::UNKNOWN)];
        for lane in [ShowLane::Handoff, ShowLane::Cold] {
            let plan = plan(&windows, lane);
            assert!(plan.is_empty(), "{lane:?}: {plan:?}");
            assert!(plan.stacking_ops().is_empty());
            assert!(plan.fullscreen_ops().is_empty());
        }
    }

    /// THE HARM, reproduced and planned away: three windows created in
    /// manifest order 1, 2, 3 (so the reveals left 3 key and in front), while
    /// the carried stack was 2 in front and key, then 1, then 3. The plan
    /// raises back to front with the key window LAST.
    #[test]
    fn the_stack_is_raised_back_to_front_with_the_key_window_last() {
        let windows = [
            (1_u64, show(1, false)),
            (2, show(0, true)),
            (3, show(2, false)),
        ];
        let plan = plan(&windows, ShowLane::Handoff);
        assert_eq!(plan.key, Some(2));
        assert_eq!(plan.raise, vec![3, 1, 2]);
        assert_eq!(
            plan.stacking_ops(),
            vec![ShowOp::Raise(3), ShowOp::Raise(1), ShowOp::Raise(2)]
        );
        assert_eq!(plan.focus_seed, vec![3, 1, 2]);
        assert!(plan.fullscreen_ops().is_empty());
    }

    /// The key flag outranks the stack when the two disagree (a window the app
    /// keyed without a focus event): the key window is raised last either way.
    #[test]
    fn the_key_flag_outranks_the_stack() {
        let windows = [(1_u64, show(0, false)), (2, show(1, true))];
        let plan = plan(&windows, ShowLane::Handoff);
        assert_eq!(plan.key, Some(2));
        assert_eq!(plan.raise, vec![1, 2]);
    }

    /// One window: its reveal already made it key and front, so there is
    /// nothing to raise — a single-window update performs no extra AppKit call
    /// unless it has to minimize or re-enter full screen, and records no carry.
    #[test]
    fn a_single_window_raises_nothing() {
        let plan = plan(&[(1_u64, show(0, true))], ShowLane::Handoff);
        assert!(plan.raise.is_empty());
        assert!(plan.stacking_ops().is_empty());
        assert_eq!(plan.key, Some(1));
        assert!(plan.is_empty(), "nothing to do: no carry is recorded");
        let mut fullscreen = show(0, true);
        fullscreen.fullscreen = Some(true);
        let plan = super::plan(&[(1_u64, fullscreen)], ShowLane::Handoff);
        assert!(plan.stacking_ops().is_empty());
        assert_eq!(plan.fullscreen_ops(), vec![ShowOp::EnterFullScreen(1)]);
    }

    /// Minimized windows are minimized FIRST, never raised, never keyed; the
    /// key moves to the frontmost window still on screen.
    #[test]
    fn minimized_windows_are_minimized_first_and_never_keyed() {
        let mut minimized_key = show(0, true);
        minimized_key.minimized = Some(true);
        let windows = [
            (1_u64, minimized_key),
            (2, show(1, false)),
            (3, show(2, false)),
        ];
        let plan = plan(&windows, ShowLane::Handoff);
        assert_eq!(plan.key, Some(2));
        assert_eq!(
            plan.stacking_ops(),
            vec![ShowOp::Minimize(1), ShowOp::Raise(3), ShowOp::Raise(2)]
        );
        // The MRU seed keeps the minimized window behind the ones on screen.
        assert_eq!(plan.focus_seed, vec![1, 3, 2]);
    }

    /// The update lane reproduces the outgoing process exactly, even with
    /// every window minimized; a cold launch the user started shows the
    /// frontmost one instead.
    #[test]
    fn a_cold_launch_never_restores_every_window_minimized() {
        let mut back = show(1, false);
        back.minimized = Some(true);
        let mut front = show(0, true);
        front.minimized = Some(true);
        let windows = [(1_u64, back), (2, front)];
        let handoff = plan(&windows, ShowLane::Handoff);
        assert_eq!(handoff.key, None);
        assert_eq!(
            handoff.stacking_ops(),
            vec![ShowOp::Minimize(1), ShowOp::Minimize(2)]
        );
        let cold = plan(&windows, ShowLane::Cold);
        assert_eq!(cold.key, Some(2));
        assert_eq!(
            cold.stacking_ops(),
            vec![ShowOp::Minimize(1), ShowOp::Raise(2)]
        );
    }

    /// Full screen is re-entered back to front with the key window LAST, so
    /// the user ends in the Space they were in; a minimized window is never
    /// full-screened (AppKit cannot hold both).
    #[test]
    fn full_screen_is_re_entered_with_the_key_window_last() {
        let mut key = show(0, true);
        key.fullscreen = Some(true);
        let mut other = show(1, false);
        other.fullscreen = Some(true);
        let mut both = show(2, false);
        both.fullscreen = Some(true);
        both.minimized = Some(true);
        let windows = [(1_u64, key), (2, other), (3, both)];
        let plan = plan(&windows, ShowLane::Handoff);
        assert_eq!(
            plan.fullscreen_ops(),
            vec![ShowOp::EnterFullScreen(2), ShowOp::EnterFullScreen(1)]
        );
        assert_eq!(plan.minimize, vec![3]);
    }

    /// A hand-edited or partial manifest is data: windows without a rank go
    /// behind the ranked ones, ties keep manifest order, and a second `key`
    /// loses to the first.
    #[test]
    fn gaps_ties_and_duplicate_keys_are_tolerated() {
        let unranked = WindowShow {
            z_order: None,
            ..show(0, true)
        };
        let windows = [
            (1_u64, unranked),
            (2, show(7, true)),
            (3, show(7, false)),
            (4, show(3, false)),
        ];
        let plan = plan(&windows, ShowLane::Handoff);
        // Front to back: 4, 2, 3, 1. The first `key = true` in manifest order
        // is window 1.
        assert_eq!(plan.key, Some(1));
        assert_eq!(plan.raise, vec![3, 2, 4, 1]);
    }

    // ---- the App carry ----------------------------------------------------
    //
    // A headless App has no OS windows, so `perform_show_op` reaches nothing
    // here: what these pin is WHEN each stage runs and what the App's own
    // bookkeeping (the MRU, the front, the carry) becomes — the half a unit
    // test can see. The operations returned are the ones performed on a
    // windowed App. `crate::window_show_conformance` replays the same seams
    // against the `NativeUpdateWindowShow` model.

    /// Three logical windows: 0 is the bootstrap, 1 and 2 are the ones the
    /// rebuild created (2 last, so it is the front the rebuild left). Like a
    /// real rebuild's, the two new windows start out recorded as focused.
    pub(crate) fn three_windows() -> (crate::App, [WindowId; 3]) {
        let mut app = crate::App::headless_for_test();
        let first = WindowId(0);
        let second = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
        let third = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
        (app, [first, second, third])
    }

    fn carry(app: &mut crate::App, lane: ShowLane, windows: &[(WindowId, WindowShow)]) {
        let bootstrap = windows.first().map(|(window, _)| *window);
        app.carried_window_show = Some(CarriedShow::new(lane, plan(windows, lane), bootstrap));
    }

    /// The `WindowEvent::Focused` arm's two steps, in its order.
    fn focus_event(app: &mut crate::App, window: WindowId, focused: bool) -> Vec<ShowOp<WindowId>> {
        app.on_focus(window, focused);
        app.carried_window_show_focus_event(window)
    }

    /// Stage 1 on the update lane: nothing moves while a carried window is
    /// still waiting for its first present, then the stack goes on back to
    /// front with the key window last, and the MRU and the front become that
    /// stack — the rebuild had left window 2 in front, the carried key was 1.
    #[test]
    fn the_stack_waits_for_the_reveal_then_seeds_the_mru_and_the_front() {
        let (mut app, [w0, w1, w2]) = three_windows();
        assert_eq!(
            app.frontmost_window,
            Some(w2),
            "PRECONDITION: the rebuild's front"
        );
        carry(
            &mut app,
            ShowLane::Handoff,
            &[
                (w0, show(1, false)),
                (w1, show(0, true)),
                (w2, show(2, false)),
            ],
        );
        app.windows.get_mut(&w2).expect("w2").pending_reveal = Some(std::time::Instant::now());
        assert!(app.settle_carried_window_show().is_empty());
        assert!(!app.carried_window_show.as_ref().expect("carried").stacked);
        assert_eq!(
            app.frontmost_window,
            Some(w2),
            "nothing moved before the reveal"
        );

        app.windows.get_mut(&w2).expect("w2").pending_reveal = None;
        assert_eq!(
            app.settle_carried_window_show(),
            vec![ShowOp::Raise(w2), ShowOp::Raise(w0), ShowOp::Raise(w1)]
        );
        assert_eq!(app.focus_order, vec![w2, w0, w1], "the MRU is the stack");
        assert_eq!(
            app.frontmost_window,
            Some(w1),
            "the carried key window is in front"
        );
        let carried = app.carried_window_show.as_ref().expect("waits for Commit");
        assert!(carried.stacked && !carried.committed);
        assert!(
            app.settle_carried_window_show().is_empty(),
            "the stack goes on ONCE"
        );
    }

    /// The adoption proof this process still owes (the update lane before its
    /// proof): the write end of a pipe whose read end the caller holds.
    #[cfg(unix)]
    fn owe_the_proof(app: &mut crate::App) -> std::fs::File {
        use std::os::fd::FromRawFd as _;
        let mut fds = [0i32; 2];
        // SAFETY: pipe(2) into a valid two-slot out-array.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
        // SAFETY: two fresh descriptors, one owner each from here.
        let (read, write) = unsafe {
            (
                std::fs::File::from_raw_fd(fds[0]),
                std::os::fd::OwnedFd::from_raw_fd(fds[1]),
            )
        };
        app.handoff_ready = Some(crate::seamless::ReadySignal::for_test(
            write,
            "window-show",
            [0; 32],
            [0; 32],
        ));
        read
    }

    /// THE UPDATE LANE'S STACK IS THE PROOF PATH'S. While the adoption proof is
    /// owed, the event loop's settle and the focus arm leave the stack alone
    /// even with every carried window revealed: the proof also waits for every
    /// window's frame to be on record, and a window minimized before that
    /// presents nothing again, so the proof would never be written. The proof
    /// path, past its last gate, applies it in the call that writes the proof.
    /// (Before 2026-09-26 the `about_to_wait` settle minimized here.)
    #[cfg(unix)]
    #[test]
    fn the_update_lane_stack_waits_for_the_proof_path() {
        let (mut app, [w0, w1, w2]) = three_windows();
        let _proof = owe_the_proof(&mut app);
        let mut minimized = show(2, false);
        minimized.minimized = Some(true);
        carry(
            &mut app,
            ShowLane::Handoff,
            &[(w0, show(1, false)), (w1, show(0, true)), (w2, minimized)],
        );
        assert!(
            app.settle_carried_window_show().is_empty(),
            "no window is minimized while the proof may still wait for its present"
        );
        assert!(focus_event(&mut app, w1, true).is_empty());
        assert!(!app.carried_window_show.as_ref().expect("carried").stacked);
        assert_eq!(
            app.settle_carried_window_show_at_proof(),
            vec![ShowOp::Minimize(w2), ShowOp::Raise(w0), ShowOp::Raise(w1)]
        );
        assert!(app.carried_window_show.as_ref().expect("carried").stacked);
    }

    /// Commit re-asserts the key window and, with no window in full screen,
    /// ends the carry. A user who typed into another window before Commit
    /// keeps the keyboard where they typed.
    #[test]
    fn commit_rekeys_the_carried_key_window_unless_typing_went_elsewhere() {
        let windows = |[w0, w1, w2]: [WindowId; 3]| {
            [
                (w0, show(1, false)),
                (w1, show(0, true)),
                (w2, show(2, false)),
            ]
        };
        let (mut app, ids) = three_windows();
        carry(&mut app, ShowLane::Handoff, &windows(ids));
        let _ = app.settle_carried_window_show();
        assert_eq!(
            app.commit_carried_window_show(&[ids[1]]),
            vec![ShowOp::Raise(ids[1])],
            "typing into the key window itself is no reason to hold back"
        );
        assert!(app.carried_window_show.is_none(), "nothing left to do");

        let (mut app, ids) = three_windows();
        carry(&mut app, ShowLane::Handoff, &windows(ids));
        let _ = app.settle_carried_window_show();
        assert!(
            app.commit_carried_window_show(&[ids[0]]).is_empty(),
            "the user typed into window 0 before Commit: it keeps the keyboard"
        );
        assert!(app.carried_window_show.is_none());
    }

    /// Stage 2: full screen waits for Commit AND for aterm to be in front, and
    /// until it happens a capture still reports the window as full screen.
    #[test]
    fn full_screen_waits_for_commit_and_for_aterm_in_front() {
        let (mut app, [w0, w1, w2]) = three_windows();
        let mut key = show(0, true);
        key.fullscreen = Some(true);
        let mut other = show(1, false);
        other.fullscreen = Some(true);
        carry(
            &mut app,
            ShowLane::Handoff,
            &[(w0, key), (w1, other), (w2, show(2, false))],
        );
        assert_eq!(
            focus_event(&mut app, w0, true),
            vec![ShowOp::Raise(w2), ShowOp::Raise(w1), ShowOp::Raise(w0)],
            "aterm in front before Commit: the stack goes on, full screen does not"
        );
        assert!(
            focus_event(&mut app, w0, true).is_empty(),
            "in front, but not committed: the parked process still shares every PTY"
        );
        let _ = focus_event(&mut app, w0, false);
        assert_eq!(
            app.commit_carried_window_show(&[]),
            vec![ShowOp::Raise(w0)],
            "committed, but the user is in another app: no Space is entered"
        );
        assert!(app.carried_fullscreen_pending(w0) && app.carried_fullscreen_pending(w1));
        assert!(!app.carried_fullscreen_pending(w2));
        assert!(app.settle_carried_window_show().is_empty());

        assert_eq!(
            focus_event(&mut app, w0, true),
            vec![ShowOp::EnterFullScreen(w1), ShowOp::EnterFullScreen(w0)],
            "aterm is in front: back to full screen, the key window last"
        );
        assert!(app.carried_window_show.is_none());
        assert!(!app.carried_fullscreen_pending(w0));
    }

    /// "Is aterm in front" is read only off focus records that are truthful:
    /// the two windows the rebuild just created start out recorded as focused,
    /// which must not count until a `Focused` event reaches them — here the
    /// user is in another app, and full screen waits until aterm is in front.
    #[test]
    fn a_fresh_window_s_default_focus_does_not_count_as_aterm_in_front() {
        let (mut app, [w0, w1, w2]) = three_windows();
        app.on_focus(w0, false);
        assert!(
            app.windows[&w1].focused && app.windows[&w2].focused,
            "PRECONDITION: a new window starts out recorded as focused"
        );
        let mut key = show(0, true);
        key.fullscreen = Some(true);
        carry(
            &mut app,
            ShowLane::Cold,
            &[(w0, key), (w1, show(1, false)), (w2, show(2, false))],
        );
        assert_eq!(
            app.settle_carried_window_show(),
            vec![ShowOp::Raise(w2), ShowOp::Raise(w1), ShowOp::Raise(w0)],
            "the stack, and no full screen off two unreported focus records"
        );
        assert!(focus_event(&mut app, w1, false).is_empty());
        assert!(focus_event(&mut app, w2, false).is_empty());
        assert_eq!(
            focus_event(&mut app, w0, true),
            vec![ShowOp::EnterFullScreen(w0)],
            "aterm came to the front"
        );
        assert!(app.carried_window_show.is_none());
    }

    /// A cold launch the user started in front: the bootstrap window's focus
    /// was reported before the rebuild, so the stack and the full screen go on
    /// in the same turn.
    #[test]
    fn a_cold_restore_in_front_goes_straight_back_to_full_screen() {
        let (mut app, [w0, w1, w2]) = three_windows();
        app.on_focus(w0, true);
        let mut key = show(0, true);
        key.fullscreen = Some(true);
        carry(
            &mut app,
            ShowLane::Cold,
            &[(w0, key), (w1, show(1, false)), (w2, show(2, false))],
        );
        assert_eq!(
            app.settle_carried_window_show(),
            vec![
                ShowOp::Raise(w2),
                ShowOp::Raise(w1),
                ShowOp::Raise(w0),
                ShowOp::EnterFullScreen(w0)
            ]
        );
        assert!(app.carried_window_show.is_none());
    }

    /// A carried window the user closed before its stage runs is dropped from
    /// the plan — never raised, never keyed — and the rest still goes on.
    #[test]
    fn a_closed_window_is_dropped_from_the_carry() {
        let (mut app, [w0, w1, w2]) = three_windows();
        carry(
            &mut app,
            ShowLane::Cold,
            &[
                (w0, show(1, false)),
                (w1, show(0, true)),
                (w2, show(2, false)),
            ],
        );
        let _ = app.close_window_logical(w1);
        assert_eq!(
            app.settle_carried_window_show(),
            vec![ShowOp::Raise(w2), ShowOp::Raise(w0)]
        );
        assert!(!app.focus_order.contains(&w1));
    }

    /// FAIL SAFE at the App seam: a headless instance records no carry at
    /// all, and a headless capture records no show state, so its layout wire
    /// is the one an older build writes.
    #[test]
    fn a_headless_app_carries_and_captures_nothing() {
        let (mut app, [w0, w1, _]) = three_windows();
        app.record_carried_window_show(&[(w0, show(0, true)), (w1, show(1, false))], Some(w0));
        assert!(app.carried_window_show.is_none());
        let manifest = app.capture_restore_manifest();
        assert!(manifest.windows.iter().all(|w| w.show.is_unknown()));
        let wire = manifest.to_toml().expect("serialize");
        assert!(!wire.contains("[windows.show]"), "wire: {wire}");
    }
}
