// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{
    MAX_PASTE_BYTES, PASTE_GESTURE_PROBE_BYTES, PASTE_INSERT_PRICE_BYTES, Terminal,
    paste_char_allowed,
};

fn write_lines(t: &mut Terminal, start: usize, n: usize) {
    for i in start..start + n {
        t.process(format!("line-{i}\r\n").as_bytes());
    }
}

#[test]
fn ring_only_scrollback_limit_governs_retention() {
    let mut t = Terminal::new(5, 20);
    t.set_scrollback_line_limit(Some(100));
    write_lines(&mut t, 0, 500);
    assert_eq!(
        t.grid().scrollback_lines(),
        100,
        "cap applies as lines scroll off"
    );

    t.set_scrollback_line_limit(Some(40));
    assert_eq!(t.grid().scrollback_lines(), 40);
    let oldest = t
        .grid()
        .get_history_line(0)
        .map(|l| l.to_string().trim_end().to_string());
    assert_eq!(
        oldest.as_deref(),
        Some("line-456"),
        "oldest lines evicted, newest kept"
    );

    t.set_scrollback_line_limit(Some(20_000));
    write_lines(&mut t, 500, 11_000);
    assert_eq!(
        t.grid().scrollback_lines(),
        40 + 11_000,
        "a raised limit really lifts the old ring cap"
    );
}

#[test]
fn scrollback_limit_targets_primary_content_never_the_alt_buffer() {
    let mut t = Terminal::new(5, 20);
    write_lines(&mut t, 0, 300);
    t.process(b"\x1b[?1049h"); // enter alt screen (primary saved off)

    t.set_scrollback_line_limit(Some(50));
    assert_eq!(
        t.main_grid().scrollback_lines(),
        50,
        "saved primary truncated through the alt screen"
    );

    write_lines(&mut t, 1000, 30);
    assert_eq!(
        t.grid().scrollback_lines(),
        0,
        "the alt buffer's zero-scrollback ring must not inherit the limit"
    );

    t.process(b"\x1b[?1049l"); // back to primary
    assert_eq!(t.grid().scrollback_lines(), 50);
    assert_eq!(t.scrollback_line_limit(), Some(50));
}

#[test]
fn requested_truncation_is_not_counted_as_loss() {
    let mut t = Terminal::new(5, 20);
    write_lines(&mut t, 0, 500);
    assert_eq!(t.scrollback_truncated_lines(), 0);
    t.set_scrollback_line_limit(Some(10));
    assert_eq!(t.grid().scrollback_lines(), 10);
    assert_eq!(t.scrollback_truncated_lines(), 0);
}

#[test]
fn bracketed_paste_blocks_embedded_terminators() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2004h");
    for (paste, wire) in [
        (
            "safe\x1b[201~rm -rf ~",
            &b"\x1b[200~safe[201~rm -rf ~\x1b[201~"[..],
        ),
        ("a\u{009B}201~b", &b"\x1b[200~a201~b\x1b[201~"[..]),
    ] {
        assert_eq!(term.format_paste(paste), wire, "{paste:?}");
    }
}

#[test]
fn unbracketed_paste_strips_escape_and_c1() {
    let term = Terminal::new(24, 80);
    assert_eq!(term.format_paste("a\x1b[31mb\u{009D}c"), b"a[31mbc");
}

#[test]
fn a_supplied_framing_decides_the_guards_and_nothing_else() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(
        term.format_paste_framed("a\x1b[31mb\nc", true),
        b"\x1b[200~a[31mb\rc\x1b[201~"
    );
    term.process(b"\x1b[?2004h");
    assert_eq!(
        term.format_paste_framed("a\x1b[31mb\nc", false),
        b"a[31mb\rc"
    );
    assert_eq!(
        term.format_paste_framed("a\x1b[31mb\nc", term.modes().bracketed_paste),
        term.format_paste("a\x1b[31mb\nc")
    );
}

