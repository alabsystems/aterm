// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Build, Fixture, HANG, POLL, held_claims, record_files};
use aterm_capacity::{Held, Need, Record};
use std::path::Path;

const UNWIND_STANDIN: &str = "ATERM_LAB_BUILD_ADMIT_UNWIND_STANDIN";

#[test]
fn sampling_threshold_is_whole_seconds_below_the_hang_detector() {
    use super::{parse, sample_delay};
    use std::time::Duration;

    assert_eq!(sample_delay("0"), Some(Duration::ZERO));
    assert_eq!(sample_delay("59"), Some(Duration::from_secs(59)));
    for invalid in ["60", "-1", "1.5", "later"] {
        assert_eq!(sample_delay(invalid), None);
    }
    let scratch = aterm_tempfile::tempdir().unwrap();
    let targo = scratch.path().join("targo");
    std::fs::write(&targo, "path validation only; never executed\n").unwrap();
    let mut args = vec!["--targo".to_owned(), targo.to_str().unwrap().to_owned()];
    assert!(parse(&args).unwrap().sample_after.is_none());
    args.extend(["--sample-after", "0"].map(str::to_owned));
    assert_eq!(parse(&args).unwrap().sample_after, Some(Duration::ZERO));
    args.extend(["--sample-after", "1"].map(str::to_owned));
    assert!(parse(&args).is_err());
    assert!(parse(&["--sample-after".to_owned()]).is_err());
}

#[test]
fn unwind_standin() {
    let Some(root) = std::env::var_os(UNWIND_STANDIN) else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    std::fs::write(root.join("ready"), std::process::id().to_string()).unwrap();
    while !root.join("release").exists() {
        std::thread::sleep(POLL);
    }
    std::fs::write(root.join("done"), std::process::id().to_string()).unwrap();
}

fn standin(root: &Path) -> aterm_uds::spawnfd::Child {
    aterm_uds::spawnfd::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "build_admit::tests::unwind_standin",
            "--test-threads=1",
        ])
        .env_clear()
        .env(UNWIND_STANDIN, root)
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .spawn()
        .unwrap()
}

fn fixture(root: &Path, child: aterm_uds::spawnfd::Child) -> Fixture {
    Fixture {
        root: root.to_path_buf(),
        ledger: aterm_capacity::Ledger::at(root.join("ledger")),
        keep: true,
        sample_after: None,
        builds: vec![Build {
            child,
            ready: root.join("ready"),
            release: root.join("release"),
            log: root.join("log"),
            witness: None,
            status: None,
            sampled: false,
        }],
    }
}

fn until(mut done: impl FnMut() -> bool) -> bool {
    let started = std::time::Instant::now();
    while !done() {
        if started.elapsed() >= HANG {
            return false;
        }
        std::thread::sleep(POLL);
    }
    true
}

#[test]
fn an_unwind_releases_its_held_child_to_finish_naturally() {
    let mut scratch =
        aterm_uds::deadline::KeptUntilRan::new(aterm_tempfile::tempdir().unwrap().keep());
    let root = scratch.path().to_path_buf();
    // The current test image stays in place; this scratch directory holds only markers.
    let child = standin(&root);
    let pid = i32::try_from(child.id()).unwrap();
    let fixture = fixture(&root, child);
    let ready = until(|| {
        std::fs::read_to_string(root.join("ready"))
            .ok()
            .is_some_and(|text| text == pid.to_string())
    });
    let identity = aterm_uds::process::read_identity(pid);
    let completed = || {
        root.join("done").is_file()
            && (aterm_uds::process::process_presence(pid)
                == aterm_uds::process::ProcessPresence::Absent
                || aterm_uds::process::read_identity(pid).is_some_and(|now| Some(now) != identity))
    };
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _fixture = fixture;
        panic!("stand-in for an output failure while the scripts are held");
    }))
    .is_err();
    let released = root.join("release").is_file();
    let finished_by_drop = released && until(completed);
    // NEGATIVE CONTROL: removing Drop must fail only after the held child is released
    // and finishes; neither an assertion nor the failed observation may strand it.
    let fallback = std::fs::write(root.join("release"), "go\n");
    let finished = until(completed);
    if ready && finished {
        scratch.ran();
    }
    assert!(
        fallback.is_ok() && finished,
        "fallback did not finish the child"
    );
    assert!(ready && identity.is_some(), "child never proved execution");
    assert!(
        unwound && finished_by_drop,
        "unwind left its child waiting for a barrier"
    );
}

