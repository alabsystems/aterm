// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! A window's side of EWMH's `_NET_WM_SYNC_REQUEST`, the basic protocol with one counter.
//!
//! A window manager that speaks it (mutter) sends the window a `_NET_WM_SYNC_REQUEST` client
//! message carrying a 64-bit serial before it resizes the window, and keeps showing the window's
//! last picture until the window's counter reaches that serial. Without it mutter shows the
//! window's contents as soon as it applies the size: on the desk (GNOME 46 mutter on Xorg,
//! NVIDIA GLX) they are black from then until aterm's next swap, so a window the window manager
//! grew flashed black (2 of 12 frames read 16 ms after each 40x30 step were all black).
//!
//! Where the server has SYNC, the window offers a counter at 0 (`_NET_WM_SYNC_REQUEST_COUNTER`,
//! and `_NET_WM_SYNC_REQUEST` in `WM_PROTOCOLS`) and answers a request by setting the counter
//! to its serial. When it answers is this rule:
//!
//! 1. A request is pending from its message until it is answered. A newer one replaces it (mutter
//!    sends none while one is pending; another manager might).
//! 2. The first `ConfigureNotify` after it is the resize it announced. If it changed the window's
//!    size, the request waits for a frame at that size: the application gets `Resized` in the
//!    same dispatch and draws. If not, it is answered at once: there is nothing new to draw.
//! 3. The application's first present after that configure ([`SyncCounter::presented`], from
//!    `WindowExtX11::x11_frame_presented`) answers it: a frame of the new size is on the window.
//! 4. At the end of each event-loop turn, after `AboutToWait`, a request waiting for its frame
//!    is answered unless a redraw is queued: the next turn then comes at once and may present it.
//!    With nothing queued no frame is coming without some other wake (the application drew
//!    nothing at the new size, or paces its frames by a timer), and holding the request would
//!    only keep the window frozen. A request waiting for its configure is held across turns:
//!    mutter 46 sends the configure a few milliseconds after the request, not with it (3 to 4 ms
//!    under the test bed's gnome-shell on `:7`, so the two reach the window in different turns),
//!    and answering first would let mutter show the new size undrawn.
//! 5. No request is held past [`HOLD_CAP`]: the event loop wakes by then
//!    ([`SyncCounter::deadline`]) and answers it whatever it waits for. Holding it longer would
//!    only freeze the window until mutter's own timeout gives up on the counter.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_x11::proto::{atom, sync, xproto};

use super::atoms::_NET_WM_SYNC_REQUEST_COUNTER;
use super::{IgnoreError as _, ReportExt as _, XConnection};

/// The longest a request is held (step 5). Its configure comes within milliseconds and its frame
/// in the turn of the configure; this bounds a manager that never configures and an application
/// that queues redraws without presenting, well inside mutter's own one-second timeout.
pub(super) const HOLD_CAP: Duration = Duration::from_millis(200);

/// What a pending request waits for (steps 2 and 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wait {
    /// The configure the request announced.
    Configure,
    /// A present after a configure that changed the window's size.
    Frame,
}

/// A request not yet answered.
#[derive(Clone, Copy, Debug)]
struct Pending {
    value: i64,
    wait: Wait,
    since: Instant,
}

impl Pending {
    /// When it must be answered, whatever it waits for (step 5).
    fn deadline(&self) -> Instant {
        self.since + HOLD_CAP
    }

    /// Whether a turn that ends at `now` holds this request (steps 4 and 5): it waits for its
    /// configure, or for its frame with a redraw queued (`redraw_queued`), and its deadline has
    /// not come.
    fn held(&self, now: Instant, redraw_queued: impl FnOnce() -> bool) -> bool {
        now < self.deadline()
            && match self.wait {
                Wait::Configure => true,
                Wait::Frame => redraw_queued(),
            }
    }
}

/// A window's resize sync counter and the request it has not answered.
#[derive(Debug)]
pub(crate) struct SyncCounter {
    xconn: Arc<XConnection>,
    counter: sync::Counter,
    pending: Mutex<Option<Pending>>,
}

