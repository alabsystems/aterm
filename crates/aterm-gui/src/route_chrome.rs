// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The chrome layered over a prepared frame, in one order.
//!
//! Every application present and every capture that rasterizes its own frame
//! (a headless `image`, a headless snapshot) prepares the route's content into
//! `input_scratch` and then calls [`App::compose_route_chrome`]. The layers and
//! their order are [`ChromeStep::order`], so a capture cannot show a layer the
//! glass lacks or omit one it shows. Every layer is a read of App state or a
//! cache of one, so a capture's composition consumes nothing a later present
//! shows. Route preparation (the terminal grid, the tab strip, pane headers,
//! the native surface) and the effects of presenting (damage, the title,
//! output stamps, the link caption a press acts on) stay with the callers.

use crate::tab_model::VisibleLeafPlan;
use crate::{App, VisibleContentRoute, WindowId};

/// One chrome layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChromeStep {
    PaneOverlay,
    FindBar,
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
                &[PaneOverlay, FindBar, SettingsPanel, BuildBadge, StatsDeck],
                &[Wallpaper],
            ),
            VisibleContentRoute::Native { .. } | VisibleContentRoute::Heterogeneous => {
                (&[FindBar, BuildBadge, RouteCard, StatsDeck], &[])
            }
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
    /// The link target caption the frame shows, for the present that accepts
    /// it to publish for a press (`commit_presented_caption`). A capture
    /// discards it.
    pub(crate) caption: Option<crate::app_mouse::ShownCaption>,
}

impl App {
    /// Layer the chrome of `route` over the frame prepared in `wid`'s
    /// `input_scratch`, in [`ChromeStep::order`]. `plan` is the visible-leaf
    /// plan the preparation used. `None` when a native or heterogeneous frame
    /// has no route card to present.
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
                // The overlay scrollbar and unfocused panes' hollow cursors,
                // under every band and card spliced after it
                // (`crate::pane_chrome`).
                ChromeStep::PaneOverlay => self.splice_pane_overlay(wid),
                // The Cmd-F find bar over the bottom terminal row; a no-op when
                // not searching.
                ChromeStep::FindBar => self.splice_find_bar(wid),
                // The modal Settings panel over the top rows; a no-op when
                // closed. Terminal routes only: a native preparation owns
                // `settings_card` (the compiled surface, with any modal in it).
                ChromeStep::SettingsPanel => self.splice_settings_panel(wid),
                // The top-right build/version badge: paint-only, its own slot,
                // shown only when no overlay is open.
                ChromeStep::BuildBadge => self.splice_build_badge(wid),
                // The native surface and the highest-priority paint-only card
                // above, folded into the one tray both backends expose.
                ChromeStep::RouteCard => {
                    if !self.compose_native_route_card(wid) {
                        return None;
                    }
                }
                // The stats Deck in the chrome headroom (`crate::stats_deck`),
                // folded under whichever card the window presents.
                ChromeStep::StatsDeck => {
                    self.splice_stats_deck(wid);
                    self.fold_stats_deck(wid);
                }
                // The Claude Code footer in each Claude pane's composer bottom
                // rule (`crate::claude_footer`), within the pane's columns; the
                // paste question, link caption and tab menu outrank it. A
                // frame with no terminal leaf writes nothing.
                ChromeStep::ClaudeFooter => self.splice_claude_footer(wid, plan, route),
                // The Autopilot menu hangs from its chip, whose hit the footer
                // just recorded (`crate::autopilot_menu`); a no-op with no menu
                // up.
                ChromeStep::AutopilotMenu => self.splice_autopilot_menu(wid),
                // The multi-line-paste confirmation is a security question: it
                // paints over the top rows, and the present seam crops the
                // native tray below it.
                ChromeStep::PasteBanner => self.splice_paste_banner(wid),
                // The hovered link's destination, last of the row bands: it
                // takes a row only where no other band claimed one. A no-op
                // with no link hovered. A capture without it would tell a
                // driving AI a link is undisclosed while a human reads it.
                ChromeStep::LinkTarget => {
                    composed.caption = self.compose_link_caption(wid, Some(plan));
                }
                // The tab context menu owns pointer and keyboard while open, so
                // it is the topmost chrome. A no-op with no menu up.
                ChromeStep::TabMenu => self.splice_tab_menu(wid),
                // The wallpaper base layer, scaled to the final frame (strip
                // rows included); its text tint yields to ink already placed.
                ChromeStep::Wallpaper => self.splice_wallpaper(wid),
            }
        }
        Some(composed)
    }
}

/// What the compositions on this thread ran: a count of
/// [`App::compose_route_chrome`] calls and the steps they ran, plus a mask of
/// steps to skip. Thread-local, so parallel tests cannot bleed into each
/// other.
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
