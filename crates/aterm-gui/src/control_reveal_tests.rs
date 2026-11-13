// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `reveal`'s verdict, its order, the session facts it reads and the spawn paths that mark a
//! session `ctl spawn`'s.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::*;
use crate::SessionCtx;
use crate::session_store::{new_store, test_handle};

const NOW: u64 = 1_800_000_000_000;

/// One process of a [`Table`].
#[derive(Clone, Copy, Default)]
struct Proc {
    parent: i32,
    session: i32,
    agent: bool,
}

/// A process table a test writes (each process its own group).
#[derive(Default)]
struct Table(BTreeMap<i32, Proc>);

impl Table {
    fn with(mut self, pid: i32, parent: i32, agent: bool) -> Self {
        self.0.insert(
            pid,
            Proc {
                parent,
                session: 0,
                agent,
            },
        );
        self
    }

    fn in_session(mut self, pid: i32, session: i32, agent: bool) -> Self {
        self.0.insert(
            pid,
            Proc {
                parent: 1,
                session,
                agent,
            },
        );
        self
    }
}

impl Procs for Table {
    fn parent_of(&self, pid: i32) -> Option<i32> {
        self.0.get(&pid).map(|p| p.parent).filter(|p| *p > 0)
    }

    fn pgid_of(&self, pid: i32) -> Option<i32> {
        self.0.contains_key(&pid).then_some(pid)
    }

    fn session_of(&self, pid: i32) -> Option<i32> {
        self.0.get(&pid).map(|p| p.session).filter(|s| *s > 0)
    }

    fn pids(&self) -> Vec<i32> {
        self.0.keys().copied().collect()
    }

    fn agent(&self, pid: i32) -> bool {
        self.0.get(&pid).is_some_and(|p| p.agent)
    }
}

/// A store holding one live session whose shell is `shell`.
fn store_with(shell: i32) -> (Store, String, Arc<SessionCtx>) {
    let store = new_store();
    let (sid, ctx) = register(&store, 1, shell);
    (store, sid, ctx)
}

fn register(store: &Store, local: u64, shell: i32) -> (String, Arc<SessionCtx>) {
    let mut handle = test_handle(local);
    handle.shell_identity = aterm_pty::ShellIdentity::from_parts(shell, None, true);
    let sid = handle.sid.as_str().to_owned();
    let ctx = Arc::clone(&handle.ctx);
    store.write().unwrap().register(handle);
    (sid, ctx)
}

/// Shell 500, its child 505 (a script), and the asking process 510 under that.
fn lineage() -> Table {
    Table::default()
        .with(500, 1, false)
        .with(505, 500, false)
        .with(510, 505, false)
}

fn line(word: &str, sid: &str) -> String {
    format!("OK 1\nverdict={word} session={sid} fleet=no\n")
}

/// [`line`] where another Mac is behind the question.
fn fleet_line(word: &str, sid: &str) -> String {
    format!("OK 1\nverdict={word} session={sid} fleet=yes\n")
}

/// ANOTHER MAC BEHIND THE QUESTION IS SAID BESIDE THE VERDICT, so a sign-in started in the
/// session opens nothing on this Mac's desk: a fleet driver admitted to type into the session
/// marks it from its admission, before anything is written, and for as long as the session
/// lives, whatever a local driver or aterm types after; a question a fleet driver or the
/// bridge asked is one. CONTROLS: a local driver's typing is `driven` and no other Mac's, and
/// a person's tab is neither.
#[test]
fn a_fleet_driver_s_typing_marks_the_session_as_another_mac_s() {
    use crate::typed_by::{Caller, Who, fleet_credit};
    let (store, sid, ctx) = store_with(500);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("allowed", &sid)
    );
    ctx.typed
        .stamp(Caller::new(Who::Word("a driver".into())), NOW - 20);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("driven", &sid),
        "a local driver is no other Mac"
    );
    let fleet = Caller::new(Who::Word(fleet_credit("m3")));
    ctx.typed.admit(&fleet);
    assert!(ctx.typed.fleet_admitted(), "from its admission");
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        fleet_line("driven", &sid)
    );
    ctx.typed
        .stamp(Caller::new(Who::Word("a driver".into())), NOW - 5);
    ctx.typed.stamp(Caller::new(Who::Aterm), NOW - 1);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        fleet_line("driven", &sid),
        "what it typed can still be running"
    );

    // Its typing alone marks a session too.
    let (store, sid, ctx) = store_with(500);
    ctx.typed
        .stamp(Caller::new(Who::Word(fleet_credit("m3"))), NOW - 5);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        fleet_line("driven", &sid)
    );
    // A question asked remotely is another Mac's, and names no session.
    assert_eq!(
        answer(&store, true, Some(510), &lineage()),
        fleet_line("remote", "-")
    );
    // aterm's own typing and the message bus's mark nothing.
    let (store, sid, ctx) = store_with(500);
    ctx.typed.stamp(Caller::new(Who::Aterm), NOW - 5);
    ctx.typed.stamp(Caller::new(Who::Bridge), NOW - 4);
    assert!(!ctx.typed.fleet_admitted());
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("allowed", &sid)
    );
}

