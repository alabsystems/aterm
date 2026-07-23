// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

fn assert_utf8_decoder_consistent(parser: &Parser) {
    kani::assert(
        (parser.state as usize) < State::COUNT,
        "UTF-8 decoder: state must remain a valid parser state",
    );
    kani::assert(
        parser.utf8_len <= 4,
        "UTF-8 decoder: utf8_len must stay <= 4",
    );
    kani::assert(
        parser.utf8_expected <= 4,
        "UTF-8 decoder: utf8_expected must stay <= 4",
    );
    kani::assert(
        parser.utf8_len <= parser.utf8_expected,
        "UTF-8 decoder: utf8_len must not exceed utf8_expected",
    );

    if parser.utf8_len == 0 {
        kani::assert(
            parser.utf8_expected == 0,
            "UTF-8 decoder: empty buffer must not expect trailing bytes",
        );
    } else {
        kani::assert(
            parser.state == State::Ground,
            "UTF-8 decoder: partial sequence must stay in Ground state",
        );
        kani::assert(
            parser.utf8_expected >= 2 && parser.utf8_expected <= 4,
            "UTF-8 decoder: active sequence must expect 2..=4 bytes",
        );
        kani::assert(
            (0xC0..=0xF7).contains(&parser.utf8_buffer[0]),
            "UTF-8 decoder: active buffer must start with a lead byte",
        );
    }

    if parser.state != State::Ground {
        kani::assert(
            parser.utf8_len == 0 && parser.utf8_expected == 0,
            "UTF-8 decoder: non-Ground parser states must not retain partial UTF-8 bytes",
        );
    }
}

/// Proof Gap 19: UTF-8 continuation bytes don't corrupt state.
///
/// Verifies that malformed UTF-8 (continuation without lead) doesn't
/// cause state corruption.
#[kani::proof]
fn utf8_continuation_safe() {
    let mut parser = Parser::new();
    let mut sink = NullSink;

    // Send a UTF-8 continuation byte without a lead byte
    let cont: u8 = kani::any();
    kani::assume(cont >= 0x80 && cont <= 0xBF);

    parser.advance(&[cont], &mut sink);

    // State must remain valid
    kani::assert(
        (parser.state as u8) < State::COUNT as u8,
        "state must be valid after orphan continuation",
    );
}

/// Proof Gap 19: A UTF-8 lead followed by arbitrary bytes preserves decoder invariants.
///
/// This covers valid continuations, malformed interruptions, and replay of
/// ASCII/control bytes through the normal parser state machine.
// TODO(#7932): non-substantive classification [type_construction] Body only constructs a value; no behavioral assertion on it.
#[kani::proof]
#[kani::unwind(8)]
fn utf8_malformed_sequences_preserve_decoder_invariants() {
    let mut parser = Parser::new();
    let mut sink = NullSink;

    let lead: u8 = kani::any();
    kani::assume((0xC0..=0xF7).contains(&lead));

    parser.advance_fast(&[lead], &mut sink);
    assert_utf8_decoder_consistent(&parser);

    let follow_up: [u8; 3] = kani::any();
    for byte in follow_up {
        parser.advance_fast(&[byte], &mut sink);
        assert_utf8_decoder_consistent(&parser);
    }
}
