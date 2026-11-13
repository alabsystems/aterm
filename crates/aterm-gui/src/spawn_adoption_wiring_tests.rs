// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn an_adopted_timeline_opens_with_a_handoff_gap() {
    use std::sync::Mutex as StdMutex;
    let fresh_cast = crate::cast::CastRecorder::new(80, 24).to_asciicast();
    let fresh_temporal = crate::temporal::TemporalRecorder::new().status_tail();
    assert!(
        !crate::session_timeline::SessionTimeline::default().muse_cards_need_account(),
        "a fresh terminal can bootstrap its first anonymous usage card"
    );
    for (from_build, word, json) in [
        (Some(812_u64), "812", "\"from_build\": 812"),
        (None, "-", "\"from_build\": null"),
    ] {
        let timeline = StdMutex::new(crate::session_timeline::SessionTimeline::default());
        let cast = StdMutex::new(crate::cast::CastRecorder::new(80, 24));
        let temporal = StdMutex::new(crate::temporal::TemporalRecorder::new());
        super::mark_handoff_gap(
            &timeline,
            &cast,
            &temporal,
            from_build,
            0,
            Default::default(),
            false,
        );
        assert!(
            timeline.lock().unwrap().muse_cards_need_account(),
            "a successor cannot attribute the predecessor's anonymous cards"
        );
        timeline
            .lock()
            .unwrap()
            .record_hand_row("spawned", "state=spawning".to_string());
        let rows: Vec<(u64, &str, String)> = timeline
            .lock()
            .unwrap()
            .since(None)
            .map(|e| (e.id, e.kind, e.payload.to_string()))
            .collect();
        assert_eq!(
            rows[0],
            (1, "handoff", format!("carried=0 from_build={word}")),
            "the handoff row opens the adopted timeline: {rows:?}"
        );
        let header = cast.lock().unwrap().to_asciicast();
        let header = header.lines().next().unwrap().to_string();
        assert!(
            header.contains("\"aterm_handoff\": {\"carried\": 0, ") && header.contains(json),
            "the cast header discloses the handoff: {header}"
        );
        assert_eq!(
            temporal.lock().unwrap().status_tail(),
            format!(" carried=0 from_build={word}"),
            "`temporal status` reports the handoff gap"
        );
    }
    assert!(!fresh_cast.contains("aterm_handoff"), "{fresh_cast}");
    assert_eq!(fresh_temporal, "", "a fresh recorder's status is unchanged");
}

#[test]
fn a_keeper_recovery_shows_the_crashed_windows_anchor_its_first_row() {
    use crate::session_timeline::{HANDOFF_ID_RESERVE, SessionTimeline};
    use std::sync::Mutex as StdMutex;
    let adopt = |timeline_id: u64, keeper: bool| {
        let timeline = StdMutex::new(SessionTimeline::default());
        super::mark_handoff_gap(
            &timeline,
            &StdMutex::new(crate::cast::CastRecorder::new(80, 24)),
            &StdMutex::new(crate::temporal::TemporalRecorder::new()),
            (!keeper).then_some(812),
            timeline_id,
            Default::default(),
            keeper,
        );
        timeline
    };
    let mut producer = SessionTimeline::default();
    for _ in 0..3 {
        producer.record_hand_row("state-change", "state=alive".to_string());
    }
    let updated = adopt(producer.handoff_id(), false);
    updated
        .lock()
        .unwrap()
        .record_hand_row("state-change", "state=alive".to_string());
    let anchor = updated.lock().unwrap().high_id().expect("rows");
    assert_eq!(anchor, 3 + HANDOFF_ID_RESERVE + 2);

    let recovered = adopt(0, true);
    let page = |anchor: u64| -> Vec<(u64, &'static str)> {
        recovered
            .lock()
            .unwrap()
            .since(Some(anchor))
            .map(|e| (e.id, e.kind))
            .collect()
    };
    assert_eq!(
        page(anchor),
        vec![(1, "handoff")],
        "the crashed window's anchor reads the recovery's first row at once"
    );
    recovered
        .lock()
        .unwrap()
        .record_hand_row("state-change", "state=alive".to_string());
    assert_eq!(
        page(anchor),
        vec![(1, "handoff"), (2, "state-change")],
        "and every row after it"
    );
    assert_eq!(page(1), vec![(2, "state-change")], "negative control");
    assert_eq!(page(2), vec![], "negative control: at the newest, nothing");
}
