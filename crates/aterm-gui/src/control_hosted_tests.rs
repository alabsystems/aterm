// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JOIN, the instance's half: a run is placed in the session whose program is an ancestor
//! of the posting process, a legacy `s-` session as a minted one; no word of the request moves
//! it; and a program worker's run is credited to the session that asked, which is the session
//! aterm placed the spawn's request in and never one a word of the spawn names. Each test gives the
//! posting connection's process and its chain of parents, as the description author's tests do
//! (`control_desc_author_tests.rs`). The landing's half is `aterm-verify`'s
//! `land_records_join_tests.rs`.

use super::*;
use crate::session_store::{SessionHandle, new_store};
use crate::task_record::TaskMark;
use aterm_session::{LaunchNonce, SessionId};

/// A session opened before the minted form, as `aterm ctl ls` still lists one.
const LEGACY: &str = "s-f56dadda2d012b94f138";
/// A session of the minted form.
const MINTED: &str = "tqn5r6e0148";
/// Another session of the same aterm, which hosts none of the runs here.
const BYSTANDER: &str = "t3xcj80edmk";

/// The pid of the program (the shell) of the session each placement test places runs in.
const PROGRAM: i32 = 6000;
/// The pid of [`BYSTANDER`]'s program.
const BYSTANDER_PROGRAM: i32 = 7000;

/// A session registered in `store` under the ID `sid`, whose program is process `program`.
fn session(store: &Store, local: u64, sid: &str, program: i32) -> SessionHandle {
    let epochs = Arc::new(crate::session_log::Epochs::new());
    epochs.note(crate::session_log::FIRST_EPOCH);
    let mut handle = crate::session_store::test_handle_identified(
        local,
        SessionId::parse(sid).expect("a session id"),
        LaunchNonce::generate(),
        epochs,
        aterm_slog::Stamp::new(1_792_000_000_000, 0),
    );
    handle.shell_identity.pid = program;
    store.write().unwrap().register(handle.clone());
    handle
}

/// The posting process `chain[0]`, each next entry the parent of the one before it and the last
/// one adopted by launchd (pid 1); every process of the chain reads its process group as
/// `group`. `aterm`'s own pid is 1 here, so no process of a chain is aterm.
fn process(chain: &[i32], group: Option<i32>) -> Who {
    let parent_of = |pid: i32| {
        let at = chain.iter().position(|of| *of == pid)?;
        Some(chain.get(at + 1).copied().unwrap_or(1))
    };
    Who::client_with(
        u32::try_from(chain[0]).ok(),
        1,
        &parent_of,
        &|_| group,
        &|_| Some("targo".to_owned()),
    )
}

/// The reply to `rest` posted by `who`.
fn post(store: &Store, rest: &str, who: &Who) -> String {
    cmd_hosted(store, rest, || who.clone())
}

/// What a session under the ID `sid` and its bystander answer to each post of the placement
/// tests, in order: the same store, processes and requests whatever form `sid` has.
struct Placement {
    /// A landing's start, four processes below the session's program (the program, an agent,
    /// its tool shell, `targo`, the landing).
    start: String,
    /// The same landing's end.
    end: String,
    /// Another pass twenty processes below the program: within the chain aterm reads.
    deep: String,
    /// The same landing started in the bystander's tree.
    elsewhere: String,
    /// A landing whose parents exited (launchd adopted it): in no session's tree.
    adopted: String,
    /// One that left the tree and kept the process group of the session's program.
    group_only: String,
    /// One whose chain of parents is longer than aterm reads, with no program in what it read.
    cut: String,
    /// A post from a caller with no peer process.
    no_process: String,
}

fn placement(sid: &str) -> Placement {
    let store = new_store();
    session(&store, 1, sid, PROGRAM);
    session(&store, 2, BYSTANDER, BYSTANDER_PROGRAM);
    let landing = process(&[6400, 6300, 6200, 6100, PROGRAM], None);
    let deep: Vec<i32> = (0..20).map(|up| 6500 - up).chain([PROGRAM]).collect();
    let endless: Vec<i32> = (0..64).map(|up| 9900 - up).chain([PROGRAM]).collect();
    Placement {
        start: post(&store, "start kind=land", &landing),
        end: post(&store, "end kind=land exit=0", &landing),
        deep: post(&store, "start kind=verify", &process(&deep, None)),
        elsewhere: post(
            &store,
            "start kind=land",
            &process(&[7400, 7300, BYSTANDER_PROGRAM], None),
        ),
        adopted: post(&store, "start kind=land", &process(&[6400, 6300], None)),
        group_only: post(&store, "start kind=land", &process(&[6400], Some(PROGRAM))),
        cut: post(&store, "start kind=land", &process(&endless, None)),
        no_process: post(&store, "start kind=land", &Who::client(None)),
    }
}

