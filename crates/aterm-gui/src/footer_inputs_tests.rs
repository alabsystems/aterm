// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the footer's held machine inputs ([`super`]): each is loaded once a change of its
//! files, and any change, however slight, is loaded.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;
use crate::work_counts::take_footer_work;

/// Sun 25 Oct 2026 19:30, UTC.
const NOW: i64 = 1_792_956_600;

fn scratch() -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix("aterm-gui-footer-inputs-")
        .tempdir()
        .expect("temp dir")
}

/// The login directory `name` under `/logins` (`C:\logins` on Windows), as a binding or a roster
/// row names it: the accounts store and the roster refuse a relative one, and a path with no
/// drive is relative on Windows. Its `{:?}` is a TOML string of it.
fn login_dir(name: &str) -> String {
    let logins = if cfg!(windows) {
        r"C:\logins"
    } else {
        "/logins"
    };
    format!("{logins}{}{name}", std::path::MAIN_SEPARATOR)
}

/// Set `path`'s modification time to `at`.
fn set_modified(path: &Path, at: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(at))
        .unwrap();
}

/// A whole wall second, a little ahead of the clock.
fn a_second() -> SystemTime {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    UNIX_EPOCH + Duration::from_secs(now.as_secs() + 2)
}

/// A STORE IS LOADED ONCE A CHANGE, HOWEVER SLIGHT (law L5 OneDecodePerStamp, for the accounts
/// store): a hundred asks of a store whose files stand load it once; a rewrite in place at the
/// same length, its modification time in the same second as before, loads it again and the
/// rewrite is what is held; a hundred asks more load nothing.
///
/// NEGATIVE CONTROL (run 2026-11-14): with [`Stamp`] taken as the file's length and its
/// modification time's whole second (the accounts outlook's old stamp), the test fails at "the
/// rewrite is held": `left: Some("me") right: Some("my")`. With [`Memo::fresh`] reading on every
/// ask, it fails at "one load while it stands": `left: 100 right: 1`.
#[test]
fn a_store_is_loaded_once_a_change_however_slight() {
    let dir = scratch();
    let store = accounts::Store::at(dir.path());
    let state = accounts::State {
        active: Some("me".into()),
        ..accounts::State::default()
    };
    store.save(&state).unwrap();
    let second = a_second();
    set_modified(&store.state_file(), second + Duration::from_millis(100));
    let mut stores = Stores::default();
    let active = |stores: &mut Stores| {
        stores
            .state(dir.path(), NOW)
            .value()
            .as_ref()
            .ok()
            .and_then(|state| state.active.clone())
    };
    let _ = take_footer_work();
    for _ in 0..100 {
        assert_eq!(active(&mut stores).as_deref(), Some("me"));
    }
    assert_eq!(
        take_footer_work().store_loads,
        1,
        "one load while it stands"
    );

    let text = std::fs::read_to_string(store.state_file()).unwrap();
    let rewritten = text.replacen("\"me\"", "\"my\"", 1);
    assert_ne!(text, rewritten, "the fixture names its active row");
    assert_eq!(text.len(), rewritten.len(), "at the same length");
    std::fs::write(store.state_file(), rewritten).unwrap();
    set_modified(&store.state_file(), second + Duration::from_millis(600));
    assert_eq!(
        active(&mut stores).as_deref(),
        Some("my"),
        "the rewrite is held"
    );
    for _ in 0..100 {
        assert_eq!(active(&mut stores).as_deref(), Some("my"));
    }
    assert_eq!(
        take_footer_work().store_loads,
        1,
        "one load for the rewrite"
    );
}

/// A CONFIGURED ROSTER IS READ ONCE AND BOUND AGAIN WHEN ITS BINDINGS MOVE: a hundred asks read
/// the roster file once and bind it to the store's bindings; a new binding is the next ask's,
/// with no read of the roster file; a rewritten roster file is read once more. A configured
/// roster with no state to bind it in, or whose bindings cannot be read, is none; an unconfigured
/// one (an absent file) reads as it is, whatever its bindings; a file that does not load is
/// none.
///
/// NEGATIVE CONTROL (run 2026-11-14): with the bound roster keyed by the roster file's stamp
/// alone (run beside the coarse [`Stamp`] of the store test's control, which the new binding's
/// other length still moves), the test fails at "the new binding": `left: Some("/logins/main-2")
/// right: Some("/logins/main-renewed")`. With [`Memo::fresh`] reading on every ask, it fails at
/// "one read while it stands": `left: 100 right: 1`.
#[test]
fn a_configured_roster_is_read_once_and_bound_again_when_its_bindings_move() {
    let dir = scratch();
    let state = dir.path().join("state");
    let store = accounts::Store::at(&state);
    let bind = |to: &str| {
        let mut logins = accounts::Logins::default();
        logins.labels.insert(
            "main".into(),
            accounts::Binding {
                dir: Some(to.into()),
                ..accounts::Binding::default()
            },
        );
        store.save_logins(&logins).unwrap();
    };
    bind(&login_dir("main-2"));
    let path = dir.path().join(accounts::ACCOUNTS_FILE);
    std::fs::write(
        &path,
        "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n",
    )
    .unwrap();
    let mut roster = Roster::default();
    let mut stores = Stores::default();
    let bound_dir = |roster: &mut Roster, stores: &mut Stores| {
        roster
            .at(&path, Some(&state), stores, NOW)
            .map(|roster| (roster.rows.len(), roster.rows[0].dir.clone()))
    };
    let _ = take_footer_work();
    for _ in 0..100 {
        assert_eq!(
            bound_dir(&mut roster, &mut stores),
            Some((1, Some(login_dir("main-2"))))
        );
    }
    assert_eq!(
        take_footer_work().roster_loads,
        1,
        "one read while it stands"
    );

    bind(&login_dir("main-renewed"));
    assert_eq!(
        bound_dir(&mut roster, &mut stores).and_then(|(_, dir)| dir),
        Some(login_dir("main-renewed")),
        "the new binding"
    );
    assert_eq!(take_footer_work().roster_loads, 0, "the roster file stood");

    std::fs::write(
        &path,
        format!(
            "[[account]]\nlabel = \"main\"\nclass = \"dedicated\"\n\n\
             [[account]]\nlabel = \"spare\"\nclass = \"dedicated\"\ndir = {:?}\n",
            login_dir("spare")
        ),
    )
    .unwrap();
    assert_eq!(bound_dir(&mut roster, &mut stores).map(|(n, _)| n), Some(2));
    assert_eq!(
        take_footer_work().roster_loads,
        1,
        "a rewritten roster is read"
    );

    assert!(
        roster.at(&path, None, &mut stores, NOW).is_none(),
        "no state to bind it in"
    );
    std::fs::write(store.logins_file(), "not json").unwrap();
    assert!(
        roster.at(&path, Some(&state), &mut stores, NOW).is_none(),
        "bindings that cannot be read bind nothing"
    );
    let absent = dir.path().join("absent.toml");
    assert_eq!(
        roster.at(&absent, Some(&state), &mut stores, NOW),
        Some(&accounts::Roster::default()),
        "an unconfigured roster reads as it is, its unreadable bindings unasked"
    );
    std::fs::write(&path, "[[account\n").unwrap();
    assert!(
        roster.at(&path, Some(&state), &mut stores, NOW).is_none(),
        "a roster file that does not load is none"
    );
}
