// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::sync::Arc;
use std::time::Duration;

use aterm_session::SessionId;

use super::*;
use crate::session_log::observed::TitleStands;
use crate::session_store::{ExitActor, ExitReason, SessionHandle, Store};
use crate::task_record::{TaskMark, TaskState};

/// The longest a test waits for what must happen (a hang detector, never a latency budget).
const HANG: Duration = Duration::from_secs(60);

/// The synthetic redaction input, its prefix and suffix separate in the source.
fn redaction_fixture() -> String {
    ["sk-live-", "0123456789abcdef"].concat()
}

/// A scratch directory of the test's own for its table's rows.
fn scratch(prefix: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .expect("scratch dir")
}

/// A store whose table keeps its rows in `dir`.
fn store_at(dir: &std::path::Path) -> Store {
    let store = crate::session_store::new_store();
    SessionTable::at(dir.to_path_buf()).serve(&store);
    store
}

/// The table of `store`, its load waited for.
fn loaded(store: &Store) -> Arc<SessionTable> {
    let table = store.read().unwrap().table();
    assert!(table.loaded_within(HANG), "the load ends");
    table
}

/// Session `local` registered in `store`, its tab opened by `by` at wall ms `wall` (its open
/// mark's `by=`).
fn opened(
    store: &Store,
    local: u64,
    by: aterm_slog::Actor,
    wall: u64,
) -> crate::session_store::SessionHandle {
    let handle = crate::session_store::test_handle(local);
    {
        let mut tl = handle.ctx.timeline.lock().unwrap();
        let at = tl.slog().now();
        tl.slog_mut().open_tab(
            crate::session_log::TabPlace {
                opened_wall: wall,
                window: 0,
                parked: false,
                parent: None,
            },
            "spawn",
            &crate::session_log::TabAsked::default(),
            by,
            at,
        );
    }
    store.write().unwrap().register(handle.clone());
    handle
}

/// What a close reads of a terminal: the person's title `user`, the program's `live` and what it
/// stands for, and the folder `cwd`.
fn at_close(user: Option<&str>, live: &str, stands: TitleStands, cwd: Option<&str>) -> AtClose {
    AtClose {
        user_title: user.map(str::to_owned),
        live_title: live.to_owned(),
        stands,
        untitled: false,
        cwd: cwd.map(str::to_owned),
    }
}

/// A Claude Code launch on `conversation` in tab `sid`, as the harness publishes it.
fn claude_launch(sid: &str, conversation: &str) -> aterm_agent::harness::relaunch::Snapshot {
    aterm_agent::harness::relaunch::Snapshot {
        launch_env: None,
        auth_observed: true,
        route: aterm_agent::harness::accounts::AuthRoute::Outside,
        profile: None,
        tab: sid.to_owned(),
        pid: 4243,
        lifetime: None,
        start: "Thu Sep 24 01:02:03 2026".to_owned(),
        shell: 4242,
        program: std::path::PathBuf::from("/opt/claude"),
        argv: vec!["/opt/claude".to_owned()],
        session: Some(conversation.to_owned()),
        cwd: "/tmp".to_owned(),
        version: Some("2.1.318".to_owned()),
        run: aterm_agent::harness::relaunch::AgentRun::Claude,
        dialect: Some(aterm_agent::harness::upgrade::Dialect::Zsh),
        login: None,
        home: None,
        claude_dir: None,
    }
}

/// AN ENDED SESSION'S ROW SURVIVES A QUIT WITH WHO AND WHY (tracker ST-15): a session a person
/// opened, titled with a credential in it, is closed by its manager's `aterm ctl close`; the
/// process ends; a new one over the same state reads the end back from disk: `exits` names it
/// with its reason and closer, and its row says who opened it, when, and a title with the
/// credential redacted. CONTROL: before the close, no row and no exit.
#[test]
fn an_ended_sessions_row_survives_a_quit_with_who_and_why() {
    let dir = scratch("aterm-table-quit");
    let manager = "s-0123456789abcdef0123";
    let sid = {
        let store = store_at(dir.path());
        let handle = opened(&store, 7, aterm_slog::Actor::Human, 1_000);
        handle.ctx.meta.lock().unwrap().set(
            "title",
            Some(format!("deploy token={}", redaction_fixture())),
        );
        let table = loaded(&store);
        assert!(!table.holds(&handle.sid), "CONTROL: no row before the end");
        assert_eq!(
            crate::control::cmd_exits(&store, "", &[]),
            "OK 0\n",
            "CONTROL: no exit before the end"
        );
        close_leaving_row(
            &store,
            7,
            ExitReason::CtlClose,
            ExitActor::Sid(manager.to_owned()),
        )
        .expect("a close");
        assert!(table.flush(), "the row is on disk before the process ends");
        handle.sid.clone()
    };

    // The relaunch: a new store and table over the same folder.
    let store = store_at(dir.path());
    let table = loaded(&store);
    let exits = crate::control::cmd_exits(&store, "", &[]);
    assert!(exits.starts_with("OK 1\n"), "{exits}");
    assert!(
        exits.contains(&format!(" sid={} ", sid.as_str()))
            && exits.contains(" reason=ctl-close ")
            && exits.contains(&format!(" by={manager} ")),
        "{exits}"
    );
    let row = table.row(&sid).expect("its row, read from disk");
    assert_eq!(row.opened_by, Some(aterm_slog::Actor::Human));
    assert_eq!(row.opened_ms, Some(1_000));
    assert!(row.ended_ms >= 1_000, "{row:?}");
    assert_eq!((row.out, row.by.as_str()), (ExitReason::CtlClose, manager));
    assert!(
        row.title.contains("[redacted") && !row.title.contains("sk-live"),
        "{:?}",
        row.title
    );
    assert_eq!(row.title_from, TitleFrom::User);
}

