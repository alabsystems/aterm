// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{CONTENT_SCROLL_BAND_RING, ContentScrollDelta, ContentScrollState, RowBandMove};
use aterm_models_core::content_scroll_band_history_model as band_history_model;

#[test]
fn band_history_model_proves_and_catches_overwritten_replay() {
    let model = band_history_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn terminal_band_history_conforms_through_overflow_and_rebaseline() {
    use crate::terminal::TerminalBuilder;
    let model = band_history_model();
    assert_eq!(
        model
            .consts
            .iter()
            .find(|(name, _)| *name == "Cap")
            .unwrap()
            .1,
        CONTENT_SCROLL_BAND_RING as i64
    );
    let mut caught_overflow = 0;
    let mut caught_unexplained = 0;
    for poison_at in [None, Some(0), Some(1), Some(8), Some(16), Some(17)] {
        let mut term = TerminalBuilder::new()
            .size(57, 151)
            .ring_buffer_size(1000)
            .build();
        let mut baseline = term.content_scroll_state();
        let mut expected = model.init_state();
        for _ in 0..2 {
            let mut written = Vec::new();
            for step in 0..=CONTENT_SCROLL_BAND_RING + 1 {
                if poison_at == Some(step) {
                    term.process(b"\x1bc");
                    assert!(model.fire("Poison", &mut expected));
                }
                if step > 0 {
                    let top = (step % 3) as u16;
                    let bytes = format!("\x1b[{};52r\x1b[52;1H\n", top + 1);
                    term.process(bytes.as_bytes());
                    written.push(RowBandMove {
                        top,
                        bottom: 51,
                        delta: -1,
                    });
                    assert!(model.fire("Step", &mut expected));
                }
                let current = term.content_scroll_state();
                let actual = match ContentScrollState::delta_since(Some(baseline), current) {
                    ContentScrollDelta::Unchanged => 0,
                    ContentScrollDelta::Bands { first_seq, count } => {
                        assert_eq!(first_seq, baseline.band_seq);
                        assert_eq!(usize::from(count), written.len());
                        for (offset, &movement) in written.iter().enumerate() {
                            assert_eq!(current.band(first_seq + offset as u64), movement);
                        }
                        1
                    }
                    ContentScrollDelta::Invalidate => 2,
                    other => panic!("unexpected reader decision {other:?}"),
                };
                assert_eq!(
                    actual, expected["verdict"],
                    "step {step}, poison {poison_at:?}"
                );
                assert_eq!(
                    current.band_seq - baseline.band_seq,
                    expected["moves"] as u64
                );
                if actual == 2 {
                    let mut unsafe_replay = expected.clone();
                    unsafe_replay.insert("verdict", 1);
                    assert!(!model.check_invariant("ReplayComplete", &unsafe_replay));
                    if expected["unexplained"] == 1 {
                        caught_unexplained += 1;
                    } else {
                        caught_overflow += 1;
                    }
                }
            }
            baseline = term.content_scroll_state();
            assert!(model.fire("Baseline", &mut expected));
            assert_eq!(
                ContentScrollState::delta_since(Some(baseline), baseline),
                ContentScrollDelta::Unchanged
            );
        }
    }
    assert_eq!(caught_overflow, 2);
    assert!(caught_unexplained > 0);
}

fn state(uniform: u64, epoch: u64, batches: u64, seq: u64) -> ContentScrollState {
    ContentScrollState {
        uniform_up_rows: uniform,
        invalidation_epoch: epoch,
        band_batches: batches,
        band_seq: seq,
        ..Default::default()
    }
}

#[test]
fn delta_since_replays_only_fully_explained_epochs() {
    let prev = state(10, 4, 2, 7);
    assert_eq!(
        ContentScrollState::delta_since(None, prev),
        ContentScrollDelta::Baseline
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), prev),
        ContentScrollDelta::Unchanged
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(13, 4, 2, 7)),
        ContentScrollDelta::Translate(3)
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10 + u64::from(u16::MAX) + 1, 4, 2, 7)),
        ContentScrollDelta::Invalidate,
        "an unrepresentable exact delta retires instead of under-translating"
    );

    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 8)),
        ContentScrollDelta::Bands {
            first_seq: 7,
            count: 1
        }
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 12)),
        ContentScrollDelta::Bands {
            first_seq: 7,
            count: 5
        }
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 9, 7, 12)),
        ContentScrollDelta::Bands {
            first_seq: 7,
            count: 5
        }
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 6, 3, 8)),
        ContentScrollDelta::Invalidate
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(13, 5, 3, 8)),
        ContentScrollDelta::Invalidate,
        "later uniform output cannot hide an intervening non-uniform batch"
    );
    let ring = CONTENT_SCROLL_BAND_RING as u64;
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 7 + ring)),
        ContentScrollDelta::Bands {
            first_seq: 7,
            count: 16
        }
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 7 + ring + 1)),
        ContentScrollDelta::Invalidate,
        "seventeen moves between presents overflow the ring: today's reset"
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 5, 3, 7)),
        ContentScrollDelta::Invalidate
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 4, 2, 8)),
        ContentScrollDelta::Invalidate
    );
    assert_eq!(
        ContentScrollState::delta_since(Some(prev), state(10, 4, 3, 7)),
        ContentScrollDelta::Invalidate
    );
    for regressed in [
        state(9, 4, 2, 7),
        state(10, 3, 2, 7),
        state(10, 4, 1, 7),
        state(10, 4, 2, 6),
    ] {
        assert_eq!(
            ContentScrollState::delta_since(Some(prev), regressed),
            ContentScrollDelta::Invalidate
        );
    }
}
