// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The menu-bar menu's fleet traffic: this Mac's fleet activity record
//! (`aterm_types::fleet_activity`) added up over the last hour, kept current by one background
//! reader so the menu's open reads no file.
//!
//! The reader looks every [`TICK`] and when the menu opens. A look reads the record only where
//! it was written since the last one (`aterm_agent::fleet_activity::Watch`), adds the held rows
//! up again (a row leaves the hour by the clock alone), and writes the answer to a slot the
//! glance copies (`App::status_glance`). It tells the App only when the answer changed.
//!
//! The reader also keeps the latest refusal a menu the person opened showed, beside the record
//! (`aterm_ctl::fleet_activity::Record::note_menu_seen`): the menu-bar item's blocked mark
//! shows for a refusal later than that (`status_item::blocked_unseen`), so the mark a person
//! has looked at does not come back when the app restarts. The App states it in memory
//! ([`FleetTraffic::menu_opened`]); the file is the reader's to write, and to read again at
//! each look, so a menu opened in another aterm of this account clears the mark here too.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aterm_types::fleet_activity::Summary;

/// How often the reader looks at the record: how soon a request shows in the menu and a
/// refusal on the bar, and how soon one that left the hour is gone from both.
#[cfg_attr(
    any(not(unix), test),
    expect(dead_code, reason = "the reader is unix's, and a test looks by hand")
)]
pub(crate) const TICK: Duration = Duration::from_secs(10);

/// The traffic as the glance reads it: what the reader last made of the record, and the latest
/// refusal a menu the person opened showed.
pub(crate) struct FleetTraffic {
    slot: Arc<Mutex<Option<Summary>>>,
    seen_ms: Arc<AtomicU64>,
    /// Wakes the reader before its tick; dropping it ends the reader.
    kick: Option<std::sync::mpsc::Sender<()>>,
}

impl FleetTraffic {
    /// The last hour as the reader last added it up; `None` until its first look.
    pub(crate) fn summary(&self) -> Option<Summary> {
        self.slot.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The latest refusal by this Mac's lock that a menu the person opened showed, in Unix
    /// milliseconds; `0` where none did.
    pub(crate) fn seen_ms(&self) -> u64 {
        self.seen_ms.load(Ordering::Acquire)
    }

    /// The person opened a menu that showed this Mac's lock refusing a request at
    /// `blocked_ms`, the latest in it. The reader is woken to keep it on disk and to look at
    /// the record, so the next open shows what came since.
    #[cfg_attr(
        all(not(target_os = "macos"), not(test)),
        expect(dead_code, reason = "the menu-bar status item is macOS's")
    )]
    pub(crate) fn menu_opened(&self, blocked_ms: u64) {
        self.seen_ms.fetch_max(blocked_ms, Ordering::AcqRel);
        if let Some(kick) = &self.kick {
            let _ = kick.send(());
        }
    }

    /// A traffic with no reader behind it, holding `summary` and a refusal seen at `seen_ms`:
    /// a test's.
    #[cfg(test)]
    pub(crate) fn over(summary: Option<Summary>, seen_ms: u64) -> Self {
        Self {
            slot: Arc::new(Mutex::new(summary)),
            seen_ms: Arc::new(AtomicU64::new(seen_ms)),
            kick: None,
        }
    }
}

/// One reader of the record in a state folder: what a look does, apart from its thread.
#[cfg(unix)]
struct Reader {
    watch: aterm_agent::fleet_activity::Watch,
    record: aterm_ctl::fleet_activity::Record,
    slot: Arc<Mutex<Option<Summary>>>,
    seen_ms: Arc<AtomicU64>,
}

#[cfg(unix)]
impl Reader {
    /// A reader of the record in `state` for `traffic`, which has looked at nothing yet.
    fn in_state(state: &std::path::Path, traffic: &FleetTraffic) -> Self {
        Self {
            watch: aterm_agent::fleet_activity::Watch::in_state(state),
            record: aterm_ctl::fleet_activity::Record::in_state(state),
            slot: Arc::clone(&traffic.slot),
            seen_ms: Arc::clone(&traffic.seen_ms),
        }
    }

