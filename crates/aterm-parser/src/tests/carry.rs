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
        assert_restored_state_is_safe(&successor, &format!("split {split}"));
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

/// Everything a sink saw except the hooked string's own DCS calls (`dcs_hook`,
/// `dcs_put`, `dcs_unhook`), which go to a handler that keeps nothing when the
/// carry is taken: what the screen is made of.
fn outside_the_hook(sink: &RecordingSink) -> String {
    format!(
        "prints={:?} executes={:?} csi={:?} csi_sub={:?} esc={:?} osc={:?} apc=({}, {:?}, {})",
        sink.prints,
        sink.executes,
        sink.csi_dispatches,
        sink.csi_dispatches_with_subparams,
        sink.esc_dispatches,
        sink.osc_dispatches,
        sink.apc_starts,
        sink.apc_data,
        sink.apc_ends
    )
}

/// A parser with C1-control interpretation on or off.
fn parser_c1(c1: bool) -> Parser {
    if c1 {
        Parser::with_c1_controls()
    } else {
        Parser::new()
    }
}

/// `carry_unhooking` carries a hooked DCS string as one to IGNORE: the
/// successor swallows the rest of its payload to the terminator — never
/// printing it, never hooking a handler it did not start — and then reads
/// what follows exactly as the live parser does. That includes a tmux
/// passthrough wrapper's doubled ESC, which breaks out of the string into the
/// inner sequence in both, UTF-8 in the payload (a tmux control-mode line, a
/// kitty command's JSON) and an 8-bit ST, in either C1 mode. Everywhere else it is `carry` itself.
///
/// RED with the first carried form, `DcsIgnore`: the tmux control-mode tail
/// printed `\u{fffd} \u{276f} ls` and `%end` before `after`, the kitty tail
/// `\u{fffd}llo"}`, and the sixel ended by `0x9C` swallowed `after`.
#[test]
fn a_hooked_dcs_carried_unhooking_is_swallowed_to_its_terminator() {
    let cases: [(&[u8], &[u8]); 6] = [
        (b"\x1bPq#0;2;0;0;0", b"#0!10~-\x1b\\after"),
        (b"\x1bPtmu", b"x;\x1b\x1b]2;inner\x07\x1b\\after"),
        // tmux control mode (`DCS 1000 p`): UTF-8 in its lines.
        (
            b"\x1bP1000p%begin 1 1 0\n",
            "%output %1 caf\u{e9} \u{276f} ls\n%end\x1b\\after".as_bytes(),
        ),
        // A kitty remote-control command: UTF-8 in its JSON.
        (
            b"\x1bP@kitty-cmd{\"t\":\"",
            "h\u{e9}llo \u{672c}\"}\x1b\\after".as_bytes(),
        ),
        // A sixel ended by the 8-bit ST.
        (b"\x1bPq#0;2;0;0;0", b"#0!10~-\x9cafter"),
        // An unknown string ended by the 8-bit ST after a character that
        // holds 0x9C itself (U+672C is E6 9C AC): with C1 off that byte is
        // the character's and the string ends at the ST after it; with C1 on
        // it ends the string early, in both.
        (b"\x1bP@x", b"y\xe6\x9c\xac\x9cafter"),
    ];
    for c1 in [false, true] {
        for (head, tail) in cases {
            let mut outgoing = parser_c1(c1);
            outgoing.advance_fast(head, &mut RecordingSink::default());
            assert_eq!(outgoing.state(), State::DcsPassthrough, "{head:?}");
            assert_eq!(outgoing.carry(), None, "control: `carry` holds none");
            let carry = outgoing.carry_unhooking();
            assert_eq!(carry.state, HOOKED_STRING_CARRY, "{head:?}");
            assert_eq!(
                carry,
                ParserCarry {
                    state: HOOKED_STRING_CARRY,
                    ..ParserCarry::default()
                },
                "nothing but the state"
            );
            let mut successor = parser_c1(c1);
            successor.restore_carry(&carry).expect("restores");
            let mut sink = RecordingSink::default();
            successor.advance_fast(tail, &mut sink);
            let mut live = RecordingSink::default();
            outgoing.advance_fast(tail, &mut live);
            assert_eq!(
                outside_the_hook(&sink),
                outside_the_hook(&live),
                "c1={c1} {:?}: the same screen after it",
                String::from_utf8_lossy(head)
            );
            assert!(
                sink.prints.iter().collect::<String>().ends_with("after"),
                "c1={c1} {head:?}: {:?}",
                sink.prints
            );
            assert!(sink.dcs_hooks.is_empty() && sink.dcs_puts.is_empty());
            assert!(sink.apc_starts == 0 && sink.apc_data.is_empty() && sink.apc_ends == 0);
        }
    }
    // The tmux tail's whole text is swallowed, the 8-bit-ST sixel's too: only
    // `after` prints (the live parser's answer, which the loop compares).
    let mut successor = Parser::new();
    successor
        .restore_carry(&ParserCarry {
            state: HOOKED_STRING_CARRY,
            ..ParserCarry::default()
        })
        .expect("restores");
    let mut sink = RecordingSink::default();
    successor.advance_fast(
        "%output %1 caf\u{e9}\n%end\x1b\\after".as_bytes(),
        &mut sink,
    );
    assert_eq!(sink.prints.iter().collect::<String>(), "after");

    let mut ground = Parser::new();
    ground.advance_fast(b"\x1b[1;", &mut RecordingSink::default());
    assert_eq!(Some(ground.carry_unhooking()), ground.carry());
}

