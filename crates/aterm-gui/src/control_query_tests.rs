// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use aterm_core::terminal::{Terminal, take_narrowed_query_steps};
use aterm_messages::WaitEnd;

use super::{
    cmd_cell, cmd_custody, cmd_search, distinct_images, gather_styled_frame, serialize_dims,
    serialize_dims_json, styled_frame_payload,
};
use crate::session_waits::{RewrapGauge, RewrapReading};
use crate::term_lock;

#[test]
fn native_focus_keeps_unavailable_and_inactive_windows_unready() {
    use crate::platform::NativeFocus;

    for (focus, ready) in [
        (None, "-"),
        (Some((false, true)), "0"),
        (Some((true, false)), "0"),
        (Some((true, true)), "1"),
    ] {
        let observed = focus.map(|(app_active, window_key)| NativeFocus {
            window: 42,
            app_active,
            window_key,
        });
        let reply = super::native_focus_with("", || Ok((observed, 17)));
        let fields: std::collections::BTreeMap<_, _> = reply
            .split_whitespace()
            .filter_map(|word| word.split_once('='))
            .collect();
        assert_eq!(fields["key"], ready);
        assert_eq!(fields["available"], if focus.is_some() { "1" } else { "0" });
        assert_eq!(fields["posted_keys"], "17", "only observes the counter");
    }
    for bad in ["x", "count=0", "unread=ok", "probe"] {
        assert_eq!(
            super::native_focus_with(bad, || panic!("bad syntax reached the native query")),
            "ERR usage: native-focus\n"
        );
    }
    assert_eq!(
        super::native_focus_with("", || Err("event loop closed".into())),
        "ERR event loop closed\n"
    );
}

fn place(t: &mut Terminal, row: u16, col: u16, id: u8, tag: u8) {
    t.process(format!("\x1b[{row};{col}H").as_bytes());
    let raster = vec![tag; 10 * 20 * 4];
    let b64 = aterm_codec::base64::encode(&raster).expect("encode");
    let mut apc = format!("\x1b_Ga=T,f=32,s=10,v=20,i={id};").into_bytes();
    apc.extend_from_slice(b64.as_bytes());
    apc.extend_from_slice(b"\x1b\\");
    t.process(&apc);
}

#[test]
fn distinct_images_are_row_major_regardless_of_placement_order() {
    let mut t = Terminal::new(10, 40);
    t.set_cell_pixel_size(10, 20);
    place(&mut t, 5, 3, 1, b'F');
    place(&mut t, 1, 33, 2, b'E');
    place(&mut t, 1, 8, 3, b'B');
    place(&mut t, 1, 2, 4, b'A');
    place(&mut t, 1, 20, 5, b'D');
    place(&mut t, 3, 10, 6, b'C');

    let got: Vec<(usize, usize, u8)> = distinct_images(&t)
        .into_iter()
        .map(|(r, c, img)| (r, c, img.bytes[0]))
        .collect();
    assert_eq!(
        got,
        vec![
            (0, 1, b'A'),
            (0, 7, b'B'),
            (0, 19, b'D'),
            (0, 32, b'E'),
            (2, 9, b'C'),
            (4, 2, b'F'),
        ],
        "images must be reported row-major, not in placement or map order"
    );
}

#[test]
fn a_frame_change_that_moves_no_picture_encodes_no_image() {
    use crate::work_counts::take_images_encoded;
    let frame = styled_frame_payload;
    let images = |frame: &str| {
        let at = frame
            .rfind("\"images\":")
            .expect("the frame ends with its images");
        frame[at..].to_string()
    };
    let mut t = Terminal::new(6, 40);
    t.set_cell_pixel_size(10, 20);
    place(&mut t, 1, 2, 1, b'A');
    place(&mut t, 1, 8, 2, b'B');
    place(&mut t, 3, 10, 3, b'C');
    take_images_encoded();

    let first = frame(&t);
    assert_eq!(
        take_images_encoded(),
        3,
        "the first frame encodes each picture"
    );
    t.process(b"\x1b[6;1Hmore text");
    let second = frame(&t);
    assert_ne!(first, second, "the frame changed");
    assert_eq!(take_images_encoded(), 0, "and no picture is encoded again");
    assert_eq!(images(&first), images(&second), "the same images array");

    place(&mut t, 5, 20, 4, b'D');
    let third = frame(&t);
    assert_eq!(take_images_encoded(), 1, "a new picture is encoded");
    assert_eq!(images(&third).matches("\"b64\":").count(), 4);
}