impl SyncCounter {
    /// Create `window`'s counter at 0 and name it in `_NET_WM_SYNC_REQUEST_COUNTER`. `None`
    /// where the server has no SYNC or the counter could not be made; the window then leaves
    /// `_NET_WM_SYNC_REQUEST` out of `WM_PROTOCOLS`, and no manager sends it a request.
    pub(super) fn offer(xconn: &Arc<XConnection>, window: xproto::Window) -> Option<SyncCounter> {
        let major = xconn.sync_major()?;
        let counter = xconn.xcb_connection().generate_id().ok()?;
        // Own the id before the first request that can create it. A later failure in this
        // offer or the window's constructor must release it without a Window wrapper.
        let sync = SyncCounter { xconn: Arc::clone(xconn), counter, pending: Mutex::new(None) };
        // Checked, so a failure stays this window's and does not reach Xlib's error handler,
        // where the window's creation would take it for its own.
        xconn.send_void(sync::create_counter(major, counter, 0)).ok()?.check().ok()?;
        match xconn.send_void(xproto::change_property32(
            xproto::PropMode::Replace,
            window,
            xconn.atoms()[_NET_WM_SYNC_REQUEST_COUNTER],
            atom::CARDINAL,
            &[counter],
        )) {
            Ok(cookie) => cookie.ignore_error(),
            Err(_) => return None,
        }
        Some(sync)
    }

    fn pending(&self) -> MutexGuard<'_, Option<Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Step 1: the window manager asked for `value`.
    pub(super) fn requested(&self, value: i64) {
        *self.pending() = Some(Pending { value, wait: Wait::Configure, since: Instant::now() });
    }

    /// Step 2: a `ConfigureNotify` came, which changed the window's size or (`resized` false)
    /// did not.
    pub(super) fn configured(&self, xconn: &XConnection, resized: bool) {
        let mut pending = self.pending();
        match pending.as_mut() {
            Some(p) if p.wait == Wait::Configure && resized => p.wait = Wait::Frame,
            Some(p) if p.wait == Wait::Configure => {
                let value = p.value;
                *pending = None;
                drop(pending);
                self.answer(xconn, value);
            },
            _ => {},
        }
    }

    /// Step 3: the application presented a frame of the window.
    pub(super) fn presented(&self, xconn: &XConnection) {
        let mut pending = self.pending();
        if let Some(p) = pending.take_if(|p| p.wait == Wait::Frame) {
            drop(pending);
            self.answer(xconn, p.value);
        }
    }

    /// Steps 4 and 5: an event-loop turn ended at `now`, or the loop woke for [`Self::deadline`].
    /// `redraw_queued` says whether a redraw is queued for the next turn; it is asked only of a
    /// request waiting for its frame.
    pub(super) fn turn_ended(
        &self,
        xconn: &XConnection,
        now: Instant,
        redraw_queued: impl FnOnce() -> bool,
    ) {
        let mut pending = self.pending();
        if let Some(p) = pending.take_if(|p| !p.held(now, redraw_queued)) {
            drop(pending);
            self.answer(xconn, p.value);
        }
    }

    /// When the event loop must wake to answer the pending request, if one is (step 5).
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.pending().as_ref().map(Pending::deadline)
    }

    /// Set the counter to `value`, and send it at once: the window manager is waiting.
    fn answer(&self, xconn: &XConnection, value: i64) {
        let Some(major) = xconn.sync_major() else {
            return;
        };
        let what = "Failed to answer the window manager's resize sync request";
        if let Some(cookie) =
            xconn.send_void(sync::set_counter(major, self.counter, value)).or_report(xconn, what)
        {
            cookie.ignore_error();
        }
        xconn.xcb_connection().flush().or_report(xconn, what);
    }
}

impl Drop for SyncCounter {
    fn drop(&mut self) {
        let xconn = &self.xconn;
        if let Some(major) = xconn.sync_major() {
            if let Ok(cookie) = xconn.send_void(sync::destroy_counter(major, self.counter)) {
                cookie.ignore_error();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steps 4 and 5: a turn holds a request waiting for its configure, and one waiting for its
    /// frame only while a redraw is queued, and neither past the cap.
    #[test]
    fn a_turn_holds_a_configure_or_a_queued_frame_until_the_cap() {
        let since = Instant::now();
        let configure = Pending { value: 7, wait: Wait::Configure, since };
        assert!(configure.held(since, || false), "waiting for its configure: held");
        let frame = Pending { wait: Wait::Frame, ..configure };
        assert!(frame.held(since, || true), "its frame with a redraw queued: held");
        assert!(!frame.held(since, || false), "its frame with nothing queued: answered");
        for p in [configure, frame] {
            assert!(!p.held(since + HOLD_CAP, || true), "{:?} at the cap: answered", p.wait);
        }
    }
}
