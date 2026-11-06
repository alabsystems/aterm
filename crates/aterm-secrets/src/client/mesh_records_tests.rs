// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The records file's write hook ([`crate::membership_log::store::on_written`]) and the mesh's
//! classified ingest ([`ingest_mesh_records`]) on a real vault folder.

use super::*;
use crate::membership_log::store::classified_tests::chain;
use crate::membership_log::store::{MemberVerdict, with_written, without_written_call};
use crate::membership_log::{RecordId, encode_records};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Instant;

/// One hook call: the folder it named and the ids its store held complete.
type Call = (PathBuf, BTreeSet<RecordId>);

/// Run `run` with a hook that records each call on this thread.
fn hooked<T>(run: impl FnOnce() -> T) -> (T, Vec<Call>) {
    let calls: Rc<RefCell<Vec<Call>>> = Rc::default();
    let seen = calls.clone();
    let out = with_written(
        move |w| {
            seen.borrow_mut()
                .push((w.paths_dir.to_path_buf(), w.store.complete_inventory()));
        },
        run,
    );
    let calls = calls.borrow().clone();
    (out, calls)
}

#[test]
fn a_save_that_adds_records_calls_the_hook_once_with_them_and_one_that_adds_none_never() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(&root.path().join("mac"));
    let c = chain();

    let (added, calls) = hooked(|| {
        save_records(&paths, &encode_records([&c.genesis, &c.first_by_m1])).expect("save")
    });
    assert_eq!(added, 2);
    let held: BTreeSet<RecordId> = [*c.genesis.id(), *c.first_by_m1.id()].into();
    assert_eq!(calls, vec![(paths.dir.clone(), held)]);

    let (added, calls) = hooked(|| {
        save_records(&paths, &encode_records([&c.genesis, &c.first_by_m1])).expect("save again")
    });
    assert_eq!(added, 0);
    assert!(calls.is_empty(), "{calls:?}");

    // MUTANT: the writer's hook call removed. A save that adds a record reaches no hook.
    let (added, calls) = hooked(|| {
        without_written_call(|| save_records(&paths, c.admit.bytes()).expect("save admit"))
    });
    assert_eq!(added, 1);
    assert!(calls.is_empty(), "{calls:?}");
}

#[test]
fn a_mesh_ingest_writes_and_calls_the_hook_only_when_a_record_enters() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(&root.path().join("mac"));
    let c = chain();

    let (verdicts, calls) = hooked(|| {
        ingest_mesh_records(&paths, vec![c.genesis.clone(), c.first_by_m1.clone()]).expect("ingest")
    });
    assert_eq!(
        verdicts,
        vec![
            (*c.genesis.id(), MemberVerdict::Applied),
            (*c.first_by_m1.id(), MemberVerdict::Applied),
        ]
    );
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        load_records(&paths)
            .expect("load")
            .holds(c.first_by_m1.id())
    );

    // `by_m2` misses its parent and its signer's seat: it waits, nothing enters, no write.
    let (verdicts, calls) =
        hooked(|| ingest_mesh_records(&paths, vec![c.by_m2.clone()]).expect("ingest"));
    assert!(
        matches!(verdicts[..], [(id, MemberVerdict::Waiting(_))] if id == *c.by_m2.id()),
        "{verdicts:?}"
    );
    assert!(calls.is_empty(), "{calls:?}");

    // The seat lands, then the waiting record does.
    let (verdicts, calls) = hooked(|| {
        let seat = ingest_mesh_records(&paths, vec![c.admit.clone()]).expect("seat");
        let again = ingest_mesh_records(&paths, vec![c.by_m2.clone()]).expect("again");
        (seat, again)
    });
    assert_eq!(verdicts.0, vec![(*c.admit.id(), MemberVerdict::Applied)]);
    assert_eq!(verdicts.1, vec![(*c.by_m2.id(), MemberVerdict::Applied)]);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(load_records(&paths).expect("load").holds(c.by_m2.id()));
}

#[test]
fn a_record_another_process_saves_is_in_the_file_this_one_loads() {
    const CHILD_HOME: &str = "ATERM_MESH_RECORDS_TEST_HOME";
    const CHILD_RECORDS: &str = "ATERM_MESH_RECORDS_TEST_RECORDS";
    const SAVED: &str = "mesh-records-saved=";
    if let (Some(home), Some(file)) = (
        std::env::var_os(CHILD_HOME),
        std::env::var_os(CHILD_RECORDS),
    ) {
        let paths = Paths::for_home(Path::new(&home));
        let bytes = std::fs::read(&file).expect("records to save");
        let added = save_records(&paths, &bytes).expect("child save");
        println!("{SAVED}{added}");
        return;
    }

    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(&root.path().join("mac"));
    let c = chain();
    let file = root.path().join("offered");
    std::fs::write(&file, encode_records([&c.genesis, &c.first_by_m1])).expect("offered");
    let mut child = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    child
        .args([
            "--exact",
            "client::mesh_records_tests::a_record_another_process_saves_is_in_the_file_this_one_loads",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_HOME, &paths.home)
        .env(CHILD_RECORDS, &file);
    let output = aterm_uds::deadline::output_until_complete_stdout(
        &mut child,
        Instant::now() + aterm_spec::wait::MUST_HAPPEN,
    )
    .expect("child");
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).expect("utf-8");
    assert!(text.contains(&format!("{SAVED}2")), "{text}");

    let store = load_records(&paths).expect("load");
    assert!(store.holds(c.genesis.id()));
    assert!(store.holds(c.first_by_m1.id()));
}
