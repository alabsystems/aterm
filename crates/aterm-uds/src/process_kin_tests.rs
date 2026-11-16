// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn frame(argc: i32, words: &[&[u8]]) -> Vec<u8> {
    let mut bytes = argc.to_ne_bytes().to_vec();
    bytes.extend_from_slice(b"/bin/aterm\0\0\0");
    for w in words {
        bytes.extend_from_slice(w);
        bytes.push(0);
    }
    bytes
}

#[test]
fn only_the_marker_vars_are_read_and_an_empty_environment_is_withheld() {
    let words: [&[u8]; 7] = [
        b"/bin/aterm",
        b"ATERM_VERIFY_RUN=argv-not-env",
        b"HOME=/h",
        b"ATERM_VERIFY_RUN=71-1792163800331520",
        b"ATERM_PARENT_SESSION_ID=s7",
        b"ATERM_CHILD=1",
        b"ATERM_TRUST_CHECK=0f1e",
    ];
    let read = markers_from_procargs2(&frame(2, &words)).expect("a buffer");
    assert_eq!(
        read,
        Markers {
            withheld: false,
            parent_session: Some("s7".into()),
            child: Some("1".into()),
            verify_run: Some("71-1792163800331520".into()),
            trust_check: Some("0f1e".into()),
        }
    );
    let environ: Vec<u8> = words[1..].join(&0u8);
    assert_eq!(
        markers_from_environ(&environ).expect("within the bound"),
        Markers {
            verify_run: Some("argv-not-env".into()),
            ..read.clone()
        },
        "an environ file reads as the same words"
    );
    let shifted = markers_from_procargs2(&frame(1, &words[..2])).expect("a buffer");
    assert_eq!(shifted.verify_run.as_deref(), Some("argv-not-env"));
    let withheld = markers_from_procargs2(&frame(1, &[b"/bin/sleep", b""])).expect("a buffer");
    assert!(withheld.withheld, "{withheld:?}");
    assert_eq!(withheld.verify_run, None);
    assert_eq!(markers_from_procargs2(&[1, 0]), None, "no argc");
    assert_eq!(
        markers_from_procargs2(&frame(9, &words)),
        None,
        "argc past the words"
    );
}

/// A read between two identity reads that disagree is refused: an exit, a reused pid, or an
/// exec where the identity has a pid version. Linux's identity has none, so an exec there
/// is one identity and the read stands. Negative control: compare the pid versions alone,
/// and on Linux, where both are `None`, the reused pid's read stands.
#[test]
fn markers_read_across_an_exec_are_refused() {
    let id = |birth, idversion| super::super::Identity {
        pid: 71,
        birth: super::super::Birth(birth),
        idversion,
    };
    let script = |reads: Vec<Option<super::super::Identity>>| {
        let mut reads = reads.into_iter();
        bracketed(71, move |_| reads.next().flatten(), || Some("env"))
    };
    let apple = |idversion| Some(id(1_000_002, Some(idversion)));
    assert_eq!(script(vec![apple(7), apple(7)]), Some("env"));
    assert_eq!(script(vec![apple(7), apple(8)]), None, "an exec");
    assert_eq!(script(vec![apple(7), None]), None, "an exit");
    let linux = |ticks| Some(id(ticks, None));
    assert_eq!(
        script(vec![linux(51_832_760), linux(51_832_901)]),
        None,
        "a pid reused between the reads"
    );
    assert_eq!(
        script(vec![linux(51_832_760), linux(51_832_760)]),
        Some("env"),
        "an exec under one pid and start is one identity"
    );
}

/// Set on [`a_marked_child_stand_in`] to make it the marked child.
const MARKED_CHILD_ENV: &str = "ATERM_UDS_MARKED_CHILD";

/// The line [`a_marked_child_stand_in`] writes once it runs.
const MARKED_CHILD_RUNS: &str = "the marked child runs";

/// The one-minute hang detector of the marked child's waits.
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// The child [`a_spawned_childs_markers_are_read_off_its_exec_environment`] reads: with
/// [`MARKED_CHILD_ENV`] set it writes [`MARKED_CHILD_RUNS`] and returns once its stdin
/// closes, which its parent's end closes too; without it, it returns.
#[test]
fn a_marked_child_stand_in() {
    if std::env::var_os(MARKED_CHILD_ENV).is_none() {
        return;
    }
    // Keep the readiness proof on its own line even after libtest's single-thread prefix.
    println!("\n{MARKED_CHILD_RUNS}");
    let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0u8; 1]);
}

/// A child kept from this process's `ATERM_*` settings goes without every `ATERM_*` name,
/// each marker but the verify run's mark among them, and keeps every other name
/// ([`stripped_from_child`]). Negative controls: the rule without its [`VERIFY_RUN_VAR`]
/// exemption strips the mark, one that only spares the mark strips `PATH`, and a prefix
/// without its `_` strips `ATERMX`.
#[test]
fn a_child_kept_from_aterm_settings_goes_without_them_but_the_verify_runs_mark() {
    use std::ffi::OsStr;
    for marker in MARKER_VARS {
        assert_eq!(
            stripped_from_child(OsStr::new(marker)),
            marker != VERIFY_RUN_VAR,
            "{marker}"
        );
    }
    for (name, stripped) in [
        ("ATERM_LOG", true),
        ("ATERM_", true),
        ("ATERM_VERIFY_RUNS", true),
        ("PATH", false),
        ("HOME", false),
        ("ATERM", false),
        ("ATERMX", false),
        ("XATERM_CHILD", false),
        ("aterm_child", false),
    ] {
        assert_eq!(stripped_from_child(OsStr::new(name)), stripped, "{name}");
    }
}

