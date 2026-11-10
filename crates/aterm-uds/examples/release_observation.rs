// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded measurement of the close-only flock race beside an ordinary Command spawn loop.
//! Run with `targo --unverified run -p aterm-uds --example release_observation`.

#[cfg(all(unix, debug_assertions))]
fn main() {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    let root = aterm_tempfile::tempdir().unwrap();
    let path = root.path().join("lock");
    std::fs::write(&path, b"").unwrap();
    if std::env::args().any(|arg| arg == "--held") {
        let owner = std::fs::File::open(&path).unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "negative control explicitly unlocks below"
        )]
        owner.try_lock().unwrap();
        let probe = std::fs::File::open(&path).unwrap();
        let began = Instant::now();
        let result = std::panic::catch_unwind(|| {
            aterm_uds::deadline::once_released(|| {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "negative control probes a never-released native lock"
                )]
                probe.try_lock().map_err(io::Error::from)
            });
        });
        owner.unlock().unwrap();
        println!(
            "permanent_holder_failed={} wall_s={:.6}",
            result.is_err(),
            began.elapsed().as_secs_f64()
        );
        assert!(result.is_err());
        return;
    }
    let stop = AtomicBool::new(false);
    let began = Instant::now();
    let (attempts, old_held, helper_passed, children) = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let mut children = 0;
            while !stop.load(Ordering::Acquire) {
                #[expect(
                    clippy::disallowed_types,
                    reason = "measurement requires the ordinary std spawn window"
                )]
                let status = std::process::Command::new("/usr/bin/true")
                    .status()
                    .unwrap();
                assert!(status.success());
                children += 1;
            }
            children
        });
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let stopping = Stop(&stop);
        let mut old_held = 0;
        let mut helper_passed = 0;
        let mut attempts = 0;
        for _ in 0..100_000 {
            let owner = std::fs::File::open(&path).unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "measure a close-only lease, deliberately no explicit unlock"
            )]
            owner.try_lock().unwrap();
            let probe = std::fs::File::open(&path).unwrap();
            drop(owner);
            attempts += 1;
            #[expect(clippy::disallowed_methods, reason = "probe explicitly unlocks below")]
            let sample = || probe.try_lock().map_err(io::Error::from);
            match sample() {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    old_held += 1;
                    aterm_uds::deadline::once_released(sample);
                    helper_passed += 1;
                }
                Err(error) => panic!("probe: {error}"),
            }
            probe.unlock().unwrap();
            if old_held >= 100 {
                break;
            }
        }
        drop(stopping);
        (attempts, old_held, helper_passed, worker.join().unwrap())
    });
    println!(
        "attempts={attempts} children={children} old_false_held={old_held} helper_passed={helper_passed} wall_s={:.6}",
        began.elapsed().as_secs_f64()
    );
    assert!(
        old_held > 0,
        "no race observed within the bounded measurement; no gain established"
    );
    assert_eq!(old_held, helper_passed);
}

#[cfg(not(all(unix, debug_assertions)))]
fn main() {
    eprintln!("release_observation requires Unix and debug assertions");
}
