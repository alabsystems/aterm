// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Only a file with exactly one known `level=` line raises the level. CONTROL: the same
/// word decodes to its level when it stands alone.
#[test]
fn decode_fails_closed() {
    use FleetAccess::{Full, ReadOnly, ReadWrite};
    assert_eq!(decode(None), ReadOnly, "no file");
    for closed in [
        &b""[..],
        b"\n",
        b"garbage",
        b"level=\n",
        b"level=wide\n",
        b"level=Full\n",
        b"level=full \n",
        b" level=full\n",
        b"access=full\n",
        b"level=full\nlevel=full\n",
        b"level=read\nlevel=full\n",
        b"level=full\nlevel=wide\n",
        b"level=full\xff\n",
    ] {
        assert_eq!(decode(Some(closed)), ReadOnly, "{closed:?}");
    }
    assert_eq!(decode(Some(b"level=read\n")), ReadOnly);
    assert_eq!(decode(Some(b"level=write\n")), ReadWrite);
    assert_eq!(decode(Some(b"level=full")), Full);
    assert_eq!(
        decode(Some(b"at=1\nby=cli\nlevel=full\nnote=x\n")),
        Full,
        "the other lines are not levels"
    );
}

#[cfg(unix)]
fn scratch() -> (aterm_tempfile::TempDir, std::path::PathBuf) {
    let tmp = aterm_tempfile::Builder::new()
        .prefix("aterm-fleet-access-")
        .tempdir()
        .expect("a scratch directory");
    let state = tmp.path().join("state");
    (tmp, state)
}

#[cfg(unix)]
#[test]
fn a_level_round_trips_and_a_reset_is_read_only() {
    let (_tmp, state) = scratch();
    let fresh = read(&state);
    assert_eq!(
        (fresh.level, fresh.unread.clone(), fresh.at, fresh.by),
        (FleetAccess::ReadOnly, None, None, None),
        "no state folder, no file: Read Only, and nothing unread"
    );
    assert!(!reset(&state).expect("nothing to remove"));
    let change = set(&state, FleetAccess::Full, By::Cli).expect("written");
    assert_eq!(
        change,
        Change {
            from: FleetAccess::ReadOnly,
            to: FleetAccess::Full
        }
    );
    let now = read(&state);
    assert_eq!((now.level, now.by), (FleetAccess::Full, Some(By::Cli)));
    assert!(now.unread.is_none() && now.at.is_some());
    let change = set(&state, FleetAccess::ReadWrite, By::Menu).expect("written");
    assert_eq!(
        (change.from, change.to),
        (FleetAccess::Full, FleetAccess::ReadWrite)
    );
    assert_eq!(read(&state).by, Some(By::Menu));
    // The same level stated again is written: the file says who stated it last, and it is
    // another file, which a raise pending elsewhere sees.
    let before = basis(&state);
    let same = set(&state, FleetAccess::ReadWrite, By::Cli).expect("stated again");
    assert_eq!((same.from, same.to), (same.to, FleetAccess::ReadWrite));
    assert_eq!(read(&state).by, Some(By::Cli));
    assert_ne!(basis(&state), before, "a statement is seen");
    assert_eq!(
        basis(&state),
        basis(&state),
        "control: a read changes nothing"
    );
    assert!(reset(&state).expect("removed"));
    assert_eq!(read(&state).level, FleetAccess::ReadOnly);
    assert!(!state.join(FILE).exists());
}

/// A file that is there and unusable is Read Only, and says why. CONTROL: the same bytes in
/// a private file are used.
#[cfg(unix)]
#[test]
fn an_unusable_file_is_read_only_and_says_why() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_tmp, state) = scratch();
    set(&state, FleetAccess::Full, By::Cli).expect("written");
    let file = state.join(FILE);

    for mode in [0o644, 0o620, 0o602] {
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
        let open = read(&state);
        assert_eq!(open.level, FleetAccess::ReadOnly, "{mode:o}");
        assert!(
            matches!(open.unread, Some(Unread::NotPrivate(_))),
            "{mode:o}: {open:?}"
        );
    }
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read(&state).level, FleetAccess::Full, "control");

    let mut large = b"level=full\n".to_vec();
    large.resize(MAX + 1, b'\n');
    aterm_uds::private_file::write_private(&state, FILE, &large).unwrap();
    let large = read(&state);
    assert_eq!(large.level, FleetAccess::ReadOnly);
    assert!(
        matches!(large.unread, Some(Unread::Malformed(_))),
        "{large:?}"
    );

    aterm_uds::private_file::write_private(&state, FILE, b"level=full\nlevel=read\n").unwrap();
    let two = read(&state);
    assert_eq!(
        (two.level, two.unread),
        (FleetAccess::ReadOnly, Some(Unread::NotUnderstood))
    );
    aterm_uds::private_file::write_private(&state, FILE, b"level=read\n").unwrap();
    assert_eq!(read(&state).unread, None, "Read Only in so many words");
    // A level asked over a file that was not understood is written, even Read Only.
    aterm_uds::private_file::write_private(&state, FILE, b"junk\n").unwrap();
    set(&state, FleetAccess::ReadOnly, By::Cli).expect("rewritten");
    assert_eq!(read(&state).unread, None);
}

