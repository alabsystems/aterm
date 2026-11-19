// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who a description's author is: the session aterm observed the control connection's process
//! to live in, ranked by the described session's own log, through the `mark` and `meta`
//! dispatch with the connection's process given by the test.

use aterm_slog::{Actor, Node, Rank};

use super::control_session::caller_actor_as;
use super::{MetaRequest, Scope, dispatch_mark_as, serve_meta_as};
use crate::input::InputEvent;
use crate::session_store::{ExitActor, SessionHandle, Store, new_store, test_handle};
use crate::task_record::{TaskMark, TaskState};
use crate::typed_by::Who;

/// A session registered in `store` whose shell is process `shell`.
fn tab(store: &Store, local: u64, shell: i32) -> SessionHandle {
    let mut h = test_handle(local);
    h.shell_identity.pid = shell;
    store.write().unwrap().register(h.clone());
    h
}

/// An owner-token client's process `pid`, whose parent is `parent` and whose process group
/// nothing reads: under a tab's shell where `parent` is that shell, in no tab where `parent`
/// is 1 (a process whose parents exited, which launchd adopted).
fn client(pid: i32, parent: i32) -> impl Fn() -> Who {
    grouped(pid, parent, None)
}

/// [`client`], the process's group read as `group`: a child a double fork detached keeps the
/// group of the tab's shell that started it.
fn grouped(pid: i32, parent: i32, group: Option<i32>) -> impl Fn() -> Who {
    move || {
        Who::client_with(
            u32::try_from(pid).ok(),
            1,
            &|p| (p == pid).then_some(parent),
            &|p| (p == pid).then_some(group).flatten(),
            &|_| Some("aterm".to_string()),
        )
    }
}

/// `mark <rest>` on `h` through the dispatch, from the owner-token client `from`.
fn mark(h: &SessionHandle, store: &Store, from: impl FnOnce() -> Who, rest: &str) -> String {
    dispatch_mark_as(&h.ctx, Scope::Owner, rest, store, from)
}

/// `meta <rest>` on `h` through the dispatch, from the owner-token client `from`.
fn meta(h: &SessionHandle, store: &Store, from: impl FnOnce() -> Who, rest: &str) -> String {
    serve_meta_as(
        MetaRequest {
            rest,
            term: &h.term,
            prove: &|_| None,
            prove_claim: &|_, _| None,
            keep_child: &|_, _| None,
            session: h.local_id,
            ctx: &h.ctx,
            store,
            scope: Scope::Owner,
        },
        crate::act_barrier::before_act,
        from,
        |_| {},
    )
}

/// The session's own `title` claims as `described` lists them: the text, its author's word,
/// the author's rank and whether it stands.
fn titles(ctx: &crate::SessionCtx) -> Vec<(String, String, Rank, bool)> {
    let g = ctx.timeline.lock().unwrap();
    let now = g.slog().now().wall_ms;
    g.slog()
        .log()
        .state()
        .desc
        .listing(now)
        .iter()
        .filter(|r| r.scope == Node::SESSION && r.slot == "title")
        .map(|r| {
            (
                r.claim.value.clone(),
                r.claim.by.word().to_string(),
                r.rank,
                r.standing,
            )
        })
        .collect()
}

/// One `title` claim as [`titles`] lists it.
fn claim(text: &str, by: &str, rank: Rank, stands: bool) -> (String, String, Rank, bool) {
    (text.to_string(), by.to_string(), rank, stands)
}

fn pin(ctx: &crate::SessionCtx) -> Option<String> {
    ctx.meta.lock().unwrap().user_title.clone()
}

/// The reply to a `mark` a reader made, without its seq: `OK node=<n> by=<actor>`.
fn marked(reply: &str) -> String {
    let (head, tail) = reply.split_once(" node=").unwrap_or((reply, ""));
    assert!(head.starts_with("OK seq="), "{reply}");
    format!("OK node={}", tail.trim_end())
}

/// The session's `desc` and `meta` records of its title at the host tier, each from its kind
/// on (the seq, the `mark -` and the time left off).
fn title_records(ctx: &crate::SessionCtx) -> Vec<String> {
    let g = ctx.timeline.lock().unwrap();
    g.slog()
        .log()
        .window()
        .map(|r| aterm_slog::encode(r, aterm_slog::Tier::Host).expect("a logged record fits"))
        .filter_map(|l| {
            let from = l
                .find(" desc field=title")
                .or_else(|| l.find(" meta field=title"))?;
            let to = l.rfind(" t=")?;
            Some(l[from + 1..to].to_string())
        })
        .collect()
}

/// The headless app and its first tab, whose in-grid strip presents the rename field.
fn app_tab() -> (crate::App, crate::WindowId, SessionHandle) {
    let mut app = crate::App::headless_for_test();
    app.tab_strip_rows = 1;
    let tab0 = app
        .store
        .read()
        .unwrap()
        .by_local(0)
        .cloned()
        .expect("tab 0");
    (app, crate::WindowId(0), tab0)
}

/// An owner-token client `pid` whose chain of parents is `depth` processes long (`pid` the
/// first) and whose last process's parent is `top`; a tab's shell (6999, 7999) is launchd's
/// child, and any other parent read fails.
fn nested(pid: i32, depth: i32, top: i32) -> impl Fn() -> Who {
    move || {
        Who::client_with(
            u32::try_from(pid).ok(),
            1,
            &|p| {
                if (pid..pid + depth - 1).contains(&p) {
                    Some(p + 1)
                } else if p == pid + depth - 1 {
                    Some(top)
                } else {
                    matches!(p, 6999 | 7999).then_some(1)
                }
            },
            &|_| None,
            &|_| Some("sh".to_string()),
        )
    }
}

