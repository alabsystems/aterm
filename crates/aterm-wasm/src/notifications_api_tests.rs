// SPDX-License-Identifier: MIT
// Copyright 2026 Andrew Yates

use super::{
    drain_notifications_json, enqueue, Notification, NotificationQueue, NotificationUrgency,
    MAX_QUEUED_NOTIFICATIONS,
};

#[test]
fn serialization_keeps_nullable_fields_escaping_and_urgencies() {
    let queue = NotificationQueue::default();
    for note in [
        Notification {
            id: Some("i\"".to_owned()),
            title: Some("漢\t".to_owned()),
            body: Some("\\\0\u{1f}🙂".to_owned()),
            urgency: NotificationUrgency::Low,
        },
        Notification {
            id: None,
            title: Some(String::new()),
            body: Some(String::new()),
            urgency: NotificationUrgency::Critical,
        },
        Notification {
            id: Some("n".to_owned()),
            title: None,
            body: None,
            urgency: NotificationUrgency::Normal,
        },
    ] {
        enqueue(&queue, note);
    }
    assert_eq!(
        drain_notifications_json(&queue).as_deref(),
        Some(concat!(
            r#"[{"id":"i\"","title":"漢\t","body":"\\\u0000\u001f🙂","urgency":"low"},"#,
            r#"{"id":null,"title":"","body":"","urgency":"critical"},"#,
            r#"{"id":"n","title":null,"body":null,"urgency":"normal"}]"#
        ))
    );
    assert!(drain_notifications_json(&queue).is_none());
}

#[test]
#[ignore = "manual native notification queue serialization measurement; run alone"]
fn notification_queue_batch_cost() {
    use std::hint::black_box;
    use std::time::Instant;

    let count: usize = std::env::var("ATERM_NOTIFICATION_BENCH_COUNT")
        .map_or(Ok(MAX_QUEUED_NOTIFICATIONS), |value| value.parse())
        .expect("notification count");
    assert!((1..=MAX_QUEUED_NOTIFICATIONS).contains(&count));
    let queue = NotificationQueue::default();
    let note = Notification {
        id: None,
        title: None,
        body: Some("bench".to_owned()),
        urgency: NotificationUrgency::Normal,
    };
    let record = r#"{"id":null,"title":null,"body":"bench","urgency":"normal"}"#;
    let expected = format!("[{}]", vec![record; count].join(","));
    eprintln!(
        "notification queue: count={count} batches=20 drains_per_batch=100 bytes={}",
        expected.len()
    );
    for batch in 0..20 {
        let start = Instant::now();
        for _ in 0..100 {
            for _ in 0..count {
                enqueue(&queue, note.clone());
            }
            let output =
                drain_notifications_json(black_box(&queue)).expect("queued synthetic notes");
            assert_eq!(black_box(&output), &expected);
        }
        eprintln!(
            "notification queue batch={batch} elapsed_ns={}",
            start.elapsed().as_nanos()
        );
    }
    assert!(drain_notifications_json(&queue).is_none());
}

#[test]
fn authorized_osc9_round_trips_through_the_drain() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.authorize_notifications(true);
    t.process(b"\x1b]9;Build finished\x07");
    assert_eq!(
        t.take_notifications().as_deref(),
        Some(r#"[{"id":null,"title":null,"body":"Build finished","urgency":"normal"}]"#),
        "OSC 9 message must surface as a body-only notification"
    );
    assert!(t.take_notifications().is_none(), "drain empties the queue");
}

#[test]
fn unauthorized_notifications_never_enqueue() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.process(b"\x1b]9;nope\x07");
    t.process(b"\x1b]99;u=2;nope\x07");
    t.process(b"\x1b]777;notify;Nope;Nope\x07");
    assert!(
        t.take_notifications().is_none(),
        "unauthorized notifications must never reach the queue"
    );
    t.authorize_notifications(true);
    t.authorize_notifications(false);
    t.process(b"\x1b]9;still nope\x07");
    assert!(
        t.take_notifications().is_none(),
        "revoke re-closes the gate"
    );
}

#[test]
fn advanced_forms_carry_id_title_body_urgency() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.authorize_notifications(true);
    t.process(b"\x1b]99;i=7:u=2:p=title;Alert\x07");
    t.process(b"\x1b]777;notify;Title Here;Body Here\x07");
    assert_eq!(
        t.take_notifications().as_deref(),
        Some(concat!(
            r#"[{"id":"7","title":"Alert","body":null,"urgency":"critical"},"#,
            r#"{"id":null,"title":"Title Here","body":"Body Here","urgency":"normal"}]"#
        )),
        "OSC 99 id/urgency/title and OSC 777 title/body must round-trip"
    );
}

/// The queue's own bound: past its cap a notification is refused, the queued ones kept.
#[test]
fn queue_is_bounded_and_drops_new_when_full() {
    let queue = NotificationQueue::default();
    let note = |i: usize| Notification {
        id: None,
        title: None,
        body: Some(format!("n {i}")),
        urgency: NotificationUrgency::Normal,
    };
    for i in 0..(MAX_QUEUED_NOTIFICATIONS + 8) {
        enqueue(&queue, note(i));
    }
    let json = drain_notifications_json(&queue).expect("queued notifications");
    assert_eq!(
        json.matches("\"body\":\"n ").count(),
        MAX_QUEUED_NOTIFICATIONS,
        "the cap must hold under a flood"
    );
    assert!(
        json.contains("\"n 0\""),
        "oldest survives (drop-new posture)"
    );
    assert!(
        !json.contains(&format!("\"n {MAX_QUEUED_NOTIFICATIONS}\"")),
        "overflow notifications are refused, not evicting old ones"
    );
    assert!(
        drain_notifications_json(&queue).is_none(),
        "drain empties the queue"
    );
    enqueue(&queue, note(0));
    assert!(
        drain_notifications_json(&queue)
            .expect("post-drain enqueue works")
            .contains("\"n 0\""),
        "capacity is restored by draining"
    );
}

/// A program's flood reaches the queue at the engine's notification pace, oldest first, so
/// it stays under the queue's cap. Every read is at one clock, so the pace refills nothing
/// however long the loop takes. NEGATIVE CONTROL: with the engine unpaced, the queue fills
/// to its cap.
#[test]
fn a_flood_reaches_the_queue_at_the_engines_pace() {
    let mut t = crate::system_font_terminal(24, 80, 16.0);
    t.authorize_notifications(true);
    let at = aterm_core::terminal::ClockReading::now();
    for i in 0..(MAX_QUEUED_NOTIFICATIONS + 8) {
        t.term
            .process_at(format!("\x1b]9;n {i}\x07").as_bytes(), at);
    }
    let json = t.take_notifications().expect("queued notifications");
    let queued = json.matches("\"body\":\"n ").count();
    assert!(
        (1..MAX_QUEUED_NOTIFICATIONS).contains(&queued),
        "the pace holds a flood under the cap: {queued}"
    );
    assert!(json.contains("\"n 0\""), "the first is delivered");
    t.term.process_at(b"\x1b]9;later\x07", at);
    assert!(
        t.take_notifications().is_none(),
        "a spent pace queues none at the same clock"
    );
}
