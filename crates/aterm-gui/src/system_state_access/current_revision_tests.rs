// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::consent_state::SlotKind;
use crate::devtools_helper::{Origin, test_arms};
use aterm_containment::consent::{
    DevToolList, DevToolListLabel, DevToolListing, FdaProbe, FdaState,
};
use aterm_containment::{DevToolGrant, DevToolProbe, DevToolProbeLabel, DevToolState, ProbeGate};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};

static SERIAL: Mutex<()> = Mutex::new(());
static GRANTED: AtomicBool = AtomicBool::new(true);
static READS: AtomicUsize = AtomicUsize::new(0);
static HELD_READ: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>> = Mutex::new(None);

fn subject() -> Option<&'static str> {
    Some(test_arms::SUBJECT)
}

fn read_grant(_gate: ProbeGate, _subject: &str) -> DevToolProbe {
    READS.fetch_add(1, Ordering::SeqCst);
    DevToolProbe {
        state: if GRANTED.load(Ordering::SeqCst) {
            DevToolState::Allowed
        } else {
            DevToolState::Denied
        },
        label: DevToolProbeLabel::Observed,
        requests: Some(1),
        not_allowed: Some(u32::from(!GRANTED.load(Ordering::SeqCst))),
        answered_at_ms: None,
    }
}

fn read_list(_gate: ProbeGate, _subject: &str) -> DevToolList {
    let granted = GRANTED.load(Ordering::SeqCst);
    let held = HELD_READ.lock().unwrap().take();
    if let Some((entered, resume)) = held {
        entered.send(()).unwrap();
        resume
            .recv_timeout(Duration::from_secs(60))
            .expect("the test releases its old read");
    }
    test_arms::list(
        if granted {
            DevToolListing::On
        } else {
            DevToolListing::Off
        },
        DevToolListLabel::Read,
        Some(2),
    )
}

fn fda_on(_gate: ProbeGate) -> FdaProbe {
    probe(FdaState::Granted)
}

struct Scenario {
    app: App,
    ready: mpsc::Receiver<SlotKind>,
    records_before: usize,
    _serial: MutexGuard<'static, ()>,
}

impl Scenario {
    fn granted() -> Self {
        let serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        GRANTED.store(true, Ordering::SeqCst);
        READS.store(0, Ordering::SeqCst);
        set_front(false);
        let mut app = new_mac(test_arms::opening(), test_arms::fda_opening());
        app.consent = reread(FdaState::Granted);
        app.consent.set_fda_arm_for_test(fda_on);
        app.consent.set_devtools_arms_for_test(read_grant, subject);
        app.consent.probes_for_test().devtools_list = read_list;
        app.set_privacy_for_test(switches(None));
        focus(&mut app, true);
        let (send, ready) = mpsc::channel();
        app.consent.set_slot_wake(Arc::new(move |kind| {
            let _ = send.send(kind);
        }));
        let mut scenario = Self {
            app,
            ready,
            records_before: records().len(),
            _serial: serial,
        };
        scenario.app.read_devtools_list(Instant::now());
        scenario.finish_read();
        scenario.app.settle_access_steps(Instant::now());
        assert_eq!(
            scenario.app.devtools_helper.reading().unwrap().grant,
            DevToolGrant::Granted
        );
        assert_eq!(READS.load(Ordering::SeqCst), 1);
        assert_eq!(scenario.app.devtools_helper.queued(), None);
        scenario
    }

    fn finish_read(&mut self) {
        loop {
            let kind = self
                .ready
                .recv_timeout(Duration::from_secs(60))
                .expect("the dispatched inert probe completes");
            if kind == SlotKind::Devtools {
                break;
            }
        }
        self.app.on_devtools_probe_ready();
    }

    fn revoke(&mut self) {
        GRANTED.store(false, Ordering::SeqCst);
        // The retired slot has no answer and no retry floor. The helper's
        // independently retained Granted value must not answer a new approval.
        self.app.consent.refresh_devtools();
        assert!(self.app.consent.devtools_slot_idle_for_test());
        assert_eq!(
            self.app.devtools_helper.reading().unwrap().grant,
            DevToolGrant::Granted
        );
    }

    fn require_current_read(&mut self) {
        assert!(
            !self.app.consent.devtools_slot_idle_for_test(),
            "a new access revision dispatches its own current reading"
        );
        assert!(
            !records()[self.records_before..].contains(&(
                2,
                Access::DeveloperTools,
                AccessOutcome::Granted
            )),
            "the previous revision's Granted value earns no current completion"
        );
        self.finish_read();
        assert_eq!(READS.load(Ordering::SeqCst), 2);
        assert_eq!(
            self.app.devtools_helper.reading().unwrap().grant,
            DevToolGrant::NotGranted
        );
        assert_eq!(self.app.devtools_helper.queued(), Some(Origin::Automatic));
    }
}

#[test]
fn devtools_current_revision_rereads_a_previously_granted_permission() {
    let mut scenario = Scenario::granted();
    scenario.revoke();
    let opens = test_arms::opens();
    approve(&mut scenario.app, 2);
    scenario.app.settle_access_steps(Instant::now());
    scenario.app.resume_held_launch_steps(Instant::now());
    scenario.require_current_read();
    assert_eq!(
        test_arms::opens(),
        opens,
        "a reading while a terminal is in front opens no pane"
    );
    scenario.app.read_devtools_list(Instant::now());
    scenario.app.settle_access_steps(Instant::now());
    assert_eq!(
        READS.load(Ordering::SeqCst),
        2,
        "one reading for the new approval"
    );
}