#[test]
fn custody_names_the_three_events_that_change_nothing() {
    use crate::app_input::{PressKind, apply_press_custody};

    let mut t = Terminal::new(6, 20);
    for i in 0..10 {
        t.process(format!("line{i}\r\n").as_bytes());
    }
    t.scroll_display(1);
    {
        let sel = t.text_selection_mut();
        sel.start_selection(
            0,
            0,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        sel.update_selection(1, 5, aterm_core::selection::SelectionSide::Right);
        sel.complete_selection();
    }
    let term = Arc::new(Mutex::new(t));

    let mut seen = Vec::new();
    for kind in [PressKind::Inert, PressKind::Repeat, PressKind::Release] {
        apply_press_custody(&mut term_lock(&term), kind);
        seen.push(cmd_custody(&term));
    }
    assert_eq!(
        seen,
        vec![
            "OK last=InertPress event=3 changed=UserScroll took_selection=none offset=1 owner=user \
             selection=yes scrollback=5\n"
                .to_string(),
            "OK last=RepeatPress event=2 changed=UserScroll took_selection=none offset=1 owner=user \
             selection=yes scrollback=5\n"
                .to_string(),
            "OK last=ReleaseEvent event=4 changed=UserScroll took_selection=none offset=1 owner=user \
             selection=yes scrollback=5\n"
                .to_string(),
        ],
        "three events that moved NOTHING must still be told apart by name — and \
         none of them may claim to have CHANGED anything: the scroll that really \
         took the viewport is still the answer to `changed`"
    );

    apply_press_custody(&mut term_lock(&term), PressKind::Typing);
    assert_eq!(
        cmd_custody(&term),
        "OK last=TypingPress event=1 changed=TypingPress took_selection=TypingPress offset=0 owner=tail \
         selection=no scrollback=5\n",
        "typing is the one handover, and the verb shows both the name and the effect"
    );

    term_lock(&term).process(b"$ \r\n");
    assert_eq!(
        cmd_custody(&term),
        "OK last=OutputAtLive event=5 changed=TypingPress took_selection=TypingPress offset=0 owner=tail \
         selection=no scrollback=6\n",
        "ordinary output is the last EVENT but must not become the last CHANGE"
    );

    term_lock(&term).process(b"\x1b[3J");
    assert_eq!(
        cmd_custody(&term),
        "OK last=OutputInvalidatesTheCoordinateSpace event=7 changed=TypingPress \
         took_selection=OutputInvalidatesTheCoordinateSpace \
         offset=0 owner=tail selection=no scrollback=0\n",
        "ED 3 destroyed the coordinate space; the verb names that, not a press"
    );
}

#[test]
fn a_later_click_does_not_erase_why_the_selection_actually_died() {
    let mut t = Terminal::new(4, 20);
    for i in 0..8 {
        t.process(format!("line{i}\r\n").as_bytes());
    }
    {
        let sel = t.text_selection_mut();
        sel.start_selection(
            1,
            0,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        sel.update_selection(1, 4, aterm_core::selection::SelectionSide::Right);
        sel.complete_selection();
    }
    let term = Arc::new(Mutex::new(t));

    term_lock(&term).process(b"\x1b[2;1H\x1b[Kreplaced");
    assert!(
        !term_lock(&term).text_selection().has_selection(),
        "precondition: the output really did take the highlight"
    );
    let before = cmd_custody(&term);
    assert!(
        before.contains("took_selection=OutputDamagesTheSelectedRows"),
        "the culprit is named while it is fresh: {before}"
    );

    crate::app_mouse::note_selection_custody(&mut term_lock(&term), false);

    let after = cmd_custody(&term);
    assert!(
        after.contains("changed=UserClear"),
        "the click IS the last thing that moved custody, and `changed` says so: {after}"
    );
    assert!(
        after.contains("took_selection=OutputDamagesTheSelectedRows"),
        "…but the explanation of the involuntary loss must SURVIVE it: {after}"
    );
}

#[test]
fn custody_names_output_that_took_the_selection_with_no_action_to_blame() {
    let mut t = Terminal::new(4, 20);
    t.set_scrollback_line_limit(Some(2));
    for i in 0..8 {
        t.process(format!("line{i}\r\n").as_bytes());
    }
    assert_eq!(t.grid().scrollback_lines(), 2, "a two-line history floor");
    {
        let sel = t.text_selection_mut();
        sel.start_selection(
            -2,
            0,
            aterm_core::selection::SelectionSide::Left,
            aterm_core::selection::SelectionType::Simple,
        );
        sel.update_selection(-2, 5, aterm_core::selection::SelectionSide::Right);
        sel.complete_selection();
        assert!(sel.has_selection(), "the oldest retained row is selected");
    }
    let term = Arc::new(Mutex::new(t));
    crate::app_input::apply_press_custody(
        &mut term_lock(&term),
        crate::app_input::PressKind::Inert,
    );

    term_lock(&term).process(b"a\r\nb\r\nc\r\n");
    assert!(
        !term_lock(&term).text_selection().has_selection(),
        "the eviction really did take the highlight"
    );
    assert_eq!(
        cmd_custody(&term),
        "OK last=OutputTookTheSelectionUnattributed event=6 \
         changed=OutputTookTheSelectionUnattributed \
         took_selection=OutputTookTheSelectionUnattributed offset=0 owner=tail \
         selection=no scrollback=2\n",
        "output took it for a reason the model cannot name — which is a real \
         answer that rules out the keyboard, and `last=none` was not"
    );
}

#[test]
fn dims_serializers_expose_explicit_bottom_padding() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.backend.set_pad(12);
    app.backend.set_pad_top(2);
    app.windows.get_mut(&wid).unwrap().metrics.pad = 12;
    app.windows.get_mut(&wid).unwrap().metrics.pad_top = 2;
    let term = app.front_terminal(wid).unwrap().term.clone();
    let snapshot = app.try_dims_snapshot(0, &term).unwrap();

    let text = serialize_dims(&snapshot);
    assert!(text.contains("pad=12 pad_top=2 pad_bottom=12 head="));
    assert!(!text.contains("pad_bottom=22"), "deleted inferred bottom");

    let json = serialize_dims_json(&snapshot);
    assert!(json.contains("\"pad\":12,\"pad_top\":2,\"pad_bottom\":12"));
    assert!(!json.contains("\"pad_bottom\":22"));
}
fn term_with(lines: &[&str]) -> Arc<Mutex<Terminal>> {
    let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
    for l in lines {
        term.lock().unwrap().process(format!("{l}\r\n").as_bytes());
    }
    term
}

#[test]
fn screen_stamp_matches_the_text_verbs_rows() {
    for lines in [
        vec![],
        vec![
            "\u{23fa} All 248 tests pass; the branch is ready.",
            "  Next: the e2e suite.",
        ],
        vec![
            "trailing spaces follow   ",
            "",
            "\u{4e16}\u{754c} wide glyphs",
        ],
    ] {
        let term = term_with(&lines);
        let (_, stamp) = super::screen_stamp(&term.lock().unwrap());
        let served = super::cmd_text(&term);
        let body = served
            .split_once('\n')
            .map_or(String::new(), |(_, rest)| rest.to_string());
        assert_eq!(
            stamp,
            aterm_hash::fnv1a_64(body.as_bytes()),
            "stamp over {lines:?} must hash the rows `text` served:\n{served:?}"
        );
        let published = super::ScreenText::of(&term.lock().unwrap());
        assert_eq!(
            published.whole(),
            body,
            "the published text is `text`'s body"
        );
        assert_eq!(published.fnv(), stamp, "and its hash is the stamp");
    }
}

#[test]
fn agent_frame_stream_matches_whole_screen_text_across_grid_changes() {
    fn parity(t: &Terminal, phase: &str) {
        let text = super::screen_text(t);
        let all: Vec<&str> = text.split_terminator('\n').collect();
        assert_eq!(
            all.len(),
            t.rows() as usize,
            "{phase}: every row is present"
        );
        for tail in [0, 1, crate::presence::CLASSIFY_ROWS, 100] {
            let (fp, rows) = crate::session_status::screen_agent_frame(
                &crate::frame_memo::ScreenText::of(t),
                tail,
            );
            let from = all.len().saturating_sub(tail);
            let expected: Vec<String> = all[from..].iter().map(|row| (*row).to_string()).collect();
            assert_eq!(
                fp,
                aterm_hash::fnv1a_64(text.as_bytes()),
                "{phase}: fingerprint for tail={tail}"
            );
            assert_eq!(rows, expected, "{phase}: last {tail} rows");
        }
    }

    let mut t = Terminal::new(48, 80);
    parity(&t, "blank main grid");
    for n in 0..46 {
        t.process(format!("line {n:02}: 世界 e\u{301} trail   \r\n").as_bytes());
    }
    parity(&t, "deep Unicode main grid");
    t.process(b"\x1b[?1049h\x1b[H");
    t.process("代替 screen e\u{301}  ".as_bytes());
    parity(&t, "alternate grid");
    t.resize(24, 100);
    parity(&t, "resized alternate grid");
    t.process(b"\x1b[?1049l");
    parity(&t, "restored main grid");
}

#[test]
fn modes_frames_its_count_over_distinct_key_lines() {
    let term = term_with(&[]);
    assert_eq!(
        super::cmd_modes(&term),
        concat!(
            "OK 14\n",
            "alt_screen=false\n",
            "cursor_visible=true\n",
            "app_cursor_keys=false\n",
            "app_keypad=false\n",
            "bracketed_paste=false\n",
            "mouse_mode=none\n",
            "mouse_encoding=x10\n",
            "insert_mode=false\n",
            "auto_wrap=true\n",
            "origin_mode=false\n",
            "kitty_keyboard=none\n",
            "modify_other_keys=0\n",
            "focus_reporting=false\n",
            "sync_output=false\n",
        )
    );

    term.lock().unwrap().process(
        b"\x1b[?1049h\x1b[?25l\x1b[?1h\x1b=\x1b[?2004h\x1b[?1003h\x1b[?1006h\x1b[4h\x1b[?7l\x1b[?6h\x1b[>31u\x1b[>4;2m\x1b[?1004h\x1b[?2026h",
    );
    assert_eq!(
        super::cmd_modes(&term),
        concat!(
            "OK 14\n",
            "alt_screen=true\n",
            "cursor_visible=false\n",
            "app_cursor_keys=true\n",
            "app_keypad=true\n",
            "bracketed_paste=true\n",
            "mouse_mode=any\n",
            "mouse_encoding=sgr\n",
            "insert_mode=true\n",
            "auto_wrap=false\n",
            "origin_mode=true\n",
            "kitty_keyboard=disambiguate,report_events,report_alternates,report_all_keys,report_text\n",
            "modify_other_keys=2\n",
            "focus_reporting=true\n",
            "sync_output=true\n",
        )
    );
}

#[test]
fn cell_attrs_carry_the_width_markers() {
    let term = term_with(&["\u{6f22}a"]);
    let attrs = |reply: String| reply.trim_end().split(' ').nth(4).map(str::to_owned);
    let lead = super::cmd_cell(&term, "0 0");
    let cont = super::cmd_cell(&term, "0 1");
    let ascii = super::cmd_cell(&term, "0 2");
    assert_eq!(attrs(lead.clone()).as_deref(), Some("wide"), "{lead}");
    assert_eq!(attrs(cont.clone()).as_deref(), Some("wide_cont"), "{cont}");
    assert_eq!(attrs(ascii.clone()).as_deref(), Some("none"), "{ascii}");
}

#[test]
fn a_history_rows_cell_reads_as_it_did_on_the_screen() {
    let term = Arc::new(Mutex::new(Terminal::new(5, 12)));
    term.lock().unwrap().process(
        b"\x1b[1;38;2;92;92;255m> Hi\x1b[0m\r\n\
          \x1b[1;38;2;92;92;255m            \x1b[0m\r\n\
          \x1b[1;38;2;92;92;255;48;2;1;2;3m\x1b[K\x1b[0m\r\n\
          \x1b[38;2;208;208;208m  plain\x1b[0m\r\n",
    );
    let cols = 0..12;
    let live: Vec<Vec<String>> = (0..4)
        .map(|r| {
            cols.clone()
                .map(|c| cmd_cell(&term, &format!("{r} {c}")))
                .collect()
        })
        .collect();
    for (r, row) in live.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            assert!(cell.starts_with("OK "), "{r} {c}: {cell}");
            assert_eq!(
                &cmd_cell(&term, &format!("abs={r} {c}")),
                cell,
                "a live row by its absolute number, {r} {c}"
            );
        }
    }
    term.lock()
        .unwrap()
        .process(b"a\r\nb\r\nc\r\nd\r\ne\r\nf\r\n");
    assert!(
        term.lock().unwrap().grid().scrollback_lines() >= 4,
        "the rows are in the history"
    );
    for (r, row) in live.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            assert_eq!(
                &cmd_cell(&term, &format!("abs={r} {c}")),
                cell,
                "history row {r}, column {c}"
            );
        }
    }
    let far: String = (0..6000).map(|i| format!("{i}\r\n")).collect();
    term.lock().unwrap().process(far.as_bytes());
    for (r, row) in live.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            assert_eq!(
                &cmd_cell(&term, &format!("abs={r} {c}")),
                cell,
                "stored history row {r}, column {c}"
            );
        }
    }
    assert!(live[0][0].starts_with("OK > 5c5cff "), "{}", live[0][0]);
    assert!(live[0][0].ends_with(" bold\n"), "{}", live[0][0]);
    assert_ne!(live[3][0], live[0][0], "the control: the plain row");
    assert_eq!(cmd_cell(&term, "abs=100000 0"), "ERR out of range\n");
    assert_eq!(cmd_cell(&term, "abs=x 0"), "ERR bad args\n");
    assert_eq!(cmd_cell(&term, "abs=0 12"), "ERR out of range: cols 0-11\n");
}

