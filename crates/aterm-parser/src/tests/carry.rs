// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The seamless-update parser carry: `Parser::carry` + `Parser::restore_carry`.

use super::*;

/// Every escape and glyph shape the carry continues exactly: SGR (with
/// subparameters), a private-marker mode set, a charset designation, a CSI
/// with an intermediate, an ignored CSI, OSC titles terminated by BEL and by
/// ST, and two-, three- and four-byte UTF-8.
const STREAM: &[u8] = b"a\x1b[38;5;196mX\x1b[?25l\x1b[4:3mY\x1b(0q\x1b(B\x1b]0;title\x07Z\
\x1b]2;t2\x1b\\\xc3\xa9\xe6\xbc\xa2\xf0\x9f\x98\x80\x1b[1;2 qW\x1b[1<2?mV\x1b7\x1b#8end";

fn feed(parser: &mut Parser, bytes: &[u8], sink: &mut RecordingSink) {
    parser.advance_fast(bytes, sink);
}

/// Split `STREAM` at every byte: the prefix goes to one parser, the carry to
/// a fresh one, the suffix to that — and the actions are exactly the actions
/// of one parser fed the whole stream. The carry at every split restores
/// (the negative control for the refusal table below: real carries pass).
#[test]
fn a_carry_continues_every_split_exactly() {
    let mut whole = RecordingSink::default();
    feed(&mut Parser::new(), STREAM, &mut whole);
    for split in 0..=STREAM.len() {
        let (head, tail) = STREAM.split_at(split);
        let mut sink = RecordingSink::default();
        let mut outgoing = Parser::new();
        feed(&mut outgoing, head, &mut sink);
        let carry = outgoing.carry().expect("no DCS passthrough in the stream");
        let mut successor = Parser::new();
        successor
            .restore_carry(&carry)
            .unwrap_or_else(|refusal| panic!("split {split}: {carry:?} refused: {refusal}"));
        assert_eq!(
            successor.carry().as_ref(),
            Some(&carry),
            "split {split}: carry is a fixed point"
        );
        feed(&mut successor, tail, &mut sink);
        assert_eq!(
            format!("{sink:?}"),
            format!("{whole:?}"),
            "split {split} ({:?} | {:?})",
            String::from_utf8_lossy(head),
            String::from_utf8_lossy(tail)
        );
    }
}

/// The byte-by-byte path (`advance`) carries the same way.
#[test]
fn a_carry_from_the_byte_path_continues_too() {
    let mut whole = RecordingSink::default();
    Parser::new().advance(STREAM, &mut whole);
    for split in 0..=STREAM.len() {
        let (head, tail) = STREAM.split_at(split);
        let mut sink = RecordingSink::default();
        let mut outgoing = Parser::new();
        outgoing.advance(head, &mut sink);
        let mut successor = Parser::new();
        successor
            .restore_carry(&outgoing.carry().expect("carried"))
            .expect("restores");
        successor.advance(tail, &mut sink);
        assert_eq!(format!("{sink:?}"), format!("{whole:?}"), "split {split}");
    }
}

#[test]
fn a_ground_parser_carries_nothing_but_a_split_glyph() {
    let mut parser = Parser::new();
    assert!(parser.carry().expect("ground").is_ground());
    parser.advance_fast(b"x\xe6\xbc", &mut RecordingSink::default());
    assert_eq!(parser.state(), State::Ground);
    let carry = parser.carry().expect("ground");
    assert!(!carry.is_ground(), "the split glyph's head is carried");
    assert_eq!(carry.utf8_tail, vec![0xE6, 0xBC]);
    assert_eq!(carry.utf8_expected, 3);
}

/// A hooked DCS string lives in its handler: nothing to carry.
#[test]
fn a_hooked_dcs_is_not_carried() {
    let mut parser = Parser::new();
    parser.advance_fast(b"\x1bPq#0;2;0;0;0", &mut RecordingSink::default());
    assert_eq!(parser.state(), State::DcsPassthrough);
    assert_eq!(parser.carry(), None);
}