#[test]
fn the_verdict_is_the_first_fact_that_holds_in_order() {
    let all = SessionFacts {
        spawned: true,
        carried: true,
        held: true,
        driven: true,
        agent: true,
        fleet: true,
    };
    let mut facts = RevealFacts {
        remote: true,
        peer: false,
        session: Some(all),
    };
    let mut seen = Vec::new();
    seen.push(reveal_verdict(facts));
    facts.remote = false;
    seen.push(reveal_verdict(facts));
    facts.peer = true;
    seen.push(reveal_verdict(RevealFacts {
        session: None,
        ..facts
    }));
    for clear in 0..=5 {
        let session = SessionFacts {
            spawned: clear < 1,
            carried: clear < 2,
            held: clear < 3,
            driven: clear < 4,
            agent: clear < 5,
            // Who typed decides no verdict: `driven` already withholds.
            fleet: true,
        };
        seen.push(reveal_verdict(RevealFacts {
            session: Some(session),
            ..facts
        }));
    }
    assert_eq!(
        seen,
        [
            Verdict::Remote,
            Verdict::Indirect,
            Verdict::None,
            Verdict::Spawned,
            Verdict::Carried,
            Verdict::Held,
            Verdict::Driven,
            Verdict::Agent,
            Verdict::Allowed,
        ]
    );
    let words: Vec<&str> = seen.iter().map(|v| v.word()).collect();
    assert_eq!(
        words,
        [
            "remote", "indirect", "none", "spawned", "carried", "held", "driven", "agent",
            "allowed"
        ]
    );
}

#[test]
fn allowed_only_when_no_fact_withholds() {
    for bits in 0u16..256 {
        let bit = |n: u16| bits & (1 << n) != 0;
        let session = SessionFacts {
            spawned: bit(3),
            carried: bit(4),
            held: bit(5),
            driven: bit(6),
            agent: bit(7),
            fleet: false,
        };
        let facts = RevealFacts {
            remote: bit(0),
            peer: bit(1),
            session: bit(2).then_some(session),
        };
        let clear = !facts.remote && facts.peer && facts.session == Some(SessionFacts::default());
        assert_eq!(
            reveal_verdict(facts) == Verdict::Allowed,
            clear,
            "{facts:?}"
        );
    }
}

#[test]
fn a_person_s_plain_tab_answers_allowed_and_a_ctl_spawned_one_spawned() {
    let (store, sid, ctx) = store_with(500);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("allowed", &sid)
    );
    assert_eq!(
        answer(&store, false, Some(500), &lineage()),
        line("allowed", &sid),
        "the shell itself asking"
    );
    mark_ctl_spawned(&store, &sid);
    assert_eq!(ctx.spawn_mark.read(), SpawnMark::CTL);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("spawned", &sid)
    );
}

#[test]
fn remote_indirect_and_none_name_no_session() {
    let (store, sid, _) = store_with(500);
    assert_eq!(
        answer(&store, true, Some(510), &lineage()),
        fleet_line("remote", "-")
    );
    assert_eq!(
        answer(&store, false, None, &lineage()),
        line("indirect", "-")
    );
    let stray = lineage().with(900, 1, false).with(910, 900, false);
    assert_eq!(answer(&store, false, Some(910), &stray), line("none", "-"));
    // The negative control: the same table, asked from inside the session.
    assert_eq!(
        answer(&store, false, Some(510), &stray),
        line("allowed", &sid)
    );
}

#[test]
fn the_remote_hold_answers_held() {
    let (store, sid, _) = store_with(500);
    {
        let _driving = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: "0c".repeat(16),
            host: "mac-0c".to_owned(),
        });
        assert_eq!(
            crate::fabric::cmd_hold(
                &store,
                &format!("{sid} on"),
                crate::fabric::HoldIssuer::Remote
            ),
            "OK hold=1\n"
        );
    }
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("held", &sid)
    );
}

