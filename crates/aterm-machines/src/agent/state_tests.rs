// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::port::Fake;

fn layout() -> Layout {
    Layout { root: "/A".into() }
}

#[test]
fn a_scheduled_run_guard_does_not_refuse_the_next_valid_taker() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = Layout {
        root: dir.path().to_path_buf(),
    };
    std::fs::create_dir_all(layout.state()).unwrap();
    let live = crate::port::Live::new();
    drop(
        guard_started(
            &live,
            &layout,
            std::time::Instant::now() - std::time::Duration::from_secs(31),
        )
        .unwrap(),
    );
    assert_eq!(
        take_lock(&live, &layout, "1-1", std::process::id()).unwrap(),
        Lock::Taken
    );
    let record = holder(&live, &layout).unwrap();
    assert_eq!(record.pid, std::process::id());
    assert_eq!(record.run, "1-1");
    assert_eq!(record.start, started(&live, std::process::id()));
    let held = live
        .exclusive(&layout.lock_guard(), std::time::Duration::from_secs(60))
        .unwrap();
    assert!(
        guard_started(
            &live,
            &layout,
            std::time::Instant::now() - std::time::Duration::from_secs(61)
        )
        .is_err()
    );
    assert_eq!(holder(&live, &layout).unwrap(), record);
    drop(held);
    let changed = Holder {
        run: "2-2".into(),
        ..record.clone()
    };
    release_holder(&live, &layout, &changed).unwrap();
    assert_eq!(holder(&live, &layout).unwrap(), record);
    release_holder(&live, &layout, &record).unwrap();
    assert!(holder(&live, &layout).is_none());
    assert_eq!(
        take_lock(&live, &layout, "2-2", std::process::id()).unwrap(),
        Lock::Taken
    );
}

#[test]
fn a_floor_only_rises_and_survives_a_rewrite() {
    let fake = Fake::root();
    assert_eq!(floors(&fake, &layout()), Read::Absent);
    raise_floor(&fake, &layout(), "signing@ctl", 5).unwrap();
    raise_floor(&fake, &layout(), "recovery@ctl", 2).unwrap();
    raise_floor(&fake, &layout(), "signing@ctl", 3).unwrap();
    let Read::Value(list) = floors(&fake, &layout()) else {
        panic!("floors unreadable");
    };
    assert_eq!(floor_of(&list, "signing@ctl"), 5);
    assert_eq!(floor_of(&list, "recovery@ctl"), 2);
    assert_eq!(floor_of(&list, "nobody"), 0);
    assert_eq!(fake.mode_of("/A/state/floors.toml"), Some(0o644));
}

#[test]
fn a_damaged_floors_file_is_unusable_never_zero() {
    let fake = Fake::root();
    fake.file(
        "/A/state/floors.toml",
        b"[[floor]]\nprincipal = \"x\"\nserial = \"many\"\n",
    );
    assert!(matches!(floors(&fake, &layout()), Read::Unusable(_)));
    assert!(floors_or_refuse(&fake, &layout()).is_err());
    assert!(raise_floor(&fake, &layout(), "x", 9).is_err());
}

#[test]
fn a_live_holder_keeps_the_lock_and_a_dead_one_loses_it() {
    let fake = Fake::root();
    assert_eq!(take_lock(&fake, &layout(), "1-1", 10).unwrap(), Lock::Taken);
    fake.alive(10);
    assert_eq!(
        take_lock(&fake, &layout(), "2-2", 11).unwrap(),
        Lock::Busy { run: "1-1".into() }
    );
    assert_eq!(busy(&fake, &layout()).as_deref(), Some("1-1"));
    fake.file("/A/state/run.lock", b"pid=12 run=1-1\n");
    assert_eq!(busy(&fake, &layout()), None);
    assert_eq!(take_lock(&fake, &layout(), "3-3", 13).unwrap(), Lock::Taken);
    assert_eq!(holder(&fake, &layout()).unwrap().run, "3-3");
}

#[test]
fn a_reused_pid_does_not_keep_a_crashed_runs_lock() {
    let stale = b"pid=10 run=1-1 start=Fri_Oct_16_22:36:10_2026\n";
    let fake = Fake::root();
    fake.file("/A/state/run.lock", stale);
    fake.alive(10);
    fake.born(10, 1_792_224_062);
    fake.born(11, 1_792_224_300);
    assert_eq!(busy(&fake, &layout()), None);
    assert!(!running(&fake, &layout(), "1-1"));
    assert_eq!(take_lock(&fake, &layout(), "2-2", 11).unwrap(), Lock::Taken);
    assert_eq!(
        holder(&fake, &layout()).unwrap().start.as_deref(),
        Some("Sat_Oct_17_08:05:00_2026")
    );
    let fake = Fake::root();
    fake.file("/A/state/run.lock", stale);
    fake.alive(10);
    fake.born(10, 1_792_190_170);
    assert_eq!(busy(&fake, &layout()).as_deref(), Some("1-1"));
    assert_eq!(
        take_lock(&fake, &layout(), "2-2", 11).unwrap(),
        Lock::Busy { run: "1-1".into() }
    );
    let fake = Fake::root();
    fake.file("/A/state/run.lock", stale);
    fake.alive(10);
    assert_eq!(busy(&fake, &layout()).as_deref(), Some("1-1"));
}

