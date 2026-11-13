// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use aterm_core::terminal::Terminal;

use super::{
    OFFSCREEN_BAD_SINCE, OFFSCREEN_DEFAULT_MAX, OFFSCREEN_USAGE, OffscreenArgs, OffscreenSince,
    cmd_text, offscreen_args, visible_row,
};

fn cmd_offscreen(term: &Arc<Mutex<Terminal>>, rest: &str) -> String {
    super::cmd_offscreen(term, &crate::frame_memo::FrameSlot::default(), rest)
}

const ROWS: u16 = 8;
const TRANSCRIPT: usize = 6;
const ORIGIN: u64 = 0xA11_0000_0042;

fn row(i: usize) -> String {
    format!("row {i:05} of the transcript")
}

fn frame(start: usize) -> Vec<String> {
    let mut v: Vec<String> = (start..start + TRANSCRIPT).map(row).collect();
    v.push("> type a message".to_string());
    v.push("footer: ready".to_string());
    v
}

fn paint(t: &mut Terminal, rows: &[String]) {
    let mut b = String::from("\x1b[?2026h");
    for (r, text) in rows.iter().enumerate() {
        let _ = write!(b, "\x1b[{};1H{text}\x1b[K", r + 1);
    }
    b.push_str("\x1b[?2026l");
    t.process(b.as_bytes());
}

fn scrolled_engine(frames: usize) -> Terminal {
    let mut t = Terminal::new(ROWS, 40);
    t.set_alt_archive_enabled(true); // whatever the default says here
    t.set_alt_archive_origin(ORIGIN);
    t.process(b"\x1b[?1049h");
    for s in 0..frames {
        paint(&mut t, &frame(s));
    }
    t
}

fn scrolled(frames: usize) -> Arc<Mutex<Terminal>> {
    Arc::new(Mutex::new(scrolled_engine(frames)))
}

fn parse(reply: &str) -> (usize, HashMap<String, String>, Vec<String>) {
    let (header, body) = reply.split_once('\n').expect("a header line");
    let mut toks = header.split_whitespace();
    assert_eq!(toks.next(), Some("OK"), "{reply}");
    let n: usize = toks.next().unwrap().parse().expect("a count");
    let fields = toks
        .map(|t| {
            let (k, v) = t.split_once('=').expect("key=value");
            (k.to_string(), v.to_string())
        })
        .collect();
    let lines: Vec<String> = body.lines().map(str::to_string).collect();
    assert_eq!(
        lines.len(),
        n,
        "OK <n> counts every line that follows: {reply}"
    );
    assert!(body.is_empty() || body.ends_with('\n'), "{reply:?}");
    (n, fields, lines)
}

fn field(fields: &HashMap<String, String>, key: &str) -> u64 {
    fields
        .get(key)
        .unwrap_or_else(|| panic!("no {key}= in {fields:?}"))
        .parse()
        .unwrap()
}

fn rows(from: usize, to: usize) -> Vec<String> {
    (from..to).map(row).collect()
}

#[test]
fn offscreen_args_accept_each_option_once_in_any_order() {
    assert_eq!(offscreen_args(""), Ok(OffscreenArgs::default()));
    assert_eq!(offscreen_args("   "), Ok(OffscreenArgs::default()));
    assert_eq!(
        offscreen_args("screen=1 max=5 tail=2 since=9"),
        Ok(OffscreenArgs {
            since: Some(OffscreenSince {
                origin: None,
                index: 9
            }),
            tail: Some(2),
            max: Some(5),
            screen: true,
        })
    );
    assert_eq!(
        offscreen_args("since=77:0").map(|a| a.since),
        Ok(Some(OffscreenSince {
            origin: Some(77),
            index: 0
        }))
    );
    assert_eq!(
        offscreen_args("since=0").map(|a| a.since.unwrap().index),
        Ok(0)
    );
}

#[test]
fn offscreen_args_refuse_junk_repeats_zeros_and_non_numbers() {
    for bad in [
        "junk",
        "5",
        "--json",
        "since",
        "since=",
        "since=x",
        "since=-1",
        "since=+3",
        "since=1:",
        "since=:1",
        "since=1:2:3",
        "since=a:2",
        "since=1 since=2",
        "tail=0",
        "tail=",
        "tail=-2",
        "tail=+2",
        "tail=2 tail=3",
        "max=0",
        "max=x",
        "max=3 max=3",
        "screen",
        "screen=0",
        "screen=2",
        "screen=1 screen=1",
        "tail=2 junk",
        "since=99999999999999999999999",
    ] {
        assert_eq!(
            offscreen_args(bad),
            Err(OFFSCREEN_USAGE.to_string()),
            "{bad:?}"
        );
        assert_eq!(
            cmd_offscreen(&scrolled(3), bad),
            OFFSCREEN_USAGE,
            "the verb answers the parser's line for {bad:?}"
        );
    }
}

