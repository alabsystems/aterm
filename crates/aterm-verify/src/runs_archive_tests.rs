// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const SEED: &str = "run v=1 boot=B id=41983-1 uniqueid=100 start=1 sid=t0123456789\n\
root path=/wt-a\n\
group pgid=500 leader_birth=5 leader_uniqueid=101 stage=first%20stage\n\
closed pgid=500\n\
group pgid=500 leader_birth=6 leader_uniqueid=102 stage=second%20stage\n\
closed pgid=500\n";

fn fixture(bytes: &[u8]) -> (aterm_tempfile::TempDir, Runs, FakeKernel) {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("41983-1.run");
    std::fs::write(&path, bytes).unwrap();
    let runs = Runs {
        log: Mutex::new(open_regular_write(&path, true).unwrap()),
        path,
        boot: "B".into(),
        id: RunId {
            pid: 41983,
            birth: Birth(1),
        },
        uniqueid: 100,
        roots: vec![PathBuf::from("/wt-a")],
        built: Vec::new(),
        swept: Mutex::new(Vec::new()),
        made: Mutex::new(BTreeMap::new()),
        said: Mutex::new(Vec::new()),
        may_forget: AtomicBool::new(false),
        poster: Arc::new(Inert),
        kin: runs_kin::KinLedger::new(dir.path().join("x.kin"), None, Vec::new()),
    };
    let mut kernel = FakeKernel::default();
    let mut owner = lin(41983, 100, 41983, 1);
    owner.identity.birth = Birth(1);
    kernel.put(owner, "/usr/local/bin/aterm-verify");
    (dir, runs, kernel)
}

#[test]
fn an_admitted_clear_finish_archives_exact_generations_before_removal() {
    let (_dir, runs, kernel) = fixture(SEED.as_bytes());
    let logs = aterm_tempfile::tempdir().unwrap();
    let archive = archive_path(&logs.path().join("native.log"));
    assert!(runs.end_of_run(&kernel).survivors.is_empty());
    runs.finish(&kernel, Some(&archive)).unwrap();
    assert!(!runs.path.exists());
    let text = std::fs::read_to_string(&archive).unwrap();
    assert_eq!(text, format!("{SEED}end at=42\n"));
    let record = aterm_uds::runs::completed_archive(&text).unwrap();
    assert_eq!(record.id, Some(runs.id));
    assert_eq!(record.boot, "B");
    assert_eq!(record.sid.as_deref(), Some("t0123456789"));
    assert_eq!(record.roots, vec![PathBuf::from("/wt-a")]);
    assert_eq!(record.groups.len(), 2);
    assert_eq!(record.groups[0].leader_birth, Birth(5));
    assert_eq!(record.groups[1].leader_birth, Birth(6));
    assert!(record.groups.iter().all(|group| group.closed));
}

#[test]
fn an_unjudged_finish_archives_without_inventing_an_end_or_removal() {
    let (_dir, runs, mut kernel) = fixture(SEED.as_bytes());
    kernel.put(lin(75, 304, 75, 1), "/wt-a/target/debug/aterm");
    kernel.markers.insert(75, verify_run("41983-1"));
    kernel.held.insert(75, Some(true));
    let report = runs.end_of_run(&kernel);
    assert!(
        report
            .survivors
            .iter()
            .any(|row| row.survivor.reason == SurvivorReason::ExecutionUnobserved)
    );
    let logs = aterm_tempfile::tempdir().unwrap();
    let archive = archive_path(&logs.path().join("native.log"));
    runs.finish(&kernel, Some(&archive)).unwrap();
    assert_eq!(std::fs::read(&archive).unwrap(), SEED.as_bytes());
    assert_eq!(std::fs::read(&runs.path).unwrap(), SEED.as_bytes());
    assert!(aterm_uds::runs::completed_archive(SEED).is_none());
}

#[test]
fn a_partial_record_is_archived_byte_for_byte_and_remains_native_owned() {
    let partial = format!("{SEED}group pgid=501 leader_birth=");
    let (_dir, runs, kernel) = fixture(partial.as_bytes());
    let _ = runs.end_of_run(&kernel);
    let logs = aterm_tempfile::tempdir().unwrap();
    let archive = archive_path(&logs.path().join("native.log"));
    runs.finish(&kernel, Some(&archive)).unwrap();
    assert_eq!(std::fs::read(&archive).unwrap(), partial.as_bytes());
    assert_eq!(std::fs::read(&runs.path).unwrap(), partial.as_bytes());
    assert!(aterm_uds::runs::completed_archive(&partial).is_none());
}

#[test]
fn a_failed_archive_preserves_the_original_and_existing_destination() {
    let (_dir, runs, kernel) = fixture(SEED.as_bytes());
    let _ = runs.end_of_run(&kernel);
    let logs = aterm_tempfile::tempdir().unwrap();
    let archive = archive_path(&logs.path().join("native.log"));
    std::fs::write(&archive, b"unrelated archive").unwrap();
    assert!(runs.finish(&kernel, Some(&archive)).is_err());
    assert_eq!(std::fs::read(&archive).unwrap(), b"unrelated archive");
    assert_eq!(
        std::fs::read_to_string(&runs.path).unwrap(),
        format!("{SEED}end at=42\n")
    );
}

#[test]
fn no_admitted_log_retains_the_original_without_a_terminal_claim() {
    let (_dir, runs, kernel) = fixture(SEED.as_bytes());
    let _ = runs.end_of_run(&kernel);
    runs.finish(&kernel, None).unwrap();
    assert_eq!(std::fs::read(&runs.path).unwrap(), SEED.as_bytes());
}
