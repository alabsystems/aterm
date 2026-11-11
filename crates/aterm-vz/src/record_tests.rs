// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::collections::HashMap;

use super::*;

/// Each scripted pid's unique id; its start time is [`BIRTH`] unless `births` says.
struct Script(
    RefCell<HashMap<u32, u64>>,
    RefCell<HashMap<u32, Option<u64>>>,
);

const BIRTH: u64 = 1_792_000_000_000_000;

impl Script {
    fn of(pids: impl IntoIterator<Item = (u32, u64)>) -> Self {
        Self(
            RefCell::new(pids.into_iter().collect()),
            RefCell::new(HashMap::new()),
        )
    }
}

impl Pids for Script {
    fn uniq(&self, pid: u32) -> Option<u64> {
        self.0.borrow().get(&pid).copied()
    }

    fn birth_us(&self, pid: u32) -> Option<u64> {
        self.uniq(pid)?;
        self.1.borrow().get(&pid).copied().unwrap_or(Some(BIRTH))
    }
}

fn record(pid: u32, uniq: u64) -> Record {
    Record {
        pid,
        uniq,
        birth_us: BIRTH,
        helper_sha256: "ab".repeat(32),
        proto: crate::proto::PROTO,
        live_disk: "/vz/live/aterm-linux.img".into(),
        started: 1_792_000_000,
        boot_id: None,
        agent_sha256: Some("cd".repeat(32)),
    }
}

#[test]
fn a_record_round_trips_and_a_partial_one_is_none() {
    let r = record(4242, 77);
    assert_eq!(Record::parse(&r.json()), Some(r.clone()));
    let mut hello = r;
    hello.boot_id = Some("b00t".into());
    assert_eq!(Record::parse(&hello.json()), Some(hello));
    assert_eq!(Record::parse("{\"pid\": 1}"), None);
    assert_eq!(Record::parse("not json"), None);
}

#[test]
fn a_record_names_a_live_helper_only_by_its_unique_id() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path());
    std::fs::create_dir_all(paths.run_dir()).unwrap();
    let pids = Script::of([(4242, 77)]);
    assert_eq!(holder(&paths, &pids), Holder::None);
    let r = record(4242, 77);
    r.write(&paths.record()).unwrap();
    assert_eq!(holder(&paths, &pids), Holder::Live(r.clone()));
    // The negative control: the same pid, another process.
    pids.0.borrow_mut().insert(4242, 78);
    assert_eq!(holder(&paths, &pids), Holder::Dead(r.clone()));
    pids.0.borrow_mut().clear();
    assert_eq!(holder(&paths, &pids), Holder::Dead(r.clone()));
    std::fs::write(paths.socket(), b"").unwrap();
    std::fs::write(paths.key_disk(), b"").unwrap();
    remove_leftovers(&paths, &r);
    assert!(!paths.record().exists() && !paths.socket().exists());
    assert!(!paths.key_disk().exists(), "no key outlives its VM");
}

#[test]
fn leftovers_of_another_record_are_kept() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path());
    std::fs::create_dir_all(paths.run_dir()).unwrap();
    let newer = record(5000, 1);
    newer.write(&paths.record()).unwrap();
    std::fs::write(paths.key_disk(), b"").unwrap();
    remove_leftovers(&paths, &record(4242, 77));
    assert_eq!(Record::read(&paths.record()), Some(newer));
    assert!(
        paths.key_disk().exists(),
        "the newer helper's key disk stays"
    );
}

#[test]
fn a_pid_whose_unique_id_differs_is_never_signalled() {
    // The pid is this test's own: a signal would end the test, so reaching the end shows
    // none was sent.
    let me = std::process::id();
    let pids = Script::of([(me, 2)]);
    let why = kill_helper(&record(me, 1), &pids).expect_err("refused");
    assert!(
        why.contains("another process") && why.contains("not signalled"),
        "{why}"
    );
    pids.0.borrow_mut().clear();
    let why = kill_helper(&record(me, 1), &pids).expect_err("gone");
    assert!(why.contains("gone"), "{why}");
}

/// A record from an earlier boot: its pid and unique id match a process of this boot (every
/// process a boot makes before its first pid wrap has a unique id equal to its pid), and
/// only the start time tells them apart. That process is no helper and is never signalled.
/// Negative control: judged by unique id alone, it reads live.
#[test]
fn a_record_from_an_earlier_boot_names_no_live_helper_and_nothing_is_signalled() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::at(dir.path());
    std::fs::create_dir_all(paths.run_dir()).unwrap();
    // The pid is this test's own: a signal would end the test.
    let me = std::process::id();
    let earlier = record(me, u64::from(me));
    earlier.write(&paths.record()).unwrap();
    let pids = Script::of([(me, u64::from(me))]);
    assert_eq!(
        holder(&paths, &pids),
        Holder::Live(earlier.clone()),
        "the same start"
    );
    pids.1
        .borrow_mut()
        .insert(me, Some(BIRTH + 4 * 86_400_000_000));
    assert_eq!(
        holder(&paths, &pids),
        Holder::Dead(earlier.clone()),
        "a later start"
    );
    assert!(!is_live(&earlier, &pids));
    let why = kill_helper(&earlier, &pids).expect_err("another process");
    assert!(
        why.contains("another process") && why.contains("not signalled"),
        "{why}"
    );
    pids.1.borrow_mut().insert(me, None);
    assert_eq!(
        holder(&paths, &pids),
        Holder::Dead(earlier.clone()),
        "start unread"
    );
    let why = kill_helper(&earlier, &pids).expect_err("unread");
    assert!(
        why.contains("cannot be read") && why.contains("not signalled"),
        "{why}"
    );
}

