// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn rows(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_fenced_panic_is_caught_with_its_location_and_the_depth_unwinds() {
    assert_eq!(guarded(|| 7), Ok(7));
    let p = guarded(|| -> u8 {
        let v: Vec<u8> = std::hint::black_box(Vec::new());
        v[3]
    })
    .expect_err("the stand-in panics");
    assert!(p.location.starts_with(concat!(file!(), ":")), "{p:?}");
    assert!(p.message.contains("index out of bounds"), "{p:?}");
    let s = guarded(|| -> () { panic!("{}", String::from("formatted")) }).unwrap_err();
    assert_eq!(s.message, "formatted");
    assert_eq!(DEPTH.with(Cell::get), 0, "the fence is left after a panic");
    let outer = guarded(|| guarded(|| -> () { panic!("inner") }).unwrap_err());
    assert_eq!(outer.expect("the outer read returns").message, "inner");
    assert_eq!(DEPTH.with(Cell::get), 0);
}

#[test]
fn read_or_none_degrades_a_panic_to_none() {
    let screen = rows(&["x"]);
    assert_eq!(read_or_none("test-ron", "test", &screen, || 3), Some(3));
    assert_eq!(
        read_or_none("test-ron", "test", &screen, || -> u8 { panic!("boom") }),
        None
    );
    assert_eq!(DEPTH.with(Cell::get), 0);
}

#[test]
fn one_warn_per_session_per_location() {
    let screen = rows(&["", "─────", " Bash command", " ❯ 1. Yes", "   2. No"]);
    let at = |loc: &str| ReaderPanic {
        location: format!("crates/aterm-phase/src/prompt.rs:{loc}"),
        message: "slice index starts at 1 but ends at 0".to_string(),
    };
    assert!(warn_once(
        "test-warn-a",
        "agent verdict",
        &at("966:21"),
        &screen
    ));
    for _ in 0..1000 {
        assert!(!warn_once(
            "test-warn-a",
            "agent verdict",
            &at("966:21"),
            &screen
        ));
    }
    assert!(warn_once(
        "test-warn-b",
        "agent verdict",
        &at("966:21"),
        &screen
    ));
    assert!(warn_once(
        "test-warn-a",
        "agent verdict",
        &at("12:1"),
        &screen
    ));
}

#[test]
fn the_shape_names_classes_not_text() {
    let screen = rows(&[
        "",
        &"─".repeat(40),
        " Bash command",
        "   │ rm -rf secret-dir",
        " Do you want to proceed?",
        " ❯ 1. Yes",
        "   2. No",
        "❯ ",
        "╌╌╌",
    ]);
    assert_eq!(screen_shape(&screen), "_-x|?##>-");
    let tall: Vec<String> = (0..200).map(|i| format!("row {i}")).collect();
    assert_eq!(screen_shape(&tall).len(), 80);
}
