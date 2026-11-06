// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use std::cell::{Cell, RefCell};
use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::mpsc;
use std::time::Duration;

use aterm_agent::machine_name::file;
use aterm_types::machine_name::Nickname;

use super::*;

fn view(name: &str, nick: &str, fleet: bool) -> MachineView {
    MachineView {
        name: name.to_owned(),
        nick: nick.to_owned(),
        fleet,
    }
}

fn state() -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().expect("a tempdir");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("a private dir");
    root
}

#[test]
fn the_title_is_the_name_and_the_nickname_clipped_to_sixteen_scalars() {
    assert_eq!(
        machine_menu_bar_title(&view("m8", "Heron", true)),
        "m8 Heron"
    );
    assert_eq!(
        machine_menu_bar_title(&view("m0", "Otter", false)),
        "m0 Otter"
    );
    let sixteen = "Abcdefghijklmnop";
    assert_eq!(
        machine_menu_bar_title(&view("m8", sixteen, true)),
        format!("m8 {sixteen}"),
        "sixteen stand whole"
    );
    assert_eq!(
        machine_menu_bar_title(&view("m8", "Abcdefghijklmnopq", true)),
        "m8 Abcdefghijklmno\u{2026}",
        "fifteen and an ellipsis"
    );
    assert_eq!(
        machine_menu_bar_title(&view("m8", "Été-très-long-surnom", true))
            .chars()
            .count(),
        3 + NICK_SHOWN,
        "counted in scalars, not bytes"
    );
    assert_eq!(machine_menu_bar_title(&view("m8", "", true)), "m8");
}

#[test]
fn the_first_row_says_whether_this_machine_is_in_a_fleet() {
    assert_eq!(
        machine_menu_info_row(&view("m8", "Heron", true)),
        "m8 \u{00B7} Heron \u{00B7} in a fleet"
    );
    assert_eq!(
        machine_menu_info_row(&view("m0", "Otter", false)),
        "m0 \u{00B7} Otter \u{00B7} not in a fleet"
    );
}

/// A nickname no draw can make (the nouns are one word each), so a rename to it always changes
/// the view.
const RENAMED: &str = "Blue Heron";

fn rename(dir: &std::path::Path) {
    file::replace(dir, |name| {
        name.nick = Nickname::parse(RENAMED).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(())
    })
    .expect("renames");
}

fn join(dir: &std::path::Path) {
    let key = format!("{}\n", "ab".repeat(32));
    aterm_uds::private_file::write_private(dir, aterm_net::fleet::KEY_FILE, key.as_bytes())
        .expect("joins");
}

#[test]
fn the_reader_posts_the_name_and_each_change_it_hears() {
    let root = state();
    let dir = root.path();
    let posted = RefCell::new(Vec::new());
    let waits = RefCell::new(Vec::new());
    let post = |view: MachineView| {
        posted.borrow_mut().push(view);
        true
    };
    let wait = |timeout: Option<Duration>| {
        waits.borrow_mut().push(timeout);
        match waits.borrow().len() {
            // The watch's first look tells with nothing changed: the read posts nothing.
            1 => Heard::Change,
            2 => {
                rename(dir);
                Heard::Change
            }
            3 => {
                join(dir);
                Heard::Change
            }
            _ => Heard::Ended,
        }
    };
    read_loop(dir, || Ok(None), post, wait);
    let posted = posted.into_inner();
    let first = posted.first().expect("a first view").clone();
    assert_eq!((first.name.as_str(), first.fleet), ("m0", false));
    assert_eq!(
        posted,
        [first, view("m0", RENAMED, false), view("m0", RENAMED, true)],
        "one post per change, none for a read that found none"
    );
    assert_eq!(
        waits.into_inner(),
        [None; 4],
        "a read that succeeded waits for a change alone, and the reader ends with its watch"
    );
}

#[test]
fn a_read_that_fails_makes_nothing_and_is_tried_again_after_a_minute() {
    let root = state();
    let asked = Cell::new(0);
    let roster = || {
        asked.set(asked.get() + 1);
        if asked.get() == 1 {
            Err(io::Error::other("the roster is busy"))
        } else {
            Ok(Some("m8".to_owned()))
        }
    };
    let waits = RefCell::new(Vec::new());
    let mut posted = Vec::new();
    read_loop(
        root.path(),
        roster,
        |view| {
            posted.push(view);
            false
        },
        |timeout| {
            assert!(
                file::read(root.path()).expect("reads").is_none(),
                "nothing made of a failed read"
            );
            waits.borrow_mut().push(timeout);
            Heard::Quiet
        },
    );
    assert_eq!(waits.into_inner(), [Some(RETRY_AFTER)]);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].name, "m8", "the next read's roster");
}

#[test]
fn the_watch_hears_a_rename_by_replace_and_a_fleet_key_written() {
    let root = state();
    let dir = root.path().to_path_buf();
    let (watch, changes) = watch(&dir).expect("a watch");
    let (tell, views) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        read_loop(
            &dir,
            || Ok(None),
            |view| tell.send(view).is_ok(),
            |timeout| heard(&changes, timeout),
        );
    });
    // Each wait is a one-minute hang detector for a change that must be heard.
    let hang = Duration::from_secs(60);
    let first = views.recv_timeout(hang).expect("a first view");
    rename(root.path());
    assert_eq!(
        views.recv_timeout(hang).expect("the rename heard"),
        view(&first.name, RENAMED, false)
    );
    join(root.path());
    assert_eq!(
        views.recv_timeout(hang).expect("the fleet key heard"),
        view(&first.name, RENAMED, true)
    );
    drop(watch);
    reader.join().expect("the reader ends with its watch");
}

#[test]
fn no_watch_reads_once_and_retries_only_a_failed_read() {
    assert_eq!(unwatched(None), Heard::Ended);
    assert_eq!(unwatched(Some(Duration::ZERO)), Heard::Quiet);
}

#[test]
fn a_title_read_before_an_update_commits_is_kept() {
    let mut app = crate::App::headless_for_test();
    app.incoming_handoff_pending = true;
    assert!(
        !app.drops_wake_before_commit(&Wake::MachineName(view("m8", "Heron", true))),
        "the reader posts a view once, so a candidate that dropped it would show no title"
    );
}
