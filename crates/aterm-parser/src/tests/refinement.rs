// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Refinement mapping: the concrete `Parser` projected onto `ParserModel`,
//! the VT state machine's variables (state, parameter and intermediate
//! counts, the parameter accumulator). UTF-8 decoding is the layer above the
//! state machine and is not modelled.

use aterm_spec::Refines;

use crate::{Parser, State};

/// The abstract parser state. The UTF-8 buffer, OSC data and APC tracking
/// are not part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParserModel {
    pub state: State,
    /// Accumulated CSI/DCS parameters.
    pub param_count: usize,
    pub intermediate_count: usize,
    /// The parameter being accumulated.
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
    // After dispatch the finalized param remains until the next sequence
    // clears the buffer on entry.
    assert_eq!(model.param_count, 1);

    parser.advance(b"\x1b[", &mut sink);
    let model = parser.project();
    assert!(model.state.is_csi());
    assert_eq!(model.param_count, 0, "Clear action resets params on entry");
}