#[test]
fn offscreen_returns_the_rows_the_app_scrolled_off_oldest_first() {
    let term = scrolled(10);
    let reply = cmd_offscreen(&term, "");
    let (n, f, lines) = parse(&reply);
    assert_eq!(n, 9, "{reply}");
    assert_eq!(lines, rows(0, 9));
    assert_eq!(field(&f, "first"), 1);
    assert_eq!(field(&f, "last"), 9);
    assert_eq!(field(&f, "lost"), 0);
    assert_eq!(field(&f, "breaks"), 0);
    assert_eq!(field(&f, "back"), 0);
    assert_eq!(field(&f, "origin"), ORIGIN);
    assert_eq!(field(&f, "alt"), 1);
    let t = term.lock().unwrap();
    assert_eq!(field(&f, "seq"), t.content_seq());
    assert_eq!(field(&f, "epoch"), u64::from(t.alt_archive().epoch()));
    assert!(!f.contains_key("more") && !f.contains_key("screen_rows"));
    assert_eq!(t.grid().scrollback_lines(), 0);
}

#[test]
fn offscreen_since_is_exclusive_and_origin_checked() {
    let term = scrolled(10);
    let (_, f, lines) = parse(&cmd_offscreen(&term, "since=4"));
    assert_eq!(lines, rows(4, 9));
    assert_eq!(field(&f, "first"), 5);
    let (n, f, _) = parse(&cmd_offscreen(&term, "since=9"));
    assert_eq!((n, field(&f, "first"), field(&f, "last")), (0, 10, 9));
    assert_eq!(cmd_offscreen(&term, "since=10"), OFFSCREEN_BAD_SINCE);
    assert_eq!(
        cmd_offscreen(&term, &format!("since={ORIGIN}:4")),
        cmd_offscreen(&term, "since=4")
    );
    assert_eq!(
        cmd_offscreen(&term, &format!("since={ORIGIN}:10")),
        OFFSCREEN_BAD_SINCE
    );
    for other in ["7:4", "7:10", "7:999999"] {
        let (_, f, lines) = parse(&cmd_offscreen(&term, &format!("since={other}")));
        assert_eq!(lines, rows(0, 9), "{other} reads from the start");
        assert_eq!(
            field(&f, "origin"),
            ORIGIN,
            "and says whose archive it read"
        );
    }
    let empty = scrolled(1);
    let (n, f, _) = parse(&cmd_offscreen(&empty, "since=0"));
    assert_eq!((n, field(&f, "last"), field(&f, "first")), (0, 0, 1));
    assert_eq!(cmd_offscreen(&empty, "since=1"), OFFSCREEN_BAD_SINCE);
}

#[test]
fn offscreen_pages_with_max_and_tails_with_tail() {
    let term = scrolled(10);
    let (_, f, lines) = parse(&cmd_offscreen(&term, "tail=3"));
    assert_eq!(lines, rows(6, 9));
    assert_eq!(field(&f, "first"), 7);
    assert_eq!(f.get("more").map(String::as_str), Some("1"));
    let (_, f, lines) = parse(&cmd_offscreen(&term, "tail=3 since=7"));
    assert_eq!(lines, rows(7, 9), "tail never reaches back past since");
    assert!(!f.contains_key("more"));
    let (_, _, lines) = parse(&cmd_offscreen(&term, "max=2 tail=3"));
    assert_eq!(lines, rows(7, 9), "the newest min(tail, max)");
    let (_, f, lines) = parse(&cmd_offscreen(&term, "tail=50"));
    assert_eq!(lines, rows(0, 9));
    assert!(!f.contains_key("more"));
}