#[test]
fn paste_payload_probe_is_the_formatter_sanitizer_without_protocol_wrappers() {
    assert!(!Terminal::paste_has_payload(""));
    assert!(!Terminal::paste_has_payload("\x1b\x03\u{009b}\x7f"));
    assert!(!Terminal::paste_has_payload("\u{0301}\u{fe0f}\u{200d}"));
    assert!(Terminal::paste_has_payload("\t"));
    assert!(Terminal::paste_has_payload("\n"));
    assert!(Terminal::paste_has_payload("中文🙂"));

    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?2004h");
    assert_eq!(term.format_paste(""), b"\x1b[200~\x1b[201~");
    assert!(
        !Terminal::paste_has_payload(""),
        "bracket wrappers are protocol framing, not authored movement"
    );

    let adversarial = "\x1b".repeat(MAX_PASTE_BYTES);
    assert!(
        !Terminal::paste_has_payload(&adversarial),
        "a cap-sized control-only paste stays dark on the bounded probe"
    );
    let hidden = format!("{}x", "\x1b".repeat(PASTE_GESTURE_PROBE_BYTES));
    assert!(
        !Terminal::paste_has_payload(&hidden),
        "content beyond the constant-cost prefix is conservatively unlicensed"
    );
}

/// What a paste keeps is what its formatter sends, line breaks as CRs: a control-only text
/// keeps nothing, so a turn that pasted one says nothing stands.
#[test]
fn a_paste_keeps_what_its_formatter_sends() {
    let term = Terminal::new(24, 80);
    for text in [
        "",
        "\x01\x1b\u{9b}\x7f",
        "a\x01b\tc\r\nd\ne",
        "e\u{301}\x1b[2Jx",
        "中文🙂",
    ] {
        let kept = Terminal::paste_kept_text(text);
        assert_eq!(
            term.format_paste_framed(text, false),
            kept.replace("\r\n", "\r").replace('\n', "\r").into_bytes(),
            "{text:?}"
        );
    }
    assert_eq!(Terminal::paste_kept_text("\x01\x1b\u{9b}\x7f"), "");
    assert_eq!(Terminal::paste_kept_text("a\x01b\tc\r\n"), "ab\tc\r\n");
    let over = "x".repeat(MAX_PASTE_BYTES + 1);
    assert_eq!(Terminal::paste_kept_text(&over).len(), MAX_PASTE_BYTES);
}

#[test]
fn paste_sends_bytes_is_the_formatters_output_for_every_char() {
    let term = Terminal::new(24, 80);
    let mut buf = [0u8; 4];
    let mut kept = 0usize;
    for c in (0..=0x10_ffff_u32).filter_map(char::from_u32) {
        let alone = c.encode_utf8(&mut buf);
        let wrapped = format!("\x1b\u{0085}{alone}\x03");
        for text in [&*alone, &*wrapped] {
            for bracketed in [false, true] {
                assert_eq!(
                    Terminal::paste_sends_bytes(text, bracketed),
                    !term.format_paste_framed(text, bracketed).is_empty(),
                    "{c:?} in {text:?}, bracketed={bracketed}"
                );
            }
        }
        kept += usize::from(paste_char_allowed(c));
    }
    assert!(kept > 1_000_000, "the walk met the kept chars: {kept}");
    let controls: String = (0..=0x9f_u32)
        .filter_map(char::from_u32)
        .filter(|c| !paste_char_allowed(*c))
        .collect();
    assert_eq!(
        controls.chars().count(),
        0x20 + 0x21 - 3,
        "every control but TAB, LF, CR"
    );
    for text in [
        controls.clone(),
        controls.repeat(3),
        "\x1b".repeat(MAX_PASTE_BYTES + 1),
        format!("{}x", "\x1b".repeat(MAX_PASTE_BYTES)),
    ] {
        assert!(
            !Terminal::paste_sends_bytes(&text, false),
            "{:?}",
            &text[..8]
        );
        assert!(Terminal::paste_sends_bytes(&text, true));
    }
    assert!(Terminal::paste_sends_bytes(
        &format!("{}x", "\x1b".repeat(200)),
        false
    ));
}

