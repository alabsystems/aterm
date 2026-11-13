// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(at: u64, pid: u32, prev: PrevRun) -> Row {
    Row {
        at,
        pid,
        started: at - 1,
        started_us: None,
        build: 1_790_305_290,
        version: "0.93.0".into(),
        launch: LaunchKind::Cold,
        prev,
        keeper: None,
        restores: None,
    }
}

/// A launch records its keeper link (`keeper=`) before any restores; a row without it
/// (every older row) still parses, and a word this build cannot read refuses the row.
#[test]
fn a_launch_s_keeper_link_rides_the_row() {
    for keeper in [
        KeeperAtLaunch::Off,
        KeeperAtLaunch::Absent,
        KeeperAtLaunch::On,
        KeeperAtLaunch::Armed,
    ] {
        let mut r = row(1_790_500_000, 88, prev(EndClass::Killed));
        r.keeper = Some(keeper);
        r.restores = Some(Restores {
            exact: 1,
            ..Restores::default()
        });
        let line = r.to_line();
        let word = format!(" keeper={} restored=1:0:0", keeper.token());
        assert!(line.contains(&word), "{line}");
        assert_eq!(Row::parse(&line), Some(r.clone()), "{line}");
        assert_eq!(KeeperAtLaunch::parse(keeper.token()), Some(keeper));
        let older = line.replace(&format!(" keeper={}", keeper.token()), "");
        assert_eq!(
            Row::parse(&older).map(|row| row.keeper),
            Some(None),
            "an older row"
        );
        let unknown = line.replace(&format!("keeper={}", keeper.token()), "keeper=maybe");
        assert_eq!(Row::parse(&unknown), None, "{unknown}");
    }
    assert!(!KeeperAtLaunch::Off.link_on());
    assert!(KeeperAtLaunch::Absent.link_on());
    assert!(KeeperAtLaunch::Armed.link_on());
}

fn prev(class: EndClass) -> PrevRun {
    PrevRun {
        pid: Some(7641),
        build: Some(1_790_000_000),
        version: Some("0.92.0".into()),
        class,
        uptime: Some(Uptime {
            secs: 172_800,
            at_least: true,
        }),
        stall: Tri::Yes,
        pressure: Pressure::Critical,
        trial: 2,
    }
}

#[test]
fn a_row_round_trips_through_its_line_for_every_class() {
    for class in [
        EndClass::Clean,
        EndClass::Handoff,
        EndClass::Killed,
        EndClass::Signal(11),
        EndClass::Panic,
        EndClass::Running,
        EndClass::Unknown,
        EndClass::None,
    ] {
        let r = row(1_790_500_000, 123, prev(class));
        let line = r.to_line();
        assert!(!line.contains('\n'));
        assert_eq!(Row::parse(&line), Some(r), "{line}");
    }
    let mut exact = row(1_790_500_000, 9, PrevRun::none());
    exact.prev.uptime = Some(Uptime {
        secs: 5,
        at_least: false,
    });
    exact.launch = LaunchKind::Successor;
    assert_eq!(Row::parse(&exact.to_line()), Some(exact));
}

/// A row carries the kernel's birth microseconds (`started_us=`) beside its
/// second under the same row version; an older row without the key still
/// parses (`None`), and an out-of-range value rejects the row.
#[test]
fn a_row_names_its_process_to_the_microsecond() {
    let mut r = row(1_790_500_000, 4242, prev(EndClass::Clean));
    r.started_us = Some(123_456);
    let line = r.to_line();
    assert!(
        line.starts_with(&format!(
            "v={ROW_VERSION} at=1790500000 pid=4242 started=1790499999 started_us=123456 "
        )),
        "{line}"
    );
    assert_eq!(Row::parse(&line), Some(r.clone()));
    let older = line.replace(" started_us=123456", "");
    assert_eq!(Row::parse(&older).map(|row| row.started_us), Some(None));
    assert_eq!(Row::parse(&line.replace("123456", "1000000")), None);
    assert_eq!(Row::parse(&line.replace("123456", "x")), None);

    assert!(r.is_process(4242, 1_790_499_999, Some(123_456)));
    assert!(
        !r.is_process(4242, 1_790_499_999, Some(654_321)),
        "a pid reused within the second"
    );
    assert!(r.is_process(4242, 1_790_499_999, None), "the second alone");
    assert!(!r.is_process(4242, 1_790_499_998, Some(123_456)));
    assert!(!r.is_process(4243, 1_790_499_999, Some(123_456)));
    let older = Row::parse(&older).expect("an older row");
    assert!(older.is_process(4242, 1_790_499_999, Some(654_321)));
}

