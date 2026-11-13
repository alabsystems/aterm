// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn usable() -> PathReading {
    PathReading {
        status: PathStatus::Satisfied,
        constrained: false,
        expensive: false,
        link: aterm_objc::path_monitor::PathLink::Wifi,
    }
}

#[test]
fn a_return_to_the_same_usable_path_preserves_the_recovery_edge() {
    let observations = Observations::default();
    assert_eq!(observations.reading(), None);
    observations.publish(usable());
    let first = observations.reading().unwrap();
    observations.publish(usable());
    assert_eq!(observations.reading(), Some(first));
    observations.publish(PathReading {
        status: PathStatus::Unsatisfied,
        ..usable()
    });
    assert!(!observations.reading().unwrap().available);
    observations.publish(usable());
    let recovered = observations.reading().unwrap();
    assert!(recovered.available);
    assert!(recovered.revision > first.revision);
}

#[test]
fn low_data_and_interface_changes_publish_new_retry_evidence() {
    let observations = Observations::default();
    observations.publish(usable());
    let initial = observations.reading().unwrap();
    observations.publish(PathReading {
        constrained: true,
        ..usable()
    });
    let held = observations.reading().unwrap();
    assert!(held.constrained && held.revision > initial.revision);
    observations.publish(PathReading {
        link: aterm_objc::path_monitor::PathLink::Wired,
        ..usable()
    });
    let changed = observations.reading().unwrap();
    assert!(changed.available && !changed.constrained && changed.revision > held.revision);
}

#[test]
fn a_path_change_before_wait_is_not_lost() {
    let observations = Arc::new(Observations::default());
    observations.publish(usable());
    let revision = observations.reading().unwrap().revision;
    observations.publish(PathReading {
        status: PathStatus::Unsatisfied,
        ..usable()
    });
    let (done, completed) = mpsc::sync_channel(1);
    let waiting = Arc::clone(&observations);
    let worker = std::thread::spawn(move || {
        // Longer than the hang detector: the changed revision must end the wait,
        // rather than the fallback timer.
        waiting.wait(Some(revision), Duration::from_secs(3600));
        done.send(waiting.reading()).unwrap();
    });
    let observed = completed
        .recv_timeout(Duration::from_secs(60))
        .expect("the already-published path change must end the wait")
        .unwrap();
    worker.join().unwrap();
    assert!(!observed.available);
    assert!(observed.revision > revision);
}
