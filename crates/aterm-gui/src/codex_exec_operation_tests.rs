// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::super::tests::{
    Sleeper, ask_of, born, intent, process, receipt_of, selected, source, target,
};
use super::*;
use aterm_types::update_boundary::{Boundary, GuardedItem};

/// The update boundary the fixtures' custody holds, apart from this process's own.
static CUSTODY: Boundary = Boundary::new();

/// An environment whose home, state and configuration lie in `root`, so nothing a fixture's
/// operation could write reaches this Mac's own.
fn environment_in(root: &std::path::Path) -> Env {
    let mut env = Env::from_process().expect("this process's environment");
    env.home = Some(root.join("home"));
    env.state = root.join("state");
    env.config = Some(root.join("aterm.toml"));
    env
}

/// An exec operation of `source`'s process as `arm` makes one, of [`selected`]'s proposal and of
/// that process's own preparation ([`ask_of`]), holding native custody reserved in `tab`. A tab
/// holds one native custody at a time, so a fixture's second operation in custody takes its
/// custody from another tab: what Stop judges is whether an operation holds custody, not where it
/// was reserved.
fn armed(tab: &crate::SessionCtx, source: ExecSource, env: &Env) -> Operation {
    let target = target();
    let (proposal, armed) = arm_selection(
        &Mutex::new(selected(Instant::now())),
        &target.account,
        &target.user,
    )
    .expect("a selection to arm");
    let mut receipt = receipt_of(&proposal);
    receipt.id = format!("{:032x}", source.process.birth);
    let custody = tab
        .fenced_writes
        .reserve_native(
            1,
            &receipt.id,
            CUSTODY
                .try_hold(GuardedItem::HarnessNativeMutation)
                .expect("the fixtures' boundary"),
        )
        .expect("the tab's native custody");
    let binding = Binding {
        id: receipt.id.clone(),
        capability: "c".repeat(64),
    };
    let authority = tab.meta.lock().unwrap().supervisor_authority(0);
    let (gate, _settlement) = settlement::Execution::new(
        env.clone(),
        proposal.destination.label.clone(),
        settlement::Initial {
            receipt,
            authority,
            source_connection: 7,
            human_seq: 0,
            deadline: u64::MAX,
            tx: Transaction::new(1),
            custody: Arc::new(custody),
        },
    );
    Operation {
        binding,
        command: source.command_for(&source.auth_path).expect("its command"),
        preparation: Arc::new(ask_of(source.clone(), &intent().wall)),
        source,
        successor: None,
        checkpoint: "{}".into(),
        proposal,
        armed,
        tree: None,
        gate,
    }
}

/// `operation`, its native custody already given back, as one is once its monitor has settled it.
fn settled(operation: Operation) -> Operation {
    operation.gate.lock().custody.take();
    operation
}

/// Keep `operation` in `tab`'s table, in its process's place, as `arm` does.
fn keep(tab: &crate::SessionCtx, operation: Operation) -> Arc<Mutex<Operation>> {
    tab.fenced_writes
        .native
        .lock()
        .unwrap()
        .exec
        .keep(operation)
}

/// The operation `table` gives a verb of `process`, as `arm` and the id/cap verbs look it up.
fn found(table: &Table, process: &Process) -> Option<Arc<Mutex<Operation>>> {
    table.operation(ProcessKey::of(process)).cloned()
}

/// EACH PROCESS FINDS ONLY ITS OWN OPERATION: in a tab that keeps the exec operations of two
/// processes, the lookup every verb of a process makes (`arm`'s check of its custody, and each
/// id/cap verb's, by the asking process's key) finds that process's own operation and never the
/// other's, and the operation a later `arm` of one keeps takes the place of that process's
/// earlier one alone. A later process at one of their pids (another birth) finds none.
#[test]
fn each_process_finds_only_its_own_operation() {
    let root = aterm_tempfile::tempdir().expect("a root");
    let env = environment_in(root.path());
    let tab = crate::session_store::test_handle_on(918_313, -1);
    let other = Sleeper::start();
    let me = std::process::id();
    let mine = process(me, born(me));
    let theirs = other.process();
    let of =
        |process: &Process| settled(armed(&tab.ctx, source(process.clone(), root.path()), &env));
    let mut table = Table::default();
    let first = table.keep(of(&mine));
    let second = table.keep(of(&theirs));
    let is = |found: Option<Arc<Mutex<Operation>>>, kept: &Arc<Mutex<Operation>>| {
        found.is_some_and(|found| Arc::ptr_eq(&found, kept))
    };
    assert!(is(found(&table, &mine), &first), "the first finds its own");
    assert!(
        is(found(&table, &theirs), &second),
        "the second finds its own"
    );
    let again = table.keep(of(&mine));
    assert!(
        is(found(&table, &mine), &again),
        "a later arm takes its own process's place"
    );
    assert!(
        is(found(&table, &theirs), &second),
        "the other process's operation stands"
    );
    assert!(
        found(&table, &process(me, born(me) + 1)).is_none(),
        "a later process at this pid finds no operation"
    );
    assert_eq!(table.operations().len(), 2);
    assert_eq!(
        table.processes(),
        BTreeSet::from([ProcessKey::of(&mine), ProcessKey::of(&theirs)])
    );
}

