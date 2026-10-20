// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::crash_signal::markers::{Owner, probe};

/// The files under `<dir>/edges`, sorted.
fn edge_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("edges"))
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    files.sort();
    files
}

/// Child provisions wait, [`DEPTH`] of them, as edge-token files written and
/// locked under the control dir they were made for. A take hands its file over
/// whole: still there, still locked, and named by the env the child gets. A
/// control dir that moves retires the ones made under the old one, a provision
/// nobody took removing its own file, and with no control dir none waits.
/// NEGATIVE CONTROL: a take for a dir nothing was made under gets nothing.
#[test]
fn provisions_wait_under_their_control_dir_and_an_untaken_one_removes_its_file() {
    let first = aterm_tempfile::tempdir().expect("a control dir");
    let second = aterm_tempfile::tempdir().expect("another control dir");
    let (first, second) = (first.path(), second.path());
    let ready = Mutex::new(Vec::new());

    fill_provisions(
        &ready,
        Some(first),
        Provision::prepare,
        &AtomicBool::new(false),
    );
    let made = edge_files(first);
    assert_eq!(made.len(), DEPTH, "{made:?}");
    for file in &made {
        assert_eq!(probe(file), Owner::Live, "{} is locked", file.display());
    }
    assert!(
        take_from(&ready, Some(second)).is_none(),
        "nothing waits for a dir nothing was made under"
    );
    let (env, prov, file) = take_from(&ready, Some(first)).expect("one waits");
    let file = file.expect("its edge-token file");
    let named = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    assert_eq!(
        named(aterm_types::domain::ENV_EDGE_TOKENS).as_deref(),
        file.path().to_str(),
        "the child is told the file's path"
    );
    assert_eq!(
        named(aterm_types::domain::ENV_SESSION_ID).as_deref(),
        Some(prov.child_sid.as_str()),
        "and the identity the tokens are for"
    );
    assert_eq!(probe(file.path()), Owner::Live, "taken still locked");

    fill_provisions(
        &ready,
        Some(second),
        Provision::prepare,
        &AtomicBool::new(false),
    );
    assert_eq!(
        edge_files(first),
        [file.path().to_path_buf()],
        "the untaken one under the old dir removed its file; the taken one is its spawn's"
    );
    assert_eq!(edge_files(second).len(), DEPTH);
    fill_provisions(&ready, None, Provision::prepare, &AtomicBool::new(false));
    assert!(edge_files(second).is_empty(), "no control dir: none waits");
    assert!(ready.lock().unwrap().is_empty());

    remove_edge_file(file);
    assert!(edge_files(first).is_empty(), "a retired file is removed");
}

/// A dir that refuses the file leaves nothing waiting, so every spawn takes
/// the inline path, which says why as it always did.
#[test]
fn a_dir_that_refuses_the_file_leaves_no_provision() {
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    // `edges` is a file, so no edge-token file can be written under it.
    std::fs::write(scratch.path().join("edges"), b"").expect("a file in the way");
    let ready = Mutex::new(Vec::new());
    fill_provisions(
        &ready,
        Some(scratch.path()),
        Provision::prepare,
        &AtomicBool::new(false),
    );
    assert!(ready.lock().unwrap().is_empty());
}

/// A fill the final exit stopped waiting for mints nothing that outlives it:
/// a provision minted once the exit set its flag removes its own file rather
/// than waiting where the exit already took the provisions from. NEGATIVE
/// CONTROL: the same fill before the flag keeps [`DEPTH`] files.
#[test]
fn a_fill_after_the_release_leaves_no_file() {
    let dir = aterm_tempfile::tempdir().expect("a control dir");
    let ready = Mutex::new(Vec::new());
    let released = AtomicBool::new(false);
    fill_provisions(&ready, Some(dir.path()), Provision::prepare, &released);
    assert_eq!(edge_files(dir.path()).len(), DEPTH, "before the release");
    drop(std::mem::take(&mut *ready.lock().unwrap()));
    assert!(
        edge_files(dir.path()).is_empty(),
        "the exit's take removed them"
    );
    // The exit's flag lands while the fill mints its first.
    let mint = |d: &Path| {
        released.store(true, Ordering::Release);
        Provision::prepare(d)
    };
    fill_provisions(&ready, Some(dir.path()), mint, &released);
    assert!(
        ready.lock().unwrap().is_empty(),
        "nothing kept after the release"
    );
    assert!(edge_files(dir.path()).is_empty(), "and no file left");
    // A retirement already queued is the worker's; one after release must
    // complete inline, beyond the exit's final Flush rather than behind it.
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let retiring = AtomicBool::new(false);
    let file = || {
        Provision::prepare(dir.path())
            .unwrap()
            .into_parts()
            .unwrap()
            .2
            .unwrap()
    };
    let before = file();
    let before_path = before.path().to_path_buf();
    retire_using(before, |job| send_to(&tx, &retiring, job));
    assert!(before_path.exists(), "queued, still owned by the worker");
    assert_eq!(probe(&before_path), Owner::Live);
    let Job::Retire(before) = rx.try_recv().expect("the queued retirement") else {
        panic!("a retirement");
    };
    remove_edge_file(before);
    assert!(!before_path.exists(), "the real worker removal");

    retiring.store(true, Ordering::Release);
    let (done_tx, done_rx) = channel();
    assert!(send_to(&tx, &retiring, Job::Flush(done_tx)).is_ok());
    let after = file();
    let after_path = after.path().to_path_buf();
    retire_using(after, |job| send_to(&tx, &retiring, job));
    assert!(
        !after_path.exists(),
        "POST_RELEASE_RETIREMENT_MUST_FINISH_INLINE"
    );
    let Job::Flush(done) = rx.try_recv().expect("only the prefix flush waits") else {
        panic!("a late retirement was queued behind the flush");
    };
    done.send(()).unwrap();
    assert!(done_rx.try_recv().is_ok());
    assert!(
        rx.try_recv().is_err(),
        "no retirement remains after the flush"
    );
}