/// A record an earlier build wrote, with no start time, is unproven: never live and never
/// signalled, though its pid holds its unique id; a wait on its end watches the unique id.
/// A start time that is no number leaves the record unwhole. Negative control: the same
/// record with its start time is live and may be signalled past this gate.
#[test]
fn a_record_without_a_start_time_is_unproven_and_never_signalled() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::at(tmp.path().join("vz.noindex"));
    std::fs::create_dir_all(paths.run_dir()).unwrap();
    let mut v = aterm_json::from_str::<aterm_json::Value>(&record(4242, 77).json()).unwrap();
    v.as_object_mut().unwrap().remove("birth_us");
    let text = aterm_json::to_string(&v).unwrap();
    let earlier = Record::parse(&text).expect("whole");
    assert_eq!(
        earlier,
        Record {
            birth_us: 0,
            ..record(4242, 77)
        }
    );
    std::fs::write(paths.record(), &text).unwrap();
    let pids = Script::of([(4242, 77)]);
    assert_eq!(holder(&paths, &pids), Holder::Unproven(earlier.clone()));
    assert!(!is_live(&earlier, &pids));
    assert!(may_run(&earlier, &pids), "its pid holds its unique id");
    assert!(
        !may_run(&earlier, &Script::of([(4242, 78)])),
        "another process"
    );
    assert!(!may_run(&earlier, &Script::of([])), "gone");
    let why = kill_helper(&earlier, &pids).expect_err("never signalled");
    assert!(
        why.contains("no start time") && why.contains("not signalled"),
        "{why}"
    );
    assert!(is_live(&record(4242, 77), &pids), "with its start time");
    v.as_object_mut()
        .unwrap()
        .insert("birth_us".into(), aterm_json::Value::from("soon"));
    assert_eq!(Record::parse(&aterm_json::to_string(&v).unwrap()), None);
}

#[cfg(target_vendor = "apple")]
#[test]
fn the_kernel_reads_this_process_s_start_time() {
    let me = std::process::id();
    let birth = Kernel.birth_us(me).expect("this process's start");
    assert!(birth > 1_700_000_000_000_000, "{birth}");
    assert_eq!(Kernel.birth_us(me), Some(birth), "stable");
}

#[test]
fn pid_0_and_pid_1_are_never_a_helpers_and_never_signalled() {
    // `proc_pidinfo` reads pid 0 with unique id 0 and pid 1 with 1 on macOS, so a record
    // naming either would pass the unique-id gate: it is no record at all.
    for pid in [0u32, 1] {
        assert!(!helper_pid(pid));
        let text = record(pid, u64::from(pid)).json();
        assert_eq!(Record::parse(&text), None, "pid {pid}");
    }
    // The negative control: pid 2 is a record.
    assert!(helper_pid(2));
    assert_eq!(Record::parse(&record(2, 2).json()), Some(record(2, 2)));
    // A record built in code naming pid 1 with its unique id is refused before any kill
    // (were it sent, launchd would refuse it: EPERM, other words).
    let pids = Script::of([(1, 1)]);
    let why = kill_helper(&record(1, 1), &pids).expect_err("refused");
    assert_eq!(why, "pid 1 is never a helper's: not signalled");
}

#[test]
fn a_socket_path_past_sun_path_is_refused_by_name() {
    let short = Paths::at("/tmp/vz");
    assert!(short.checked_socket().is_ok());
    let long = Paths::at(format!("/tmp/{}", "d".repeat(90)));
    let why = long.checked_socket().expect_err("too long");
    assert!(why.contains("past sun_path's 104"), "{why}");
    let nul = Paths::at("/tmp/vz\0bad");
    let why = nul.checked_socket().expect_err("interior NUL");
    assert!(why.contains("interior NUL"), "{why}");

    let mut v = aterm_json::from_str::<aterm_json::Value>(&record(4242, 77).json()).unwrap();
    v.as_object_mut().unwrap().remove("boot_id");
    v.as_object_mut()
        .unwrap()
        .insert("agent_sha256".into(), aterm_json::Value::Null);
    let parsed = Record::parse(&aterm_json::to_string(&v).unwrap()).expect("absent/null ok");
    assert_eq!(parsed.boot_id, None);
    assert_eq!(parsed.agent_sha256, None);
    for key in ["boot_id", "agent_sha256"] {
        let mut bad = aterm_json::from_str::<aterm_json::Value>(&record(4242, 77).json()).unwrap();
        bad.as_object_mut()
            .unwrap()
            .insert(key.into(), aterm_json::Value::from(123u64));
        assert_eq!(
            Record::parse(&aterm_json::to_string(&bad).unwrap()),
            None,
            "non-string {key} must be refused"
        );
    }
}