/// THE ROW KEEPS THE LAUNCH OF AN AGENT THAT HAD EXITED (tracker ST-15): a Claude Code ran in the
/// session and exited before the session ended, so the harness's live publication of its launch
/// ended with it; the row read back from disk still holds the launch, and it resumes. CONTROL: the
/// live publication is gone before the close.
#[test]
fn the_row_keeps_the_launch_of_an_agent_that_had_exited() {
    let dir = scratch("aterm-table-launch");
    let store = store_at(dir.path());
    let handle = opened(&store, 3, aterm_slog::Actor::Human, 2_000);
    let sid = handle.sid.as_str().to_owned();
    let snap = claude_launch(&sid, "0b22c0de-1111-4222-8333-444455556666");
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
    crate::harness_host::publish_snapshot_for_test(&sid, None);
    assert!(
        crate::harness_host::published_snapshot(&sid).is_none(),
        "CONTROL: the live publication ended with the agent"
    );
    close_leaving_row(&store, 3, ExitReason::ShellExit, ExitActor::Unknown).expect("a close");
    let table = loaded(&store);
    assert!(table.flush());
    drop(store);

    let store = store_at(dir.path());
    let row = loaded(&store).row(&handle.sid).expect("its row");
    let launch = row.launch.expect("the launch the row kept");
    assert_eq!(launch, crate::restore::AgentRestore::of(snap));
    assert!(launch.resumes(), "its conversation resumes: {launch:?}");
}

/// AN ENDED SESSION IS NAMED FROM DISK AFTER A RESTART (tracker ST-15): three sessions end, one
/// titled by a person, one whose terminal title was its running command line, one titled only by
/// its folder; a new table over the same folder lists all three newest first, named by the
/// person's title and by their folders, never by the command line, and holds each; none is this
/// process's to name by a local id. CONTROL: the command line was the live title.
#[test]
fn an_ended_session_is_named_from_disk_after_a_restart() {
    let dir = scratch("aterm-table-named");
    let store = store_at(dir.path());
    let ends = [
        at_close(Some("my review"), "zsh", TitleStands::Own, Some("/srv/a")),
        at_close(
            None,
            "make deploy",
            TitleStands::CommandLine,
            Some("/srv/b"),
        ),
        at_close(None, "", TitleStands::Own, Some("/srv/c")),
    ];
    assert_eq!(ends[1].live_title, "make deploy", "CONTROL");
    let mut sids = Vec::new();
    for (local, at) in (11..).zip(ends) {
        let handle = opened(&store, local, aterm_slog::Actor::Ctl, 3_000);
        close_leaving_row_with(&store, local, ExitReason::CtlClose, ExitActor::Ctl, at)
            .expect("a close");
        sids.push(handle.sid.clone());
    }
    assert!(loaded(&store).flush());
    drop(store);

    let store = store_at(dir.path());
    let table = loaded(&store);
    let heads = table.heads_newest_first().expect("loaded");
    let named: Vec<(SessionId, String)> = heads
        .iter()
        .map(|h| (h.sid.clone(), table.row(&h.sid).expect("its row").title))
        .collect();
    assert_eq!(
        named,
        vec![
            (sids[2].clone(), "/srv/c".to_owned()),
            (sids[1].clone(), "/srv/b".to_owned()),
            (sids[0].clone(), "my review".to_owned()),
        ]
    );
    for (local, sid) in (11..).zip(&sids) {
        assert!(table.holds(sid));
        assert!(table.by_local(local).is_none(), "another process's row");
    }
}

/// A row of session `sid` that its close left with no task, title or launch.
fn bare_row(sid: &SessionId) -> Row {
    Row {
        sid: sid.clone(),
        id: 0,
        ended_ms: 5_000,
        opened_ms: None,
        opened_by: None,
        out: ExitReason::CtlClose,
        by: "ctl".to_owned(),
        by_node: None,
        why: None,
        exit_code: None,
        ep: None,
        inst: None,
        local: None,
        title: String::new(),
        title_from: TitleFrom::None,
        cwd: None,
        task: None,
        told_end: false,
        totals: TabTotals::default(),
        launch: None,
        trimmed: Vec::new(),
    }
}

/// Mark session `h`'s task `state`, started by `from`.
fn mark_task(h: &SessionHandle, state: TaskState, from: &str) {
    let mut mark = TaskMark::new(state);
    mark.from = Some(from.to_owned());
    crate::task_record::mark_session_task_within(&h.ctx, h.local_id, &mark, Duration::ZERO)
        .expect("unfenced");
}

/// The conclusion session `h`'s task record stands at.
fn conclusion_of(h: &SessionHandle) -> Told {
    let timeline = h.ctx.timeline.lock().unwrap();
    let task = timeline.task().expect("a task");
    Told {
        started_ms: task.started_unix_ms,
        since_ms: task.since_unix_ms,
    }
}

/// A table over `dir` under `budget` whose load waits until the sender returned beside it sends
/// or goes. The pattern `let (table, open) = gated(…)` drops the sender first, so a test that fails
/// unwinds rather than hangs: the table's drop joins its worker, which the held loader still feeds.
fn gated(
    dir: &std::path::Path,
    budget: Budget,
) -> (Arc<SessionTable>, std::sync::mpsc::Sender<()>) {
    let (open, gate) = std::sync::mpsc::channel();
    (
        SessionTable::at_with(dir.to_path_buf(), budget, Some(gate)),
        open,
    )
}

/// The row file of session `sid` in `dir`.
fn row_file(dir: &std::path::Path, sid: &SessionId) -> std::path::PathBuf {
    dir.join(format!("{}.toml", sid.as_str()))
}

/// ROWS OF BOTH ID FORMS READ BACK (tracker ST-15, LT-01): a session of the earlier `s-` form and
/// one of the `t` form each leave a row; a new table over the folder reads both, newest first,
/// and leaves a file that names no session, or that does not read, as it is. CONTROL: the two IDs
/// are of the two forms.
#[test]
fn rows_of_both_id_forms_read_back_and_a_stray_file_is_left() {
    let dir = scratch("aterm-table-forms");
    let earlier = SessionId::parse("s-0123456789abcdef0123").expect("a legacy tab's id");
    let minted = SessionId::generate();
    assert!(
        earlier.is_minted() && minted.is_minted() && minted.as_str().starts_with('t'),
        "CONTROL: {earlier} and {minted}"
    );
    {
        let table = SessionTable::at(dir.path().to_path_buf());
        table.write(bare_row(&earlier));
        table.write(bare_row(&minted));
        assert!(table.flush());
    }
    let stray = dir.path().join("notes.toml");
    let unread = row_file(dir.path(), &SessionId::generate());
    std::fs::write(&stray, "v = 1\n").expect("a stray file");
    std::fs::write(&unread, "not a row").expect("a row that does not read");

    let table = SessionTable::at(dir.path().to_path_buf());
    assert!(table.loaded_within(HANG));
    let sids: Vec<SessionId> = table
        .heads_newest_first()
        .expect("loaded")
        .into_iter()
        .map(|head| head.sid)
        .collect();
    assert_eq!(sids, [minted, earlier]);
    assert!(stray.is_file() && unread.is_file(), "left as they are");
}

