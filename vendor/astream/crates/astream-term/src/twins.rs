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

/// `echo_watermark_monotone` as `predict_island.rs` defines it in Clean:
/// `if cur_wm < new_wm then new_wm else cur_wm`.
#[must_use]
pub const fn echo_watermark_monotone(cur_wm: u64, new_wm: u64) -> u64 {
    if cur_wm < new_wm {
        new_wm
    } else {
        cur_wm
    }
}

/// `echo_causal_watermark_select` as `predict_island.rs` defines it in Clean:
/// `if in_off == pred_in_off then next_wm else cur_wm`.
#[must_use]
pub const fn echo_causal_watermark_select(
    in_off: u64,
    pred_in_off: u64,
    next_wm: u64,
    cur_wm: u64,
) -> u64 {
    if in_off == pred_in_off {
        next_wm
    } else {
        cur_wm
    }
}

/// `event_advance_offset_monotone` as `events_island.rs` defines it in Clean:
/// `if cur_offset < next_offset then next_offset else cur_offset`.
#[must_use]
pub const fn event_advance_offset_monotone(cur_offset: u64, next_offset: u64) -> u64 {
    if cur_offset < next_offset {
        next_offset
    } else {
        cur_offset
    }
}

/// `op_clamp_scroll_top` as `ops_island.rs` defines it in Clean:
/// `if top < bottom then top else bottom`.
#[must_use]
pub const fn op_clamp_scroll_top(top: u64, bottom: u64) -> u64 {
    if top < bottom {
        top
    } else {
        bottom
    }
}

/// `perceive_clamp_take_count` as `perceive_island.rs` defines it in Clean:
/// `if requested < total then requested else total`.
#[must_use]
pub const fn perceive_clamp_take_count(requested: u64, total: u64) -> u64 {
    if requested < total {
        requested
    } else {
        total
    }
}

/// `record_client_seq_advance` as `record_island.rs` defines it in Clean:
/// `if cur_seq < proposed_seq then proposed_seq else cur_seq`.
#[must_use]
pub const fn record_client_seq_advance(cur_seq: u64, proposed_seq: u64) -> u64 {
    if cur_seq < proposed_seq {
        proposed_seq
    } else {
        cur_seq
    }
}

/// `render_clamp_end_offset` as `render_island.rs` defines it in Clean:
/// `if to_off < max_off then to_off else max_off`.
#[must_use]
pub const fn render_clamp_end_offset(to_off: u64, max_off: u64) -> u64 {
    if to_off < max_off {
        to_off
    } else {
        max_off
    }
}

/// `screen_clamp_dim` as `screen_island.rs` defines it in Clean:
/// `if dim < max_dim then dim else max_dim`.
#[must_use]
pub const fn screen_clamp_dim(dim: u64, max_dim: u64) -> u64 {
    if dim < max_dim {
        dim
    } else {
        max_dim
    }
}

#[cfg(test)]
#[path = "../../astream-wire/twin_answers.rs"]
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
            answers(|[a, b]| crate::predict::echo_watermark_monotone(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::echo_watermark_monotone(a, b)), want);
        let want =
            "000111222012012012012012012012012012000111222012012012012012012012012012000111222";
        assert_eq!(
            answers(|[a, b, c, d]| crate::predict::echo_causal_watermark_select(a, b, c, d)),
            want
        );
        assert_eq!(
            answers(|[a, b, c, d]| super::echo_causal_watermark_select(a, b, c, d)),
            want
        );
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::events::event_advance_offset_monotone(a, b)),
            want
        );
        assert_eq!(
            answers(|[a, b]| super::event_advance_offset_monotone(a, b)),
            want
        );
        let want = "000011012";
        assert_eq!(
            answers(|[a, b]| crate::ops::op_clamp_scroll_top(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::op_clamp_scroll_top(a, b)), want);
        let want = "000011012";
        assert_eq!(
            answers(|[a, b]| crate::perceive::perceive_clamp_take_count(a, b)),
            want
        );
        assert_eq!(
            answers(|[a, b]| super::perceive_clamp_take_count(a, b)),
            want
        );
        let want = "012112222";
        assert_eq!(
            answers(|[a, b]| crate::record::record_client_seq_advance(a, b)),
            want
        );
        assert_eq!(
            answers(|[a, b]| super::record_client_seq_advance(a, b)),
            want
        );
        let want = "000011012";
        assert_eq!(
            answers(|[a, b]| crate::render::render_clamp_end_offset(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::render_clamp_end_offset(a, b)), want);
        let want = "000011012";
        assert_eq!(
            answers(|[a, b]| crate::screen::screen_clamp_dim(a, b)),
            want
        );
        assert_eq!(answers(|[a, b]| super::screen_clamp_dim(a, b)), want);
    }

    // Not vacuous: `echo_watermark_monotone` with its branches swapped answers otherwise.
    #[test]
    fn a_twin_with_its_branches_swapped_answers_otherwise() {
        assert_ne!(answers(|[a, b]| if a < b { a } else { b }), "012112222");
    }
}
