// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The document host's tests that need aterm-gui. The host itself is
//! `amacs_files::host` (aliased here as `native_document_host`), and its
//! self-contained tests live beside it there. These three reach what only
//! aterm-gui has: a stand-in child of this test binary (`test_alone`), and the
//! config service's semantic admission cap (`native_config_service`).

use std::fs;
use std::path::{Path, PathBuf};

use crate::native_document_host::{
    AtomicCommitResult, DEFAULT_DOCUMENT_LIMIT, DocumentGrantStore, GrantAccess,
    commit_atomic_bytes, path_to_file_uri, read_atomic_file,
};
use crate::native_document_io::{AtomicSaveResult, AtomicSaveStage, SaveReducer};

fn unique_file(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aterm-native-document-host-{}-{}",
        std::process::id(),
        name
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hello world.md");
    fs::write(&path, bytes).unwrap();
    path
}

fn file_uri(path: &Path) -> String {
    path_to_file_uri(path).unwrap()
}

#[cfg(unix)]
#[test]
fn writerless_fifo_is_rejected_without_blocking_config_or_save_observation() {
    use std::os::unix::ffi::OsStrExt as _;

    use crate::native_document_host::{DocumentHostError, observe_file, read_config_atomic_file};

    const CHILD: &str = "ATERM_DOCUMENT_FIFO_TEST_CHILD";
    const PATH: &str = "ATERM_DOCUMENT_FIFO_TEST_PATH";
    // The child says WHEN it reached the FIFO calls, so the wait below is for those
    // calls and not this machine's process startup. See the parent half for what that
    // cost measured.
    let started_marker = |fifo: &Path| fifo.with_file_name("child-reached-the-fifo");
    if std::env::var_os(CHILD).is_some() {
        let path = PathBuf::from(std::env::var_os(PATH).unwrap());
        fs::write(started_marker(&path), b"1").expect("the child can write beside the fifo");
        assert!(matches!(
            read_config_atomic_file(&path, 1024, false),
            Err(DocumentHostError::NotAFile)
        ));
        assert!(observe_file(&path, 16).is_err());
        return;
    }

    let dir =
        std::env::temp_dir().join(format!("aterm-native-document-fifo-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("aterm.toml");
    let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `path_c` is a live NUL-terminated pathname and mkfifo retains
    // no pointer. The private path does not exist.
    assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
    let mut child = crate::test_alone::StandIn::this_test();
    child.command().env(CHILD, "1").env(PATH, &path);
    let mut child = [child.spawn()];
    // TWO WAITS, because they answer different questions and only one of them is this
    // test's subject. What it guards is that opening a writerless FIFO does not block
    // FOR EVER, so the second wait starts when the child says it got there — the marker
    // it writes as its first act. Timing the child's whole life instead conflated the
    // block with the cost of spawning a fresh copy of this test binary (a loaded
    // machine failed a 3 s bound on that cost alone). Each is a hang detector, never a
    // latency budget, and says what it means when it fires: a child that never reaches
    // the calls is a different defect from one that hangs in them.
    let marker = started_marker(&path);
    crate::test_alone::until_or_ended("the child reaches the FIFO calls", &mut child, || {
        marker.exists()
    });
    let [mut child] = child;
    let returned = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        std::time::Duration::from_millis(5),
        || child.exited().then_some(()),
    );
    assert!(
        returned.is_some(),
        "writerless FIFO observation blocked: the child reached the calls and did not \
         return from them within {:?}",
        aterm_spec::wait::MUST_HAPPEN
    );
    child.passed();
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn manual_can_open_and_repair_config_above_the_semantic_admission_cap() {
    let path = unique_file(
        "manual-repair-oversized-config",
        &vec![b' '; crate::native_config_service::MAX_CONFIG_FILE_BYTES + 1],
    );
    let mut grants = DocumentGrantStore::new();
    let opened = grants
        .open_local_config(
            &file_uri(&path),
            GrantAccess::ReadWrite,
            DEFAULT_DOCUMENT_LIMIT,
        )
        .unwrap();
    assert_eq!(
        opened.text.len(),
        crate::native_config_service::MAX_CONFIG_FILE_BYTES + 1
    );

    let mut documents = crate::test_document_store();
    let document = documents.open(
        opened.grant.canonical_uri.clone(),
        "theme = \"Nord\"\n".to_string(),
    );
    let mut reducer = SaveReducer::new(document, opened.observed);
    let plan = reducer
        .begin(&documents.snapshot(document).unwrap())
        .unwrap();
    assert!(matches!(
        grants.execute_save(opened.grant.id, &plan),
        AtomicSaveResult::Committed(_)
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "theme = \"Nord\"\n");
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// Two independent test processes capture the same disk generation, then
/// race distinct desired bytes through the shipping lock/CAS primitive.
/// Exactly one may commit; the loser must observe Conflict and cannot erase
/// the winner. This also exercises PID-separated temporary names for real.
#[test]
fn cross_process_same_baseline_has_one_winner_and_no_temp_collision() {
    const CHILD: &str = "ATERM_CONFIG_COMMIT_TEST_CHILD";
    // Every wait below is for something that must happen — a child test
    // process of this whole binary starting (seconds of exec on a loaded
    // machine), the parent's go, the peer's lock hold ending — so each is
    // a hang detector, never a latency budget (bounds of 10 s and 3 s
    // failed all three cross-process tests together under a 24-way CPU
    // load).
    const HANG: std::time::Duration = std::time::Duration::from_secs(60);
    if let Ok(role) = std::env::var(CHILD) {
        let path = PathBuf::from(std::env::var("ATERM_CONFIG_COMMIT_TEST_PATH").unwrap());
        let root = path.parent().unwrap();
        let baseline = read_atomic_file(&path, 4096, false).unwrap().baseline;
        fs::write(root.join(format!("ready-{role}")), b"ready").unwrap();
        let go = root.join("go");
        let deadline = std::time::Instant::now() + HANG;
        while !go.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "parent never released race"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let desired = format!("winner = {role:?}\n");
        let retry_deadline = std::time::Instant::now() + HANG;
        let verdict = loop {
            match commit_atomic_bytes(&baseline, desired.as_bytes()) {
                AtomicCommitResult::Committed(_) => break "committed",
                AtomicCommitResult::Conflict { .. } => break "conflict",
                AtomicCommitResult::Failed {
                    stage: AtomicSaveStage::Preflight,
                    cause:
                        Some(crate::native_document_io::SaveCause {
                            kind: std::io::ErrorKind::WouldBlock,
                            ..
                        }),
                    ..
                } => {
                    assert!(
                        std::time::Instant::now() < retry_deadline,
                        "save lock remained busy past the bounded retry window"
                    );
                    std::thread::yield_now();
                }
                other => panic!("unexpected child commit result: {other:?}"),
            }
        };
        fs::write(root.join(format!("result-{role}")), verdict).unwrap();
        return;
    }

    let root =
        std::env::temp_dir().join(format!("aterm-config-process-race-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let path = root.join("aterm.toml");
    fs::write(&path, "winner = \"none\"\n").unwrap();
    // Each racer is this very test as a stand-in, which must run exactly
    // this test and pass it.
    let mut children: Vec<_> = ["manual", "settings"]
        .into_iter()
        .map(|role| {
            let mut child = crate::test_alone::StandIn::this_test();
            child
                .command()
                .env(CHILD, role)
                .env("ATERM_CONFIG_COMMIT_TEST_PATH", &path);
            child.spawn()
        })
        .collect();
    crate::test_alone::until_or_ended("both racers reach the barrier", &mut children, || {
        ["manual", "settings"]
            .iter()
            .all(|role| root.join(format!("ready-{role}")).exists())
    });
    fs::write(root.join("go"), b"go").unwrap();
    for child in children {
        child.passed();
    }
    let results = ["manual", "settings"]
        .map(|role| fs::read_to_string(root.join(format!("result-{role}"))).unwrap());
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_str() == "committed")
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_str() == "conflict")
            .count(),
        1
    );
    let final_text = fs::read_to_string(&path).unwrap();
    assert!(final_text.contains("manual") || final_text.contains("settings"));
    assert!(fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")
    }));
    let _ = fs::remove_dir_all(&root);
}
