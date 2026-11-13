// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const DIR: &str = "../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308";
const BEFORE: &[u8] =
    include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/before.bin");
const HALF: &[u8] =
    include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/half.bin");
const REST: &[u8] =
    include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/rest.bin");
const END: &[u8] =
    include_bytes!("../../aterm-core/tests/fixtures/half-drawn/claude-2.1.308/end.bin");

/// A cast of `reads`, one event each, a millisecond apart, as `aterm ctl cast` writes it.
fn cast_text(reads: &[&[u8]]) -> String {
    let mut out = String::from("{\"version\": 2, \"width\": 144, \"height\": 50}\n");
    for (i, r) in reads.iter().enumerate() {
        let data = std::str::from_utf8(r).unwrap_or_else(|e| panic!("{DIR}: {e}"));
        let quoted = aterm_json::to_string(&data).expect("a JSON string");
        out.push_str(&format!("[{:.6}, \"o\", {quoted}]\n", i as f64 * 0.001));
    }
    out
}

fn scan_claude(reads: &[&[u8]]) -> String {
    let cast = parse_cast(&cast_text(reads)).expect("a cast");
    let reader = Program::Claude.reader();
    let guard = reader.busy_guard().expect("a busy guard");
    scan(
        &cast,
        reader,
        &aterm_regex::Regex::new(&guard).expect("the guard"),
        0,
        None,
    )
}

/// Claude Code 2.1.308's MEASURED split (aterm-core's `half-drawn` fixtures): one read ends
/// inside a frame, and its busy gap is the frame the kernel holds (`open`). NEGATIVE
/// CONTROL: the same reads with the split joined end inside no frame and leave no gap.
#[test]
fn the_measured_split_is_one_open_gap() {
    let split = scan_claude(&[b"\x1b[47;3H", BEFORE, HALF, REST, END]);
    assert!(split.contains(", 1 ending inside an open frame"), "{split}");
    assert!(
        split.contains("busy gaps: 1 (open 1, settle 0, torn 0, UNGUARDED 0"),
        "{split}"
    );
    assert!(split.contains("gap at event 2 "), "{split}");
    let joined: Vec<u8> = [HALF, REST].concat();
    let whole = scan_claude(&[b"\x1b[47;3H", BEFORE, &joined, END]);
    assert!(whole.contains(", 0 ending inside an open frame"), "{whole}");
    assert!(whole.contains("busy gaps: 0 "), "{whole}");
}

/// The same MEASURED split under the running title Claude Code 2.1.308 sets mid-turn (its
/// `end.bin` runs `◑`/`◐`): `await gone <guard>` reads no title, so the half frame is still
/// the one open gap, though the reading beside the title calls it busy. NEGATIVE CONTROL: the
/// split joined under the same title leaves no gap.
#[test]
fn a_running_title_hides_no_busy_row_gap() {
    let title = "\x1b]0;\u{25D0} Claude Code\x07\x1b[47;3H";
    let split = scan_claude(&[title.as_bytes(), BEFORE, HALF, REST, END]);
    assert!(
        split.contains("busy gaps: 1 (open 1, settle 0, torn 0, UNGUARDED 0"),
        "{split}"
    );
    assert!(split.contains("gap at event 2 "), "{split}");
    assert!(split.contains(" open phase=busy "), "{split}");
    let joined: Vec<u8> = [HALF, REST].concat();
    let whole = scan_claude(&[title.as_bytes(), BEFORE, &joined, END]);
    assert!(whole.contains("busy gaps: 0 "), "CONTROL: {whole}");
}