/// A ROW REACHES DISK WHILE THE LOAD STILL READS, AND THE BUDGET WAITS FOR THE LOAD (tracker
/// ST-15): a close's row is written and flushed, as a quit flushes it, though the rows earlier runs
/// left are not read yet; only once they are does the budget judge the table, and then it drops
/// the oldest. CONTROL: the load is still held when the row lands.
#[test]
fn a_row_reaches_disk_while_the_load_still_reads_and_the_budget_waits_for_it() {
    let dir = scratch("aterm-table-gate");
    let old = SessionId::generate();
    {
        let table = SessionTable::at(dir.path().to_path_buf());
        table.write(bare_row(&old));
        assert!(table.flush());
    }
    let (table, open) = gated(
        dir.path(),
        Budget {
            bytes: u64::MAX,
            rows: 1,
        },
    );
    table.sessions_settled();
    let new = SessionId::generate();
    table.write(bare_row(&new));
    assert!(table.flush(), "the write landed");
    assert!(
        !table.loaded_within(Duration::ZERO),
        "CONTROL: the load is held"
    );
    assert!(row_file(dir.path(), &new).is_file(), "the row is on disk");
    assert!(
        row_file(dir.path(), &old).is_file(),
        "the budget judged no table it had not read whole"
    );

    open.send(()).expect("the loader waits");
    assert!(table.loaded_within(HANG));
    assert!(table.flush());
    assert!(
        !row_file(dir.path(), &old).exists() && !table.holds(&old),
        "the oldest row went"
    );
    assert!(table.holds(&new) && row_file(dir.path(), &new).is_file());
}

/// ROWS THE BUDGET DROPS TOGETHER TAKE ALL THEIR RESULTS (tracker ST-15, R4): two done workers'
/// rows an earlier run left, each naming its result, go in the one budget run that judges the next
/// run's table once its load ends, under a budget of one row, and both results and sidecars go with
/// them, though no live record names either. CONTROL: both stood after the earlier run.
#[test]
fn rows_the_budget_drops_together_take_all_their_results() {
    let dir = scratch("aterm-table-batch");
    let work = scratch("aterm-table-batch-work");
    let mut results = Vec::new();
    {
        let store = store_at(dir.path());
        let table = loaded(&store);
        for local in [1, 2] {
            let worker = opened(&store, local, aterm_slog::Actor::Human, 1_000);
            let (_, snap, _) = crate::task_launch::tests::done_worker(&worker, work.path());
            let sid = close_leaving_row(&store, local, ExitReason::CtlClose, ExitActor::Ctl)
                .expect("a close");
            crate::task_launch::retire_task_file(&store, &sid, &[]);
            let sidecar = aterm_agent::harness::result::sidecar_of(&snap.path);
            results.push((snap.path, sidecar));
        }
        assert!(table.flush());
    }
    assert!(
        results
            .iter()
            .all(|(path, sidecar)| path.exists() && sidecar.exists()),
        "CONTROL"
    );

    let store = crate::session_store::new_store();
    let (table, open) = gated(
        dir.path(),
        Budget {
            bytes: u64::MAX,
            rows: 1,
        },
    );
    table.serve(&store);
    table.sessions_settled();
    let new = SessionId::generate();
    table.write(bare_row(&new));
    assert!(table.flush());
    open.send(()).expect("the loader waits");
    assert!(table.loaded_within(HANG));
    assert!(table.flush());
    assert_eq!(
        table
            .heads_newest_first()
            .expect("loaded")
            .into_iter()
            .map(|head| head.sid)
            .collect::<Vec<_>>(),
        [new],
        "both earlier rows went"
    );
    for (path, sidecar) in &results {
        assert!(
            !path.exists() && !sidecar.exists(),
            "{} went with its row",
            path.display()
        );
    }
}

/// THE BUDGET DROPS THE OLDEST ROWS, THEIR RESULTS FIRST, BUT KEEPS WHAT LIVE SESSIONS NEED
/// (tracker ST-15, ST-D14): under a budget of three rows, a row a live worker names as its creator
/// and a concluded worker's row its live manager was not told of stay; the oldest of the rest go,
/// a done worker's result and sidecar with its row. Once its manager is told, the told worker's
/// row is no longer kept for it, and only a word of that very conclusion tells it. CONTROL: the
/// result was there while its row stood.
#[test]
fn the_budget_drops_the_oldest_rows_with_their_results_but_keeps_what_live_sessions_need() {
    let dir = scratch("aterm-table-budget");
    let work = scratch("aterm-table-budget-work");
    let store = crate::session_store::new_store();
    SessionTable::at_with(
        dir.path().to_path_buf(),
        Budget {
            bytes: u64::MAX,
            rows: 3,
        },
        None,
    )
    .serve(&store);
    let table = loaded(&store);
    table.sessions_settled();
    let human = aterm_slog::Actor::Human;
    let manager = opened(&store, 1, human, 1_000);
    // A close as a tab's: its row, then its sweep of the tasks directory, which removes the
    // session's task file and keeps the result its row names.
    let close = |local: u64| {
        let sid = close_leaving_row(&store, local, ExitReason::CtlClose, ExitActor::Ctl)
            .expect("a close");
        crate::task_launch::retire_task_file(&store, &sid, &[]);
    };

    // The creator of a live worker.
    let creator = opened(&store, 2, human, 1_000);
    close(2);
    let worker = opened(&store, 3, human, 1_000);
    mark_task(&worker, TaskState::Working, creator.sid.as_str());
    // A worker that concluded, its live manager not told.
    let untold = opened(&store, 4, human, 1_000);
    mark_task(&untold, TaskState::Done, manager.sid.as_str());
    let told = conclusion_of(&untold);
    close(4);
    // A done worker with a result.
    let done = opened(&store, 5, human, 1_000);
    let (_, snap, _) = crate::task_launch::tests::done_worker(&done, work.path());
    let sidecar = aterm_agent::harness::result::sidecar_of(&snap.path);
    close(5);
    assert!(table.flush());
    assert!(snap.path.exists() && sidecar.exists(), "CONTROL");
    // Two more ends: the done worker's row and the next go, oldest first.
    let plain = opened(&store, 6, human, 1_000);
    close(6);
    let newest = opened(&store, 7, human, 1_000);
    close(7);
    assert!(table.flush());
    for (sid, kept) in [
        (&creator.sid, true),
        (&untold.sid, true),
        (&done.sid, false),
        (&plain.sid, false),
        (&newest.sid, true),
    ] {
        assert_eq!(table.holds(sid), kept, "{sid}");
        assert_eq!(row_file(dir.path(), sid).is_file(), kept, "{sid}'s file");
    }
    assert!(
        !snap.path.exists() && !sidecar.exists(),
        "the result went with its row"
    );

    // A word of another conclusion tells nothing: the next end drops the newest unprotected row.
    let other = Told {
        since_ms: told.since_ms + 1,
        ..told
    };
    table.told_end(&untold.sid, other);
    let next = opened(&store, 8, human, 1_000);
    close(8);
    assert!(table.flush());
    assert!(table.holds(&untold.sid) && !table.holds(&newest.sid));
    // Its manager told, the untold worker's row is the oldest the budget may drop.
    table.told_end(&untold.sid, told);
    let last = opened(&store, 9, human, 1_000);
    close(9);
    assert!(table.flush());
    assert!(!table.holds(&untold.sid), "told, its row went");
    assert!(table.holds(&creator.sid) && table.holds(&next.sid) && table.holds(&last.sid));
}

