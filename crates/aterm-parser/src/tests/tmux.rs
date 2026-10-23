// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Tmux passthrough wrapper (`ESC P tmux; ... ESC \`) parser tests.

use super::*;

/// What `parser` delivers for `bytes` cut at every byte, and one byte at a time.
fn feed_every_way(c1: bool, bytes: &[u8]) -> RecordingSink {
    let fresh = || {
        let mut parser = Parser::new();
        parser.c1_controls_enabled = c1;
        parser
    };
    let mut whole = RecordingSink::default();
    fresh().advance_fast(bytes, &mut whole);
    let shown = String::from_utf8_lossy(bytes).into_owned();
    for cut in 1..bytes.len() {
        let mut parser = fresh();
        let mut sink = RecordingSink::default();
        parser.advance_fast(&bytes[..cut], &mut sink);
        parser.advance_fast(&bytes[cut..], &mut sink);
        assert_eq!(sink, whole, "{shown:?} c1={c1} cut after byte {cut}");
    }
    let mut parser = fresh();
    let mut sink = RecordingSink::default();
    for byte in bytes {
        parser.advance_fast(std::slice::from_ref(byte), &mut sink);
        parser.assert_invariants();
    }
    assert_eq!(sink, whole, "{shown:?} c1={c1} byte at a time");
    whole
}