/// A process in session A that describes session B, through `mark … desc` and through `meta
/// set title`, is recorded by A's sid, listed, and never stands: B's title, which a control
/// client in no tab set, is what B shows before and after, and A's `meta unset title` takes
/// back A's own claim and nothing of B's.
///
/// CONTROL: the same process describing its own session A stands there as A's program, by
/// both verbs, so it is where the process lives that ranks it and not what it wrote. And it is
/// a description's author alone that is read this way: the same process's `mark - note` and
/// `meta set role` are recorded by the connection's own word, `ctl`.
#[test]
fn a_process_in_one_session_describing_another_is_listed_and_never_stands() {
    let store = new_store();
    let a = tab(&store, 1, 6999);
    let b = tab(&store, 2, 7999);
    let (a_sid, in_a, outside) = (a.sid.as_str(), client(7001, 6999), client(9001, 1));
    assert_eq!(meta(&b, &store, &outside, "set title B's work"), "OK\n");
    let theirs = claim("B's work", "ctl", Rank::Ctl, true);
    assert_eq!(titles(&b.ctx), [theirs.clone()]);

    let reply = mark(&b, &store, &in_a, "- desc field=title value=taken%20over");
    assert_eq!(marked(&reply), format!("OK node=- by={a_sid}"));
    assert_eq!(
        titles(&b.ctx),
        [
            theirs.clone(),
            claim("taken over", a_sid, Rank::Listed, false)
        ],
        "A's claim on B is listed under the title that stands"
    );
    assert_eq!(
        meta(&b, &store, &in_a, "set title and again"),
        "OK\n",
        "a listed title write answers OK"
    );
    assert_eq!(
        titles(&b.ctx),
        [
            theirs.clone(),
            claim("and again", a_sid, Rank::Listed, false)
        ],
        "one author, one claim: A's second replaces A's first and still does not stand"
    );
    assert_eq!(pin(&b.ctx).as_deref(), Some("B's work"));
    assert_eq!(meta(&b, &store, &in_a, "unset title"), "OK\n");
    assert_eq!(titles(&b.ctx), [theirs], "A withdrew its own claim alone");
    assert_eq!(pin(&b.ctx).as_deref(), Some("B's work"));

    let reply = mark(&a, &store, &in_a, "- desc field=title value=my%20own");
    assert_eq!(marked(&reply), format!("OK node=- by={a_sid}"));
    assert_eq!(
        titles(&a.ctx),
        [claim("my own", a_sid, Rank::Program, true)],
        "CONTROL: in its own session the same process is the program, and stands"
    );
    assert_eq!(meta(&a, &store, &in_a, "set title mine by meta"), "OK\n");
    assert_eq!(pin(&a.ctx).as_deref(), Some("mine by meta"));
    assert_eq!(
        titles(&a.ctx),
        [claim("mine by meta", a_sid, Rank::Program, true)]
    );

    let reply = mark(&a, &store, &in_a, "- note text=building");
    assert_eq!(marked(&reply), "OK node=- by=ctl", "any other mark kind");
    assert_eq!(meta(&a, &store, &in_a, "set role lead"), "OK\n");
    let g = a.ctx.timeline.lock().unwrap();
    let role_by = g
        .slog()
        .log()
        .window()
        .rev()
        .find(|r| r.k == aterm_slog::Kind::Meta)
        .map(|r| r.by);
    assert_eq!(role_by, Some(Actor::Ctl), "any other metadata field");
}

/// A writer that left the session's process tree (both its parents exited, so launchd adopted
/// it and no tab's shell is above it) is recorded `by=ctl`, and ranks below the session's own
/// program: its title is listed under the program's, which still stands, by `mark` and by
/// `meta set title`. Leaving the tree lowered its rank; it did not raise it. A writer that kept
/// the process group of the tab's shell is `by=ctl` too: only the chain of parents is read.
///
/// CONTROL: in a session whose program claimed nothing the same detached writer's title
/// stands, as a control client's from no session does; and the writer that kept the shell's
/// group is in that tab for the exits ledger, which counts the group.
#[test]
fn a_double_forked_writer_ranks_below_the_sessions_own_claim() {
    let store = new_store();
    let s = tab(&store, 3, 6999);
    let (sid, program, detached) = (s.sid.as_str(), client(7001, 6999), client(9001, 1));
    let reply = mark(&s, &store, &program, "- desc field=title value=building");
    assert_eq!(marked(&reply), format!("OK node=- by={sid}"));
    let own = claim("building", sid, Rank::Program, true);

    let reply = mark(&s, &store, &detached, "- desc field=title value=hijacked");
    assert_eq!(marked(&reply), "OK node=- by=ctl");
    assert_eq!(
        titles(&s.ctx),
        [own.clone(), claim("hijacked", "ctl", Rank::Ctl, false)],
        "the detached writer's claim is listed under the program's"
    );
    assert_eq!(
        meta(&s, &store, &detached, "set title hijacked again"),
        "OK\n"
    );
    assert_eq!(
        titles(&s.ctx),
        [
            own.clone(),
            claim("hijacked again", "ctl", Rank::Ctl, false)
        ]
    );
    assert_eq!(
        pin(&s.ctx).as_deref(),
        Some("building"),
        "the title write pinned what stands: the program's"
    );

    let in_group = grouped(9002, 1, Some(6999));
    let reply = mark(
        &s,
        &store,
        &in_group,
        "- desc field=title value=kept%20the%20group",
    );
    assert_eq!(
        marked(&reply),
        "OK node=- by=ctl",
        "a detached child in the shell's process group is not the session's program"
    );
    assert_eq!(
        titles(&s.ctx),
        [own, claim("kept the group", "ctl", Rank::Ctl, false)]
    );
    assert_eq!(
        caller_actor_as(Scope::Owner, Some(&s.ctx), &store, &in_group),
        ExitActor::Sid(sid.to_string()),
        "CONTROL: the exits ledger, which counts the group, places it in the tab"
    );

    let quiet = tab(&store, 4, 5999);
    let reply = mark(
        &quiet,
        &store,
        &detached,
        "- desc field=title value=hijacked",
    );
    assert_eq!(marked(&reply), "OK node=- by=ctl");
    assert_eq!(
        titles(&quiet.ctx),
        [claim("hijacked", "ctl", Rank::Ctl, true)],
        "CONTROL: with no program claim above it the ctl claim stands"
    );
    assert_eq!(
        meta(&quiet, &store, &detached, "set title its title"),
        "OK\n"
    );
    assert_eq!(pin(&quiet.ctx).as_deref(), Some("its title"));
}