fn hosted_by(sid: &str) -> String {
    format!("OK hosted={sid} worker=-\n")
}

const NOWHERE: &str = "OK hosted=- worker=-\n";

/// A landing started in a session opened before the minted ID form is placed in that session by
/// the lineage of the process that posts it, and the answer is the legacy `s-` ID `aterm ctl
/// @self` addresses there: its start and its end, and a pass twenty processes below the
/// session's program. The request cannot name a session: a post carrying any word that would
/// (`session=`, `sid=`, `hosted=`, `worker=`, an `@<sid>` in its words) is a usage error
/// answered before the process is even read, and the same landing posted without that word is
/// still the legacy session's.
///
/// NEGATIVE CONTROLS: the same landing in the bystander's tree is the bystander's; one whose
/// parents exited, one that kept only the program's process group, one whose chain is longer
/// than aterm reads and a caller with no peer process are hosted by no session.
#[test]
fn join_a_landing_started_in_a_legacy_s_tab_is_placed_by_lineage() {
    assert!(aterm_types::tab_id::is_legacy(LEGACY));
    let placed = placement(LEGACY);
    assert_eq!(placed.start, hosted_by(LEGACY));
    assert_eq!(placed.end, hosted_by(LEGACY));
    assert_eq!(placed.deep, hosted_by(LEGACY));
    assert_eq!(placed.elsewhere, hosted_by(BYSTANDER));
    for (case, reply) in [
        ("adopted by launchd", &placed.adopted),
        ("only the process group", &placed.group_only),
        ("a chain longer than aterm reads", &placed.cut),
        ("no peer process", &placed.no_process),
    ] {
        assert_eq!(reply, NOWHERE, "{case}");
    }

    let store = new_store();
    session(&store, 1, LEGACY, PROGRAM);
    session(&store, 2, BYSTANDER, BYSTANDER_PROGRAM);
    let landing = process(&[6400, 6300, PROGRAM], None);
    for claim in [
        format!("start kind=land session={BYSTANDER}"),
        format!("start kind=land sid={BYSTANDER}"),
        format!("start kind=land hosted={BYSTANDER}"),
        format!("start kind=land worker={BYSTANDER}"),
        format!("start kind=land @{BYSTANDER}"),
        format!("start {BYSTANDER} kind=land"),
        format!("@{BYSTANDER} start kind=land"),
        format!("start kind={BYSTANDER}@x"),
    ] {
        let read = std::cell::Cell::new(false);
        let reply = cmd_hosted(&store, &claim, || {
            read.set(true);
            landing.clone()
        });
        assert_eq!(reply, USAGE, "{claim}");
        assert!(!read.get(), "{claim}: a refused post places nothing");
    }
    assert_eq!(post(&store, "start kind=land", &landing), hosted_by(LEGACY));
    // The verb is the instance owner's alone, takes no session to aim at, and no other Mac's
    // drive can send it: the only caller is a local process aterm reads.
    let row = aterm_types::control_verbs::spec("hosted").expect("the verb's row");
    assert_eq!(row.access, aterm_types::control_verbs::Access::OwnerOnly);
    assert!(!row.fleet);
    assert_eq!(
        super::super::owner_only_refusal(
            "hosted",
            Some(&aterm_types::control_request::Selector::parse(BYSTANDER)),
            super::super::Scope::Owner
        )
        .as_deref(),
        Some("ERR denied: hosted takes no @<sid>\n")
    );
}

/// A session with a minted ID is placed the same way: the same store, processes and requests
/// under a minted ID answer what the legacy session's did, each with the minted ID in the
/// legacy one's place, so nothing in the placement reads the ID's form.
///
/// NEGATIVE CONTROL: the two differ in the ID alone, and the cases no session hosts are the
/// same words under both.
#[test]
fn join_a_minted_id_session_is_placed_the_same_way() {
    assert!(aterm_types::tab_id::is_tab_id(MINTED));
    let (minted, legacy) = (placement(MINTED), placement(LEGACY));
    assert_eq!(minted.start, hosted_by(MINTED));
    assert_eq!(minted.end, hosted_by(MINTED));
    assert_eq!(minted.deep, hosted_by(MINTED));
    let pairs = [
        (&minted.start, &legacy.start),
        (&minted.end, &legacy.end),
        (&minted.deep, &legacy.deep),
        (&minted.elsewhere, &legacy.elsewhere),
        (&minted.adopted, &legacy.adopted),
        (&minted.group_only, &legacy.group_only),
        (&minted.cut, &legacy.cut),
        (&minted.no_process, &legacy.no_process),
    ];
    for (minted, legacy) in pairs {
        assert_eq!(*minted, legacy.replace(LEGACY, MINTED));
    }
    assert_ne!(minted.start, legacy.start);
    assert_eq!(minted.elsewhere, hosted_by(BYSTANDER));
    for (case, reply) in [
        ("adopted by launchd", &minted.adopted),
        ("only the process group", &minted.group_only),
        ("a chain longer than aterm reads", &minted.cut),
        ("no peer process", &minted.no_process),
    ] {
        assert_eq!(reply, NOWHERE, "{case}");
    }
}