/// A child's four markers are read off the kernel's copy of its exec environment between two
/// identity reads, and nothing once it has exited (unreaped or reaped) or for pid 1. Its
/// working directory is the one it was started in ([`crate::holders::cwd_of`]), and none once
/// it is reaped. Its serial reads the same twice and is gone once it is reaped. Apple's unique serial differs
/// from this process's; Linux's start tick is no earlier and may equal its parent's tick.
/// A platform with no marker reader (any but macOS, Linux and Android) or no serial reader
/// (any but Apple's and Linux) reads none. The child is this test binary run in place: macOS
/// withholds a platform binary's environment. Negative controls: an empty buffer in place of
/// the child's arguments reads no markers on macOS; on Linux, reading `environ` without
/// `bracketed` reads the exited child's empty file as withheld markers.
#[test]
fn a_spawned_childs_markers_are_read_off_its_exec_environment() {
    use super::super::{ProcessPresence, process_presence};
    let me = i32::try_from(std::process::id()).expect("pid");
    let run = format!("{me}-1");
    let dir = aterm_tempfile::Builder::new()
        .prefix("marked-child-")
        .tempdir()
        .expect("temp dir");
    let mut child = crate::spawnfd::command(std::env::current_exe().expect("the test binary"))
        .current_dir(dir.path())
        .args([
            "--exact",
            "process::kin::tests::a_marked_child_stand_in",
            "--nocapture",
            "--quiet",
            "--test-threads=1",
        ])
        .env(MARKED_CHILD_ENV, "1")
        .env(VERIFY_RUN_VAR, &run)
        .env(TRUST_CHECK_VAR, "5eed")
        .env("ATERM_CHILD", "1")
        .env("ATERM_PARENT_SESSION_ID", "s-markers")
        .stdin(crate::spawnfd::Stdio::piped())
        .stdout(crate::spawnfd::Stdio::piped())
        .stderr(crate::spawnfd::Stdio::null())
        .spawn()
        .expect("re-run this test binary as the marked child");
    crate::deadline::prints(&mut child, MARKED_CHILD_RUNS, HANG);
    let pid = i32::try_from(child.id()).expect("pid");
    let read = read_process_markers(pid);
    let cwd = crate::holders::cwd_of(child.id());
    let (serial, again) = (process_serial(pid), process_serial(pid));
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + HANG;
    while process_presence(pid) != ProcessPresence::Exited {
        assert!(
            std::time::Instant::now() < deadline,
            "the marked child (pid {pid}) did not exit within its hang detector"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let unreaped = read_process_markers(pid);
    let _ = child.wait();
    if cfg!(any(target_vendor = "apple", target_os = "linux")) {
        assert_eq!(
            cwd,
            Some(std::fs::canonicalize(dir.path()).expect("canonical")),
            "the child's working directory"
        );
        assert!(serial.is_some() && serial == again, "{serial:?} {again:?}");
        let parent = process_serial(me).expect("this process's serial");
        if cfg!(target_vendor = "apple") {
            assert_ne!(serial, Some(parent));
        } else {
            assert!(
                serial.is_some_and(|start| start >= parent),
                "Linux start ticks may tie across a fork: {serial:?} {parent}"
            );
        }
        assert_eq!(process_serial(pid), None, "a reaped child's serial");
    } else {
        assert_eq!(cwd, None, "no directory reader here");
        assert_eq!(serial, None, "no serial reader here");
    }
    if !cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "android"
    )) {
        assert_eq!(read, None, "no marker reader here");
        return;
    }
    assert_eq!(
        read,
        Some(Markers {
            withheld: false,
            parent_session: Some("s-markers".into()),
            child: Some("1".into()),
            verify_run: Some(run),
            trust_check: Some("5eed".into()),
        }),
        "the child's markers"
    );
    assert_eq!(unreaped, None, "an exited child its parent has not reaped");
    assert_eq!(read_process_markers(pid), None, "a reaped child");
    assert_eq!(
        crate::holders::cwd_of(child.id()),
        None,
        "a reaped child's directory"
    );
    assert_eq!(read_process_markers(1), None, "launchd's or init's");
}

#[test]
fn a_marker_named_twice_reads_as_getenv_reads_it() {
    let words: [&[u8]; 4] = [
        b"/bin/aterm",
        b"ATERM_VERIFY_RUN=71-1",
        b"ATERM_VERIFY_RUN=72-2",
        b"ATERM_CHILD=1",
    ];
    let read = markers_from_procargs2(&frame(1, &words)).expect("a buffer");
    assert_eq!(read.verify_run.as_deref(), Some("71-1"), "{read:?}");
    assert_eq!(read.child.as_deref(), Some("1"));
}

/// A pid namespace is the inode its link names, digits only; another kind of namespace, or
/// a link with no inode, names none. Linux reads this process's; macOS has none.
#[test]
fn a_pid_namespace_is_the_inode_its_link_names() {
    assert_eq!(
        namespace_inode("pid:[4026531836]").as_deref(),
        Some("4026531836")
    );
    assert_eq!(namespace_inode("pid:[]"), None);
    assert_eq!(namespace_inode("net:[4026531840]"), None, "another kind");
    assert_eq!(namespace_inode("pid:[40265x]"), None);
    assert_eq!(pid_namespace().is_some(), cfg!(target_os = "linux"));
}

