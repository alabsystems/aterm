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
    pub(crate) const fn is_empty(&self) -> bool {
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
            // The proof is owed and this is not its path: nothing else can be
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
            .filter(|(_, event, _)| {
                matches!(
                    event,
                    aterm_window::event::WindowEvent::KeyboardInput { .. }
                        | aterm_window::event::WindowEvent::Ime(_)
                )
            })
            .filter_map(|(winit_id, ..)| self.winit_to_window.get(winit_id).copied())
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
        let now = std::time::Instant::now();
        for op in &ops {
            self.perform_show_op(*op);
            // A window owing its restore re-grid waits for the full screen's size from here
            // (`App::pay_restore_regrid_if_due`).
            if let ShowOp::EnterFullScreen(window) = *op
                && let Some(owed) = self
                    .windows
                    .get_mut(&window)
                    .and_then(|ws| ws.restore_regrid_owed.as_mut())
            {
                owed.since = now;
            }
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
#[path = "window_show_tests.rs"]
mod tests;
