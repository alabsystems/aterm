// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The unix second now, for a record a test just wrote and its file's own time.
fn wall() -> u64 {
    aterm_time::unix_secs().unwrap()
}

#[test]
fn survey_checks_kernel_identity_instead_of_trusting_a_recent_record() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    let path = registry.join("0.156.0.json");
    let record = |start| aterm_json::json!({"holder": {"pid": pid, "boot": boot, "start": start}, "rows": ["red"]});
    std::fs::write(&path, record(start).to_string()).unwrap();
    assert!(survey(dir.path(), wall())[0].starts_with("release_line alive=1 "));
    // Negative control: the same pid and a recent row cannot revive another start.
    std::fs::write(&path, record(start.wrapping_add(1)).to_string()).unwrap();
    assert!(survey(dir.path(), wall())[0].starts_with("release_line alive=0 "));
}

/// Each kind of positive kernel evidence reads dead, and only an exact identity reads alive.
/// NEGATIVE CONTROLS: a malformed pid (0, launchd's or init's 1, or one past `i32::MAX`) is
/// unknown, never dead, where the previous `kill(pid, 0)` rule called 0 and `u32::MAX` dead
/// with no kernel read at all.
#[test]
fn judge_needs_positive_kernel_evidence_for_death() {
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    assert_eq!(judge(pid, &boot, start), Judged::Alive);
    assert_eq!(judge(pid, &boot, start.wrapping_add(1)), Judged::Dead);
    let other_boot = if boot.starts_with('0') { "1" } else { "0" };
    assert_eq!(judge(pid, other_boot, start), Judged::Dead);
    // A reaped child: its pid is absent, or names a later process with another start.
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let child_start = identity(child_pid).map(|(_, start)| start);
    child.kill().unwrap();
    child.wait().unwrap();
    let child_start = child_start.unwrap();
    assert_eq!(judge(child_pid, &boot, child_start), Judged::Dead);
    for malformed in [0, 1, u32::MAX] {
        assert_eq!(
            judge(malformed, &boot, start),
            Judged::Unknown,
            "{malformed}"
        );
    }
}

/// A kernel read that fails or is refused is unknown, never death, for a pid that passes the
/// pid filter: an unreadable boot id, an unknown presence, and a present process whose start
/// cannot be read (another user's, or a zombie).
/// NEGATIVE CONTROLS: the same reads answering prove death (another boot, an absent pid, a
/// different start) and life (the same start).
#[test]
fn a_failed_kernel_read_is_unknown_and_never_death() {
    use aterm_uds::process::ProcessPresence;
    let judged = |boot: Option<&str>, presence: ProcessPresence, start: Option<u64>| {
        judge_with(
            4242,
            "b",
            7,
            &Kernel {
                boot_id: &|| boot.map(str::to_owned),
                presence: &|pid| {
                    assert_eq!(pid, 4242);
                    presence
                },
                start_of: &|_| start,
            },
        )
    };
    assert_eq!(judged(None, ProcessPresence::Absent, None), Judged::Unknown);
    assert_eq!(
        judged(Some("b"), ProcessPresence::Unknown, Some(8)),
        Judged::Unknown
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Present, None),
        Judged::Unknown
    );
    assert_eq!(
        judged(Some("c"), ProcessPresence::Present, Some(7)),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Absent, None),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Exited, None),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Present, Some(8)),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Present, Some(7)),
        Judged::Alive
    );
}

/// The row answering a holder query is this kernel's judgement of exactly that identity.
#[test]
fn a_holder_row_judges_exactly_the_identity_asked() {
    use aterm_ctl::release_holder::Identity;
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    let me = Identity { pid, boot, start };
    assert_eq!(me.read(&holder_row(&me)), Some(Some(true)));
    let reused = Identity {
        start: start.wrapping_add(1),
        ..me.clone()
    };
    assert_eq!(reused.read(&holder_row(&reused)), Some(Some(false)));
    // NEGATIVE CONTROL: the row about one identity says nothing of another.
    assert_eq!(me.read(&holder_row(&reused)), None);
}

fn current_record(padding: usize) -> String {
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    aterm_json::json!({"holder":{"pid":pid,"boot":boot,"start":start},
        "rows":[],"padding":"x".repeat(padding)})
    .to_string()
}