/// launchd's children split three ways by their launcher's pid version: launchd's own is
/// launchd-started, another is an orphan launchd adopted, and with launchd's version unread
/// neither is claimed. A process whose parent is not launchd is neither, and one with no
/// launcher version (Linux) is never launchd-started.
#[test]
fn launchd_started_and_orphaned_split_launchds_children() {
    const LAUNCHD: i32 = 7;
    let unread = || -> Option<u64> { panic!("a versioned lineage reads no parent") };
    let started = lineage(1, Some(LAUNCHD));
    assert!(started.launchd_started(Some(LAUNCHD)), "{started:?}");
    assert!(!started.orphaned(Some(LAUNCHD), 100, unread), "{started:?}");
    let adopted = lineage(1, Some(31));
    assert!(!adopted.launchd_started(Some(LAUNCHD)), "{adopted:?}");
    assert!(adopted.orphaned(Some(LAUNCHD), 100, unread), "{adopted:?}");
    let l7 = lineage(LAUNCHD.unsigned_abs(), Some(LAUNCHD));
    assert!(!l7.launchd_started(Some(LAUNCHD)), "{l7:?}");
    assert!(!l7.orphaned(Some(LAUNCHD), 100, unread), "{l7:?}");
    for launchd_unread in [started, adopted] {
        assert!(!launchd_unread.launchd_started(None), "{launchd_unread:?}");
        assert!(
            !launchd_unread.orphaned(None, 100, unread),
            "{launchd_unread:?}"
        );
    }
    assert!(
        !lineage(1, None).launchd_started(None),
        "no versions prove no launch"
    );
}

/// Only a row the judge calls held counts, and the oldest is the oldest held row's fork age:
/// a lone unstarted thread that is young, a started process and an unreadable row count
/// nothing.
#[test]
fn held_programs_counts_old_unstarted_rows_only() {
    use aterm_sysprobe::jobs::StartFacts;
    const NINE_H: u64 = 9 * 3_600_000;
    let held = |age_ms| StartFacts {
        lone_zero: true,
        user_ns: Some(0),
        program_check_wait: true,
        local: Some(true),
        age_ms: Some(age_ms),
        ..StartFacts::default()
    };
    let read = |pid| match pid {
        1 => Some(held(NINE_H)),
        2 => Some(held(20_000)),
        3 => Some(held(0)),
        4 => Some(StartFacts::default()),
        6 => Some(StartFacts {
            program_check_wait: false,
            ..held(NINE_H)
        }),
        7 => Some(StartFacts {
            user_ns: Some(1),
            ..held(NINE_H)
        }),
        _ => None,
    };
    assert_eq!(
        held_among(&[1, 2, 3, 4, 5, 6, 7], read),
        HeldNow {
            count: 2,
            oldest_ms: NINE_H
        }
    );
    assert_eq!(held_among(&[3, 4, 5, 6, 7], read), HeldNow::default());
}

/// Only a held row forked at or after the daemon's start counts: one forked before it, a
/// started row and an unreadable row count nothing, and a held row whose fork is unread
/// counts, since it may be one.
#[test]
fn held_since_counts_only_held_rows_forked_at_or_after_the_daemon() {
    use aterm_sysprobe::jobs::StartFacts;
    const T: u64 = 1_800_000_000_000_000;
    let held = |start_us| StartFacts {
        lone_zero: true,
        user_ns: Some(0),
        program_check_wait: true,
        local: Some(true),
        age_ms: Some(30_000),
        start_us,
        ..StartFacts::default()
    };
    let read = |pid| match pid {
        1 => Some(held(Some(T - 1_000_000))),
        2 => Some(held(Some(T))),
        3 => Some(held(Some(T + 5_000_000))),
        4 => Some(StartFacts {
            start_us: Some(T + 9_000_000),
            ..StartFacts::default()
        }),
        6 => Some(held(None)),
        _ => None,
    };
    assert_eq!(held_since_among(&[1, 2, 3, 4, 5], read, T), 2);
    assert_eq!(held_since_among(&[1, 2, 3, 4, 5], read, T + 9_000_001), 0);
    assert_eq!(held_since_among(&[1, 6], read, T), 1);
}

/// The live count reads this user's processes on macOS and answers nothing elsewhere. On
/// macOS a lone `0` is read with its fork or not at all, so no held row there has an unread
/// one, and a daemon that started at the end of time has none forked since. Negative control:
/// have the macOS `held_since` answer `None`, and this fails.
#[test]
fn the_live_held_since_reads_this_users_processes_on_macos_only() {
    if !cfg!(target_os = "macos") {
        assert_eq!(held_since(0), None);
        return;
    }
    assert_eq!(held_since(u64::MAX), Some(0));
}

/// A held program's age reads in seconds, then minutes, then hours with two-digit minutes.
#[test]
fn a_held_age_reads_in_seconds_minutes_then_hours() {
    for (ms, words) in [
        (20_999, "20 s"),
        (59_999, "59 s"),
        (60_000, "1 m"),
        (3_599_999, "59 m"),
        (3_600_000, "1 h 00 m"),
        (36_300_000, "10 h 05 m"),
        (35_940_000, "9 h 59 m"),
    ] {
        assert_eq!(held_age(ms), words, "{ms}");
    }
}

#[test]
fn a_launcher_version_is_older_across_the_wrap() {
    assert!(launched_before(10, 11));
    assert!(!launched_before(11, 10));
    assert!(launched_before(i32::MAX, i32::MIN));
}

