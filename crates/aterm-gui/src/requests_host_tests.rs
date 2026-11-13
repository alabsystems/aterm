// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::Scope;
use crate::session_store::{ExitReason, new_store, test_handle};
use aterm_requests::{Out, State, TaskEnd};

/// This test thread's requests in a folder of its own until dropped.
struct Here(aterm_tempfile::TempDir);

impl Here {
    fn new() -> Self {
        let dir = aterm_tempfile::tempdir().expect("a folder");
        set_dir_for_test(Some(dir.path().to_path_buf()));
        Self(dir)
    }

    fn journal(&self) -> String {
        std::fs::read_to_string(self.0.path().join(aterm_requests::JOURNAL)).unwrap_or_default()
    }

    fn store(&self) -> aterm_requests::Store {
        aterm_requests::Store::open(self.0.path()).expect("the store")
    }
}

impl Drop for Here {
    fn drop(&mut self) {
        set_dir_for_test(None);
    }
}

const BY: &str = "fleet:87be5228:s-0123456789abcdef0123";
const NODE: &str = "87be5228c0ffee0087be5228c0ffee00";
/// Another Mac whose node id begins as [`NODE`]'s does: its drive's actor token is the same.
const TWIN: &str = "87be5228ffffffffffffffffffffffff";

/// The drive of `node` as the requester `token` names it.
fn fleet(token: &str, node: &str) -> ExitActor {
    ExitActor::Fleet {
        token: token.to_owned(),
        node: node.to_owned(),
    }
}

fn fleet_asker(by: &str) -> Asker {
    fleet_asker_on(NODE, by)
}

fn fleet_asker_on(node: &str, by: &str) -> Asker {
    Asker {
        fleet: true,
        from: From {
            node: node.to_owned(),
            host: "m114".to_owned(),
            by: by.to_owned(),
            program: Some("claude".to_owned()),
            model: None,
            account: None,
        },
        goal: None,
        asked: None,
    }
}

/// A request `asker` opens with `made`, whose spawn landed tab `sid`: its id.
fn opened(asker: &Asker, made: Made, sid: &str) -> String {
    let prepared =
        prepare(Some(asker), None, made, Some("check the work folder"), None).expect("prepared");
    let (pending, receipt) = hand(begin(prepared).expect("begun"), None);
    pending.expect("a request").settle(Some(sid));
    // A hang detector: the writer thread answers once it wrote the tab.
    receipt
        .expect("a receipt")
        .named()
        .expect("a request named")
}

fn tab_left(sid: &SessionId, reason: ExitReason, code: Option<i32>) -> TabLeft {
    TabLeft {
        sid: sid.clone(),
        reason,
        code,
        task: None,
        by: ExitActor::Unknown,
        spent: None,
    }
}

#[test]
fn a_spawn_from_a_persons_shell_makes_no_request_and_from_an_agent_tab_does() {
    let store = new_store();
    let mut tab = test_handle(1);
    tab.shell_identity.pid = 6999;
    let sid = tab.sid.as_str().to_owned();
    let ctx = std::sync::Arc::clone(&tab.ctx);
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(tab);
    let in_tab = || {
        crate::typed_by::Who::client_with(
            Some(7001),
            1,
            &|pid| match pid {
                7001 => Some(6999),
                _ => None,
            },
            &|_| None,
            &|_| Some("aterm".to_string()),
        )
    };
    // A person's shell: the caller is that tab, and no agent runs in it.
    assert_eq!(asker_as(Scope::Owner, &store, in_tab), None);
    assert_eq!(
        prepare(
            None,
            Some("r-20261109-1411-local-01"),
            Made::Shell,
            None,
            None
        )
        .err(),
        Some("ERR spawn req=r-20261109-1411-local-01: this caller makes no request\n".to_owned())
    );
    {
        let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        let pgid = timeline.agent().program_pgid;
        timeline.set_program(pgid, Some("claude".to_owned()));
        timeline.publish_agent(
            "idle",
            None,
            None,
            Some(aterm_phase::Program::Claude),
            crate::session_timeline::AgentStamp {
                generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
                fp: 1,
            },
        );
    }
    let asker = asker_as(Scope::Owner, &store, in_tab).expect("an agent's tab asks");
    assert!(!asker.fleet);
    assert_eq!(asker.from.by, sid);
    assert_eq!(asker.from.program.as_deref(), Some("claude"));
    assert_eq!(asker.from.host, "this-mac");
    // Control: the same agent tab, asked by a client in no tab, makes none.
    let elsewhere =
        || crate::typed_by::Who::client_with(Some(8001), 1, &|_| None, &|_| None, &|_| None);
    assert_eq!(asker_as(Scope::Owner, &store, elsewhere), None);
    // Its spawn opens a request of this Mac's (`local`), its why the fallback.
    let here = Here::new();
    let worker = test_handle(2);
    let id = opened(&asker, Made::Agent("claude"), worker.sid.as_str());
    assert!(id.contains("-local-"), "{id}");
    let request = here.store().get(&id).cloned().expect("recorded");
    assert_eq!(request.open.via, aterm_requests::Via::CtlSpawn);
    assert_eq!(request.open.by, sid);
    assert_eq!(request.state(), State::Running);
}

