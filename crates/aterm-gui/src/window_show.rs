// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Window show state across an update: full screen, minimized, the window stack, and the key window.

use crate::WindowId;
use crate::restore::WindowShow;

/// Front-to-back stacking ranks for `live` windows (aligned index for index; 0 is the frontmost), from the focus MRU `focus_order` (most recent last).
#[cfg(any(target_os = "macos", test))]
pub(crate) fn stacking_ranks<W: Copy + Ord>(live: &[W], focus_order: &[W]) -> Vec<u32> {
    let mut front_to_back: Vec<W> = live
        .iter()
        .copied()
        .filter(|window| !focus_order.contains(window))
        .collect();
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

/// One window's captured [`WindowShow`].
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
    /// A cold launch restoring a quit's manifest: the same, except that it never leaves every window minimized.
    Cold,
}

/// What a restore does with the carried show states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShowPlan<W> {
    /// Windows to miniaturize, back to front.
    pub minimize: Vec<W>,
    /// Windows to raise, back to front, the key window last.
    pub raise: Vec<W>,
    /// The window typing should go to: the carried key window when it is not minimized, else the frontmost window that is not.
    pub key: Option<W>,
    /// Windows to re-enter full screen, back to front, the key window last.
    pub fullscreen: Vec<W>,
    /// The focus MRU this stack implies, most recent last.
    pub focus_seed: Vec<W>,
}

impl<W> ShowPlan<W> {
    /// Whether the plan has nothing to do.
    pub(crate) fn is_empty(&self) -> bool {
        self.minimize.is_empty() && self.raise.is_empty() && self.fullscreen.is_empty()
    }
}

/// One window operation a restore performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShowOp<W> {
    /// Miniaturize into the Dock (winit `set_minimized(true)`).
    Minimize(W),
    /// Order front within the app and make it the app's key window, without activating the app (winit `set_visible(true)`, `makeKeyAndOrderFront:`).
    Raise(W),
    /// Enter native full screen on its current display (winit `set_fullscreen(Borderless(None))`).
    EnterFullScreen(W),
}

impl<W: Copy + Eq> ShowPlan<W> {
    /// Key the most recent window in `typed` that this plan raises or minimizes instead of the carried key window.
    pub(crate) fn key_typed_window(&mut self, typed: &[W]) -> Option<W> {
        let window = *typed
            .iter()
            .rev()
            .find(|window| self.raise.contains(window) || self.minimize.contains(window))?;
        if let Some(at) = self
            .minimize
            .iter()
            .position(|candidate| *candidate == window)
        {
            self.minimize.remove(at);
            self.raise.push(window);
        }
        if self.key == Some(window) {
            return None;
        }
        self.key = Some(window);
        for list in [&mut self.raise, &mut self.fullscreen, &mut self.focus_seed] {
            if let Some(at) = list.iter().position(|candidate| *candidate == window) {
                let window = list.remove(at);
                list.push(window);
            }
        }
        Some(window)
    }
}

impl<W: Copy> ShowPlan<W> {
    /// Stage 1, the stack: minimize first, so the key window the raises settle on is the last window AppKit was told to key, then raise back to front.
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

/// Plan a restore of `windows` — `(live window, carried show state)` in manifest order.
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
    let mut front_to_back: Vec<(W, WindowShow)> = windows.to_vec();
    front_to_back.sort_by_key(|(_, show)| show.z_order.unwrap_or(u32::MAX));
    let mut minimized: Vec<W> = front_to_back
        .iter()
        .filter(|(_, show)| show.minimized == Some(true))
        .map(|(window, _)| *window)
        .collect();
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

/// The show state a restore carried, from the moment its windows exist until the last of it is applied (`App::carried_window_show`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CarriedShow {
    pub lane: ShowLane,
    pub plan: ShowPlan<WindowId>,
    /// Stage 1 is done: the stack is on glass.
    pub stacked: bool,
    /// This process owns every window: always on a cold launch, from Commit on the update lane.
    pub committed: bool,
    /// The carried windows whose focus the App's record reports truthfully — what "is aterm in front" is read from.
    pub heard: Vec<WindowId>,
}

impl CarriedShow {
    /// A fresh carry for `plan`, nothing applied yet.
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
    #[cfg(target_os = "macos")]
    pub(crate) fn fullscreen_pending(&self, window: WindowId) -> bool {
        self.plan.fullscreen.contains(&window)
    }
}

impl crate::App {
    /// Record, at the end of the restore rebuild, the live window each manifest window became and the show state it carried.
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

    /// A `Focused` event reached `window` (the `WindowEvent::Focused` arm, after `on_focus` recorded it).
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

    /// Advance the carried show state as far as it can go now, and return the operations performed (the seam the tests read).
    pub(crate) fn settle_carried_window_show(&mut self) -> Vec<ShowOp<WindowId>> {
        let may_stack = self.handoff_ready.is_none();
        self.settle_carried_window_show_as(may_stack)
    }

    /// The proof path's settle (`maybe_signal_handoff_ready`, past its last gate: every OS window painted and revealed, the restore drained, the control service ready).
    pub(crate) fn settle_carried_window_show_at_proof(&mut self) -> Vec<ShowOp<WindowId>> {
        self.settle_carried_window_show_as(true)
    }

