// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Which windows carry the System Resources band.
//!
//! The band is the stats deck and the Network State band under it, one head reservation
//! ([`crate::stats_deck::reserved_deck_px`]). A window carries it or it does not
//! (`WindowState::carries_band`): the first window a process creates is born with it, every
//! later window without, and a person moves it per window (View ▸ Show System Resources Here,
//! [`App::toggle_window_band`]). There is no main window: closing the window that carries the
//! band, or dragging its last tab out, moves the band nowhere.
//!
//! A window's own form is a pure function of the setting (`[sys] stats_bars`) and that bit:
//! [`StatsBars::in_window`], read through [`App::window_bars`] by every site that sizes or
//! draws one window. [`App::stats_bars_form`] stays the setting, for the sites that ask whether
//! any bars show.

use crate::App;
use crate::WindowId;
use crate::stats_deck::StatsBars;

/// What a refused gesture is told ([`App::toggle_window_band`]).
pub(crate) const BAND_PERSON_ONLY: &str =
    "Show System Resources Here is the person's own act at the window";

impl StatsBars {
    /// The form a window takes under this setting, where `carries` is whether the window
    /// carries the System Resources band: `off` is Off in every window, `sill` the Sill in
    /// every window (the bit is kept and inert), and `deck` the Deck where the window carries
    /// the band and the Sill where it does not.
    ///
    /// What a window without the band keeps is this one arm.
    pub(crate) const fn in_window(self, carries: bool) -> Self {
        match (self, carries) {
            (Self::Deck, false) => Self::Sill,
            (form, _) => form,
        }
    }
}

impl App {
    /// Window `wid`'s own stats bars: the setting in that window ([`StatsBars::in_window`]). A
    /// window that is not there carries nothing.
    pub(crate) fn window_bars(&self, wid: WindowId) -> StatsBars {
        let carries = self.windows.get(&wid).is_some_and(|ws| ws.carries_band);
        self.stats_bars_form().in_window(carries)
    }

    /// Whether window `wid` carries the System Resources band (the row's check).
    pub(crate) fn window_carries_band(&self, wid: WindowId) -> bool {
        self.windows.get(&wid).is_some_and(|ws| ws.carries_band)
    }

    /// View ▸ Show System Resources Here: window `wid` takes the System Resources band, or
    /// gives it up for the Sill. A person's act: a gesture no person at the window stands
    /// behind ([`Self::gesture_unvouched`]: a control client's key into the palette, an
    /// accessibility client's, relayed input) is refused with [`BAND_PERSON_ONLY`]. And only
    /// while the setting is `deck` (under `sill` and `off` no window draws the band, so the act
    /// does nothing). Answers whether the bit moved.
    ///
    /// It flips the window's bit, drops its lens (the Deck's and the Sill's lenses are placed
    /// apart) and re-grids that window alone inside the frame it holds
    /// ([`Self::held_inner_px`]), booked `resizes site=deck`: the frame is kept, each tab of the
    /// window re-grids once under the head its new form takes, and no other window's tabs see
    /// anything. One window does not keep its frame: one that holds Settings and is now under
    /// the least size its new head asks grows to it ([`Self::hold_least_window_size`]), which
    /// re-grids its tabs again. An attached window's head is re-derived by the resize itself
    /// (`on_resize_inner` on macOS, `sync_synthetic_strip_head` on Windows and Linux, both
    /// from [`Self::deck_head_px`]), which sets the renderer's head to this window's as any
    /// resize of an attached window does; this function writes no head of its own, and every
    /// other window is answered from its own metrics ([`Self::win_head`]). A headless window's
    /// head is the boot head whatever the bit, so there the act changes what the window draws
    /// and books nothing.
    pub(crate) fn toggle_window_band(&mut self, wid: WindowId) -> bool {
        if let Some(who) = self.gesture_unvouched() {
            aterm_log::info!(
                "stats: refused a gesture by {who:?} to move the System Resources band: only the person at the window moves it"
            );
            self.pending_action_refusal = Some(BAND_PERSON_ONLY.to_owned());
            return false;
        }
        if self.stats_bars_form() != StatsBars::Deck {
            return false;
        }
        // The size the window keeps, read under the head it has now.
        let Some(held) = self.held_inner_px(wid) else {
            return false;
        };
        {
            let Some(mut ws) = self.windows.get_mut(&wid) else {
                return false;
            };
            ws.carries_band = !ws.carries_band;
            // A card that goes with the form leaves the native page under it to be hovered
            // again, as a closed lens does.
            if ws.stats_lens.open.is_some() || ws.stats_lens.rect.is_some() {
                ws.native_rehover = true;
            }
            ws.stats_lens = crate::stats_lens::LensSlot::default();
        }
        {
            let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Deck);
            self.on_resize(wid, held);
            self.hold_least_window_size(wid);
        }
        // A palette already open resolved the row's check before the bit moved.
        self.palette_refresh_live();
        self.request_window_redraw(wid);
        true
    }

    /// A lib fixture's stand-in for the platform arms of `on_resize_inner`: a window that asked
    /// for it (`WindowState::head_follows_form_for_test`) takes the head its own form gives at
    /// a `frame_h_px` frame, as an attached window does at each resize.
    #[cfg(test)]
    pub(crate) fn sync_fixture_head(&mut self, wid: WindowId, frame_h_px: u32) {
        let Some((head_pts, scale)) = self
            .windows
            .get(&wid)
            .filter(|ws| ws.head_follows_form_for_test)
            .map(|ws| (ws.head_pts, ws.scale))
        else {
            return;
        };
        let head = self.deck_head_px(wid, head_pts, scale, frame_h_px);
        if let Some(mut ws) = self.windows.get_mut(&wid) {
            ws.metrics.head = head;
        }
    }
}

#[cfg(test)]
#[path = "stats_band_tests.rs"]
pub(crate) mod tests;