/// Linux's `environ` goes through the same selection as macOS's words; an empty file is
/// withheld and a read past the bound is unknown. Negative control: start
/// `markers_from_words` with `withheld` false, and the empty file reads as an environment
/// that names no run.
#[test]
fn linux_markers_are_read_off_environ_and_an_empty_one_is_withheld() {
    let environ = b"HOME=/h\0NO_EQUALS\0ATERM_VERIFY_RUN=71-1\0ATERM_CHILD=1\0\
                    ATERM_VERIFY_RUN=72-2\0ATERM_PARENT_SESSION_ID=s7\0";
    assert_eq!(
        markers_from_environ(environ),
        Some(Markers {
            withheld: false,
            parent_session: Some("s7".into()),
            child: Some("1".into()),
            verify_run: Some("71-1".into()),
            trust_check: None,
        })
    );
    let empty = markers_from_environ(b"").expect("an empty file reads");
    assert!(
        empty.withheld,
        "an empty environment is withheld: {empty:?}"
    );
    assert_eq!(empty.verify_run, None);
    let unterminated = markers_from_environ(b"ATERM_CHILD=1").expect("a last word");
    assert_eq!(unterminated.child.as_deref(), Some("1"));
    let past = vec![b'A'; super::super::PROCESS_ARGV_MAX + 1];
    assert_eq!(markers_from_environ(&past), None, "past the bound");
}

fn identity(idversion: Option<i32>) -> super::super::Identity {
    super::super::Identity {
        pid: 4242,
        birth: super::super::Birth(51_832_760),
        idversion,
    }
}

fn lineage(parent: u32, launcher_version: Option<i32>) -> Lineage {
    Lineage {
        identity: identity(launcher_version.map(|_| 9)),
        uniqueid: 51_832_760,
        parent,
        pgid: 4242,
        launcher_version,
        tty: None,
    }
}

/// A launcher is proven by two pid versions that agree, and by nothing where the kernel
/// keeps none. Negative control: compare the two `Option`s directly, and the Linux row's two
/// missing versions read as a launch.
#[test]
fn a_launcher_is_proven_only_by_two_versions() {
    assert_eq!(
        lineage(1, Some(7)).launched_by(&identity(Some(7))),
        Some(true)
    );
    assert_eq!(
        lineage(1, Some(7)).launched_by(&identity(Some(8))),
        Some(false)
    );
    assert_eq!(lineage(1, None).launched_by(&identity(None)), None);
    assert_eq!(lineage(1, Some(7)).launched_by(&identity(None)), None);
}

/// With a launcher version, orphaned is launchd's rule and the parent is never read; without
/// one, init adopted it or a parent older than the run did. Negative controls: `<=` for `<`
/// makes the run's own launcher (born at the floor) an adopter; dropping the older-parent
/// clause misses the subreaper.
#[test]
fn orphaned_is_launchds_rule_with_a_version_and_an_older_adopter_without_one() {
    let unread = || -> Option<u64> { panic!("a versioned lineage reads no parent") };
    assert!(lineage(1, Some(7)).orphaned(Some(1026), 100, unread));
    assert!(
        !lineage(1, Some(1026)).orphaned(Some(1026), 100, unread),
        "launchd's own"
    );
    assert!(
        !lineage(1, Some(7)).orphaned(None, 100, unread),
        "launchd unread"
    );
    assert!(
        !lineage(7, Some(7)).orphaned(Some(1026), 100, unread),
        "a live parent"
    );

    assert!(
        lineage(1, None).orphaned(None, 100, || None),
        "init adopted it"
    );
    for (parent, want, case) in [
        (Some(10), true, "a subreaper older than the run"),
        (Some(100), false, "the run's own launcher"),
        (Some(150), false, "a parent the run started"),
        (None, false, "a parent that does not read"),
    ] {
        assert_eq!(
            lineage(4243, None).orphaned(None, 100, || parent),
            want,
            "{case}"
        );
    }
}

#[test]
fn this_process_reads_its_lineage_and_its_group() {
    let me = i32::try_from(std::process::id()).expect("pid");
    if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
        assert_eq!(read_lineage(me), None);
        return;
    }
    let mine = read_lineage(me).expect("this process reads");
    assert_eq!(mine.identity.pid, me);
    assert_eq!(
        Some(mine.identity.birth),
        super::super::read_birth(me),
        "the birth read_birth reads"
    );
    let members = group_members(mine.pgid).expect("the group lists");
    assert!(members.pids.contains(&me), "{members:?}");
    assert!(!mine.orphaned(launchd_idversion(), 0, || None));
    if cfg!(target_vendor = "apple") {
        assert!(launchd_idversion().is_some(), "launchd's version reads");
    } else {
        assert_eq!(mine.identity.idversion, None, "Linux keeps no pid version");
    }
    assert_eq!(at_program_check(me), Some(false), "this process runs");
    let mut child = crate::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn");
    let pid = i32::try_from(child.id()).expect("pid");
    let read = read_lineage(pid);
    let listed = group_members(mine.pgid).is_some_and(|m| m.pids.contains(&pid));
    let _ = child.kill();
    let _ = child.wait();
    let read = read.expect("the child reads");
    assert_eq!(read.parent, me.unsigned_abs());
    assert_eq!(read.pgid, mine.pgid, "spawned into this group");
    if cfg!(target_vendor = "apple") {
        assert!(
            read.uniqueid > mine.uniqueid,
            "a later fork reads a larger unique id"
        );
    } else {
        assert!(
            read.uniqueid >= mine.uniqueid,
            "a later fork never reads an earlier tick"
        );
    }
    assert!(listed, "a child in the group is listed");
    assert_eq!(read_lineage(pid), None, "a reaped child reads nothing");
    assert_eq!(at_program_check(pid), None, "a reaped child is not judged");
    assert_eq!(
        at_program_check(1),
        None,
        "launchd's or init's is not judged"
    );
    assert!(boot_id().is_some(), "this boot's id reads");
}