/// Claude Code 2.1.311's spinner row, drawn above a status line's composer with no
/// `esc to interrupt`, is no busy row; under a running title the reading beside it is busy
/// all the same, so a spinner frame between title-held reply frames is no gap and a cast of
/// spinner frames alone is not blind. NEGATIVE CONTROL: the titles stripped, no read is busy.
#[test]
fn a_spinner_under_a_running_title_is_busy() {
    use aterm_phase::prompt::fixtures::{STATUSLINE_MIDREPLY_2_1_311, screen as rows_of};
    let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
    let (top, _) = aterm_phase::phase::composer_rules(&mid).expect("a composer");
    let mut spinning = mid.clone();
    spinning.insert(
        top,
        "\u{2736} Deliberating\u{2026} (4s \u{00B7} \u{2193} 120 tokens)".to_string(),
    );
    let (a, b) = (Some("\u{25D0} Claude Code"), Some("\u{25D1} Claude Code"));
    let between = scan_status_line(&[(&mid, a), (&spinning, b), (&mid, a)]);
    assert!(between.contains("\nbusy states: 3\n"), "{between}");
    assert!(between.contains("busy gaps: 0 "), "{between}");
    let alone = scan_status_line(&[(&spinning, a), (&spinning, b), (&spinning, a)]);
    assert!(alone.contains("\nbusy states: 3\n"), "{alone}");
    let stripped = scan_status_line(&[(&mid, None), (&spinning, None), (&mid, None)]);
    assert!(
        stripped.contains("\nbusy states: 0 (blind: "),
        "CONTROL: {stripped}"
    );
}

/// Under a status line the turn's one sign is the reading beside its running title: a frame
/// mid-reply that it does not read busy (here the composer cut, so the title holds nothing)
/// is a gap, the read the harness takes there. NEGATIVE CONTROL: the composer kept, no gap.
#[test]
fn under_a_status_line_a_gap_is_the_titled_busy_read_gone() {
    use aterm_phase::prompt::fixtures::{STATUSLINE_MIDREPLY_2_1_311, screen as rows_of};
    let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
    let (top, _) = aterm_phase::phase::composer_rules(&mid).expect("a composer");
    let cut: Vec<String> = mid[..top].to_vec();
    let (a, b) = (Some("\u{25D0} Claude Code"), Some("\u{25D1} Claude Code"));
    let gap = scan_status_line(&[(&mid, a), (&cut, b), (&mid, a)]);
    assert!(gap.contains("busy gaps: 1 "), "{gap}");
    assert!(gap.contains("gap at event 1 "), "{gap}");
    let kept = scan_status_line(&[(&mid, a), (&mid, b), (&mid, a)]);
    assert!(kept.contains("busy gaps: 0 "), "CONTROL: {kept}");
}

/// `frames_cast` of `frames` scanned as Claude Code's.
fn scan_status_line(frames: &[(&Vec<String>, Option<&str>)]) -> String {
    let frames: Vec<(&[String], Option<&str>)> =
        frames.iter().map(|(r, t)| (r.as_slice(), *t)).collect();
    let cast = parse_cast(&frames_cast(&frames)).expect("a cast");
    let reader = Program::Claude.reader();
    let guard = reader.busy_guard().expect("a busy guard");
    scan(
        &cast,
        reader,
        &aterm_regex::Regex::new(&guard).expect("the guard"),
        0,
        None,
    )
}

/// Claude Code 2.1.311's status-line reply (lab `mr`, MEASURED frames) draws no busy row: the
/// scan counts the reads its running title holds busy and finds no gap. NEGATIVE CONTROL: the
/// same cast with its titles stripped shows no busy state, and the scan says it was blind.
#[test]
fn a_status_line_reply_is_seen_busy_by_its_title() {
    use aterm_phase::prompt::fixtures::{
        STATUSLINE_MIDREPLY_2_1_311, STATUSLINE_REPLY_DONE_2_1_311, screen as rows_of,
    };
    let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
    let done = rows_of(STATUSLINE_REPLY_DONE_2_1_311);
    let scan_of = |titles: [Option<&str>; 3]| {
        let text = frames_cast(&[
            (mid.as_slice(), titles[0]),
            (mid.as_slice(), titles[1]),
            (done.as_slice(), titles[2]),
        ]);
        let cast = parse_cast(&text).expect("a cast");
        let reader = Program::Claude.reader();
        let guard = reader.busy_guard().expect("a busy guard");
        scan(
            &cast,
            reader,
            &aterm_regex::Regex::new(&guard).expect("the guard"),
            0,
            None,
        )
    };
    let titled = scan_of([
        Some("\u{25D0} Claude Code"),
        Some("\u{25D1} Claude Code"),
        Some("\u{2733} Claude Code"),
    ]);
    assert!(titled.contains("\nbusy states: 2\n"), "{titled}");
    assert!(titled.contains("busy gaps: 0 "), "{titled}");
    let stripped = scan_of([None, None, None]);
    assert!(
        stripped.contains("\nbusy states: 0 (blind: "),
        "CONTROL: {stripped}"
    );
}

