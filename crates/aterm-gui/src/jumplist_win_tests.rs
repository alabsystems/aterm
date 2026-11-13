// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::front_door_exe;
use std::path::{Path, PathBuf};

fn install_dir() -> PathBuf {
    PathBuf::from(r"C:\Users\me\AppData\Local\Programs\aterm")
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// The shipped layout: whichever image commits the list, every row names the windowed
/// `aterm-gui.exe` (a row naming the console image flashes a console on every click).
#[test]
fn a_row_names_the_windowed_image_whoever_commits_the_list() {
    let dir = install_dir();
    let both = |path: &Path| path == dir.join(exe("aterm")) || path == dir.join(exe("aterm-gui"));
    for running in [
        "aterm-gui",
        "aterm",
        "aterm-ctl",
        "aterm-cli",
        "atpkg",
        "aterm-fleet",
    ] {
        let current = dir.join(exe(running));
        assert_eq!(
            front_door_exe(&current, both, false),
            dir.join(exe("aterm-gui")),
            "a list committed by {running} must launch the windowed front door"
        );
    }
}

/// A build tree: the row names `aterm-windowed.exe`, never the thin dev `aterm-gui.exe`
/// (no router: `new-window` is `unknown command` there); without it the console
/// `aterm.exe` still routes the verb.
#[test]
fn a_build_tree_row_skips_the_thin_dev_bin() {
    let dir = PathBuf::from(r"C:\src\aterm\target\debug");
    let thin = dir.join(exe("aterm-gui"));
    let windowed = dir.join(exe("aterm-windowed"));
    let console = dir.join(exe("aterm"));
    let everything = |path: &Path| path == thin || path == windowed || path == console;
    assert_eq!(front_door_exe(&thin, everything, true), windowed);
    let no_windowed = |path: &Path| path == thin || path == console;
    assert_eq!(front_door_exe(&thin, no_windowed, true), console);
}

/// No front door on disk: fall back to this process, never a row naming a missing file.
#[test]
fn a_lone_copy_falls_back_to_itself() {
    let current = Path::new(r"C:\tmp\scratch").join(exe("aterm-ctl"));
    assert_eq!(front_door_exe(&current, |_| false, false), current);
}

/// Only the windowed image commits the owner's list, and a private window
/// (its own `ATERM_STATE_HOME`) never does: its rows would launch a scratch
/// build and its empty Recent file would empty the owner's Recent rows.
#[test]
fn a_private_window_and_the_console_image_leave_the_list_alone() {
    use super::owns_the_jump_list;
    use crate::ImageSubsystem::{NotWindowed, Windowed};
    assert!(owns_the_jump_list(Windowed, false));
    assert!(!owns_the_jump_list(Windowed, true), "a private window");
    assert!(!owns_the_jump_list(NotWindowed, false), "the console image");
    assert!(!owns_the_jump_list(NotWindowed, true));
}

use super::{Built, E_ACCESSDENIED, StepError, Worker};
use crate::jumplist_recent::{parse, render};
use std::cell::RefCell;

fn rows(list: &[&str]) -> Vec<String> {
    list.iter().map(|d| (*d).to_owned()).collect()
}

/// A worker over a scratch file; the tests hand it their own commit.
fn scratch_worker(file: &Path) -> Worker {
    Worker {
        exe: Vec::new(),
        attach: false,
        file: Some(file.to_path_buf()),
        committed: None,
        warned: false,
        denied: false,
        tracked_before: false,
    }
}

fn kept(file: &Path) -> Vec<String> {
    parse(&std::fs::read_to_string(file).unwrap_or_default())
}

/// Notes reach the file newest first; the list is committed only when the
/// rows it would show change; a row the owner removed leaves the file; a
/// shell that takes no list at all stops the worker.
#[test]
fn the_worker_commits_only_when_the_rows_change() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let file = scratch.path().join(crate::jumplist_recent::FILE_NAME);
    let mut worker = scratch_worker(&file);
    let commits = RefCell::new(Vec::<Vec<String>>::new());
    let removed = RefCell::new(Vec::<String>::new());
    let mut commit = |shown: &[String]| -> Result<Built, StepError> {
        commits.borrow_mut().push(shown.to_vec());
        let gone = std::mem::take(&mut *removed.borrow_mut());
        Ok(Built {
            shown: shown
                .iter()
                .filter(|r| !gone.contains(r))
                .cloned()
                .collect(),
            removed: gone,
            refused: None,
        })
    };
    let exists = |_: &str| true;
    assert!(worker.refresh_with(&[], true, &exists, &mut commit));
    assert_eq!(commits.borrow().as_slice(), [Vec::<String>::new()]);
    assert!(!file.exists());
    worker.refresh_with(&rows(&[r"C:\a"]), true, &exists, &mut commit);
    assert_eq!(commits.borrow().last(), Some(&rows(&[r"C:\a"])));
    assert_eq!(kept(&file), rows(&[r"C:\a"]));
    worker.refresh_with(&rows(&[r"c:\A"]), true, &exists, &mut commit);
    assert_eq!(commits.borrow().len(), 2);
    worker.refresh_with(&rows(&[r"C:\b"]), true, &exists, &mut commit);
    assert_eq!(commits.borrow().last(), Some(&rows(&[r"C:\b", r"C:\a"])));
    *removed.borrow_mut() = rows(&[r"C:\b"]);
    worker.refresh_with(&rows(&[r"C:\c"]), true, &exists, &mut commit);
    assert_eq!(kept(&file), rows(&[r"C:\c", r"C:\a"]));
    worker.refresh_with(&[], true, &exists, &mut commit);
    assert_eq!(commits.borrow().len(), 4, "the rows shown did not change");
    worker.refresh_with(&[], true, &|dir: &str| dir != r"C:\c", &mut commit);
    assert_eq!(commits.borrow().last(), Some(&rows(&[r"C:\a"])));
    assert_eq!(kept(&file), rows(&[r"C:\c", r"C:\a"]));

    let mut failing = |_: &[String]| -> Result<Built, StepError> { Err(("CommitList", -1)) };
    let mut fresh = scratch_worker(&file);
    assert!(!fresh.refresh_with(&[], true, &exists, &mut failing));
}