#[test]
fn unbracketed_paste_strips_c0_signal_bytes() {
    let term = Terminal::new(24, 80);
    assert_eq!(term.format_paste("a\x03b\x04c\x1ad\x08e\x7ff"), b"abcdef");
    assert_eq!(term.format_paste("a\tb\n"), b"a\tb\r");
}

#[test]
fn paste_converts_line_breaks_to_cr() {
    let mut term = Terminal::new(24, 80);
    assert_eq!(term.format_paste("x\r\ny\nz"), b"x\ry\rz");
    term.process(b"\x1b[?2004h");
    assert_eq!(term.format_paste("x\r\ny\nz"), b"\x1b[200~x\ry\rz\x1b[201~");
}

#[test]
fn drain_bell_reflects_real_bel_byte() {
    let mut term = Terminal::new(24, 80);
    assert!(!term.drain_bell(), "no bell before any BEL byte");
    term.process(b"\x07");
    assert!(term.drain_bell(), "BEL byte sets the flag");
    assert!(!term.drain_bell(), "flag is cleared after draining");
}

/// After the host's reset the program's frame is whole, as after a batch that ends with RIS,
/// so the next unfinished DEC 2026 frame's hang clock starts at its own batch, not at the frame
/// before the reset (whose deadline had passed, so a half-drawn frame read as whole).
/// NEGATIVE CONTROL: with `Terminal::reset` leaving `frame_unfinished_since`, `` host reset
/// true: the next frame's own clock `` fails.
#[test]
fn a_reset_restarts_the_unfinished_frames_hang_clock() {
    use crate::terminal::{ClockReading, FRAME_HANG};
    use std::time::Duration;

    let t0 = ClockReading::now();
    let at = |s: u64| ClockReading {
        monotonic: t0.monotonic + Duration::from_secs(s),
        wall_ms: None,
    };
    for host in [false, true] {
        let mut term = Terminal::new(4, 20);
        term.process_at(b"\x1b[?2026hA", at(0));
        assert!(term.frame_unfinished_at(at(0).monotonic), "PRECONDITION");
        if host {
            term.reset();
        } else {
            term.process_at(b"\x1bc", at(1));
        }
        term.process_at(b"\x1b[?2026hB", at(61));
        assert_eq!(
            term.frame_hang_deadline(),
            Some(at(61).monotonic + FRAME_HANG),
            "host reset {host}: the next frame's own clock"
        );
        assert!(
            term.frame_unfinished_at(at(61).monotonic),
            "host reset {host}"
        );
    }
}