#[test]
fn a_cell_past_the_grid_names_the_grid() {
    let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
    assert_eq!(
        cmd_cell(&term, "2 0"),
        "ERR out of range: rows 0-1, cols 0-7\n"
    );
    assert_eq!(
        cmd_cell(&term, "0 8"),
        "ERR out of range: rows 0-1, cols 0-7\n"
    );
    assert!(cmd_cell(&term, "1 7").starts_with("OK"));
}

#[test]
fn sparse_blank_queries_fold_live_defaults_and_decscnm() {
    let term = Arc::new(Mutex::new(Terminal::new(2, 8)));
    {
        let mut t = term_lock(&term);
        t.process(b"\x1b]10;rgb:11/22/33\x07");
        t.process(b"\x1b]11;rgb:44/55/66\x07");
        t.process(b"X");
        assert!(
            t.render_row_at_screen(0).get(7).is_none(),
            "negative control: column 7 is an implicit sparse-tail cell"
        );
    }

    let normal = cmd_cell(&term, "0 7");
    assert!(
        normal.contains("112233 445566"),
        "normal implicit blank uses live OSC defaults: {normal:?}"
    );
    {
        let t = term_lock(&term);
        let snap = gather_styled_frame(&t);
        assert_eq!(snap.cells[0].cells[0].cell.fg, [0x11, 0x22, 0x33]);
        assert_eq!(snap.cells[0].cells[0].cell.bg, [0x44, 0x55, 0x66]);
        assert_eq!(
            snap.cells[0].cells[7].cell.fg,
            snap.cells[0].cells[0].cell.fg
        );
        assert_eq!(
            snap.cells[0].cells[7].cell.bg, snap.cells[0].cells[0].cell.bg,
            "materialized and implicit default-colour cells agree"
        );
    }

    term_lock(&term).process(b"\x1b[?5h");
    let reversed = cmd_cell(&term, "0 7");
    assert!(
        reversed.contains("445566 112233"),
        "DECSCNM swaps the implicit blank exactly like painted cells: {reversed:?}"
    );
    {
        let t = term_lock(&term);
        let snap = gather_styled_frame(&t);
        assert_eq!(snap.cells[0].cells[0].cell.fg, [0x44, 0x55, 0x66]);
        assert_eq!(snap.cells[0].cells[0].cell.bg, [0x11, 0x22, 0x33]);
        assert_eq!(
            snap.cells[0].cells[7].cell.fg,
            snap.cells[0].cells[0].cell.fg
        );
        assert_eq!(
            snap.cells[0].cells[7].cell.bg,
            snap.cells[0].cells[0].cell.bg
        );
    }

    let configured = {
        let fresh = Terminal::new(2, 8);
        (fresh.default_foreground(), fresh.default_background())
    };
    let mut t = term_lock(&term);
    t.process(b"\x1b[?5l\x1b]110\x07\x1b]111\x07");
    let snap = gather_styled_frame(&t);
    assert_eq!(
        snap.cells[0].cells[7].cell.fg,
        [configured.0.r, configured.0.g, configured.0.b]
    );
    assert_eq!(
        snap.cells[0].cells[7].cell.bg,
        [configured.1.r, configured.1.g, configured.1.b]
    );
}

#[test]
fn styled_frame_glyphs_are_cell_grapheme_across_a_mixed_row() {
    let mut term = Terminal::new(2, 12);
    term.process("ab \u{754C}e\u{0301}".as_bytes());
    let snap = gather_styled_frame(&term);

    assert_eq!(snap.glyph(0, 0), "a");
    assert_eq!(snap.glyph(0, 1), "b");
    assert_eq!(snap.glyph(0, 2), " ", "a written blank is a SPACE");
    assert_eq!(snap.glyph(0, 3), "\u{754C}", "the wide LEAD owns the glyph");
    assert_eq!(snap.glyph(0, 4), "", "the wide continuation owns nothing");
    assert_eq!(
        snap.glyph(0, 5),
        "e\u{0301}",
        "combining mark kept with base"
    );
    assert_eq!(snap.glyph(0, 6), " ", "the untouched tail is blank cells");

    for r in 0..snap.rows {
        for c in 0..snap.cols {
            assert_eq!(
                snap.glyph(r, c),
                term.cell_grapheme(r, c).expect("in-range cell"),
                "snapshot glyph at ({r},{c})"
            );
        }
    }

    let frame = styled_frame_payload(&term);
    let wire: Vec<&str> = frame
        .match_indices("\"glyph\":\"")
        .map(|(at, key)| {
            let rest = &frame[at + key.len()..];
            let end = rest.find("\",\"fg\"").expect("glyph field precedes fg");
            &rest[..end]
        })
        .collect();
    assert_eq!(
        wire.len(),
        snap.rows * snap.cols,
        "one glyph field per cell, no trim"
    );
    for (i, got) in wire.iter().enumerate() {
        let (r, c) = (i / snap.cols, i % snap.cols);
        assert_eq!(
            *got,
            term.cell_grapheme(r, c).expect("in-range cell"),
            "wire glyph at ({r},{c}) of {frame}"
        );
    }
}

#[test]
fn styled_frame_selection_is_typed_and_side_adjusted() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    let mut simple = Terminal::new(3, 10);
    {
        let sel = simple.text_selection_mut();
        sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(2, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    let simple_frame = styled_frame_payload(&simple);
    assert!(
        simple_frame.contains(
            "\"selection\":{\"start_row\":0,\"start_col\":1,\"end_row\":2,\
             \"end_col\":4,\"kind\":\"simple\",\"is_block\":false}"
        ),
        "linear selection kind/geometry must survive: {simple_frame}"
    );

    let mut block = Terminal::new(3, 10);
    {
        let sel = block.text_selection_mut();
        sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Block);
        sel.update_selection(2, 4, SelectionSide::Right);
        sel.complete_selection();
    }
    let block_frame = styled_frame_payload(&block);
    assert!(
        block_frame.contains(
            "\"selection\":{\"start_row\":0,\"start_col\":1,\"end_row\":2,\
             \"end_col\":4,\"kind\":\"block\",\"is_block\":true}"
        ),
        "rectangular selection kind must not collapse into linear: {block_frame}"
    );
    assert_ne!(
        simple_frame, block_frame,
        "negative control: identical raw endpoints with different paint geometry"
    );

    let mut sided = Terminal::new(2, 10);
    {
        let sel = sided.text_selection_mut();
        sel.start_selection(0, 1, SelectionSide::Right, SelectionType::Simple);
        sel.update_selection(0, 5, SelectionSide::Left);
        sel.complete_selection();
    }
    let sided_frame = styled_frame_payload(&sided);
    assert!(
        sided_frame.contains(
            "\"selection\":{\"start_row\":0,\"start_col\":2,\"end_row\":0,\
             \"end_col\":4,\"kind\":\"simple\",\"is_block\":false}"
        ),
        "half-cell sides must project to the renderer's 2..=4 range: {sided_frame}"
    );
    assert!(
        !sided_frame.contains(
            "\"selection\":{\"start_row\":0,\"start_col\":1,\"end_row\":0,\
             \"end_col\":5"
        ),
        "negative control: raw one-cell-too-wide bounds must not leak"
    );
}

#[test]
fn styled_frame_carries_live_selection_and_cursor_colors() {
    let mut term = Terminal::new(2, 8);
    let defaults = styled_frame_payload(&term);
    assert!(
        defaults.contains("\"cursor\":{")
            && defaults.contains("\"color\":\"default\"")
            && defaults.contains("\"selection_bg\":\"default\"")
            && defaults.contains("\"selection_fg\":\"dynamic\""),
        "default/dynamic overlay policies must be explicit: {defaults}"
    );

    term.process(
        b"\x1b]12;rgb:ab/cd/ef\x07\
          \x1b]17;rgb:12/34/56\x07\
          \x1b]19;rgb:fe/dc/ba\x07",
    );
    let fixed = styled_frame_payload(&term);
    assert!(
        fixed.contains("\"color\":\"abcdef\""),
        "OSC 12 cursor color lost: {fixed}"
    );
    assert!(
        fixed.contains("\"selection_bg\":\"123456\""),
        "OSC 17 selection background lost: {fixed}"
    );
    assert!(
        fixed.contains("\"selection_fg\":\"fedcba\""),
        "OSC 19 selection foreground lost: {fixed}"
    );

    term.process(b"\x1b]112\x07\x1b]117\x07\x1b]119\x07");
    let reset = styled_frame_payload(&term);
    assert!(
        reset.contains("\"color\":\"default\"")
            && reset.contains("\"selection_bg\":\"default\"")
            && reset.contains("\"selection_fg\":\"dynamic\""),
        "OSC 112/117/119 reset policies must survive: {reset}"
    );
}