#[test]
fn offscreen_caps_a_reply_at_the_default_page() {
    let extra = 5;
    let term = scrolled(OFFSCREEN_DEFAULT_MAX + extra + 1);
    let (n, f, lines) = parse(&cmd_offscreen(&term, ""));
    assert_eq!(n, OFFSCREEN_DEFAULT_MAX);
    assert_eq!(f.get("more").map(String::as_str), Some("1"));
    assert_eq!(lines.first(), Some(&row(0)));
    let page_last = field(&f, "first") + n as u64 - 1;
    let (n, f, lines) = parse(&cmd_offscreen(&term, &format!("since={page_last}")));
    assert_eq!(n, extra);
    assert!(!f.contains_key("more"));
    assert_eq!(
        lines,
        rows(OFFSCREEN_DEFAULT_MAX, OFFSCREEN_DEFAULT_MAX + extra)
    );
}

#[test]
fn offscreen_counts_evicted_rows_as_lost() {
    let mut t = scrolled_engine(1);
    t.set_alt_archive_budget(10 * aterm_core::terminal::alt_archive_row_charge(row(0).len()));
    for s in 1..31 {
        paint(&mut t, &frame(s));
    }
    let term = Arc::new(Mutex::new(t));
    let (n, f, lines) = parse(&cmd_offscreen(&term, "since=0"));
    let first = field(&f, "first");
    assert_eq!(field(&f, "last"), 30);
    assert!(first > 1 && n < 30, "some rows were evicted: {f:?}");
    assert_eq!(
        field(&f, "lost"),
        first - 1,
        "every row before first is lost"
    );
    assert_eq!(lines.first(), Some(&row(first as usize - 1)));
    let (_, f, _) = parse(&cmd_offscreen(&term, &format!("since={}", first - 1)));
    assert_eq!(field(&f, "lost"), 0, "nothing after since was evicted");
    let (_, f, _) = parse(&cmd_offscreen(&term, "since=3"));
    assert_eq!(field(&f, "lost"), first - 4);
}

#[test]
fn offscreen_reports_breaks_alt_and_back() {
    let mut t = scrolled_engine(10);
    paint(&mut t, &frame(8)); // the app scrolled BACK one row: row 8 is on screen again
    let term = Arc::new(Mutex::new(t));
    let (_, f, _) = parse(&cmd_offscreen(&term, ""));
    assert_eq!(field(&f, "back"), 1, "{f:?}");
    assert_eq!(field(&f, "last"), 9, "scrolling back archives nothing");
    term.lock().unwrap().process(b"\x1b[?1049l");
    let (_, f, _) = parse(&cmd_offscreen(&term, ""));
    assert_eq!(field(&f, "alt"), 0);
    assert_eq!(field(&f, "breaks"), 1, "leaving the alt screen is a break");
    let last = field(&f, "last");
    let (_, f, _) = parse(&cmd_offscreen(&term, &format!("since={last}")));
    assert_eq!(
        field(&f, "breaks"),
        1,
        "a break after last: the screen does not continue"
    );
}