/// A CONCLUSION TOLD BEFORE ITS ROW COMES IS TOLD ON IT, AND ON NO ROW OF ANOTHER (tracker ST-15):
/// a worker's manager hears its end while its close is under way; the row that close then leaves
/// says so, on disk too. CONTROL: a word of another conclusion leaves a row untold.
#[test]
fn a_conclusion_told_before_its_row_comes_is_told_on_that_row_alone() {
    let dir = scratch("aterm-table-told");
    let store = store_at(dir.path());
    let table = loaded(&store);
    let manager = opened(&store, 1, aterm_slog::Actor::Human, 1_000);
    let (first, second) = (
        opened(&store, 2, aterm_slog::Actor::Ctl, 1_000),
        opened(&store, 3, aterm_slog::Actor::Ctl, 1_000),
    );
    for worker in [&first, &second] {
        mark_task(worker, TaskState::Done, manager.sid.as_str());
    }
    let told = conclusion_of(&first);
    table.told_end(
        &first.sid,
        Told {
            started_ms: told.started_ms + 1,
            ..told
        },
    );
    table.told_end(&second.sid, conclusion_of(&second));
    for local in [2, 3] {
        close_leaving_row(&store, local, ExitReason::ShellExit, ExitActor::Unknown)
            .expect("a close");
    }
    assert!(table.flush());
    drop(store);

    let store = store_at(dir.path());
    let table = loaded(&store);
    assert!(
        !table.row(&first.sid).expect("its row").told_end,
        "CONTROL: another conclusion's word"
    );
    assert!(table.row(&second.sid).expect("its row").told_end);
}

/// A ROW PAST ITS BOUND GIVES UP, IN ORDER, ITS AGENT'S LONG ARGV WORDS, ITS FOLDER AND ITS LAUNCH,
/// AND IS NEVER LOST (tracker ST-15): a launch whose argv holds a first prompt past the bound keeps
/// its resume and its folder; one past it with no long word gives up its folder, then itself.
/// CONTROL: each untrimmed row is past the bound.
#[test]
fn a_row_past_its_bound_gives_up_argv_then_folder_then_launch_and_is_never_lost() {
    let sid = SessionId::generate();
    let launch = crate::restore::AgentRestore::of(claude_launch(
        sid.as_str(),
        "0b22c0de-1111-4222-8333-444455556666",
    ));
    let with = |argv: Vec<String>| {
        let mut row = bare_row(&sid);
        let mut launch = launch.clone();
        launch.argv = argv;
        row.launch = Some(launch);
        row.cwd = Some("/srv/work".to_owned());
        row
    };
    let prompted = with(vec!["claude".to_owned(), "p".repeat(ROW_MAX_BYTES)]);
    let wide = with(vec!["w".repeat(ARGV_WORD_MAX); 20]);
    for row in [&prompted, &wide] {
        assert!(row.file().encode().len() > ROW_MAX_BYTES, "CONTROL");
    }

    let text = prompted.encode();
    assert!(text.len() <= ROW_MAX_BYTES, "{}", text.len());
    let back = Row::decode(&text, None).expect("it reads");
    assert_eq!(back.trimmed, ["argv"]);
    let kept = back.launch.expect("the launch stays");
    assert_eq!(kept.argv, ["claude"]);
    assert!(kept.resumes(), "{kept:?}");
    assert_eq!(back.cwd.as_deref(), Some("/srv/work"));

    let text = wide.encode();
    assert!(text.len() <= ROW_MAX_BYTES, "{}", text.len());
    let back = Row::decode(&text, None).expect("it reads");
    assert_eq!(back.trimmed, ["cwd", "launch"]);
    assert!(back.launch.is_none() && back.cwd.is_none());
    assert_eq!(back.sid, sid);
}

/// A ROW'S TITLE FOLLOWS THE TAB'S LADDER AND NEVER KEEPS A COMMAND LINE (tracker ST-15, ST-02):
/// a program's own title stands as its program's, redacted; a tab nothing it runs titled stands by
/// its task's first line, a command line a shell's integration wrote included, else by its folder.
/// CONTROL: the command line was the live title.
#[test]
fn a_rows_title_follows_the_tabs_ladder_and_never_keeps_a_command_line() {
    let at = |live: &str, stands, untitled| AtClose {
        user_title: None,
        live_title: live.to_owned(),
        stands,
        untitled,
        cwd: Some("/srv/work".to_owned()),
    };
    let line = Some("fix the parser");
    assert_eq!(
        standing_title(&at("htop", TitleStands::Own, false), line),
        ("htop".to_owned(), TitleFrom::Program)
    );
    let (title, from) = standing_title(
        &at("psql postgres://dev:hunter22@db", TitleStands::Own, false),
        line,
    );
    assert!(
        from == TitleFrom::Program && !title.contains("hunter22"),
        "{title:?}"
    );
    let command = at("make deploy", TitleStands::CommandLine, true);
    assert_eq!(command.live_title, "make deploy", "CONTROL");
    assert_eq!(
        standing_title(&command, line),
        ("fix the parser".to_owned(), TitleFrom::Task)
    );
    assert_eq!(
        standing_title(&command, None),
        ("/srv/work".to_owned(), TitleFrom::Folder)
    );
}