/// With no running title a read is busy by its busy row alone, as before the scan read titles:
/// a frame that keeps Claude Code's spinner row but has lost its footer's `esc to interrupt`
/// is a busy gap, the read `await gone esc.to.interrupt` answers on, though the reader still
/// reads the spinner as a live turn. NEGATIVE CONTROL: the footer kept, no gap.
#[test]
fn with_no_running_title_a_gap_is_the_busy_row_gone() {
    use aterm_phase::prompt::fixtures::{STATUSLINE_MIDREPLY_2_1_311, screen as rows_of};
    let mid = rows_of(STATUSLINE_MIDREPLY_2_1_311);
    let (top, _) = aterm_phase::phase::composer_rules(&mid).expect("a composer");
    let mut torn = mid.clone();
    torn.insert(
        top,
        "\u{2736} Deliberating\u{2026} (4s \u{00B7} \u{2193} 120 tokens)".to_string(),
    );
    let reader = Program::Claude.reader();
    assert_eq!(
        reader.phase(&torn),
        aterm_phase::phase::Phase::Busy,
        "the reader reads the spinner"
    );
    let mode = torn
        .iter()
        .position(|r| r.contains("auto mode on"))
        .expect("the mode row");
    let mut shown = torn.clone();
    shown[mode].push_str(" \u{00B7} esc to interrupt");
    let scan_of = |middle: &[String]| {
        let text = frames_cast(&[
            (shown.as_slice(), None),
            (middle, None),
            (shown.as_slice(), None),
        ]);
        let cast = parse_cast(&text).expect("a cast");
        let guard = reader.busy_guard().expect("a busy guard");
        scan(
            &cast,
            reader,
            &aterm_regex::Regex::new(&guard).expect("the guard"),
            0,
            None,
        )
    };
    let gap = scan_of(&torn);
    assert!(gap.contains("\nbusy states: 2\n"), "{gap}");
    assert!(gap.contains("busy gaps: 1 "), "{gap}");
    assert!(gap.contains("gap at event 1 "), "{gap}");
    let kept = scan_of(&shown);
    assert!(kept.contains("busy gaps: 0 "), "CONTROL: {kept}");
}

#[test]
fn a_cast_keeps_its_output_events_and_refuses_a_bad_line() {
    let text = "{\"version\": 2, \"width\": 80, \"height\": 24}\n[0.5, \"o\", \"a\"]\n\
                [0.6, \"i\", \"k\"]\n[0.7, \"o\", \"b\"]\n";
    let cast = parse_cast(text).expect("a cast");
    assert_eq!((cast.cols, cast.rows, cast.events.len()), (80, 24, 2));
    assert_eq!(cast.events[1].data, "b");
    assert!(parse_cast("{\"version\": 2}\n").is_err(), "no size");
    assert!(parse_cast("{\"width\": 0, \"height\": 24}\n").is_err());
    assert!(parse_cast("{\"width\": 80, \"height\": 0}\n").is_err());
    assert!(parse_cast("{\"width\": 1, \"height\": 1}\n[1, \"o\"]\n").is_err());
    assert!(parse_cast("{\"width\": 1, \"height\": 1}\n[-0.1, \"o\", \"a\"]\n").is_err());
    assert!(parse_cast("{\"width\": 1, \"height\": 1}\n[1e100, \"o\", \"a\"]\n").is_err());
}
