// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Cadence, FILES_AGAIN, Files};
use crate::{
    alab::{Registry, registry::MACHINE},
    file::materialize::{self, Pass},
    table::tests::Lab,
    typed::{grid::tests::put, keys},
};
use aterm_file::{File, Word};
use aterm_json::json;
use aterm_secrets::commons::roster::Role;
use std::time::{Duration, Instant};

/// The Commons owner's materializer passes shortly after a table store of its process grows,
/// and again each minute: the schedule's clock is the test's, so nothing here waits on time.
/// Other tests of this binary append too, which can only make a pass due sooner than this
/// test's own write does, never before [`Cadence::settle`] has passed.
/// NEGATIVE CONTROL: with `table::store::appended` not calling `file::grew`, the look after the
/// write sees nothing: `` assertion failed: files.moved_at.is_some_and(|at| at <= seen) ``.
#[test]
fn the_owner_materializes_after_a_store_grows_and_each_minute() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = lab._home.path().join("aterm.toml");
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let machines = File::at(&crate::file::root(paths), &crate::file::MACHINES.file());
    let cadence = Cadence::LIVE;
    let start = Instant::now();
    assert!(cadence.settle + Duration::from_secs(1) < cadence.first);
    let mut files = Files::new(cadence, start);
    assert_eq!(
        files.due(start, crate::file::growth()),
        None,
        "nothing is due as the owner starts"
    );
    let pass = |again: bool| {
        materialize::pass(
            paths,
            &config,
            None,
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait: materialize::VAULT_WAIT,
                again,
                now_ms: 1_800_000_000_000,
            },
        )
    };

    let machine = keys::machine_pk("m7");
    registry
        .write(vec![vec![
            put(&MACHINE, &machine, "label", json!("m7")),
            put(&MACHINE, &machine, "status", json!("active")),
        ]])
        .unwrap();
    // The first look after the store grew sees it; the pass waits for the burst to settle.
    let seen = start + Duration::from_millis(500);
    assert_eq!(files.due(seen, crate::file::growth()), None);
    assert!(files.moved_at.is_some_and(|at| at <= seen));
    let again = files
        .due(seen + cadence.settle, crate::file::growth())
        .expect("a pass is due once the growth has settled");
    assert!(again, "the owner's first pass projects whatever is here");
    pass(again);
    let read = machines.read();
    assert_eq!(read.status.word, Word::Live);
    assert_eq!(read.named("m7").len(), 1);
    let passes = machines.state().unwrap().seq;

    // With no growth seen since, the next pass is the regular one, and it projects nothing
    // again.
    let after = seen + cadence.settle;
    assert_eq!(files.regular, after + cadence.every);
    assert_eq!(files.due(after + cadence.every / 2, files.seen), None);
    let again = files
        .due(after + cadence.every, files.seen)
        .expect("the regular pass is due");
    assert!(!again);
    pass(again);
    assert_eq!(machines.state().unwrap().seq, passes + 1);
    assert_eq!(
        machines.read().status.rev(),
        1,
        "nothing arrived: one revision"
    );

    // A pass that met a busy vault is followed sooner than the regular one, and every
    // [`FILES_AGAIN`]th pass projects again.
    let at = after + cadence.every;
    files.met_busy(at);
    assert_eq!(files.regular, at + cadence.busy);
    let mut now = at;
    let mut projected_again = 0;
    for _ in 0..FILES_AGAIN {
        now += cadence.every;
        projected_again += u32::from(files.due(now, files.seen).expect("a regular pass"));
    }
    assert_eq!(projected_again, 1);
}

/// The owner's pass publishes what this Mac queued before it materializes: one pass turns a
/// queued intent into one record and leaves a snapshot that holds it, and the next pass signs
/// nothing more.
#[test]
fn the_owners_pass_publishes_a_queued_write_and_then_materializes_it() {
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = lab._home.path().join("aterm.toml");
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let machine = keys::machine_pk("m7");
    registry
        .write(vec![vec![
            put(&MACHINE, &machine, "label", json!("m7")),
            put(&MACHINE, &machine, "status", json!("active")),
        ]])
        .unwrap();
    let hang = || Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    assert!(!super::files_pass(paths, &config, true, hang()));
    let machines = File::at(&crate::file::root(paths), &crate::file::MACHINES.file());
    let draft = crate::file::write::draft_set(
        paths,
        crate::file::MACHINES.name,
        "m7",
        "note",
        Some("said at the desk"),
    )
    .unwrap()
    .unwrap();
    let intent = machines
        .write(
            &aterm_file::OwnerAct::verb("person", None).unwrap(),
            draft,
            1,
        )
        .unwrap();
    let records = || {
        crate::att::Handle::at(paths.clone(), &crate::att::REGISTRY)
            .history()
            .unwrap()
            .len()
    };
    let before = records();
    assert!(!super::files_pass(paths, &config, false, hang()));
    assert_eq!(records(), before + 1, "the pass signed the intent");
    let outbox = machines.outbox();
    assert!(outbox.queued.is_empty());
    assert_eq!(outbox.done[0].intent.id, intent.id);
    assert!(outbox.done[0].seen, "the same pass materialized its record");
    let published = machines.published();
    assert_eq!(published.status.word, Word::Live);
    assert_eq!(published.named("m7")[0].cell("note"), "said at the desk");
    assert!(!super::files_pass(paths, &config, false, hang()));
    assert_eq!(records(), before + 1, "a later pass signs nothing more");
}