#[test]
fn devtools_current_revision_retires_a_still_fresh_granted_cache() {
    for granted_now in [false, true] {
        let mut scenario = Scenario::granted();
        let opens = test_arms::opens();
        GRANTED.store(granted_now, Ordering::SeqCst);
        assert_eq!(
            scenario
                .app
                .consent
                .devtools_cached(ProbeGate::on(), false)
                .grant(),
            DevToolGrant::Granted
        );
        approve(&mut scenario.app, 2);
        scenario.app.settle_access_steps(Instant::now());
        // No timeout or external invalidation makes the previous answer stale:
        // the new approval itself must retire it before requesting this read.
        if !granted_now {
            assert_ne!(
                scenario
                    .app
                    .consent
                    .devtools_cached(ProbeGate::on(), false)
                    .grant(),
                DevToolGrant::Granted
            );
            scenario.require_current_read();
        } else {
            scenario.finish_read();
            assert_eq!(READS.load(Ordering::SeqCst), 2);
            assert_eq!(
                scenario.app.devtools_helper.reading().unwrap().grant,
                DevToolGrant::Granted
            );
            assert_eq!(scenario.app.devtools_helper.queued(), None);
            assert_eq!(
                scenario
                    .app
                    .next_devtools_hand_over_deadline(Instant::now()),
                None
            );
        }
        assert_eq!(
            test_arms::opens(),
            opens,
            "a fresh On result opens nothing; an Off waits for the page"
        );
    }
}

#[test]
fn devtools_current_revision_replaces_a_retired_in_flight_read_without_focus() {
    let mut scenario = Scenario::granted();
    let (entered, began) = mpsc::channel();
    let (resume, proceed) = mpsc::channel();
    *HELD_READ.lock().unwrap() = Some((entered, proceed));
    scenario.app.consent.refresh_devtools();
    let _ = scenario.app.consent.devtools(ProbeGate::on(), false);
    began
        .recv_timeout(Duration::from_secs(60))
        .expect("the old injected read reaches its barrier");
    GRANTED.store(false, Ordering::SeqCst);
    let opens = test_arms::opens();
    approve(&mut scenario.app, 2);
    let now = Instant::now();
    scenario.app.settle_access_steps(now);
    scenario.app.resume_held_launch_steps(now);
    let retry = scenario.app.next_devtools_hand_over_deadline(now);
    let queued = scenario.app.hand_devtools_to_owner(Origin::SystemStateAct);
    resume.send(()).unwrap();

    // The old completion carries On, but its retired ticket supplies no
    // answer. Its actual wake must dispatch the current read immediately.
    scenario.finish_read();
    assert!(
        !scenario.app.consent.devtools_slot_idle_for_test(),
        "the retired completion owes its replacement without a focus event"
    );
    assert_eq!(
        scenario.app.devtools_helper.reading().unwrap().grant,
        DevToolGrant::Granted
    );
    scenario.finish_read();
    assert_eq!(READS.load(Ordering::SeqCst), 3);
    assert!(
        retry.is_some_and(|at| at > now),
        "a refused/in-flight dispatch retains a future wake"
    );
    assert_eq!(
        queued,
        crate::devtools_helper::HandOver::Queued,
        "old Granted cannot refuse a current row act while its read waits"
    );
    assert_eq!(
        scenario.app.devtools_helper.reading().unwrap().grant,
        DevToolGrant::NotGranted
    );
    assert_eq!(
        scenario.app.devtools_helper.queued(),
        Some(Origin::Automatic)
    );
    assert_eq!(test_arms::opens(), opens);
}

#[test]
fn devtools_current_revision_keeps_a_paused_recheck_until_the_probe_is_on() {
    for off in ["enabled", "check"] {
        let mut scenario = Scenario::granted();
        scenario.revoke();
        let opens = test_arms::opens();
        scenario.app.set_privacy_for_test(switches(Some(off)));
        approve(&mut scenario.app, 2);
        scenario.app.settle_access_steps(Instant::now());
        scenario.app.resume_held_launch_steps(Instant::now());
        assert!(scenario.app.consent.devtools_slot_idle_for_test());
        assert_eq!(
            READS.load(Ordering::SeqCst),
            1,
            "{off}: no probe while paused"
        );
        assert_eq!(test_arms::opens(), opens);

        scenario.app.set_privacy_for_test(switches(None));
        scenario.app.settle_access_steps(Instant::now());
        scenario.app.resume_held_launch_steps(Instant::now());
        scenario.require_current_read();
        assert_eq!(
            test_arms::opens(),
            opens,
            "{off}: no pane behind a terminal"
        );
    }
}

#[test]
fn devtools_current_revision_unchanged_approval_does_not_restart_a_settled_watch() {
    let mut scenario = Scenario::granted();
    let opens = test_arms::opens();
    approve(&mut scenario.app, 1);
    scenario.app.settle_access_steps(Instant::now());
    scenario.app.resume_held_launch_steps(Instant::now());
    scenario
        .app
        .read_devtools_list(Instant::now() + crate::devtools_watch::REREAD_EVERY * 9);
    assert_eq!(READS.load(Ordering::SeqCst), 1);
    assert_eq!(scenario.app.devtools_helper.queued(), None);
    assert_eq!(test_arms::opens(), opens);
}
