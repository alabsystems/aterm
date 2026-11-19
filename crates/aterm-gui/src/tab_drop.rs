// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Release a dragged tab onto a strip, the session navigator, or the desktop.

use crate::tab_model::TabId;
use crate::{App, Wake, WindowId};

#[derive(Clone, Copy)]
pub(crate) enum TabDropSite {
    Strip(Option<TabId>),
    Content(f64, f64),
}

/// The second word of the local drag token carries the complete session or tab ID.
const STRIP_TAB: u64 = 0;
const SESSION_ROW: u64 = 1;

impl App {
    /// Claim the compositor's target for an outside release. Captured motion continues to
    /// drive strip reordering and local row drags until this exact mouse-up.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn claim_surface_drop(
        &mut self,
        source: WindowId,
        resolve: impl FnOnce([u64; 2]) -> bool,
    ) -> bool {
        let Some(ws) = self.windows.get(&source) else {
            return false;
        };
        let token = if let Some((session, origin)) = ws.dock.session_drag {
            let (x, y) = ws.last_cursor_px;
            if ws.pointer_position_known
                && (x - origin.0).hypot(y - origin.1)
                    < crate::client_frame::MOVE_THRESHOLD_LOGICAL_PX * ws.scale
            {
                return false;
            }
            [SESSION_ROW, session]
        } else if let Some(drag) = ws.strip_drag {
            [STRIP_TAB, drag.tab.get()]
        } else {
            return false;
        };
        if !resolve(token) {
            return false;
        }
        let button = aterm_types::mouse::MouseButton::Left;
        let owner = ws.native_button_owners[usize::from(button.slot())];
        if let Some(mut ws) = self.windows.get_mut(&source) {
            ws.strip_drag = None;
            ws.dock.session_drag = None;
        }
        self.finish_mouse_release(source, button, owner);
        true
    }

    pub(crate) fn finish_surface_drop(
        &mut self,
        source: WindowId,
        token: [u64; 2],
        target: Option<(WindowId, f64, f64)>,
    ) {
        if let [SESSION_ROW, session] = token {
            if let Some((target, x, y)) = target {
                self.finish_dock_session_drag_on(source, target, session, (x, y));
            }
        } else if let [STRIP_TAB, id] = token
            && let Some(tab) = self
                .windows
                .get(&source)
                .and_then(|ws| ws.tab_set.shown().iter().find(|t| t.id.get() == id))
                .map(|t| t.id)
        {
            if let Some((target, x, y)) = target {
                self.drop_tab_at(source, tab, target, self.grid_tab_drop_site(target, x, y));
            } else if self.tab_home_window() == Some(source) {
                self.drop_tab_outside(source, tab);
            }
        }
    }

    /// Insert the live tab immediately before the hit tab, or at the end of the strip.
    pub(crate) fn drop_tab_on_strip(
        &mut self,
        source: WindowId,
        tab: TabId,
        target: WindowId,
        before: Option<TabId>,
    ) -> bool {
        let Some(source_window) = self.windows.get(&source) else {
            return false;
        };
        let Some(from) = source_window
            .tab_set
            .shown()
            .iter()
            .position(|t| t.id == tab)
        else {
            return false;
        };
        if source != target && Some(target) != self.tab_home_window() {
            return false;
        }
        let Some(destination) = self.windows.get(&target) else {
            return false;
        };
        let slot = match before {
            Some(id) => {
                let Some(index) = destination.tab_set.shown().iter().position(|t| t.id == id)
                else {
                    return false;
                };
                index
            }
            None => destination.tab_set.shown_len(),
        };
        if source == target {
            self.move_tab(target, from, slot - usize::from(from < slot));
            return true;
        }
        if self
            .transfer_tab_between(source, tab, target, true)
            .is_err()
        {
            return false;
        }
        if let Some(from) = self.tab_index_for_id(target, tab) {
            self.move_tab(target, from, slot);
        }
        true
    }

    fn drop_tab_at(
        &mut self,
        source: WindowId,
        tab: TabId,
        target: WindowId,
        site: TabDropSite,
    ) -> bool {
        match site {
            TabDropSite::Strip(before) => self.drop_tab_on_strip(source, tab, target, before),
            TabDropSite::Content(x, y) => self.drop_tab_in_session_dock(source, tab, target, x, y),
        }
    }

    /// A queued detach carries the tab's identity and focus and uses the shortcut's attach rollback.
    fn drop_tab_outside(&mut self, source: WindowId, tab: TabId) -> bool {
        if self
            .windows
            .get(&source)
            .and_then(|ws| ws.tab_set.active())
            .map(|t| t.id)
            != Some(tab)
        {
            return false;
        }
        let Some(command) = self.tab_window_shortcut(source, "n") else {
            return false;
        };
        if self.headless {
            return self.apply_tab_window_command_logical(command).is_some();
        }
        self.proxy
            .as_ref()
            .is_some_and(|p| p.send_event(Wake::TabWindowCommand(command)).is_ok())
    }

    #[cfg(target_os = "macos")]
    fn native_tab_drop_site(
        &self,
        number: isize,
        screen: (i32, i32),
    ) -> Option<(WindowId, TabDropSite)> {
        self._toolbars.iter().find_map(|(&wid, toolbar)| {
            crate::toolbar::native_drop_site(toolbar, number, screen).map(|site| (wid, site))
        })
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn current_native_tab_drop_site(&self) -> Option<(WindowId, TabDropSite)> {
        if self.headless {
            return None;
        }
        let (number, screen) = crate::toolbar::current_native_drop_point();
        self.native_tab_drop_site(number, screen)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn finish_native_tab_drop(
        &mut self,
        source: WindowId,
        tab: TabId,
        number: isize,
        screen: (i32, i32),
    ) -> bool {
        match self.native_tab_drop_site(number, screen) {
            Some((wid, site)) => self.drop_tab_at(source, tab, wid, site),
            None => self.drop_tab_outside(source, tab),
        }
    }

    fn grid_tab_drop_site(&self, target: WindowId, x: f64, y: f64) -> TabDropSite {
        let Some(col) = self.strip_col_at(target, x, y) else {
            return TabDropSite::Content(x, y);
        };
        let before = self.windows.get(&target).and_then(|ws| {
            ws.tab_segments.iter().find_map(|segment| {
                let crate::tab_bar::TabHit::Select(index) = segment.kind else {
                    return None;
                };
                let mid = segment.start_col + (segment.end_col - segment.start_col) / 2;
                (col < mid)
                    .then(|| ws.painted_tab_index(index))
                    .flatten()
                    .and_then(|i| ws.tab_set.tab_at(i))
                    .map(|t| t.id)
            })
        });
        TabDropSite::Strip(before)
    }

    pub(crate) fn finish_grid_tab_drop(&mut self, receiver: WindowId) -> bool {
        let source = if self
            .windows
            .get(&receiver)
            .is_some_and(|ws| ws.strip_drag.is_some())
        {
            receiver
        } else if let Some(front) = self.frontmost_window.filter(|wid| *wid != receiver) {
            front
        } else {
            return false;
        };
        let Some(drag) = self
            .windows
            .get_mut(&source)
            .and_then(|mut ws| ws.strip_drag.take())
        else {
            return false;
        };
        let Some(ws) = self.windows.get(&receiver) else {
            return false;
        };
        let (x, y) = ws.last_cursor_px;
        let inside = |x: f64, y: f64, w: u32, h: u32| {
            x >= 0.0 && y >= 0.0 && x < f64::from(w) && y < f64::from(h)
        };
        if ws
            .win_px
            .is_none_or(|size| inside(x, y, size.width, size.height))
            && ws.pointer_position_known
        {
            if source == receiver && self.strip_col_at(receiver, x, y).is_some() {
                // Motion on the source strip already reordered every crossed tab.
                return true;
            }
            return self.drop_tab_at(
                source,
                drag.tab,
                receiver,
                self.grid_tab_drop_site(receiver, x, y),
            );
        }
        if let Some(origin) = ws.os_window.as_ref().and_then(|w| w.inner_position().ok()) {
            let screen = (x + f64::from(origin.x), y + f64::from(origin.y));
            let hit = self
                .focus_order
                .iter()
                .rev()
                .chain(self.windows.keys())
                .find_map(|&wid| {
                    if wid == receiver {
                        return None;
                    }
                    let w = self.windows.get(&wid)?.os_window.as_ref()?;
                    if w.is_minimized() == Some(true) {
                        return None;
                    }
                    #[cfg(target_os = "macos")]
                    if w.is_visible() == Some(false) {
                        return None;
                    }
                    let origin = w.inner_position().ok()?;
                    let (x, y) = (
                        screen.0 - f64::from(origin.x),
                        screen.1 - f64::from(origin.y),
                    );
                    let size = w.inner_size();
                    inside(x, y, size.width, size.height).then_some((wid, x, y))
                });
            if let Some((wid, x, y)) = hit {
                return self.drop_tab_at(source, drag.tab, wid, self.grid_tab_drop_site(wid, x, y));
            }
        }
        self.drop_tab_outside(source, drag.tab)
    }
}

#[cfg(test)]
#[path = "tab_drop_tests.rs"]
mod tests;