#[test]
fn survey_omits_oversize_valid_json_and_growth_after_admission() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let path = registry.join("line.json");
    let small = current_record(0);
    std::fs::write(&path, &small).unwrap();
    assert_eq!(survey(dir.path(), wall()).len(), 1);
    let (held, metadata) = aterm_uds::regular_file::open(&path).unwrap();
    let large = current_record(MAX_RECORD_BYTES as usize);
    assert!(aterm_json::from_str::<aterm_json::Value>(&large).is_ok());
    std::fs::write(&path, large).unwrap();
    assert!(read_record(held, metadata.len()).is_none());
    assert!(survey(dir.path(), wall()).is_empty());
    std::fs::write(&path, small).unwrap();
    assert_eq!(survey(dir.path(), wall()).len(), 1);
}

#[test]
fn admitted_record_does_not_reopen_a_replaced_path() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let path = registry.join("line.json");
    let old = current_record(0);
    std::fs::write(&path, &old).unwrap();
    let (held, metadata) = aterm_uds::regular_file::open(&path).unwrap();
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, current_record(MAX_RECORD_BYTES as usize)).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(read_record(held, metadata.len()).unwrap(), old.as_bytes());
    assert!(survey(dir.path(), wall()).is_empty());
}

#[test]
fn survey_refuses_final_aliases_directories_and_fifos() {
    use std::os::unix::ffi::OsStrExt as _;
    if !aterm_uds::test_binary::isolated_lifecycle(
        "release_progress::tests::survey_refuses_final_aliases_directories_and_fifos",
    ) {
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, current_record(0)).unwrap();
    std::os::unix::fs::symlink(&target, registry.join("alias.json")).unwrap();
    std::fs::create_dir(registry.join("directory.json")).unwrap();
    let fifo = registry.join("fifo.json");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the C string is terminated and live; this creates only a FIFO
    // in the test's owned directory, with no reader or writer to wait for.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    assert!(survey(dir.path(), wall()).is_empty());
    std::fs::write(registry.join("valid.json"), current_record(0)).unwrap();
    assert_eq!(survey(dir.path(), wall()).len(), 1);
}

/// The registry a cutter writes its record in ([`crate::harness::cli::harness_state_dir`]) is
/// the one agh reads to prove a cut's process tree ([`aterm_ctl::release_holder::registry`]).
#[test]
fn the_cutters_registry_is_the_one_agh_reads() {
    assert_eq!(
        crate::harness::cli::harness_state_dir()
            .ok()
            .map(|state| state.join(DIRECTORY)),
        aterm_ctl::release_holder::registry()
    );
}

/// The injected clock of the lease tests, years past any file a test writes.
const NOW: u64 = 2_000_000_000;

/// A record naming holder `(pid, start)` in this boot, renewed at `at`, padded by `padding`
/// bytes of evidence.
fn record_of(pid: u32, start: u64, at: Option<u64>, padding: usize) -> aterm_json::Value {
    let (boot, _) = identity(std::process::id()).unwrap();
    aterm_json::json!({"holder": {"pid": pid, "boot": boot, "start": start}, "at": at,
        "step": "commit", "evidence": "x".repeat(padding), "rows": []})
}

/// Write `record` at `path` and set the file's time to unix second `mtime`.
fn write_record(path: &Path, record: &aterm_json::Value, mtime: u64) {
    std::fs::write(path, record.to_string()).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(mtime))
        .unwrap();
}

/// A proven death stands lapsed only once its last renewal, the later of the record's `at` and
/// its file's time, lies more than a whole lease from `now` either way; a live or unknown holder
/// never does, however old its record.
/// NEGATIVE CONTROLS: a renewal exactly one lease old stays leased (a `>=` rule drops it); an old
/// `at` in a file written since stays leased (a rule on `at` alone), and so does a fresh `at` in
/// an old file (a rule on the file's time alone); a renewal more than a lease ahead lapses (the
/// skew guard `line_claim::expired` applies); the very process and pid 1 stay however old (a
/// rule on age alone, or one that reads unknown as dead).
#[test]
fn a_dead_holders_record_lapses_only_a_whole_lease_after_its_last_renewal() {
    let pid = std::process::id();
    let (_, start) = identity(pid).unwrap();
    let dead = |at| record_of(pid, start.wrapping_add(1), at, 0);
    let lease = MAX_LEASE_S;
    for (record, mtime, want) in [
        (
            dead(Some(NOW - lease - 1)),
            Some(NOW - lease - 1),
            Standing::DeadLapsed,
        ),
        (
            dead(Some(NOW - lease)),
            Some(NOW - lease),
            Standing::DeadLeased,
        ),
        (
            dead(Some(NOW - 10 * lease)),
            Some(NOW - 60),
            Standing::DeadLeased,
        ),
        (
            dead(Some(NOW - 60)),
            Some(NOW - 10 * lease),
            Standing::DeadLeased,
        ),
        (dead(None), Some(NOW - lease - 1), Standing::DeadLapsed),
        (dead(Some(NOW - lease - 1)), None, Standing::DeadLapsed),
        (dead(None), None, Standing::DeadLeased),
        (
            dead(Some(NOW + lease + 1)),
            Some(NOW + lease + 1),
            Standing::DeadLapsed,
        ),
        (
            dead(Some(NOW + lease)),
            Some(NOW + lease),
            Standing::DeadLeased,
        ),
        (record_of(pid, start, Some(0), 0), Some(0), Standing::Live),
        (record_of(1, start, Some(0), 0), Some(0), Standing::Unknown),
    ] {
        assert_eq!(
            standing(&record, mtime, NOW),
            Some(want),
            "{record} {mtime:?}"
        );
    }
    assert_eq!(
        standing(&aterm_json::json!({"holder": {}}), Some(0), NOW),
        None
    );
}