/// A person's title is cleared by a person and by nobody else. It survives a control
/// client's `meta unset title`, the session's own program's, its creator's, and a clear of the
/// rename field that a control client opened and committed (by keys it sent, and by an
/// `invoke`): each takes back at most its own claim.
///
/// CONTROL: the person's own clear of the rename field removes it, and with it the title a
/// script had put under it, so nothing is left pinned or standing.
#[test]
fn only_a_person_clears_a_persons_claim() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let store = app.store.clone();
    // The app's first tab, its shell process 6999, created by the tab whose shell is 7999.
    let mut tab0 = store.read().unwrap().by_local(0).cloned().expect("tab 0");
    tab0.shell_identity.pid = 6999;
    store.write().unwrap().register(tab0.clone());
    let creator = tab(&store, 50, 7999);
    let mut task = TaskMark::new(TaskState::Working);
    task.from = Some(creator.sid.as_str().to_string());
    let _ = tab0.ctx.timeline.lock().unwrap().mark_task(
        &task,
        &Default::default(),
        std::time::Instant::now(),
        1,
    );
    let (script, program, parent) = (client(9001, 1), client(7001, 6999), client(8001, 7999));
    let ctx = &tab0.ctx;
    let persons = claim("mine", "human", Rank::Person, true);

    assert_eq!(app.write_session_title_pin(0, "mine"), Ok(true));
    let writers: [(&dyn Fn() -> Who, &str, Rank); 3] = [
        (&script, "ctl", Rank::Ctl),
        (&program, tab0.sid.as_str(), Rank::Program),
        (&parent, creator.sid.as_str(), Rank::Creator),
    ];
    for (who, by, rank) in writers {
        assert_eq!(meta(&tab0, &store, who, "set title theirs"), "OK\n", "{by}");
        assert_eq!(
            titles(ctx),
            [persons.clone(), claim("theirs", by, rank, false)],
            "{by}'s title is listed under the person's"
        );
        assert_eq!(meta(&tab0, &store, who, "unset title"), "OK\n", "{by}");
        assert_eq!(titles(ctx), [persons.clone()], "{by} took back its own");
        assert_eq!(meta(&tab0, &store, who, "unset title"), "OK\n", "{by}");
        assert_eq!(titles(ctx), [persons.clone()], "{by} holds none to take");
        assert_eq!(pin(ctx).as_deref(), Some("mine"), "{by}");
    }

    app.with_controller_mark(true, |app| {
        assert!(app.begin_active_session_rename(wid));
        app.commit_session_rename(wid, 0, "");
    });
    assert_eq!(
        (titles(ctx), pin(ctx).as_deref()),
        (vec![persons.clone()], Some("mine")),
        "a clear of a field a controller's keys opened is nobody's, and clears nothing"
    );
    assert_eq!(meta(&tab0, &store, &script, "set title a script's"), "OK\n");
    let scripts = claim("a script's", "ctl", Rank::Ctl, false);
    let invoked = app.invoke_attributed(crate::session_store::ExitActor::Ctl, |app| {
        assert!(app.begin_active_session_rename(wid));
        app.commit_session_rename(wid, 0, "");
        Ok(String::new())
    });
    assert_eq!(invoked, Ok(String::new()));
    assert_eq!(
        (titles(ctx), pin(ctx).as_deref()),
        (vec![persons.clone()], Some("mine")),
        "a clear an invoke committed is its control client's: that client's own claim went"
    );
    assert_eq!(meta(&tab0, &store, &script, "set title a script's"), "OK\n");
    assert_eq!(titles(ctx), [persons, scripts]);

    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "");
    assert_eq!(
        (titles(ctx), pin(ctx)),
        (vec![], None),
        "CONTROL: the person's own clear removes their title and the script's under it"
    );
}