/// A full reset (RIS, or the host's `Terminal::reset`) keeps what the program sent before it
/// that a polling host has not drained yet: the bell, and every app event (OSC 7's cwd, the
/// OSC 133 marks with the rows they were made on, D's exit), as `clear` keeps them.
/// NEGATIVE CONTROLS: with `TransientState::reset` clearing `bell_pending` again, `` the BEL
/// before the RIS still drains `` fails; with it clearing `osc_events`, or keeping only the
/// events that name no row, `` RIS: the events before it `` fails.
#[test]
fn a_reset_keeps_what_the_program_sent_before_it() {
    let mut term = Terminal::new(24, 80);
    term.process(b"\x07\x1bc");
    assert_eq!(term.bell_total(), 1, "the BEL counted");
    assert!(term.drain_bell(), "the BEL before the RIS still drains");
    assert!(!term.drain_bell(), "and only once");
    let mut across = Terminal::new(24, 80);
    across.process(b"\x07");
    across.process(b"\x1bc");
    assert!(
        across.drain_bell(),
        "an undrained BEL outlives a RIS in a later batch"
    );
    let mut host = Terminal::new(24, 80);
    host.process(b"\x07");
    host.reset();
    assert!(
        host.drain_bell(),
        "an undrained BEL outlives the host's reset"
    );

    let sent: &[u8] = b"\x1b]7;file:///tmp\x07\x1b]133;A\x07$ \x1b]133;B\x07true\r\n\x1b]133;C\x07\x1b]133;D;0\x07";
    let drain =
        |term: &mut Terminal| std::iter::from_fn(|| term.take_osc_event()).collect::<Vec<_>>();
    let mut unreset = Terminal::new(24, 80);
    unreset.set_osc_event_polling(true);
    unreset.process(sent);
    let all = drain(&mut unreset);
    let has = |code: u32, part: &str| {
        all.iter()
            .any(|(c, payload)| *c == code && payload.contains(part))
    };
    assert!(
        has(7, "/tmp") && has(133, ";row=") && has(133, "D;"),
        "PRECONDITION: a cwd, a mark that names a row and an exit: {all:?}"
    );
    for (case, erase) in [
        ("RIS", Some(&b"\x1bc"[..])),
        ("clear", Some(&b"\x1b[H\x1b[2J\x1b[3J"[..])),
        ("the host's reset", None),
    ] {
        let mut term = Terminal::new(24, 80);
        term.set_osc_event_polling(true);
        match erase {
            Some(bytes) => term.process(&[sent, bytes].concat()),
            None => {
                term.process(sent);
                term.reset();
            }
        }
        assert_eq!(drain(&mut term), all, "{case}: the events before it");
    }
}

#[test]
fn bels_ring_at_most_once_per_hundred_ms() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use crate::terminal::ClockReading;

    let mut term = Terminal::new(24, 80);
    let rang = Arc::new(AtomicU64::new(0));
    let seen = Arc::clone(&rang);
    term.set_bell_callback(move || {
        seen.fetch_add(1, Ordering::Relaxed);
    });
    let t0 = ClockReading::now();
    let at = |ms: u64| ClockReading {
        monotonic: t0.monotonic + Duration::from_millis(ms),
        wall_ms: None,
    };
    term.process_at(b"\x07\x07\x07", at(0));
    term.process_at(b"\x07", at(99));
    assert_eq!(rang.load(Ordering::Relaxed), 1, "a burst is one bell");
    assert_eq!(term.bell_total(), 1);
    term.process_at(b"\x07", at(100));
    assert_eq!(rang.load(Ordering::Relaxed), 2, "a bell 100 ms on rings");
    assert_eq!(term.bell_total(), 2);

    let flood = 10_000;
    let mut rang_at = Vec::new();
    for i in 0..21u64 {
        let before = term.bell_total();
        term.process_at(b"\x07", at(flood + i * 60));
        if term.bell_total() > before {
            rang_at.push(i * 60);
        }
    }
    assert_eq!(
        rang_at,
        (0..=10u64).map(|k| k * 120).collect::<Vec<_>>(),
        "a flood rings once each 100 ms, not once"
    );
    assert_eq!(
        rang.load(Ordering::Relaxed),
        2 + 11,
        "the callback each time"
    );
    assert_eq!(term.bell_total(), 2 + 11);
}

#[test]
fn osc_7_queues_real_cwd_path() {
    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);
    term.process(b"\x1b]7;file://host/home/user/project\x07");
    assert_eq!(term.current_working_directory(), Some("/home/user/project"));
    assert_eq!(
        term.take_osc_event(),
        Some((7, "//host/home/user/project".to_string()))
    );
    assert_eq!(term.take_osc_event(), None, "queue drained");

    term.process(b"\x1b]7;file:///var/log\x07");
    assert_eq!(term.take_osc_event(), Some((7, "/var/log".to_string())));
    term.process(b"\x1b]7;file://localhost/var/tmp\x07");
    assert_eq!(term.take_osc_event(), Some((7, "/var/tmp".to_string())));
}

