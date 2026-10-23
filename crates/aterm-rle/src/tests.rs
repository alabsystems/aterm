// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tests for run-length encoding primitives.

use super::*;

fn assert_rle_matches_model(rle: &Rle<u8>, model: &[u8], step: usize) {
    assert_eq!(
        rle.len() as usize,
        model.len(),
        "length mismatch at step {step}"
    );
    let run_total: u32 = rle.runs.iter().map(|run| run.length).sum();
    assert_eq!(
        run_total, rle.total_length,
        "sum of run lengths must match total_length at step {step}"
    );
    let mut expected_start = 0u32;
    for (run_idx, run) in rle.runs.iter().enumerate() {
        assert!(
            run.length > 0,
            "zero-length run at index {run_idx} (step {step})"
        );
        expected_start += run.length;
    }
    assert_eq!(
        expected_start, rle.total_length,
        "run starts must accumulate to total_length at step {step}"
    );
    assert!(
        rle.runs.windows(2).all(|w| w[0].value != w[1].value),
        "adjacent runs share a value at step {step}"
    );
    let mut reference = rle.clone();
    reference.compact();
    assert_eq!(
        rle.runs, reference.runs,
        "local compact_around must match full compact() at step {step}"
    );

    for (idx, expected) in model.iter().copied().enumerate() {
        let index = idx as u32;
        assert_eq!(
            rle.get(index),
            Some(expected),
            "get({index}) mismatch at step {step}"
        );
        let (run_idx, offset_in_run) = rle
            .find_run(index)
            .expect("find_run should succeed for in-bounds index");
        assert!(
            offset_in_run < rle.runs[run_idx].length,
            "offset {} out of bounds for run {} (len {}) at step {}",
            offset_in_run,
            run_idx,
            rle.runs[run_idx].length,
            step
        );
        assert_eq!(
            rle.runs[run_idx].value, expected,
            "run value mismatch for index {index} at step {step}"
        );
    }
    assert_eq!(
        rle.get(model.len() as u32),
        None,
        "out-of-bounds get should return None at step {step}"
    );
}

#[test]
fn rle_set_single_element() {
    for (what, start, index, value, runs) in [
        ("same value keeps one run", &[1u8, 1, 1][..], 1, 1, 1),
        ("middle splits into three", &[1, 1, 1, 1, 1], 2, 9, 3),
        ("start splits into two", &[1, 1, 1], 0, 9, 2),
        ("end splits into two", &[1, 1, 1], 2, 9, 2),
        ("compacts with both neighbours", &[1, 2, 1], 1, 1, 1),
    ] {
        let mut rle = Rle::from_iter(start.iter().copied());
        assert!(rle.set(index, value), "{what}: set in bounds");
        assert_eq!(rle.get(index), Some(value), "{what}");
        assert_eq!(rle.run_count(), runs, "{what}: run count");
        assert_eq!(rle.len() as usize, start.len(), "{what}: length");
    }
}

#[test]
fn local_compaction_observations_refine_the_derived_model() {
    let model = aterm_models_core::rle_local_compact_model();
    for (splice, value) in [
        (None, 2),
        (Some("SpliceSameAsNeighbors"), 1),
        (Some("SpliceDifferent"), 3),
    ] {
        let mut before = model.init_state();
        if let Some(action) = splice {
            assert!(model.fire(action, &mut before));
        }
        let mut rle = Rle::from_iter([0u8, 1, 1, 2, 2, 2, 1, 1, 1, 1, 3]);
        rle.set_range(3, 6, value);
        let crosses =
            |boundary| rle.find_run(boundary - 1).unwrap().0 == rle.find_run(boundary).unwrap().0;
        let mut observed = before.clone();
        observed.insert("left_val", i64::from(rle.get(2).unwrap()));
        observed.insert("mid_val", i64::from(rle.get(3).unwrap()));
        observed.insert("right_val", i64::from(rle.get(6).unwrap()));
        observed.insert("merged_left", i64::from(crosses(3)));
        observed.insert("merged_right", i64::from(crosses(6)));
        observed.insert("compacted", 1);
        assert!(
            model
                .successors("CompactAroundMid", &before)
                .contains(&observed),
            "set_range topology is not the compact transition: {before:?} -> {observed:?}"
        );
        assert!(model.check_invariant("NoAdjacentEqual", &observed));
        assert_eq!((rle.get(0), rle.get(10), rle.len()), (Some(0), Some(3), 11));

        if observed["merged_right"] == 1 {
            let mut omitted_right = observed;
            omitted_right.insert("merged_right", 0);
            assert!(
                !model
                    .successors("CompactAroundMid", &before)
                    .contains(&omitted_right)
            );
            assert!(!model.check_invariant("NoAdjacentEqual", &omitted_right));
            assert!(
                aterm_spec::interp::with_buggy(&model, 1)
                    .successors("CompactAroundMid", &before)
                    .contains(&omitted_right)
            );
        }
    }
}