/// STOP CLOSES EVERY OPERATION IN CUSTODY IN ITS TAB: a person's Stop (`cancel_owner`) in a tab
/// that keeps the exec operations of three processes closes each of the two that hold native
/// custody for good (`terminate`, its transaction cancelled) and keeps their custody until their
/// sources close, and it leaves the one that holds none as it was. CONTROL: in a tab whose only
/// operation holds no custody, Stop closes nothing and nothing is pending.
#[test]
fn stop_closes_every_operation_in_custody_in_its_tab() {
    let root = aterm_tempfile::tempdir().expect("a root");
    let env = environment_in(root.path());
    let tab = crate::session_store::test_handle_on(918_310, -1);
    let elsewhere = crate::session_store::test_handle_on(918_311, -1);
    let me = std::process::id();
    let birth = born(me);
    let of = |at: &crate::SessionCtx, birth: u64| {
        armed(at, source(process(me, birth), root.path()), &env)
    };
    let idle = keep(&tab.ctx, settled(of(&tab.ctx, birth + 2)));
    assert!(
        !cancel_owner(&tab.ctx),
        "CONTROL: nothing in custody to stop"
    );
    assert!(!pending(&tab.ctx), "CONTROL");
    let first = keep(&tab.ctx, of(&tab.ctx, birth));
    let second = keep(&tab.ctx, of(&elsewhere.ctx, birth + 1));
    assert_eq!(
        tab.ctx
            .fenced_writes
            .native
            .lock()
            .unwrap()
            .exec
            .operations()
            .len(),
        3
    );
    assert!(pending(&tab.ctx));
    assert!(
        cancel_owner(&tab.ctx),
        "Stop closes what it finds in custody"
    );
    for (n, operation) in [&first, &second].into_iter().enumerate() {
        let launch = operation.lock().unwrap();
        let state = launch.gate.lock();
        assert!(state.terminate && !state.active, "operation {n} is closed");
        assert_eq!(state.tx.stage(), Stage::Cancelled, "operation {n}");
        assert!(
            state.custody.is_some(),
            "operation {n} keeps its custody until its source closes"
        );
    }
    let launch = idle.lock().unwrap();
    let state = launch.gate.lock();
    assert!(
        !state.terminate && state.tx.stage() == Stage::Armed,
        "the operation in no custody is left as it was"
    );
    drop(state);
    drop(launch);
    assert!(
        pending(&tab.ctx),
        "custody stays pending until the sources close"
    );
}

/// A GONE PROCESS'S OPERATION IS FORGOTTEN ONCE IT HOLDS NOTHING: of a tab's exec operations,
/// `forget_gone` drops the settled one of a process the kernel shows gone (this test's pid at a
/// birth that is not its own: that pid's next process), and keeps a gone process's one still in
/// custody, for its monitor to settle and a Stop to reach, and the settled one of a process that
/// lives, which a later verb of that process may still find.
#[test]
fn a_gone_process_operation_is_forgotten_once_it_holds_nothing() {
    let root = aterm_tempfile::tempdir().expect("a root");
    let env = environment_in(root.path());
    let tab = crate::session_store::test_handle_on(918_312, -1);
    let me = std::process::id();
    let birth = born(me);
    let of = |birth: u64| armed(&tab.ctx, source(process(me, birth), root.path()), &env);
    let live = process(me, birth);
    let gone = process(me, birth + 1);
    let held = process(me, birth + 2);
    keep(&tab.ctx, settled(of(live.birth)));
    keep(&tab.ctx, settled(of(gone.birth)));
    keep(&tab.ctx, of(held.birth));
    forget_gone(&tab.ctx.fenced_writes.native);
    let table = &tab.ctx.fenced_writes.native.lock().unwrap().exec;
    assert!(
        found(table, &gone).is_none(),
        "a gone process's settled operation is forgotten"
    );
    assert!(
        found(table, &held).is_some(),
        "a gone process's operation in custody is kept"
    );
    assert!(
        found(table, &live).is_some(),
        "a live process's settled operation is kept"
    );
    assert_eq!(table.operations().len(), 2);
}
