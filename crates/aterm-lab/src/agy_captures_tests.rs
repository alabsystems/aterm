// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::pyjson::J;

/// Each box's script is one call of the tool that draws it, in the shape
/// the stub serves (`name`, `input`), the file the write makes the one
/// the edit changes.
#[test]
fn each_box_is_one_call_of_agys_own_tool() {
    let calls: Vec<J> = SCRIPTS.iter().map(|s| J::parse(s).unwrap()).collect();
    let got: Vec<&str> = calls
        .iter()
        .map(|c| c.get("name").and_then(J::as_str).unwrap())
        .collect();
    assert_eq!(
        got,
        ["run_command", "write_file", "edit_file", "ask_question",]
    );
    assert!(
        calls
            .iter()
            .all(|c| matches!(c.get("input"), Some(J::Obj(_))))
    );
    let file = |c: &J| {
        let i = c.get("input").unwrap();
        i.get("Path").and_then(J::as_str).map(str::to_string)
    };
    assert_eq!(file(&calls[1]), file(&calls[2]));
    assert_eq!(file(&calls[1]).as_deref(), Some("@PROJ@/lab-write.txt"));
}

/// Every row a capture waits for is a pattern the regex engine takes,
/// and matches the row it was read off (agy 1.4.0).
#[test]
fn every_screen_a_capture_waits_for_matches_its_measured_row() {
    {
        let s = &AGY;
        for re in [
            s.idle,
            s.trust,
            s.slash,
            s.thinking,
            s.streaming,
            s.interrupted,
            s.shell,
            s.write,
            s.edit,
            s.question,
            s.answered,
            s.exit,
        ] {
            assert!(aterm_regex::Regex::new(re).is_ok(), "{re}");
        }
    }
    let m = |re: &str, row: &str| aterm_regex::Regex::new(re).unwrap().is_match(row);
    assert!(m(AGY.thinking, "⣽  tick 6..."));
    assert!(
        !m(AGY.thinking, "  tick 16 tick 17"),
        "the streamed text is no thought"
    );
    assert!(m(AGY.answered, "> Red"));
    assert!(m(AGY.idle, "? for shortcuts      Gemini 3.1 Pro · low"));
    assert!(m(
        SURVEY,
        " How's the CLI experience so far? Help us improve:"
    ));
    assert!(m(
        "^Error ID: ",
        "Error ID: 5eed0000-1111-4222-8333-444455556666-1"
    ));
    assert!(!m("^Error ID: ", "  Error ID: quoted in a reply"));
}

/// A refusal's capture counts the answers of the status its spec gives;
/// a spec with none is a 200.
#[test]
fn a_refusal_is_counted_by_its_own_status() {
    for (name, _, spec) in crate::stub::gemini::measured_refusals() {
        let want = spec
            .get("json")
            .and_then(|j| j.get("error"))
            .and_then(|e| e.get("code"))
            .and_then(J::as_i64)
            .and_then(|c| u16::try_from(c).ok());
        assert_eq!(Some(refusal_status(&spec)), want, "{name}");
    }
    assert_eq!(refusal_status(&J::obj([("text", J::s("hi"))])), 200);
}