#[test]
fn rle_set_range_cases() {
    for (what, start, from, to, probes, runs) in [
        (
            "entire",
            &[1u8, 2, 3, 4, 5][..],
            0,
            5,
            &[(0, 9), (4, 9)][..],
            Some(1),
        ),
        (
            "partial",
            &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            2,
            5,
            &[(0, 1), (2, 9), (4, 9), (5, 1)],
            None,
        ),
        (
            "across runs",
            &[1, 1, 2, 2, 3, 3],
            1,
            5,
            &[(0, 1), (1, 9), (4, 9), (5, 3)],
            None,
        ),
        (
            "merges with both neighbours",
            &[9, 9, 2, 2, 9, 9],
            2,
            4,
            &[(2, 9), (3, 9)],
            Some(1),
        ),
        (
            "an end past the length clips",
            &[1, 2, 3],
            1,
            100,
            &[(0, 1), (1, 9), (2, 9)],
            None,
        ),
        ("an empty range", &[1, 2, 3], 1, 1, &[(1, 2)], Some(3)),
        (
            "an inverted range",
            &[1, 2, 3],
            2,
            1,
            &[(1, 2), (2, 3)],
            Some(3),
        ),
        ("an empty sequence", &[], 0, 5, &[], Some(0)),
    ] {
        let mut rle = Rle::from_iter(start.iter().copied());
        rle.set_range(from, to, 9);
        assert_eq!(rle.len() as usize, start.len(), "{what}: length");
        for &(index, value) in probes {
            assert_eq!(rle.get(index), Some(value), "{what}: index {index}");
        }
        if let Some(runs) = runs {
            assert_eq!(rle.run_count(), runs, "{what}: run count");
        }
    }
}

#[test]
fn rle_run_structure_consistent_across_mutations() {
    let mut rle = Rle::from_iter([0u8, 1, 1, 2, 2, 2, 3, 3]);
    let mut model: Vec<u8> = rle.iter().collect();
    assert_rle_matches_model(&rle, &model, 0);

    let mut seed = 0xA5A5_1D3D_u32;

    for step in 1..=512usize {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let value = (seed & 0b11) as u8;

        match seed % 7 {
            0 => {
                rle.push(value);
                model.push(value);
            }
            1 => {
                let count = (seed >> 8) % 4;
                rle.extend_with(value, count);
                for _ in 0..count {
                    model.push(value);
                }
            }
            2 => {
                if !model.is_empty() {
                    let idx = ((seed >> 8) as usize) % model.len();
                    assert!(rle.set(idx as u32, value));
                    model[idx] = value;
                }
            }
            3 => {
                if !model.is_empty() {
                    let a = ((seed >> 4) as usize) % model.len();
                    let b = ((seed >> 12) as usize) % model.len();
                    let (start, end) = if a <= b { (a, b + 1) } else { (b, a + 1) };
                    rle.set_range(start as u32, end as u32, value);
                    for cell in &mut model[start..end] {
                        *cell = value;
                    }
                }
            }
            4 => {
                let new_len = ((seed >> 16) % 64) as usize;
                rle.resize(new_len as u32);
                model.resize(new_len, 0);
            }
            5 => {
                let new_len = ((seed >> 20) % 64) as usize;
                rle.resize_with(new_len as u32, value);
                model.resize(new_len, value);
            }
            _ => {
                rle.clear();
                model.clear();
            }
        }

        assert_rle_matches_model(&rle, &model, step);
    }
}

#[path = "tests_exhaustive.rs"]
mod exhaustive;

#[path = "tests_performance.rs"]
mod performance;

#[path = "tests_saturation.rs"]
mod saturation;

#[test]
fn rle_const_helpers_invariants() {
    use crate::{
        RleCapacityError, Run, checked_rle_extend_total, clamp_rle_extend_count, clamp_rle_range,
        decode_rle_span_triple_le, decode_run_pair_le, encode_rle_span_triple_le,
        encode_run_pair_le, multi_run_edge_lengths, point_split_lengths, remaining_rle_capacity,
        single_run_split_lengths,
    };

    let run = Run::new(7u8, 10);
    assert!(!run.is_empty() && run.contains_offset(9) && !run.contains_offset(10));

    let err = RleCapacityError {
        requested: 100,
        available: 40,
    };
    assert_eq!(err.overflow_amount(), 60);
    assert!(!err.is_full());

    assert_eq!(remaining_rle_capacity(100), u32::MAX - 100);
    assert_eq!(clamp_rle_extend_count(u32::MAX - 5, 20), 5);
    assert_eq!(checked_rle_extend_total(10, 5), Ok(15));
    assert!(checked_rle_extend_total(u32::MAX - 2, 5).is_err());

    assert_eq!(clamp_rle_range(2, 12, 10), Some((2, 10)));
    assert_eq!(clamp_rle_range(10, 12, 10), None);
    assert_eq!(single_run_split_lengths(10, 2, 7), Some((2, 5, 3)));
    assert_eq!(point_split_lengths(10, 3), Some((3, 1, 6)));
    assert_eq!(multi_run_edge_lengths(5, 2, 6, 4), Some((2, 2)));

    let enc = encode_run_pair_le(0x1122_3344, 0x5566_7788);
    assert_eq!(decode_run_pair_le(enc), (0x1122_3344, 0x5566_7788));
    let tri = encode_rle_span_triple_le(10, 20, 30);
    assert_eq!(decode_rle_span_triple_le(tri), (10, 20, 30));
}
