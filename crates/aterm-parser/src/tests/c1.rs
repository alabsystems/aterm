// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! C1 controls: 8-bit introducers and ST, off by default, and 0x9C in string
//! payloads.

use super::super::*;
use super::RecordingSink;

#[test]
fn parse_c1_csi() {
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // 8-bit CSI (0x9B) followed by params
    parser.advance_fast(b"\x9b31m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 1);
    assert_eq!(sink.csi_dispatches[0].0, vec![31]);
}

#[test]
fn parse_c1_osc() {
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // 8-bit OSC (0x9D) followed by data
    parser.advance_fast(b"\x9d0;Title\x07", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"Title".to_vec()],
        "C1 OSC introducer (0x9D) should produce same parsed payload as ESC ]"
    );
}

#[test]
fn osc_payload_preserves_non_st_c1_bytes() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();
    let payload: Vec<u8> = (0x80..=0x9B).chain(0x9D..=0x9F).collect();
    let mut input = vec![0x1B, b']', b'0', b';'];
    input.extend_from_slice(&payload);
    input.push(0x07);

    parser.advance_fast(&input, &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), payload],
        "OSC should keep non-ST C1 bytes as payload data"
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn osc_string_c1_st_byte_is_data_not_terminator() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x9C is treated as data in OscString state (not ST terminator).
    // Required for UTF-8 correctness: 0x9C is a valid continuation byte
    // in CJK characters (e.g., 本 = E6 9C AC). Terminate with BEL. (#3745)
    parser.advance_fast(b"\x1b]0;Title\x9cX\x07", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    let expected_payload: Vec<u8> = b"Title\x9cX".to_vec();
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), expected_payload],
        "0x9C should be accumulated as data, not treated as ST terminator"
    );
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn c1_disabled_by_default() {
    // C1 controls are off by default: 0x9B is U+FFFD, then "31m" prints.
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 8-bit CSI (0x9B) should NOT be interpreted as CSI when C1 disabled
    // Instead: 0x9B -> replacement char, "31m" -> printed as regular chars
    parser.advance_fast(b"\x9b31m", &mut sink);

    assert_eq!(sink.csi_dispatches.len(), 0);
    assert_eq!(sink.prints.len(), 4);
    assert_eq!(sink.prints[0], char::REPLACEMENT_CHARACTER);
    assert_eq!(sink.prints[1], '3');
    assert_eq!(sink.prints[2], '1');
    assert_eq!(sink.prints[3], 'm');
}

#[test]
fn dcs_passthrough_preserves_non_st_high_bytes() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Every high byte but 0x9C (ST).
    let payload: Vec<u8> = (0x80..=0x9Bu8).chain(0x9D..=0xFFu8).collect();
    let mut input = vec![0x1B, b'P', b'@']; // DCS hook
    input.extend_from_slice(&payload);
    input.extend_from_slice(b"\x1b\\"); // ST

    parser.advance_fast(&input, &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1, "DCS hook should fire once");
    assert_eq!(
        sink.dcs_puts, payload,
        "DCS passthrough should keep all non-ST high bytes as payload data"
    );
    assert_eq!(sink.dcs_unhooks, 1, "DCS unhook should fire once");
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn dcs_passthrough_st_c1_byte_terminates_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x9C ends a DCS as ST.
    let mut input = vec![0x1B, b'P', b'@']; // DCS hook
    input.extend_from_slice(b"data");
    input.push(0x9C); // ST via C1
    input.push(b'X'); // should be printed as regular char

    parser.advance_fast(&input, &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1);
    assert_eq!(sink.dcs_puts, b"data".to_vec());
    assert_eq!(sink.prints, vec!['X']);
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn dcs_passthrough_st_c1_byte_terminates_sequence_advance_fast() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    let mut input = vec![0x1B, b'P', b'@']; // DCS hook
    input.extend_from_slice(b"data");
    input.push(0x9C); // ST via C1
    input.extend_from_slice(b"VISIBLE");

    parser.advance_fast(&input, &mut sink);

    assert_eq!(sink.dcs_hooks.len(), 1);
    assert_eq!(sink.dcs_puts, b"data".to_vec());
    assert_eq!(sink.dcs_unhooks, 1, "advance_fast must unhook on 0x9C");
    assert_eq!(sink.prints, "VISIBLE".chars().collect::<Vec<_>>());
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn apc_payload_preserves_non_st_c1_bytes() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Every high byte but 0x9C (ST).
    let payload: Vec<u8> = (0x80..=0x9Bu8).chain(0x9D..=0xFFu8).collect();
    let mut input = vec![0x1B, b'_']; // APC start
    input.extend_from_slice(&payload);
    input.extend_from_slice(b"\x1b\\"); // ST

    parser.advance_fast(&input, &mut sink);

    assert_eq!(sink.apc_starts, 1, "APC start should fire once");
    assert_eq!(
        sink.apc_data, payload,
        "APC should keep all non-ST high bytes as payload data"
    );
    assert_eq!(sink.apc_ends, 1, "APC end should fire once");
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn apc_st_c1_byte_terminates_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // 0x9C ends an APC as ST.
    let mut input = vec![0x1B, b'_']; // APC start
    input.extend_from_slice(b"data");
    input.push(0x9C); // ST via C1
    input.push(b'X'); // should be printed as regular char

    parser.advance_fast(&input, &mut sink);

    assert_eq!(sink.apc_starts, 1);
    assert_eq!(sink.apc_data, b"data".to_vec());
    assert_eq!(sink.prints, vec!['X']);
    assert_eq!(parser.state(), State::Ground);
}