#[test]
fn a_drive_turn_attaches_to_its_request_and_a_foreign_drive_does_not() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(1);
    let sid = tab.sid.as_str().to_owned();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(tab);
    let id = opened(&fleet_asker(BY), Made::Shell, &sid);
    let line = format!("@{sid} turn echo hi");
    let head = aterm_types::control_request::head(&line);
    let typed = "OK typed=8\n";
    let before = here.journal();
    // NEGATIVE CONTROL: another node's drive, the same node's other session, and a person
    // all type into the tab, and none of it attaches.
    for stranger in [
        fleet(
            "fleet:c0ffee00:s-0123456789abcdef0123",
            "c0ffee00c0ffee00c0ffee00c0ffee00",
        ),
        fleet("fleet:87be5228:-", NODE),
        fleet(BY, TWIN),
        ExitActor::Human,
        ExitActor::Ctl,
    ] {
        let owned = owned_target(&head, &store).expect("the tab is the request's");
        attach_after(owned, typed, &stranger);
        assert_eq!(here.journal(), before, "{stranger:?}");
    }
    // A turn that typed nothing attaches nothing either.
    let owned = owned_target(&head, &store).expect("the tab is the request's");
    attach_after(owned, "OK skipped reason=busy typed=0\n", &fleet(BY, NODE));
    assert_eq!(here.journal(), before);
    // A verb that does not attach, and a tab no request owns, name nothing.
    let status = format!("@{sid} status");
    assert!(owned_target(&aterm_types::control_request::head(&status), &store).is_none());
    let other = test_handle(2);
    let other_line = format!("@{} turn echo hi", other.sid.as_str());
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(other);
    assert!(owned_target(&aterm_types::control_request::head(&other_line), &store).is_none());
    // The requester's own turn attaches.
    let owned = owned_target(&head, &store).expect("the tab is the request's");
    attach_after(owned, typed, &fleet(BY, NODE));
    assert!(
        here.journal().contains(&format!(
            "op=attach req={id} tab={sid} verb=turn n=1 by={BY}"
        )),
        "{}",
        here.journal()
    );
}

#[test]
fn requests_survive_this_process_forgetting_them() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(1);
    let sid = tab.sid.as_str().to_owned();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(tab);
    let id = opened(&fleet_asker(BY), Made::Shell, &sid);
    // The process an update starts holds no store of its own yet.
    forget_for_test();
    let line = format!("@{sid} close");
    let head = aterm_types::control_request::head(&line);
    let owned = owned_target(&head, &store).expect("read back from the journal");
    attach_after(owned, "OK closed\n", &fleet(BY, NODE));
    let request = here.store().get(&id).cloned().expect("the request");
    assert_eq!(
        request.tabs[0].verbs.get(&Verb::Close).copied(),
        Some(1),
        "{}",
        here.journal()
    );
    // A spawn naming it joins it from here too, and another requester's is refused.
    let joined =
        prepare(Some(&fleet_asker(BY)), Some(&id), Made::Shell, None, None).expect("prepared");
    forget_for_test();
    assert!(begin(joined).is_ok());
    let foreign = prepare(
        Some(&fleet_asker("fleet:87be5228:-")),
        Some(&id),
        Made::Shell,
        None,
        None,
    )
    .expect("prepared");
    assert_eq!(
        begin(foreign).err(),
        Some(format!("ERR spawn req={id}: another requester's request\n"))
    );
    // NEGATIVE CONTROL: the requester's own token from a Mac whose node only begins as its does.
    let twin = prepare(
        Some(&fleet_asker_on(TWIN, BY)),
        Some(&id),
        Made::Shell,
        None,
        None,
    )
    .expect("prepared");
    assert_eq!(
        begin(twin).err(),
        Some(format!("ERR spawn req={id}: another requester's request\n"))
    );
}