/// Two hundred dead records whose leases lapsed beside one live holder survey as the live row
/// alone; two hundred dead records still leased survey within [`SURVEY_BYTES_MAX`], the live row
/// first.
/// NEGATIVE CONTROLS: the previous reader returned a row for every record (401 here), and a
/// survey without its byte bound returns all 200 leased rows.
#[test]
fn the_survey_leaves_out_lapsed_records_and_bounds_the_rest() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let pid = std::process::id();
    let (_, start) = identity(pid).unwrap();
    write_record(
        &registry.join("live.json"),
        &record_of(pid, start, Some(NOW - 5 * MAX_LEASE_S), 700),
        NOW,
    );
    for n in 0..200 {
        let at = NOW - MAX_LEASE_S - 1 - n;
        write_record(
            &registry.join(format!("lapsed-{n}.json")),
            &record_of(pid, start.wrapping_add(1 + n), Some(at), 700),
            at,
        );
    }
    let rows = survey(dir.path(), NOW);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].starts_with("release_line alive=1 "));
    for n in 0..200 {
        let at = NOW - 60 - n;
        write_record(
            &registry.join(format!("leased-{n}.json")),
            &record_of(pid, start.wrapping_add(1000 + n), Some(at), 700),
            at,
        );
    }
    let rows = survey(dir.path(), NOW);
    let bytes: usize = rows.iter().map(|row| row.len() + 1).sum();
    assert!(bytes <= SURVEY_BYTES_MAX, "{bytes}");
    assert!(rows.len() > 1 && rows.len() < 200, "{}", rows.len());
    assert!(rows[0].starts_with("release_line alive=1 "));
    assert!(
        rows[1..]
            .iter()
            .all(|row| row.starts_with("release_line alive=0 "))
    );
    // The newest renewal comes first among the dead: the record renewed at NOW - 60.
    let newest = aterm_codec::pct::decode(rows[1].split_once(" data=").unwrap().1);
    assert!(newest.contains(&format!("\"at\":{}", NOW - 60)), "{newest}");
}

/// `collect` removes exactly the records that stand lapsed, and nothing else in the folder.
/// NEGATIVE CONTROLS: a record still leased, a live holder's and an unknown one's however old,
/// an unparsable file, a record under another suffix, a link to a lapsed record, a folder named
/// like a record and a `write_atomic_durable` stage all stay; a collector by age alone removes
/// the old live and unknown records, one by suffix alone the unparsable file, and one that
/// follows links the link's target.
#[test]
fn collect_removes_only_records_whose_leases_lapsed_for_every_reader() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let pid = std::process::id();
    let (_, start) = identity(pid).unwrap();
    let old = NOW - 10 * MAX_LEASE_S;
    let lapsed = record_of(pid, start.wrapping_add(1), Some(old), 0);
    write_record(&registry.join("0.206.0-1-2.json"), &lapsed, old);
    write_record(
        &registry.join("leased.json"),
        &record_of(pid, start.wrapping_add(2), Some(NOW - 60), 0),
        NOW - 60,
    );
    write_record(
        &registry.join("live.json"),
        &record_of(pid, start, Some(old), 0),
        old,
    );
    write_record(
        &registry.join("unknown.json"),
        &record_of(1, start, Some(old), 0),
        old,
    );
    std::fs::write(registry.join("junk.json"), b"{not json").unwrap();
    write_record(&registry.join("other.txt"), &lapsed, old);
    let target = dir.path().join("target.json");
    write_record(&target, &lapsed, old);
    std::os::unix::fs::symlink(&target, registry.join("alias.json")).unwrap();
    std::fs::create_dir(registry.join("folder.json")).unwrap();
    std::fs::create_dir(registry.join(".stage-a.noindex")).unwrap();
    write_record(
        &registry.join(".stage-a.noindex").join("data"),
        &lapsed,
        old,
    );
    assert_eq!(collect(dir.path(), NOW), 1);
    let mut left: Vec<String> = std::fs::read_dir(&registry)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            ".stage-a.noindex",
            "alias.json",
            "folder.json",
            "junk.json",
            "leased.json",
            "live.json",
            "other.txt",
            "unknown.json"
        ]
    );
    assert!(target.is_file());
    assert!(registry.join(".stage-a.noindex").join("data").is_file());
    // A second pass finds nothing more to remove, and a registry that is absent is no error.
    assert_eq!(collect(dir.path(), NOW), 0);
    assert_eq!(collect(&dir.path().join("absent"), NOW), 0);
}