/// A person's clear of the rename field takes off a title a control client set where the
/// person holds no claim of their own: the write is not a no-op, the claim goes and the pin
/// with it.
///
/// CONTROL: the same clear of a field a control client's keys opened leaves the script's
/// title.
#[test]
fn a_persons_clear_removes_a_title_a_script_set() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let store = app.store.clone();
    let tab0 = store.read().unwrap().by_local(0).cloned().expect("tab 0");
    let ctx = &tab0.ctx;
    assert_eq!(
        meta(&tab0, &store, client(9001, 1), "set title a script's"),
        "OK\n"
    );
    let scripts = claim("a script's", "ctl", Rank::Ctl, true);
    assert_eq!(pin(ctx).as_deref(), Some("a script's"));

    app.with_controller_mark(true, |app| {
        assert!(app.begin_active_session_rename(wid));
        app.commit_session_rename(wid, 0, "");
    });
    assert_eq!(
        (titles(ctx), pin(ctx).as_deref()),
        (vec![scripts], Some("a script's")),
        "CONTROL: a controller's clear is listed and removes nothing"
    );
    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "");
    assert_eq!((titles(ctx), pin(ctx)), (vec![], None));
}

/// A rename's author is who touched the field, not whose gesture settles it: a title a person
/// typed into a field they opened is theirs when a control client's `invoke` settles the edit
/// (an action that is not the field's own keeps what was typed and ends the edit first).
///
/// CONTROL: where a control client's keys also changed the text before the same `invoke`
/// settled it, the commit is not the person's: it is nobody's, listed under the person's
/// title, which stands.
#[test]
fn a_rename_a_person_typed_is_theirs_whoever_settles_it() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let tab0 = app
        .store
        .read()
        .unwrap()
        .by_local(0)
        .cloned()
        .expect("tab 0");
    let ctx = &tab0.ctx;
    let settle_by_invoke = |app: &mut crate::App| {
        let settled = app.invoke_attributed(ExitActor::Ctl, |app| {
            app.settle_rename_edit(wid);
            Ok(String::new())
        });
        assert_eq!(settled, Ok(String::new()));
        assert_eq!(
            app.rename_edit_session(wid),
            None,
            "the invoke settled the edit"
        );
    };
    let persons = claim("my title", "human", Rank::Person, true);

    assert!(app.begin_active_session_rename(wid));
    assert!(app.rename_input_event(wid, &InputEvent::Text("my title".into())));
    settle_by_invoke(&mut app);
    assert_eq!(titles(ctx), [persons.clone()]);
    assert_eq!(pin(ctx).as_deref(), Some("my title"));

    assert!(app.begin_active_session_rename(wid));
    app.with_controller_mark(true, |app| {
        assert!(app.rename_field_edit(wid, crate::app_search::SearchEdit::MoveEnd));
        assert!(app.rename_input_event(wid, &InputEvent::Text(" too".into())));
    });
    settle_by_invoke(&mut app);
    assert_eq!(
        titles(ctx),
        [persons, claim("my title too", "-", Rank::Listed, false)],
        "CONTROL: a field a controller's keys changed is not the person's"
    );
    assert_eq!(pin(ctx).as_deref(), Some("my title"));
}

/// A rename a control client opened and typed is never a person's, though a person's click
/// settles it: its keys come with no attribution, so the text is nobody's, listed, and the
/// tab keeps no title.
///
/// CONTROL: the text a person typed into a field they opened, settled by the same click, is
/// the person's and becomes the title.
#[test]
fn a_rename_a_controller_typed_is_not_a_persons_whoever_settles_it() {
    let mut app = crate::App::headless_for_test();
    let wid = crate::WindowId(0);
    app.tab_strip_rows = 1;
    let tab0 = app
        .store
        .read()
        .unwrap()
        .by_local(0)
        .cloned()
        .expect("tab 0");
    let ctx = &tab0.ctx;
    let theirs = claim("Evil", "-", Rank::Listed, false);

    app.with_controller_mark(true, |app| {
        assert!(app.begin_active_session_rename(wid));
        assert!(app.rename_input_event(wid, &InputEvent::Text("Evil".into())));
    });
    app.settle_rename_edit(wid);
    assert_eq!(
        app.rename_edit_session(wid),
        None,
        "the click settled the edit"
    );
    assert_eq!(titles(ctx), [theirs.clone()]);
    assert_eq!(pin(ctx), None, "a listed rename moves nothing shown");

    assert!(app.begin_active_session_rename(wid));
    assert!(app.rename_input_event(wid, &InputEvent::Text("Mine".into())));
    app.settle_rename_edit(wid);
    assert_eq!(
        titles(ctx),
        [claim("Mine", "human", Rank::Person, true), theirs],
        "CONTROL: a person's own"
    );
    assert_eq!(pin(ctx).as_deref(), Some("Mine"));
}

/// The creator a claim ranks by is the session the task record's `from` names: a process in
/// that session describing the worker ranks `creator` and stands, and a process in a third
/// session is listed.
///
/// CONTROL: before the record names it, and again once the record says the task came from no
/// session (`from=ctl`), the same claim of the same session is only listed.
#[test]
fn a_creators_claim_ranks_creator_and_a_third_sessions_is_listed() {
    let store = new_store();
    let worker = tab(&store, 5, 5000);
    let lead = tab(&store, 6, 6000);
    let third = tab(&store, 7, 7000);
    let (in_lead, in_third) = (client(6001, 6000), client(7001, 7000));
    let from = |sid: &str| {
        let mut task = TaskMark::new(TaskState::Working);
        task.from = Some(sid.to_string());
        let mut g = worker.ctx.timeline.lock().unwrap();
        let _ = g.mark_task(&task, &Default::default(), std::time::Instant::now(), 1);
    };
    let reply = mark(
        &worker,
        &store,
        &in_lead,
        "- desc field=title value=fix%20the%20fold",
    );
    assert!(reply.starts_with("OK seq="), "{reply}");
    let leads = |rank, stands| claim("fix the fold", lead.sid.as_str(), rank, stands);
    assert_eq!(
        titles(&worker.ctx),
        [leads(Rank::Listed, false)],
        "CONTROL: no task record names a creator yet"
    );
    from(lead.sid.as_str());
    assert_eq!(titles(&worker.ctx), [leads(Rank::Creator, true)]);
    let reply = mark(
        &worker,
        &store,
        &in_third,
        "- desc field=title value=not%20mine",
    );
    assert!(reply.starts_with("OK seq="), "{reply}");
    assert_eq!(
        titles(&worker.ctx),
        [
            leads(Rank::Creator, true),
            claim("not mine", third.sid.as_str(), Rank::Listed, false)
        ]
    );
    from(crate::task_record::FROM_CTL);
    assert_eq!(
        titles(&worker.ctx)[0],
        leads(Rank::Listed, false),
        "CONTROL: a task from no session has no creator"
    );
}

