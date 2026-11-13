// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn progress_keeps_its_origin_after_replacement_and_scope_unwind() {
    use crate::checker_watch::{CheckerPhase, CheckerWatch};
    static WATCH: CheckerWatch = CheckerWatch::new();
    let generation = WATCH.register(100);
    assert!(WATCH.beat(generation, CheckerPhase::Checking));
    let progress = || Progress::Verifying {
        version: "test".into(),
    };
    assert!(!report_for(CHECKER_REPORTER.get(), progress()));
    let scope = checker_scope(&WATCH, generation);
    let captured = CHECKER_REPORTER.get();
    assert!(
        std::thread::spawn(move || report_for(captured, progress()))
            .join()
            .unwrap()
    );
    let replacement = WATCH.supersede(generation).unwrap();
    assert!(WATCH.beat(replacement, CheckerPhase::Checking));
    assert!(!report_for(captured, progress()));
    assert!(!report_for(CHECKER_REPORTER.get(), progress()));
    drop(scope);
    assert!(!report_for(CHECKER_REPORTER.get(), progress()));

    let scope = checker_scope(&WATCH, replacement);
    assert!(report_for(CHECKER_REPORTER.get(), progress()));
    let unwind = std::panic::catch_unwind(|| {
        let _nested = checker_scope(&WATCH, generation);
        panic!("checker scope unwind");
    });
    assert!(unwind.is_err());
    assert!(report_for(CHECKER_REPORTER.get(), progress()));
    drop(scope);
    assert!(!report_for(CHECKER_REPORTER.get(), progress()));
}

#[test]
fn a_download_with_no_observer_still_restamps_the_checking_heartbeat() {
    use crate::checker_watch::{CheckerPhase, WATCH};
    assert!(
        OBSERVER.get().is_none(),
        "PRECONDITION: the session's shape"
    );
    let dir = crate::scratch_dir("update-beat");
    let part = dir.join("aterm.zip.part");
    let generation = WATCH.register(1_000);
    assert!(WATCH.beat(generation, CheckerPhase::Checking));
    let _checker = checker_scope(&WATCH, generation);
    let stamped = WATCH.snapshot().unwrap().at_secs;
    let watch = watch_download(&part, "0.1.0", "https://example.invalid/aterm.zip");
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut bytes = 0;
    let moved = loop {
        let beat = WATCH.snapshot().unwrap();
        if beat.generation == generation && beat.at_secs > stamped {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        bytes += 4096;
        std::fs::write(&part, vec![0u8; bytes]).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    };
    drop(watch);
    let _ = WATCH.supersede(generation);
    assert!(
        moved,
        "the bytes a session downloads must keep its checker's heartbeat fresh"
    );
}

type Seen = Arc<std::sync::Mutex<Vec<Progress>>>;
type Sink = Arc<dyn Fn(Progress) + Send + Sync>;

fn recorder() -> (Seen, Sink) {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let into = Arc::clone(&seen);
    (seen, Arc::new(move |p| into.lock().unwrap().push(p)))
}

fn wait_for(seen: &std::sync::Mutex<Vec<Progress>>, want: &Progress) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if seen.lock().unwrap().contains(want) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    seen.lock().unwrap().contains(want)
}

#[test]
fn the_probed_size_rides_every_report_once_it_lands() {
    let dir = crate::scratch_dir("update-size");
    let part = dir.join("aterm.zip.part");
    let (seen, sink) = recorder();
    let w = watch_with(&part, "0.95.0", || Some(74_000_000), sink);
    let first = Progress::Downloading {
        version: "0.95.0".into(),
        bytes_done: 0,
        bytes_total: 0,
    };
    assert_eq!(seen.lock().unwrap().first(), Some(&first), "at once");
    assert!(wait_for(
        &seen,
        &Progress::Downloading {
            version: "0.95.0".into(),
            bytes_done: 0,
            bytes_total: 74_000_000,
        }
    ));
    std::fs::write(&part, vec![0u8; 4096]).unwrap();
    assert!(wait_for(
        &seen,
        &Progress::Downloading {
            version: "0.95.0".into(),
            bytes_done: 4096,
            bytes_total: 74_000_000,
        }
    ));
    drop(w);
}

#[test]
fn a_silent_or_slow_size_probe_leaves_the_total_unknown() {
    let dir = crate::scratch_dir("update-slow");
    let part = dir.join("aterm.zip.part");
    std::fs::write(&part, vec![0u8; 100]).unwrap();
    let (seen, sink) = recorder();
    let w = watch_with(&part, "0.95.0", || None, sink);
    assert!(wait_for(
        &seen,
        &Progress::Downloading {
            version: "0.95.0".into(),
            bytes_done: 100,
            bytes_total: 0,
        }
    ));
    drop(w);
    let (_, sink) = recorder();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let answered = Arc::new(AtomicBool::new(false));
    let probe_answered = Arc::clone(&answered);
    let slow = watch_with(
        &part,
        "0.95.0",
        move || {
            let _ = gate.recv_timeout(Duration::from_secs(120));
            probe_answered.store(true, Ordering::Release);
            Some(1)
        },
        sink,
    );
    drop(slow);
    assert!(
        !answered.load(Ordering::Acquire),
        "the download's end never waits for the probe"
    );
    let _ = release.send(());
}