/// AN EXITED AGENT'S LAUNCH CROSSES A SEAMLESS UPDATE TO THE ROW (tracker ST-15): a Claude Code
/// exits in a session before the update; the park carries its launch, the predecessor's record of
/// it goes with its process, and the successor keeps it, so the row the session leaves when it
/// ends there still holds it. CONTROL: the predecessor held the launch, and the successor holds
/// none of its own.
#[test]
fn an_exited_agents_launch_crosses_a_seamless_update_to_its_row() {
    let dir = scratch("aterm-table-carry");
    let store = store_at(dir.path());
    let handle = opened(&store, 21, aterm_slog::Actor::Human, 4_000);
    let sid = handle.sid.as_str().to_owned();
    let snap = claude_launch(&sid, "0b22c0de-1111-4222-8333-444455556666");
    crate::harness_host::publish_snapshot_for_test(&sid, Some(&snap));
    crate::harness_host::publish_snapshot_for_test(&sid, None);

    let manifest =
        crate::session_store::SessionHandoff::from_store(&store.read().unwrap()).to_toml();
    let read = crate::session_store::SessionHandoff::from_toml(&manifest.expect("it writes"))
        .expect("it reads");
    assert!(
        crate::harness_host::take_last_launch(&sid).is_some(),
        "CONTROL: the predecessor held it"
    );
    assert!(
        crate::harness_host::take_last_launch(&sid).is_none(),
        "CONTROL"
    );
    assert_eq!(
        crate::harness_host::adopt_last_launches(&read.last_launches),
        1
    );

    close_leaving_row(&store, 21, ExitReason::ShellExit, ExitActor::Unknown).expect("a close");
    assert!(loaded(&store).flush());
    let store = store_at(dir.path());
    let launch = loaded(&store)
        .row(&handle.sid)
        .expect("its row")
        .launch
        .expect("the launch it kept");
    assert_eq!(launch, crate::restore::AgentRestore::of(snap));
}

/// A RESULT ANOTHER RUN'S ROW NAMES IS KEPT WHILE ITS FILE STANDS (tracker ST-15): two runs share
/// the folder (a seamless update's predecessor and successor overlap); a row the other writes
/// after this one loaded keeps its result, and the sidecar beside it, from this run's sweeps until
/// the row's file goes; a table that has not loaded keeps every result. CONTROL: the result of a
/// session with no row is not kept.
#[test]
fn a_result_another_runs_row_names_is_kept_while_its_file_stands() {
    let dir = scratch("aterm-table-other");
    let work = scratch("aterm-table-other-work");
    let tasks = crate::task_record::tasks_dir().expect("the test binary's state root");
    let ours = SessionTable::at(dir.path().to_path_buf());
    assert!(ours.loaded_within(HANG));

    let theirs = store_at(dir.path());
    let worker = opened(&theirs, 1, aterm_slog::Actor::Ctl, 1_000);
    let (_, snap, _) = crate::task_launch::tests::done_worker(&worker, work.path());
    close_leaving_row(&theirs, 1, ExitReason::CtlClose, ExitActor::Ctl).expect("a close");
    assert!(loaded(&theirs).flush());

    let name = |path: &std::path::Path| path.file_name().unwrap().to_str().unwrap().to_owned();
    let sidecar = aterm_agent::harness::result::sidecar_of(&snap.path);
    let keeps = ours.result_keeps(&tasks);
    assert!(keeps.holds(&name(&snap.path)) && keeps.holds(&name(&sidecar)));
    let rowless = format!("{}-3.result.md", SessionId::generate().as_str());
    assert!(!keeps.holds(&rowless), "CONTROL");
    std::fs::remove_file(row_file(dir.path(), &worker.sid)).expect("its row's file");
    assert!(
        !keeps.holds(&name(&snap.path)),
        "its row gone, it is not kept"
    );

    let (loading, open) = gated(dir.path(), Budget::MACHINE);
    assert!(loading.result_keeps(&tasks).holds(&rowless));
    drop(open);
    assert!(loading.loaded_within(HANG));
}

/// THE BUDGET WAITS FOR THE LAUNCH'S SESSIONS BEFORE IT JUDGES WHAT THEY PROTECT (tracker ST-15):
/// an earlier run left two rows, the older a manager's whose worker the restore brings back live.
/// The next launch's load ends before that worker is registered; the table, one row over its
/// budget, drops nothing until the launch's sessions are settled, and then keeps the manager's row,
/// which the live worker names as its creator, and drops the other. CONTROL: the manager's row is
/// the older, the first the budget would drop unprotected.
#[test]
fn the_budget_waits_for_the_launchs_sessions_before_it_judges_what_they_protect() {
    let dir = scratch("aterm-table-settled");
    let (manager, other) = (SessionId::generate(), SessionId::generate());
    {
        let table = SessionTable::at(dir.path().to_path_buf());
        table.write(bare_row(&manager));
        table.write(bare_row(&other));
        assert!(table.flush());
        assert!(table.loaded_within(HANG));
        let ids: Vec<SessionId> = table
            .exits_since(0)
            .expect("loaded")
            .into_iter()
            .map(|head| head.sid)
            .collect();
        assert_eq!(
            ids,
            [manager.clone(), other.clone()],
            "CONTROL: oldest first"
        );
    }
    let store = crate::session_store::new_store();
    let table = SessionTable::at_with(
        dir.path().to_path_buf(),
        Budget {
            bytes: u64::MAX,
            rows: 1,
        },
        None,
    );
    table.serve(&store);
    assert!(table.loaded_within(HANG));
    assert!(table.flush(), "whatever the load ran has run");
    assert!(
        table.holds(&manager) && table.holds(&other),
        "nothing is judged before the launch's sessions are registered"
    );

    // The restore registers the worker, whose task names the manager as its creator.
    let worker = opened(&store, 1, aterm_slog::Actor::Ctl, 1_000);
    mark_task(&worker, TaskState::Working, manager.as_str());
    table.sessions_settled();
    assert!(table.flush());
    assert!(
        table.holds(&manager) && row_file(dir.path(), &manager).is_file(),
        "the live worker's creator stays"
    );
    assert!(
        !table.holds(&other) && !row_file(dir.path(), &other).exists(),
        "the budget dropped the other"
    );
}