#[test]
fn parse_c1_osc_with_c1_st() {
    // With C1 controls on, 0x9C ends an OSC as ST.
    let mut parser = Parser::with_c1_controls();
    let mut sink = RecordingSink::default();

    // C1 OSC (0x9D) + data + C1 ST (0x9C)
    parser.advance_fast(b"\x9d0;Title\x9c", &mut sink);

    assert_eq!(sink.osc_dispatches.len(), 1);
    assert_eq!(
        sink.osc_dispatches[0],
        vec![b"0".to_vec(), b"Title".to_vec()],
        "C1 OSC with C1 ST terminator should parse correctly when C1 enabled"
    );
    assert_eq!(parser.state(), State::Ground);
}

/// Regression test for #7556: C1 control bytes must not introduce new
/// sequences when C1 controls are disabled and the parser is mid-sequence.
///
/// Before the fix, 0x9B (C1 CSI) in CsiParam state would transition to
/// CsiEntry, effectively injecting a new CSI sequence from within an
/// existing one. This could be exploited by a malicious remote server.
#[test]
fn c1_disabled_blocks_injection_in_csi_param_state() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start a CSI sequence, then inject C1 CSI (0x9B) mid-params.
    // Without the fix, 0x9B would start a new CSI sequence.
    // With the fix, 0x9B is silently dropped.
    //
    // "\x1b[1" puts parser in CsiParam state, then 0x9B arrives.
    // After 0x9B is dropped, "31m" continues as params+final for the
    // original CSI, producing CSI 1;31 m (a valid SGR sequence with
    // params adjusted by the state machine).
    parser.advance_fast(b"\x1b[1\x9b31m", &mut sink);

    assert_eq!(
        sink.csi_dispatches.len(),
        1,
        "C1 CSI (0x9B) in CsiParam state should be dropped when C1 disabled"
    );
}

/// With C1 controls off, 0x9B in `Escape` is dropped.
#[test]
fn c1_disabled_blocks_injection_in_escape_state() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Put parser into Escape state with ESC, then send 0x9B.
    // Without fix: 0x9B transitions to CsiEntry (new sequence).
    // With fix: 0x9B is dropped in Escape state.
    parser.advance_fast(b"\x1b\x9b31m", &mut sink);

    assert_eq!(
        sink.csi_dispatches.len(),
        0,
        "C1 CSI (0x9B) in Escape state should be dropped when C1 disabled"
    );
}

/// With C1 controls off, 0x90 (DCS) and 0x9D (OSC) are dropped mid-CSI too.
#[test]
fn c1_disabled_blocks_dcs_osc_injection_mid_sequence() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();

    // Start a CSI, inject C1 DCS (0x90) and C1 OSC (0x9D).
    // Both should be dropped, not start new DCS/OSC sequences.
    parser.advance_fast(b"\x1b[1\x90q", &mut sink);
    assert_eq!(
        sink.dcs_hooks.len(),
        0,
        "C1 DCS (0x90) mid-CSI should be dropped when C1 disabled"
    );

    parser.advance_fast(b"\x1b[1\x9d0;Title\x07", &mut sink);
    assert_eq!(
        sink.osc_dispatches.len(),
        0,
        "C1 OSC (0x9D) mid-CSI should be dropped when C1 disabled"
    );
}