/// To a reader that would read or signal it, a process whose one thread never ran has not
/// run, whatever the program check's words call it: a start-suspended child (one thread,
/// handle `0`, stopped and young, so not held) has not, and this process has.
#[test]
fn a_start_suspended_child_has_not_run_though_it_is_not_held() {
    let me = i32::try_from(std::process::id()).expect("pid");
    if !cfg!(target_vendor = "apple") {
        assert_eq!(thread_ran(me), cfg!(target_os = "linux").then_some(true));
        return;
    }
    let held = crate::deadline::Held::start(crate::spawnfd::command("/bin/sleep").arg("30"))
        .expect("a start-suspended sleep");
    let pid = i32::try_from(held.id()).expect("pid");
    assert_eq!(
        at_program_check(pid),
        Some(false),
        "the words do not hold it"
    );
    assert_eq!(thread_ran(pid), Some(false), "its thread never ran");
    assert_eq!(thread_ran(me), Some(true), "this process runs");
    drop(held);
}

/// A hop of pid `pid` with unique id `uniqueid`, whose parent is `parent` with unique id
/// `parent_uniqueid`, on no terminal.
fn a_hop(pid: u32, uniqueid: u64, parent: u32, parent_uniqueid: u64) -> Hop {
    Hop {
        identity: super::super::Identity {
            pid: i32::try_from(pid).expect("a pid"),
            birth: super::super::Birth(u64::from(pid)),
            idversion: Some(1),
        },
        uniqueid,
        parent_uniqueid,
        parent,
        tty: None,
    }
}

/// A run and `parents` parents above it: hop `k` (the run is hop 0) is pid `100 + k` with
/// unique id `10_000 - k`, older than its child, and names hop `k + 1` as its parent by both;
/// the last names launchd.
fn a_line(parents: u32) -> Vec<Hop> {
    (0..=parents)
        .map(|k| {
            let (pid, uniqueid) = (100 + k, 10_000 - u64::from(k));
            if k == parents {
                a_hop(pid, uniqueid, 1, 1)
            } else {
                a_hop(pid, uniqueid, pid + 1, uniqueid - 1)
            }
        })
        .collect()
}

/// A classifier that names the pids of `stops` and nothing else.
fn by_pid(stops: &[(u32, Stop)]) -> impl FnMut(&Hop) -> Option<Stop> + '_ {
    move |hop: &Hop| {
        stops
            .iter()
            .find(|(pid, _)| *pid == hop.identity.pid.unsigned_abs())
            .map(|(_, stop)| *stop)
    }
}

/// [`walk`] from `line`'s first hop, each pid read off `line` (`None` for one it lacks) and
/// judged by `stop`, with the pids it read and the pids it judged, in order.
fn walked(line: &[Hop], mut stop: impl FnMut(&Hop) -> Option<Stop>) -> (Walk, Vec<u32>, Vec<u32>) {
    let (mut read, mut judged) = (Vec::new(), Vec::new());
    let end = walk(
        line[0],
        |pid| {
            read.push(pid);
            line.iter()
                .find(|hop| hop.identity.pid.unsigned_abs() == pid)
                .copied()
        },
        |hop| {
            judged.push(hop.identity.pid.unsigned_abs());
            stop(hop)
        },
    );
    (end, read, judged)
}

/// Each parent must be the process its child names by unique id, from the run's own parent up:
/// one at the named pid with another unique id ends the walk Broken there and is never judged,
/// though the classifier would take it for the tab, whether it is older than the child or
/// younger. That holds at the run's own parent (the link a launcher that exits at once leaves
/// open to a reuse of its pid) and at one further up. With the unique id its child names, the
/// same line reaches the tab. NEGATIVE CONTROLS: a walk that asked only for a parent older than
/// its child (`parent.uniqueid < child.uniqueid`) takes the older process for the tab; one that
/// skipped the check on its first step reaches the tab through the impostor at the run's parent.
#[test]
fn walk_requires_each_hop_to_name_its_parent() {
    let line = a_line(4);
    let tab = [(102, Stop::Tab)];
    assert_eq!(
        walked(&line, by_pid(&tab)),
        (Walk::Tab, vec![101, 102], vec![100, 101, 102])
    );
    for (at, read, judged) in [
        (1, vec![101], vec![100]),
        (2, vec![101, 102], vec![100, 101]),
    ] {
        for (uniqueid, case) in [
            (line[at - 1].parent_uniqueid - 1, "an older process"),
            (line[0].uniqueid + 1, "a younger process"),
        ] {
            let mut reused = line.clone();
            reused[at].uniqueid = uniqueid;
            assert_eq!(
                walked(&reused, by_pid(&tab)),
                (Walk::Broken, read.clone(), judged.clone()),
                "{case} at hop {at}'s pid"
            );
        }
    }
}

