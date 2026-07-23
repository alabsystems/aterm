// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Parser TLA+ refinement mapping.
//!
//! Projects the concrete `Parser` state to the abstract `ParserModel` that
//! corresponds to the TLA+ `Parser.tla` specification variables:
//!   - `state`      ↦ one of 14 named parser states
//!   - `params`     ↦ bounded parameter vector (max 16)
//!   - `intermediates` ↦ bounded intermediate vector (max 4)
//!   - `currentParam`  ↦ current parameter accumulator
//!
//! UTF-8 decoding state is intentionally excluded — the TLA+ spec models
//! the VT parser state machine, not the encoding layer above it.
//! See `Parser.tla` DESIGN NOTE on FV-11.

use aterm_spec::Refines;

use crate::{Parser, State};

/// Abstract parser model corresponding to `Parser.tla` variables.
///
/// This captures only the state variables that appear in the TLA+ spec.
/// Implementation details (UTF-8 buffer, osc_data, APC tracking) are
/// excluded because the TLA+ spec does not model them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserModel {
    /// Current parser state (one of 14 states in the TLA+ `States` set).
    pub state: State,
    /// Number of accumulated CSI/DCS parameters (TLA+ `params` sequence length).
    pub param_count: usize,
    /// Number of intermediate bytes (TLA+ `intermediates` sequence length).
    pub intermediate_count: usize,
    /// Current parameter accumulator value (TLA+ `currentParam`).
    pub current_param: u32,
}

impl Refines<ParserModel> for Parser {
    fn project(&self) -> ParserModel {
        ParserModel {
            state: self.state,
            param_count: self.params.len(),
            intermediate_count: self.intermediates.len(),
            current_param: self.current_param,
        }
    }
}

#[test]
fn test_initial_state_projects_to_ground() {
    let parser = Parser::new();
    let model = parser.project();
    assert_eq!(model.state, State::Ground);
    assert_eq!(model.param_count, 0);
    assert_eq!(model.intermediate_count, 0);
    assert_eq!(model.current_param, 0);
}

#[test]
fn test_csi_entry_projects_state_and_params() {
    use crate::NullSink;

    let mut parser = Parser::new();
    let mut sink = NullSink;

    // Feed ESC [ to enter CsiEntry, then "1;2" to accumulate params
    parser.advance(b"\x1b[1;2", &mut sink);

    let model = parser.project();
    assert!(
        model.state.is_csi(),
        "expected CSI state, got {:?}",
        model.state
    );
    assert_eq!(model.param_count, 1, "semicolon finalizes first param");
    // current_param holds the in-progress "2"
    assert_eq!(model.current_param, 2);
}

#[test]
fn test_ground_after_complete_sequence() {
    use crate::NullSink;

    let mut parser = Parser::new();
    let mut sink = NullSink;

    // Complete CSI sequence: ESC [ 1 m (SGR)
    parser.advance(b"\x1b[1m", &mut sink);

    let model = parser.project();
    assert_eq!(model.state, State::Ground);
    // After CSI dispatch the finalized param remains until the next
    // sequence clears the buffer (TLA+ Clear action fires on entry).
    assert_eq!(model.param_count, 1);

    // Start a new sequence to observe the Clear action
    parser.advance(b"\x1b[", &mut sink);
    let model = parser.project();
    assert!(model.state.is_csi());
    assert_eq!(model.param_count, 0, "Clear action resets params on entry");
}