/// THE HOOKED STRING'S CARRY READS EVERY BYTE AS THE HOOKED STRING DOES
/// ([`HOOKED_STRING_CARRY`]). The table, entry by entry: for every byte the
/// carried state stays in itself exactly where `DcsPassthrough` stays in
/// itself, and where either leaves, both go to the same state by the same
/// action. Then the parsers: a hooked live parser and a successor restored
/// from its `carry_unhooking`, fed every one- and two-byte tail (65,792 of
/// them), with C1 controls off and on, through both the byte path and the
/// fast path, agree on everything outside the hook and end in corresponding
/// states — and, once out of the string, in the same one.
#[test]
fn the_hooked_string_carry_reads_every_byte_as_the_hooked_string() {
    for byte in 0..=u8::MAX {
        let live = TRANSITIONS[State::DcsPassthrough as usize][usize::from(byte)];
        let carried = TRANSITIONS[HOOKED_STRING_CARRY as usize][usize::from(byte)];
        let stays = |transition: crate::table::Transition, from: State| {
            (transition.next_state == from)
                .then_some(())
                .ok_or((transition.next_state, transition.action as u8))
        };
        assert_eq!(
            stays(live, State::DcsPassthrough),
            stays(carried, HOOKED_STRING_CARRY),
            "byte {byte:#04x}"
        );
    }

    let mut tails: Vec<Vec<u8>> = (0..=u8::MAX).map(|b| vec![b]).collect();
    for a in 0..=u8::MAX {
        for b in 0..=u8::MAX {
            tails.push(vec![a, b]);
        }
    }
    for c1 in [false, true] {
        let mut hooked = parser_c1(c1);
        hooked.advance(b"\x1bP1000p%", &mut RecordingSink::default());
        assert_eq!(hooked.state(), State::DcsPassthrough);
        let mut restored = parser_c1(c1);
        restored
            .restore_carry(&hooked.carry_unhooking())
            .expect("restores");
        for fast in [false, true] {
            for tail in &tails {
                let (mut live, mut successor) = (hooked.clone(), restored.clone());
                let (mut live_sink, mut successor_sink) =
                    (RecordingSink::default(), RecordingSink::default());
                if fast {
                    live.advance_fast(tail, &mut live_sink);
                    successor.advance_fast(tail, &mut successor_sink);
                } else {
                    live.advance(tail, &mut live_sink);
                    successor.advance(tail, &mut successor_sink);
                }
                assert_eq!(
                    outside_the_hook(&successor_sink),
                    outside_the_hook(&live_sink),
                    "c1={c1} fast={fast} tail={tail:02x?}"
                );
                if live.state() == State::DcsPassthrough {
                    assert_eq!(
                        successor.state(),
                        HOOKED_STRING_CARRY,
                        "c1={c1} fast={fast} tail={tail:02x?}: still in the string"
                    );
                } else {
                    assert_eq!(
                        (successor.state(), successor.carry()),
                        (live.state(), live.carry()),
                        "c1={c1} fast={fast} tail={tail:02x?}: out of it alike"
                    );
                }
            }
        }
    }
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
            "a lead past U+10FFFF",
            ParserCarry {
                utf8_tail: vec![0xF5],
                utf8_expected: 4,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "an overlong two-byte lead",
            ParserCarry {
                utf8_tail: vec![0xC0],
                utf8_expected: 2,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "an overlong three-byte prefix",
            ParserCarry {
                utf8_tail: vec![0xE0, 0x9F],
                utf8_expected: 3,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "a surrogate prefix",
            ParserCarry {
                utf8_tail: vec![0xED, 0xA0],
                utf8_expected: 3,
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
        (
            "a prefix past U+10FFFF",
            ParserCarry {
                utf8_tail: vec![0xF4, 0x90],
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
        (
            "a string head outside a string",
            ParserCarry {
                string_utf8_tail: vec![0xE6],
                ..ParserCarry::default()
            },
            CarryRefusal::FieldOutsideState,
        ),
        (
            "an invalid string head",
            ParserCarry {
                state: State::SosPmApcString,
                string_utf8_tail: vec![0xE6, b'a'],
                ..ParserCarry::default()
            },
            CarryRefusal::Utf8Tail,
        ),
    ];
    for (label, carry, refusal) in cases {
        let mut parser = Parser::new();
        parser.advance_fast(b"\x1b[1", &mut RecordingSink::default());
        let before = format!("{parser:?}");
        assert_eq!(parser.restore_carry(&carry), Err(refusal), "{label}");
        assert_eq!(
            format!("{parser:?}"),
            before,
            "{label}: a refusal installs nothing"
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
        // The highest and lowest well-formed prefixes (U+10FFFF's head, the
        // first non-overlong two-byte lead) and the edges of the lead-specific
        // second-byte ranges.
        ParserCarry {
            utf8_tail: vec![0xF4, 0x8F, 0xBF],
            utf8_expected: 4,
            ..ParserCarry::default()
        },
        ParserCarry {
            utf8_tail: vec![0xC2],
            utf8_expected: 2,
            ..ParserCarry::default()
        },
        ParserCarry {
            utf8_tail: vec![0xE0, 0xA0],
            utf8_expected: 3,
            ..ParserCarry::default()
        },
        ParserCarry {
            utf8_tail: vec![0xED, 0x9F],
            utf8_expected: 3,
            ..ParserCarry::default()
        },
        ParserCarry {
            utf8_tail: vec![0xF0, 0x90, 0x80],
            utf8_expected: 4,
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
        ParserCarry {
            state: State::SosPmApcString,
            string_utf8_tail: vec![0xE6, 0x9C],
            ..ParserCarry::default()
        },
        ParserCarry {
            state: State::DcsIgnore,
            string_utf8_tail: vec![0xF4, 0x8F, 0xBF],
            ..ParserCarry::default()
        },
    ];
    for c1 in [false, true] {
        for carry in &accepted {
            let mut parser = parser_c1(c1);
            parser
                .restore_carry(carry)
                .unwrap_or_else(|refusal| panic!("{carry:?}: {refusal}"));
            assert_eq!(
                parser.carry().as_ref(),
                Some(carry),
                "a restored carry reads back"
            );
            assert_restored_state_is_safe(&parser, &format!("c1={c1} {carry:?}"));
        }
    }
}

/// UTF-8 tails to put in a carry. The pending prefixes: every Table 3-7 row's
/// lead alone, then (three- and four-byte rows) with the lowest second byte
/// the row allows, then (four-byte rows) with a third byte. Beside them,
/// tails no stream ends in: a second byte just outside each range-checked
/// row's range (`E0`, `ED`, `F0`, `F4`), an ASCII byte, continuation bytes
/// and the leads that begin no sequence as heads, and a complete character of
/// each length.
fn utf8_tails() -> Vec<Vec<u8>> {
    let mut tails = vec![vec![]];
    for lead in [
        0xC2u8, 0xDF, 0xE0, 0xE1, 0xEC, 0xED, 0xEE, 0xEF, 0xF0, 0xF1, 0xF3, 0xF4,
    ] {
        tails.push(vec![lead]);
        let low = match lead {
            0xE0 => 0xA0,
            0xF0 => 0x90,
            _ => 0x80,
        };
        match lead {
            0xC2..=0xDF => tails.push(vec![lead, low]),
            0xE0..=0xEF => tails.extend([vec![lead, low], vec![lead, low, 0x80]]),
            _ => tails.extend([
                vec![lead, low],
                vec![lead, low, 0x80],
                vec![lead, low, 0x80, 0x80],
            ]),
        }
    }
    tails.extend([
        vec![0xE0, 0x9F],
        vec![0xED, 0xA0],
        vec![0xF0, 0x8F],
        vec![0xF4, 0x90],
    ]);
    for head in [b'a', 0x80, 0xBF, 0xC0, 0xC1, 0xF5, 0xF7, 0xF8] {
        tails.push(vec![head]);
    }
    tails
}

/// THE HOSTILE CARRIES: `restore_carry` either installs a carry that reads
/// back exactly and leaves a safe parser ([`assert_restored_state_is_safe`],
/// in both C1 modes), or refuses it and installs nothing — over carries no
/// parser wrote, at, inside and just past every bound. Two products, each
/// over all 14 states:
///
/// * the sequence fields, with both UTF-8 tails empty: 0, 1, 2, `MAX_PARAMS`
///   and `MAX_PARAMS + 1` parameters (`u16::MAX` and 0 alternating) × a
///   subparameter mask of 0, 1, 2, the bit at the parameter count, or every
///   bit × `current_param` 0, 1 or `u32::MAX` × both `param_started` and both
///   `last_was_colon` × intermediates of every length to
///   `MAX_INTERMEDIATES + 1` in `0x20`, `0x2F`, `0x30`, `0x3C`, `0x3F` and
///   `0x40`, and two mixed × no OSC payload or one byte × both
///   `osc_overflowed`;
/// * the UTF-8 fields, with the sequence fields empty: every
///   [`utf8_tails`] tail × `utf8_expected` 0..=5 × every [`utf8_tails`] tail
///   as `string_utf8_tail`.
///
/// No acceptance check reads a field of both groups, and a state that takes
/// either group's fields refuses the other's, so a carry that mixes them is
/// refused by a check one of the two products already reaches. Every state
/// a carry may name is accepted at least once and every refusal is met, so
/// neither product is vacuous.
#[test]
fn restore_carry_installs_only_safe_carries_whoever_wrote_them() {
    let mut start = Parser::new();
    start.advance_fast(b"\x1b[1", &mut RecordingSink::default());
    let before = format!("{start:?}");
    let mut accepted: Vec<State> = Vec::new();
    let mut refused: Vec<CarryRefusal> = Vec::new();
    let mut judge = |carry: &ParserCarry| {
        let mut parser = start.clone();
        match parser.restore_carry(carry) {
            Ok(()) => {
                if !accepted.contains(&carry.state) {
                    accepted.push(carry.state);
                }
                assert_eq!(parser.carry().as_ref(), Some(carry), "reads back");
                assert_restored_state_is_safe(&parser, &format!("{carry:?}"));
                let mut c1 = parser_c1(true);
                assert_eq!(c1.restore_carry(carry), Ok(()), "{carry:?} with C1 on");
                assert_restored_state_is_safe(&c1, &format!("c1 {carry:?}"));
            }
            Err(refusal) => {
                assert_eq!(
                    format!("{parser:?}"),
                    before,
                    "{carry:?} refused ({refusal}) installed something"
                );
                if !refused.contains(&refusal) {
                    refused.push(refusal);
                }
            }
        }
    };

    let mut intermediates = vec![vec![], vec![0x3F, 0x20], vec![0x20, 0x40]];
    for byte in [0x20, 0x2F, 0x30, 0x3C, 0x3F, 0x40] {
        for len in 1..=MAX_INTERMEDIATES + 1 {
            intermediates.push(vec![byte; len]);
        }
    }
    let tails = utf8_tails();
    for state in State::ALL {
        for len in [0, 1, 2, MAX_PARAMS, MAX_PARAMS + 1] {
            let params: Vec<u16> = (0..len)
                .map(|i| if i % 2 == 0 { u16::MAX } else { 0 })
                .collect();
            for subparam_mask in [0, 1, 2, 1 << len, u32::MAX] {
                for current_param in [0, 1, u32::MAX] {
                    for (param_started, last_was_colon) in
                        [(false, false), (false, true), (true, false), (true, true)]
                    {
                        for intermediates in &intermediates {
                            for osc in [&[][..], b"x"] {
                                for osc_overflowed in [false, true] {
                                    judge(&ParserCarry {
                                        state,
                                        params: params.clone(),
                                        subparam_mask,
                                        current_param,
                                        param_started,
                                        last_was_colon,
                                        intermediates: intermediates.clone(),
                                        osc: osc.to_vec(),
                                        osc_overflowed,
                                        ..ParserCarry::default()
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        for utf8_tail in &tails {
            for utf8_expected in 0..=5 {
                for string_utf8_tail in &tails {
                    judge(&ParserCarry {
                        state,
                        utf8_tail: utf8_tail.clone(),
                        utf8_expected,
                        string_utf8_tail: string_utf8_tail.clone(),
                        ..ParserCarry::default()
                    });
                }
            }
        }
    }

    let carried = [
        State::Ground,
        State::Escape,
        State::EscapeIntermediate,
        State::CsiEntry,
        State::CsiParam,
        State::CsiIntermediate,
        State::CsiIgnore,
        State::DcsIgnore,
        State::OscString,
        State::SosPmApcString,
    ];
    assert_eq!(accepted, carried, "the states a carry restores");
    for refusal in [
        CarryRefusal::State(State::DcsEntry),
        CarryRefusal::State(State::DcsParam),
        CarryRefusal::State(State::DcsIntermediate),
        CarryRefusal::State(State::DcsPassthrough),
        CarryRefusal::Params,
        CarryRefusal::SubparamMask,
        CarryRefusal::CurrentParam,
        CarryRefusal::Intermediates,
        CarryRefusal::Osc,
        CarryRefusal::Utf8Tail,
        CarryRefusal::FieldOutsideState,
    ] {
        assert!(refused.contains(&refusal), "no carry met {refusal:?}");
    }
}

/// What a restored carry may install, whoever wrote it: a state inside the
/// invariant that names no hookable DCS state, arms no DCS or APC consumer,
/// keeps subparameter bits below the parameters, holds a UTF-8 tail only at
/// `Ground` and still incomplete, and a string's UTF-8 head only in a
/// string — and the invariant still holds after every possible next byte.
/// The next byte goes through the fast path, the successor's, and through the
/// byte path too unless a UTF-8 tail is pending: the byte path collects no
/// UTF-8, so only the fast path could have written or can finish one.
fn assert_restored_state_is_safe(parser: &Parser, at: &str) {
    parser.assert_invariants();
    assert!(
        !matches!(
            parser.state(),
            State::DcsEntry | State::DcsParam | State::DcsIntermediate | State::DcsPassthrough
        ),
        "{at}: a hookable DCS state"
    );
    assert!(
        !parser.dcs_active && !parser.apc_active,
        "{at}: a consumer is armed"
    );
    assert_eq!(
        parser.subparam_mask >> parser.params.len(),
        0,
        "{at}: subparameter bits past the parameters"
    );
    assert!(
        parser.utf8_len == 0
            || (parser.state() == State::Ground && parser.utf8_len < parser.utf8_expected),
        "{at}: a UTF-8 tail outside Ground, or a complete one"
    );
    assert!(
        parser.string_utf8 == crate::utf8::Utf8Prefix::NONE
            || matches!(parser.state(), State::DcsIgnore | State::SosPmApcString),
        "{at}: a string's UTF-8 head outside a string"
    );
    for byte in 0..=u8::MAX {
        let mut next = vec![parser.clone()];
        next[0].advance_fast(&[byte], &mut NullSink);
        if parser.utf8_len == 0 {
            let mut slow = parser.clone();
            slow.advance(&[byte], &mut NullSink);
            next.push(slow);
        }
        for next in &next {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| next.assert_invariants()))
                .is_err()
            {
                panic!("{at}: the invariant broke at the next byte {byte:#04x}");
            }
        }
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

/// THE CARRIES THAT ONLY SWALLOW:
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
    let mut big_image = b"\x1b]1337;File=inline=1:".to_vec();
    big_image.extend(std::iter::repeat_n(b'A', OSC_CARRY_CAP + 1));
    // (bytes, swallowed, a picture the next frame replaces)
    for (bytes, swallowed, picture) in [
        (&big_osc[..], true, false),
        (&big_image[..], true, true),
        (&b"\x1bP1;2"[..], true, false),
        (&b"\x1bP"[..], true, false),
        (&b"\x1bP1$"[..], true, false),
        (&b"\x1b_Ga=T,f=100;"[..], true, true),
        (&b"\x1b]1337;File=inline=1:AAAA"[..], false, false),
        (&b"\x1b]0;a short title"[..], false, false),
        (&b"\x1b^a privacy message"[..], false, false),
        (&b"\x1bX a start of string"[..], false, false),
        (&b"\x1b[38;5;19"[..], false, false),
        (&b"\xe6\xbc"[..], false, false),
        (&b"plain text"[..], false, false),
    ] {
        let parser = at(bytes);
        let at = format!(
            "{:?} in {:?}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(24)]),
            parser.state()
        );
        assert_eq!(parser.carry_swallows_sequence(), swallowed, "{at}");
        assert_eq!(parser.carry_swallows_picture(), picture, "{at}");
    }
}

/// A STRING'S CARRY CONTINUES A UTF-8 CHARACTER SPLIT AT THE HANDOFF. Whether
/// a `0x9C` in a DCS or APC payload is ST or the next byte of a character
/// depends on the bytes before it
/// (`tests::c1::a_0x9c_inside_a_utf8_character_does_not_end_the_string`), so
/// a carry taken between the two holds the character's head: split the string
/// at every byte of a payload whose characters hold `0x9C` as their second,
/// third and fourth byte, carry the head, and the successor swallows the rest
/// of the string as the live parser does and prints only what follows it.
#[test]
fn a_string_carry_continues_a_split_utf8_character() {
    let payload = "\u{71c}\u{672c}\u{d71c}\u{1f71c}\u{1c71c}x";
    for head in [&b"\x1bP1000p"[..], b"\x1bP1:", b"\x1b_", b"\x1bX"] {
        for st in [&b"\x1b\\"[..], b"\x9c"] {
            let stream = [head, payload.as_bytes(), st, b"after"].concat();
            for split in head.len()..=head.len() + payload.len() {
                let (before, after) = stream.split_at(split);
                let mut outgoing = Parser::new();
                outgoing.advance_fast(before, &mut RecordingSink::default());
                let carry = outgoing.carry_unhooking();
                let mut successor = Parser::new();
                successor.restore_carry(&carry).expect("restores");
                let mut sink = RecordingSink::default();
                successor.advance_fast(after, &mut sink);
                assert_eq!(
                    sink.prints.iter().collect::<String>(),
                    "after",
                    "{:?} st={st:02x?} split at {}",
                    String::from_utf8_lossy(head),
                    split - head.len()
                );
            }
        }
    }
}