/// The walk ends at the first process the classifier names: another owner at hop 2 wins over
/// the tab's shell at hop 4, which is never read, a tab at hop 2 wins over an owner at hop 4,
/// and with no owner on the way the line reaches the tab. NEGATIVE CONTROL: a walk that went on
/// past another owner to look for the tab answers Tab.
#[test]
fn walk_stops_at_the_first_owned_process() {
    let line = a_line(5);
    assert_eq!(
        walked(&line, by_pid(&[(102, Stop::Other), (104, Stop::Tab)])),
        (Walk::Other(line[2]), vec![101, 102], vec![100, 101, 102])
    );
    assert_eq!(
        walked(&line, by_pid(&[(102, Stop::Tab), (104, Stop::Other)])).0,
        Walk::Tab
    );
    assert_eq!(
        walked(&line, by_pid(&[(104, Stop::Tab)])),
        (
            Walk::Tab,
            vec![101, 102, 103, 104],
            vec![100, 101, 102, 103, 104]
        )
    );
}

/// A run three `setsid` launches below the tab's shell reaches it: each launcher leads a
/// session with no terminal, and the walk follows parents across sessions, judging the run and
/// then each parent once; this aterm, above the shell, is never read. A classifier that stops
/// at a terminal other than the tab's ([`Hop::tty`]) ends the walk at a launcher in another
/// terminal's session. NEGATIVE CONTROL: a walk that judged each child in place of its parent
/// reads one step past the shell.
#[test]
fn walk_reaches_the_tab_through_three_setsid_hops() {
    const TAB: u32 = 0x1000_0007;
    let mut line = a_line(5);
    line[4].tty = Some(TAB);
    let (shell, aterm) = (line[4].identity, line[5].identity);
    let classify = |hop: &Hop| {
        if hop.identity == shell {
            Some(Stop::Tab)
        } else if hop.identity == aterm || hop.tty.is_some_and(|tty| tty != TAB) {
            Some(Stop::Other)
        } else {
            None
        }
    };
    assert_eq!(
        walked(&line, classify),
        (
            Walk::Tab,
            vec![101, 102, 103, 104],
            vec![100, 101, 102, 103, 104]
        )
    );
    line[2].tty = Some(TAB + 1);
    assert_eq!(walked(&line, classify).0, Walk::Other(line[2]));
}

/// The classifier judges the run itself before any parent is read. `kitty codex exec …` typed
/// in the tab leaves the run leading a session on kitty's terminal, under a kitty that is still
/// the tab shell's child on the tab's terminal: the rule that stops at a process on another
/// terminal ends the walk at the run, Other, with nothing read. The same run with no terminal
/// (`setsid`) climbs through kitty to the shell, and a run the classifier names the tab is the
/// tab. NEGATIVE CONTROL: a walk that judged parents only judges kitty and then the shell, and
/// takes the run on kitty's terminal for the tab's.
#[test]
fn walk_judges_the_run_itself() {
    const TAB: u32 = 0x1000_0007;
    let mut line = a_line(3);
    line[0].tty = Some(TAB + 1);
    line[1].tty = Some(TAB);
    line[2].tty = Some(TAB);
    let shell = line[2].identity;
    let classify = |hop: &Hop| {
        if hop.identity == shell {
            Some(Stop::Tab)
        } else if hop.tty.is_some_and(|tty| tty != TAB) {
            Some(Stop::Other)
        } else {
            None
        }
    };
    assert_eq!(
        walked(&line, classify),
        (Walk::Other(line[0]), Vec::new(), vec![100])
    );
    line[0].tty = None;
    assert_eq!(
        walked(&line, classify),
        (Walk::Tab, vec![101, 102], vec![100, 101, 102])
    );
    assert_eq!(
        walked(&line, by_pid(&[(100, Stop::Tab)])),
        (Walk::Tab, Vec::new(), vec![100])
    );
}

/// A child whose parent is launchd (`1`) or none (`0`) ends the walk Orphaned at that child,
/// and launchd is never read: at the top of a line that never met the tab, and at a run whose
/// own parent is launchd, with nothing read and only the run judged. NEGATIVE CONTROL: a walk
/// that ended only at parent `0` reads launchd, which a reader that does not refuse pid 1
/// answers.
#[test]
fn walk_refuses_an_orphan() {
    let mut line = a_line(3);
    line.push(a_hop(1, 1, 0, 0));
    assert_eq!(
        walked(&line, by_pid(&[])),
        (
            Walk::Orphaned,
            vec![101, 102, 103],
            vec![100, 101, 102, 103]
        )
    );
    for parent in [1, 0] {
        assert_eq!(
            walked(&[a_hop(100, 10_000, parent, 1), line[4]], by_pid(&[])),
            (Walk::Orphaned, Vec::new(), vec![100]),
            "parent {parent}"
        );
    }
}

/// A parent that does not read (gone, another user's, a zombie) ends the walk Unread there,
/// though the tab's shell stands above it, and nothing past it is read or judged. NEGATIVE
/// CONTROL: a walk that took an unread parent for the top of the line answers Orphaned.
#[test]
fn walk_refuses_an_unreadable_hop() {
    let mut line = a_line(4);
    line.remove(2);
    assert_eq!(
        walked(&line, by_pid(&[(104, Stop::Tab)])),
        (Walk::Unread, vec![101, 102], vec![100, 101])
    );
}