#[test]
fn osc_133_marks_queue_real_payloads() {
    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);
    term.process(b"\x1b]133;A\x07");
    term.process(b"\x1b]133;B\x07");
    term.process(b"\x1b]133;C\x07");
    term.process(b"\x1b]133;D;42\x07");

    let codes: Vec<(u32, String)> = std::iter::from_fn(|| term.take_osc_event()).collect();
    assert_eq!(codes.len(), 4, "one event per accepted mark");
    assert_eq!(codes[0].0, 133);
    assert!(codes[0].1.starts_with("A;row="), "A carries row/col");
    assert_eq!(codes[3].1, "D;exit=42", "D carries the real exit code");
}

/// One shell prompt's marks, cwd and clipboard write: four OSC 133 marks, an OSC 7 and an
/// OSC 52, each of which a polling host is told of.
fn prompt(i: usize) -> Vec<u8> {
    format!(
        "\x1b]133;A\x07$ \x1b]133;B\x07echo {i}\r\n\x1b]133;C\x07{i}\r\n\x1b]133;D;0\x07\
         \x1b]7;file:///tmp/{i}\x07\x1b]52;c;aGk=\x07"
    )
    .into_bytes()
}

#[test]
fn a_host_that_never_polls_is_queued_no_osc_event() {
    let mut term = Terminal::new(24, 80);
    term.authorize_clipboard_access(crate::terminal::ClipboardAccess::Write);
    for i in 0..300 {
        term.process(&prompt(i));
    }
    assert!(
        !term.has_osc_events(),
        "nobody drains the queue, so it holds nothing"
    );
    assert_eq!(term.transient.osc_events.bytes(), 0);

    term.set_osc_event_polling(true);
    term.process(&prompt(300));
    assert_eq!(
        std::iter::from_fn(|| term.take_osc_event()).count(),
        6,
        "a polling host is told of each"
    );
    term.process(&prompt(301));
    term.set_osc_event_polling(false);
    assert!(
        !term.has_osc_events(),
        "turning polling off drops what was queued"
    );
    term.process(&prompt(302));
    assert!(!term.has_osc_events());
}

/// A region scroll that archives a row splices the absolute rows at every CSI; the queued
/// marks it walks are the ones it can move, so marks left in history cost it nothing.
#[test]
fn a_region_scroll_walks_no_queued_mark_it_cannot_move() {
    use super::super::transient_state::{OscEventQueue, shift_meter};
    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);
    for i in 0..300 {
        term.process(&prompt(i));
    }
    term.process(&b"\r\n".repeat(30));
    let queued = term.transient.osc_events.snapshot();
    assert_eq!(queued.len(), OscEventQueue::MAX_EVENTS, "the queue is full");
    let mut scroll = b"\x1b[1;23r\x1b[23;1H".to_vec();
    for _ in 0..4096 {
        scroll.extend_from_slice(b"\n\x1b[m");
    }
    let history = term.grid().scrollback_lines();
    let _ = shift_meter::take();
    term.process(&scroll);
    assert_eq!(
        term.grid().scrollback_lines(),
        history + 4096,
        "every line spliced"
    );
    assert_eq!(shift_meter::take(), 0, "no mark sat at or past a splice");
    assert_eq!(
        term.transient.osc_events.snapshot(),
        queued,
        "and none moved"
    );

    // A mark on the protected footer row rides it down by every spliced row.
    term.process(b"\x1b[r\x1b[24;1H\x1b]133;A\x07");
    let footer = term
        .transient
        .osc_events
        .snapshot()
        .pop()
        .expect("the footer mark");
    let row = |payload: &str| -> u64 {
        let (_, rest) = payload.split_once(";row=").expect("a row");
        rest.split_once(";col=")
            .expect("a col")
            .0
            .parse()
            .expect("a number")
    };
    term.process(&scroll);
    let moved = term
        .transient
        .osc_events
        .snapshot()
        .pop()
        .expect("the footer mark");
    assert_eq!(row(&moved.1), row(&footer.1) + 4096);
    assert_eq!(
        term.transient.osc_events.snapshot().len(),
        OscEventQueue::MAX_EVENTS
    );
    assert!(term.transient.osc_events.bytes() <= OscEventQueue::MAX_BYTES);
}