/// A shell that shows fewer rows than asked (a short
/// `JumpListItems_Maximum`) is not a change: the same ask commits once.
#[test]
fn the_shells_slot_cut_does_not_recommit_the_same_rows() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let file = scratch.path().join(crate::jumplist_recent::FILE_NAME);
    let mut worker = scratch_worker(&file);
    let commits = RefCell::new(0usize);
    let mut two_slots = |shown: &[String]| -> Result<Built, StepError> {
        *commits.borrow_mut() += 1;
        Ok(Built {
            shown: shown.iter().take(2).cloned().collect(),
            removed: Vec::new(),
            refused: None,
        })
    };
    let exists = |_: &str| true;
    let notes = rows(&[r"C:\a", r"C:\b", r"C:\c"]);
    worker.refresh_with(&notes, true, &exists, &mut two_slots);
    assert_eq!(*commits.borrow(), 1);
    worker.refresh_with(&rows(&[r"C:\c"]), true, &exists, &mut two_slots);
    worker.refresh_with(&[], true, &exists, &mut two_slots);
    assert_eq!(*commits.borrow(), 1, "the rows asked for did not change");
    worker.refresh_with(&rows(&[r"C:\a"]), true, &exists, &mut two_slots);
    assert_eq!(*commits.borrow(), 2, "C:\\a moved to the front");
}

/// With Windows' recent items off, the file goes and no Recent row is
/// committed; the shell's own refusal (`E_ACCESSDENIED`) does the same, and
/// holds until the switch is seen off and on again.
#[test]
fn recent_items_off_keeps_nothing() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let file = scratch.path().join(crate::jumplist_recent::FILE_NAME);
    std::fs::write(&file, render(&rows(&[r"C:\a"]))).expect("seed");
    let mut worker = scratch_worker(&file);
    let commits = RefCell::new(Vec::<Vec<String>>::new());
    let mut commit = |shown: &[String]| -> Result<Built, StepError> {
        commits.borrow_mut().push(shown.to_vec());
        Ok(Built {
            shown: shown.to_vec(),
            removed: Vec::new(),
            refused: None,
        })
    };
    let exists = |_: &str| true;
    worker.refresh_with(&rows(&[r"C:\b"]), false, &exists, &mut commit);
    assert!(!file.exists(), "nothing is kept while recent items are off");
    assert_eq!(commits.borrow().last(), Some(&Vec::new()));
    worker.refresh_with(&rows(&[r"C:\b"]), true, &exists, &mut commit);
    assert_eq!(kept(&file), rows(&[r"C:\b"]));

    let mut refusing = |shown: &[String]| -> Result<Built, StepError> {
        commits.borrow_mut().push(shown.to_vec());
        Ok(Built {
            shown: Vec::new(),
            removed: Vec::new(),
            refused: Some(E_ACCESSDENIED),
        })
    };
    worker.refresh_with(&rows(&[r"C:\c"]), true, &exists, &mut refusing);
    assert!(!file.exists(), "the shell said recent items are off");
    worker.refresh_with(&rows(&[r"C:\d"]), true, &exists, &mut commit);
    assert!(
        !file.exists(),
        "the refusal holds while the switch reads the same"
    );
    worker.refresh_with(&[], false, &exists, &mut commit);
    worker.refresh_with(&rows(&[r"C:\e"]), true, &exists, &mut commit);
    assert_eq!(kept(&file), rows(&[r"C:\e"]));
}