#[test]
fn a_spawn_settles_its_request_in_the_order_its_tab_lived() {
    let here = Here::new();
    let asker = fleet_asker(BY);
    let request = || {
        let prepared =
            prepare(Some(&asker), None, Made::Shell, Some("quick"), None).expect("prepared");
        let (pending, receipt) = hand(begin(prepared).expect("begun"), None);
        let receipt = receipt.expect("a receipt");
        (pending.expect("a request"), receipt.id.clone(), receipt)
    };
    // A tab that leaves at once, answered after its control thread stopped waiting: the main
    // thread queues its answer, then its leaving, and the writer keeps that order.
    let quick = test_handle(1);
    let (pending, quick_id, receipt) = request();
    drop(receipt);
    pending.settle(Some(quick.sid.as_str()));
    left(tab_left(&quick.sid, ExitReason::ShellExit, Some(0)));
    // Control: a leaving queued before its tab's answer finds no request to conclude.
    let early = test_handle(2);
    let (pending, early_id, _receipt) = request();
    left(tab_left(&early.sid, ExitReason::ShellExit, Some(0)));
    pending.settle(Some(early.sid.as_str()));
    // A spawn that never reached the main thread is refused, and its reply names no request.
    let (pending, never_id, receipt) = request();
    drop(pending);
    assert_eq!(receipt.named(), None);
    // A hang detector: the writer thread writes them; nothing here waits on time otherwise.
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    let store = here.store();
    let state = |id: &str| store.get(id).map(aterm_requests::Request::state);
    assert_eq!(
        state(&quick_id),
        Some(State::Concluded(Out::Done)),
        "{}",
        here.journal()
    );
    assert_eq!(state(&early_id), Some(State::Running), "{}", here.journal());
    assert_eq!(
        store.get(&never_id).and_then(|request| request.end),
        Some((Out::Failed, aterm_requests::EndWhy::SpawnRefused)),
        "{}",
        here.journal()
    );
}

#[test]
fn a_join_holds_its_request_open_while_its_last_tab_leaves() {
    let here = Here::new();
    let asker = fleet_asker(BY);
    let (first, second) = (test_handle(1), test_handle(2));
    let id = opened(&asker, Made::Shell, first.sid.as_str());
    // The requester joins while its only tab is closing, and the tab leaves before the join's
    // tab lands.
    let joined = prepare(Some(&asker), Some(&id), Made::Shell, None, None).expect("prepared");
    let (pending, receipt) = hand(begin(joined).expect("joined"), None);
    left(tab_left(&first.sid, ExitReason::ShellExit, Some(0)));
    pending
        .expect("a request")
        .settle(Some(second.sid.as_str()));
    // A hang detector: the writer answers once it wrote the tab.
    assert_eq!(receipt.expect("a receipt").named(), Some(id.clone()));
    assert_eq!(
        here.store().get(&id).map(aterm_requests::Request::state),
        Some(State::Running),
        "{}",
        here.journal()
    );
    left(tab_left(&second.sid, ExitReason::ShellExit, Some(0)));
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert_eq!(
        here.store().get(&id).map(aterm_requests::Request::state),
        Some(State::Concluded(Out::Done)),
        "{}",
        here.journal()
    );
}

#[test]
fn a_tab_that_leaves_concludes_its_request_with_its_exit() {
    let here = Here::new();
    let asker = fleet_asker(BY);
    let (worker, failing, closed) = (test_handle(1), test_handle(2), test_handle(3));
    let cases = [
        (
            opened(&asker, Made::Agent("claude"), worker.sid.as_str()),
            TabLeft {
                sid: worker.sid.clone(),
                reason: ExitReason::CtlClose,
                code: None,
                task: Some(TaskEnd::Finished),
                by: fleet(BY, NODE),
                spent: None,
            },
            Out::Done,
        ),
        (
            opened(&asker, Made::Shell, failing.sid.as_str()),
            TabLeft {
                sid: failing.sid.clone(),
                reason: ExitReason::ShellExit,
                code: Some(1),
                task: None,
                by: ExitActor::Unknown,
                spent: None,
            },
            Out::Failed,
        ),
        (
            opened(&asker, Made::Shell, closed.sid.as_str()),
            TabLeft {
                sid: closed.sid.clone(),
                reason: ExitReason::UiClose,
                code: None,
                task: None,
                by: ExitActor::Human,
                spent: None,
            },
            Out::Abandoned,
        ),
    ];
    let mut want = Vec::new();
    for (id, tab_left, out) in cases {
        left(tab_left);
        want.push((id, out));
    }
    // A hang detector: the writer thread writes them; nothing here waits on time otherwise.
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    let store = here.store();
    for (id, out) in want {
        assert_eq!(
            store.get(&id).map(aterm_requests::Request::state),
            Some(State::Concluded(out)),
            "{id}: {}",
            here.journal()
        );
    }
}