/// `payload` in tmux's passthrough wrapper.
fn wrapped(payload: &[u8]) -> Vec<u8> {
    let mut out = b"\x1bPtmux;".to_vec();
    for &byte in payload {
        if byte == 0x1B {
            out.push(0x1B);
        }
        out.push(byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// The payloads a program wraps.
const PAYLOADS: [&[u8]; 9] = [
    b"\x1bP$qm\x1b\\",
    b"\x1bP+q544E\x1b\\",
    b"\x1bPq#0;2;100;0;0#0~~~~~~-~~~~~~\x1b\\",
    b"\x1b_Gi=31,a=q;AAAA\x1b\\",
    b"\x1b]52;c;aGk=\x07",
    b"\x1b]2;title\x1b\\",
    b"\x1b[5;7H\x1b[1mbold\x1b[m",
    "caf\u{e9} \u{672c}".as_bytes(),
    b"\x1bP$qm\x9c",
];

/// THE WRAPPER IS A TRANSPORT. Each payload wrapped reads exactly as it reads
/// bare — the same prints, dispatches, hooks, puts and ends, the DCS ended
/// at ST complete — with text before and after, however the bytes are cut,
/// in both C1 modes. tmux 3.5a, given these wrappers with `allow-passthrough
/// on`, writes exactly the bare payloads to its own terminal.
#[test]
fn a_wrapped_sequence_reads_as_it_reads_bare() {
    for c1 in [false, true] {
        for payload in PAYLOADS {
            let bare = feed_every_way(c1, &[b"<", payload, b">"].concat());
            let got = feed_every_way(c1, &[b"<", &wrapped(payload)[..], b">"].concat());
            assert_eq!(
                got,
                bare,
                "c1={c1} {:?}: wrapped reads as bare",
                String::from_utf8_lossy(payload)
            );
        }
        let bare = feed_every_way(c1, b"\x1bP$qm\x1b\\\x1bP+q544E\x1b\\");
        let two = [wrapped(b"\x1bP$qm\x1b\\"), wrapped(b"\x1bP+q544E\x1b\\")].concat();
        assert_eq!(feed_every_way(c1, &two), bare, "c1={c1} back to back");
        let bare = feed_every_way(c1, b"\x1b[1;2Hok");
        let split = [&wrapped(b"\x1b[1;")[..], b"2Hok"].concat();
        assert_eq!(feed_every_way(c1, &split), bare, "c1={c1} unfinished");
    }
}

/// A string the payload leaves open ends with its wrapper, as at its ST, and
/// what follows the wrapper reads from Ground: a title is set, a query is
/// ended at ST, and the text after prints. Behind tmux the string would run on
/// into whatever tmux wrote next.
#[test]
fn a_string_left_open_ends_with_its_wrapper() {
    for c1 in [false, true] {
        for (open, bare) in [
            (&b"\x1b]2;t"[..], &b"\x1b]2;t\x1b\\ok\x07"[..]),
            (b"\x1bP$qm", b"\x1bP$qm\x1b\\ok\x07"),
            (b"\x1bP$qm\x1b", b"\x1bP$qm\x1b\\ok\x07"),
            (b"\x1b_Ga=q", b"\x1b_Ga=q\x1b\\ok\x07"),
        ] {
            let got = feed_every_way(c1, &[&wrapped(open)[..], b"ok\x07"].concat());
            assert_eq!(got, feed_every_way(c1, bare), "c1={c1} {open:?}");
        }
    }
}

/// An `ESC \` the program did not double ends its own string as it ends the
/// wrapper, so an ST-ended sequence wrapped by a program that doubled nothing
/// still runs, and the text after it prints, as before the unwrap (its ESC
/// broke the wrapper off). RED before: the wrapper's end left the payload's
/// OSC open, which swallowed the text after it.
#[test]
fn an_st_the_program_did_not_double_ends_its_string() {
    for c1 in [false, true] {
        for payload in [
            &b"\x1b]2;t\x1b\\"[..],
            b"\x1bP$qm\x1b\\",
            b"\x1bPq#0;2;100;0;0#0~~~~~~\x1b\\",
            b"\x1b_Ga=q\x1b\\",
        ] {
            let bare = feed_every_way(c1, &[payload, b"ok"].concat());
            let got = feed_every_way(c1, &[b"\x1bPtmux;", payload, b"ok"].concat());
            assert_eq!(got, bare, "c1={c1} {payload:?}");
        }
    }
}

/// RIS (`ESC c`) resets the parser whole, the wrapper it arrives in included,
/// whether the program doubled its ESC or not: what follows reads as bare, an
/// ST-ended title and the text after it too. RED before: the wrapper stayed
/// open, and the title's ST ended it instead of the title.
#[test]
fn a_reset_ends_the_wrapper() {
    for c1 in [false, true] {
        let after = b"\x1b]2;after\x1b\\hi";
        let bare = feed_every_way(c1, &[&b"\x1bc"[..], after].concat());
        for head in [&b"\x1bPtmux;\x1bc"[..], b"\x1bPtmux;\x1b\x1bc"] {
            let mut parser = Parser::new();
            parser.c1_controls_enabled = c1;
            parser.advance_fast(head, &mut RecordingSink::default());
            assert_eq!(parser.tmux_passthrough(), TmuxPassthrough::Off, "{head:?}");
            let got = feed_every_way(c1, &[head, after].concat());
            assert_eq!(got, bare, "c1={c1} {head:?}");
            let mut parser = Parser::new();
            parser.c1_controls_enabled = c1;
            let mut sink = RecordingSink::default();
            parser.advance_fast(&[head, b"\x1b[1mhi"].concat(), &mut sink);
            assert_eq!(sink.prints, ['h', 'i'], "c1={c1} {head:?}");
            assert!(
                parser.tmux_passthrough() == TmuxPassthrough::Off
                    && parser.state() == State::Ground,
                "c1={c1} {head:?}"
            );
        }
    }
}

/// Negative control: the comparison above tells the two apart. The same
/// wrapper with a head that is not `tmux;` is an ignored DCS, which its
/// doubled ESC breaks off, and the payload's doubled ST breaks the payload's
/// DCS off: the query is canceled, never ended at ST, exactly as every
/// wrapper read before the unwrap.
#[test]
fn a_head_that_is_not_tmux_is_an_ignored_dcs() {
    for c1 in [false, true] {
        let payload = b"\x1bP$qm\x1b\\";
        let mut bytes = wrapped(payload);
        bytes[6] = b':';
        let sink = feed_every_way(c1, &bytes);
        assert_ne!(sink, feed_every_way(c1, payload), "c1={c1}");
        assert_eq!((sink.dcs_unhooks, sink.dcs_cancels), (0, 1), "c1={c1}");
        assert_eq!(sink.dcs_hooks, vec![(vec![], b"$".to_vec(), b'q')]);
        for head in [
            &b"\x1bP1tmux;"[..],
            b"\x1bP tmux;",
            b"\x1bPtmuxx",
            b"\x1bPtm",
        ] {
            let sink = feed_every_way(c1, &[head, b"\x1b\\ok"].concat());
            assert_eq!(sink.prints.iter().collect::<String>(), "ok", "{head:?}");
        }
    }
}

/// An ESC the program did not double keeps both bytes: such a sequence ran
/// before the unwrap (its ESC broke the wrapper off) and still runs. tmux
/// drops that ESC and forwards the rest as text (measured: `] 2 ; t BEL`).
#[test]
fn an_escape_the_program_did_not_double_keeps_its_sequence() {
    for c1 in [false, true] {
        let bare = feed_every_way(c1, b"\x1b]2;t\x07ok");
        let got = feed_every_way(c1, b"\x1bPtmux;\x1b]2;t\x07\x1b\\ok");
        assert_eq!(got, bare, "c1={c1}");
    }
}

/// CAN, SUB and the 8-bit ST are payload, as tmux forwards them: they act on
/// the payload's own sequence and leave the wrapper open, so a later doubled
/// ESC is still one ESC and the text after the wrapper's `ESC \` prints.
#[test]
fn a_cancel_inside_the_wrapper_cancels_the_payload_not_the_wrapper() {
    for c1 in [false, true] {
        let bare = feed_every_way(c1, b"\x1bP$q\x18\x1b[1mok");
        let got = feed_every_way(c1, b"\x1bPtmux;\x1b\x1bP$q\x18\x1b\x1b[1m\x1b\\ok");
        assert_eq!(got, bare, "c1={c1}");
        assert_eq!((got.dcs_unhooks, got.dcs_cancels), (0, 1), "c1={c1}");
    }
}

/// A wrapper inside a wrapper is unwrapped once: the inner `ESC P tmux;` is
/// a DCS the sink ignores, broken off by its own doubled ESC, which then
/// begins the payload as it did before the unwrap.
#[test]
fn a_wrapper_inside_a_wrapper_is_unwrapped_once() {
    let mut parser = Parser::new();
    let mut sink = RecordingSink::default();
    parser.advance_fast(&wrapped(&wrapped(b"\x1b]2;t\x07")), &mut sink);
    assert_eq!(parser.tmux_passthrough(), TmuxPassthrough::Off);
    assert_eq!(parser.state(), State::Ground);
    assert_eq!(sink.dcs_hooks, vec![(vec![], vec![], b't')]);
    assert_eq!((sink.dcs_unhooks, sink.dcs_cancels), (0, 1));
    assert_eq!(
        sink.osc_dispatches,
        vec![vec![b"2".to_vec(), b"t".to_vec()]]
    );
}

/// Where a wrapper stands, and what ends it from there: a `CAN`, which ends
/// what the payload left torn (canceled, never ended at ST) and the head of a
/// wrapper not yet open, then `ESC \` while one is open. What follows reads
/// from Ground.
#[test]
fn a_cancel_then_st_end_the_wrapper_from_where_it_stands() {
    for (head, stands) in [
        (&b"\x1bPtmux;"[..], TmuxPassthrough::Open),
        (b"\x1bPtmux;\x1b", TmuxPassthrough::Escape),
        (b"\x1bPtmux;\x1b\x1bPq#0", TmuxPassthrough::Open),
        (b"\x1bPtmux;\x1b\x1b]2;torn", TmuxPassthrough::Open),
        (b"\x1bPtmu", TmuxPassthrough::Head(3)),
        (b"\x1bPtmux;\x1b\\", TmuxPassthrough::Off),
    ] {
        let mut parser = Parser::new();
        let mut sink = RecordingSink::default();
        parser.advance_fast(head, &mut sink);
        assert_eq!(parser.tmux_passthrough(), stands, "{head:?}");
        let closing: &[u8] = if stands.is_open() {
            b"\x18\x1b\\"
        } else {
            b"\x18"
        };
        parser.advance_fast(&[closing, b"ok"].concat(), &mut sink);
        assert_eq!(parser.tmux_passthrough(), TmuxPassthrough::Off, "{head:?}");
        assert_eq!(parser.state(), State::Ground, "{head:?}");
        assert!(
            sink.prints.ends_with(&['o', 'k']),
            "{head:?}: {:?}",
            sink.prints
        );
        assert_eq!(sink.dcs_unhooks, 0, "{head:?}: nothing ended at ST");
        assert!(
            sink.osc_dispatches.is_empty(),
            "{head:?}: nothing dispatched"
        );
    }
}

/// A carry taken at any byte of a wrapped stream continues in the successor
/// exactly as the live parser goes on: the wrapper's head, the open wrapper
/// and the ESC it waits on travel with the payload's own state. Where the
/// payload's DCS is hooked, no `carry` exists (the handler holds the
/// string), as without a wrapper.
#[test]
fn a_carry_inside_a_wrapper_continues_in_the_successor() {
    let stream = [
        &b"<"[..],
        &wrapped(b"\x1b]2;title\x1b\\\x1b[1mX\x1b_Ga=q\x1b\\")[..],
        b">",
        &wrapped("\u{672c}".as_bytes())[..],
    ]
    .concat();
    for c1 in [false, true] {
        let mut live_all = Parser::new();
        live_all.c1_controls_enabled = c1;
        let mut whole = RecordingSink::default();
        live_all.advance_fast(&stream, &mut whole);
        for cut in 0..=stream.len() {
            let mut outgoing = Parser::new();
            outgoing.c1_controls_enabled = c1;
            let mut before = RecordingSink::default();
            outgoing.advance_fast(&stream[..cut], &mut before);
            let Some(carry) = outgoing.carry() else {
                assert!(outgoing.in_hooked_dcs(), "c1={c1} cut {cut}");
                continue;
            };
            let mut successor = Parser::new();
            successor.c1_controls_enabled = c1;
            successor.restore_carry(&carry).expect("restores");
            assert_eq!(
                successor.tmux_passthrough(),
                outgoing.tmux_passthrough(),
                "c1={c1} cut {cut}"
            );
            let swallows = outgoing.carry_swallows_sequence();
            let mut after = RecordingSink::default();
            successor.advance_fast(&stream[cut..], &mut after);
            let mut live = RecordingSink::default();
            outgoing.advance_fast(&stream[cut..], &mut live);
            if swallows {
                assert_eq!(after.prints, live.prints, "c1={c1} cut {cut}");
            } else {
                assert_eq!(after, live, "c1={c1} cut {cut}");
            }
        }
    }
}

/// A head carried anywhere but in the ignored DCS its `ESC P t` began, or
/// past four bytes, is refused.
#[test]
fn a_head_outside_its_ignored_dcs_is_refused() {
    for (state, tmux) in [
        (State::Ground, TmuxPassthrough::Head(2)),
        (State::DcsIgnore, TmuxPassthrough::Head(0)),
        (State::DcsIgnore, TmuxPassthrough::Head(5)),
    ] {
        let carry = ParserCarry {
            state,
            tmux,
            ..ParserCarry::default()
        };
        assert_eq!(
            Parser::new().restore_carry(&carry),
            Err(CarryRefusal::FieldOutsideState),
            "{state:?} {tmux:?}"
        );
    }
    let carry = ParserCarry {
        state: State::DcsIgnore,
        tmux: TmuxPassthrough::Head(4),
        ..ParserCarry::default()
    };
    let mut parser = Parser::new();
    parser
        .restore_carry(&carry)
        .expect("a head in its ignored DCS");
    let mut sink = RecordingSink::default();
    parser.advance_fast(b";\x1b\x1b]2;t\x07\x1b\\ok", &mut sink);
    assert_eq!(sink.prints.iter().collect::<String>(), "ok");
    assert_eq!(sink.osc_dispatches.len(), 1);
}