/// AN IGNORED DCS STRING READS EVERY BYTE AS A HOOKED ONE DOES. `DcsIgnore`
/// (a DCS whose header is malformed: a `:` parameter, a private marker after a
/// parameter, a parameter byte after an intermediate) is a control string like
/// `DcsPassthrough`; the DEC parser ignores everything in it up to ST, with
/// GR bytes (`0xA0..=0xFF`, every UTF-8 lead and most continuations) read as
/// their GL equivalents. A UTF-8 byte in the payload must not leave the string
/// (`ESC P 1 : h é llo ❯ ls ESC \ after` must print only `after`), and with C1
/// controls off `0x9C` must still end it as it ends a hooked DCS.
///
/// The table, entry by entry: the ignored string stays in itself exactly where
/// the hooked one does, and where either leaves, both go to the same state.
/// Then the parsers, fed every one- and two-byte tail, C1 off and on, agree on
/// everything outside the hook.
#[test]
fn an_ignored_dcs_reads_every_byte_as_a_hooked_one() {
    for byte in 0..=u8::MAX {
        let hooked = TRANSITIONS[State::DcsPassthrough as usize][usize::from(byte)];
        let ignored = TRANSITIONS[State::DcsIgnore as usize][usize::from(byte)];
        let leaves = |transition: crate::table::Transition, from: State| {
            (transition.next_state != from).then_some(transition.next_state)
        };
        assert_eq!(
            leaves(hooked, State::DcsPassthrough),
            leaves(ignored, State::DcsIgnore),
            "byte {byte:#04x}"
        );
    }

    let outside_the_hook = |sink: &RecordingSink| {
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
    };
    let mut tails: Vec<Vec<u8>> = (0..=u8::MAX).map(|b| vec![b]).collect();
    for a in 0..=u8::MAX {
        for b in 0..=u8::MAX {
            tails.push(vec![a, b]);
        }
    }
    for c1 in [false, true] {
        let fresh = || {
            if c1 {
                Parser::with_c1_controls()
            } else {
                Parser::new()
            }
        };
        let mut hooked = fresh();
        hooked.advance_fast(b"\x1bP1000p%", &mut RecordingSink::default());
        assert_eq!(hooked.state(), State::DcsPassthrough);
        let mut ignored = fresh();
        ignored.advance_fast(b"\x1bP1:%", &mut RecordingSink::default());
        assert_eq!(ignored.state(), State::DcsIgnore);
        for tail in &tails {
            let (mut live, mut dropped) = (hooked.clone(), ignored.clone());
            let (mut live_sink, mut dropped_sink) =
                (RecordingSink::default(), RecordingSink::default());
            live.advance_fast(tail, &mut live_sink);
            dropped.advance_fast(tail, &mut dropped_sink);
            assert_eq!(
                outside_the_hook(&dropped_sink),
                outside_the_hook(&live_sink),
                "c1={c1} tail={tail:02x?}"
            );
            assert_eq!(
                dropped.state() == State::DcsIgnore,
                live.state() == State::DcsPassthrough,
                "c1={c1} tail={tail:02x?}: in the string alike"
            );
        }
    }

    // End to end: a UTF-8 payload, then an 8-bit ST.
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();
    parser.advance_fast(
        "\x1bP1:h\u{e9}llo \u{276f} ls\x1b\\after".as_bytes(),
        &mut sink,
    );
    parser.advance_fast(b"\x1bP1:x\x9cafter", &mut sink);
    assert_eq!(sink.prints.iter().collect::<String>(), "afterafter");
}