/// A lease's look at an idle drive's shell concludes its request and ends the requester's
/// extended authority over the tab. NEGATIVE CONTROL: the same look at a busy shell leaves both.
#[test]
fn a_lease_look_ends_an_idle_drive_shell_and_its_extended_authority() {
    let here = Here::new();
    let store = new_store();
    let tab = test_handle(1);
    let sid = tab.sid.clone();
    store
        .write()
        .unwrap_or_else(|p| p.into_inner())
        .register(tab);
    let id = opened(&fleet_asker(BY), Made::Shell, sid.as_str());
    assert!(crate::control::record_driven_shell(
        &store,
        sid.as_str(),
        NODE
    ));
    let look = |idle| aterm_requests::Look {
        idle_after_ms: 0,
        tabs: [(
            sid.as_str().to_owned(),
            aterm_requests::TabNow {
                idle,
                quiet_since_ms: None,
            },
        )]
        .into(),
        answered: None,
    };
    let at = now_unix_ms() + 1;
    lease_look(at, look(false));
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert_eq!(
        here.store().get(&id).map(aterm_requests::Request::state),
        Some(State::Running),
        "{}",
        here.journal()
    );
    assert_eq!(
        crate::control::drive_spawned_by(&sid).as_deref(),
        Some(NODE)
    );
    lease_look(at, look(true));
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert_eq!(
        here.store().get(&id).and_then(|request| request.end),
        Some((Out::Abandoned, aterm_requests::EndWhy::Idle)),
        "{}",
        here.journal()
    );
    assert_eq!(
        placed(sid.as_str(), None).and_then(|placed| placed.end),
        Some((Out::Abandoned, aterm_requests::EndWhy::Idle))
    );
    assert_eq!(crate::control::drive_spawned_by(&sid), None);
}

/// Keep as Tab records a keep on the request that owns the tab, which concludes released.
/// NEGATIVE CONTROL: a person's own tab, which no request owns, records nothing.
#[test]
fn kept_records_a_keep_on_its_request_and_none_on_a_persons_tab() {
    let here = Here::new();
    let (driven, persons) = (test_handle(1), test_handle(2));
    let id = opened(&fleet_asker(BY), Made::Shell, driven.sid.as_str());
    let before = here.journal();
    kept(vec![persons.sid.as_str().to_owned()], &ExitActor::Human);
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert_eq!(here.journal(), before);
    kept(vec![driven.sid.as_str().to_owned()], &ExitActor::Human);
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert!(
        here.journal().contains(&format!(
            "op=keep req={id} tab={} by=human",
            driven.sid.as_str()
        )),
        "{}",
        here.journal()
    );
    assert_eq!(
        here.store().get(&id).and_then(|request| request.end),
        Some((Out::Released, aterm_requests::EndWhy::Kept))
    );
}

/// The next process's sweep concludes a request whose tab it does not host. NEGATIVE CONTROLS:
/// one whose tab it hosts stays running, and a process not alone on its state home sweeps
/// nothing and may ask again.
#[test]
fn a_relaunch_concludes_requests_whose_tabs_are_gone() {
    let here = Here::new();
    let tab = test_handle(1);
    let id = opened(&fleet_asker(BY), Made::Shell, tab.sid.as_str());
    // The process a crash's relaunch starts holds no store or index of its own yet.
    forget_for_test();
    let born = now_unix_ms() + 1;
    let ask = |hosted: BTreeSet<String>, alone: fn() -> bool, swept: &Arc<AtomicBool>| Sweep {
        at: born,
        born_ms: born,
        hosted,
        alone,
        swept: Arc::clone(swept),
    };
    let hosting = Arc::new(AtomicBool::new(false));
    sweep(ask([tab.sid.as_str().to_owned()].into(), || true, &hosting));
    let held = Arc::new(AtomicBool::new(false));
    sweep(ask(BTreeSet::new(), || false, &held));
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert_eq!(
        here.store().get(&id).map(aterm_requests::Request::state),
        Some(State::Running)
    );
    assert!(hosting.load(Ordering::Acquire), "it swept");
    assert!(!held.load(Ordering::Acquire), "not alone, it did not");
    sweep(ask(BTreeSet::new(), || true, &held));
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert_eq!(
        here.store().get(&id).and_then(|request| request.end),
        Some((Out::Abandoned, aterm_requests::EndWhy::Relaunch)),
        "{}",
        here.journal()
    );
    assert_eq!(
        placed(tab.sid.as_str(), None).and_then(|placed| placed.end),
        Some((Out::Abandoned, aterm_requests::EndWhy::Relaunch))
    );
}