    /// One look at `now_ms`: the hour before it added up into the slot, and the refusal last
    /// seen brought level with the file ([`Self::seen`]). Answers whether the summary or the
    /// refusal last seen changed: what the bar's mark is drawn from.
    fn look(&mut self, now_ms: u64) -> bool {
        let summary = self
            .watch
            .summary(now_ms, aterm_types::fleet_activity::WINDOW_MS);
        let changed = {
            let mut slot = self.slot.lock().unwrap_or_else(|p| p.into_inner());
            let changed = slot.as_ref() != Some(&summary);
            if changed {
                *slot = Some(summary);
            }
            changed
        };
        self.seen(now_ms) || changed
    }

    /// Bring the refusal last seen level between memory and the file at `now_ms`: the later of
    /// the two is the one known from now on, here and on disk, so an open stated here reaches
    /// the file and one another aterm of this account wrote reaches this one. A time more than
    /// a minute after `now_ms` (`aterm_types::fleet_activity::AHEAD_MS`) was written before
    /// the clock was set back and is forgotten in both, so a refusal since then is not hidden
    /// behind it: the mark shows once more rather than not at all. Answers whether the time
    /// in memory moved. A file that could not be written is tried again at the next look.
    fn seen(&self, now_ms: u64) -> bool {
        let ahead = now_ms.saturating_add(aterm_types::fleet_activity::AHEAD_MS);
        let on_disk = self.record.menu_seen();
        let known = self.seen_ms.load(Ordering::Acquire);
        let kept = on_disk.filter(|at| *at <= ahead).unwrap_or(0);
        let mut moved = false;
        if known > ahead {
            // Only this value is forgotten: an open stated meanwhile stands.
            moved = self
                .seen_ms
                .compare_exchange(known, kept, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();
        }
        let now_known = self.seen_ms.fetch_max(kept, Ordering::AcqRel).max(kept);
        moved |= now_known != known;
        if on_disk.unwrap_or(0) != now_known && now_known <= ahead {
            let _ = self.record.note_menu_seen(now_known);
        }
        moved
    }
}

/// START THE READER of the record in `state` on thread `aterm-fleet-traffic`: it looks at
/// once, then looks every `tick` and at each
/// [`FleetTraffic::menu_opened`], and calls `post` after a look that changed the slot. The
/// caller reads no file and waits for none. The reader ends when `post` answers `false` or the
/// [`FleetTraffic`] is dropped. `None` where the thread could not start.
#[cfg(unix)]
#[cfg_attr(
    test,
    expect(dead_code, reason = "a test looks by hand, on its own thread")
)]
pub(crate) fn spawn(
    state: &std::path::Path,
    tick: Duration,
    post: impl Fn() -> bool + Send + 'static,
) -> Option<FleetTraffic> {
    let state = state.to_path_buf();
    let (kick, kicked) = std::sync::mpsc::channel();
    let traffic = FleetTraffic {
        slot: Arc::new(Mutex::new(None)),
        seen_ms: Arc::new(AtomicU64::new(0)),
        kick: Some(kick),
    };
    let theirs = FleetTraffic {
        slot: Arc::clone(&traffic.slot),
        seen_ms: Arc::clone(&traffic.seen_ms),
        kick: None,
    };
    crate::qos::spawn_lane_thread(
        "aterm-fleet-traffic",
        crate::qos::Role::Background,
        move || {
            let mut reader = Reader::in_state(&state, &theirs);
            loop {
                if reader.look(aterm_ctl::fleet_activity::now_ms()) && !post() {
                    return;
                }
                match kicked.recv_timeout(tick) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        },
    )
    .ok()?;
    Some(traffic)
}

impl crate::App {
    /// The fleet traffic's reader, started by the first glance built once the App has its event
    /// loop's proxy: the menu-bar item's install in a window, `aterm ctl chrome` on an instance
    /// with no item. `None` until then, where no state folder is named, where the thread did
    /// not start, and off unix. A unit test starts no reader and reads the traffic it set.
    pub(crate) fn fleet_traffic(&self) -> Option<&FleetTraffic> {
        #[cfg(all(unix, not(test)))]
        if self.fleet_traffic.get().is_none() {
            let proxy = self.proxy.clone()?;
            let state = aterm_types::dirs::state_dir()?;
            let _ = self.fleet_traffic.set(spawn(&state, TICK, move || {
                proxy.send_event(crate::Wake::FleetTraffic).is_ok()
            }));
        }
        self.fleet_traffic.get().and_then(Option::as_ref)
    }
}

#[cfg(all(test, unix))]
#[path = "fleet_traffic_tests.rs"]
mod tests;
