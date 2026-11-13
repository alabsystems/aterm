// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_resize_row_reads_its_id_and_a_run_row_is_none() {
    let line = "resize 7 t=1234 run=2 ord=3 from=80x24 to=80x23 alt=1 sync=0 site=chrome at=- win=80x24 chrome=1";
    assert_eq!(parse_resize(line), Some(7));
    assert_eq!(parse_resize("run 2 t=1 n=2"), None);
}

#[test]
fn phases_parse_in_any_spelling_and_refuse_strangers() {
    assert_eq!(parse_phases("A H").unwrap(), vec!['A', 'H']);
    assert_eq!(parse_phases("b,t").unwrap(), vec!['B', 'T']);
    assert!(
        parse_phases("x").is_err(),
        "phase X went with the message band"
    );
    assert!(parse_phases("Q").is_err());
}

#[test]
fn the_stand_in_frame_fills_the_screen_and_parks_on_the_composer() {
    let f = frame(24, 80, "ab");
    assert_eq!(f.matches(";1H").count(), 24, "one cursor move per row");
    assert!(f.contains("\x1b[1;1H  ⏺ transcript line 00"));
    assert!(f.contains("\x1b[24;1H  ◯ context 12%"));
    assert!(f.ends_with("\x1b[20;5H\x1b[?2026l"));
}

#[test]
fn a_verdict_is_the_first_stderr_line_without_the_client_prefix() {
    let r = Reply {
        code: 0,
        stdout: "$ hi\n".into(),
        stderr: "aterm-ctl: turn submitted=1 status=settled\n$ hi\n".into(),
    };
    assert_eq!(verdict(&r), "turn submitted=1 status=settled");
    let r = Reply {
        code: 0,
        stdout: "OK lease released\n".into(),
        stderr: String::new(),
    };
    assert_eq!(verdict(&r), "OK lease released");
}

#[test]
fn a_resizes_reply_that_failed_is_no_reading_never_an_empty_ledger() {
    let ok = Reply {
        code: 0,
        stdout: "resize 7 t=1234 run=2 ord=3 from=80x24 to=80x23 alt=1\nrun 2 t=1 n=1\n".into(),
        stderr: "aterm-ctl: OK 2 total=7 runs=2 flaps=0 risks=0 render=ok\n".into(),
    };
    assert_eq!(ledger_rows("resizes since=6", &ok), Ok(vec![7]));
    let none = Reply {
        stderr: "OK 0 total=7 runs=2 flaps=0 risks=0 render=ok\n".into(),
        ..Reply::default()
    };
    assert_eq!(ledger_rows("resizes since=7", &none), Ok(Vec::new()));

    let refused = Reply {
        code: 1,
        stdout: String::new(),
        stderr: "aterm-ctl: ERR usage: resizes [<n>] [since=<id>]\n".into(),
    };
    let why = ledger_rows("resizes since=x", &refused).unwrap_err();
    assert!(why.contains("ERR usage"), "{why}");
    let timed_out = Reply { code: 124, ..ok };
    assert!(ledger_rows("resizes since=6", &timed_out).is_err());
    assert!(ledger_rows("resizes since=6", &Reply::default()).is_err());
}