fn builds(term: &Arc<Mutex<Terminal>>) -> (u64, u64) {
    let t = term_lock(term);
    (t.search_index_rebuilds(), t.search_index_refreshes())
}

#[test]
fn repeat_search_stable_and_write_invalidates() {
    let term = term_with(&["NEEDLE_alpha"]);
    for i in 0..20 {
        term.lock()
            .unwrap()
            .process(format!("filler {i}\r\n").as_bytes());
    }
    let r1 = cmd_search(&term, "NEEDLE_alpha", &RewrapGauge::default());
    assert!(r1.starts_with("OK 1"), "scrolled-off needle found: {r1}");
    let built = builds(&term);
    let r2 = cmd_search(&term, "NEEDLE_alpha", &RewrapGauge::default());
    assert_eq!(r1, r2, "cache-hit repeat must be byte-identical");
    assert!(cmd_search(&term, "NEEDLE_beta", &RewrapGauge::default()).starts_with("OK 0"));
    assert_eq!(
        builds(&term),
        built,
        "unchanged content is served from the slot"
    );
    term.lock().unwrap().process(b"NEEDLE_beta\r\n");
    let rb = cmd_search(&term, "NEEDLE_beta", &RewrapGauge::default());
    assert!(rb.starts_with("OK 1"), "post-write needle found: {rb}");
    assert_eq!(
        builds(&term),
        (built.0 + 1, built.1 + 1),
        "ordinary output refreshes the kept index"
    );
    let after = cmd_search(&term, "NEEDLE_alpha", &RewrapGauge::default());
    let (before_head, before_hits) = r1.split_once('\n').expect("header");
    let (after_head, after_hits) = after.split_once('\n').expect("header");
    assert!(before_head.ends_with(" screen_first=18"), "{r1}");
    assert!(after_head.ends_with(" screen_first=19"), "{after}");
    assert_eq!(
        search_header_without_frame(&after),
        search_header_without_frame(&r1)
    );
    assert_eq!(
        after_hits, before_hits,
        "absolute row of the old needle is stable across the refresh"
    );
    term.lock().unwrap().process(b"NEEDLE_gamma\r\n");
    let mut plan =
        term_lock(&term).search_plan(&super::search_request(super::search_max_lines(), None));
    term_lock(&term).read_search_chunk(&mut plan, 1);
    let named = plan.rows_to_read() + 1;
    assert!(
        named <= 6,
        "{named} rows: ordinary output refreshes the four visible rows, at most one \
         appended row, and the one row below the boundary that a soft-wrap run could \
         have reached into it from"
    );
}

#[test]
fn incremental_refresh_captures_an_edited_row_that_scrolled_away() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 40)));
    term.lock().unwrap().process(b"OLD_VISIBLE_TOKEN");
    assert!(cmd_search(&term, "OLD_VISIBLE_TOKEN", &RewrapGauge::default()).starts_with("OK 1"));

    term.lock()
        .unwrap()
        .process(b"\x1b[H\x1b[2KNEW_HISTORY_TOKEN\x1b[4;1H\r\n");

    assert!(cmd_search(&term, "NEW_HISTORY_TOKEN", &RewrapGauge::default()).starts_with("OK 1"));
    assert!(cmd_search(&term, "OLD_VISIBLE_TOKEN", &RewrapGauge::default()).starts_with("OK 0"));
}

#[test]
fn incremental_refresh_re_reads_rows_a_rows_grow_revealed_and_output_rewrote() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
    for i in 0..10 {
        term.lock().unwrap().process(format!("L{i}\r\n").as_bytes());
    }
    assert!(
        cmd_search(&term, "L4", &RewrapGauge::default()).starts_with("OK 1"),
        "primed"
    );

    term.lock().unwrap().resize(8, 20);
    assert_eq!(term.lock().unwrap().grid().scrollback_lines(), 3);
    let mut repaint = b"\x1b[HL7\x1b[K\r\nL8\x1b[K\r\nL9\x1b[K".to_vec();
    repaint.extend_from_slice(&b"\r\n\x1b[K".repeat(5));
    repaint.extend_from_slice(b"\x1b[4;1H");
    term.lock().unwrap().process(&repaint);

    assert!(
        cmd_search(&term, "L4", &RewrapGauge::default()).starts_with("OK 0"),
        "a line the repaint overwrote is gone"
    );
    let hit = cmd_search(&term, "L9", &RewrapGauge::default());
    let mut lines = hit.lines();
    assert_eq!(lines.next(), Some("OK 1 screen_first=3"), "{hit}");
    let row: u64 = lines
        .next()
        .and_then(|l| l.split(' ').next())
        .and_then(|r| r.parse().ok())
        .expect("a match row");
    assert_eq!(
        row_text(&term, row),
        "L9",
        "reported on the row it is printed on"
    );
}

#[test]
fn find_measures_ambiguous_characters_as_the_grid_draws_them() {
    let wide = aterm_core::config::TerminalConfig {
        ambiguous_width_double: true,
        ..Default::default()
    };
    let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
    term.lock().unwrap().apply_config(&wide);
    term.lock().unwrap().process("→→foo bar".as_bytes());
    assert_eq!(
        cmd_search(&term, "foo", &RewrapGauge::default()),
        "OK 1 screen_first=0\n0 4 3\n"
    );

    let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
    term.lock().unwrap().apply_config(&wide);
    term.lock().unwrap().process("→→→→abfoo".as_bytes());
    assert!(
        term.lock().unwrap().row_continues_previous(1),
        "precondition: `foo` wrapped"
    );
    assert_eq!(
        cmd_search(&term, "abfoo", &RewrapGauge::default()),
        "OK 1 screen_first=0\n0 8 5\n"
    );
}

#[test]
fn search_reports_a_hit_where_the_grid_drew_it() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
    term.lock()
        .unwrap()
        .process("\x1b[1;3H\u{1f1e7}\x1b[1;1H\u{1f1e6}\x1b[1;5HXYZ".as_bytes());
    assert_eq!(
        term.lock().unwrap().cell_grapheme(0, 4).as_deref(),
        Some("X"),
        "precondition: the grid draws X in column 4"
    );
    assert_eq!(
        cmd_search(&term, "XYZ", &RewrapGauge::default()),
        "OK 1 screen_first=0\n0 4 3\n"
    );
}

#[test]
fn a_live_ambiguous_width_switch_is_not_served_from_the_find_cache() {
    let wide = aterm_core::config::TerminalConfig {
        ambiguous_width_double: true,
        ..Default::default()
    };
    let cached = Arc::new(Mutex::new(Terminal::new(4, 20)));
    cached.lock().unwrap().process("→→foo bar".as_bytes());
    let narrow = cmd_search(&cached, "foo", &RewrapGauge::default());
    cached.lock().unwrap().apply_config(&wide);
    let after_switch = cmd_search(&cached, "foo", &RewrapGauge::default());

    let fresh = Arc::new(Mutex::new(Terminal::new(4, 20)));
    fresh.lock().unwrap().process("→→foo bar".as_bytes());
    fresh.lock().unwrap().apply_config(&wide);
    let fresh = cmd_search(&fresh, "foo", &RewrapGauge::default());
    assert_eq!(
        fresh, "OK 1 screen_first=0\n0 2 3\n",
        "where the grid paints it"
    );
    assert_eq!(narrow, fresh, "a row keeps the cells it was drawn in");
    assert_eq!(after_switch, fresh);
}

#[test]
fn a_lines_hits_do_not_change_when_it_scrolls_into_history() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
    term.lock()
        .unwrap()
        .process(b"foo   \r\nbar \x1b[41m  \x1b[0m\r\nbaz\r\nfoo");
    let queries = ["foo$ regex", "bar$ regex", "foo {2} regex", "bar {3} regex"];
    let ask = |q: &str| cmd_search(&term, q, &RewrapGauge::default());
    let on_screen: Vec<String> = queries.iter().map(|q| ask(q)).collect();
    assert_eq!(
        on_screen[0], "OK 1 screen_first=0\n3 0 3\n",
        "only the line ending at `foo`"
    );
    assert_eq!(
        on_screen[2], "OK 1 screen_first=0\n0 0 5\n",
        "the printed spaces are text"
    );
    term.lock().unwrap().process(b"\r\n1\r\n2\r\n3\r\n4");
    let in_history: Vec<String> = queries.iter().map(|q| ask(q)).collect();
    for (before, after) in on_screen.iter().zip(&in_history) {
        let (before_head, before_rows) = before.split_once('\n').expect("header");
        let (after_head, after_rows) = after.split_once('\n').expect("header");
        assert!(before_head.ends_with(" screen_first=0"), "{before}");
        assert!(after_head.ends_with(" screen_first=4"), "{after}");
        assert_eq!(
            before_head
                .split_once(" screen_first=")
                .map(|(head, _)| head),
            after_head
                .split_once(" screen_first=")
                .map(|(head, _)| head),
        );
        assert_eq!(after_rows, before_rows, "absolute hit coordinates stay put");
    }
}

