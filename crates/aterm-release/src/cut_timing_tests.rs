// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// NEGATIVE CONTROL: serial evaluation cannot receive the sibling's entry.
/// The minute is a hang detector, not a performance assertion.
#[test]
fn independent_gates_overlap_and_join_even_when_one_refuses() {
    let (left_send, left_recv) = std::sync::mpsc::channel();
    let (right_send, right_recv) = std::sync::mpsc::channel();
    let completed = std::sync::atomic::AtomicBool::new(false);
    let done = &completed;
    let (left, right) = independent(
        ("fixture", move || {
            left_send.send(()).unwrap();
            right_recv
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
            Err::<(), _>("fixture refusal")
        }),
        ("policy", move || {
            right_send.send(()).unwrap();
            left_recv
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
            done.store(true, Ordering::SeqCst);
            Ok::<_, &str>(17)
        }),
    );
    assert_eq!(left, Err("fixture refusal"));
    assert_eq!(right, Ok(17));
    assert!(completed.load(Ordering::SeqCst));
}

/// NEGATIVE CONTROL: nested in-process work cannot report process CPU as
/// exclusively its own while a sibling is also consuming that counter.
#[test]
fn logged_parallel_descendants_keep_unknown_cpu_and_their_events() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join("after-cut.log");
    independent_logged(
        Some(&log),
        ("site", || {
            let nested = Timer::start(&log, "hosting");
            assert!(nested.cpu.is_none());
        }),
        ("tap", || ()),
    );
    let text = std::fs::read_to_string(log).unwrap();
    let events: Vec<aterm_json::Value> = text
        .lines()
        .map(|line| aterm_json::from_str(line.strip_prefix("release-event: ").unwrap()).unwrap())
        .collect();
    assert_eq!(events.len(), 6);
    for event in events
        .iter()
        .filter(|event| event["event"].as_str() == Some("cut-gate-finished"))
    {
        assert!(event["self_cpu_s"].is_null());
        assert!(event["child_cpu"]["observed_s"].is_null());
    }
    assert!(
        STACK.with_borrow(Vec::is_empty),
        "joined siblings leave no concurrent frame"
    );
    let serial = Timer::new("serial after join");
    assert_eq!(serial.parent_id, None);
    assert!(!serial.children.lock().unwrap().parallel);
}

#[test]
fn arrival_requires_the_execing_pid() {
    assert!(is_exec_receiver(Some("42"), 42));
    assert!(!is_exec_receiver(Some("43"), 42));
    assert!(!is_exec_receiver(Some("invalid"), 42));
    assert!(!is_exec_receiver(None, 42));
}

#[test]
fn missing_or_regressed_counters_are_not_zero_cpu() {
    assert!(delta(None, Some(3)).is_null());
    assert!(delta(Some(4), Some(3)).is_null());
    assert_eq!(
        delta(Some(1_000_000_000), Some(3_000_000_000)).as_f64(),
        Some(2.0)
    );
    assert_eq!(measure("return-value", || 7), 7);
}

/// NEGATIVE CONTROL: a plain worker-local Timer loses its parent and
/// reports process-wide CPU as though that worker alone consumed it.
#[test]
fn parallel_gates_keep_parent_identity_without_double_counting_cpu() {
    let root = Timer::new("parent");
    let worker_timer = Timer::parallel_parent();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let timer = worker_timer("worker");
                assert_eq!(timer.parent_id, Some(root.id));
                assert!(timer.cpu.is_none());
            })
            .join()
            .unwrap();
    });
    let children = *root.children.lock().unwrap();
    assert!(children.parallel);
    assert!(children.own.is_none());
    assert!(children.child.is_none());
}

#[test]
fn exclusive_cost_keeps_between_gate_work_without_counting_nested_work_twice() {
    let total = Cost {
        wall: 20.0,
        own: Some(8.0),
        child: Some(12.0),
        parallel: false,
    };
    let phase = Cost {
        wall: 10.0,
        own: Some(4.0),
        child: Some(9.0),
        parallel: false,
    };
    let nested = Cost {
        wall: 3.0,
        own: Some(1.0),
        child: Some(2.0),
        parallel: false,
    };
    let mut sum = total.excluding(phase);
    sum.add(phase.excluding(nested));
    sum.add(nested);
    assert_eq!(
        (sum.wall, sum.own, sum.child),
        (total.wall, total.own, total.child)
    );
    let unknown = total.excluding(Cost {
        child: None,
        ..phase
    });
    assert_eq!(unknown.child, None);
    assert_eq!(phase.excluding(total).child, None);
    let root = Timer::new("root");
    assert_eq!(root.parent_id, None);
    let child = Timer::new("nested");
    assert_eq!(child.parent_id, Some(root.id));
    assert_ne!(child.id, root.id);
    drop(child);
    assert!(root.children.lock().unwrap().wall > 0.0);
    assert_eq!(STACK.with_borrow(Vec::len), 1);
    drop(root);
    assert!(STACK.with_borrow(Vec::is_empty));
}

/// NEGATIVE CONTROL: merging the log sink without the cost/identity fields
/// loses the exclusive accounting, while keeping only stdout loses the log.
#[test]
fn logged_timing_keeps_the_same_identity_and_exclusive_cost() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let log = dir.path().join("phase.log");
    let root = Timer::new("root");
    let timer = Timer::start(&log, "logged");
    let id = timer.id;
    drop(timer);
    let events: Vec<aterm_json::Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|line| aterm_json::from_str(line.strip_prefix("release-event: ").unwrap()).unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(event["gate_id"].as_u64(), Some(id));
        assert_eq!(event["process_image"].as_str(), Some(image_id()));
        assert_eq!(event["parent_gate_id"].as_u64(), Some(root.id));
        assert_eq!(
            event["process_id"].as_u64(),
            Some(u64::from(std::process::id()))
        );
    }
    assert_eq!(events[0]["event"].as_str(), Some("cut-gate-started"));
    assert_eq!(events[1]["event"].as_str(), Some("cut-gate-finished"));
    assert!(events[1]["exclusive"]["execution_s"].as_f64().is_some());
}
