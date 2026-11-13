// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn a_stat_line_reads_its_parent_session_serial_and_name() {
    let line =
        "4242 (trust d) x) S 4200 4242 4100 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 987654 0 0";
    let k = stat_kin(line, 4242).unwrap();
    assert_eq!(
        (
            k.ppid, k.sid, k.serial, k.zombie, k.ctty, k.trustd, k.forked
        ),
        (4200, Some(4100), 987_654, false, false, false, false)
    );
    assert!(!k.leader_only);
    let k = stat_kin(&line.replace("(trust d) x) S", "(trustd) Z"), 4242).unwrap();
    assert!(k.zombie && k.trustd && !k.leader_only);
    let leader = line
        .replace(") S", ") Z")
        .replace(" 20 0 1 0 ", " 20 0 3 0 ");
    let k = stat_kin(&leader, 4242).unwrap();
    assert!(k.leader_only && !k.zombie, "two threads still run");
    assert_eq!(k.serial, 987_654);
    let k = stat_kin(&line.replace(" 4100 0 ", " 4100 34816 "), 4242).unwrap();
    assert!(k.ctty && !k.zombie, "on /dev/pts/0");
    assert_eq!(k.serial, 987_654);
    assert!(stat_kin(line, 4243).is_none(), "another pid's line");
    assert!(
        stat_kin(&line.replacen("4242 ", "+4242 ", 1), 4242).is_none(),
        "plus-prefixed pid"
    );
    assert!(
        stat_kin(&line.replace(") S 4200 ", ") S +4200 "), 4242).is_none(),
        "plus-prefixed ppid"
    );
}

/// The re-read before a signal must show the listed serial, no zombie, under the listed
/// parent, each tie still holding, and no `trustd` in it or its parent.
#[test]
fn a_signal_goes_only_to_the_process_listed_under_its_parent_and_no_trustd() {
    let listed = Proc {
        pid: 301,
        ppid: 300,
        serial: Some(3010),
        sid: Some(200),
        trustd: false,
        marked: None,
        exe: None,
        at_slot: false,
        in_session: false,
        spawning: false,
        tie: Vec::new(),
    };
    let now = |ppid, serial, zombie, trustd| Kin {
        ppid,
        serial,
        sid: Some(200),
        zombie,
        ctty: false,
        trustd,
        forked: false,
        leader_only: false,
    };
    // The kin `table` holds, by pid.
    let read = |table: &[(i32, Kin)]| {
        let table = table.to_vec();
        move |pid| table.iter().find(|(p, _)| *p == pid).map(|(_, k)| *k)
    };
    let parent = now(200, 3000, false, false);
    let seed = now(1, 2000, false, false);
    let trustd = now(1, 3000, false, true);
    assert!(may_signal(
        &listed,
        &now(300, 3010, false, false),
        &mut read(&[(300, parent)])
    ));
    assert!(
        may_signal(&listed, &now(300, 3010, false, false), &mut read(&[])),
        "a parent that cannot be read is no trustd"
    );
    for (why, now, table) in [
        ("a zombie", now(300, 3010, true, false), vec![(300, parent)]),
        (
            "another serial",
            now(300, 3011, false, false),
            vec![(300, parent)],
        ),
        ("reparented", now(1, 3010, false, false), vec![]),
        ("a trustd", now(300, 3010, false, true), vec![(300, parent)]),
        (
            "run by a trustd",
            now(300, 3010, false, false),
            vec![(300, trustd)],
        ),
    ] {
        assert!(!may_signal(&listed, &now, &mut read(&table)), "{why}");
    }
    let tied = Proc {
        tie: vec![(300, 3000), (200, 2000)],
        ..listed.clone()
    };
    assert!(may_signal(
        &tied,
        &now(300, 3010, false, false),
        &mut read(&[(300, parent), (200, seed)])
    ));
    for (why, table) in [
        (
            "another process at its parent's pid",
            vec![(300, now(200, 3001, false, false)), (200, seed)],
        ),
        ("its parent unread", vec![(200, seed)]),
        (
            "its parent under another",
            vec![(300, now(1, 3000, false, false)), (200, seed)],
        ),
        (
            "another process at its parent's parent's pid",
            vec![(300, parent), (200, now(1, 2001, false, false))],
        ),
        ("its parent's parent unread", vec![(300, parent)]),
    ] {
        assert!(
            !may_signal(&tied, &now(300, 3010, false, false), &mut read(&table)),
            "tied by its parents alone, {why}"
        );
    }
    let unread = Proc {
        serial: None,
        ..listed
    };
    assert!(
        !may_signal(
            &unread,
            &now(300, 3010, false, false),
            &mut read(&[(300, parent)])
        ),
        "a row whose serial was not read"
    );
}