#[test]
fn osc_52_flood_is_held_to_the_event_byte_budget() {
    use super::super::transient_state::OscEventQueue;
    let mut term = Terminal::new(24, 80);
    term.set_osc_event_polling(true);
    term.authorize_clipboard_access(crate::terminal::ClipboardAccess::Write);
    let payload = |i: u8| char::from(b'a' + i % 26).to_string().repeat(60_000);
    for i in 0..40 {
        let encoded = aterm_codec::base64::encode(payload(i).as_bytes()).expect("encodes");
        term.process(format!("\x1b]52;c;{encoded}\x07").as_bytes());
    }
    assert!(term.transient.osc_events.bytes() <= OscEventQueue::MAX_BYTES);

    let kept: Vec<_> = std::iter::from_fn(|| term.take_osc_event()).collect();
    let newest = 40 - kept.len();
    assert_eq!(kept.len(), OscEventQueue::MAX_BYTES / 60_000);
    for (offset, (code, text)) in kept.iter().enumerate() {
        assert_eq!(*code, 52);
        assert!(*text == payload(u8::try_from(newest + offset).unwrap()));
    }
}

#[test]
fn paste_insert_cells_prices_the_sanitized_single_line_body() {
    let path = "/Users//example/Desktop/Screenshot\\ 2026-09-10\\ at\\ 14.02.11.png ";
    assert_eq!(
        Terminal::paste_insert_cells(path, false),
        Some(u16::try_from(path.chars().count()).unwrap()),
        "an ASCII path is one cell per character"
    );
    assert_eq!(Terminal::paste_insert_cells("foo bar ", false), Some(8));
    assert_eq!(
        Terminal::paste_insert_cells("中🙂", false),
        Some(4),
        "CJK and emoji are two cells each"
    );
    assert_eq!(
        Terminal::paste_insert_cells("\u{0301}\u{fe0f}\u{200d}", false),
        Some(0),
        "a base-less run of marks advances nothing"
    );
    assert_eq!(
        Terminal::paste_insert_cells("a\x1b\x03b\u{009b}c\x7f", false),
        Some(3),
        "controls are stripped before pricing, as the formatter strips them"
    );
    assert_eq!(Terminal::paste_insert_cells("", false), Some(0));
    assert_eq!(
        Terminal::paste_insert_cells("a\nb", false),
        None,
        "a line break: the landing row is unknowable"
    );
    assert_eq!(Terminal::paste_insert_cells("a\r\nb", false), None);
    assert_eq!(
        Terminal::paste_insert_cells("a\tb", false),
        None,
        "a TAB: the shell's tab stops are unknowable"
    );
    let long = "x".repeat(PASTE_INSERT_PRICE_BYTES + 1);
    assert_eq!(
        Terminal::paste_insert_cells(&long, false),
        None,
        "a body past the probe wraps or is collapsed: unknown width"
    );
    let exact = "x".repeat(PASTE_INSERT_PRICE_BYTES);
    assert_eq!(
        Terminal::paste_insert_cells(&exact, false),
        Some(u16::try_from(PASTE_INSERT_PRICE_BYTES).unwrap())
    );
    assert!(Terminal::paste_insert_cells("±", true) >= Terminal::paste_insert_cells("±", false));
}