/// A session's log knows whose session it is after it is replaced: a log that started over at
/// an update's handoff ranks the session's own process `program` and its creator's `creator`,
/// as the log before it did.
///
/// CONTROL: a session log no store registered was told nothing, and names nobody.
#[test]
fn the_sessions_identity_survives_a_log_replacement() {
    let store = new_store();
    let s = tab(&store, 8, 6999);
    let lead = tab(&store, 9, 7999);
    let ident = |h: &SessionHandle| {
        let g = h.ctx.timeline.lock().unwrap();
        let ident = *g.slog().log().state().desc.ident();
        (
            ident.own.map(|id| id.to_string()),
            ident.creator.map(|id| id.to_string()),
        )
    };
    {
        let mut g = s.ctx.timeline.lock().unwrap();
        let mut task = TaskMark::new(TaskState::Working);
        task.from = Some(lead.sid.as_str().to_string());
        let _ = g.mark_task(&task, &Default::default(), std::time::Instant::now(), 1);
    }
    let named = (
        Some(s.sid.as_str().to_string()),
        Some(lead.sid.as_str().to_string()),
    );
    assert_eq!(ident(&s), named);
    assert_eq!(
        ident(&test_handle(10)),
        (None, None),
        "CONTROL: an unregistered session's log names nobody"
    );
    {
        let mut g = s.ctx.timeline.lock().unwrap();
        let (before, now) = (g.slog().log().ep(), g.slog().now());
        g.slog_mut()
            .adopt(crate::session_log::LogAdopt::default(), false, None, now);
        assert_ne!(g.slog().log().ep(), before, "the log was replaced");
        g.slog_mut().open_session(None, None, now);
    }
    assert_eq!(ident(&s), named, "the new log was told who the session is");
    let reply = mark(
        &s,
        &store,
        client(7001, 6999),
        "- desc field=title value=building",
    );
    assert!(reply.starts_with("OK seq="), "{reply}");
    let reply = mark(
        &s,
        &store,
        client(8001, 7999),
        "- desc field=title value=assigned",
    );
    assert!(reply.starts_with("OK seq="), "{reply}");
    assert_eq!(
        titles(&s.ctx),
        [
            claim("building", s.sid.as_str(), Rank::Program, true),
            claim("assigned", lead.sid.as_str(), Rank::Creator, false),
        ]
    );
}

/// A tab's ID has two written forms, and a session is one identity in whichever it holds: a
/// process in a tab that keeps the `s-` form and one in a tab with the `t` form each rank
/// `program` in its own session, and each is listed in the other's. The author, the session's
/// own identity and its creator's are all read through one function, so they agree.
///
/// CONTROL: a name no session was minted with is nobody's identity, and a claim by it is not
/// a session's.
#[cfg(unix)]
#[test]
fn the_two_written_forms_of_a_sid_are_each_one_identity() {
    use crate::session_log::{Epochs, FIRST_EPOCH, sid_ident};
    use aterm_session::{LaunchNonce, SessionId};
    let store = new_store();
    let identified = |local: u64, sid: &str, shell: i32| {
        let epochs = std::sync::Arc::new(Epochs::new());
        epochs.note(FIRST_EPOCH);
        let mut h = crate::session_store::test_handle_identified(
            local,
            SessionId::parse(sid).expect("a minted sid"),
            LaunchNonce::generate(),
            epochs,
            aterm_slog::Stamp::new(1_760_000_000_000, 0),
        );
        h.shell_identity.pid = shell;
        store.write().unwrap().register(h.clone());
        h
    };
    let (old_sid, new_sid) = ("s-1a2b3c4d5e6f70819203", "tqn5r6e0148");
    assert!(aterm_types::tab_id::is_legacy(old_sid) && aterm_types::tab_id::is_tab_id(new_sid));
    let old = identified(11, old_sid, 6999);
    let new = identified(12, new_sid, 7999);
    let (in_old, in_new) = (client(6001, 6999), client(8001, 7999));
    for (h, own, other, own_sid, other_sid) in [
        (&old, &in_old, &in_new, old_sid, new_sid),
        (&new, &in_new, &in_old, new_sid, old_sid),
    ] {
        let reply = mark(h, &store, own, "- desc field=title value=mine");
        assert_eq!(marked(&reply), format!("OK node=- by={own_sid}"));
        let reply = mark(h, &store, other, "- desc field=title value=theirs");
        assert_eq!(marked(&reply), format!("OK node=- by={other_sid}"));
        assert_eq!(
            titles(&h.ctx),
            [
                claim("mine", own_sid, Rank::Program, true),
                claim("theirs", other_sid, Rank::Listed, false),
            ],
            "{own_sid}"
        );
        let own_id = sid_ident(own_sid).expect("a minted sid");
        assert_eq!(
            crate::session_log::sid_actor(own_sid),
            Some(Actor::Sid(own_id)),
            "the author and the identity are one reading of the sid"
        );
        let g = h.ctx.timeline.lock().unwrap();
        assert_eq!(g.slog().log().state().desc.ident().own, Some(own_id));
    }
    for unminted in ["ctl", "build-agent", "S-1A2B3C4D5E6F70819203", "human"] {
        assert_eq!(sid_ident(unminted), None, "CONTROL: {unminted}");
    }
}

