// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the stand-in's frames, steps and keys: every frame read by the readers the harness
//! reads a real Claude Code with.

use aterm_phase::phase::{Phase, background_wait, in_process_work, worker_phase};
use aterm_phase::reader::{ClaudeReader, ScreenReader};

use super::*;
use crate::occupancy_wake::{ANSWERED, STARTED, declare_command, script, workflow_waits};
use crate::screen::composer_of;

const SIZE: (usize, usize) = (144, 50);

fn at(drawn: Drawn) -> Vec<String> {
    frame(drawn, SIZE, "the task", STARTED, "")
}

/// EACH FRAME READS AS THE STATE IT DRAWS, by the harness's own readers: at rest idle, a live turn
/// busy with no background wait, and the wait on a workflow a break, each of its two rows read on
/// its own: the status row's own words as the workflow pending in Claude Code's process, and the
/// footer's progress row as a workflow running. NEGATIVE CONTROL: both rows taken off, the frame
/// is no break.
#[test]
fn each_frame_reads_as_what_it_draws() {
    const STATUS: &str = "Waiting for";
    const PROGRESS: &str = "agents done";
    let waiting = at(Drawn::Waiting(62));
    assert!(workflow_waits(&waiting), "{waiting:#?}");
    assert_eq!(
        in_process_work(&waiting),
        Some("waiting for a dynamic workflow"),
        "{waiting:#?}"
    );
    assert_eq!(composer_of(&waiting).as_deref(), Some(""));
    let live = at(Drawn::Live(3));
    assert_eq!(background_wait(&live), None, "{live:#?}");
    assert_eq!(worker_phase(&live), Phase::Busy, "{live:#?}");
    let idle = at(Drawn::Idle);
    assert_eq!(background_wait(&idle), None, "{idle:#?}");
    assert_eq!(worker_phase(&idle), Phase::Idle, "{idle:#?}");
    let without = |cut: &[&str]| -> Vec<String> {
        waiting
            .iter()
            .filter(|row| !cut.iter().any(|c| row.contains(c)))
            .cloned()
            .collect()
    };
    let status = without(&[PROGRESS]);
    assert_eq!(
        background_wait(&status),
        Some("waiting for a dynamic workflow"),
        "{status:#?}"
    );
    let progress = without(&[STATUS]);
    assert_eq!(
        background_wait(&progress),
        Some("a workflow running"),
        "{progress:#?}"
    );
    assert_eq!(in_process_work(&progress), None, "{progress:#?}");
    let cut = without(&[STATUS, PROGRESS]);
    assert!(!workflow_waits(&cut), "{cut:#?}");
}

/// THE LINE READS BACK WHOLE (the echo the door waits for): wrapped at any width, the reader's
/// composer holds what was typed, and the frame stays the screen's size. CONTROL: a line left off
/// its last character does not read back.
#[test]
fn a_long_line_in_the_composer_reads_back_whole() {
    let line = "[aterm harness] ".to_string() + &"word ".repeat(70);
    let line = line.trim_end();
    for size in [(80, 24), (144, 50), (200, 50)] {
        let rows = frame(Drawn::Waiting(62), size, "the task", STARTED, line);
        assert_eq!(rows.len(), size.1, "{rows:#?}");
        let (_, shown) = ClaudeReader.composer(&rows).expect("a composer");
        let shown: String = shown.concat().split_whitespace().collect();
        let want: String = line.split_whitespace().collect();
        assert_eq!(shown, want, "{rows:#?}");
        let short = frame(
            Drawn::Waiting(62),
            size,
            "the task",
            STARTED,
            &line[..line.len() - 1],
        );
        let (_, cut) = ClaudeReader.composer(&short).expect("a composer");
        let cut: String = cut.concat().split_whitespace().collect();
        assert_ne!(cut, want, "{short:#?}");
    }
}