#[test]
fn print_anchor_counts_the_pending_wrap_column() {
    let mut t = Terminal::new(4, 10);
    t.process(b"\x1b[?7h");
    t.process(b"123456789");
    let (row, col, seq) = t.print_anchor().expect("printed");
    assert_eq!((row, col), (0, 9), "nine glyphs: the anchor is the cursor");
    assert_eq!(t.cursor().col, 9);
    t.process(b"0");
    let (row, col, seq2) = t.print_anchor().expect("printed");
    assert_eq!(
        (row, col),
        (0, 10),
        "the tenth glyph: the anchor is one past the last column"
    );
    assert_eq!(t.cursor().col, 9, "…while the cursor stays parked on it");
    assert!(t.grid().pending_wrap());
    assert!(seq2 > seq);
    t.process(b"a");
    assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((1, 1)));
    assert_eq!((t.cursor().row, t.cursor().col), (1, 1));
    t.process(b"\x1b[3;1H");
    t.process(b"abcdefghij");
    assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((2, 10)));
    t.process(b"\x1b[4;1H");
    assert!(
        !t.grid().pending_wrap(),
        "a cursor address clears the deferred wrap"
    );
    assert_eq!((t.cursor().row, t.cursor().col), (3, 0));
    assert_eq!(
        t.print_anchor().map(|(r, c, _)| (r, c)),
        Some((2, 10)),
        "the anchor is the last PRINT's, not the cursor's"
    );
}

#[test]
fn print_anchor_glyph_is_the_run_s_last_cell() {
    let mut t = Terminal::new(6, 10);
    assert_eq!(t.print_anchor_glyph(), None, "nothing printed yet");
    t.process(b"p% ab");
    assert_eq!(t.print_anchor_glyph(), Some('b'));
    t.process(b"\x1b[1B\r bck: c_\x1b[A\x1b[3G");
    assert_eq!(
        t.print_anchor().map(|(r, c, _)| (r, c)),
        Some((1, 8)),
        "the run ended one past the fake cursor"
    );
    assert_eq!((t.cursor().row, t.cursor().col), (0, 2));
    assert_eq!(t.print_anchor_glyph(), Some('_'), "…on the fake cursor");
    t.process(b"\x1b[3;1H0123456789");
    assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((2, 10)));
    assert_eq!(t.print_anchor_glyph(), Some('9'));
    t.process("\x1b[4;1H中".as_bytes());
    assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((3, 2)));
    assert_eq!(t.print_anchor_glyph(), Some('\0'));
    t.process(b"\x1b[5;1Hxy\x1b[2K");
    assert_eq!(t.print_anchor_glyph(), Some(' '));
    t.process(b"\x1b[6;1H");
    for i in 0..12 {
        t.process(format!("\r\nline{i}").as_bytes());
    }
    t.process(b"\x1b[6;1Hzz");
    t.scroll_display(3);
    assert!(t.grid().display_offset() > 0, "the viewport scrolled back");
    let mut shown = Vec::new();
    t.row_cols_into(5, &mut shown);
    assert_ne!(
        shown.get(1),
        Some(&'z'),
        "non-vacuity: the viewport shows history on that row"
    );
    assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((5, 2)));
    assert_eq!(t.print_anchor_glyph(), Some('z'));
    t.scroll_display(-3);
    t.resize(3, 10);
    assert_eq!(t.print_anchor().map(|(r, c, _)| (r, c)), Some((5, 2)));
    assert_eq!(t.print_anchor_glyph(), None);
    let mut u = Terminal::new(2, 4);
    u.process(b"ab");
    u.process(b"\x1bc");
    assert_eq!(u.print_anchor(), None);
    assert_eq!(u.print_anchor_glyph(), None);
}

#[test]
fn print_seq_advances_on_every_print_run_and_nothing_else() {
    let mut t = Terminal::new(4, 10);
    let s0 = t.print_seq();
    t.process(b"ab");
    let s1 = t.print_seq();
    assert!(s1 > s0, "a print run advances it");
    t.process(b"\x1b[C\x1b[2;1H\x1b[31m\x07\x1b[6n");
    assert_eq!(
        t.print_seq(),
        s1,
        "a cursor move, an SGR, a bell or a query prints nothing"
    );
    t.process("中".as_bytes());
    assert!(t.print_seq() > s1, "a wide glyph prints");
    let s2 = t.print_seq();
    t.process(b"\x1bc");
    assert_eq!(t.print_anchor(), None, "a reset forgets the anchor");
    assert!(t.print_seq() <= s2, "…and never advances the sequence");
}