/// A ROW ANOTHER PROCESS WRITES OR REMOVES AFTER THIS ONE LOADED IS SEEN HERE (tracker ST-15): two
/// runs share the folder (a seamless update's predecessor and successor, a second instance). A row
/// the other writes after this run's load is held and read here, and `exits` lists another; once
/// the other removes them, neither is held or listed here. CONTROL: before the other's writes,
/// this run holds and lists neither.
#[test]
fn a_row_another_process_writes_or_removes_after_the_load_is_seen() {
    let dir = scratch("aterm-table-shared");
    let ours = SessionTable::at(dir.path().to_path_buf());
    assert!(ours.loaded_within(HANG));
    let listed = |table: &SessionTable| -> Vec<SessionId> {
        table
            .exits_since(0)
            .expect("loaded")
            .into_iter()
            .map(|head| head.sid)
            .collect()
    };
    let (looked_up, paged) = (SessionId::generate(), SessionId::generate());
    assert!(
        !ours.holds(&looked_up) && listed(&ours).is_empty(),
        "CONTROL"
    );

    let theirs = SessionTable::at(dir.path().to_path_buf());
    assert!(theirs.loaded_within(HANG));
    theirs.write(bare_row(&looked_up));
    theirs.write(bare_row(&paged));
    assert!(theirs.flush());
    assert!(
        ours.holds(&looked_up),
        "a lookup the index missed reads the file"
    );
    assert_eq!(
        ours.row(&looked_up).map(|row| row.sid),
        Some(looked_up.clone())
    );
    assert!(
        listed(&ours).contains(&paged),
        "exits lists the folder again"
    );

    assert!(theirs.remove(&looked_up) && theirs.flush());
    assert!(!ours.holds(&looked_up), "its file went, so its head did");
    assert!(theirs.remove(&paged) && theirs.flush());
    assert!(listed(&ours).is_empty(), "exits lists neither");
}

/// A RESULT TWO ROWS NAME STAYS UNTIL BOTH ROWS GO, WHATEVER ITS NAME (tracker ST-15): a manager and
/// its worker each mark one file of the tasks directory as their result, a name no done edge
/// writes (`task mark result_path=` takes any file there); both end; a person removes the worker's
/// row and the file stays, since the manager's row names it; once that row goes too, so does the
/// file. CONTROL: the name is no snapshot's.
#[test]
fn a_result_two_rows_name_stays_until_both_rows_go_whatever_its_name() {
    let dir = scratch("aterm-table-shared-result");
    let tasks = crate::task_record::tasks_dir().expect("the test binary's state root");
    crate::control_auth::ensure_private_dir(&tasks).expect("the tasks directory");
    let name = format!("report-{}.md", SessionId::generate().as_str());
    assert!(crate::task_launch::result_owner(&name).is_none(), "CONTROL");
    let report = tasks.join(&name);
    std::fs::write(&report, "done.\n").expect("the report");

    let store = store_at(dir.path());
    let table = loaded(&store);
    let (manager, worker) = (
        opened(&store, 1, aterm_slog::Actor::Human, 1_000),
        opened(&store, 2, aterm_slog::Actor::Ctl, 1_000),
    );
    for handle in [&manager, &worker] {
        let mut mark = TaskMark::new(TaskState::Done);
        mark.result_path = Some(report.clone());
        mark.result_bytes = 6;
        crate::task_record::mark_session_task_within(
            &handle.ctx,
            handle.local_id,
            &mark,
            Duration::ZERO,
        )
        .expect("unfenced");
    }
    for local in [1, 2] {
        close_leaving_row(&store, local, ExitReason::CtlClose, ExitActor::Ctl).expect("a close");
    }
    assert!(table.flush());

    assert!(table.remove(&worker.sid) && table.flush());
    assert!(report.is_file(), "the manager's row still names it");
    assert!(table.remove(&manager.sid) && table.flush());
    assert!(!report.exists(), "no row names it now, and it went");
}

/// A ROW WHOSE WRITE FAILED IS WRITTEN AGAIN AT THE FLUSH, AND A FLUSH THAT CANNOT LAND IT SAYS SO
/// (tracker ST-15): the table's folder cannot be made (a file stands where its parent should be),
/// so a close's row is kept in memory and the flush answers false; once the folder can be made, the
/// next flush writes it. CONTROL: no row file while the folder could not be made.
#[test]
fn a_row_whose_write_failed_is_written_again_at_the_flush() {
    let root = scratch("aterm-table-retry");
    let blocker = root.path().join("state");
    std::fs::write(&blocker, "not a folder").expect("a file in the folder's way");
    let dir = blocker.join("sessions");
    let table = SessionTable::at(dir.clone());
    assert!(table.loaded_within(HANG));
    let sid = SessionId::generate();
    table.write(bare_row(&sid));
    assert!(
        !table.flush(),
        "the row did not land, and the flush says so"
    );
    assert!(
        table.holds(&sid) && !row_file(&dir, &sid).exists(),
        "CONTROL"
    );

    std::fs::remove_file(&blocker).expect("the way cleared");
    assert!(table.flush(), "the row is written again");
    assert!(row_file(&dir, &sid).is_file());
}