fn row_text(term: &Arc<Mutex<Terminal>>, abs: u64) -> String {
    use aterm_core::terminal::AbsRowText;
    let row = term_lock(term).abs_row_text_within(abs, usize::MAX);
    match row {
        AbsRowText::Text(s) => s,
        AbsRowText::Evicted | AbsRowText::OutOfRange | AbsRowText::Unreadable => {
            "<evicted>".to_string()
        }
    }
}

#[test]
fn a_blank_row_inside_a_soft_wrap_run_still_holds_its_width_of_the_line() {
    for (shape, text, erase, blank, tail, hit) in [
        (
            "the middle row",
            &b"0123456789ABCDEFGHIJKLMNOPQRST"[..],
            &b"\x1b[2;1H\x1b[2K"[..],
            1,
            "KLMNOPQRST",
            "OK 1 screen_first=0\n2 0 10\n",
        ),
        (
            "the head row",
            &b"0123456789ABCDEFGHIJ"[..],
            &b"\x1b[1;1H\x1b[2K"[..],
            0,
            "ABCDEFGHIJ",
            "OK 1 screen_first=0\n1 0 10\n",
        ),
    ] {
        let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
        term.lock().unwrap().process(text);
        term.lock().unwrap().process(erase);
        assert_eq!(row_text(&term, blank), "", "{shape} really is blank");
        assert_eq!(row_text(&term, blank + 1), tail, "{shape}: printed below");
        assert_eq!(
            cmd_search(&term, tail, &RewrapGauge::default()),
            hit,
            "{shape}: the hit is reported on the row it is printed on"
        );
    }
}

#[test]
fn regex_anchors_bind_to_the_logical_line_not_the_rows_a_soft_wrap_split_it_into() {
    let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
    term.lock().unwrap().process(b"0123456789ABCDEFGHIJ");

    assert_eq!(
        cmd_search(&term, "^ABC regex", &RewrapGauge::default()),
        "OK 0 screen_first=0\n",
        "a continuation row's column 0 is not where the line begins"
    );
    assert_eq!(
        cmd_search(&term, "9$ regex", &RewrapGauge::default()),
        "OK 0 screen_first=0\n",
        "the column the wrap pushed to a row's end is not where the line ends"
    );

    assert_eq!(
        cmd_search(&term, "^0123 regex", &RewrapGauge::default()),
        "OK 1 screen_first=0\n0 0 4\n"
    );
    assert_eq!(
        cmd_search(&term, "J$ regex", &RewrapGauge::default()),
        "OK 1 screen_first=0\n1 9 1\n"
    );
    assert_eq!(
        cmd_search(
            &term,
            "^0123456789ABCDEFGHIJ$ regex",
            &RewrapGauge::default()
        ),
        "OK 1 screen_first=0\n0 0 20\n",
        "anchored end to end, the run is ONE line whose len counts through the wrap"
    );

    term.lock().unwrap().process(b"\x1b[3;1Hxyz");
    assert_eq!(
        cmd_search(&term, "^xyz$ regex", &RewrapGauge::default()),
        "OK 1 screen_first=0\n2 0 3\n"
    );
}

#[test]
fn search_index_is_not_reused_across_terminals() {
    let a = term_with(&["NEEDLE_shared", "pad a"]);
    assert!(cmd_search(&a, "NEEDLE_shared", &RewrapGauge::default()).starts_with("OK 1"));
    let b = term_with(&["nothing here", "pad b"]);
    assert!(
        cmd_search(&b, "NEEDLE_shared", &RewrapGauge::default()).starts_with("OK 0"),
        "terminal B must not see terminal A's cached index"
    );
    assert!(cmd_search(&a, "NEEDLE_shared", &RewrapGauge::default()).starts_with("OK 1"));
}

#[test]
fn alt_screen_swap_invalidates_cache() {
    let term = term_with(&["NEEDLE_main", "pad"]);
    assert!(cmd_search(&term, "NEEDLE_main", &RewrapGauge::default()).starts_with("OK 1"));
    term.lock().unwrap().process(b"\x1b[?1049h");
    assert!(
        cmd_search(&term, "NEEDLE_main", &RewrapGauge::default()).starts_with("OK 0"),
        "alt screen active: main-grid content is not addressable"
    );
    term.lock().unwrap().process(b"\x1b[?1049l");
    assert!(cmd_search(&term, "NEEDLE_main", &RewrapGauge::default()).starts_with("OK 1"));
}

#[test]
fn search_cap_evicts_scrollback_but_floors_at_the_live_screen() {
    let mut lines: Vec<String> = vec!["OLDNEEDLE".to_string()];
    lines.extend((0..40).map(|i| format!("filler {i}")));
    lines.extend(["TOPVIS", "z1", "z2"].map(str::to_string));
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let term = term_with(&refs);
    let full = |needle: &str| {
        super::cmd_search_capped(
            &term,
            needle,
            super::DEFAULT_MAX_CACHED_LINES,
            RewrapReading::default,
        )
    };

    let top = full("TOPVIS");
    let abs: u64 = top
        .lines()
        .nth(1)
        .and_then(|l| l.split(' ').next())
        .and_then(|n| n.parse().ok())
        .expect("match row");
    assert_eq!(abs, term.lock().unwrap().grid().base_y() as u64, "{top}");

    let old = super::cmd_search_capped(&term, "OLDNEEDLE", 1, RewrapReading::default);
    assert!(
        old.starts_with("OK 0") && old.contains("incomplete"),
        "deep-scrollback needle evicted under a tiny cap, honestly flagged: {old}"
    );
    let tiny = super::cmd_search_capped(&term, "TOPVIS", 1, RewrapReading::default);
    assert!(
        tiny.starts_with("OK 1"),
        "the top visible row is still found under a tiny cap (the retained floor): {tiny}"
    );

    let deep = full("OLDNEEDLE");
    assert!(
        deep.starts_with("OK 1") && !deep.contains("incomplete"),
        "raising the cap rebuilds at the new depth, complete: {deep}"
    );
}

#[test]
fn a_clear_ends_the_incomplete_qualifier_instead_of_latching_it() {
    let owned: Vec<String> = (0..200).map(|line| format!("row {line}")).collect();
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let term = term_with(&refs);

    let partial = super::cmd_search_capped(&term, "row 199", 16, RewrapReading::default);
    assert!(
        partial.contains("incomplete"),
        "history deeper than the cap is honestly partial: {partial}"
    );

    term.lock()
        .unwrap()
        .process(b"\x1b[H\x1b[2J\x1b[3JNEEDLE\r\n");

    let exact = super::cmd_search_capped(&term, "NEEDLE", 16, RewrapReading::default);
    assert!(
        exact.starts_with("OK 1"),
        "the post-clear needle is found: {exact}"
    );
    assert!(
        !exact.contains("incomplete"),
        "nothing is left un-searched, so the count is stated plainly: {exact}"
    );
    let found =
        super::search_full_history_capped(&term, "NEEDLE", false, false, 16).expect("search");
    assert!(!found.results.incomplete);
    assert_eq!(found.searched_lines, None, "the whole buffer was searched");
    assert_eq!(
        found.results.lowest_retained_line, 0,
        "an exhaustive index reports no retained-history watermark"
    );
}

fn search_header_without_frame(reply: &str) -> &str {
    let header = reply.lines().next().expect("search header");
    let (head, first) = header.split_once(" screen_first=").expect("search frame");
    first.parse::<u64>().expect("absolute screen row");
    head
}

#[test]
fn the_search_header_names_the_match_limit_and_the_rewrap() {
    let row = "a".repeat(40);
    let rows = super::MAX_SEARCH_MATCHES / 40 + 100;
    let owned: Vec<&str> = (0..rows).map(|_| row.as_str()).collect();
    let term = term_with(&owned);
    let full = super::cmd_search_capped(
        &term,
        "a",
        super::DEFAULT_MAX_CACHED_LINES,
        RewrapReading::default,
    );
    let header = search_header_without_frame(&full);
    assert_eq!(
        header,
        format!("OK {} incomplete capped", super::MAX_SEARCH_MATCHES)
    );
    let found = super::search_full_history_capped(
        &term,
        "a",
        false,
        false,
        super::DEFAULT_MAX_CACHED_LINES,
    )
    .expect("search");
    assert_eq!(
        found.searched_lines, None,
        "the match limit is not a history cut"
    );

    let small = term_with(&["needle"]);
    let cap = super::DEFAULT_MAX_CACHED_LINES;
    let gauge = Arc::new(RewrapGauge::default());
    let held = gauge.begin_job(1);
    let away = super::cmd_search_capped(&small, "missing", cap, || gauge.reading());
    assert_eq!(away, "OK 0 incomplete rewrapping screen_first=0\n");
    held.finish(WaitEnd::Done);
    let home = super::cmd_search_capped(&small, "missing", cap, || gauge.reading());
    assert_eq!(
        home, "OK 0 screen_first=0\n",
        "a job that ended before the search is no overlap"
    );
}