/// THE CARET STANDS AFTER THE TEXT, on its last row, so a composer read finds its row.
#[test]
fn the_caret_stands_after_the_draft() {
    let cols = 40;
    for (draft, want_rows, want_col) in [
        ("", 0, 2),
        ("short", 0, 7),
        (&*"x".repeat(38), 0, 40),
        (&*"x".repeat(39), 1, 3),
    ] {
        let rows = frame(Drawn::Waiting(62), (cols, 30), "the task", "", draft);
        let first = rows
            .iter()
            .position(|r| r.chars().all(|c| c == '─') && !r.is_empty())
            .expect("a rule")
            + 1;
        let (row, col) = caret(&rows, draft, cols);
        assert_eq!((row - first, col), (want_rows, want_col), "{draft}");
    }
}

/// THE SCRIPT'S STEPS ARE THE AGENT'S OWN, in order: the scenario's declaration, its workflow and
/// its two replies. A subagent's steps (`when`) are left out. CONTROL: such a step alone gives none.
#[test]
fn the_agent_plays_its_own_steps_of_the_scenarios_script() {
    let run = "occupancy-wake-claude-01";
    assert_eq!(
        Vec::from(steps(&script(run))),
        vec![
            Step::Bash(declare_command(run)),
            Step::Workflow,
            Step::Text(STARTED.to_string()),
            Step::Text(ANSWERED.to_string()),
        ]
    );
    let subagent = J::Arr(vec![J::obj([("when", J::s("x")), ("text", J::s("slept"))])]);
    assert!(steps(&subagent).is_empty());
    assert!(steps(&J::s("no script")).is_empty());
}

/// TYPED KEYS MAKE THE DRAFT, Enter submits it, and a control sequence is not text. An empty
/// composer submits nothing.
#[test]
fn keys_make_the_draft_and_enter_submits_it() {
    let mut draft = String::new();
    assert_eq!(typed(&mut draft, "abcd\x7f"), Vec::<String>::new());
    assert_eq!(draft, "abc");
    assert_eq!(typed(&mut draft, "\x1b[I\x1b[200~d"), Vec::<String>::new());
    assert_eq!(draft, "abcd");
    assert_eq!(typed(&mut draft, "e\r"), vec!["abcde".to_string()]);
    assert_eq!(draft, "");
    assert_eq!(typed(&mut draft, "\r\n"), Vec::<String>::new());
    assert_eq!(
        typed(&mut draft, "one\rtwo\r"),
        vec!["one".to_string(), "two".to_string()]
    );
    assert_eq!(typed(&mut draft, "gone\x15"), Vec::<String>::new());
    assert_eq!(draft, "");
}

/// ONLY WHAT [`lay`] LAID IS THE STAND-IN, by the absolute path the lab starts it by: `main` keeps
/// every other `claude` the fleet fixture's, a relative path to the laid one included (the safety
/// review's N1: a bare `claude` read the working directory). NEGATIVE CONTROL: the same folder
/// before the lay.
#[test]
fn only_what_was_laid_is_the_stand_in() {
    let dir = aterm_tempfile::tempdir().expect("a scratch folder");
    let bin = dir.path().join("bin");
    std::fs::write(&bin, "").expect("a program");
    let claude = dir.path().join(DIR).join("claude");
    assert!(!configured(&claude.to_string_lossy()));
    assert_eq!(lay(dir.path(), &bin).expect("laid"), claude);
    assert!(configured(&claude.to_string_lossy()));
    assert!(!configured("claude"));
    let cwd = std::env::current_dir().expect("the test's folder");
    let up = "../".repeat(cwd.components().count().saturating_sub(1));
    let relative = format!(
        "{up}{}",
        claude
            .strip_prefix("/")
            .expect("an absolute path")
            .to_string_lossy()
    );
    assert!(
        Path::new(&relative)
            .parent()
            .is_some_and(|d| d.join(MARK).is_file()),
        "{relative} reaches the laid folder from {}",
        cwd.display()
    );
    assert!(!configured(&relative), "{relative}");
    assert_eq!(lay(dir.path(), &bin).expect("laid over"), claude);
}