/// Reads row 0 of `t` as the echo proof does.
fn echo_row(t: &Terminal, row: u16) -> (Vec<char>, Vec<bool>) {
    let (mut chars, mut visible, mut drawn) = (Vec::new(), Vec::new(), Vec::new());
    t.screen_row_echo_cells_into(row, &mut chars, &mut visible, &mut drawn);
    (chars, visible)
}

#[test]
fn echo_cells_read_the_live_row_and_count_only_shown_ink() {
    let mut t = Terminal::new(4, 12);
    t.process(b"ab\x1b[8mcd\x1b[0m\x1b[34;44mef\x1b[0m\x1b[38;2;1;1;1;40mgh\x1b[0m");
    t.process("中i\u{301}".as_bytes());
    let (chars, visible) = echo_row(&t, 0);
    assert_eq!(chars.len(), 12, "one entry per column");
    assert_eq!(
        &chars[..11],
        &['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', '中', '\0', 'i']
    );
    assert_eq!(
        &visible[..11],
        &[
            true, true, false, false, false, false, false, false, true, true, false
        ],
        "SGR 8, one palette slot on itself, a near-black RGB on black and a combining mark \
         are not shown"
    );
    assert_eq!(
        (chars[11], visible[11]),
        (' ', true),
        "past the row: a shown blank"
    );

    let mut inverse = Terminal::new(2, 6);
    inverse.process(b"\x1b[7mxy\x1b[27m\x1b[7;8mz");
    let (_, visible) = echo_row(&inverse, 0);
    assert_eq!(
        &visible[..3],
        &[true, true, false],
        "inverse shows; inverse SGR 8 does not"
    );

    let mut scrolled = Terminal::new(3, 8);
    for i in 0..6 {
        scrolled.process(format!("line{i}\r\n").as_bytes());
    }
    scrolled.process(b"live");
    scrolled.scroll_display(2);
    assert!(
        scrolled.grid().display_offset() > 0,
        "the viewport scrolled back"
    );
    let mut shown = Vec::new();
    scrolled.row_cols_into(2, &mut shown);
    assert_ne!(
        &shown[..4],
        &['l', 'i', 'v', 'e'],
        "non-vacuity: the viewport shows history"
    );
    assert_eq!(
        &echo_row(&scrolled, 2).0[..4],
        &['l', 'i', 'v', 'e'],
        "the live row"
    );
    assert_eq!(
        echo_row(&scrolled, 3),
        (Vec::new(), Vec::new()),
        "past the screen"
    );
}

#[test]
fn paste_preserves_non_ascii_spaces() {
    let mut t = Terminal::new(4, 80);
    let macos_screenshot =
        "'/Users//example/Desktop/Screenshot 2026-10-19 at 12.17.46\u{202f}PM.png'";
    let expected = macos_screenshot.as_bytes();
    assert_eq!(t.format_paste_framed(macos_screenshot, false), expected);
    assert_eq!(
        t.format_paste_framed(macos_screenshot, true),
        [b"\x1b[200~".as_slice(), expected, b"\x1b[201~".as_slice()].concat()
    );
    assert_eq!(
        Terminal::paste_insert_cells(macos_screenshot, false),
        Some(u16::try_from(macos_screenshot.chars().count()).unwrap())
    );
    t.process(b"\x1b[?2004h");
    assert_eq!(
        t.format_paste("a\u{00a0}b\u{202f}c"),
        "\x1b[200~a\u{00a0}b\u{202f}c\x1b[201~".as_bytes()
    );
}