#[test]
fn a_start_is_the_word_ps_printed() {
    assert_eq!(lstart_word(0), "Thu_Jan_1_00:00:00_1970");
    assert_eq!(lstart_word(951_782_400), "Tue_Feb_29_00:00:00_2000");
    assert_eq!(lstart_word(1_792_190_170), "Fri_Oct_16_22:36:10_2026");
    assert_eq!(lstart_word(1_792_224_062), "Sat_Oct_17_08:01:02_2026");
    assert_ne!(lstart_word(1_792_190_171), lstart_word(1_792_190_170));
}

/// `/bin/ps -o lstart= -p <pid>` under `TZ=UTC0`, the reader [`started`] replaced, as
/// its oracle: `None` when it does not answer. `zone` is the `TZ` it runs under.
#[cfg(target_os = "macos")]
fn ps_says(pid: u32, zone: &str) -> Option<String> {
    let out = aterm_uds::spawnfd::command(aterm_uds::os_path!("ps"))
        .env("TZ", zone)
        .env("LC_ALL", "C")
        .args(["-o", "lstart=", "-p"])
        .arg(pid.to_string())
        .output()
        .ok()?;
    let word = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    (out.status.success() && !word.is_empty()).then_some(word)
}

/// The kernel's start of this process, its parent and launchd (root's, which the old
/// `ps` read too) is what `ps` says. NEGATIVE CONTROL: `ps` under another zone says
/// another word, so the comparison can fail; a pid no process holds reads `None`.
#[cfg(target_os = "macos")]
#[test]
fn the_kernel_start_is_what_ps_says() {
    let live = crate::port::Live::new();
    let me = std::process::id();
    let parent = aterm_uds::process::read_process_parent(i32::try_from(me).unwrap())
        .expect("this process's parent");
    for pid in [me, parent, 1] {
        let ours = started(&live, pid).expect("the kernel's record");
        assert_eq!(Some(ours.clone()), ps_says(pid, "UTC0"), "pid {pid}");
        assert_ne!(Some(ours), ps_says(pid, "Asia/Tokyo"), "pid {pid}");
    }
    assert_eq!(started(&live, 2_147_483_647), None);
    assert_eq!(ps_says(2_147_483_647, "UTC0"), None);
}

#[test]
fn a_principal_new_to_the_list_starts_at_the_highest_floor() {
    let fake = Fake::root();
    inherit_floors(&fake, &layout(), &["new@ctl".into()]).unwrap();
    assert_eq!(
        floors(&fake, &layout()),
        Read::Absent,
        "a fresh host has none"
    );
    raise_floor(&fake, &layout(), "signing@ctl", 41).unwrap();
    raise_floor(&fake, &layout(), "recovery@ctl", 3).unwrap();
    inherit_floors(
        &fake,
        &layout(),
        &["renamed@ctl".into(), "recovery@ctl".into()],
    )
    .unwrap();
    let Read::Value(list) = floors(&fake, &layout()) else {
        panic!("floors unreadable");
    };
    assert_eq!(floor_of(&list, "renamed@ctl"), 41);
    assert_eq!(floor_of(&list, "recovery@ctl"), 41);
    assert_eq!(floor_of(&list, "signing@ctl"), 41);
    let fake = Fake::root();
    fake.file("/A/state/floors.toml", b"not toml [");
    assert!(inherit_floors(&fake, &layout(), &["x".into()]).is_err());
}

#[test]
fn of_takers_racing_one_stale_lock_exactly_one_takes_it() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = Layout {
        root: dir.path().to_path_buf(),
    };
    std::fs::create_dir_all(layout.state()).unwrap();
    std::fs::write(layout.lock(), "pid=2147483647 run=1-1\n").unwrap();
    let live = crate::port::Live::new();
    let me = std::process::id();
    let taken = std::thread::scope(|s| {
        let racers: Vec<_> = (2..10)
            .map(|i| {
                let (live, layout) = (&live, &layout);
                s.spawn(move || take_lock(live, layout, &format!("{i}-{i}"), me).unwrap())
            })
            .collect();
        racers
            .into_iter()
            .map(|r| r.join().unwrap())
            .filter(|lock| *lock == Lock::Taken)
            .count()
    });
    assert_eq!(taken, 1);
}

#[test]
fn the_live_lock_is_exclusive_in_a_real_directory() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let layout = Layout {
        root: dir.path().to_path_buf(),
    };
    std::fs::create_dir_all(layout.state()).unwrap();
    let live = crate::port::Live::new();
    let me = std::process::id();
    assert_eq!(take_lock(&live, &layout, "1-1", me).unwrap(), Lock::Taken);
    assert_eq!(
        take_lock(&live, &layout, "2-2", me).unwrap(),
        Lock::Busy { run: "1-1".into() }
    );
    std::fs::write(layout.lock(), "pid=2147483647 run=1-1\n").unwrap();
    assert_eq!(take_lock(&live, &layout, "2-2", me).unwrap(), Lock::Taken);
}
