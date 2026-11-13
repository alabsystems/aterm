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
    /// Arrows answered by the lens pixels this frame includes, for its successful present.
    pub(crate) lens_input: Option<crate::stats_lens::LensPaintReceipt>,
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
        // A route card folds a native page under the badge ([`App::compose_native_route_card`]):
        // a terminal route shows none, so its composition drops the one a native frame left, on
        // every path that composes (a window's redraw, a headless capture, a glassless lay).
        if matches!(route, VisibleContentRoute::Terminal { .. })
            && let Some(window) = self.windows.get_mut(&wid)
        {
            window.route_card = None;
        }
        let mut composed = Composed::default();
        for step in ChromeStep::order(route) {
            #[cfg(test)]
            if !trace::admit(step) {
                continue;
            }
            match step {
                ChromeStep::PaneOverlay => self.splice_pane_overlay(wid),
                ChromeStep::FindBar => self.splice_find_bar_from_plan(wid, Some(plan)),
                ChromeStep::RewindBand => self.splice_rewind_band(wid, plan),
                ChromeStep::SettingsPanel => self.splice_settings_panel(wid),
                ChromeStep::BuildBadge => self.splice_build_badge(wid),
                ChromeStep::RouteCard => {
                    if !self.compose_native_route_card(wid) {
                        return None;
                    }
                }
                ChromeStep::StatsDeck => {
                    self.splice_stats_deck(wid);
                    self.stack_or_fold_stats_deck(wid);
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
        if crate::icon_scene::active()
            && let Some(cell) = self.spawn_cell_px(wid)
            && let Some(window) = self.windows.get_mut(&wid)
        {
            crate::icon_scene::apply(&mut window.input_scratch, cell);
        }
        composed.lens_input = self.stats_lens_input_receipt(wid);
        Some(composed)
    }
}

/// What the compositions on this thread ran: a count of [`App::compose_route_chrome`] calls and the
/// steps they ran, plus a mask of steps to skip.
#[cfg(test)]
#[path = "route_chrome_trace_tests.rs"]
pub(crate) mod trace;

#[cfg(test)]
#[path = "capture_route_chrome_tests.rs"]
mod tests;