/// A rename field committed as it opened states nothing, judged by what it opened holding and
/// not by the pin at commit: a title a script sets while a person's empty field is open stays
/// when the person settles the field untouched, and no record of the person's is written; a
/// field the session's own program opened with `invoke`, settled untouched, writes no claim of
/// the program's over the script's, so the script's next title still moves the pin.
///
/// CONTROL: the person who empties a field that opened holding the script's title removes it.
#[test]
fn a_rename_field_committed_as_it_opened_writes_nothing_whatever_was_pinned_meanwhile() {
    let (mut app, wid, tab0) = app_tab();
    let store = app.store.clone();
    let ctx = &tab0.ctx;
    let script = client(9001, 1);

    assert!(app.begin_active_session_rename(wid));
    assert_eq!(meta(&tab0, &store, &script, "set title nightly"), "OK\n");
    assert_eq!(pin(ctx).as_deref(), Some("nightly"));
    let before = title_records(ctx);
    app.settle_rename_edit(wid);
    assert_eq!(
        app.rename_edit_session(wid),
        None,
        "the click settled the edit"
    );
    assert_eq!(
        (titles(ctx), pin(ctx).as_deref()),
        (
            vec![claim("nightly", "ctl", Rank::Ctl, true)],
            Some("nightly")
        ),
        "an empty field left as it opened takes nothing off"
    );
    assert_eq!(title_records(ctx), before, "and writes no record");

    let own = ExitActor::Sid(tab0.sid.as_str().to_string());
    let opened = app.invoke_attributed(own, |app| {
        assert!(app.begin_active_session_rename(wid));
        Ok(String::new())
    });
    assert_eq!(opened, Ok(String::new()));
    app.settle_rename_edit(wid);
    assert_eq!(
        title_records(ctx),
        before,
        "the program's untouched field claims nothing"
    );
    assert_eq!(meta(&tab0, &store, &script, "set title nightly 2"), "OK\n");
    assert_eq!(
        pin(ctx).as_deref(),
        Some("nightly 2"),
        "no program claim holds the title"
    );

    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "");
    assert_eq!(
        (titles(ctx), pin(ctx)),
        (vec![], None),
        "CONTROL: emptying it removes it"
    );
    assert_eq!(
        title_records(ctx).last().map(String::as_str),
        Some("meta field=title by=human src=meta conf=exact")
    );
}

/// What another writer titled the tab while a person's field was open stays when the person
/// commits the field holding what it opened with, trimmed or not: the field opened holding a
/// script's `A`, the script set `B`, and the person's commit of `A` leaves `B` and writes
/// nothing. A title a restore pinned with no claim, committed as the field opened holding it,
/// is still nobody's, and the script's next title replaces it.
///
/// CONTROL: a person who edits the field to the text pinned now claims it, so the script's
/// next title is listed under the person's.
#[test]
fn an_untouched_rename_leaves_what_another_writer_titled_while_it_was_open() {
    let (mut app, wid, tab0) = app_tab();
    let store = app.store.clone();
    let ctx = &tab0.ctx;
    let script = client(9001, 1);

    let carried = crate::session_timeline::SessionMeta {
        user_title: Some("from last time".into()),
        ..crate::session_timeline::SessionMeta::default()
    };
    assert!(crate::session_timeline::restore_carried_meta(ctx, &carried));
    let restored = title_records(ctx);
    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "from last time");
    assert_eq!(
        (title_records(ctx), pin(ctx).as_deref()),
        (restored, Some("from last time")),
        "a restored title committed untouched gets no claim and no record"
    );
    assert_eq!(titles(ctx), []);
    assert_eq!(meta(&tab0, &store, &script, "set title A"), "OK\n");
    assert_eq!(
        pin(ctx).as_deref(),
        Some("A"),
        "and the script's replaces it"
    );

    for committed in ["A", "  A "] {
        assert_eq!(meta(&tab0, &store, &script, "set title A"), "OK\n");
        assert!(app.begin_active_session_rename(wid));
        assert_eq!(meta(&tab0, &store, &script, "set title B"), "OK\n");
        let before = title_records(ctx);
        app.commit_session_rename(wid, 0, committed);
        assert_eq!(
            (titles(ctx), pin(ctx).as_deref()),
            (vec![claim("B", "ctl", Rank::Ctl, true)], Some("B")),
            "{committed:?}"
        );
        assert_eq!(title_records(ctx), before, "{committed:?} writes no record");
    }

    assert!(app.begin_active_session_rename(wid));
    assert_eq!(meta(&tab0, &store, &script, "set title C"), "OK\n");
    app.commit_session_rename(wid, 0, "C");
    let persons = claim("C", "human", Rank::Person, true);
    assert_eq!(
        titles(ctx),
        [persons.clone(), claim("C", "ctl", Rank::Ctl, false)],
        "CONTROL: an edited field is the person's, though it says what is pinned"
    );
    assert_eq!(meta(&tab0, &store, &script, "set title D"), "OK\n");
    assert_eq!(
        (titles(ctx), pin(ctx).as_deref()),
        (
            vec![persons, claim("D", "ctl", Rank::Ctl, false)],
            Some("C")
        )
    );
}

