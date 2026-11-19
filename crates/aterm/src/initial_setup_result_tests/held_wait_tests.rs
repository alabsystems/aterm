// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn an_unreleased_hold_records_timeout_and_fails_its_owning_fixture() {
    let _globals = globals();
    let fixture = TestIo::install();

    // An expired deadline alone is harmless once the required release happened.
    drop(wait_while_until(
        script(),
        |script| script.hold,
        Instant::now(),
    ));
    assert!(!script().held_wait_timed_out);

    change(|script| script.hold = true);
    let (finished, answer) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let failed = std::panic::catch_unwind(|| {
            drop(wait_while_until(
                script(),
                |script| script.hold,
                Instant::now(),
            ));
        })
        .is_err();
        finished
            .send(failed)
            .expect("return the caught wait failure");
    });
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    worker.join().expect("the worker caught the wait failure");
    {
        let script = script();
        assert!(script.hold, "expiry never releases the hold");
        assert!(
            script.held_wait_timed_out,
            "the caught failure stays recorded"
        );
    }

    // The background panic was caught. Teardown must still fail the owning case after its
    // workers drain; this catch is the negative control observing that failure.
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(fixture)));
    assert!(
        ended.is_err(),
        "a caught background timeout cannot become a pass"
    );
    assert!(observe_idle() && stop_worker_idle());
    assert!(!SETUP_FIXTURE_UNDRAINED.load(std::sync::atomic::Ordering::Acquire));
}