/// A launch that recovered sessions records how their screens came back
/// (`restored=<exact>:<screen>:<meta>`); a row without it (every older row, and a
/// launch that recovered nothing) still parses, and a token this build cannot read
/// refuses the row rather than misread it.
#[test]
fn a_recovery_s_restores_ride_the_row() {
    let mut r = row(1_790_500_000, 77, prev(EndClass::Killed));
    r.restores = Some(Restores {
        exact: 3,
        screen: 2,
        meta: 1,
        in_flight_lost: 4096,
        tail_overflows: 1,
        marks_lost: 2,
        resizes_unjournaled: 0,
        history_lost: 1200,
        history_short: 2,
        whys: vec![
            ("in-flight".into(), 1),
            ("no-region".into(), 1),
            ("tail-overflow".into(), 1),
        ],
        refused: 0,
    });
    let line = r.to_line();
    let tokens = " restored=3:2:1 restore_lost=4096:1:2:0 \
                  restore_why=in-flight:1,no-region:1,tail-overflow:1 restore_history=1200:2";
    assert!(line.ends_with(tokens), "{line}");
    assert_eq!(Row::parse(&line), Some(r.clone()));

    let mut exact = r.clone();
    exact.restores = Some(Restores {
        exact: 2,
        ..Restores::default()
    });
    let exact_line = exact.to_line();
    assert!(
        exact_line
            .ends_with(" restored=2:0:0 restore_lost=0:0:0:0 restore_why=- restore_history=0:0"),
        "{exact_line}"
    );
    assert_eq!(Row::parse(&exact_line), Some(exact));

    let older = line.replace(tokens, "");
    assert_eq!(
        Row::parse(&older).map(|row| row.restores),
        Some(None),
        "an older row"
    );
    let rungs_only = line.replace(tokens, " restored=3:2:1");
    assert_eq!(
        Row::parse(&rungs_only).and_then(|row| row.restores),
        Some(Restores {
            exact: 3,
            screen: 2,
            meta: 1,
            ..Restores::default()
        }),
        "a row with the rungs alone"
    );
    let before_ring = line.replace(" restore_history=1200:2", "");
    assert_eq!(
        Row::parse(&before_ring).and_then(|row| row.restores),
        Some(Restores {
            history_lost: 0,
            history_short: 0,
            ..r.restores.clone().expect("restores")
        }),
        "a row from before the history ring"
    );
    for (key, good, bad) in [
        ("restored", "3:2:1", "3:1"),
        ("restored", "3:2:1", "3:1:0:9"),
        ("restored", "3:2:1", "3:x:0"),
        ("restored", "3:2:1", ""),
        ("restore_lost", "4096:1:2:0", "4096:1:2"),
        ("restore_lost", "4096:1:2:0", "4096:1:2:0:5"),
        (
            "restore_why",
            "in-flight:1,no-region:1,tail-overflow:1",
            "in-flight",
        ),
        (
            "restore_why",
            "in-flight:1,no-region:1,tail-overflow:1",
            ":1",
        ),
        (
            "restore_why",
            "in-flight:1,no-region:1,tail-overflow:1",
            "in-flight:x",
        ),
        ("restore_history", "1200:2", "1200"),
        ("restore_history", "1200:2", "1200:2:3"),
        ("restore_history", "1200:2", "x:2"),
    ] {
        let bad_line = line.replace(&format!("{key}={good}"), &format!("{key}={bad}"));
        assert_ne!(bad_line, line, "{key}={bad}");
        assert_eq!(Row::parse(&bad_line), None, "{bad_line}");
    }
}