/// A `0x9C` INSIDE A UTF-8 CHARACTER DOES NOT END THE STRING. With C1 controls
/// off, a DCS (hooked or ignored) and an SOS, PM or APC string end at the 8-bit
/// ST `0x9C` — but `0x9C` is also a UTF-8 continuation byte, about one in 64 of
/// them: U+672C 本 is `E6 9C AC`. A payload carrying such a character ended
/// inside it, and the rest of the payload printed and ran as terminal input:
/// `ESC P 1 : 本 x ESC \ after` printed `\u{fffd}xafter`, and a tmux
/// control-mode or passthrough line, a kitty command's JSON or an APC payload
/// did the same. A `0x9C` that extends the well-formed UTF-8 prefix the payload
/// ends in (Unicode Table 3-7, the Ground decoder's own predicate) is that
/// character's byte; anywhere else it is ST — after ASCII, after a whole
/// character, after a lead it cannot follow (`E0`, `F4`), after a broken
/// prefix. With C1 controls on, a raw `0x9C` is the 8-bit ST wherever it falls.
///
/// Every string state, whole and one byte per read.
#[test]
fn a_0x9c_inside_a_utf8_character_does_not_end_the_string() {
    // 0x9C as the second, third and fourth byte, after the leads whose second
    // byte may be 0x9C (`DC`, `E6`, `ED`, `F0`).
    let payload = "\u{71c}\u{672c}\u{d71c}\u{1f71c}\u{1c71c} x \u{672c}\u{672c}";
    assert_eq!(payload.bytes().filter(|&b| b == 0x9C).count(), 11);
    let heads: [(&[u8], &str); 5] = [
        (b"\x1bP1000p", "hooked DCS"),
        (b"\x1bP1:", "ignored DCS"),
        (b"\x1b_", "APC"),
        (b"\x1bX", "SOS"),
        (b"\x1b^", "PM"),
    ];
    let feeds: [(&str, fn(&mut Parser, &[u8], &mut RecordingSink)); 2] = [
        ("advance_fast", |p, b, s| p.advance_fast(b, s)),
        ("one byte per read", |p, b, s| {
            for byte in b {
                p.advance_fast(std::slice::from_ref(byte), s);
            }
        }),
    ];
    for (head, name) in heads {
        for st in [&b"\x1b\\"[..], &b"\x9c"[..]] {
            let input = [head, payload.as_bytes(), st, b"after"].concat();
            for (how, feed) in feeds {
                let mut parser = Parser::new();
                let mut sink = RecordingSink::default();
                feed(&mut parser, &input, &mut sink);
                let what = format!("{name} {how} st={st:02x?}");
                assert_eq!(sink.prints.iter().collect::<String>(), "after", "{what}");
                assert!(
                    sink.csi_dispatches.is_empty() && sink.executes.is_empty(),
                    "{what}"
                );
                assert_eq!(parser.state(), State::Ground, "{what}");
                let delivered = match name {
                    "hooked DCS" => &sink.dcs_puts,
                    _ => &sink.apc_data,
                };
                let expect: &[u8] = match name {
                    "hooked DCS" | "APC" => payload.as_bytes(),
                    _ => b"",
                };
                assert_eq!(delivered.as_slice(), expect, "{what}: the whole payload");
            }
        }

        // Every other 0x9C is ST: after ASCII, after a whole character, after
        // a lead it cannot follow, after a broken prefix, after a byte that
        // begins no sequence.
        for before in [
            &b"x"[..],
            "\u{672c}".as_bytes(),
            b"\xe0",
            b"\xf4",
            b"\xe6x",
            b"\xf0\x90\x80\x80",
            b"\xc0",
            b"\xf5",
        ] {
            let input = [head, before, b"\x9cafter"].concat();
            for (how, feed) in feeds {
                let mut parser = Parser::new();
                let mut sink = RecordingSink::default();
                feed(&mut parser, &input, &mut sink);
                assert_eq!(
                    sink.prints.iter().collect::<String>(),
                    "after",
                    "{name} {how} {before:02x?}: the 0x9C is ST"
                );
            }
        }

        // With C1 controls on, the raw 0x9C is the 8-bit ST even inside one.
        let mut parser = Parser::with_c1_controls();
        let mut sink = RecordingSink::default();
        parser.advance_fast(&[head, "\u{672c}after".as_bytes()].concat(), &mut sink);
        assert_eq!(
            sink.prints.iter().collect::<String>(),
            "\u{fffd}after",
            "{name}: C1 on"
        );
    }
}

