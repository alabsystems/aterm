// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn asks(facts: &Facts, question: &[&[u8]], objects: &[&str], answer: &str) -> (String, bool) {
    let mut asked = false;
    let got = facts
        .answer(question, objects, || {
            asked = true;
            Ok(answer.to_owned())
        })
        .unwrap();
    (got, asked)
}

/// An answer about object IDs is asked once. NEGATIVE CONTROLS: the same words about another
/// object, other words about the same object, and a store that keeps nothing are each asked.
#[test]
fn an_answer_about_an_object_is_asked_once() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let facts = Facts::at(dir.path().join("window"));
    let question: [&[u8]; 3] = [b"git", b"diff", COMMIT.as_bytes()];
    assert_eq!(
        asks(&facts, &question, &[COMMIT], "a\0b\0"),
        ("a\0b\0".into(), true)
    );
    assert_eq!(
        asks(&facts, &question, &[COMMIT], "never read"),
        ("a\0b\0".into(), false),
        "the kept answer stands, bytes and all"
    );
    assert_eq!(facts.counts(), (1, 1));

    let other: [&[u8]; 3] = [b"git", b"diff", OTHER.as_bytes()];
    assert!(asks(&facts, &other, &[OTHER], "c\0").1, "another object");
    let reworded: [&[u8]; 4] = [b"git", b"diff", b"--stat", COMMIT.as_bytes()];
    assert!(
        asks(&facts, &reworded, &[COMMIT], "d\0").1,
        "a question worded otherwise is another question"
    );
    let upgraded = Facts::at(dir.path().join("window")).read_by("git version 9.9.9");
    assert!(
        asks(&upgraded, &question, &[COMMIT], "f").1,
        "another Git's answer is not this one's"
    );
    assert!(!asks(&upgraded, &question, &[COMMIT], "unread").1);
    let nothing = Facts::none();
    assert!(asks(&nothing, &question, &[COMMIT], "e").1);
    assert!(asks(&nothing, &question, &[COMMIT], "e").1);
}

/// A ref moves, so a question that names one is asked every time; an error is no answer.
#[test]
fn a_question_about_a_ref_and_a_failed_question_are_never_kept() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let facts = Facts::at(dir.path());
    let question: [&[u8]; 2] = [b"git", b"HEAD"];
    assert!(asks(&facts, &question, &["HEAD"], "one").1);
    assert_eq!(
        asks(&facts, &question, &["HEAD"], "two"),
        ("two".into(), true)
    );

    let failing: [&[u8]; 2] = [b"git", COMMIT.as_bytes()];
    let refused = facts.answer(&failing, &[COMMIT], || Err("unreadable".into()));
    assert_eq!(refused, Err("unreadable".into()));
    assert_eq!(
        asks(&facts, &failing, &[COMMIT], "readable now"),
        ("readable now".into(), true),
        "the failure kept nothing"
    );
}

/// A cut or edited file is no answer: the question is asked and the file replaced.
#[test]
fn a_damaged_fact_is_asked_again() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let facts = Facts::at(dir.path());
    let question: [&[u8]; 2] = [b"git", COMMIT.as_bytes()];
    assert!(asks(&facts, &question, &[COMMIT], "whole answer").1);
    let kept = dir.path().join(name("", &question));
    let text = std::fs::read_to_string(&kept).unwrap();
    std::fs::write(&kept, &text[..text.len() - 3]).unwrap();
    assert_eq!(
        asks(&facts, &question, &[COMMIT], "whole answer"),
        ("whole answer".into(), true)
    );
    assert!(!asks(&facts, &question, &[COMMIT], "unread").1);
}

/// Opening a window removes the windows nothing opened for thirty days and keeps the rest.
#[test]
fn an_idle_window_is_removed_and_a_used_one_kept() {
    let windows = aterm_tempfile::tempdir().unwrap();
    let window = |name: &str, idle: std::time::Duration| {
        let dir = windows.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fact"), b"x").unwrap();
        let mark = std::fs::File::create(dir.join(USED)).unwrap();
        mark.set_modified(std::time::SystemTime::now() - idle)
            .unwrap();
        dir
    };
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    let idle = window("idle", day * 31);
    let recent = window("recent", day * 29);
    let current = window("current", day * 40);
    let unmarked = windows.path().join("unmarked");
    std::fs::create_dir_all(&unmarked).unwrap();
    retire_idle(windows.path(), &current);
    assert!(!idle.exists());
    assert!(recent.join("fact").exists());
    assert!(current.join("fact").exists(), "the window being opened");
    assert!(unmarked.exists(), "nothing says how old it is");
}
