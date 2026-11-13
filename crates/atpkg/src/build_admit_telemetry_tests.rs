// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use super::*;

fn run(ledger: &Path) -> Run {
    Run {
        start_ms: 1_000,
        admitted_ms: Some(4_000),
        what: "targo test -p secret-crate".to_owned(),
        checkout: Some("/Users//me/secret-project".to_owned()),
        ledger: Some(ledger.to_path_buf()),
    }
}

fn facts() -> HostFacts {
    HostFacts {
        host: "m101.local".to_owned(),
        model: "Mac16,7".to_owned(),
        cpu: "Apple M4 Pro".to_owned(),
        cores: 14,
        mem_mib: 24_576,
        os: "macOS 27.0.1".to_owned(),
        aterm: "0.160.0+gtest".to_owned(),
    }
}

fn at(end: BuildEnd) -> Row {
    let f = facts();
    row(
        &run(Path::new("/l")),
        "targo test",
        &end,
        3 << 30,
        10_000,
        &f,
        f.digest(),
    )
}

/// `exit(code)`'s wait status.
fn exited(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

#[test]
fn a_build_row_says_its_wall_wait_peak_and_no_words() {
    let r = at(BuildEnd::Status(exited(0)));
    assert_eq!(r.kind, Kind::Test, "the verb test");
    assert_eq!(r.wall_ms, Fig::Known(6_000), "admission to end");
    assert_eq!(r.wait_ms, Fig::Known(3_000), "admitted less asked");
    assert_eq!(r.peak_bytes, Fig::Known(3 << 30));
    assert_eq!(
        (r.cpu_ms.clone(), r.cache.clone()),
        (Fig::Na, Fig::Unknown("targo".to_owned()))
    );
    assert_eq!((r.label.as_str(), r.host.as_str()), ("targo", "m101"));
    assert_eq!(r.meter, Meter::Sampled);
    let line = r.line();
    assert!(
        !line.contains("secret-crate") && !line.contains("secret-project"),
        "{line}"
    );
    let f = facts();
    let check = row(
        &run(Path::new("/l")),
        "targo trust check",
        &BuildEnd::Unknown,
        0,
        10_000,
        &f,
        f.digest(),
    );
    assert_eq!(check.kind, Kind::Build, "every verb but test");
    assert_eq!(
        check.peak_bytes,
        Fig::Unknown("aterm-capacity".to_owned()),
        "no peak read"
    );
    let unadmitted = Run {
        admitted_ms: None,
        ..run(Path::new("/l"))
    };
    let r = row(
        &unadmitted,
        "targo build",
        &BuildEnd::Unknown,
        1,
        10_000,
        &f,
        f.digest(),
    );
    assert_eq!(
        (r.wall_ms, r.wait_ms),
        (
            Fig::Unknown("aterm-capacity".to_owned()),
            Fig::Unknown("aterm-capacity".to_owned())
        ),
        "no grant read, no clock"
    );
}

/// The stamps are wall-clock times: a clock set back between them gives no interval, never a
/// known 0. NEGATIVE CONTROL: stamps in order give theirs.
#[test]
fn a_clock_set_back_is_no_known_interval() {
    let f = facts();
    let stepped = Run {
        start_ms: 5_000,
        admitted_ms: Some(4_000),
        ..run(Path::new("/l"))
    };
    let r = row(
        &stepped,
        "targo build",
        &BuildEnd::Unknown,
        1,
        3_000,
        &f,
        f.digest(),
    );
    assert_eq!(
        (r.wall_ms, r.wait_ms),
        (
            Fig::Unknown("clock".to_owned()),
            Fig::Unknown("clock".to_owned())
        )
    );
    let r = row(
        &run(Path::new("/l")),
        "targo build",
        &BuildEnd::Unknown,
        1,
        4_000,
        &f,
        f.digest(),
    );
    assert_eq!((r.wall_ms, r.wait_ms), (Fig::Known(0), Fig::Known(3_000)));
}

#[test]
fn each_end_maps_to_its_outcome_and_stop() {
    let words = |end| {
        let r = at(end);
        (r.outcome, r.stop, r.code, r.sig)
    };
    assert_eq!(
        words(BuildEnd::Status(exited(0))),
        (Outcome::Ok, Stop::Exit, Some(0), None)
    );
    assert_eq!(
        words(BuildEnd::Status(exited(101))),
        (Outcome::Fail, Stop::Exit, Some(101), None)
    );
    assert_eq!(
        words(BuildEnd::Status(ExitStatus::from_raw(9))),
        (Outcome::Fail, Stop::Signal, None, Some(9))
    );
    assert_eq!(
        words(BuildEnd::Success(true)),
        (Outcome::Ok, Stop::Unknown, None, None)
    );
    assert_eq!(
        words(BuildEnd::Success(false)),
        (Outcome::Fail, Stop::Unknown, None, None)
    );
    assert_eq!(
        words(BuildEnd::Unknown),
        (Outcome::Unknown, Stop::Unknown, None, None)
    );
    assert_eq!(
        words(BuildEnd::Unfinished),
        (Outcome::Unknown, Stop::Unknown, None, None)
    );
}

#[test]
fn a_run_not_whole_is_known_only_by_a_failing_status() {
    assert_eq!(
        BuildEnd::judged(Some(exited(0)), true),
        BuildEnd::Status(exited(0))
    );
    assert_eq!(
        BuildEnd::judged(Some(exited(0)), false),
        BuildEnd::Unknown,
        "a cancelled run's zero exit is no finished work"
    );
    assert_eq!(
        BuildEnd::judged(Some(ExitStatus::from_raw(2)), false),
        BuildEnd::Status(ExitStatus::from_raw(2)),
        "the signal that cancelled it"
    );
    assert_eq!(BuildEnd::judged(None, true), BuildEnd::Unknown);
    assert_eq!(BuildEnd::of_roots(3, 0, 0), BuildEnd::Unknown);
    assert_eq!(BuildEnd::of_roots(3, 3, 0), BuildEnd::Success(true));
    assert_eq!(BuildEnd::of_roots(3, 3, 1), BuildEnd::Success(false));
    assert_eq!(
        BuildEnd::of_roots(3, 1, 1),
        BuildEnd::Success(false),
        "a failure is known though roots are missing"
    );
    assert_eq!(
        BuildEnd::of_roots(3, 2, 0),
        BuildEnd::Unfinished,
        "a root never validated: not known to have succeeded"
    );
    // The book's success is the old one at every site.
    assert!(BuildEnd::judged(Some(exited(0)), true).succeeded());
    assert!(!BuildEnd::judged(Some(exited(0)), false).succeeded());
    assert!(!BuildEnd::judged(Some(exited(1)), true).succeeded());
    assert!(!BuildEnd::Unknown.succeeded());
    // The epoch table's old rule, `completed != 0 && failed == 0`, at each count.
    for roots in 0..4 {
        for completed in 0..=roots {
            for failed in 0..=completed {
                assert_eq!(
                    BuildEnd::of_roots(roots, completed, failed).succeeded(),
                    completed != 0 && failed == 0,
                    "{roots} {completed} {failed}"
                );
            }
        }
    }
}

#[test]
fn only_this_users_ledger_writes_a_row() {
    let tmp = aterm_tempfile::tempdir().expect("tempdir");
    let users = tmp.path().join("ledger");
    std::fs::create_dir(&users).expect("ledger");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir(&scratch).expect("scratch");
    let dir = tmp.path().join("telemetry");
    let lost = aterm_telemetry::direct::Lost::new();
    let end = BuildEnd::Status(exited(3));
    let write = |row: &Row| aterm_telemetry::direct::write(dir.clone(), &facts(), row, &lost);
    assert!(
        !ended_in(&run(&scratch), "targo test", &end, 1, &users, write),
        "a scratch ledger writes no row"
    );
    assert!(!dir.exists(), "nothing was written");
    let none = Run {
        ledger: None,
        ..run(&users)
    };
    assert!(!ended_in(&none, "targo test", &end, 1, &users, write));
    let beside = tmp.path().join("ledger-2");
    std::fs::create_dir(&beside).expect("beside");
    assert!(
        !ended_in(&run(&beside), "targo test", &end, 1, &users, write),
        "a folder whose name the user's begins is no folder of it"
    );
    assert!(!dir.exists(), "nothing was written");
    // A cohort claim's file lies in a subfolder of the ledger's.
    let cohort = users.join("active.noindex").join("key");
    std::fs::create_dir_all(&cohort).expect("cohort");
    assert!(ended_in(
        &run(&cohort),
        "targo build",
        &end,
        1,
        &users,
        write
    ));
    assert!(ended_in(&run(&users), "targo test", &end, 1, &users, write));
    let read = aterm_telemetry::log::read(&dir, 4, None).expect("read");
    assert_eq!(read.rows.len(), 2);
    assert_eq!(read.rows[0].kind, Kind::Build);
    let r = &read.rows[1];
    assert_eq!(
        (&r.kind, r.outcome, r.code, r.label.as_str()),
        (&Kind::Test, Outcome::Fail, Some(3), "targo")
    );
}
