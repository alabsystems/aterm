// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The band's poll while it rides the reading's turns: counts of polls, frames asked and
//! deadlines armed, on a clock the test steps.

use super::*;
use aterm_types::network_state::PeerObservation;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A window that draws the deck, a stand-in probe thread, and a network source that publishes a
/// new revision at every read (`stopped` holds it at the last one, `busy` makes it answer
/// nothing), observed at the wall clock's now.
struct Rig {
    app: crate::App,
    wid: crate::WindowId,
    reads: Arc<AtomicU64>,
    stopped: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    landed: u64,
    _stats: crate::stats_deck::tests::Attached,
    _reader: Box<dyn std::any::Any>,
}

impl Rig {
    fn new(focused: bool) -> Self {
        let stats = crate::stats_deck::tests::attach_stats();
        let mut app = crate::App::headless_for_test();
        let wid = *app.windows.keys().next().expect("window");
        app.windows.get_mut(&wid).expect("window").focused = focused;
        let reader = app.vitals.attach_test_reader();
        let reads = Arc::new(AtomicU64::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let (count, stop, hold) = (reads.clone(), stopped.clone(), busy.clone());
        let published = std::sync::Mutex::new(inactive_snapshot());
        app.network_source = Some(Arc::new(move |_| {
            let n = count.fetch_add(1, Ordering::SeqCst) + 1;
            if hold.load(Ordering::SeqCst) {
                return None;
            }
            let mut published = published.lock().expect("publication");
            if !stop.load(Ordering::SeqCst) {
                let mut next = NetworkStateSnapshot {
                    revision: n,
                    ..Default::default()
                };
                next.network.visibility = Visibility::Active;
                next.network.observed_at_ms = aterm_time::unix_millis().expect("a wall clock");
                *published = Arc::new(next);
            }
            Some(published.clone())
        }));
        Self {
            app,
            wid,
            reads,
            stopped,
            busy,
            landed: 0,
            _stats: stats,
            _reader: Box::new(reader),
        }
    }

    /// One turn of the loop at `now`, as `new_events` serves the two.
    fn turn(&mut self, now: Instant) {
        self.app.service_vitals(now);
        self.app.service_network_state(now);
    }

    /// The turn that asks for the next reading: at the moment the reading's own deadline names.
    fn ask(&mut self) -> Instant {
        let due = self.app.vitals.next_due.expect("a reading is armed");
        let before = self.app.timed_reading_asked();
        self.turn(due);
        assert_ne!(
            self.app.timed_reading_asked(),
            before,
            "the turn asked for a reading"
        );
        due
    }

    /// The probe thread hands a reading over and its `Wake::Vitals` turn runs at `now`.
    fn land(&mut self, now: Instant) {
        self.landed += 1;
        self.app.vitals.land_for_test(self.landed, now);
        self.turn(now);
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }

    fn frames_asked(&self) -> u64 {
        u64::from(self.app.windows[&self.wid].redraw_requests_for_test.get())
    }

    /// The first turn: it asks for the first reading and polls at once.
    fn start(&mut self) -> Instant {
        let t0 = Instant::now();
        self.turn(t0);
        assert!(self.app.network_rides_reading(t0), "the band alone draws");
        assert_eq!(self.reads(), 1, "the first turn polls at once");
        self.land(t0 + Duration::from_millis(20));
        t0
    }
}

/// An idle window: over K readings the source is read K times, K frames are asked, and no
/// deadline of the band's own falls before the reading's next ask.
#[test]
fn a_band_alone_is_polled_by_each_readings_ask_and_drawn_by_its_frame() {
    for focused in [false, true] {
        let mut rig = Rig::new(focused);
        rig.start();
        let (reads, frames) = (rig.reads(), rig.frames_asked());
        for k in 1..=8_u64 {
            let next_ask = rig.app.vitals.next_due.expect("a reading is armed");
            assert!(
                rig.app
                    .network_state_deadline()
                    .is_none_or(|own| own > next_ask),
                "focused={focused} reading {k}: the band arms no deadline before the ask"
            );
            let asked = rig.ask();
            assert_eq!(
                rig.reads(),
                reads + k,
                "focused={focused} reading {k}: its ask polled"
            );
            assert_eq!(
                rig.frames_asked(),
                frames + k - 1,
                "focused={focused} reading {k}: the poll asked for no frame of its own"
            );
            assert!(rig.app.network_ride.owes(), "the band's change waits");
            assert_eq!(rig.app.network_snapshot.revision, reads + k);
            rig.land(asked + Duration::from_millis(20));
            assert_eq!(
                rig.reads(),
                reads + k,
                "focused={focused} reading {k}: its landing read nothing"
            );
            assert_eq!(
                rig.frames_asked(),
                frames + k,
                "focused={focused} reading {k}: one frame, the reading's"
            );
            assert!(!rig.app.network_ride.owes());
        }
    }
}

/// Another turn still adopts, at most once a second, and asks for no frame; the ask's turn
/// polls whatever the second's gate says.
#[test]
fn a_turn_between_readings_adopts_without_a_frame_and_the_ask_still_polls() {
    let mut rig = Rig::new(false);
    rig.start();
    let asked = rig.ask();
    rig.land(asked + Duration::from_millis(20));
    let (reads, frames) = (rig.reads(), rig.frames_asked());
    let next_ask = rig.app.vitals.next_due.expect("a reading is armed");
    assert!(
        next_ask > asked + Duration::from_secs(2),
        "the background's"
    );
    rig.turn(asked + Duration::from_millis(500));
    assert_eq!(rig.reads(), reads, "within the second: no read");
    rig.turn(next_ask - Duration::from_millis(500));
    assert_eq!(rig.reads(), reads + 1, "a second on: the turn adopts");
    assert_eq!(rig.app.network_snapshot.revision, reads + 1);
    assert_eq!(rig.frames_asked(), frames, "and asks for no frame");
    assert_eq!(rig.ask(), next_ask);
    assert_eq!(
        rig.reads(),
        reads + 2,
        "the ask polls half a second after the last read"
    );
    assert_eq!(rig.frames_asked(), frames, "still no frame of the band's");
    rig.land(next_ask + Duration::from_millis(20));
    assert_eq!(rig.frames_asked(), frames + 1, "one frame for both");
}

/// A reading that does not land holds the band's change back one reading period: the next
/// ask's turn asks for its frame.
#[test]
fn a_reading_that_never_lands_holds_the_band_back_one_period() {
    let mut rig = Rig::new(false);
    rig.start();
    let frames = rig.frames_asked();
    rig.ask();
    assert_eq!(
        rig.frames_asked(),
        frames,
        "the change waits for its reading"
    );
    rig.ask();
    assert_eq!(
        rig.frames_asked(),
        frames + 1,
        "the reading did not land: the next ask draws the band"
    );
    assert!(!rig.app.network_ride.owes());
}

/// While the observer republishes, the ride's deadline is the adopted observation's expiry,
/// later than the next ask; a read that finds the reader busy arms one retry a second on,
/// which draws what it reads at once, and a second busy read arms none.
#[test]
fn the_ride_keeps_the_expiry_edge_and_one_retry_after_a_busy_read() {
    let mut rig = Rig::new(false);
    rig.start();
    let asked = rig.ask();
    let edge = rig.app.network_ride.edge().expect("an observation expires");
    assert_eq!(rig.app.network_state_deadline(), Some(edge));
    assert!(
        edge <= asked + Duration::from_millis(REACHABLE_MS + 1),
        "no later than the observation's expiry"
    );
    rig.land(asked + Duration::from_millis(20));

    rig.busy.store(true, Ordering::SeqCst);
    let asked = rig.ask();
    assert!(rig.app.network_source_busy);
    assert_eq!(
        rig.app.network_state_deadline(),
        Some(asked + Duration::from_secs(1)),
        "one retry, a second on"
    );
    rig.land(asked + Duration::from_millis(20));
    let (reads, frames) = (rig.reads(), rig.frames_asked());
    rig.busy.store(false, Ordering::SeqCst);
    rig.turn(asked + Duration::from_secs(1));
    assert_eq!(rig.reads(), reads + 1, "the retry read");
    assert!(!rig.app.network_source_busy);
    assert_eq!(rig.frames_asked(), frames + 1, "and drew what it read");
    assert_ne!(
        rig.app.network_state_deadline(),
        Some(asked + Duration::from_secs(2)),
        "no tick follows the retry"
    );

    rig.busy.store(true, Ordering::SeqCst);
    let asked = rig.ask();
    rig.turn(asked + Duration::from_secs(1));
    assert!(rig.app.network_source_busy, "busy at the retry too");
    assert_eq!(
        rig.app.network_state_deadline(),
        None,
        "a second busy read arms nothing: the next reading's ask reads again"
    );
    let _ = rig.stopped.load(Ordering::SeqCst);
}

/// [`next_expiry`] names the first millisecond at which [`fresh_snapshot`] changes what it left
/// alone a millisecond before, for the network's observation, a peer's lease and a service.
#[test]
fn the_expiry_edge_is_the_first_millisecond_the_snapshot_changes_by_age() {
    let now = 1_000_000_u64;
    let reachable = |seen: u64, checked: u64, lease: Option<u64>| PeerObservation {
        slot: 7,
        identity: MachineIdentity {
            machine_id: [7; 16],
            incarnation: [1; 16],
            label: None,
        },
        first_seen_ms: 1,
        last_seen_ms: seen,
        last_checked_ms: Some(checked),
        last_repair: None,
        last_authenticated_endpoint: None,
        reachability: Reachability::Reachable,
        instances: None,
        samples: Vec::new(),
        lease_ms: lease,
    };
    let mut network_first = NetworkStateSnapshot::default();
    network_first.network.visibility = Visibility::Active;
    network_first.network.observed_at_ms = now - 5_000;
    network_first
        .network
        .peers
        .push(reachable(now - 1_000, now - 2_000, None));
    let mut peer_first = network_first.clone();
    peer_first.network.observed_at_ms = now;
    peer_first.network.peers[0] = reachable(now - 1_000, now - 9_000, Some(20_000));
    let mut service_first = NetworkStateSnapshot::default();
    service_first.services.secrets.availability = ServiceAvailability::Cached;
    service_first.services.secrets.observed_at_ms = now - 59_000;
    for (what, snapshot, edge) in [
        (
            "the network's",
            network_first,
            now - 5_000 + REACHABLE_MS + 1,
        ),
        ("a peer's lease", peer_first, now - 9_000 + 20_000 + 1),
        ("a service's", service_first, now - 59_000 + 60_000 + 1),
    ] {
        let snapshot = Arc::new(snapshot);
        assert_eq!(next_expiry(&snapshot, now), Some(edge), "{what} expiry");
        assert!(
            Arc::ptr_eq(&fresh_snapshot(snapshot.clone(), Some(edge - 1)), &snapshot),
            "{what} claim stands a millisecond before its edge"
        );
        let expired = fresh_snapshot(snapshot.clone(), Some(edge));
        assert_ne!(
            expired.as_ref(),
            snapshot.as_ref(),
            "{what} claim ends at its edge"
        );
        assert!(
            next_expiry(&expired, edge).is_none_or(|next| next > edge),
            "{what} edge passed: it is not named again"
        );
    }
    assert_eq!(
        next_expiry(&inactive_snapshot(), now),
        None,
        "nothing current: nothing expires"
    );
}
