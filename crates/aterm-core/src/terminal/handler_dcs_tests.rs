// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::terminal::Terminal;

#[test]
fn a_handback_does_not_answer_a_torn_query() {
    let mut t = Terminal::new(24, 80);
    t.process(b"\x1b[1m\x1bP$qm");
    let h = t.foreground_handback_scoped(false).expect("torn DECRQSS");
    assert_eq!(h.bytes, vec![0x18]);
    assert_eq!(t.take_response(), None);

    let mut t = Terminal::new(24, 80);
    t.process(b"\x1bP+q544E");
    assert_eq!(t.manual_handback().bytes, vec![0x18]);
    assert_eq!(t.take_response(), None);
}

fn fed(bytes: &[u8], cut: usize) -> Terminal {
    let mut t = Terminal::new(24, 80);
    let (head, tail) = bytes.split_at(cut);
    t.process(head);
    t.process(tail);
    t
}

#[test]
fn a_query_broken_off_by_an_escape_is_not_answered() {
    for query in [&b"\x1bP$qm"[..], b"\x1bP+q544E"] {
        for brk in [
            &b"\x1b[5;7H"[..],
            b"\x1b]2;t\x07\x1b[5;7H",
            b"\x1b\x1b[5;7H",
        ] {
            let bytes = [query, brk].concat();
            for cut in 0..=bytes.len() {
                let mut t = fed(&bytes, cut);
                let shown = String::from_utf8_lossy(&bytes);
                assert_eq!(t.take_response(), None, "{shown:?} cut {cut}: answered");
                let cursor = t.cursor();
                assert_eq!((cursor.row, cursor.col), (4, 6), "{shown:?} cut {cut}");
            }
        }
    }
}

fn tmux_wrapped(payload: &[u8]) -> Vec<u8> {
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

#[test]
fn a_query_wrapped_for_tmux_is_answered_as_the_bare_one() {
    for query in [&b"\x1bP$qm\x1b\\"[..], b"\x1bP+q544E\x1b\\"] {
        let bare = fed(query, 0).take_response();
        assert!(bare.is_some(), "{query:?}: the bare query is answered");
        let bytes = tmux_wrapped(query);
        for cut in 0..=bytes.len() {
            let shown = String::from_utf8_lossy(&bytes);
            assert_eq!(
                fed(&bytes, cut).take_response(),
                bare,
                "{shown:?} cut {cut}"
            );
        }
    }
}

#[test]
fn a_query_ended_at_st_is_answered_however_it_is_cut() {
    for (query, reply) in [
        (&b"\x1bP$qm"[..], &b"\x1bP1$r0m\x1b\\"[..]),
        (b"\x1bP+q544E", b"\x1bP1+r544E="),
    ] {
        for st in [&b"\x1b\\"[..], b"\x9c"] {
            let bytes = [query, st].concat();
            for cut in 0..=bytes.len() {
                let response = fed(&bytes, cut).take_response();
                assert!(
                    response.as_deref().is_some_and(|r| r.starts_with(reply)),
                    "{:?} cut {cut}: {response:?}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }
}

#[test]
fn a_consumed_string_broken_off_runs_what_breaks_it_off() {
    let heads: [&[u8]; 6] = [
        b"\x1bPtmux;",
        b"\x1bP1000p%begin 1 1 0\n",
        b"\x1bP2000pabc",
        b"\x1bP1;1{ @???",
        b"\x1bP@kitty-cmd{",
        b"\x1bP=1s",
    ];
    for head in heads {
        let bytes = [head, &b"\x1b\x1b]2;inner\x07\x1b\\after"[..]].concat();
        for cut in 0..=bytes.len() {
            let mut t = fed(&bytes, cut);
            let shown = String::from_utf8_lossy(&bytes);
            assert_eq!(t.title(), "inner", "{shown:?} cut {cut}");
            assert_eq!(
                t.row_text(0).as_deref().map(str::trim_end),
                Some("after"),
                "{shown:?} cut {cut}"
            );
            assert_eq!(t.take_response(), None, "{shown:?} cut {cut}");
        }
    }
}

#[test]
fn a_stray_wrapper_head_breaks_no_sequence_after_it() {
    let head = b"\x1bPtmux;";
    let title = b"\x1b]2;t\x1b\\ok";
    for cut in 0..=head.len() + title.len() {
        let t = fed(&[&head[..], title].concat(), cut);
        assert_eq!(t.title(), "t", "cut {cut}");
        assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("ok"));
        assert!(t.parser_is_ground(), "cut {cut}");
    }
    let mut t = Terminal::new(24, 80);
    t.process(head);
    t.process(b"\x1bc");
    assert!(t.parser_is_ground(), "RIS ends the wrapper");
    t.process(b"\x1b]2;after-ris\x1b\\hi");
    assert_eq!(t.title(), "after-ris");
    assert_eq!(t.row_text(0).as_deref().map(str::trim_end), Some("hi"));
    let mut t = Terminal::new(24, 80);
    t.process(head);
    for _ in 0..3 {
        t.process(b"\x1b]133;A\x07% \x1b]133;B\x07\r\n");
    }
    t.process(b"\x1bP+q544e\x1b\\");
    let response = t.take_response();
    assert!(
        response
            .as_deref()
            .is_some_and(|r| r.starts_with(b"\x1bP1+r544E=")),
        "{response:?}"
    );
    assert!(t.parser_is_ground());
}

#[test]
fn a_handback_does_not_answer_a_query_torn_at_its_escape() {
    for query in [&b"\x1b[1m\x1bP$qm\x1b"[..], b"\x1bP+q544E\x1b"] {
        let mut t = Terminal::new(24, 80);
        t.process(query);
        assert_eq!(t.manual_handback().bytes, vec![0x18], "{query:?}");
        assert_eq!(t.take_response(), None, "{query:?}");
    }
}

fn image_cells(t: &Terminal) -> usize {
    let grid = t.grid();
    (0..grid.rows())
        .flat_map(|r| (0..grid.cols()).map(move |c| (r, c)))
        .filter(|&(r, c)| grid.cell_extra(r, c).is_some_and(|e| e.image().is_some()))
        .count()
}

#[test]
fn a_sixel_wrapped_for_tmux_is_placed() {
    let sixel = b"\x1bPq#0;2;100;0;0#0~~~~~~-~~~~~~\x1b\\";
    let placed = image_cells(&fed(sixel, 0));
    assert!(placed > 0);
    let bytes = tmux_wrapped(sixel);
    for cut in 0..=bytes.len() {
        assert_eq!(image_cells(&fed(&bytes, cut)), placed, "cut {cut}");
    }
}

#[test]
fn a_sixel_broken_off_by_an_escape_is_not_placed() {
    let sixel = b"\x1bPq#0;2;100;0;0#0~~~~~~-~~~~~~";
    for brk in [&b"\x1b[5;7H"[..], b"\x1b\x1b\\"] {
        let bytes = [&sixel[..], brk].concat();
        for cut in 0..=bytes.len() {
            assert_eq!(image_cells(&fed(&bytes, cut)), 0, "{brk:?} cut {cut}");
        }
    }
    for st in [&b"\x1b\\"[..], b"\x9c"] {
        let bytes = [&sixel[..], st].concat();
        for cut in 0..=bytes.len() {
            assert!(image_cells(&fed(&bytes, cut)) > 0, "{st:?} cut {cut}");
        }
    }
}
