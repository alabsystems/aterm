// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn scratch(label: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("atc-waits-{label}-"))
        .tempdir()
        .expect("a scratch dir")
}

/// A mark reads back while its waiter holds it, says what it last said, and is gone once
/// it drops; a mark whose lock nobody holds (its waiter died) is no wait.
#[test]
fn a_mark_is_a_wait_while_its_waiter_holds_it() {
    let dir = scratch("live");
    assert!(waiting(dir.path()).is_empty());
    let mut mark = Mark::at(dir.path(), "lab linux proof waits for the lane").expect("laid");
    let seen = waiting(dir.path());
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(
        (seen[0].pid, seen[0].text.as_str()),
        (std::process::id(), "lab linux proof waits for the lane")
    );
    mark.say("still waiting\tfor pid 7 (lab linux exec)");
    assert_eq!(
        waiting(dir.path())[0].text,
        "still waiting for pid 7 (lab linux exec)"
    );
    assert_eq!(waiting(dir.path())[0].stuck, None);
    let stuck = Stuck {
        holders: vec![7],
        span: Duration::from_secs(900),
    };
    mark.say_stuck(
        "waiting for pid 7, which has made no progress",
        Some(&stuck),
    );
    let seen = waiting(dir.path());
    assert_eq!(
        (seen[0].text.as_str(), seen[0].stuck.as_ref()),
        (
            "waiting for pid 7, which has made no progress",
            Some(&stuck)
        )
    );
    mark.say("waiting for pid 7, which works again");
    assert_eq!(waiting(dir.path())[0].stuck, None, "a record said anew");
    let path = mark.laid[0].path.clone();
    drop(mark);
    assert!(waiting(dir.path()).is_empty());
    assert!(!path.exists(), "the mark goes with its wait");

    std::fs::write(
        dir.path().join("1-0.wait"),
        record(1, 0, "a waiter that died", None),
    )
    .expect("a dead waiter's mark");
    assert!(
        waiting(dir.path()).is_empty(),
        "a mark nobody holds is no wait"
    );
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt as _;
        let fifo = dir.path().join("fifo-0.wait");
        let c_fifo = CString::new(fifo.as_os_str().as_bytes()).expect("cstr");
        // SAFETY: `c_fifo` is a live NUL-terminated path in our scratch directory; `mkfifo` retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0, "mkfifo");
        assert_eq!(live(&fifo), None);
        let held = Mark::at(dir.path(), "real waiter").expect("held");
        let link = dir.path().join("link-0.wait");
        std::os::unix::fs::symlink(&held.laid[0].path, &link).expect("symlink");
        assert_eq!(live(&link), None);
        assert_eq!(waiting(dir.path()).len(), 1);
        drop(held);
    }
}

/// A mark laid in two directories is a wait in each, says the same in each, and goes from
/// each with its wait; a directory that takes none leaves it laid in the other, and one laid
/// in none is none. Before a wait is marked in this user's directory its dead marks go: a
/// mark nobody holds, and one still being laid once it is old; a live mark, and one being
/// laid now, stay.
#[test]
fn a_mark_is_laid_in_each_directory_and_the_dead_are_swept() {
    let a = scratch("each-a");
    let b = scratch("each-b");
    let gone = a.path().join("let-go");
    let dirs = [a.path().to_path_buf(), gone.clone(), b.path().to_path_buf()];
    let mut mark = Mark::in_each(&dirs, "waits for the lane").expect("laid");
    mark.say("still waits for the lane");
    for d in [a.path(), b.path()] {
        let seen = waiting(d);
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].text, "still waits for the lane");
    }
    assert!(Mark::in_each(std::slice::from_ref(&gone), "x").is_none());
    let dead = a.path().join("1-0.wait");
    std::fs::write(&dead, record(1, 0, "a waiter that died", None)).expect("dead");
    let laying = a.path().join("1-1.taking");
    std::fs::write(&laying, "").expect("one being laid");
    sweep(a.path());
    assert!(!dead.exists(), "a dead mark stays");
    assert!(laying.exists(), "a mark being laid now went");
    assert_eq!(waiting(a.path()).len(), 1, "the live mark went");
    let old = std::time::SystemTime::now() - LAYING - Duration::from_secs(1);
    std::fs::File::options()
        .write(true)
        .open(&laying)
        .and_then(|f| f.set_modified(old))
        .expect("aged");
    sweep(a.path());
    assert!(!laying.exists(), "a dead waiter's half-laid mark stays");
    drop(mark);
    assert!(waiting(a.path()).is_empty() && waiting(b.path()).is_empty());
    assert_eq!(std::fs::read_dir(b.path()).expect("b").count(), 0);
}

