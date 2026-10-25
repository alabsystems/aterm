// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The chrome layered over a prepared frame, in one order.

use crate::tab_model::VisibleLeafPlan;
use crate::{App, VisibleContentRoute, WindowId};

/// One chrome layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChromeStep {
    PaneOverlay,
    FindBar,
    RewindBand,
    SettingsPanel,
    BuildBadge,
    RouteCard,
    StatsDeck,
    ClaudeFooter,
    AutopilotMenu,
    PasteBanner,
    LinkTarget,
    TabMenu,
    Wallpaper,
}

impl ChromeStep {
    /// The layers `route` composes, in order.
    pub(crate) fn order(route: VisibleContentRoute) -> impl Iterator<Item = ChromeStep> {
        use ChromeStep::*;
        let (before, after): (&'static [ChromeStep], &'static [ChromeStep]) = match route {
            VisibleContentRoute::Terminal { .. } => (
                &[
                    PaneOverlay,
                    FindBar,
                    RewindBand,
                    SettingsPanel,
                    BuildBadge,
                    StatsDeck,
                ],
                &[Wallpaper],
            ),
            VisibleContentRoute::Native { .. } | VisibleContentRoute::Heterogeneous => (
                &[FindBar, RewindBand, BuildBadge, RouteCard, StatsDeck],
                &[],
            ),
        };
        before
            .iter()
            .chain(&[
                ClaudeFooter,
                AutopilotMenu,
                PasteBanner,
                LinkTarget,
                TabMenu,
            ])
            .chain(after)
            .copied()
    }
}

/// What one composition produced.
#[derive(Debug, Default)]
pub(crate) struct Composed {
    /// The link target caption the frame shows, for the present that accepts it to publish for a
    /// press (`commit_presented_caption`).
    pub(crate) caption: Option<crate::app_mouse::ShownCaption>,
}

impl App {
    /// Layer the chrome of `route` over the frame prepared in `wid`'s `input_scratch`, in
    /// [`ChromeStep::order`].
    pub(crate) fn compose_route_chrome(
        &mut self,
        wid: WindowId,
        route: VisibleContentRoute,
        plan: &VisibleLeafPlan,
    ) -> Option<Composed> {
        #[cfg(test)]
        trace::composition();
        let mut composed = Composed::default();
        for step in ChromeStep::order(route) {
            #[cfg(test)]
            if !trace::admit(step) {
                continue;
            }
            match step {
                ChromeStep::PaneOverlay => self.splice_pane_overlay(wid),
                ChromeStep::FindBar => self.splice_find_bar(wid),
                ChromeStep::RewindBand => self.splice_rewind_band(wid),
                ChromeStep::SettingsPanel => self.splice_settings_panel(wid),
                ChromeStep::BuildBadge => self.splice_build_badge(wid),
                ChromeStep::RouteCard => {
                    if !self.compose_native_route_card(wid) {
                        return None;
                    }
                }
                ChromeStep::StatsDeck => {
                    self.splice_stats_deck(wid);
                    self.fold_stats_deck(wid);
                }
                ChromeStep::ClaudeFooter => self.splice_claude_footer(wid, plan, route),
                ChromeStep::AutopilotMenu => self.splice_autopilot_menu(wid),
                ChromeStep::PasteBanner => self.splice_paste_banner(wid),
                ChromeStep::LinkTarget => {
                    composed.caption = self.compose_link_caption(wid, Some(plan));
                }
                ChromeStep::TabMenu => self.splice_tab_menu(wid),
                ChromeStep::Wallpaper => self.splice_wallpaper(wid),
            }
        }
        if let Some(window) = self.windows.get_mut(&wid) {
            crate::icon_scene::apply(&mut window.input_scratch);
        }
        Some(composed)
    }
}

/// What the compositions on this thread ran: a count of [`App::compose_route_chrome`] calls and the
/// steps they ran, plus a mask of steps to skip.
#[cfg(test)]
pub(crate) mod trace {
    use super::ChromeStep;
    use std::cell::{Cell, RefCell};

    thread_local! {
        static COMPOSITIONS: Cell<usize> = const { Cell::new(0) };
        static STEPS: RefCell<Vec<ChromeStep>> = const { RefCell::new(Vec::new()) };
        static MASK: RefCell<Vec<ChromeStep>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn composition() {
        COMPOSITIONS.with(|c| c.set(c.get() + 1));
    }

    /// Whether `step` runs; a step that runs is recorded.
    pub(super) fn admit(step: ChromeStep) -> bool {
        if MASK.with(|mask| mask.borrow().contains(&step)) {
            return false;
        }
        STEPS.with(|steps| steps.borrow_mut().push(step));
        true
    }

    /// The compositions and steps recorded since the last take, which resets
    /// both.
    pub(crate) fn take() -> (usize, Vec<ChromeStep>) {
        (
            COMPOSITIONS.with(|c| c.replace(0)),
            STEPS.with(|steps| std::mem::take(&mut *steps.borrow_mut())),
        )
    }

    /// Skip `steps` in every composition on this thread until the guard
    /// drops.
    #[must_use]
    pub(crate) fn mask(steps: &[ChromeStep]) -> MaskGuard {
        MASK.with(|mask| *mask.borrow_mut() = steps.to_vec());
        MaskGuard
    }

    pub(crate) struct MaskGuard;

    impl Drop for MaskGuard {
        fn drop(&mut self) {
            MASK.with(|mask| mask.borrow_mut().clear());
        }
    }
}

#[cfg(test)]
#[path = "capture_route_chrome_tests.rs"]
mod tests;
