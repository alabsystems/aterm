// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Plain-Rust twins of the functions this crate's Clean islands compile.
//!
//! An island (`*_island.rs`) mounts only under `cfg(clean_islands)`, which build.rs
//! sets when the compiler answers that it is Trust: no stock rustc can lex Clean. Under
//! Trust, each function an island compiles from Clean (`clean_origin_mir` I5) is the one
//! Rust calls; under every other compiler it is its twin below, written from the same
//! definition. The tests hold the twins, and whichever function a build calls, to the
//! answers the island gives, which every Trust test run re-checks against the island.

/// `heal_frontier_monotone` as `cut_island.rs` defines it in Clean:
/// `if cur < req then req else cur`.
#[must_use]
pub const fn heal_frontier_monotone(cur: u64, req: u64) -> u64 {
    if cur < req {
        req
    } else {
        cur
    }
}

/// `meet_frontier_monotone` as `cut_island.rs` defines it in Clean:
/// `if a < b then a else b`.
#[must_use]
pub const fn meet_frontier_monotone(a: u64, b: u64) -> u64 {
    if a < b {
        a
    } else {
        b
    }
}

/// `causal_gate_frontier` as `cut_island.rs` defines it in Clean:
/// `if cause_req <= cause_cut then eff_healed else eff_cur`.
#[must_use]
pub const fn causal_gate_frontier(
    cause_req: u64,
    cause_cut: u64,
    eff_healed: u64,
    eff_cur: u64,
) -> u64 {
    if cause_req <= cause_cut {
        eff_healed
    } else {
        eff_cur
    }
}

/// `frame_clamp_payload_len` as `frame_island.rs` defines it in Clean:
/// `if len < cap then len else cap`.
#[must_use]
pub const fn frame_clamp_payload_len(len: u64, cap: u64) -> u64 {
    if len < cap {
        len
    } else {
        cap
    }
}

/// `crc_table_index_clamp` as `hash_island.rs` defines it in Clean:
/// `if idx < max_idx then idx else max_idx`.
#[must_use]
pub const fn crc_table_index_clamp(idx: u64, max_idx: u64) -> u64 {
    if idx < max_idx {
        idx
    } else {
        max_idx
    }
}

/// `offset_advance_monotone` as `offset_island.rs` defines it in Clean:
/// `if current < candidate then candidate else current`.
#[must_use]
pub const fn offset_advance_monotone(current: u64, candidate: u64) -> u64 {
    if current < candidate {
        candidate
    } else {
        current
    }
}

/// `partition_modulus_clamp` as `partition_island.rs` defines it in Clean:
/// `if floor < num_partitions then num_partitions else floor`.
#[must_use]
pub const fn partition_modulus_clamp(num_partitions: u64, floor: u64) -> u64 {
    if floor < num_partitions {
        num_partitions
    } else {
        floor
    }
}

/// `monotone_clock_tick` as `receipt_island.rs` defines it in Clean:
/// `if now_ms < sample_ms then sample_ms else now_ms`.
#[must_use]
pub const fn monotone_clock_tick(now_ms: u64, sample_ms: u64) -> u64 {
    if now_ms < sample_ms {
        sample_ms
    } else {
        now_ms
    }
}

/// `step_prefix_advance` as `steps_island.rs` defines it in Clean:
/// `if valid_len < next_pos then next_pos else valid_len`.
#[must_use]
pub const fn step_prefix_advance(valid_len: u64, next_pos: u64) -> u64 {
    if valid_len < next_pos {
        next_pos
    } else {
        valid_len
    }
}

/// `segment_depth_advance` as `subject_island.rs` defines it in Clean:
/// `if matched_depth < candidate_depth then candidate_depth else matched_depth`.
#[must_use]
pub const fn segment_depth_advance(matched_depth: u64, candidate_depth: u64) -> u64 {
    if matched_depth < candidate_depth {
        candidate_depth
    } else {
        matched_depth
    }
}

#[cfg(test)]
#[path = "../twin_answers.rs"]
mod twin_answers;

#[cfg(test)]
mod tests {
    use super::twin_answers::answers;

    // Each function's answers over every operand tuple of `EDGES` (`twin_answers.rs`), as
    // indices into it. `crate::...` is the island's function under Trust and the twin
    // elsewhere, so every build holds the twin to the island's answers.
    #[test]
    fn the_twins_answer_as_the_islands_do() {
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::cut::heal_frontier_monotone(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::heal_frontier_monotone(a, b)), want);
        let want = "000011012";
        assert_eq!(
            answers(|[a, b]| crate::cut::meet_frontier_monotone(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::meet_frontier_monotone(a, b)), want);
        let want =
            "000111222000111222000111222012012012000111222000111222012012012012012012000111222";
        assert_eq!(
            answers(|[a, b, c, d]| crate::cut::causal_gate_frontier(a, b, c, d)),
            want
        );
        assert_eq!(
            answers(|[a, b, c, d]| super::causal_gate_frontier(a, b, c, d)),
            want
        );
        let want = "000011012";
        assert_eq!(
            answers(|[a, b]| crate::frame::frame_clamp_payload_len(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::frame_clamp_payload_len(a, b)), want);
        let want = "000011012";
        assert_eq!(
            answers(|[a, b]| crate::hash::crc_table_index_clamp(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::crc_table_index_clamp(a, b)), want);
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::offset::offset_advance_monotone(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::offset_advance_monotone(a, b)), want);
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::partition::partition_modulus_clamp(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::partition_modulus_clamp(a, b)), want);
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::receipt::monotone_clock_tick(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::monotone_clock_tick(a, b)), want);
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::steps::step_prefix_advance(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::step_prefix_advance(a, b)), want);
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::subject::segment_depth_advance(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::segment_depth_advance(a, b)), want);
    }

    // Not vacuous: `heal_frontier_monotone` with its branches swapped answers otherwise.
    #[test]
    fn a_twin_with_its_branches_swapped_answers_otherwise() {
        assert_ne!(answers(|[a, b]| if a < b { a } else { b }), "012112222");
    }
}