/// The sessions a keeper offered and the launch refused ride the row
/// (`restore_refused=`), a recovery that refused every offer included; a row without the
/// word (every older row, and one that refused none) reads 0, and one this build cannot
/// read refuses the row.
#[test]
fn a_recovery_s_refusals_ride_the_row() {
    let mut r = row(1_790_500_000, 77, prev(EndClass::Killed));
    r.restores = Some(Restores {
        exact: 3,
        refused: 1,
        ..Restores::default()
    });
    let line = r.to_line();
    assert!(
        line.ends_with(" restored=3:0:0 restore_lost=0:0:0:0 restore_why=- restore_history=0:0 restore_refused=1"),
        "{line}"
    );
    assert_eq!(Row::parse(&line), Some(r.clone()));
    assert_eq!(r.restores.as_ref().map(Restores::recovered), Some(3));

    let mut all_refused = r.clone();
    all_refused.restores = Some(Restores {
        refused: 4,
        ..Restores::default()
    });
    let all_line = all_refused.to_line();
    assert!(all_line.contains(" restored=0:0:0 "), "{all_line}");
    assert_eq!(Row::parse(&all_line), Some(all_refused));

    let older = line.replace(" restore_refused=1", "");
    assert_eq!(
        Row::parse(&older).and_then(|row| row.restores),
        Some(Restores {
            exact: 3,
            ..Restores::default()
        }),
        "a row from before the word"
    );
    let bad = line.replace("restore_refused=1", "restore_refused=x");
    assert_eq!(Row::parse(&bad), None, "{bad}");
}

#[test]
fn a_line_is_one_line_whatever_the_version_string_holds() {
    let mut r = row(10, 1, PrevRun::none());
    r.version = "0.93.0 injected=1\nv=1".into();
    r.prev.version = Some("a=b".into());
    let line = r.to_line();
    assert!(!line.contains('\n'));
    let back = Row::parse(&line).expect("a sanitized row parses");
    assert_eq!(back.version, "?");
    assert_eq!(back.prev.version, None);
}

#[test]
fn malformed_and_foreign_lines_are_skipped_not_fatal() {
    let good = row(20, 2, prev(EndClass::Killed)).to_line();
    let text = format!(
        "garbage\nv=2 at=1\n{}\nv=1 at=x pid=1\n\n{good}\n",
        row(10, 1, PrevRun::none()).to_line()
    );
    let rows = parse_rows(&text);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].prev.class, EndClass::Killed);
}

#[test]
fn the_summary_counts_ends_and_names_the_last_unexpected_one() {
    let rows = vec![
        row(1_790_000_000, 1, PrevRun::none()),
        row(1_790_000_100, 2, prev(EndClass::Clean)),
        row(1_790_000_200, 3, prev(EndClass::Killed)),
        row(1_790_000_300, 4, prev(EndClass::Clean)),
        row(1_790_000_400, 5, prev(EndClass::Signal(6))),
        row(1_790_000_500, 6, prev(EndClass::Handoff)),
    ];
    let s = summarize(&rows);
    assert_eq!(s.rows, 6);
    assert_eq!(
        s.ends,
        vec![
            (EndClass::Clean, 2),
            (EndClass::Handoff, 1),
            (EndClass::Killed, 1),
            (EndClass::Signal(6), 1),
        ]
    );
    assert_eq!(s.unexpected_stalled, 2);
    assert_eq!(s.unexpected_critical, 2);
    assert_eq!(s.last_unexpected.as_ref().map(|r| r.pid), Some(5));
    let (_, unexpected) = doctor_lines(&rows, Path::new("/x/recovery-ledger.log"));
    assert!(unexpected);
}

#[test]
fn an_empty_or_all_clean_ledger_is_no_note() {
    let (_, unexpected) = doctor_lines(&[], Path::new("/x/l"));
    assert!(!unexpected);
    let rows = vec![row(1, 1, PrevRun::none()), row(2, 2, prev(EndClass::Clean))];
    let (_, unexpected) = doctor_lines(&rows, Path::new("/x/l"));
    assert!(!unexpected);
}

#[cfg(unix)]
fn scratch(tag: &str) -> crate::ScratchDir {
    crate::scratch_dir(&format!("ledger-{tag}"))
}

