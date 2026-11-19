// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn local_panel_mem_disk_tok_uses_dated_whole_folds_of_one_agent_run() {
    let owner = Owner {
        launch: "terminal".into(),
        generation: 2,
        pgid: 71,
        conversation: "codex thread".into(),
        started: Some(17),
        floor: None,
    };
    let mut panes = Arc::default();
    let row = |sec: u64, input, output| Counters {
        at_ns: sec * 1_000_000_000,
        collected_ns: sec * 1_000_000_000,
        input,
        output,
    };
    let mut observe = |sec: u64, owner: &Owner, counters| {
        absorb(
            &mut panes,
            &Sample {
                at_ns: sec * 1_000_000_000,
                pane_token_observations: vec![Observation {
                    pane: 5,
                    owner: owner.clone(),
                    counters,
                }],
                ..Sample::default()
            },
        );
        panes[&5].clone()
    };
    let initial = observe(10, &owner, Some(row(10, Some(200), Some(100))));
    assert!(initial.latest_at(10_000_000_000).is_none());
    let measured = observe(15, &owner, Some(row(15, Some(200), Some(120))));
    let read = measured.latest_at(15_000_000_000).unwrap();
    assert_eq!(
        (read.input_per_s, read.output_per_s, read.window_ns),
        (Some(0.0), Some(4.0), BUCKET_NS)
    );
    assert_eq!(measured.buckets(15_000_000_000)[11], Some(read));
    let cached = observe(20, &owner, Some(row(15, Some(200), Some(120))));
    assert_eq!(cached.buckets(20_000_000_000)[11], None);
    let partial = observe(21, &owner, None);
    assert!(partial.latest_at(21_000_000_000).is_none());
    let first_after_gap = observe(25, &owner, Some(row(25, None, Some(500))));
    assert!(first_after_gap.latest_at(25_000_000_000).is_none());
    let wide = observe(40, &owner, Some(row(40, None, Some(530))));
    let read = wide.latest_at(40_000_000_000).unwrap();
    assert_eq!((read.input_per_s, read.output_per_s), (None, Some(2.0)));
    assert!(
        wide.buckets(40_000_000_000)[9..]
            .iter()
            .all(Option::is_none)
    );
    assert!(wide.latest_at(101_000_000_000).is_none());
    let with_input = observe(41, &owner, Some(row(41, Some(1_000), Some(535))));
    assert_eq!(
        with_input.latest_at(41_000_000_000).unwrap().input_per_s,
        None
    );
    let reset = observe(45, &owner, Some(row(45, Some(1_005), Some(0))));
    assert!(reset.latest_at(45_000_000_000).is_none());
    assert!(reset.buckets(45_000_000_000)[11].is_none());
    let next = Owner {
        generation: 3,
        ..owner
    };
    let restarted = observe(50, &next, Some(row(50, Some(1_000), Some(500))));
    assert!(restarted.latest.is_none());
    assert!(
        restarted
            .buckets(50_000_000_000)
            .iter()
            .all(Option::is_none)
    );
    let same_conversation_elsewhere = Owner {
        launch: "another terminal".into(),
        ..next.clone()
    };
    for sec in [55, 60] {
        let counters = Some(row(sec, Some(1_000 + sec), Some(500 + sec)));
        absorb(
            &mut panes,
            &Sample {
                at_ns: sec * 1_000_000_000,
                pane_token_observations: vec![
                    Observation {
                        pane: 5,
                        owner: next.clone(),
                        counters,
                    },
                    Observation {
                        pane: 7,
                        owner: same_conversation_elsewhere.clone(),
                        counters,
                    },
                ],
                ..Sample::default()
            },
        );
    }
    assert!(
        panes
            .values()
            .all(|h| h.latest_at(60_000_000_000).is_none()),
        "two terminals claiming one provider stream cannot each take the entire delta"
    );
}
