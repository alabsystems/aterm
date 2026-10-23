// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

//! Window-side EWMH `_NET_WM_SYNC_REQUEST` counter ([`SyncCounter::presented`], [`HOLD_CAP`]).

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use aterm_x11::proto::{atom, sync, xproto};

use super::atoms::_NET_WM_SYNC_REQUEST_COUNTER;
use super::{IgnoreError as _, ReportExt as _, XConnection};

/// The longest a request is held (step 5).
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

    /// Whether a turn that ends at `now` holds this request (steps 4 and 5).
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
    /// Create `window`'s counter at 0 and name it in `_NET_WM_SYNC_REQUEST_COUNTER`.
    pub(super) fn offer(xconn: &Arc<XConnection>, window: xproto::Window) -> Option<SyncCounter> {
        let major = xconn.sync_major()?;
        let counter = xconn.xcb_connection().generate_id().ok()?;
        let sync = SyncCounter { xconn: Arc::clone(xconn), counter, pending: Mutex::new(None) };
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

    /// Step 2: a `ConfigureNotify` arrived, whether or not it changed the window's size.
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

    /// Set the counter to `value`, and send it at once.
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