#[test]
fn offscreen_screen_appends_the_live_screen_under_the_same_count() {
    let term = scrolled(10);
    let (n, f, lines) = parse(&cmd_offscreen(&term, "since=6 screen=1"));
    let m = field(&f, "screen_rows") as usize;
    assert_eq!(m, usize::from(ROWS));
    assert_eq!(n, 3 + m);
    assert_eq!(lines[..3], rows(6, 9)[..]);
    let text = cmd_text(&term);
    let screen: Vec<&str> = text.lines().skip(1).collect();
    assert_eq!(
        lines[3..].iter().map(String::as_str).collect::<Vec<_>>(),
        screen
    );
    assert_eq!(lines[3], row(9), "the screen continues the archive");
    let (n, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
    assert_eq!((n, field(&f, "screen_rows")), (m, m as u64));
}

#[test]
fn offscreen_screen_says_when_the_frame_is_open() {
    let term = scrolled(10);
    term.lock()
        .unwrap()
        .process(b"\x1b[?2026h\x1b[2;1Hrow half-dra");
    let (_, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
    assert_eq!(f.get("frame").map(String::as_str), Some("open"), "{f:?}");
    let (_, f, _) = parse(&cmd_offscreen(&term, "since=9"));
    assert!(!f.contains_key("frame"), "no screen half: {f:?}");
    term.lock().unwrap().process(b"wn\x1b[K\x1b[?2026l");
    let (_, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
    assert!(!f.contains_key("frame"), "closed: {f:?}");
    let begun = std::time::Instant::now()
        .checked_sub(aterm_core::terminal::FRAME_HANG + std::time::Duration::from_secs(1))
        .expect("an instant a minute back");
    term.lock().unwrap().process_at(
        b"\x1b[?2026h\x1b[3;1Hhung",
        aterm_core::terminal::ClockReading {
            monotonic: begun,
            wall_ms: None,
        },
    );
    assert!(term.lock().unwrap().sync_frame_unfinished());
    let (_, f, _) = parse(&cmd_offscreen(&term, "since=9 screen=1"));
    assert!(!f.contains_key("frame"), "past its hang detector: {f:?}");
}

#[test]
fn paging_with_since_last_as_documented_reads_every_row() {
    let term = scrolled(10);
    let mut all = Vec::new();
    let mut since = 0u64;
    let mut pages = 0;
    loop {
        let reply = cmd_offscreen(&term, &format!("since={since} max=4"));
        let (_, f, lines) = parse(&reply);
        all.extend(lines);
        since = field(&f, "last");
        pages += 1;
        if !f.contains_key("more") || pages > 10 {
            break;
        }
    }
    let want: Vec<String> = (0..9).map(row).collect();
    assert_eq!(
        all, want,
        "paging with since=<last> must neither skip nor repeat"
    );
}

#[test]
fn a_disabled_archive_is_not_reported_as_an_empty_one() {
    let mut t = scrolled_engine(0);
    t.set_alt_archive_enabled(false);
    for s in 0..10 {
        paint(&mut t, &frame(s));
    }
    let term = Arc::new(Mutex::new(t));
    let (n, f, _) = parse(&cmd_offscreen(&term, ""));
    assert_eq!(n, 0);
    assert_eq!(f.get("enabled").map(String::as_str), Some("0"), "{f:?}");
    term.lock().unwrap().set_alt_archive_enabled(true);
    let (_, f, _) = parse(&cmd_offscreen(&term, ""));
    assert!(!f.contains_key("enabled"), "{f:?}");
}

#[test]
fn back_under_a_pinned_header_names_rows_the_archive_holds() {
    fn framed(start: usize) -> Vec<String> {
        let mut v = vec!["HEADER pinned title bar".to_string()];
        v.extend((start..start + 5).map(row));
        v.push("> type a message".to_string());
        v.push("footer: ready".to_string());
        v
    }
    let mut t = scrolled_engine(0);
    for s in 0..10 {
        paint(&mut t, &framed(s));
    }
    paint(&mut t, &framed(8)); // the app scrolled back one row
    let term = Arc::new(Mutex::new(t));
    let reply = cmd_offscreen(&term, "screen=1");
    let (n, f, lines) = parse(&reply);
    let m = field(&f, "screen_rows") as usize;
    let back = field(&f, "back") as usize;
    let (pin, back_at) = (field(&f, "pin") as usize, field(&f, "back_at"));
    let first = field(&f, "first");
    let (archived, screen) = lines.split_at(n - m);
    assert_eq!((back, pin), (1, 1), "{f:?}");
    for k in 0..back {
        let at = usize::try_from(back_at - first).unwrap() + k;
        assert_eq!(
            screen[pin + k],
            archived[at],
            "screen row pin+{k} is archived row back_at+{k}: {f:?}"
        );
    }
    assert_eq!(
        screen[0], "HEADER pinned title bar",
        "the header is not re-shown"
    );
    term.lock()
        .unwrap()
        .process(b"\x1b[?2026h\x1b[2;1Hnew row\x1b[K\x1b[?2026l");
    let (_, f, _) = parse(&cmd_offscreen(&term, ""));
    if field(&f, "back") == 0 {
        assert!(
            !f.contains_key("pin") && !f.contains_key("back_at"),
            "{f:?}"
        );
    }
}

#[test]
fn archived_rows_are_byte_identical_to_the_text_rows_they_were() {
    fn special(i: usize) -> String {
        format!("r{i:03} \u{65e5}\u{672c} cafe\u{301} \u{1F44D}\x1b[3Cgap")
    }
    let mut t = scrolled_engine(0);
    let mut shown = Vec::new();
    for s in 0..8 {
        let mut v: Vec<String> = (s..s + 6).map(special).collect();
        v.push("> type a message".to_string());
        v.push("footer: ready".to_string());
        paint(&mut t, &v);
        shown.push(visible_row(&t, 0));
    }
    let term = Arc::new(Mutex::new(t));
    let (_, _, lines) = parse(&cmd_offscreen(&term, ""));
    assert_eq!(lines, shown[..7].to_vec());
}