/// The session `sid` as an open mark's `by=` names it.
fn by_session(sid: &str) -> aterm_slog::Actor {
    crate::session_log::exit_actor(&crate::session_store::ExitActor::Sid(sid.to_owned()))
}

/// The worker session of a `verify land --tab` landing, its program process 8000: a program
/// worker's task record, `working`, whose spawn named `from` as the session that started it,
/// in a session `opened_by` opened (its open mark's `by=`: who aterm placed the spawn's request
/// in).
fn worker(store: &Store, from: &str, opened_by: aterm_slog::Actor) -> SessionHandle {
    let handle = session(store, 9, MINTED, 8000);
    let mut task = TaskMark::new(TaskState::Working);
    task.from = Some(from.to_owned());
    let mut timeline = handle.ctx.timeline.lock().unwrap();
    let at = timeline.slog().now();
    timeline.slog_mut().open_tab(
        crate::session_log::TabPlace {
            opened_wall: at.wall_ms,
            window: 0,
            parked: true,
            parent: None,
        },
        "task",
        &crate::session_log::TabAsked::default(),
        opened_by,
        at,
    );
    let _ = timeline.mark_task(&task, &Default::default(), std::time::Instant::now(), 1);
    timeline.set_task_worker(aterm_phase::Program::Generic, None);
    timeline.set_task_program(Some("targo"));
    drop(timeline);
    handle
}

/// The landing a worker's program runs: `targo`, then the landing, under the worker's program.
fn worker_landing() -> Who {
    process(&[8200, 8100, 8000], None)
}