#[test]
fn a_driver_s_typing_answers_driven_and_aterm_s_own_does_not() {
    use crate::typed_by::{Caller, Who};
    let (store, sid, ctx) = store_with(500);
    ctx.typed.stamp(Caller::new(Who::Aterm), NOW - 10);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("allowed", &sid),
        "aterm's own harness typing drives nothing"
    );
    ctx.typed
        .stamp(Caller::new(Who::Word("a driver".into())), NOW - 3_600_000);
    ctx.typed.stamp(Caller::new(Who::Aterm), NOW - 10);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("driven", &sid),
        "an hour-old drive still counts, whatever aterm typed since"
    );
}

#[test]
fn a_driver_counts_from_its_admission_before_anything_is_written() {
    use crate::typed_by::{Caller, Who};
    let (store, sid, ctx) = store_with(500);
    let aterm = crate::control_input_ack::InputMark::at_epoch(
        &ctx,
        1,
        aterm_slog::Actor::Ctl,
        crate::session_log::InputVerb::Send,
        None,
        false,
    )
    .typed_by(&ctx, Some(Caller::new(Who::Aterm)));
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("allowed", &sid),
        "aterm's own admitted typing drives nothing"
    );
    drop(aterm);
    let _admitted = crate::control_input_ack::InputMark::at_epoch(
        &ctx,
        1,
        aterm_slog::Actor::Ctl,
        crate::session_log::InputVerb::Send,
        None,
        false,
    )
    .typed_by(&ctx, Some(Caller::new(Who::Word("a driver".into()))));
    assert!(ctx.typed.last().is_none(), "nothing is written yet");
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("driven", &sid)
    );
}

#[test]
fn a_session_an_update_carried_over_answers_carried_and_keeps_its_mark() {
    let (store, sid, ctx) = store_with(500);
    ctx.spawn_mark
        .0
        .store(SpawnMark::CARRIED, Ordering::Release);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("carried", &sid)
    );
    mark_ctl_spawned(&store, &sid);
    assert_eq!(
        ctx.spawn_mark.read(),
        SpawnMark::CARRIED,
        "a later `ctl spawn` answer never clears it"
    );
    assert_eq!(SpawnMark::at_spawn(true).read(), SpawnMark::CARRIED);
    assert_eq!(SpawnMark::at_spawn(false).read(), SpawnMark::PERSON);
    assert_eq!(SpawnMark::default().read(), SpawnMark::PERSON);
}

#[test]
fn an_agent_in_the_session_s_tree_answers_agent() {
    let (store, sid, ctx) = store_with(500);
    register(&store, 2, 600);
    // Negative controls: an agent in another session's tree, and one above the shell.
    let elsewhere = lineage()
        .with(600, 1, false)
        .with(620, 600, true)
        .with(400, 1, true);
    assert_eq!(
        answer(&store, false, Some(510), &elsewhere),
        line("allowed", &sid)
    );
    let child = elsewhere.with(520, 500, true);
    assert_eq!(
        answer(&store, false, Some(510), &child),
        line("agent", &sid),
        "an agent the shell started"
    );
    let detached = lineage().in_session(530, 500, true);
    assert_eq!(
        answer(&store, false, Some(510), &detached),
        line("agent", &sid),
        "an agent reparented to launchd stays in the shell's terminal session"
    );
    ctx.timeline.lock().unwrap().set_agent_reader_for_test(true);
    assert_eq!(
        answer(&store, false, Some(510), &lineage()),
        line("agent", &sid),
        "the agent the session's screen reads"
    );
}

#[cfg(unix)]
#[test]
fn the_connection_s_kernel_peer_decides() {
    use crate::control::{DirectPeer, ServingFd};
    let own = i32::try_from(std::process::id()).unwrap();
    let (store, sid, _) = store_with(own);
    let (_client, server) = aterm_uds::CtlStream::pair().expect("socket pair");
    let _fd = ServingFd::enter(&server);
    let table = Table::default();
    assert_eq!(
        served(&store, false, "", &table),
        line("indirect", "-"),
        "a connection the kernel names no direct peer of"
    );
    let _direct = DirectPeer::enter();
    assert_eq!(served(&store, false, "", &table), line("allowed", &sid));
    assert_eq!(served(&store, true, "", &table), fleet_line("remote", "-"));
    {
        let _driving = crate::fabric::DrivingAs::enter(crate::fabric::Driver {
            node: "0d".repeat(16),
            host: "mac-0d".to_owned(),
        });
        assert_eq!(served(&store, false, "", &table), fleet_line("remote", "-"));
    }
    assert_eq!(
        served(&store, false, "now", &table),
        "ERR usage: reveal takes no words\n"
    );
}