    /// [`Self::settle_carried_window_show`]'s body; `may_stack` is whether this
    /// caller may apply the stack.
    fn settle_carried_window_show_as(&mut self, may_stack: bool) -> Vec<ShowOp<WindowId>> {
        if self.carried_window_show.is_none() {
            return Vec::new();
        }
        let typed = self.pre_commit_typed_windows();
        let Some(carried) = self.carried_window_show.as_mut() else {
            return Vec::new();
        };
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
            // due either (full screen waits for the stack and for Commit).
            if !may_stack {
                return performed;
            }
            let revealed = carried.plan.focus_seed.iter().all(|window| {
                windows
                    .get(window)
                    .is_some_and(|ws| ws.pending_reveal.is_none())
            });
            if !revealed {
                return performed;
            }
            carried.stacked = true;
            if !carried.committed
                && let Some(typed_into) = carried.plan.key_typed_window(&typed)
            {
                aterm_log::info!(
                    "window show: typing reached window {} before the stack, so the stack keys \
                     it rather than the carried key window",
                    typed_into.0
                );
            }
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

    /// The carried windows keys or text were typed into while the update finished.
    pub(crate) fn pre_commit_typed_windows(&self) -> Vec<WindowId> {
        self.handoff_deferred_input
            .iter()
            .filter(|(_, event)| {
                matches!(
                    event,
                    aterm_window::event::WindowEvent::KeyboardInput { .. }
                        | aterm_window::event::WindowEvent::Ime(_)
                )
            })
            .filter_map(|(winit_id, _)| self.winit_to_window.get(winit_id).copied())
            .collect()
    }

    /// The update has committed: this process owns every window now.
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
    #[cfg(target_os = "macos")]
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
            ShowOp::Raise(_) | ShowOp::EnterFullScreen(_)
                if os_window.is_minimized() == Some(true) => {}
            ShowOp::Raise(_) => os_window.set_visible(true),
            ShowOp::EnterFullScreen(_) => {
                os_window.set_fullscreen(Some(aterm_window::window::Fullscreen::Borderless(None)));
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

    /// The MRU is the stack: the most recently focused window is rank 0, and
    /// a window closed since it was focused is not ranked at all.
    #[test]
    fn stacking_ranks_follow_the_focus_mru() {
        let ranks = stacking_ranks(&[1_u64, 2, 3], &[9, 1, 3, 2]);
        assert_eq!(ranks, vec![2, 0, 1]);
    }

    /// A window that never gained focus sits in front of every window that did
    /// (its reveal ordered it front and nothing has reordered since), newest
    /// first — the successor that restored while the user was elsewhere.
    #[test]
    fn never_focused_windows_rank_in_front_newest_first() {
        let ranks = stacking_ranks(&[1_u64, 2, 3, 4], &[2, 1]);
        assert_eq!(ranks, vec![2, 3, 1, 0]);
        assert_eq!(stacking_ranks(&[5_u64, 7], &[]), vec![1, 0]);
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
        let minimized = ShowProbe {
            fullscreen: false,
            minimized: Some(true),
        };
        assert_eq!(
            captured_show(Some(minimized), 0, true, true).fullscreen,
            Some(false)
        );
    }

    /// A manifest that carries no show state plans nothing, on either lane.
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

    /// Three windows created in manifest order 1, 2, 3 (so the reveals left 3
    /// key and in front), while the carried stack was 2 in front and key, then
    /// 1, then 3: the plan raises back to front with the key window last.
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
        assert_eq!(plan.key, Some(1));
        assert_eq!(plan.raise, vec![3, 2, 4, 1]);
    }

    /// Three logical windows: 0 is the bootstrap, 1 and 2 are the ones the rebuild created (2 last, so it is the front the rebuild left).
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

    /// Window 2 was carried minimized, but every carried window is on glass
    /// from its reveal until the stack, and the reveal left window 2 key.
    #[test]
    fn keys_typed_into_a_window_carried_minimized_keep_it_out_of_the_dock() {
        let windows = |[w0, w1, w2]: [WindowId; 3]| {
            let mut minimized = show(1, false);
            minimized.minimized = Some(true);
            [(w0, show(2, false)), (w1, show(0, true)), (w2, minimized)]
        };
        for typed in [false, true] {
            let (mut app, ids) = three_windows();
            carry(&mut app, ShowLane::Handoff, &windows(ids));
            app.frontmost_window = Some(ids[2]);
            app.incoming_handoff_pending = true;
            if typed {
                let winit_id = aterm_window::window::WindowId::from(2u64);
                app.winit_to_window.insert(winit_id, ids[2]);
                app.queue_pre_commit_input(
                    winit_id,
                    aterm_window::event::WindowEvent::Ime(aterm_window::event::Ime::Commit(
                        "git pu".into(),
                    )),
                );
            }
            let stack = app.settle_carried_window_show_at_proof();
            if typed {
                assert!(
                    !stack.contains(&ShowOp::Minimize(ids[2])),
                    "the window typed into is not sent to the Dock: {stack:?}"
                );
                assert_eq!(
                    stack.last(),
                    Some(&ShowOp::Raise(ids[2])),
                    "it is raised last, so it keeps the keyboard: {stack:?}"
                );
                assert_eq!(
                    app.focus_order.last(),
                    Some(&ids[2]),
                    "the focus seed puts it most recent"
                );
            } else {
                assert_eq!(
                    stack.first(),
                    Some(&ShowOp::Minimize(ids[2])),
                    "nothing typed: it goes back to the Dock: {stack:?}"
                );
                assert_eq!(stack.last(), Some(&ShowOp::Raise(ids[1])), "{stack:?}");
            }
            let key = if typed { ids[2] } else { ids[1] };
            let typed_into = app.pre_commit_typed_windows();
            let commit = app.commit_carried_window_show(&typed_into);
            assert_eq!(
                commit,
                vec![ShowOp::Raise(key)],
                "typed={typed}: Commit keys it again"
            );
            assert_eq!(
                app.frontmost_window,
                Some(key),
                "typed={typed}: the keyboard is where the person was typing"
            );
        }
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

    /// A headless instance records no carry, and a headless capture records no
    /// show state, so its layout wire omits `[windows.show]`.
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
