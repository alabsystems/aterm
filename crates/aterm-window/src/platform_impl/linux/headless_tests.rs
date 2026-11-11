// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Added by the aterm project in 2026 to its copy of winit 0.30.13
// (see the repository NOTICE).

use super::*;

fn drive<T: 'static>(
    mut loop_: EventLoop<T>,
    mut on_turn: impl FnMut(&RootELW, usize),
) -> Vec<String> {
    let mut log = Vec::new();
    let mut turns = 0usize;
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let remaining = until.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "headless event-loop test hung");
        let status = loop_.pump_events(Some(remaining), |event, target| match &event {
            Event::NewEvents(cause) => log.push(format!("NewEvents({cause:?})")),
            Event::Resumed => log.push("Resumed".into()),
            Event::UserEvent(_) => log.push("UserEvent".into()),
            Event::AboutToWait => {
                log.push("AboutToWait".into());
                turns += 1;
                on_turn(target, turns);
            },
            Event::LoopExiting => log.push("LoopExiting".into()),
            _ => {},
        });
        assert!(Instant::now() < until, "headless event-loop test hung");
        if let PumpStatus::Exit(code) = status {
            assert_eq!(code, 0);
            break;
        }
    }
    log
}

#[test]
fn the_first_turn_is_init_then_resumed_and_the_last_is_loop_exiting() {
    let loop_: EventLoop<()> = EventLoop::new().unwrap();
    let log = drive(loop_, |target, _| target.p.exit());
    assert_eq!(log.first().map(String::as_str), Some("NewEvents(Init)"));
    assert_eq!(log.get(1).map(String::as_str), Some("Resumed"));
    assert_eq!(log.last().map(String::as_str), Some("LoopExiting"));
    assert_eq!(log.iter().filter(|l| *l == "Resumed").count(), 1, "Resumed is once, not per turn");
}

#[test]
fn an_event_posted_before_the_loop_runs_is_still_delivered() {
    let loop_: EventLoop<u32> = EventLoop::new().unwrap();
    loop_.create_proxy().send_event(7).unwrap();
    let log = drive(loop_, |target, _| target.p.exit());
    assert!(log.contains(&"UserEvent".to_string()), "a pre-posted event was dropped: {log:?}");
}

#[test]
fn an_event_posted_after_init_is_delivered_from_another_thread_under_wait() {
    let loop_: EventLoop<u32> = EventLoop::new().unwrap();
    let proxy = loop_.create_proxy();
    loop_.window_target().p.set_control_flow(ControlFlow::Wait);
    let (start, ready) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        ready.recv_timeout(Duration::from_secs(60)).expect("the first turn");
        proxy.send_event(11).expect("live loop");
    });
    let log = drive(loop_, |target, turn| {
        if turn == 1 {
            start.send(()).expect("the proxy worker");
        } else {
            target.p.exit();
        }
    });
    worker.join().expect("the proxy worker");
    assert!(log.contains(&"UserEvent".to_string()), "a threaded wake never arrived: {log:?}");
}

#[test]
fn a_due_wait_until_reports_resume_time_reached() {
    let loop_: EventLoop<()> = EventLoop::new().unwrap();
    let deadline = Instant::now();
    loop_.window_target().p.set_control_flow(ControlFlow::WaitUntil(deadline));
    let log = drive(loop_, |target, turn| {
        if turn >= 2 {
            target.p.exit();
        }
    });
    assert!(
        log.iter().any(|l| l.contains("ResumeTimeReached")),
        "an elapsed deadline must report ResumeTimeReached, got {log:?}"
    );
}

#[test]
fn an_exit_on_the_init_turn_runs_no_further_turn() {
    let loop_: EventLoop<()> = EventLoop::new().unwrap();
    let log = drive(loop_, |target, _| target.p.exit());
    assert_eq!(
        log.iter().filter(|l| l.starts_with("NewEvents")).count(),
        1,
        "a loop told to exit on Init ran another turn: {log:?}"
    );
}