/// The index finds a request's tab by its sid, and a shell a cold restore stood in for it by its
/// lineage; a process that lost its index reads it back from the journal. NEGATIVE CONTROL: the
/// new shell's sid alone names no request.
#[test]
fn placed_finds_a_restored_shell_by_its_lineage() {
    let _here = Here::new();
    let (old, new) = (test_handle(1), test_handle(2));
    opened(&fleet_asker(BY), Made::Shell, old.sid.as_str());
    let (old, new) = (old.sid.as_str(), new.sid.as_str());
    let found = placed(new, Some(old)).expect("by its lineage");
    assert_eq!(found.via, aterm_requests::Via::FleetShell);
    assert_eq!((found.node.as_str(), found.host.as_str()), (NODE, "m114"));
    assert!(placed(new, None).is_none());
    forget_for_test();
    assert!(
        placed(old, None).is_none(),
        "PRECONDITION: the index is gone"
    );
    load();
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    assert!(placed(old, None).is_some(), "read back");
}

#[test]
fn listed_counts_unreviewed_newest_first_with_the_fallback_why() {
    let here = Here::new();
    let (shell, worker) = (test_handle(1), test_handle(2));
    let fleet_id = opened(&fleet_asker(BY), Made::Shell, shell.sid.as_str());
    let local = Asker {
        fleet: false,
        from: From {
            node: String::new(),
            host: "this-mac".to_owned(),
            by: "s-0123456789abcdef0123".to_owned(),
            program: Some("claude".to_owned()),
            model: None,
            account: None,
        },
        goal: Some("a goal the content tier keeps".to_owned()),
        asked: None,
    };
    let prepared =
        prepare(Some(&local), None, Made::Agent("claude"), None, None).expect("prepared");
    let (pending, receipt) = hand(begin(prepared).expect("begun"), None);
    pending
        .expect("a request")
        .settle(Some(worker.sid.as_str()));
    let local_id = receipt.expect("a receipt").named().expect("recorded");
    left(tab_left(&shell.sid, ExitReason::ShellExit, Some(0)));
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    let first = listed().expect("the writer listed the store");
    let ids: Vec<&str> = first.rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, [local_id.as_str(), fleet_id.as_str()], "newest first");
    assert_eq!(first.unreviewed, 1);
    let (local_row, fleet_row) = (&first.rows[0], &first.rows[1]);
    assert_eq!(&*local_row.why, "a goal the content tier keeps");
    assert_eq!(local_row.why_src, WhySrc::Goal);
    assert_eq!(local_row.stage, Stage::Running);
    assert_eq!(
        (fleet_row.stage, fleet_row.out, fleet_row.open_tabs),
        (Stage::Concluded, Some(Out::Done), 0)
    );
    assert_eq!(&*fleet_row.why, "check the work folder");
    // A review written on a control thread is listed once the writer lists again.
    let dir = here.0.path().to_path_buf();
    with_store(&dir, |store| {
        store
            .review(now_unix_ms(), &fleet_id, "ctl", None)
            .map_err(|refusal| std::io::Error::other(refusal.to_string()))
    })
    .expect("reviewed");
    // NEGATIVE: until the writer lists again, the index still counts it.
    assert_eq!(listed().expect("listed").unreviewed, 1);
    relisted(&dir);
    assert!(flush_within(Duration::from_secs(60)), "the writer flushed");
    let again = listed().expect("listed");
    assert_eq!(again.unreviewed, 0);
    assert_eq!(again.rows[1].stage, Stage::Reviewed);
}