#[test]
fn a_long_soft_wrapped_run_is_searched_and_one_left_unfinished_is_named() {
    let cap = super::DEFAULT_MAX_CACHED_LINES;
    let unit = "function a(b){return b+1};var needle_x=[1,2,3];";
    let len = 1_000_000;
    let mut bundle = unit.repeat(len / unit.len() + 1);
    bundle.truncate(len);
    let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
    term.lock()
        .unwrap()
        .process(format!("$ cat bundle.js\r\n{bundle}\r\n$ echo x=1\r\n").as_bytes());
    let reply = super::cmd_search_capped(&term, "[a-z]+= regex", cap, RewrapReading::default);
    let header = search_header_without_frame(&reply);
    let count: usize = header
        .strip_prefix("OK ")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a complete count, not a refusal: {header}"));
    assert!(count > len / unit.len(), "{header}");

    let long = "x".repeat(70_000);
    let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
    term.lock()
        .unwrap()
        .process(format!("before z\r\n{long}\r\nafter z\r\n").as_bytes());
    let reply = super::cmd_search_capped(
        &term,
        "(?:x?){1020}z case regex",
        cap,
        RewrapReading::default,
    );
    let mut lines = reply.lines();
    assert_eq!(
        search_header_without_frame(lines.next().expect("header")),
        "OK 2 incomplete long-line",
        "{reply}"
    );
    assert_eq!(
        lines.map(|line| line.split(' ').nth(1)).collect::<Vec<_>>(),
        vec![Some("7"), Some("6")],
        "the short lines around the unfinished run are still searched: {reply}"
    );
}

#[test]
fn the_search_header_names_the_depth_cap_beside_another_cause() {
    let header = |reply: &str| search_header_without_frame(reply).to_string();
    let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
    let mut text = String::from("old z\r\n");
    for i in 0..400 {
        text.push_str(&format!("filler {i}\r\n"));
    }
    text.push_str(&"x".repeat(70_000));
    text.push_str("\r\nafter z\r\n");
    term.lock().unwrap().process(text.as_bytes());
    let costly = "(?:x?){1020}z case regex";
    let deep = super::cmd_search_capped(&term, costly, 200, RewrapReading::default);
    assert_eq!(header(&deep), "OK 1 incomplete long-line older", "{deep}");
    let whole = super::cmd_search_capped(
        &term,
        costly,
        super::DEFAULT_MAX_CACHED_LINES,
        RewrapReading::default,
    );
    assert_eq!(header(&whole), "OK 2 incomplete long-line", "{whole}");
    let plain = super::cmd_search_capped(&term, "z case regex", 200, RewrapReading::default);
    assert_eq!(header(&plain), "OK 1 incomplete", "{plain}");

    let row = "a".repeat(40);
    let rows = super::MAX_SEARCH_MATCHES / 40 + 100;
    let owned: Vec<&str> = (0..rows).map(|_| row.as_str()).collect();
    let term = term_with(&owned);
    let cap = super::MAX_SEARCH_MATCHES / 40 + 10;
    let full = super::cmd_search_capped(&term, "a", cap, RewrapReading::default);
    assert_eq!(
        header(&full),
        format!("OK {} incomplete capped older", super::MAX_SEARCH_MATCHES)
    );
}

#[test]
fn a_rewrap_inside_the_search_marks_its_short_list() {
    let mut t = Terminal::with_scrollback(
        4,
        40,
        8,
        aterm_scrollback::Scrollback::new(64, 512, 8_000_000),
    );
    let mut buf = b"NEEDLE\r\n".to_vec();
    for i in 0..200 {
        buf.extend_from_slice(format!("filler {i}\r\n").as_bytes());
    }
    t.process(&buf);
    let term = Arc::new(Mutex::new(t));
    let cap = super::DEFAULT_MAX_CACHED_LINES;
    let gauge = Arc::new(RewrapGauge::default());
    let away = RefCell::new(None);
    let reply = super::cmd_search_capped(&term, "NEEDLE", cap, || {
        let taken = away.borrow_mut().take();
        match taken {
            None => {
                let now = gauge.reading();
                let mut t = term_lock(&term);
                let pending = t
                    .resize_offloading_scrollback(4, 30)
                    .expect("a width change detaches the tiered history");
                let job = gauge.begin_job(pending.lines_total());
                *away.borrow_mut() = Some((pending, job));
                now
            }
            Some((pending, job)) => {
                let next = term_lock(&term).finish_resize_offload(pending.reflow());
                assert!(next.is_none(), "one pass");
                job.finish(WaitEnd::Done);
                gauge.reading()
            }
        }
    });
    assert!(!gauge.reading().running, "idle at both readings");
    assert_eq!(
        search_header_without_frame(&reply),
        "OK 0 incomplete rewrapping"
    );
    assert_eq!(reply.lines().count(), 1, "no matches in detached history");
    let home = super::cmd_search_capped(&term, "NEEDLE", cap, || gauge.reading());
    assert!(
        home.starts_with("OK 1 screen_first="),
        "the re-attached history holds the needle: {home}"
    );
}

#[test]
fn cold_search_snapshot_work_is_bounded_by_retained_cap() {
    let owned: Vec<String> = (0..200).map(|line| format!("row {line}")).collect();
    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let term = term_with(&refs);

    let plan = term_lock(&term).search_plan(&super::search_request(16, None));
    assert_eq!(
        plan.rows_to_read(),
        16,
        "cold copy must never exceed the configured retained suffix"
    );
    drop(plan);
    let found = super::search_full_history_capped(&term, "row 199", false, false, 16)
        .expect("bounded cold search");
    assert!(found.consistent);
    assert!(found.results.incomplete);
    assert!(found.results.lowest_retained_line > 0);
    assert_eq!(
        found.searched_lines,
        Some(16),
        "the find surfaces learn how far it searched"
    );
}

#[test]
fn alternating_terminal_searches_reuse_bounded_per_terminal_cache() {
    for write in [false, true] {
        let first = term_with(&["first needle"]);
        let second = term_with(&["second needle"]);
        super::search_full_history(&first, "needle", false, false).unwrap();
        let built = builds(&first);
        super::search_full_history(&second, "needle", false, false).unwrap();
        if write {
            first.lock().unwrap().process(b"more\r\n");
        }
        let repeat = super::search_full_history(&first, "needle", false, false).unwrap();
        assert!(repeat.consistent);
        assert_eq!(
            builds(&first).0 - built.0,
            u64::from(write),
            "write={write}: switching back to a recent terminal reuses its index"
        );
    }
}

fn kept(term: &Arc<Mutex<Terminal>>) -> bool {
    let request = super::search_request(super::search_max_lines(), None);
    term_lock(term).search_plan(&request).is_hit()
}

#[test]
fn the_ninth_searched_terminal_releases_the_least_recent_index() {
    let cap = super::SEARCH_RESIDENT_CAP;
    for search_oldest_again in [false, true] {
        let terms: Vec<_> = (0..=cap)
            .map(|i| term_with(&[format!("needle {i}").as_str()]))
            .collect();
        let search = |term: &Arc<Mutex<Terminal>>| {
            super::search_full_history(term, "needle", false, false).expect("searched");
        };
        for term in &terms[..cap] {
            search(term);
        }
        if search_oldest_again {
            search(&terms[0]);
        }
        search(&terms[cap]);
        let released = usize::from(search_oldest_again);
        for (i, term) in terms.iter().enumerate() {
            assert_eq!(
                kept(term),
                i != released,
                "search_oldest_again={search_oldest_again}: terminal {i}"
            );
        }
    }
}

#[test]
fn a_dropped_terminal_leaves_the_residency_ledger_uncounted() {
    for drop_first in [false, true] {
        let ledger = super::SearchResidency::new(2);
        let mut first = Some(term_with(&[]));
        let (second, third) = (term_with(&[]), term_with(&[]));
        assert!(ledger.touch(first.as_ref().expect("alive")).is_none());
        assert!(ledger.touch(&second).is_none());
        if drop_first {
            first = None;
        }
        let evicted = ledger.touch(&third);
        assert_eq!(evicted.is_some(), !drop_first, "drop_first={drop_first}");
        if let (Some(evicted), Some(first)) = (&evicted, &first) {
            assert!(Arc::ptr_eq(evicted, first));
        }
    }
}