/// A NEW ROW TAKES AN ID ABOVE EVERY ROW THE TABLE HOLDS, THE CLOCK STEPPING BACK INCLUDED (tracker
/// ST-15): an earlier run left a row whose id is far past this clock (its clock ran ahead, or this
/// one stepped back since); a row written while the load still reads and one written after it
/// both take ids above it, so `exits since=<that id>` pages to both, here and in a later run.
/// CONTROL: the earlier row's id is past the wall clock's.
#[test]
fn a_new_row_takes_an_id_above_every_row_the_table_holds() {
    let dir = scratch("aterm-table-ids");
    let far = 4_000_000_000_000_000_u64;
    let now = aterm_time::since_epoch().map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(0));
    assert!(far > now, "CONTROL");
    let ahead = SessionId::generate();
    let mut row = bare_row(&ahead);
    row.id = far;
    std::fs::write(row_file(dir.path(), &ahead), row.encode()).expect("the earlier run's row");

    let paged = |table: &SessionTable| -> Vec<SessionId> {
        table
            .exits_since(far)
            .expect("loaded")
            .into_iter()
            .map(|head| head.sid)
            .collect()
    };
    let (early, late) = (SessionId::generate(), SessionId::generate());
    {
        let (table, open) = gated(dir.path(), Budget::MACHINE);
        table.write(bare_row(&early));
        open.send(()).expect("the loader waits");
        assert!(table.loaded_within(HANG));
        table.write(bare_row(&late));
        assert!(table.flush());
        assert_eq!(paged(&table), [early.clone(), late.clone()]);
    }
    let table = SessionTable::at(dir.path().to_path_buf());
    assert!(table.loaded_within(HANG));
    assert_eq!(paged(&table), [early, late], "on disk too");
}

/// A ROW KEEPS ITS TASK'S FIRST LINE REDACTED, AS ITS TITLE IS (tracker ST-15, R1): a worker whose
/// task's first line holds a credential ends; neither its title nor its task's line keeps the
/// credential, in memory or in its file. CONTROL: the record's line held it.
#[test]
fn a_rows_task_line_is_redacted_as_its_title_is() {
    let store = crate::session_store::new_store();
    let handle = opened(&store, 1, aterm_slog::Actor::Ctl, 1_000);
    mark_task(&handle, TaskState::Working, "ctl");
    let mut task = handle
        .ctx
        .timeline
        .lock()
        .unwrap()
        .task()
        .cloned()
        .expect("a task");
    task.line = Some(format!("deploy with token={}", redaction_fixture()));
    assert!(
        task.line
            .as_deref()
            .is_some_and(|line| line.contains("sk-live")),
        "CONTROL"
    );
    let ended = Ended {
        local: 1,
        out: ExitReason::CtlClose,
        by: ExitActor::Ctl,
        why: None,
        exit_code: None,
        ended_ms: 2_000,
        started_ms: None,
        tab: None,
        ep: 0,
        task: Some(task),
    };
    let at = AtClose {
        untitled: true,
        ..at_close(None, "make deploy", TitleStands::CommandLine, None)
    };
    let mut launch = claude_launch(handle.sid.as_str(), "0b22c0de-1111-4222-8333-444455556666");
    launch.argv.extend([
        "--".to_owned(),
        format!("deploy with token={}", redaction_fixture()),
    ]);
    crate::harness_host::publish_snapshot_for_test(handle.sid.as_str(), Some(&launch));
    let row = row_at_close(&handle.sid, &ended, &at);
    assert!(
        row.launch.is_some(),
        "the prompt-bearing launch was captured"
    );
    assert_eq!(row.title_from, TitleFrom::Task);
    let line = row
        .task
        .as_ref()
        .and_then(|task| task.line.as_deref())
        .expect("its line");
    assert!(
        !line.contains("sk-live") && line.contains("[redacted"),
        "{line}"
    );
    assert!(!row.title.contains("sk-live"), "{}", row.title);
    assert!(!row.encode().contains("sk-live"), "nor its file");
}

/// A TEMPORARY FILE AN ENDED WRITER LEFT IS SWEPT AT THE LOAD, BUT NOT ONE A LIVE WRITER HOLDS
/// (tracker ST-15): a write cut short leaves `<sid>.toml.tmp-<pid>`; the next table over the folder
/// removes such a file of a process that has ended, and leaves one of this process. CONTROL: the
/// ended writer's pid names no live process.
#[test]
fn a_temporary_file_an_ended_writer_left_is_swept_at_the_load() {
    let dir = scratch("aterm-table-tmp");
    let sid = SessionId::generate();
    let dead = u32::try_from(i32::MAX).expect("fits");
    assert!(!aterm_uds::process::pid_alive(dead), "CONTROL");
    let stale = dir.path().join(format!("{}.toml.tmp-{dead}", sid.as_str()));
    let live = dir
        .path()
        .join(format!("{}.toml.tmp-{}", sid.as_str(), std::process::id()));
    for path in [&stale, &live] {
        std::fs::write(path, "v = 1\n").expect("a temporary file");
    }
    let table = SessionTable::at(dir.path().to_path_buf());
    assert!(table.loaded_within(HANG));
    assert!(!stale.exists(), "the ended writer's file went");
    assert!(live.is_file(), "a live writer's stays");
    assert!(!table.holds(&sid), "neither is a row");
}

/// WHAT WAITS FOR THE LOAD RUNS ONCE IT ENDS, AND NOT BEFORE (tracker ST-15): a one-shot sweep the
/// launch hands the table runs once its load has ended, when it no longer holds every result; on a
/// loaded table it runs at once. CONTROL: before the load, a result no row names is held.
#[test]
fn what_waits_for_the_load_runs_once_it_ends() {
    let dir = scratch("aterm-table-after-load");
    let tasks = crate::task_record::tasks_dir().expect("the test binary's state root");
    let rowless = format!("{}-3.result.md", SessionId::generate().as_str());
    let (table, open) = gated(dir.path(), Budget::MACHINE);
    assert!(table.result_keeps(&tasks).holds(&rowless), "CONTROL");
    let (ran, seen) = std::sync::mpsc::channel();
    let keeps = table.result_keeps(&tasks);
    let name = rowless.clone();
    table.when_loaded(move || {
        let _ = ran.send(keeps.holds(&name));
    });
    assert!(seen.try_recv().is_err(), "not before the load");
    open.send(()).expect("the loader waits");
    assert_eq!(
        seen.recv_timeout(HANG),
        Ok(false),
        "it ran once the load ended, when such a result is held no longer"
    );
    let (ran, seen) = std::sync::mpsc::channel();
    let caller = std::thread::current().id();
    table.when_loaded(move || {
        let _ = ran.send(std::thread::current().id());
    });
    assert_ne!(
        seen.recv_timeout(HANG).expect("the callback runs"),
        caller,
        "a loaded table also sweeps off the window thread"
    );
}