/// An unhooked DCS, and an SOS/PM/APC string, are carried as strings to
/// IGNORE: the rest of the payload is swallowed up to its terminator (never
/// printed, never handed to a consumer the successor did not start), and the
/// text after it prints.
#[test]
fn string_payloads_are_swallowed_to_their_terminator() {
    for (head, tail) in [
        (&b"\x1bP1$"[..], &b"qm\x1b\\after"[..]),
        (
            &b"\x1b_Gf=100,a=T;iVBORw0KGgo"[..],
            &b"AAAANSUhEUg\x1b\\after"[..],
        ),
        (&b"\x1b^private message"[..], &b" more\x1b\\after"[..]),
    ] {
        let mut outgoing = Parser::new();
        outgoing.advance_fast(head, &mut RecordingSink::default());
        let carry = outgoing.carry().expect("carried");
        assert!(
            matches!(carry.state, State::DcsIgnore | State::SosPmApcString),
            "{carry:?}"
        );
        let mut successor = Parser::new();
        successor.restore_carry(&carry).expect("restores");
        let mut sink = RecordingSink::default();
        successor.advance_fast(tail, &mut sink);
        assert_eq!(sink.prints.iter().collect::<String>(), "after", "{head:?}");
        assert!(sink.dcs_hooks.is_empty() && sink.dcs_puts.is_empty());
        assert!(sink.apc_starts == 0 && sink.apc_data.is_empty() && sink.apc_ends == 0);
    }
}

/// An OSC whose head is over `OSC_CARRY_CAP` is carried as one to discard:
/// the successor dispatches nothing for it (a truncated clipboard write or
/// image is worse than none), and the text after its terminator prints.
#[test]
fn an_over_cap_osc_is_discarded_at_its_terminator() {
    let mut head = b"\x1b]52;c;".to_vec();
    head.extend(std::iter::repeat_n(b'A', OSC_CARRY_CAP + 1));
    let mut outgoing = Parser::new();
    outgoing.advance_fast(&head, &mut RecordingSink::default());
    let carry = outgoing.carry().expect("carried");
    assert!(carry.osc_overflowed && carry.osc.is_empty());
    for terminator in [&b"\x07"[..], &b"\x1b\\"[..]] {
        let mut successor = Parser::new();
        successor.restore_carry(&carry).expect("restores");
        let mut sink = RecordingSink::default();
        let mut tail = b"AAAA".to_vec();
        tail.extend_from_slice(terminator);
        tail.extend_from_slice(b"after\x1b]0;next\x07");
        successor.advance_fast(&tail, &mut sink);
        assert_eq!(sink.prints.iter().collect::<String>(), "after");
        assert_eq!(
            sink.osc_dispatches,
            vec![vec![b"0".to_vec(), b"next".to_vec()]],
            "only the NEXT OSC dispatches"
        );
    }
}

/// CAN, SUB and ESC leave a restored state as they leave a live one.
#[test]
fn can_sub_and_esc_still_reset_a_restored_state() {
    let mut outgoing = Parser::new();
    outgoing.advance_fast(b"\x1b[38;5;19", &mut RecordingSink::default());
    let carry = outgoing.carry().expect("carried");
    for (cancel, printed) in [
        (&b"\x18"[..], "6mX"),
        (&b"\x1a"[..], "6mX"),
        (&b"\x1bc"[..], ""),
    ] {
        let mut successor = Parser::new();
        successor.restore_carry(&carry).expect("restores");
        let mut sink = RecordingSink::default();
        successor.advance_fast(cancel, &mut sink);
        if printed == "6mX" {
            successor.advance_fast(b"6mX", &mut sink);
        }
        assert_eq!(sink.prints.iter().collect::<String>(), printed);
        assert!(sink.csi_dispatches.is_empty());
    }
}