/// An empty field committed as it opened removes no claim: under a control client's `mark -
/// desc field=title` claim, which stands and pins nothing, the field opens empty, and the
/// person's commit of it empty writes no record and leaves the claim standing.
///
/// CONTROL: once the client's title is pinned through `meta set title`, the field opens
/// holding it, and the person who empties it removes it.
#[test]
fn an_empty_field_committed_as_it_opened_removes_no_claim() {
    let (mut app, wid, tab0) = app_tab();
    let store = app.store.clone();
    let ctx = &tab0.ctx;
    let script = client(9001, 1);
    let reply = mark(
        &tab0,
        &store,
        &script,
        "- desc field=title value=nightly%20build",
    );
    assert_eq!(marked(&reply), "OK node=- by=ctl");
    assert_eq!(pin(ctx), None, "a mark's claim pins nothing by itself");
    let before = title_records(ctx);
    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "");
    assert_eq!(title_records(ctx), before, "no record");
    assert_eq!(
        titles(ctx),
        [claim("nightly build", "ctl", Rank::Ctl, true)],
        "the client's claim still stands"
    );

    assert_eq!(
        meta(&tab0, &store, &script, "set title nightly build"),
        "OK\n"
    );
    assert_eq!(pin(ctx).as_deref(), Some("nightly build"));
    assert!(app.begin_active_session_rename(wid));
    app.commit_session_rename(wid, 0, "");
    assert_eq!(
        (titles(ctx), pin(ctx)),
        (vec![], None),
        "CONTROL: the person emptied a pinned title"
    );
}

/// A key an `hwkey` burst posted into the macOS rename field makes the edit a control client's,
/// as its keys do in the in-grid field: the field's delegate reports the change in a wake that
/// runs under the burst's marks ([`crate::App::native_rename_edited`]), and the commit the
/// field's end of editing posts after it is nobody's, listed, and pins nothing.
///
/// CONTROL: a change under a person's marks records nobody, so text the person typed is theirs
/// and becomes the title, though a burst's Return committed it: who touched the field is the
/// author, not who ended the edit.
#[test]
fn a_native_rename_a_burst_typed_is_not_a_persons() {
    use crate::app_take_over::GestureMarks;
    use aterm_window::event::InputOrigin;
    let (mut app, wid, tab0) = app_tab();
    let ctx = &tab0.ctx;
    let native = |app: &mut crate::App| {
        assert!(app.begin_active_session_rename(wid));
        let mut window = app.windows.get_mut(&wid).expect("the window");
        let mut edit = window.routing.rename_mut().expect("an edit");
        edit.surface = crate::app_rename::RenameSurface::Native;
        edit.session
    };
    let theirs = claim("Evil", "-", Rank::Listed, false);

    let session = native(&mut app);
    app.with_gesture_marks(GestureMarks::of(InputOrigin::Posted), |app| {
        app.native_rename_edited(wid, session);
    });
    app.commit_session_rename(wid, session, "Evil");
    assert_eq!(titles(ctx), [theirs.clone()]);
    assert_eq!(pin(ctx), None, "a listed rename moves nothing shown");

    let session = native(&mut app);
    app.with_gesture_marks(GestureMarks::of(InputOrigin::Person), |app| {
        app.native_rename_edited(wid, session);
    });
    app.with_gesture_marks(GestureMarks::of(InputOrigin::Posted), |app| {
        app.commit_session_rename(wid, session, "Mine");
    });
    assert_eq!(
        titles(ctx),
        [claim("Mine", "human", Rank::Person, true), theirs],
        "CONTROL: a person's own"
    );
    assert_eq!(pin(ctx).as_deref(), Some("Mine"));
}

/// A tab kept open after its shell exited holds no process: where the kernel gave that shell's
/// pid to a process between a writer and its own tab's shell, the writer is in its own tab, and
/// its title there ranks `program` and stands; and a launchd job that holds the pid is `ctl`
/// on a worker whose task names the exited tab in `from=`, not that worker's creator.
///
/// CONTROL: while that tab is alive the same writers are in it: listed on the other tab, and
/// the worker's creator.
#[test]
fn an_exited_tabs_reused_pid_places_no_writer_in_it() {
    use crate::session_store::SessionState;
    let store = new_store();
    let a = tab(&store, 1, 6999);
    let b = tab(&store, 2, 7999);
    let w = tab(&store, 3, 8999);
    let mut task = TaskMark::new(TaskState::Working);
    task.from = Some(a.sid.as_str().to_string());
    let _ = w.ctx.timeline.lock().unwrap().mark_task(
        &task,
        &Default::default(),
        std::time::Instant::now(),
        1,
    );
    let (a_sid, b_sid) = (a.sid.as_str(), b.sid.as_str());
    // 9001's parent holds A's shell's pid again, and is a child of B's shell.
    let in_b = || {
        Who::client_with(
            Some(9001),
            1,
            &|p| match p {
                9001 => Some(6999),
                6999 => Some(7999),
                _ => None,
            },
            &|_| None,
            &|_| Some("aterm".to_string()),
        )
    };
    let job = client(9101, 6999);
    let state = |state| store.write().unwrap().set_state(1, state);

    state(SessionState::Exited);
    assert_eq!(super::control_session::owner_tab_sid(&job(), &store), None);
    assert_eq!(meta(&b, &store, in_b, "set title X"), "OK\n");
    let own = claim("X", b_sid, Rank::Program, true);
    assert_eq!(titles(&b.ctx), [own.clone()]);
    assert_eq!(pin(&b.ctx).as_deref(), Some("X"));
    let reply = mark(&w, &store, &job, "- desc field=title value=Y");
    assert_eq!(marked(&reply), "OK node=- by=ctl");
    let ctls = claim("Y", "ctl", Rank::Ctl, true);
    assert_eq!(titles(&w.ctx), [ctls]);

    state(SessionState::Alive);
    assert_eq!(
        super::control_session::owner_tab_sid(&job(), &store).as_deref(),
        Some(a_sid)
    );
    assert_eq!(meta(&b, &store, in_b, "set title X2"), "OK\n");
    assert_eq!(
        (titles(&b.ctx), pin(&b.ctx).as_deref()),
        (
            vec![own, claim("X2", a_sid, Rank::Listed, false)],
            Some("X")
        ),
        "CONTROL: in A while A is alive"
    );
    let reply = mark(&w, &store, &job, "- desc field=title value=Y");
    assert_eq!(marked(&reply), format!("OK node=- by={a_sid}"));
    assert_eq!(
        titles(&w.ctx),
        [
            claim("Y", a_sid, Rank::Creator, true),
            claim("Y", "ctl", Rank::Ctl, false)
        ],
        "CONTROL: the worker's creator"
    );
}