#[test]
fn the_disk_lock_is_held_once_and_let_go_on_drop() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("aterm-linux.img.lock");
    let first = DiskLock::try_take(&path).unwrap().expect("free");
    assert!(DiskLock::try_take(&path).unwrap().is_none(), "held");
    drop(first);
    assert!(DiskLock::try_take(&path).unwrap().is_some(), "let go");

    let link = dir.path().join("linked.img.lock");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(
        DiskLock::try_take(&link).is_err(),
        "a symlink lock path must be refused"
    );
}

#[test]
fn the_run_directory_is_owned_and_repaired_to_0700() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let scratch = aterm_tempfile::Builder::new()
        .prefix("vz")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let paths = Paths::at(scratch.path().join("state"));
    let run = paths
        .prepare_run_dir()
        .expect("create the private run directory");
    assert_eq!(std::fs::metadata(&run).unwrap().mode() & 0o7777, 0o700);
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o755)).unwrap();
    paths.prepare_run_dir().expect("narrow the owned directory");
    assert_eq!(std::fs::metadata(&run).unwrap().mode() & 0o7777, 0o700);
    #[cfg(target_vendor = "apple")]
    {
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o0)).unwrap();
        assert_eq!(std::fs::metadata(&run).unwrap().mode() & 0o7777, 0);
        paths
            .prepare_run_dir()
            .expect("repair the unreadable owned directory");
        assert_eq!(std::fs::metadata(&run).unwrap().mode() & 0o7777, 0o700);
    }

    std::fs::rename(&run, run.with_file_name("kept-directory")).unwrap();
    std::fs::write(&run, b"owned file, not a run directory").unwrap();
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o640)).unwrap();
    let before = std::fs::metadata(&run).unwrap().mode();
    assert!(paths.prepare_run_dir().is_err(), "a plain file is refused");
    assert_eq!(std::fs::metadata(&run).unwrap().mode(), before);
    assert_eq!(
        std::fs::read(&run).unwrap(),
        b"owned file, not a run directory"
    );
}

#[test]
fn helper_setup_refuses_a_linked_run_directory_without_changing_its_target() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let scratch = aterm_tempfile::Builder::new()
        .prefix("vz")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let paths = Paths::at(scratch.path().join("state"));
    std::fs::create_dir_all(paths.root()).unwrap();
    paths
        .checked_socket()
        .expect("the fixture fits the helper socket path");
    let run = paths.run_dir();
    let outside = scratch.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o750)).unwrap();
    let before = std::fs::metadata(&outside).unwrap().mode();
    std::os::unix::fs::symlink(&outside, &run).unwrap();
    let ident = crate::helper::Ident {
        pid: 4242,
        uniq: 77,
        birth_us: BIRTH,
        helper_sha256: "ab".repeat(32),
        live_disk: paths.live_disk(),
    };
    let attempted = crate::helper::take(&paths, &ident, &Script::of([]));
    let refused = attempted.is_err();
    drop(attempted);
    assert_eq!(std::fs::metadata(&outside).unwrap().mode(), before);
    assert!(
        refused,
        "a linked run directory is refused before helper setup"
    );
    assert!(
        std::fs::symlink_metadata(&run)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(std::fs::read_dir(&outside).unwrap().next().is_none());
}

/// NEGATIVE CONTROL: `Record::read`, `Record::write`, `holder`, and `remove_leftovers`
/// refuse a symlinked `run/` directory without reading, overwriting, or unlinking files in
/// the symlink's target.
#[test]
fn record_io_and_leftover_cleanup_refuse_symlinked_run_directory() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let victim = record(4242, 77);
    let outside_record = outside.join("aterm-linux.json");
    let outside_sock = outside.join("aterm-linux.sock");
    let outside_key = outside.join("key.img");
    std::fs::write(&outside_record, victim.json()).unwrap();
    std::fs::write(&outside_sock, b"sock").unwrap();
    std::fs::write(&outside_key, b"key").unwrap();

    let paths = Paths::at(dir.path().join("state"));
    std::fs::create_dir_all(paths.root()).unwrap();
    std::os::unix::fs::symlink(&outside, paths.run_dir()).unwrap();

    assert_eq!(Record::read(&paths.record()), None);
    assert_eq!(holder(&paths, &Script::of([(4242, 77)])), Holder::None);
    assert!(record(9999, 1).write(&paths.record()).is_err());
    remove_leftovers(&paths, &victim);

    assert_eq!(
        std::fs::read_to_string(&outside_record).unwrap(),
        victim.json()
    );
    assert_eq!(std::fs::read(&outside_sock).unwrap(), b"sock");
    assert_eq!(std::fs::read(&outside_key).unwrap(), b"key");
}