/// A coarse timestamp's recent listing must be repeated once the tick has passed.
#[test]
fn session_table_rechecks_a_recent_listing_after_the_tick() {
    let dir = scratch("aterm-table-tick");
    let table = SessionTable::at(dir.path().to_path_buf());
    assert!(table.loaded_within(HANG));
    let first = SessionId::generate();
    std::fs::write(row_file(dir.path(), &first), bare_row(&first).encode()).unwrap();
    let tick = std::fs::metadata(dir.path()).unwrap().modified().unwrap();
    table.shared.refresh_at(tick);
    assert!(table.shared.lock().heads.contains_key(first.as_str()));
    let second = SessionId::generate();
    std::fs::write(row_file(dir.path(), &second), bare_row(&second).encode()).unwrap();
    std::fs::remove_file(row_file(dir.path(), &first)).unwrap();
    let mut directory = std::fs::OpenOptions::new();
    directory.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // SetFileTime needs FILE_WRITE_ATTRIBUTES; a directory needs BACKUP_SEMANTICS.
        directory.access_mode(0x0000_0100).custom_flags(0x0200_0000);
    }
    directory
        .open(dir.path())
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(tick))
        .unwrap();
    table
        .shared
        .refresh_at(tick + MTIME_TICK + Duration::from_secs(1));
    let state = table.shared.lock();
    assert!(
        state.heads.contains_key(second.as_str()),
        "the second write in the tick is seen"
    );
    assert!(
        !state.heads.contains_key(first.as_str()),
        "the removal in the tick is seen"
    );
}

/// An amendment's stale file copy cannot undo the load's renumbering.
#[test]
fn session_table_amend_keeps_the_current_id() {
    let table = SessionTable::default();
    let sid = SessionId::generate();
    table.write(bare_row(&sid));
    let stale = table.row(&sid).unwrap();
    let current = stale.id + 100;
    {
        let mut state = table.shared.lock();
        let mut row = stale.clone();
        row.id = current;
        state.index(row.head(0));
        state.rows.insert(sid.as_str().to_owned(), row);
    }
    assert!(table.shared.amend(stale, |row| {
        row.told_end = true;
        true
    }));
    assert_eq!(table.row(&sid).unwrap().id, current);
    assert_eq!(table.head(&sid).unwrap().id, current);
}

/// The first park settles a launch with no layout, so subsequent writes run the budget.
#[test]
fn session_table_a_launch_without_restore_runs_the_budget() {
    let dir = scratch("aterm-table-no-restore");
    let mut app = crate::App::headless_for_test();
    let table = SessionTable::at_with(
        dir.path().to_path_buf(),
        Budget {
            bytes: u64::MAX,
            rows: 1,
        },
        None,
    );
    table.serve(&app.store);
    assert!(table.loaded_within(HANG));
    assert!(!app.restore_pass_queued());
    assert!(!app.restore_pass_due_at_park(false, std::time::Instant::now()));
    let first = SessionId::generate();
    let second = SessionId::generate();
    table.write(bare_row(&first));
    table.write(bare_row(&second));
    assert!(table.flush());
    assert!(!table.holds(&first));
    assert!(table.holds(&second));
}

/// All pre-load ends retain their order when a past-clock row raises their floor.
#[test]
fn session_table_merge_renumbers_in_end_order() {
    let table = SessionTable::default();
    for _ in 0..32 {
        table.write(bare_row(&SessionId::generate()));
    }
    // Deliberately give the hash iteration the reverse end order. The map's slots are unchanged.
    let order: Vec<String> = table.shared.lock().heads.keys().cloned().collect();
    {
        let mut state = table.shared.lock();
        for (i, sid) in order.iter().enumerate() {
            state.heads.get_mut(sid).unwrap().id = 100 - i as u64;
            state.rows.get_mut(sid).unwrap().id = 100 - i as u64;
        }
        state.order = state
            .heads
            .values()
            .map(|h| (h.id, h.sid.as_str().to_owned()))
            .collect();
    }
    let mut ahead = bare_row(&SessionId::generate());
    ahead.id = 4_000_000_000_000_000;
    table.shared.merge(vec![ahead.head(0)]);
    let mut expected = order;
    expected.reverse();
    let actual: Vec<String> = table
        .exits_since(ahead.id)
        .unwrap()
        .into_iter()
        .map(|h| h.sid.as_str().to_owned())
        .collect();
    assert_eq!(actual, expected);
}

/// Stat runs outside the index mutex, and a write while it runs invalidates the removal.
#[test]
fn session_table_forget_stats_outside_the_lock_and_rechecks_the_generation() {
    for amend in [false, true] {
        let table = SessionTable::default();
        let sid = SessionId::generate();
        table.write(bare_row(&sid));
        let before = table.row(&sid).unwrap();
        table.shared.lock().rows.clear();
        let mut checked = false;
        table.shared.forget_if(sid.as_str(), || {
            checked = true;
            assert!(
                table.shared.state.try_lock().is_ok(),
                "stat has no index guard"
            );
            if amend {
                assert!(table.shared.amend(before.clone(), |row| {
                    row.told_end = true;
                    true
                }));
                assert_eq!(
                    table.head(&sid).unwrap().id,
                    before.id,
                    "an edit keeps its id"
                );
            } else {
                table.write(bare_row(&sid));
            }
            // The worker's write finished before the stat's caller takes the lock again.
            table.shared.lock().rows.clear();
            true
        });
        assert!(checked);
        assert!(
            table.head(&sid).is_some(),
            "a newer write cannot be forgotten (amend={amend})"
        );
    }
}

/// Cold exec retries a failed queued row before it replaces the process.
#[cfg(unix)]
#[test]
fn session_table_cold_exec_flushes_pending_rows() {
    let root = scratch("aterm-table-cold");
    let blocker = root.path().join("state");
    std::fs::write(&blocker, "blocked").unwrap();
    let dir = blocker.join("sessions");
    let table = SessionTable::at(dir.clone());
    assert!(table.loaded_within(HANG));
    let sid = SessionId::generate();
    table.write(bare_row(&sid));
    assert!(!table.flush(), "the first write cannot land");
    std::fs::remove_file(&blocker).unwrap();
    let mut app = crate::App::headless_for_test();
    table.serve(&app.store);
    app.prepare_cold_exec(7, true, false);
    assert!(
        row_file(&dir, &sid).is_file(),
        "cold exec's barrier retries the row"
    );
}