/// A `verify land --tab` landing runs as a program worker in a session of its own. Lineage
/// places it in the worker's session, and the run is credited to the session that asked for
/// it: the session the worker's task record names, which is the session aterm placed the
/// spawn's request in when it opened the worker. `hosted` is the asking (legacy) session and
/// `worker` the worker's, at the landing's start and at its end. A pass the asking session runs
/// itself is its own, with no worker.
///
/// NEGATIVE CONTROLS, each hosted by the worker's own session with no worker named: the same
/// landing once the worker's task concluded (a run started in that session afterwards is its
/// own); in an agent's worker, whose own work is what lands; in a program worker a `ctl` client
/// started, or a session of another Mac did, or whose record names itself: no session here
/// asked; in a session with no task record at all; and in a worker whose record names a session
/// that did not open it: a spawn whose `from=` names another session than the one its process
/// runs in (a forged `ATERM_PARENT_SESSION_ID`), one a process in no session sent naming a
/// session, a session the person opened, and a `task mark from=` that renames the asker later.
#[test]
fn join_a_land_tab_worker_is_credited_to_the_session_that_asked() {
    let credited = format!("OK hosted={LEGACY} worker={MINTED}\n");
    let store = new_store();
    session(&store, 1, LEGACY, PROGRAM);
    let tab = worker(&store, LEGACY, by_session(LEGACY));
    assert_eq!(post(&store, "start kind=land", &worker_landing()), credited);
    assert_eq!(
        post(&store, "end kind=land exit=0", &worker_landing()),
        credited
    );
    assert_eq!(
        post(
            &store,
            "start kind=verify",
            &process(&[6400, 6300, PROGRAM], None)
        ),
        hosted_by(LEGACY)
    );
    // A deadline that read the task while its program ran leaves the credit where it was.
    let mark = |handle: &SessionHandle, to: &TaskMark| {
        let mut timeline = handle.ctx.timeline.lock().unwrap();
        let _ = timeline.mark_task(to, &Default::default(), std::time::Instant::now(), 2);
    };
    mark(&tab, &TaskMark::new(TaskState::Stalled));
    assert_eq!(post(&store, "start kind=land", &worker_landing()), credited);
    // A mark that renames the asker moves nothing to the session it names: that session did
    // not open the worker.
    let mut renamed = TaskMark::new(TaskState::Working);
    renamed.from = Some(BYSTANDER.to_owned());
    mark(&tab, &renamed);
    assert_eq!(
        post(&store, "start kind=land", &worker_landing()),
        hosted_by(MINTED),
        "a task mark's from= credits nobody"
    );
    let mut restored = TaskMark::new(TaskState::Working);
    restored.from = Some(LEGACY.to_owned());
    mark(&tab, &restored);
    assert_eq!(post(&store, "start kind=land", &worker_landing()), credited);
    mark(&tab, &TaskMark::new(TaskState::Done));
    assert_eq!(
        post(&store, "start kind=land", &worker_landing()),
        hosted_by(MINTED),
        "a concluded task credits nobody else"
    );

    let own = hosted_by(MINTED);
    let asked = by_session(LEGACY);
    let edits: [(&str, &str, aterm_slog::Actor, &dyn Fn(&SessionHandle)); 9] = [
        ("an agent's worker", LEGACY, asked, &|handle| {
            let mut timeline = handle.ctx.timeline.lock().unwrap();
            timeline.set_task_worker(aterm_phase::Program::Claude, None);
            timeline.set_task_program(None);
        }),
        (
            "started by a ctl client",
            crate::task_record::FROM_CTL,
            aterm_slog::Actor::Ctl,
            &|_| {},
        ),
        ("started from another Mac", LEGACY, asked, &|handle| {
            let mut timeline = handle.ctx.timeline.lock().unwrap();
            timeline.set_task_manager_node(Some("0123456789abcdef".to_owned()));
        }),
        ("naming itself", MINTED, by_session(MINTED), &|_| {}),
        (
            "a spawn naming a session its process does not run in",
            LEGACY,
            by_session(BYSTANDER),
            &|_| {},
        ),
        (
            "a spawn naming a session, sent by a process in none",
            LEGACY,
            aterm_slog::Actor::Ctl,
            &|_| {},
        ),
        (
            "a session the person opened",
            LEGACY,
            aterm_slog::Actor::Human,
            &|_| {},
        ),
        (
            "a spawn nobody could be placed for",
            LEGACY,
            aterm_slog::Actor::Nobody,
            &|_| {},
        ),
        ("a program worker, as the control", LEGACY, asked, &|_| {}),
    ];
    for (case, from, opened_by, edit) in edits {
        let store = new_store();
        session(&store, 1, LEGACY, PROGRAM);
        session(&store, 2, BYSTANDER, BYSTANDER_PROGRAM);
        let tab = worker(&store, from, opened_by);
        edit(&tab);
        let want = if case.ends_with("the control") {
            &credited
        } else {
            &own
        };
        assert_eq!(
            &post(&store, "start kind=land", &worker_landing()),
            want,
            "{case}"
        );
    }
    let store = new_store();
    session(&store, 1, LEGACY, PROGRAM);
    session(&store, 9, MINTED, 8000);
    assert_eq!(post(&store, "start kind=land", &worker_landing()), own);
}

/// A post is `start` or `end`, one `kind` word, and an exit code on an end alone; any other
/// shape is refused whole. NEGATIVE CONTROL: the shapes verify sends parse.
#[test]
fn a_post_is_a_phase_a_kind_and_an_ends_exit() {
    let parsed = |rest| Post::parse(rest).map(|post| (post.phase, post.kind, post.exit));
    assert_eq!(
        parsed("start kind=land"),
        Some((Phase::Start, "land", None))
    );
    assert_eq!(
        parsed("end kind=verify exit=3"),
        Some((Phase::End, "verify", Some(3)))
    );
    assert_eq!(
        parsed("  end   exit=-9  kind=gate-2 "),
        Some((Phase::End, "gate-2", Some(-9)))
    );
    assert_eq!(parsed("end kind=land"), Some((Phase::End, "land", None)));
    for rest in [
        "",
        "kind=land",
        "begin kind=land",
        "start",
        "start kind=",
        "start kind=Land",
        "start kind=2",
        "start kind=land kind=verify",
        "start kind=land exit=0",
        "end kind=land exit=",
        "end kind=land exit=+1",
        "end kind=land exit=01",
        "end kind=land exit=1 exit=1",
        "end kind=land exit=99999999999",
        "start kind=land run=abc",
        "start land",
        &format!("start kind={}", "k".repeat(33)),
    ] {
        assert_eq!(parsed(rest), None, "{rest:?}");
    }
}