#[test]
fn the_live_table_reads_no_agent_in_this_test() {
    let own = i32::try_from(std::process::id()).unwrap();
    assert!(!Live.agent(own));
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    assert!(Live.pids().contains(&own));
    assert!(in_tree(own, own, &Live));
}

#[test]
fn every_ctl_spawn_answer_marks_its_session_and_a_person_s_tab_is_not_marked() {
    use crate::tab_spawn::TabSpawnThen;
    let mut app = crate::App::headless_for_test();
    let store = app.store.clone();
    let mut sessions = (10u64..).map(|local| register(&store, local, 0));
    let marked = |ctx: &Arc<SessionCtx>| ctx.spawn_mark.read() == SpawnMark::CTL;

    let (sid, person) = sessions.next().unwrap();
    TabSpawnThen::Nothing.answer(&mut app, crate::WindowId(0), Ok(sid));
    assert!(!marked(&person), "a tab a person opened is not ctl spawn's");

    let (sid, wire) = sessions.next().unwrap();
    let (reply, answered) = std::sync::mpsc::channel();
    TabSpawnThen::Reply {
        reply: reply.into(),
        raise: false,
    }
    .answer(&mut app, crate::WindowId(0), Ok(sid.clone()));
    assert_eq!(answered.recv().unwrap(), Ok(sid));
    assert!(marked(&wire), "wire `spawn`");

    let (sid, tab) = sessions.next().unwrap();
    let (reply, answered) = std::sync::mpsc::channel();
    TabSpawnThen::Counts(reply).answer(&mut app, crate::WindowId(0), Ok(sid));
    assert!(answered.recv().unwrap().is_ok());
    assert!(marked(&tab), "wire `tab new`");

    let (sid, worker) = sessions.next().unwrap();
    let (reply, answered) = std::sync::mpsc::channel();
    app.answer_spawn(
        Ok(crate::control::control_media::Spawned::plain_for_test(
            sid.clone(),
        )),
        reply.into(),
    );
    assert_eq!(answered.recv().unwrap(), Ok(sid));
    assert!(marked(&worker), "a booked spawn (a worker's)");

    let (sid, connected) = sessions.next().unwrap();
    let (reply, answered) = std::sync::mpsc::channel();
    assert!(crate::tab_spawn::answer_ctl_spawn(
        &app,
        reply.into(),
        Ok(sid.clone())
    ));
    assert_eq!(answered.recv().unwrap(), Ok(sid));
    assert!(marked(&connected), "`spawn connected=`");

    let (sid, fleet) = sessions.next().unwrap();
    assert_eq!(
        crate::control::control_media::fleet_shell_landed(&store, Ok(sid.clone()), "0e"),
        Ok(sid)
    );
    assert!(marked(&fleet), "a fleet driver's plain shell (`--host`)");

    let (sid, refused) = sessions.next().unwrap();
    let (reply, answered) = std::sync::mpsc::channel();
    TabSpawnThen::Reply {
        reply: reply.into(),
        raise: false,
    }
    .answer(
        &mut app,
        crate::WindowId(0),
        Err(format!("{sid} did not start")),
    );
    assert!(answered.recv().unwrap().is_err());
    assert!(!marked(&refused), "a failed spawn marks nothing");
}

#[test]
fn reveal_is_the_owner_s_and_never_a_fleet_peer_s_or_an_edge_s() {
    use crate::control::{Route, Scope, owner_only_refusal, route};
    use aterm_types::control_request::Selector;
    let form = aterm_types::control_verbs::spec_for("reveal", "").unwrap();
    assert!(matches!(route("reveal", Some(form)), Ok((Route::Owner, _))));
    assert_eq!(owner_only_refusal("reveal", None, Scope::Owner), None);
    assert_eq!(
        owner_only_refusal("reveal", Some(&Selector::SelfTok), Scope::Owner),
        None
    );
    assert!(owner_only_refusal("reveal", Some(&Selector::Local(7)), Scope::Owner).is_some());
    let edge = Scope::Edge(aterm_session::EdgeToken::generate());
    assert!(owner_only_refusal("reveal", None, edge).is_some());
    for content in [false, true] {
        for drive in [false, true] {
            assert!(
                owner_only_refusal("reveal", None, Scope::Fleet { content, drive }).is_some(),
                "content={content} drive={drive}"
            );
        }
    }
}