/// THE PUMP'S FACT IS ANSWERED WITH ITS AGE AND ITS PROCESS JUDGED HERE (UB-15, `pump=`). The
/// record a pump keeps in the harness state folder becomes one `release_pump` row whose age is
/// this host's own difference and whose liveness is this kernel's word on the recorded identity.
/// Each clone's pump keeps a record of its own: another clone's (a `--once` there, a second
/// clone's pump) is a row beside it, newest first, and overwrites nothing.
/// NEGATIVE CONTROLS: the same pid with another start reads dead; a record that names no identity
/// is unknown; an oversize file, a folder, a file not named `*.json` and a missing folder answer
/// no row.
#[test]
fn the_pump_record_is_answered_with_its_age_and_its_process_judged_here() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let folder = dir.path().join(aterm_ctl::release_pump::DIRECTORY);
    assert_eq!(pump_rows(dir.path(), wall()), Vec::<String>::new());
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("primary.json");
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    let record = |start: Option<u64>| aterm_ctl::release_pump::Record {
        holder: "m16".into(),
        line: "v0.201.0@9afa50b42".into(),
        step: "wait".into(),
        own: true,
        at: 1_000,
        decision: "a session publishes it".into(),
        pid,
        boot: start.map(|_| boot.clone()),
        start,
    };
    let only = |rows: Vec<String>| {
        let [row] = &rows[..] else {
            panic!("one row: {rows:?}");
        };
        aterm_ctl::release_pump::Pump::parse(row).unwrap()
    };
    std::fs::write(&path, record(Some(start)).json()).unwrap();
    let pump = only(pump_rows(dir.path(), 1_090));
    assert_eq!(
        (pump.alive, pump.age_s, pump.step.as_str(), pump.pid),
        (Some(true), 90, "wait", pid)
    );
    // Another clone's `--once` writes its own record: both are answered, newest first.
    let once = aterm_ctl::release_pump::Record {
        own: false,
        at: 1_050,
        pid: pid.wrapping_add(1),
        ..record(None)
    };
    std::fs::write(folder.join("once.json"), once.json()).unwrap();
    let both: Vec<_> = pump_rows(dir.path(), 1_090)
        .iter()
        .map(|row| aterm_ctl::release_pump::Pump::parse(row).unwrap())
        .collect();
    assert_eq!(
        both.iter()
            .map(|pump| (pump.own, pump.age_s))
            .collect::<Vec<_>>(),
        [(Some(false), 40), (Some(true), 90)]
    );
    std::fs::remove_file(folder.join("once.json")).unwrap();
    std::fs::write(&path, record(Some(start.wrapping_add(1))).json()).unwrap();
    assert_eq!(only(pump_rows(dir.path(), 1_090)).alive, Some(false));
    std::fs::write(&path, record(None).json()).unwrap();
    assert_eq!(only(pump_rows(dir.path(), 1_090)).alive, None);
    std::fs::write(folder.join("stray.txt"), record(Some(start)).json()).unwrap();
    assert_eq!(pump_rows(dir.path(), 1_090).len(), 1);
    let oversize = usize::try_from(aterm_ctl::release_pump::MAX_RECORD_BYTES).unwrap() + 1;
    std::fs::write(&path, " ".repeat(oversize)).unwrap();
    assert_eq!(pump_rows(dir.path(), 1_090), Vec::<String>::new());
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(pump_rows(dir.path(), 1_090), Vec::<String>::new());
}