/// A GR BYTE IN A DCS HEADER KEEPS THE STRING (round fifteen, R15-PARSER-P2).
/// A GR byte (`0xA0..=0xFF`) in `DcsEntry`, `DcsParam` or `DcsIntermediate`
/// sent the parser to Ground, so the string after it printed and ran as
/// terminal input: `ESC P 1 é q #0;2;0;0;0#0~~~~ ESC \ after` printed
/// `\u{fffd}q#0;2;0;0;0#0~~~~after`. The header now ignores it, as vte's does.
///
/// The table, entry by entry: every GR byte stays in each header state and
/// does nothing, and `0x80..=0x9F` keep the C1 entries every other sequence
/// state has. Then a string with GR bytes in each header state, fed whole,
/// one byte per read and split in two at every byte, C1 controls off and on:
/// each hooks its string, delivers
/// the payload and prints only `after`. With C1 controls off a character
/// holding `0x9C` (本, `E6 9C AC`) in the header keeps the string too: the C1
/// guard drops its `0x9C`.
#[test]
fn a_gr_byte_in_a_dcs_header_keeps_the_string() {
    for state in [State::DcsEntry, State::DcsParam, State::DcsIntermediate] {
        for byte in 0x80..=u8::MAX {
            let entry = TRANSITIONS[state as usize][usize::from(byte)];
            let expected = if byte >= 0xA0 {
                (state, crate::table::ActionType::Ignore)
            } else {
                let c1 = TRANSITIONS[State::Escape as usize][usize::from(byte)];
                (c1.next_state, c1.action)
            };
            assert_eq!(
                (entry.next_state, entry.action),
                expected,
                "{state:?} byte {byte:#04x}"
            );
        }
    }

    // (stream, the hook, the payload, C1 controls off only)
    let strings: [(&[u8], (Vec<u16>, Vec<u8>, u8), &[u8], bool); 6] = [
        (
            "\x1bP\u{e9}q#0~\x1b\\after".as_bytes(),
            (vec![], vec![], b'q'),
            b"#0~",
            false,
        ),
        (
            "\x1bP1\u{e9}q#0;2;0;0;0#0~~~~\x1b\\after".as_bytes(),
            (vec![1], vec![], b'q'),
            b"#0;2;0;0;0#0~~~~",
            false,
        ),
        (
            "\x1bP$\u{e9}qm\x1b\\after".as_bytes(),
            (vec![], vec![b'$'], b'q'),
            b"m",
            false,
        ),
        (
            b"\x1bP\xa01;\xff2$\xa0\xffqm\x1b\\after",
            (vec![1, 2], vec![b'$'], b'q'),
            b"m",
            false,
        ),
        (
            "\x1bP1\u{672c};2\u{276f}q#0~\x1b\\after".as_bytes(),
            (vec![1, 2], vec![], b'q'),
            b"#0~",
            true,
        ),
        (
            "\x1bP+\u{276f}q544e\x1b\\after".as_bytes(),
            (vec![], vec![b'+'], b'q'),
            b"544e",
            true,
        ),
    ];
    let feeds: [(&str, fn(&mut Parser, &[u8], &mut RecordingSink)); 1] =
        [("advance_fast", |p, b, s| p.advance_fast(b, s))];
    for (stream, hook, payload, c1_off_only) in strings {
        for c1 in [false, true] {
            if c1 && c1_off_only {
                continue;
            }
            for (how, feed) in feeds {
                let mut chunkings: Vec<Vec<&[u8]>> = vec![vec![stream], stream.chunks(1).collect()];
                chunkings.extend((1..stream.len()).map(|at| {
                    let (head, tail) = stream.split_at(at);
                    vec![head, tail]
                }));
                for chunks in chunkings {
                    let mut parser = if c1 {
                        Parser::with_c1_controls()
                    } else {
                        Parser::new()
                    };
                    let mut sink = RecordingSink::default();
                    for chunk in &chunks {
                        feed(&mut parser, chunk, &mut sink);
                    }
                    let what = format!(
                        "c1={c1} {how} {} reads {:?}",
                        chunks.len(),
                        String::from_utf8_lossy(stream)
                    );
                    assert_eq!(sink.dcs_hooks, vec![hook.clone()], "{what}");
                    assert_eq!(sink.dcs_puts, payload, "{what}");
                    assert_eq!(sink.dcs_unhooks, 1, "{what}");
                    assert_eq!(sink.prints.iter().collect::<String>(), "after", "{what}");
                    assert!(
                        sink.executes.is_empty() && sink.csi_dispatches.is_empty(),
                        "{what}"
                    );
                    assert_eq!(parser.state(), State::Ground, "{what}");
                }
            }
        }
    }
}