#[cfg(any(unix, windows))]
#[test]
fn a_live_marks_identity_survives_words_and_elapsed_duration() {
    let dir = scratch("identity");
    let mut mark = Mark::at(dir.path(), "waiting behind a working holder").unwrap();
    let first = live(&mark.laid[0].path).unwrap();
    assert!(first.same_mark(&first));
    for seconds in [900, 901] {
        mark.say_stuck(
            &format!("the holder has made no progress for {seconds} seconds"),
            Some(&Stuck {
                holders: vec![7],
                span: Duration::from_secs(seconds),
            }),
        );
        let next = live(&mark.laid[0].path).unwrap();
        assert!(first.same_mark(&next));
        assert!(next.same_mark(&first));
        let mut other_record = next.clone();
        other_record.pid += 1;
        assert!(!next.same_mark(&other_record));
        other_record = next.clone();
        other_record.since += 1;
        assert!(!next.same_mark(&other_record));
    }
}

#[cfg(any(unix, windows))]
#[test]
fn distinct_live_files_and_paths_never_share_a_mark_identity() {
    let dir = scratch("distinct-marks");
    let mut first = Mark::at(dir.path(), "same wait").unwrap();
    let mut other = Mark::at(dir.path(), "same wait").unwrap();
    // Make the otherwise coarsely timestamped records exactly equal without a wall-clock race.
    first.since = 1;
    other.since = 1;
    first.say("same wait");
    other.say("same wait");
    let before = live(&first.laid[0].path).unwrap();
    let simultaneous = live(&other.laid[0].path).unwrap();
    assert_eq!(
        (before.pid, before.since),
        (simultaneous.pid, simultaneous.since)
    );
    assert!(!before.same_mark(&simultaneous));

    let alias = dir.path().join("alias.wait");
    std::fs::hard_link(&first.laid[0].path, &alias).unwrap();
    let aliased = live(&alias).unwrap();
    assert_eq!(
        before.identity.as_ref().unwrap().file,
        aliased.identity.as_ref().unwrap().file
    );
    assert!(
        !before.same_mark(&aliased),
        "a file alias is not the same mark path"
    );
    std::fs::remove_file(alias).unwrap();

    let path = first.laid[0].path.clone();
    std::fs::rename(&other.laid[0].path, &path).unwrap();
    other.laid[0].path = path;
    let replaced = live(&other.laid[0].path).unwrap();
    assert_eq!((before.pid, before.since), (replaced.pid, replaced.since));
    assert_eq!(
        before.identity.as_ref().unwrap().path,
        replaced.identity.as_ref().unwrap().path
    );
    assert!(
        !before.same_mark(&replaced),
        "replacing a path cannot inherit its old confirmation"
    );
    // The original open handle remains alive, so its inode cannot have been recycled.
    assert!(first.laid[0].lock.metadata().is_ok());
    drop(other);
    drop(first);
}

#[cfg(any(unix, windows))]
#[test]
fn an_unidentified_record_cannot_confirm_any_mark() {
    let dir = scratch("unidentified");
    let mark = Mark::at(dir.path(), "waiting").unwrap();
    let current = live(&mark.laid[0].path).unwrap();
    let parsed = parse(&record(current.pid, current.since, &current.text, None)).unwrap();
    assert!(!parsed.same_mark(&parsed));
    assert!(!parsed.same_mark(&current));
    assert!(!current.same_mark(&parsed));
    let mut unavailable = current.clone();
    unavailable.identity = None;
    assert!(!unavailable.same_mark(&current));
    assert!(!unavailable.same_mark(&unavailable));
}

#[test]
fn a_long_text_is_cut_on_a_character_boundary() {
    let stuck = Stuck {
        holders: vec![7, 8],
        span: Duration::from_secs(960),
    };
    for s in [None, Some(&stuck)] {
        let line = record(9, 1, &"é".repeat(4096), s);
        assert!(line.len() <= RECORD as usize, "{}", line.len());
        let back = parse(&line).expect("a record");
        assert!(back.pid == 9 && back.text.starts_with('é'), "{back:?}");
        assert_eq!(back.stuck.as_ref(), s);
    }
    assert!(parse("0\t1\tz").is_none(), "pid 0 is no waiter");
    assert!(parse("+9\t1\tz").is_none());
    assert!(parse("9\t+1\tz").is_none());
    assert!(parse("9\t1\tz\nstuck\t960\t7,8").is_some_and(|w| w.stuck.is_some()));
    assert!(parse("9\t1\tz\nstuck\t+960\t7").is_some_and(|w| w.stuck.is_none()));
    assert!(parse("9\t1\tz\nstuck\t960\t7,+8").is_some_and(|w| w.stuck.is_none()));
    assert!(crate::named_lock::Holder::parse("+9\t1\tv\tc").is_none());
    assert!(crate::named_lock::Holder::parse("9\t+1\tv\tc").is_none());
}

#[test]
fn no_directory_marks_nothing() {
    let dir = scratch("gone");
    let gone = dir.path().join("let-go");
    assert!(Mark::at(&gone, "x").is_err());
    assert!(waiting(&gone).is_empty());
}