/// The walk reads at most [`MAX_HOPS`] (64) parents: the tab's shell as the 64th parent is
/// reached, as the 65th it is never read and the walk ends TooDeep, with the run and 64
/// parents judged, and a hop that names itself as its parent (a reader out of step with the
/// kernel) ends TooDeep after 64 reads, never looping. NEGATIVE CONTROL: a bound of 65 reads
/// reaches the 65th.
#[test]
fn walk_stops_at_64_hops() {
    assert_eq!(MAX_HOPS, 64);
    let line = a_line(66);
    let (end, read, _) = walked(&line, by_pid(&[(164, Stop::Tab)]));
    assert_eq!((end, read.len()), (Walk::Tab, 64));
    let (end, read, judged) = walked(&line, by_pid(&[(165, Stop::Tab)]));
    assert_eq!(
        (end, read.len(), judged.len(), read.last()),
        (Walk::TooDeep, 64, 65, Some(&164))
    );
    let (end, read, _) = walked(&[a_hop(100, 10_000, 100, 10_000)], by_pid(&[]));
    assert_eq!((end, read.len()), (Walk::TooDeep, 64));
}

/// This process's hop names its parent (the test runner, this user's) by pid and by unique id,
/// and reads what [`read_lineage`] reads of it. A child it spawns names it the same way and
/// walks up to it through the live reader. launchd, an exited child its parent has not reaped
/// (a zombie) and a reaped one read none; the kernel answers no flavor 18 record for a zombie,
/// so the zombie's none is the kernel's and [`read_hop`]'s own `SZOMB` refusal, kept on
/// [`read_lineage`]'s terms, is not reached here. Off Apple nothing reads. NEGATIVE CONTROL: a
/// hop that read its own unique id for its parent's fails the first link.
#[test]
fn read_hop_links_this_process_to_its_parent() {
    use super::super::{ProcessPresence, process_presence};
    let me = i32::try_from(std::process::id()).expect("pid");
    if !cfg!(target_vendor = "apple") {
        assert_eq!(read_hop(me), None);
        return;
    }
    let live = |pid: u32| i32::try_from(pid).ok().and_then(read_hop);
    let mine = read_hop(me).expect("this process reads");
    let parent = std::os::unix::process::parent_id();
    assert_eq!(mine.parent, parent);
    let theirs = live(parent).expect("its parent reads: the test runner, this user's");
    assert_eq!(
        mine.parent_uniqueid, theirs.uniqueid,
        "this process names its parent by unique id"
    );
    let lineage = read_lineage(me).expect("this process's lineage");
    assert_eq!(
        (mine.identity, mine.uniqueid, mine.parent, mine.tty),
        (
            lineage.identity,
            lineage.uniqueid,
            lineage.parent,
            lineage.tty
        ),
        "what read_lineage reads"
    );
    let mut child = crate::spawnfd::command("/bin/sleep")
        .arg("30")
        .spawn()
        .expect("spawn");
    let pid = i32::try_from(child.id()).expect("pid");
    let hop = read_hop(pid);
    let walked = hop.map(|hop| {
        walk(hop, live, |h| {
            (h.identity == mine.identity).then_some(Stop::Tab)
        })
    });
    let _ = child.kill();
    let deadline = std::time::Instant::now() + HANG;
    while process_presence(pid) != ProcessPresence::Exited {
        assert!(
            std::time::Instant::now() < deadline,
            "the child (pid {pid}) did not exit within its hang detector"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let exited = read_hop(pid);
    let _ = child.wait();
    let hop = hop.expect("the child reads");
    assert_eq!(
        (hop.parent, hop.parent_uniqueid),
        (me.unsigned_abs(), mine.uniqueid),
        "the child names this process"
    );
    assert!(hop.uniqueid > mine.uniqueid, "{hop:?} {mine:?}");
    assert_eq!(
        walked,
        Some(Walk::Tab),
        "the child walks up to this process"
    );
    assert_eq!(exited, None, "an exited child its parent has not reaped");
    assert_eq!(read_hop(pid), None, "a reaped child");
    assert_eq!(read_hop(1), None, "launchd's");
}

/// The kernel sets a process's parent unique id at its fork and again at each exec, to the
/// parent it has then, and launchd's adoption alone leaves it. A shell's background child, its
/// own exec made while the shell lives, names the shell; once the shell exits and launchd
/// adopts it, it still names the exited shell; once it execs again (`cat`, which proves it ran
/// by echoing a line) it names launchd (`1`), with its own unique id kept under a new pid
/// version. Each step waits on the one before it, so no order of the kernel's is a race here.
/// NEGATIVE CONTROL: the model this replaced, under which the launcher's unique id is kept
/// through the adoption and any exec after it, fails the read after the exec.
#[cfg(target_vendor = "apple")]
#[test]
fn an_adopted_process_names_launchd_once_it_execs() {
    use crate::spawnfd::{Child, Stdio, command};
    use std::io::{BufRead as _, Write as _};
    /// The launcher, ended and reaped however the test ends; the run ends once its input does.
    struct Ended(Child);
    impl Drop for Ended {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let (run_reads, mut to_run) = crate::pipe().expect("the run's pipe");
    let (launcher_reads, to_launcher) = crate::pipe().expect("the launcher's pipe");
    let mut launcher = command("/bin/sh");
    launcher
        .args([
            "-c",
            r#"/bin/sh -c 'echo up; read go; exec /bin/cat' <&3 3<&- & echo "$!"; read done"#,
        ])
        .stdin(Stdio::from(launcher_reads))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    launcher
        .inherit_guards_at(vec![(run_reads.into(), 3)])
        .expect("the run's input at fd 3");
    let mut launcher = Ended(launcher.spawn().expect("spawn the launcher"));
    let out = launcher
        .0
        .stdout
        .take()
        .expect("the launcher's piped stdout");
    let (tx, lines) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let next = || {
        lines
            .recv_timeout(HANG)
            .expect("a line within the hang detector")
    };
    let (mut run, mut up) = (None, false);
    while run.is_none() || !up {
        match next().as_str() {
            "up" => up = true,
            pid => run = Some(pid.parse::<i32>().expect("the run's pid")),
        }
    }
    let run = run.expect("the run's pid");
    let shell = i32::try_from(launcher.0.id())
        .ok()
        .and_then(read_hop)
        .expect("the launcher reads");
    let forked = read_hop(run).expect("the run reads");
    drop(to_launcher);
    launcher.0.wait().expect("the launcher exits");
    let adopted = read_hop(run).expect("the adopted run reads");
    to_run.write_all(b"go\nran\n").expect("write to the run");
    assert_eq!(next(), "ran", "the run, now cat, echoes");
    let execd = read_hop(run).expect("the run reads after its exec");
    drop(to_run);
    let launchd = super::uniq_identifier(1)
        .expect("launchd's identifiers")
        .p_uniqueid;
    assert_eq!(
        (forked.parent, forked.parent_uniqueid),
        (shell.identity.pid.unsigned_abs(), shell.uniqueid),
        "forked and exec'd under the shell, the run names it"
    );
    assert_eq!(
        (adopted.parent, adopted.parent_uniqueid, adopted.identity),
        (1, shell.uniqueid, forked.identity),
        "adopted, it still names the exited shell"
    );
    assert_eq!(
        (execd.parent, execd.parent_uniqueid, execd.uniqueid),
        (1, launchd, forked.uniqueid),
        "exec'd after the adoption, it names launchd"
    );
    assert_ne!(
        execd.identity.idversion, adopted.identity.idversion,
        "an exec takes a new pid version"
    );
}

/// Set in the session leader [`a_hop_reads_its_sessions_controlling_terminal`] spawns of this
/// test binary.
#[cfg(target_vendor = "apple")]
const TERMINAL_LEADER_ENV: &str = "ATERM_UDS_TEST_TERMINAL_LEADER";

/// A hop's terminal is its session's controlling terminal, numbered as the terminal's own
/// `st_rdev`: a child that leads a session of its own on a pty's terminal end (it takes the
/// terminal at its stdin, `TIOCSCTTY`) reads that terminal and still names this process as its
/// parent by unique id, and one that leads a session with none (`setsid` alone) reads none, as
/// [`read_lineage`] reads them both. The leader is this test binary staged by
/// [`TERMINAL_LEADER_ENV`]. NEGATIVE CONTROLS: a hop that read no terminal fails the leader;
/// one that read `e_tdev` without `NODEV`'s none reads the bare session's as `u32::MAX`.
#[cfg(target_vendor = "apple")]
#[test]
fn a_hop_reads_its_sessions_controlling_terminal() {
    use crate::spawnfd::{Child, Stdio, command};
    use std::os::unix::fs::MetadataExt as _;
    const NAME: &str = "process::kin::tests::a_hop_reads_its_sessions_controlling_terminal";
    if std::env::var_os(TERMINAL_LEADER_ENV).is_some() {
        // SAFETY: This child leads a session of its own with no controlling terminal
        // (`setsid`), and fd 0 is the pty's terminal end its parent handed it.
        let rc = unsafe { libc::ioctl(0, libc::c_ulong::from(libc::TIOCSCTTY), 0) };
        assert_eq!(rc, 0, "TIOCSCTTY: {}", std::io::Error::last_os_error());
        println!("ran");
        std::thread::sleep(HANG);
        return;
    }
    /// The children, each ended and reaped however the test ends.
    struct Ended(Vec<Child>);
    impl Drop for Ended {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let me = i32::try_from(std::process::id()).expect("pid");
    let (_master, terminal) = crate::holders::listing_tests::a_terminal();
    let number =
        u32::try_from(terminal.metadata().expect("fstat the terminal").rdev() & 0xffff_ffff)
            .expect("a terminal's number");
    let mut children = Ended(Vec::new());
    // `--quiet` keeps the leader's `ran` on its own line for [`crate::deadline::prints`].
    let leader = command(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            NAME,
            "--nocapture",
            "--quiet",
            "--test-threads=1",
        ])
        .env(TERMINAL_LEADER_ENV, "1")
        .setsid(true)
        .stdin(Stdio::from(terminal))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the session leader");
    children.0.push(leader);
    let leader = children.0.last_mut().expect("the leader just kept");
    crate::deadline::prints(leader, "ran", HANG);
    let leader = i32::try_from(leader.id()).expect("pid");
    let bare = command("/bin/sleep")
        .arg("60")
        .setsid(true)
        .spawn()
        .expect("spawn a session with no terminal");
    let bare_pid = i32::try_from(bare.id()).expect("pid");
    children.0.push(bare);
    let mine = read_hop(me).expect("this process reads");
    let (led, unled) = (read_hop(leader), read_hop(bare_pid));
    let lineages = (read_lineage(leader), read_lineage(bare_pid));
    drop(children);
    let (led, unled) = (
        led.expect("the leader reads"),
        unled.expect("the bare session reads"),
    );
    assert_eq!(led.tty, Some(number), "the leader's terminal is the pty's");
    assert_eq!(
        (led.parent, led.parent_uniqueid),
        (me.unsigned_abs(), mine.uniqueid),
        "a session of its own still names this process"
    );
    assert_eq!(unled.tty, None, "a session with no terminal");
    assert_eq!(
        (lineages.0.map(|l| l.tty), lineages.1.map(|l| l.tty)),
        (Some(led.tty), Some(unled.tty)),
        "as read_lineage reads them"
    );
}