/// A writer whose chain of parents is longer than aterm reads (32 processes) is nobody (`by=-`),
/// listed, and never stands, by `meta set title` and by `mark … desc`, though another
/// session's shell is above it: that shell is out of reach, so no tab is proven, and `ctl`
/// would let another session's process stand.
///
/// CONTROL: the other session's shell as the 32nd process read places the writer in that
/// session (listed, as that session); a chain of the same length that ends at launchd with no
/// tab's shell in it is `ctl` and stands; and a parent aterm cannot read (another user's, as a
/// root `login` is) ends the walk as launchd does, so that writer is `ctl` and stands too.
#[test]
fn a_writer_deeper_than_aterm_reads_is_listed_and_never_stands() {
    let store = new_store();
    let b = tab(&store, 2, 7999);
    let deep = nested(20000, 32, 7999);
    let a = tab(&store, 1, 6999);
    assert_eq!(meta(&a, &store, &deep, "set title X"), "OK\n");
    assert_eq!(
        (titles(&a.ctx), pin(&a.ctx)),
        (vec![claim("X", "-", Rank::Listed, false)], None)
    );
    let reply = mark(&a, &store, &deep, "- desc field=title value=Y");
    assert_eq!(marked(&reply), "OK node=- by=-");
    assert_eq!(titles(&a.ctx), [claim("Y", "-", Rank::Listed, false)]);

    let within = tab(&store, 3, 5003);
    assert_eq!(
        meta(&within, &store, nested(21000, 31, 7999), "set title X"),
        "OK\n"
    );
    assert_eq!(
        (titles(&within.ctx), pin(&within.ctx)),
        (vec![claim("X", b.sid.as_str(), Rank::Listed, false)], None),
        "CONTROL: the shell within reach"
    );

    let outside = tab(&store, 4, 5004);
    assert_eq!(
        meta(&outside, &store, nested(22000, 32, 1), "set title X"),
        "OK\n"
    );
    assert_eq!(
        (titles(&outside.ctx), pin(&outside.ctx).as_deref()),
        (vec![claim("X", "ctl", Rank::Ctl, true)], Some("X")),
        "CONTROL: a chain that ends at launchd"
    );

    let unread = tab(&store, 5, 5005);
    let reply = mark(
        &unread,
        &store,
        client(9002, 4242),
        "- desc field=title value=Y",
    );
    assert_eq!(marked(&reply), "OK node=- by=ctl");
    assert_eq!(
        titles(&unread.ctx),
        [claim("Y", "ctl", Rank::Ctl, true)],
        "CONTROL: a parent aterm cannot read"
    );
}

/// The typing credit's naming skips an exited tab as `owner_tab_sid` does: a writer whose
/// parent holds an exited tab's reaped shell pid again is named by the live tab above it.
/// CONTROL: with the tab alive the same writer is named by it.
#[test]
fn an_exited_tabs_reused_pid_names_no_typist() {
    use crate::session_store::SessionState;
    // 9001's parent holds A's shell's pid, and is a child of B's shell.
    let in_b = || {
        Who::client_with(
            Some(9001),
            1,
            &|p| match p {
                9001 => Some(6999),
                6999 => Some(7999),
                _ => None,
            },
            &|_| None,
            &|_| Some("aterm".to_string()),
        )
    };
    let named = |store: &Store| crate::typed_by::name_by_store(&in_b(), store).words;
    let store = new_store();
    let a = tab(&store, 1, 6999);
    let b = tab(&store, 2, 7999);
    let a_named = crate::typed_by::tab_named(&a.ctx).words;
    let b_named = crate::typed_by::tab_named(&b.ctx).words;
    assert_ne!(
        a_named, b_named,
        "PRECONDITION: the two tabs are named apart"
    );
    assert_eq!(
        named(&store),
        a_named,
        "CONTROL: a live tab's shell names its writer"
    );
    store.write().unwrap().set_state(1, SessionState::Exited);
    assert_eq!(named(&store), b_named, "the live tab above it names it");
    assert_ne!(named(&store), a_named, "an exited tab names nobody");
}