#[cfg(unix)]
#[test]
fn checked_history_distinguishes_absence_and_empty_from_unreadable_or_malformed() {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = scratch("checked");
    let path = ledger_path(&dir);
    assert_eq!(
        read_rows_checked(&path).unwrap(),
        Checked::default(),
        "absent is normal"
    );
    std::fs::create_dir(&path).unwrap();
    assert!(
        read_rows_checked(&path).is_err(),
        "a directory is not history"
    );
    assert!(
        read_rows(&path).is_empty(),
        "legacy reader remains tolerant"
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, "").unwrap();
    assert_eq!(read_rows_checked(&path).unwrap(), Checked::default());
    let expected = row(1, 1, PrevRun::none());
    std::fs::write(&path, format!("{}\n", expected.to_line())).unwrap();
    assert_eq!(
        read_rows_checked(&path).unwrap(),
        Checked {
            rows: vec![expected.clone()],
            skipped: 0
        }
    );
    std::fs::write(&path, format!("{}\nunreadable row\n", expected.to_line())).unwrap();
    assert_eq!(
        read_rows_checked(&path).unwrap(),
        Checked {
            rows: vec![expected.clone()],
            skipped: 1
        },
        "an unreadable row is skipped and counted"
    );
    assert_eq!(
        read_rows(&path),
        vec![expected.clone()],
        "legacy parsing is unchanged"
    );

    let fifo = dir.join("replacement.fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: an owned scratch pathname, a NUL-terminated name and mode only.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let reader = open_for_read(&fifo).unwrap();
    assert!(append_row(&fifo, &expected).is_err());
    assert!(read_opened_text(reader).is_err());
    assert!(read_rows_checked(&fifo).is_err());

    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_READ_BYTES + 4096)
        .unwrap();
    let opened = open_for_read(&path).unwrap();
    assert!(opened.metadata().unwrap().len() > MAX_READ_BYTES);
    std::fs::write(&path, "unreadable row\n").unwrap();
    assert_eq!(read_opened_text(opened).unwrap(), "unreadable row\n");
    assert_eq!(read_rows_checked(&path).unwrap().skipped, 1);

    let valid = format!("{}\n", expected.to_line());
    std::fs::write(&path, &valid).unwrap();
    let opened = open_for_read(&path).unwrap();
    std::fs::rename(&path, dir.join("retired.log")).unwrap();
    std::fs::write(&path, "unreadable row\n").unwrap();
    assert_eq!(read_opened_text(opened).unwrap(), valid);
    assert_eq!(
        read_rows_checked(&path).unwrap(),
        Checked {
            rows: Vec::new(),
            skipped: 1
        }
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A row a newer build wrote, with an end this build has no word for (`prev=watchdog`), is
/// skipped and counted, and the rows around it still read. NEGATIVE CONTROL: the same row
/// with an end this build knows reads in full.
#[cfg(unix)]
#[test]
fn a_newer_builds_end_is_skipped_and_counted_and_the_rest_reads() {
    let dir = scratch("newer-end");
    let path = ledger_path(&dir);
    let first = row(1, 1, PrevRun::none());
    let last = row(3, 3, PrevRun::none());
    let known = row(2, 2, PrevRun::none()).to_line();
    let token = format!(" prev={} ", PrevRun::none().class.token());
    assert!(known.contains(&token), "{known}");
    let newer = known.replacen(&token, " prev=watchdog ", 1);
    let write = |middle: &str| {
        std::fs::write(
            &path,
            format!("{}\n{middle}\n{}\n", first.to_line(), last.to_line()),
        )
        .unwrap();
    };
    write(&newer);
    assert_eq!(
        read_rows_checked(&path).unwrap(),
        Checked {
            rows: vec![first.clone(), last.clone()],
            skipped: 1
        }
    );
    write(&known);
    let read = read_rows_checked(&path).unwrap();
    assert_eq!((read.rows.len(), read.skipped), (3, 0), "{read:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[cfg(unix)]
#[test]
fn append_creates_a_private_file_and_keeps_rows_in_order() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("append");
    let path = ledger_path(&dir);
    append_row(&path, &row(1, 1, PrevRun::none())).expect("append 1");
    append_row(&path, &row(2, 2, prev(EndClass::Clean))).expect("append 2");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let rows = read_rows(&path);
    assert_eq!(rows.iter().map(|r| r.pid).collect::<Vec<_>>(), vec![1, 2]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_ledger_past_the_trim_mark_keeps_the_newest_rows() {
    let dir = scratch("trim");
    let path = ledger_path(&dir);
    let mut n: u32 = 0;
    let mut grown = 0;
    loop {
        n += 1;
        append_row(&path, &row(u64::from(n) + 10, n, prev(EndClass::Clean))).expect("append");
        let len = std::fs::metadata(&path).expect("meta").len();
        if len < grown {
            let rows = read_rows(&path);
            assert_eq!(
                rows.len(),
                KEEP_ROWS,
                "a trim keeps exactly the newest rows"
            );
            assert_eq!(
                rows.last().map(|r| r.pid),
                Some(n),
                "the newest row survives"
            );
            assert_eq!(rows.first().map(|r| r.pid), Some(n + 1 - KEEP_ROWS as u32));
            break;
        }
        grown = len;
        assert!(n < 10_000, "the ledger never trimmed");
    }
    assert!(std::fs::metadata(&path).expect("meta").len() <= TRIM_AT_BYTES);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A ledger lock is free for a second open file description the instant its
/// guard drops, even while a `dup`'d copy of the first description lives (as
/// in a concurrently forked child before `exec`).
#[cfg(unix)]
#[test]
fn a_released_ledger_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = scratch("lock-copy");
    let path = ledger_path(&dir);
    let open = || {
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .expect("open the ledger")
    };
    let file = open();
    let held = lock_briefly(&file).expect("a free ledger locks");
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe of a fresh description that is refused: it never holds the lock"
    )]
    let refused = open().try_lock().is_err();
    assert!(refused, "a held ledger lock refuses a second description");
    let childs_copy = file.try_clone().expect("dup the ledger's descriptor");
    drop(held);
    drop(file);
    let probe = open();
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe of a fresh description, let go by unlock() below"
    )]
    let free = probe.try_lock().is_ok();
    let _ = probe.unlock();
    assert!(
        free,
        "a released ledger lock must be free while a copy of its descriptor lives"
    );
    drop(childs_copy);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_symlink_in_its_place_is_never_read_or_followed() {
    let dir = scratch("symlink");
    let path = ledger_path(&dir);
    let target = dir.join("elsewhere");
    std::fs::write(
        &target,
        format!("{}\n", row(1, 1, PrevRun::none()).to_line()),
    )
    .expect("target");
    std::os::unix::fs::symlink(&target, &path).expect("symlink");
    assert!(read_rows(&path).is_empty(), "a symlink is not the ledger");
    assert!(
        append_row(&path, &row(2, 2, PrevRun::none())).is_err(),
        "an append never writes through a symlink"
    );
    assert_eq!(
        std::fs::read_to_string(&target)
            .expect("target")
            .lines()
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// NEGATIVE CONTROL: duplicate field keys and unclean tokens (including embedded `=` in a
/// value or `restore_why` word) are rejected by `Row::parse`, while the canonical single-key
/// row parses cleanly; and `trim_locked` refuses to overwrite through a pre-planted symlink at
/// the `.recovery-ledger.log.trim` staging path.
#[test]
fn duplicate_keys_and_unclean_tokens_are_rejected_and_trim_refuses_symlinks() {
    let mut base = row(1_790_600_000, 42, prev(EndClass::Killed));
    base.restores = Some(Restores {
        exact: 1,
        whys: vec![("in-flight".to_string(), 1)],
        ..Restores::default()
    });
    let good = base.to_line();
    assert_eq!(Row::parse(&good), Some(base.clone()));

    let dup = format!("{good} prev=clean");
    assert_eq!(
        Row::parse(&dup),
        None,
        "duplicate key must reject the ambiguous row"
    );

    let embedded_eq = good.replace("version=0.93.0", "version=0.93.0=1");
    assert_ne!(embedded_eq, good);
    assert_eq!(
        Row::parse(&embedded_eq),
        None,
        "value containing '=' must be rejected"
    );

    let bad_why = good.replace("restore_why=in-flight:1", "restore_why=in=flight:1");
    assert_ne!(bad_why, good);
    assert_eq!(
        Row::parse(&bad_why),
        None,
        "restore_why word containing '=' must be rejected"
    );

    #[cfg(unix)]
    {
        let dir = scratch("trim-symlink");
        let path = ledger_path(&dir);
        append_row(&path, &base).expect("append initial row");
        let outside = dir.join("outside-victim");
        std::fs::write(&outside, "keep me\n").expect("victim");
        let tmp = path.with_file_name(format!(".{LEDGER_FILE_NAME}.trim"));
        std::os::unix::fs::symlink(&outside, &tmp).expect("plant symlink");
        trim_locked(&path);
        assert_eq!(
            std::fs::read_to_string(&outside).expect("victim intact"),
            "keep me\n"
        );
        assert_eq!(read_rows(&path), vec![base]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
