// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::{ClipboardRoute, file_copy, file_paste, primary_file, route_for, route_reaches_person};

/// Only a lab file is nobody's clipboard: the platform's is the person's, and with no seam
/// set (every release build) the route is the platform's.
#[test]
fn only_a_lab_file_is_not_the_persons_clipboard() {
    assert!(route_reaches_person(&ClipboardRoute::System));
    assert!(route_reaches_person(&route_for(None)));
    assert!(!route_reaches_person(&route_for(Some(OsString::from(
        "/lab/clipboard"
    )))));
}

#[test]
fn a_clipboard_write_keeps_its_owner_until_after_close() {
    #[derive(Default)]
    struct State {
        owner: bool,
        open: bool,
        calls: Vec<&'static str>,
    }
    struct Lease(Rc<RefCell<State>>, bool);
    impl Drop for Lease {
        fn drop(&mut self) {
            let mut state = self.0.borrow_mut();
            if self.1 {
                assert!(!state.open, "the owner must outlive the open clipboard");
                state.owner = false;
                state.calls.push("destroy");
            } else {
                state.open = false;
                state.calls.push("close");
            }
        }
    }
    let place = |state: &Rc<RefCell<State>>| {
        let mut state = state.borrow_mut();
        state.calls.push("write");
        state.owner && state.open
    };
    let unowned = Rc::new(RefCell::new(State {
        open: true,
        ..State::default()
    }));
    assert!(!place(&unowned));

    for failure in ["owner", "open", "write", "none"] {
        let state = Rc::new(RefCell::new(State::default()));
        let accepted = super::write_with_owner(
            || {
                state.borrow_mut().calls.push("owner");
                if failure == "owner" {
                    return None;
                }
                state.borrow_mut().owner = true;
                Some(Lease(state.clone(), true))
            },
            |owner| {
                assert!(owner.1 && owner.0.borrow().owner);
                state.borrow_mut().calls.push("open");
                if failure == "open" {
                    return None;
                }
                state.borrow_mut().open = true;
                Some(Lease(state.clone(), false))
            },
            || place(&state) && failure != "write",
        );
        assert_eq!(accepted, failure == "none", "{failure}");
        let state = state.borrow();
        assert!(!state.owner && !state.open, "no lease retained: {failure}");
        let expected: &[&str] = match failure {
            "owner" => &["owner"],
            "open" => &["owner", "open", "destroy"],
            _ => &["owner", "open", "write", "close", "destroy"],
        };
        assert_eq!(state.calls, expected, "{failure}");
    }
}

#[test]
fn the_seam_routes_to_the_system_clipboard_or_its_file() {
    assert_eq!(route_for(None), ClipboardRoute::System);
    for path in ["/scratch/clip.txt", "relative/clip", "clip", ""] {
        assert_eq!(
            route_for(Some(OsString::from(path))),
            ClipboardRoute::File(PathBuf::from(path)),
            "{path:?}"
        );
    }
    assert!(!file_copy(Path::new(""), "x"), "no file to write");
    assert_eq!(file_paste(Path::new("")), None);
    assert_eq!(
        primary_file(Path::new("/scratch/clipboard.txt")),
        PathBuf::from("/scratch/clipboard.txt.primary"),
    );
}

#[test]
fn the_file_stands_in_for_the_clipboard() {
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let clip = dir.path().join("clipboard.txt");
    assert_eq!(file_paste(&clip), None, "no file yet: an empty clipboard");
    assert!(file_copy(&clip, "first"));
    assert!(file_copy(
        &clip,
        "second — ünïcödé
line two"
    ));
    assert_eq!(
        std::fs::read_to_string(&clip).unwrap(),
        "second — ünïcödé
line two",
        "a copy overwrites with the latest contents"
    );
    assert_eq!(
        file_paste(&clip).as_deref(),
        Some(
            "second — ünïcödé
line two"
        )
    );
    let entries = std::fs::read_dir(dir.path()).unwrap().count();
    assert_eq!(entries, 1, "the write-and-rename leaves no temporary");
    assert!(file_copy(&clip, ""), "an OSC 52 clear is a copy of nothing");
    assert_eq!(file_paste(&clip), None, "and reads back as empty");
    assert!(!file_copy(&dir.path().join("absent/clip.txt"), "x"));
}
