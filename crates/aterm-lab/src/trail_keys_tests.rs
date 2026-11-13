// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const TRAIL: &str = "aterm-ctl: OK 3 trail\n\
    admission seq=7 phase=licensed reason=licensed age_ms=213 origin=47,4 target=47,5 alt=1 licence=key source=bound source_view=- source_session=-\n\
    admission seq=8 phase=declined reason=program-row age_ms=103 origin=47,5 target=47,6 alt=1 licence=none source=bound source_view=- source_session=-\n\
    admission seq=9 phase=licensed reason=licensed age_ms=3 origin=47,6 target=47,7 alt=1 licence=key source=bound source_view=- source_session=-\n";

#[test]
fn the_admission_lines_are_read_in_order() {
    let all = admissions(TRAIL);
    assert_eq!(all.iter().map(|a| a.seq).collect::<Vec<_>>(), [7, 8, 9]);
    assert_eq!(all[1].phase, "declined");
    assert_eq!(all[1].reason, "program-row");
    assert!(admissions("aterm-ctl: OK 0 trail\n").is_empty());
}

#[test]
fn lit_wants_every_key_licensed_and_dark_wants_none() {
    let all = admissions(TRAIL);
    assert!(judged(&all, 8, 1, Want::Lit).0);
    assert!(!judged(&all, 6, 3, Want::Lit).0, "one key was refused");
    assert!(
        !judged(&all, 8, 2, Want::Lit).0,
        "one admission where two keys were typed"
    );
    assert!(judged(&all, 7, 1, Want::Lit).0, "the last key after seq 7");
    assert!(!judged(&all, 6, 3, Want::Dark).0, "two keys lit");
    assert!(judged(&all[..2], 7, 1, Want::Dark).0);
    assert!(
        !judged(&all, 9, 1, Want::Dark).0,
        "no admission is no verdict"
    );
    assert_eq!(
        judged(&all, 6, 3, Want::Lit).1,
        ["licensed", "program-row", "licensed"]
    );
}

#[test]
fn a_failed_scenario_keeps_each_admission_s_whole_line() {
    let all = admissions(TRAIL);
    assert!(all[1].line.starts_with("admission seq=8 phase=declined"));
    assert!(all[1].line.ends_with("source_session=-"));
}

#[test]
fn the_caret_is_read_once_its_frame_is_closed() {
    assert_eq!(caret("OK 21 6 1 blinking_block\n"), Some((21, 6)));
    assert_eq!(caret("21 6 1 steady_bar"), Some((21, 6)));
    assert_eq!(caret("OK 21 6 1 blinking_block frame=open\n"), None);
    assert_eq!(caret("ERR no focused window"), None);
    assert_eq!(caret(""), None);
}

#[test]
fn the_transcript_is_the_rows_above_the_composer_s_rule() {
    let screen = "a\nb\nc\n────\n❯ typed\n────\nfooter\n";
    assert_eq!(transcript(screen, 4), "a\nb\nc");
    assert_eq!(transcript(screen, 0), "");
    assert_eq!(transcript(screen, 1), "");
}

#[test]
fn the_rainbow_leads_the_lab_s_toml_and_replaces_its_own_style() {
    assert_eq!(styled(""), "cursor_trail_style = \"rainbow\"\n");
    let toml = "theme = \"x\"\ncursor_trail_style = \"comet\"\n[render]\ncursor_trail_style = 1";
    assert_eq!(
        styled(toml),
        "cursor_trail_style = \"rainbow\"\ntheme = \"x\"\n[render]\ncursor_trail_style = 1"
    );
}

#[test]
fn an_empty_lab_lines_leaves_the_height_to_the_lane() {
    assert!(lines_unset(None));
    assert!(lines_unset(Some("")));
    assert!(!lines_unset(Some("50")));
}

#[test]
fn the_welcome_s_path_is_found_with_its_columns() {
    let screen = "\n │ ✻ Welcome to Claude Code!   │\n │   cwd: /tmp/lab/work        │\n";
    let (row, first, last) = path_span(screen).unwrap();
    assert_eq!(row, 2);
    let line: Vec<char> = screen.lines().nth(2).unwrap().chars().collect();
    assert_eq!(
        line[first..=last].iter().collect::<String>(),
        "/tmp/lab/work"
    );
    let boxed = "│~/src/aterm│";
    let (_, first, last) = path_span(boxed).unwrap();
    let cells: Vec<char> = boxed.chars().collect();
    assert_eq!(
        cells[first..=last].iter().collect::<String>(),
        "~/src/aterm"
    );
    assert_eq!(path_span("no path here\n"), None);
}