/// THE REFUSAL TABLE: every carry `restore_carry` must refuse, each named by
/// the check that refuses it — and the parser is untouched by a refusal.
#[test]
fn restore_carry_refuses_every_invalid_carry() {
    let csi = |params: Vec<u16>| ParserCarry {
        state: State::CsiParam,
        params,
        ..ParserCarry::default()
    };
    let cases: Vec<(&str, ParserCarry, CarryRefusal)> = vec![
        (
            "DcsPassthrough",
            ParserCarry {
                state: State::DcsPassthrough,
                ..ParserCarry::default()
            },
            CarryRefusal::State(State::DcsPassthrough),
        ),
        (
            "DcsEntry",
            ParserCarry {
                state: State::DcsEntry,
                ..ParserCarry::default()
            },
            CarryRefusal::State(State::DcsEntry),
        ),
        (
            "DcsParam",
            ParserCarry {
                state: State::DcsParam,
                ..ParserCarry::default()
            },
            CarryRefusal::State(State::DcsParam),
        ),
        (
            "DcsIntermediate",
            ParserCarry {
                state: State::DcsIntermediate,
                ..ParserCarry::default()
            },
            CarryRefusal::State(State::DcsIntermediate),
        ),
        (
            "too many params",
            csi(vec![1; MAX_PARAMS + 1]),
            CarryRefusal::Params,
        ),
        (
            "mask bit at params.len()",
            ParserCarry {
                subparam_mask: 1 << 2,
                ..csi(vec![1, 2])
            },
            CarryRefusal::SubparamMask,
        ),
        (
            "mask bit 31",
            ParserCarry {
                subparam_mask: 1 << 31,
                ..csi(vec![1; MAX_PARAMS])
            },
            CarryRefusal::SubparamMask,
        ),
        (
            "a mask with no params",
            ParserCarry {
                subparam_mask: 1,
                ..csi(vec![])
            },
            CarryRefusal::SubparamMask,
        ),
        (
            "a value with no digit",
            ParserCarry {
                current_param: 7,
                ..csi(vec![])
            },
            CarryRefusal::CurrentParam,
        ),
        (
            "five intermediates",
            ParserCarry {
                state: State::CsiIntermediate,
                intermediates: vec![0x20; MAX_INTERMEDIATES + 1],
                ..ParserCarry::default()
            },
            CarryRefusal::Intermediates,
        ),
        (
            "an intermediate out of range",
            ParserCarry {
                state: State::EscapeIntermediate,
                intermediates: vec![b'A'],
                ..ParserCarry::default()
            },
            CarryRefusal::Intermediates,
        ),
        (
            "a private marker outside CSI",
            ParserCarry {
                state: State::EscapeIntermediate,
                intermediates: vec![b'?'],
                ..ParserCarry::default()
            },
            CarryRefusal::Intermediates,
        ),
        (
            "EscapeIntermediate with none",
            ParserCarry {
                state: State::EscapeIntermediate,
                ..ParserCarry::default()
            },
            CarryRefusal::Intermediates,
        ),
        (
            "params at Ground",
            ParserCarry {
                params: vec![1],
                ..ParserCarry::default()
            },
            CarryRefusal::FieldOutsideState,
        ),
        (
            "params in CsiEntry",
            ParserCarry {
                state: State::CsiEntry,
                param_started: true,
                ..ParserCarry::default()
            },
            CarryRefusal::FieldOutsideState,
        ),
        (
            "intermediates in Escape",
            ParserCarry {
                state: State::Escape,
                intermediates: vec![0x20],
                ..ParserCarry::default()
            },
            CarryRefusal::FieldOutsideState,
        ),
        (
            "an OSC over the cap",
            ParserCarry {
                state: State::OscString,
                osc: vec![b'x'; OSC_CARRY_CAP + 1],
                ..ParserCarry::default()
            },
            CarryRefusal::Osc,
        ),
        (
            "an overflowed OSC with a payload",
            ParserCarry {
                state: State::OscString,
                osc: vec![b'x'],
                osc_overflowed: true,
                ..ParserCarry::default()
            },
            CarryRefusal::Osc,
        ),
        (
            "an OSC payload in CsiParam",
            ParserCarry {
                osc: vec![b'x'],
                ..csi(vec![])
            },
            CarryRefusal::FieldOutsideState,
        ),
        (
            "a UTF-8 tail outside Ground",
            ParserCarry {
                state: State::Escape,
                utf8_tail: vec![0xE6],
                utf8_expected: 3,
                ..ParserCarry::default()
            },
            CarryRefusal::FieldOutsideState,
        ),
        (
            "a continuation byte as lead",
            ParserCarry {
                utf8_tail: vec![0x80],
                utf8_expected: 2,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "an ASCII lead",
            ParserCarry {
                utf8_tail: vec![b'a'],
                utf8_expected: 1,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "a lead past 0xF7",
            ParserCarry {
                utf8_tail: vec![0xF8],
                utf8_expected: 4,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "a length the lead does not name",
            ParserCarry {
                utf8_tail: vec![0xE6],
                utf8_expected: 4,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "a complete character",
            ParserCarry {
                utf8_tail: vec![0xC3, 0xA9],
                utf8_expected: 2,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "a non-continuation byte",
            ParserCarry {
                utf8_tail: vec![0xE6, b'a'],
                utf8_expected: 3,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "four bytes",
            ParserCarry {
                utf8_tail: vec![0xF0, 0x9F, 0x98, 0x80],
                utf8_expected: 4,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "an expectation with no tail",
            ParserCarry {
                utf8_expected: 2,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
    ];
    for (label, carry, refusal) in cases {
        let mut parser = Parser::new();
        parser.advance_fast(b"\x1b[1", &mut RecordingSink::default());
        let before = parser.carry();
        assert_eq!(parser.restore_carry(&carry), Err(refusal), "{label}");
        assert_eq!(
            parser.carry(),
            before,
            "{label}: a refusal leaves the parser"
        );
        assert!(!refusal.to_string().is_empty());
    }
}

/// The NEGATIVE CONTROLS beside the table: the edge of every bound restores.
#[test]
fn restore_carry_accepts_the_edge_of_every_bound() {
    let accepted = [
        ParserCarry {
            state: State::CsiParam,
            params: vec![u16::MAX; MAX_PARAMS],
            subparam_mask: (1 << MAX_PARAMS) - 1,
            current_param: u32::MAX,
            param_started: true,
            last_was_colon: true,
            intermediates: vec![b'?'],
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::CsiIntermediate,
            intermediates: vec![0x2F; MAX_INTERMEDIATES],
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::OscString,
            osc: vec![b'x'; OSC_CARRY_CAP],
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::OscString,
            osc_overflowed: true,
            ..ParserCarry::default()
        },
        ParserCarry {
            utf8_tail: vec![0xF7, 0xBF, 0xBF],
            utf8_expected: 4,
            ..ParserCarry::default()
        },
        ParserCarry {
            utf8_tail: vec![0xC0],
            utf8_expected: 2,
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::DcsIgnore,
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::SosPmApcString,
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::CsiIgnore,
            ..ParserCarry::default()
        },
    ];
    for carry in accepted {
        let mut parser = Parser::new();
        parser
            .restore_carry(&carry)
            .unwrap_or_else(|refusal| panic!("{carry:?}: {refusal}"));
        assert_eq!(parser.carry(), Some(carry), "a restored carry reads back");
        parser.assert_invariants();
    }
}

#[test]
fn state_names_round_trip() {
    for (index, state) in State::ALL.into_iter().enumerate() {
        assert_eq!(state as usize, index);
        assert_eq!(State::from_name(state.name()), Some(state));
    }
    assert_eq!(State::from_name("Nowhere"), None);
}

/// THE CARRIES THAT ONLY SWALLOW (round six of the update audit, finding 30):
/// an OSC over the carry's cap, an unhooked DCS header and an APC string with
/// its consumer started are carried as sequences to ignore to their
/// terminator, where this parser would have delivered them —
/// `carry_swallows_sequence` names exactly those. What this parser swallows
/// itself (an SOS or PM string, `DcsIgnore`, an OSC past its own cap) and
/// every sequence the carry continues are not named.
#[test]
fn carry_swallows_sequence_names_the_lossy_carries_only() {
    let at = |bytes: &[u8]| {
        let mut parser = Parser::new();
        feed(&mut parser, bytes, &mut RecordingSink::default());
        parser
    };
    let mut big_osc = b"\x1b]52;c;".to_vec();
    big_osc.extend(std::iter::repeat_n(b'A', OSC_CARRY_CAP + 1));
    for (bytes, swallowed) in [
        (&big_osc[..], true),
        (&b"\x1bP1;2"[..], true),
        (&b"\x1bP"[..], true),
        (&b"\x1bP1$"[..], true),
        (&b"\x1b_Ga=T,f=100;"[..], true),
        (&b"\x1b]0;a short title"[..], false),
        (&b"\x1b^a privacy message"[..], false),
        (&b"\x1bX a start of string"[..], false),
        (&b"\x1b[38;5;19"[..], false),
        (&b"\xe6\xbc"[..], false),
        (&b"plain text"[..], false),
    ] {
        let parser = at(bytes);
        assert_eq!(
            parser.carry_swallows_sequence(),
            swallowed,
            "{:?} in {:?}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(24)]),
            parser.state()
        );
    }
}