#[test]
fn find_and_socket_share_one_index() {
    for write in [false, true] {
        let term = term_with(&["needle one", "other", "needle two"]);
        let found = super::search_full_history_direction(
            &term,
            "needle",
            false,
            false,
            super::EngineSearchDirection::Forward,
            Some((1, 0)),
            false,
        )
        .expect("find");
        assert_eq!(found.results.matches.len(), 2);
        let built = builds(&term);
        if write {
            term.lock().unwrap().process(b"needle three\r\n");
        }
        let reply = cmd_search(&term, "needle", &RewrapGauge::default());
        let header = if write { "OK 3 " } else { "OK 2 " };
        assert!(reply.starts_with(header), "write={write}: {reply}");
        assert_eq!(
            builds(&term),
            (built.0 + u64::from(write), built.1 + u64::from(write)),
            "write={write}"
        );
    }
}

#[test]
fn a_search_raced_off_the_lock_reports_the_verdict_under_the_planned_stamps() {
    type Race = fn(&mut Terminal);
    let races: [(&str, Option<Race>, bool); 4] = [
        ("none", None, true),
        ("output", Some(|t| t.process(b"more\r\n")), true),
        ("alt switch", Some(|t| t.process(b"\x1b[?1049h")), false),
        ("width", Some(|t| t.resize(4, 30)), false),
    ];
    let depth = super::search_max_lines();
    for (label, race, frame_stable) in races {
        for (warm, point) in [(false, false), (true, false), (false, true), (true, true)] {
            let case = format!("{label} warm={warm} point={point}");
            let term = term_with(&["needle", "other"]);
            if warm {
                let _ = super::search_full_history(&term, "needle", false, false).expect(&case);
            }
            let planned = super::SearchKey::capture(&term_lock(&term), depth);
            let planned_stamps = (
                i64::try_from(planned.window().visible_base).unwrap(),
                planned.row_revision,
                planned.renumber_epoch,
                planned.content_seq,
                usize::from(planned.cols),
            );
            let request = super::search_request(depth, Some((0, 0)));
            let searched =
                super::search_terminal(&term, &request, crate::ReadOn::Control, |built, hit| {
                    assert_eq!(hit, warm, "{case}: precondition");
                    if let Some(race) = race {
                        race(&mut term_lock(&term));
                    }
                    built.query(
                        "needle",
                        false,
                        false,
                        super::EngineSearchDirection::Forward,
                        false,
                    )
                });
            if point {
                let found = super::full_history_point(super::Searched {
                    answer: searched.answer.map(|(_, point_match)| point_match),
                    key: searched.key,
                    wrap: searched.wrap,
                    verdict: searched.verdict,
                    frame_open: searched.frame_open,
                })
                .expect(&case);
                assert_eq!(found.consistent, race.is_none(), "{case}");
                assert_eq!(
                    (
                        found.base_y,
                        found.absolute_row_revision,
                        found.history_renumber_epoch,
                        found.content_seq,
                        found.cols,
                    ),
                    planned_stamps,
                    "{case}"
                );
                continue;
            }
            let found = super::full_history_search(searched).expect(&case);
            assert_eq!(
                (found.consistent, found.frame_stable),
                (race.is_none(), frame_stable),
                "{case}"
            );
            assert_eq!(
                (
                    found.base_y,
                    found.absolute_row_revision,
                    found.history_renumber_epoch,
                    found.content_seq,
                    found.cols,
                ),
                planned_stamps,
                "{case}"
            );
            let header = format!(
                "OK 1{} screen_first={}",
                if frame_stable {
                    ""
                } else {
                    " incomplete moving"
                },
                planned_stamps.0,
            );
            assert_eq!(
                super::search_reply(Ok(found), false).lines().next(),
                Some(header.as_str()),
                "{case}"
            );
        }
    }
}

#[test]
fn a_point_step_answers_a_hit_as_it_answers_a_build() {
    let costly = "(?:x?){1020}z";
    let term = Arc::new(Mutex::new(Terminal::new(4, 500)));
    term.lock()
        .unwrap()
        .process(format!("before\r\n{}\r\nafter\r\n", "x".repeat(70_000)).as_bytes());
    let point = || {
        super::search_full_history_point(
            &term,
            costly,
            true,
            true,
            super::EngineSearchDirection::Forward,
            (0, 0),
            true,
        )
        .map(|found| (found.point_match, found.consistent))
    };
    let built = point();
    assert_eq!(builds(&term).0, 1, "precondition: a build");
    let hit = point();
    assert_eq!(builds(&term).0, 1, "precondition: a hit");
    assert_eq!(built, Ok((None, true)));
    assert_eq!(hit, built);

    let request = super::search_request(super::search_max_lines(), Some((0, 0)));
    let walked = super::search_terminal(&term, &request, crate::ReadOn::Control, |built, hit| {
        assert!(hit, "precondition: a hit");
        built.index().find_direction_opts(
            costly,
            true,
            true,
            aterm_search::DirectedFind {
                anchor: built.wrap().logical_anchor().unwrap_or((0, 0)),
                direction: super::EngineSearchDirection::Forward,
                inclusive: false,
                wrap: true,
            },
        )
    });
    assert_eq!(walked.answer, Err(super::SearchOptionsError::ScanCutShort));
}

#[test]
fn metrics_refuses_an_unknown_argument_and_resets_nothing() {
    crate::metrics::record_offscreen_raster(1_000_000);
    for arg in ["rest", "percentile", "reset now", "percentiles reset"] {
        assert_eq!(
            super::cmd_metrics(None, arg),
            super::METRICS_USAGE,
            "{arg:?}"
        );
        assert_eq!(
            super::cmd_metrics_json(None, arg),
            super::METRICS_USAGE,
            "{arg:?}"
        );
    }
    let summary = super::cmd_metrics(None, "");
    let rasters: u64 = summary
        .split("offscreen_rasters=")
        .nth(1)
        .and_then(|t| t.split_whitespace().next())
        .and_then(|t| t.parse().ok())
        .expect("offscreen_rasters is a number");
    assert!(rasters > 0, "a refused argument reset the counters");
}

#[test]
fn each_metrics_form_reaches_the_wire_through_its_own_emitter_and_sink() {
    use crate::metrics::wire::{ReadCtx, law_tests};
    let ctx = ReadCtx::now();
    let forms = [
        (
            "",
            law_tests::body(|s| crate::metrics::emit_summary(&ctx, None, s)).keys,
        ),
        (
            "percentiles",
            law_tests::body(|s| crate::metrics::emit_percentiles(&ctx, s)).keys,
        ),
    ];
    assert_ne!(forms[0].1, forms[1].1);
    for (form, keys) in &forms {
        let reply = super::cmd_metrics(None, form);
        let line = reply
            .strip_suffix('\n')
            .filter(|line| line.starts_with("OK ") && !line.contains('\n'))
            .unwrap_or_else(|| panic!("{form:?}: not one OK line: {reply:?}"));
        let text_keys: Vec<&str> = law_tests::tokens(line).iter().map(|(k, _)| *k).collect();
        assert_eq!(text_keys, *keys, "{form:?}: {line}");
        let flagged = format!("{form} --json");
        let resolved =
            aterm_types::control_verbs::spec_for("metrics", &flagged).expect("metrics is a verb");
        let body = match super::super::json_ask(&resolved, "metrics", &flagged) {
            super::super::JsonAsk::Emit(body) => body,
            other => panic!("{form:?}: the dispatch emits no JSON: {other:?}"),
        };
        for reply in [
            super::cmd_metrics_json(None, &body),
            super::cmd_metrics_json(None, form),
        ] {
            let object = reply
                .strip_prefix("OK 1\n")
                .and_then(|rest| rest.strip_suffix('\n'))
                .filter(|object| object.starts_with('{') && !object.contains('\n'))
                .unwrap_or_else(|| panic!("{form:?}: not one framed object: {reply:?}"));
            let json_keys: Vec<String> = law_tests::members(object)
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            assert_eq!(json_keys, *keys, "{form:?}: {object}");
        }
    }
}

/// Plain `metrics` carries each of Commons repair's counts for this process
/// (`aterm_db::runtime::cadence`), a whole number, where Commons repair runs (unix).
///
/// NEGATIVE CONTROL (2026-11-12): with the summary leaving its Commons counts out, the reply
/// named none (`commons_rounds_out: None`).
#[cfg(unix)]
#[test]
fn metrics_carries_commons_repairs_counts() {
    use crate::metrics::wire::law_tests;
    let reply = super::cmd_metrics(None, "");
    let tokens = law_tests::tokens(reply.trim_end());
    for key in [
        "commons_rounds_out",
        "commons_rounds_in",
        "commons_rounds_quiet",
        "commons_owner_ticks",
        "commons_beacons",
        "commons_announces",
        "commons_signature_checks",
        "commons_deadline_spawns",
    ] {
        let value = tokens.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        assert!(
            value.is_some_and(|value| value.parse::<u64>().is_ok()),
            "{key}: {value:?}"
        );
    }
}

