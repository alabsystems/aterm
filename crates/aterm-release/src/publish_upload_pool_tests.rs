// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::{Condvar, Mutex, atomic::AtomicUsize, mpsc};
use std::time::Duration;

#[test]
fn upload_rate_counts_only_confirmed_payload_over_request_wall() {
    let success = upload_observation("asset.zip", 1024, 2.0, true);
    assert_eq!(success["confirmed_bytes"].as_u64(), Some(1024));
    assert_eq!(success["bytes_per_request_second"].as_f64(), Some(512.0));
    for (wall, succeeded) in [(2.0, false), (0.0, true)] {
        let event = upload_observation("asset.zip", 1024, wall, succeeded);
        assert!(event["bytes_per_request_second"].is_null());
        if !succeeded {
            assert!(event["confirmed_bytes"].is_null());
        }
    }
}

/// NEGATIVE CONTROL: losing the caller's thread-local route would let a
/// request worker use production GitHub instead of the injected fixture.
#[test]
fn upload_pool_inherits_the_test_github_route() {
    let _route = crate::github::Fixture {
        origins: aterm_update_core::github_rest::Origins::github(),
        token: Some("fixture-token".into()),
        spawn: None,
    }
    .enter();
    upload_pool(&[0, 1], 2, &|_| {
        assert_eq!(
            crate::github::Fixture::current().unwrap().token.as_deref(),
            Some("fixture-token")
        );
        Ok(())
    })
    .unwrap();
}

/// NEGATIVE CONTROL: a serial dispatcher cannot enter all four callbacks
/// while their completion gate is closed; an unbounded one exceeds four.
#[test]
fn upload_pool_overlaps_and_bounds_requests() {
    let gate = (Mutex::new(false), Condvar::new());
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            upload_pool(&[0; 12], 4, &|_| {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                tx.send(()).unwrap();
                let (lock, ready) = &gate;
                drop(
                    ready
                        .wait_while(lock.lock().unwrap(), |open| !*open)
                        .unwrap(),
                );
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
        });
        let entered = (0..4).all(|_| rx.recv_timeout(Duration::from_secs(60)).is_ok());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        worker.join().unwrap().unwrap();
        assert!(entered, "four requests must enter before any finishes");
    });
    assert_eq!(peak.load(Ordering::SeqCst), 4);
}

/// NEGATIVE CONTROL: continuing dispatch after a refusal reaches item 1.
#[test]
fn upload_pool_refuses_without_dispatching_more() {
    let seen = Mutex::new(Vec::new());
    let error = upload_pool(&[0, 1, 2], 1, &|index| {
        seen.lock().unwrap().push(*index);
        Err(Error::new("upload refused"))
    })
    .unwrap_err();
    assert!(error.to_string().contains("upload refused"));
    assert_eq!(*seen.lock().unwrap(), [0]);
}

/// NEGATIVE CONTROL: returning on the first error without joining loses
/// the other request's completion, including its intent settlement.
#[test]
fn upload_pool_joins_in_flight_requests_before_refusing() {
    let entered = (Mutex::new(0), Condvar::new());
    let finished = AtomicUsize::new(0);
    let error = upload_pool(&[0, 1], 2, &|index| {
        let mut count = entered.0.lock().unwrap();
        *count += 1;
        entered.1.notify_all();
        let (count, _) = entered
            .1
            .wait_timeout_while(count, Duration::from_secs(60), |count| *count < 2)
            .unwrap();
        assert_eq!(*count, 2, "both requests must start");
        drop(count);
        if *index == 0 {
            Err(Error::new("refused"))
        } else {
            finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.to_string(), "refused");
    assert_eq!(finished.load(Ordering::SeqCst), 1);
}