/// A write the listeners would not honour is an error: the state folder is open to others,
/// so the file would read as Read Only.
#[cfg(unix)]
#[test]
fn a_write_that_would_not_be_honoured_is_an_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let (_tmp, state) = scratch();
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(set(&state, FleetAccess::Full, By::Cli).is_err());
    assert_eq!(read(&state).level, FleetAccess::ReadOnly);
}

/// A raise asked over what the state held is written only while that still stands: the
/// level stated again by another writer, a fleet left, another fleet's key and a first key
/// each leave it unwritten. CONTROL: over the basis as it stands, it is written.
#[cfg(unix)]
#[test]
fn a_raise_is_written_only_over_the_state_it_was_asked_over() {
    use aterm_net::fleet_member::{Began, write_held};
    use std::time::Duration;
    let (_tmp, state) = scratch();
    let key = |byte: u8| {
        aterm_uds::private_file::create_private_dir(&state).unwrap();
        write_held(&state, Began::Here, |book| book.write_key(&[byte; 20]))
            .expect("the lock")
            .expect("the key");
    };
    let raise = |over: &Basis| {
        raise_over(
            &state,
            FleetAccess::ReadWrite,
            By::Menu,
            over,
            Duration::ZERO,
        )
        .expect("no error")
    };
    let level = || read(&state).level;
    key(1);

    // Stated again elsewhere, at the level in force (no file before, Read Only after).
    let asked = basis(&state);
    assert!(asked.stored.is_none() && asked.fleet.is_some());
    set(&state, FleetAccess::ReadOnly, By::Cli).unwrap();
    assert_eq!(raise(&asked), None, "another writer stated the level since");
    assert_eq!(level(), FleetAccess::ReadOnly);

    // The fleet left: its key is gone.
    let asked = basis(&state);
    std::fs::remove_file(state.join(aterm_net::fleet::KEY_FILE)).unwrap();
    assert_eq!(raise(&asked), None, "the fleet was left since");
    assert_eq!(level(), FleetAccess::ReadOnly);

    // A fleet joined since, where none was held, and another fleet's key in place of one.
    let asked = basis(&state);
    key(2);
    assert_eq!(raise(&asked), None, "a fleet was joined since");
    let asked = basis(&state);
    key(3);
    assert_eq!(raise(&asked), None, "the fleet changed since");
    assert_eq!(level(), FleetAccess::ReadOnly);

    // CONTROL: nothing moved.
    let asked = basis(&state);
    let change = raise(&asked).expect("written");
    assert_eq!(
        (change.from, change.to, level()),
        (
            FleetAccess::ReadOnly,
            FleetAccess::ReadWrite,
            FleetAccess::ReadWrite
        )
    );
    assert_eq!(read(&state).by, Some(By::Menu));
}

/// A level above Read Only waits for the book's lock, which every write of a fleet's key
/// holds: a raise, and a statement of Read and Write that raises nothing, are refused
/// while a writer holds it past their wait, and nothing is stored. Read Only takes no lock.
/// CONTROL: the same raise once the lock is free.
#[cfg(unix)]
#[test]
fn a_raise_is_refused_while_a_writer_of_the_book_holds_its_lock() {
    use aterm_net::fleet_member::{Began, write_held};
    use std::time::Duration;
    let (_tmp, state) = scratch();
    aterm_uds::private_file::create_private_dir(&state).unwrap();
    set(&state, FleetAccess::ReadWrite, By::Cli).unwrap();
    let asked = basis(&state);
    write_held(&state, Began::Here, |_book| {
        let refused = raise_over(&state, FleetAccess::Full, By::Menu, &asked, Duration::ZERO);
        assert!(refused.is_err(), "{refused:?}");
        let stated = set_within(
            &state,
            FleetAccess::ReadWrite,
            By::Menu,
            Some(Duration::ZERO),
        );
        assert!(stated.is_err(), "{stated:?}");
        assert_eq!(basis(&state), asked, "nothing was written");
        set(&state, FleetAccess::ReadOnly, By::Menu).expect("Read Only waits for nothing");
    })
    .expect("the lock");
    assert_eq!(read(&state).level, FleetAccess::ReadOnly);
    let asked = basis(&state);
    let raised = raise_over(&state, FleetAccess::Full, By::Menu, &asked, Duration::ZERO);
    assert!(matches!(raised, Ok(Some(_))), "{raised:?}");
    assert_eq!(read(&state).level, FleetAccess::Full);
}

#[cfg(unix)]
#[test]
fn a_fleet_started_worker_is_one_with_a_record() {
    let (_tmp, state) = scratch();
    let sid = "s-00000000000000aa";
    assert!(!fleet_started(&state, sid));
    let dir = state.join(aterm_types::fleet_access::DRIVE_WORKERS);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(sid), "node").unwrap();
    assert!(fleet_started(&state, sid));
    assert!(!fleet_started(&state, "s-00000000000000ab"));
    assert!(!fleet_started(&state, "../fleet-access"), "no sid, no look");
    assert!(!fleet_started(&state, ""));
}