#[test]
fn the_per_owner_ledgers_are_self_labelling_in_both_forms() {
    let reply = super::cmd_metrics_json(None, "");
    let body = reply
        .strip_prefix("OK 1\n")
        .expect("status frame")
        .trim_end();
    let value: aterm_json::Value = aterm_json::from_str(body).expect("valid metrics JSON");
    let text = super::cmd_metrics(None, "");
    for key in [
        "deadline_arms_by_owner",
        "past_arm_streak_heals",
        "frame_refill_full_causes",
    ] {
        let ledger = value
            .get(key)
            .unwrap_or_else(|| panic!("{key} is published"));
        let entries = ledger
            .as_object()
            .unwrap_or_else(|| panic!("{key} is a keyed object: {ledger}"));
        for (name, entry) in entries {
            assert!(!name.is_empty(), "{key}: every entry is named");
            let counts = if key == "deadline_arms_by_owner" {
                vec![entry.get("arms"), entry.get("past")]
            } else {
                vec![Some(entry)]
            };
            for count in counts {
                assert!(
                    count.and_then(aterm_json::Value::as_u64).is_some(),
                    "{key}: {name} publishes its counts: {entry}"
                );
            }
        }
        let prefix = format!("{key}=");
        let field = text
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix(prefix.as_str()))
            .unwrap_or_else(|| panic!("the text form publishes {key}"));
        assert!(!field.is_empty(), "{key}: empty renders as `none`");
    }
    for cause in aterm_core::render::FullRefillCause::ALL {
        let label = cause.as_str();
        assert!(
            !label.contains(',') && !label.contains(':') && !label.contains(char::is_whitespace),
            "`{label}` would break the `a:1,b:2` text encoding"
        );
    }
}

#[test]
fn metrics_remain_available_while_the_terminal_mutex_is_busy() {
    let term = term_with(&[]);
    let _held = term.lock().unwrap();

    let text = super::cmd_metrics(Some(&term), "");
    assert!(
        text.contains("rows=busy cols=busy scrollback_truncated_lines=busy"),
        "text diagnostics identify only the unavailable grid fields: {text}"
    );
    assert!(text.contains("redraw_attempts="));

    let reply = super::cmd_metrics_json(Some(&term), "");
    let body = reply.strip_prefix("OK 1\n").unwrap().trim_end();
    let value: aterm_json::Value = aterm_json::from_str(body).unwrap();
    assert!(value["rows"].is_null());
    assert!(value["cols"].is_null());
    assert!(value["scrollback_truncated_lines"].is_null());
    assert!(value.get("redraw_attempts").is_some());
}

fn cached_batch(
    term: &Arc<Mutex<Terminal>>,
    q: &str,
    direction: super::EngineSearchDirection,
) -> (super::SearchResults, Option<super::SearchMatch>) {
    let request = super::search_request(super::search_max_lines(), None);
    let plan = term_lock(term).search_plan(&request);
    assert!(
        plan.is_hit(),
        "the previous search must have kept its index"
    );
    let built = plan.build();
    let found = aterm_core::terminal::query_search_index(
        built.index(),
        q,
        false,
        false,
        direction,
        None,
        false,
        built.wrap(),
    )
    .expect("literal batch query cannot fail");
    assert!(term_lock(term).finish_search(built).0.consistent);
    found
}

#[test]
fn narrowing_skips_frames_bigger_than_the_engines_own_candidate_set() {
    let mut lines: Vec<String> = (0..40)
        .map(|i| format!("svc-api request completed size=100 seq={i}"))
        .collect();
    lines.push("svc-api request completed size=977 seq=rare".to_string());
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let term = term_with(&refs);
    let _ = take_narrowed_query_steps();

    let oracle = |q: &str| cached_batch(&term, q, super::EngineSearchDirection::Forward).0;
    let narrowed = |q: &str| {
        super::search_full_history_direction(
            &term,
            q,
            false,
            false,
            super::EngineSearchDirection::Forward,
            None,
            false,
        )
        .expect("search ok")
    };

    for q in ["siz", "size", "size="] {
        let full = narrowed(q);
        assert_eq!(full.results, oracle(q), "keystroke {q:?}");
    }
    assert!(
        take_narrowed_query_steps() >= 1,
        "reach: the common-prefix keystrokes must narrow"
    );

    let rare = narrowed("size=9");
    assert_eq!(rare.results, oracle("size=9"));
    assert_eq!(
        rare.results.matches.len(),
        1,
        "precondition: the grown query really is rare"
    );
    assert_eq!(
        take_narrowed_query_steps(),
        0,
        "cost guard: a frame bigger than the engine's candidate set must be skipped"
    );

    let deeper = narrowed("size=97");
    assert_eq!(deeper.results, oracle("size=97"));
    assert_eq!(
        take_narrowed_query_steps(),
        1,
        "the reseeded frame is small, so narrowing resumes immediately"
    );
}

#[test]
fn an_epoch_only_renumber_kills_the_narrowing_frames() {
    let forward = super::EngineSearchDirection::Forward;
    let run = |unscroll: bool| {
        let mut t = Terminal::with_scrollback(
            4,
            40,
            8,
            aterm_scrollback::Scrollback::new(64, 512, 8_000_000),
        );
        for i in 0..40 {
            let line = if i % 2 == 0 {
                format!("alpha-{i}\r\n")
            } else {
                "beta\r\n".to_string()
            };
            t.process(line.as_bytes());
        }
        let term = Arc::new(Mutex::new(t));
        let search = |q: &str| {
            super::search_full_history_direction(&term, q, false, false, forward, None, false)
                .expect("search ok")
        };
        search("alph");
        search("alpha");
        if unscroll {
            let cap = super::search_max_lines();
            let before = super::SearchKey::capture(&term_lock(&term), cap);
            term_lock(&term).process(b"\x1b[1+T");
            let after = super::SearchKey::capture(&term_lock(&term), cap);
            assert_ne!(before.renumber_epoch, after.renumber_epoch);
            let mut unslid = after;
            unslid.renumber_epoch = before.renumber_epoch;
            assert!(
                before.same_generation(&unslid),
                "precondition: the unscroll moves no generation field but the epoch"
            );
        }
        let _ = take_narrowed_query_steps();
        let grown = search("alpha-");
        let steps = take_narrowed_query_steps();
        assert_eq!(grown.results, cached_batch(&term, "alpha-", forward).0);
        assert_eq!(
            grown.results.matches.len(),
            20,
            "every alpha line, at its row after the slide"
        );
        steps
    };
    assert_eq!(
        run(false),
        1,
        "control: the stacked frame narrows the keystroke"
    );
    assert_eq!(run(true), 0, "a frame of pre-shift rows is never reused");
}

#[test]
fn incremental_typing_narrows_and_equals_the_batch_layer() {
    let term = term_with(&[
        "plain ascii find line",
        "a\u{200b}b zero width",
        "stra\u{df}e stop",
        "find find find",
        "no hits here",
    ]);
    let _ = take_narrowed_query_steps();

    let oracle = |q: &str| cached_batch(&term, q, super::EngineSearchDirection::Forward).0;
    let narrowed = |q: &str| {
        super::search_full_history_direction(
            &term,
            q,
            false,
            false,
            super::EngineSearchDirection::Forward,
            None,
            false,
        )
        .expect("search ok")
    };

    for q in ["f", "fi", "fin", "find"] {
        let full = narrowed(q);
        assert!(full.consistent, "no concurrent mutation in this test");
        assert_eq!(
            full.results,
            oracle(q),
            "narrowed keystroke {q:?} must equal the batch layer"
        );
    }
    assert!(
        take_narrowed_query_steps() >= 2,
        "reach: the grown keystrokes must have run off narrowing frames"
    );

    let full = narrowed("fin");
    assert_eq!(full.results, oracle("fin"));
    assert_eq!(
        take_narrowed_query_steps(),
        1,
        "reach: the backspaced query must be served off its own frame"
    );

    let full = narrowed("xfind");
    assert_eq!(full.results, oracle("xfind"));

    let full_zw = narrowed("\u{200b}");
    assert_eq!(full_zw.results, oracle("\u{200b}"));
    assert!(
        full_zw.results.matches.is_empty(),
        "precondition: the zero-width character spans no display column, so \
         it reports no match anywhere in this corpus"
    );
    let full_zwb = narrowed("\u{200b}b");
    assert_eq!(full_zwb.results, oracle("\u{200b}b"));
    assert!(
        full_zwb.results.matches.iter().any(|m| m.line == 1),
        "the zero-width-straddling match on the zero-width line (abs row 1) \
         must survive narrowing"
    );

    let _ = take_narrowed_query_steps();
    for q in ["f", "fi", "fin", "find", "find f"] {
        let full = super::search_full_history_direction(
            &term,
            q,
            false,
            false,
            super::EngineSearchDirection::Backward,
            None,
            false,
        )
        .expect("search ok");
        assert_eq!(
            full.results,
            cached_batch(&term, q, super::EngineSearchDirection::Backward).0,
            "backward keystroke {q:?} must equal the backward batch layer"
        );
    }
    assert!(
        take_narrowed_query_steps() >= 2,
        "reach: the backward lane must also run off narrowing frames"
    );
}