#[test]
fn dropping_a_completed_fixture_does_not_recreate_removed_scratch() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path().to_path_buf();
    std::fs::write(root.join("release"), "go\n").unwrap();
    let mut fixture = fixture(&root, standin(&root));
    let status = super::reap(&mut fixture.builds[0].child, HANG, super::Reap::Child).unwrap();
    fixture.builds[0].status = Some(status);
    let ran = root.join("ready").is_file() && root.join("done").is_file();
    std::fs::remove_dir_all(&root).unwrap();
    drop(fixture);
    assert!(status.success() && ran, "child did not finish naturally");
    assert!(
        !root.exists(),
        "dropping the fixture recreated removed scratch"
    );
}

fn claim(pid: u32) -> Record {
    Record {
        accounting: aterm_capacity::Accounting::Holder,
        timing: aterm_capacity::TimingEvidence::default(),
        pid,
        start_ms: 1,
        seq: 0,
        checkout: format!("checkout-{pid}"),
        need: Need::new("targo build").mem(1 << 30).covers(),
        held: Some(Held {
            admitted_ms: 1,
            cores: 0,
            alone: false,
        }),
        credit: Vec::new(),
        wrote: None,
        next_ms: None,
    }
}

#[test]
fn snapshots_reject_missing_waiting_duplicate_and_surviving_claims() {
    let both = vec![claim(11), claim(22)];
    assert!(held_claims(&both, &[11, 22]).is_ok());
    assert!(
        held_claims(&both, &[22]).is_err(),
        "first claim never retired"
    );
    assert!(
        held_claims(&both[..1], &[11, 22]).is_err(),
        "second build never claimed"
    );
    assert!(
        held_claims(&both, &[11, 11]).is_err(),
        "one front counted twice"
    );
    assert!(held_claims(&both[1..], &[22]).is_ok());
    let mut bad = both.clone();
    bad[1].held = None;
    assert!(held_claims(&bad, &[11, 22]).is_err(), "waiting is not held");
    bad[1] = claim(22);
    bad[1].need.covers = false;
    assert!(
        held_claims(&bad, &[11, 22]).is_err(),
        "claim does not cover its build"
    );
    bad[1] = claim(22);
    bad[1].need.mem = 0;
    assert!(
        held_claims(&bad, &[11, 22]).is_err(),
        "claim reserves no memory"
    );
    assert!(held_claims(&[], &[]).is_ok());
}

#[test]
fn an_empty_live_ledger_does_not_prove_physical_retirement() {
    let dir = aterm_tempfile::tempdir().expect("scratch ledger");
    let ledger = aterm_capacity::Ledger::at(dir.path());
    let stale = dir.path().join("0000000000001-11-0.claim");
    std::fs::write(&stale, "unlocked orphan\n").expect("orphan record");
    std::fs::write(dir.path().join("ledger.lock"), "").expect("admission lock file");
    assert!(ledger.list().expect("live claims").is_empty());
    let files = record_files(dir.path()).expect("physical records");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0], stale);
    let waiting = dir.path().join("0000000000002-22-0.wait");
    std::fs::write(&waiting, "unlocked waiting orphan\n").expect("waiting record");
    assert_eq!(record_files(dir.path()).expect("physical records").len(), 2);
    std::fs::remove_file(stale).expect("retire held record");
    std::fs::remove_file(waiting).expect("retire waiting record");
    assert!(
        record_files(dir.path())
            .expect("retired records")
            .is_empty()
    );
}
