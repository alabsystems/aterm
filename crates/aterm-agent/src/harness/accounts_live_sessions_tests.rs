// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the session table's reconcile with the sessions that live.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use super::super::{Canary, Undo, WallLadder, WallWaiter, note_session, note_wall_wait};
use super::*;

/// 2026-11-16T00:00:00Z.
const NOW: i64 = 1_794_787_200;
const HOUR: i64 = 3600;
const DAY: i64 = 24 * HOUR;

/// The reconciling window in these tests.
const ME: Owner = Owner {
    pid: 4242,
    birth: 1_789_000_000_000_001,
};
/// Another window of the same login, still running.
const PEER: Owner = Owner {
    pid: 4343,
    birth: 1_789_000_000_000_002,
};
/// A window that ended (crashed, killed, replaced by an update).
const ENDED: Owner = Owner {
    pid: 4444,
    birth: 1_789_000_000_000_003,
};
/// A window whose start cannot be read ([`Owner::runs`] `None`).
const ME_UNREAD: Owner = Owner {
    pid: 4545,
    birth: 1_789_000_000_000_004,
};

fn row(account: &str, at: i64, owner: Option<Owner>) -> SessionRow {
    SessionRow {
        account: account.into(),
        at,
        held: None,
        owner,
    }
}

fn state(rows: &[(&str, SessionRow)]) -> State {
    State {
        machine: "0a0b".into(),
        sessions: rows
            .iter()
            .map(|(sid, row)| ((*sid).to_string(), row.clone()))
            .collect(),
        ..State::default()
    }
}

/// What a reconcile is told: the sessions `held` this window holds for the pool, `tabs` the other
/// tabs it has, `PEER` runs and `ENDED` does not, and the graph names another live instance
/// serving each of `served`, this window serving each of `here`, and cannot read `unread`.
struct World {
    held: BTreeSet<&'static str>,
    tabs: BTreeSet<&'static str>,
    served: BTreeSet<&'static str>,
    here: BTreeSet<&'static str>,
    unread: BTreeSet<&'static str>,
    asked: RefCell<Vec<String>>,
}

impl World {
    fn new(held: &[&'static str], served: &[&'static str]) -> World {
        World {
            held: held.iter().copied().collect(),
            tabs: BTreeSet::new(),
            served: served.iter().copied().collect(),
            here: BTreeSet::new(),
            unread: BTreeSet::new(),
            asked: RefCell::default(),
        }
    }

    /// The reconcile with every session over due ([`OverReads`] aside).
    fn reconcile(&self, state: &mut State) -> SessionsReconciled {
        self.reconcile_due(state, &|_| true)
    }

    fn reconcile_due(&self, state: &mut State, due: &dyn Fn(&str) -> bool) -> SessionsReconciled {
        let holds = |sid: &str| self.held.contains(sid);
        let tab = |sid: &str| self.held.contains(sid) || self.tabs.contains(sid);
        let runs = |owner: Owner| match owner {
            ME | PEER => Some(true),
            ENDED => Some(false),
            _ => None,
        };
        let served = |sid: &str| {
            self.asked.borrow_mut().push(sid.to_string());
            if self.unread.contains(sid) {
                Liveness::Unknown
            } else if self.served.contains(sid) {
                Liveness::Elsewhere
            } else if self.here.contains(sid) {
                Liveness::Here
            } else {
                Liveness::Gone
            }
        };
        reconcile_sessions(
            state,
            &LiveSessions {
                me: ME,
                read_at: NOW,
                holds: &holds,
                tab: &tab,
                runs: &runs,
                served: &served,
            },
            due,
        )
    }
}

fn sids(state: &State) -> Vec<&str> {
    state.sessions.keys().map(String::as_str).collect()
}

#[test]
fn rows_a_build_before_owners_left_go_and_the_live_session_stays() {
    // As read on a Mac on 2026-11-16: three rows a build before owners noted five days earlier,
    // none of a session any instance lists, and the one live session's.
    let stale = NOW - 5 * DAY + 11 * HOUR;
    let mut state = state(&[
        ("s-00000000000000000a01", row("ada", stale, None)),
        ("s-00000000000000000a02", row("ada", stale, None)),
        ("s-00000000000000000a03", row("ada", stale, None)),
        ("s-00000000000000000b01", row("ada-2", NOW - 60, None)),
    ]);
    let world = World::new(&["s-00000000000000000b01"], &[]);
    let done = world.reconcile(&mut state);
    assert_eq!(sids(&state), ["s-00000000000000000b01"]);
    assert_eq!(
        done.forgot,
        [
            "s-00000000000000000a01",
            "s-00000000000000000a02",
            "s-00000000000000000a03"
        ]
    );
    assert_eq!(done.over, done.forgot);
    assert_eq!(done.adopted, ["s-00000000000000000b01"]);
    assert_eq!(
        state.sessions["s-00000000000000000b01"].owner,
        Some(ME),
        "the window that holds it owns its row from now"
    );
    assert_eq!(
        state.sessions["s-00000000000000000b01"].at,
        NOW - 60,
        "taking a row over leaves when it was read"
    );
}

#[test]
fn a_held_session_stays_whatever_its_row_says_and_no_graph_is_asked() {
    let old = NOW - 40 * DAY;
    let mut state = state(&[
        ("t-mine", row("a", old, Some(ME))),
        ("t-handed", row("a", old, Some(ENDED))),
        ("t-peer", row("a", old, Some(PEER))),
        ("t-legacy", row("a", old, None)),
    ]);
    let world = World::new(&["t-mine", "t-handed", "t-peer", "t-legacy"], &[]);
    let done = world.reconcile(&mut state);
    assert_eq!(sids(&state), ["t-handed", "t-legacy", "t-mine", "t-peer"]);
    assert!(done.forgot.is_empty() && done.over.is_empty());
    assert_eq!(done.adopted, ["t-handed", "t-legacy", "t-peer"]);
    assert!(state.sessions.values().all(|row| row.owner == Some(ME)));
    assert!(world.asked.borrow().is_empty());
}

#[test]
fn an_own_row_goes_with_its_tab_but_not_one_noted_since_the_tabs_were_read() {
    let mut state = state(&[
        ("t-closed", row("a", NOW - 1, Some(ME))),
        ("t-new", row("a", NOW, Some(ME))),
        ("t-newer", row("a", NOW + 5, Some(ME))),
    ]);
    let mut world = World::new(&[], &[]);
    world.here.insert("t-closed");
    let done = world.reconcile(&mut state);
    assert_eq!(
        done.forgot,
        ["t-closed"],
        "an entry still naming this window is one its retirement has not reached"
    );
    assert_eq!(
        sids(&state),
        ["t-new", "t-newer"],
        "a row noted in the read's second or later may be of a tab the read did not see"
    );
}

#[test]
fn an_own_row_the_graph_says_another_instance_serves_stays() {
    // A seamless update's successor serves it; an unreadable entry says nothing.
    let mut state = state(&[
        ("t-handed", row("a", NOW - HOUR, Some(ME))),
        ("t-unread", row("a", NOW - HOUR, Some(ME))),
    ]);
    let mut world = World::new(&[], &["t-handed"]);
    world.unread.insert("t-unread");
    let done = world.reconcile(&mut state);
    assert!(done.is_empty() && done.over.is_empty());
    assert_eq!(sids(&state), ["t-handed", "t-unread"]);
}

#[test]
fn a_row_another_running_window_owns_is_never_touched() {
    let mut state = state(&[
        ("t-peer", row("a", NOW - 40 * DAY, Some(PEER))),
        ("t-unread", row("a", NOW - 40 * DAY, Some(ME_UNREAD))),
    ]);
    let world = World::new(&[], &[]);
    assert!(world.reconcile(&mut state).is_empty());
    assert_eq!(sids(&state), ["t-peer", "t-unread"]);
    assert!(
        world.asked.borrow().is_empty(),
        "its own window answers for it: no graph is asked"
    );
}

#[test]
fn a_row_of_an_ended_window_or_no_owner_goes_only_where_no_instance_serves_it() {
    let mut state = state(&[
        ("t-ended", row("a", NOW - HOUR, Some(ENDED))),
        ("t-ended-served", row("a", NOW - HOUR, Some(ENDED))),
        ("t-ended-here", row("a", NOW - HOUR, Some(ENDED))),
        ("t-ended-unread", row("a", NOW - HOUR, Some(ENDED))),
        ("t-legacy", row("a", NOW - HOUR, None)),
        ("t-legacy-served", row("a", NOW - HOUR, None)),
        ("t-legacy-here", row("a", NOW - HOUR, None)),
    ]);
    let mut world = World::new(&[], &["t-ended-served", "t-legacy-served"]);
    world.unread.insert("t-ended-unread");
    world.here.extend(["t-ended-here", "t-legacy-here"]);
    let done = world.reconcile(&mut state);
    assert_eq!(done.forgot, ["t-ended", "t-legacy"]);
    assert!(
        done.adopted.is_empty(),
        "only the window that holds it takes a row"
    );
    assert_eq!(
        sids(&state),
        [
            "t-ended-here",
            "t-ended-served",
            "t-ended-unread",
            "t-legacy-here",
            "t-legacy-served"
        ],
        "an entry naming this window is of a tab it took since it read its tabs (a relaunch \
         adopting a crashed window's sessions)"
    );
}

#[test]
fn a_tab_of_this_window_running_no_agent_of_the_pool_is_over_whoever_noted_its_row() {
    // A tab whose agent ended while no host watched (the window was dead, its host suspended or
    // off) and that runs a shell or another CLI now: this window answers for it.
    let old = NOW - HOUR;
    let mut state = state(&[
        ("t-mine", row("a", old, Some(ME))),
        ("t-ended", row("a", old, Some(ENDED))),
        ("t-peer", row("a", old, Some(PEER))),
        ("t-legacy", row("a", old, None)),
        ("t-served", row("a", old, None)),
        ("t-unread", row("a", old, None)),
        ("t-agent", row("a", old, Some(ENDED))),
    ]);
    let mut world = World::new(&["t-agent"], &["t-served"]);
    world.unread.insert("t-unread");
    world.tabs.extend([
        "t-mine", "t-ended", "t-peer", "t-legacy", "t-served", "t-unread",
    ]);
    world
        .here
        .extend(["t-mine", "t-ended", "t-peer", "t-legacy", "t-unread"]);
    let done = world.reconcile(&mut state);
    assert_eq!(done.forgot, ["t-ended", "t-legacy", "t-mine", "t-peer"]);
    assert_eq!(
        done.adopted,
        ["t-agent"],
        "a tab that runs one holds its row"
    );
    assert_eq!(sids(&state), ["t-agent", "t-served", "t-unread"]);
}

#[test]
fn a_forgotten_session_takes_its_canary_hold_and_its_wall_wait_with_it() {
    let mut state = state(&[("t-gone", row("a", NOW - HOUR, None))]);
    state.active = Some("a".into());
    state.canary = Some(Canary {
        sid: "t-gone".into(),
        at: NOW - 60,
        took: false,
        native_receipt: None,
    });
    assert!(note_wall_wait(&mut state, "a", "t-gone", NOW - 60));
    assert!(state.canary_holds(NOW));
    let done = World::new(&[], &[]).reconcile(&mut state);
    assert_eq!(done.forgot, ["t-gone"]);
    assert!(state.sessions.is_empty());
    assert_eq!(state.canary, None);
    assert!(
        state.wall_reads.is_empty(),
        "a ladder no session waits on goes"
    );
}

#[test]
fn a_wall_wait_or_canary_with_no_row_is_reconciled_as_its_session_is() {
    let waiting = |since| WallWaiter { since, seen: since };
    let mut state = state(&[]);
    state.wall_reads.insert(
        "a".into(),
        WallLadder {
            since: NOW - HOUR,
            read: None,
            waiters: BTreeMap::from([
                ("t-dead".to_string(), waiting(NOW - HOUR)),
                ("t-held".to_string(), waiting(NOW - HOUR)),
                ("t-served".to_string(), waiting(NOW - HOUR)),
            ]),
        },
    );
    state.canary = Some(Canary {
        sid: "t-dead-canary".into(),
        at: NOW - 60,
        took: false,
        native_receipt: None,
    });
    let done = World::new(&["t-held"], &["t-served"]).reconcile(&mut state);
    assert_eq!(done.forgot, ["t-dead", "t-dead-canary"]);
    assert!(
        done.adopted.is_empty(),
        "a session with no row has no row to take"
    );
    let waiters: Vec<&str> = state.wall_reads["a"]
        .waiters
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(waiters, ["t-held", "t-served"]);
    assert_eq!(state.canary, None);
}

#[test]
fn an_undo_canary_with_no_row_is_reconciled_as_its_session_is() {
    let canary = |sid: &str| Canary {
        sid: sid.into(),
        at: NOW - HOUR,
        took: false,
        native_receipt: None,
    };
    let undo = |sid: &str| Undo {
        previous: Some("a".into()),
        previous_proven: true,
        canary: Some(canary(sid)),
    };
    let seeded = {
        let mut state = state(&[]);
        state.undo = vec![undo("t-older"), undo("t-dead"), undo("t-newer")];
        state
    };
    let mut held = seeded.clone();
    let world = World::new(&["t-dead", "t-older", "t-newer"], &[]);
    assert!(world.reconcile(&mut held).is_empty());
    assert_eq!(held, seeded, "an undo whose canary is held stays");

    let mut over = seeded.clone();
    let world = World::new(&["t-older", "t-newer"], &[]);
    let done = world.reconcile(&mut over);
    assert_eq!(done.forgot, ["t-dead"]);
    let mut forgotten = seeded.clone();
    assert!(forget_session(&mut forgotten, "t-dead"));
    assert_eq!(
        over.undo, forgotten.undo,
        "the undo stack is drained as an ended session's is"
    );
    assert_eq!(
        over.undo.first().and_then(|first| first.canary.as_ref()),
        None,
        "nothing older than the dead canary's move can be gone back to"
    );
}

#[test]
fn a_session_over_goes_only_where_its_grace_is_out() {
    let mut state = state(&[
        ("t-due", row("a", NOW - HOUR, None)),
        ("t-waits", row("a", NOW - HOUR, None)),
    ]);
    let done = World::new(&[], &[]).reconcile_due(&mut state, &|sid| sid == "t-due");
    assert_eq!(done.forgot, ["t-due"]);
    assert_eq!(done.over, ["t-due", "t-waits"]);
    assert_eq!(sids(&state), ["t-waits"]);
}

#[test]
fn a_session_read_over_keeps_its_first_mark_until_a_read_finds_it_live() {
    let mut marks = OverReads::default();
    let over = |sids: &[&str]| -> Vec<String> { sids.iter().map(|s| s.to_string()).collect() };
    marks.note(&over(&["t-a", "t-b"]), NOW);
    assert!(!marks.due("t-a", NOW + SESSION_OVER_GRACE_S - 1));
    marks.note(&over(&["t-a"]), NOW + 60);
    assert!(
        marks.due("t-a", NOW + SESSION_OVER_GRACE_S),
        "a session over at reads its grace apart goes"
    );
    marks.note(&over(&["t-a", "t-b"]), NOW + 120);
    assert!(
        !marks.due("t-b", NOW + SESSION_OVER_GRACE_S + 60),
        "a read that found it live started its grace again"
    );
    assert!(marks.due("t-b", NOW + 120 + SESSION_OVER_GRACE_S));
    assert!(!marks.due("t-never", NOW + 10 * DAY));
    marks.note(&over(&["t-c"]), NOW + DAY);
    marks.note(&over(&["t-c"]), NOW);
    assert!(!marks.due("t-c", NOW + SESSION_OVER_GRACE_S - 1));
    assert!(
        marks.due("t-c", NOW + SESSION_OVER_GRACE_S),
        "a mark ahead of a clock that stepped back is made again at the clock"
    );
    marks.clear();
    assert!(marks.is_empty());
}

#[test]
fn a_noted_row_is_this_process_s_and_its_owner_survives_the_state_file() {
    let here = Owner::here().expect("this process's start is read");
    let mut noted = state(&[]);
    note_session(&mut noted, "t-one", "a", Some("jobs"), NOW);
    noted
        .sessions
        .insert("t-legacy".into(), row("a", NOW - 40 * DAY, None));
    assert_eq!(noted.sessions["t-one"].owner, Some(here));
    let json = noted.to_json();
    let read = State::from_json(&json, NOW).expect("ours");
    assert_eq!(read.sessions, noted.sessions);
    assert_eq!(
        read.sessions["t-legacy"].owner, None,
        "a row a build before owners wrote names none"
    );
    let bad = json.replace(&here.token(), "07@1");
    let read = State::from_json(&bad, NOW).expect("ours");
    assert_eq!(
        read.sessions["t-one"].owner, None,
        "a word that is no owner names none"
    );
}

#[test]
fn an_owner_word_reads_back_exactly() {
    assert_eq!(Owner::parse(&ME.token()), Some(ME));
    for word in [
        "", "@", "1@", "@1", "0@1", "01@1", "1@01", "+1@1", "1@1@1", "1 @1", "x@1",
    ] {
        assert_eq!(Owner::parse(word), None, "{word:?}");
    }
}

#[test]
fn this_process_runs_and_its_pid_under_another_start_does_not() {
    let here = Owner::here().expect("this process's start is read");
    assert_eq!(here.pid, std::process::id());
    assert_eq!(here.runs(), Some(true));
    let reused = Owner {
        birth: here.birth + 1,
        ..here
    };
    assert_eq!(
        reused.runs(),
        Some(false),
        "another process given the same pid"
    );
}

#[cfg(unix)]
#[test]
fn a_process_that_ended_does_not_run() {
    let mut child = aterm_uds::spawnfd::command(crate::test_program::kept("/bin/sleep"))
        .arg("120")
        .spawn()
        .expect("spawns");
    let owner = Owner::of(child.id()).expect("its start is read");
    assert_eq!(owner.runs(), Some(true));
    child.kill().expect("killed");
    child.wait().expect("reaped");
    assert_eq!(owner.runs(), Some(false));
}

fn tmp(tag: &str) -> aterm_tempfile::TempDir {
    aterm_tempfile::Builder::new()
        .prefix(&format!("aterm-harness-live-sessions-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

fn live<'a>(
    holds: &'a dyn Fn(&str) -> bool,
    served: &'a dyn Fn(&str) -> Liveness,
) -> LiveSessions<'a> {
    LiveSessions {
        me: ME,
        read_at: NOW,
        holds,
        tab: holds,
        runs: &|owner| Some(owner == ME || owner == PEER),
        served,
    }
}

/// [`record_reconciled`] with a mark already its grace old on every session `over`.
fn reconciled_due(
    store: &Store,
    live: &LiveSessions<'_>,
    over: &[&str],
) -> Result<SessionsReconciled, String> {
    let mut marks = OverReads::default();
    let over: Vec<String> = over.iter().map(|s| s.to_string()).collect();
    marks.note(&over, NOW - SESSION_OVER_GRACE_S);
    record_reconciled(store, live, &mut marks, NOW)
}

#[test]
fn a_store_with_no_state_is_left_as_it_is() {
    let dir = tmp("absent");
    let store = Store::at(dir.path());
    let mut marks = OverReads::default();
    marks.note(&["t-gone".to_string()], NOW - HOUR);
    let done = record_reconciled(
        &store,
        &live(&|_| false, &|_| Liveness::Gone),
        &mut marks,
        NOW,
    );
    assert_eq!(done, Ok(SessionsReconciled::default()));
    assert!(!store.dir.exists(), "nothing is created");
    assert!(marks.is_empty(), "a table with no state names nothing over");
}

#[test]
fn a_store_whose_sessions_all_live_is_neither_written_nor_locked() {
    static SHORT: [Duration; 1] = [Duration::from_millis(20)];
    let dir = tmp("unchanged");
    let store = Store::at(dir.path()).bounded_by(&SHORT);
    let seeded = state(&[
        ("t-mine", row("a", NOW - HOUR, Some(ME))),
        ("t-peer", row("a", NOW - HOUR, Some(PEER))),
    ]);
    store.save(&seeded).expect("saves");
    let before = std::fs::read(store.state_file()).expect("reads");
    let held = store.acquire().expect("the lock is taken");
    let done = reconciled_due(
        &store,
        &live(&|sid| sid == "t-mine", &|_| Liveness::Gone),
        &[],
    );
    assert_eq!(
        done,
        Ok(SessionsReconciled::default()),
        "a reconcile that changes nothing waits on no lock"
    );
    drop(held);
    assert_eq!(std::fs::read(store.state_file()).expect("reads"), before);
}

#[test]
fn a_store_marks_a_session_over_at_the_first_read_and_forgets_it_once_its_grace_is_out() {
    let dir = tmp("grace");
    let store = Store::at(dir.path());
    store
        .save(&state(&[("t-ended", row("a", NOW - HOUR, Some(ENDED)))]))
        .expect("saves");
    let before = std::fs::read(store.state_file()).expect("reads");
    let world = live(&|_| false, &|_| Liveness::Gone);
    let mut marks = OverReads::default();
    let first = record_reconciled(&store, &world, &mut marks, NOW).expect("reconciles");
    assert_eq!(first.over, ["t-ended"]);
    assert!(first.forgot.is_empty(), "one read ends nothing");
    assert_eq!(std::fs::read(store.state_file()).expect("reads"), before);
    let early = record_reconciled(&store, &world, &mut marks, NOW + SESSION_OVER_GRACE_S - 1)
        .expect("reconciles");
    assert!(early.forgot.is_empty());
    let late = record_reconciled(&store, &world, &mut marks, NOW + SESSION_OVER_GRACE_S)
        .expect("reconciles");
    assert_eq!(late.forgot, ["t-ended"]);
    assert!(marks.is_empty(), "a session forgotten keeps no mark");
    let (state, _) = store.load(NOW).expect("ours");
    assert!(state.sessions.is_empty());
}

#[test]
fn a_store_forgets_its_ended_sessions_and_wakes_the_sessions_a_canary_held() {
    let dir = tmp("forgets");
    let store = Store::at(dir.path());
    let mut seeded = state(&[
        ("t-ended", row("a", NOW - HOUR, Some(ENDED))),
        ("t-handed", row("a", NOW - HOUR, None)),
        ("t-peer", row("a", NOW - HOUR, Some(PEER))),
    ]);
    seeded.active = Some("a".into());
    seeded.canary = Some(Canary {
        sid: "t-ended".into(),
        at: NOW - 60,
        took: false,
        native_receipt: None,
    });
    store.save(&seeded).expect("saves");
    let done = reconciled_due(
        &store,
        &live(&|sid| sid == "t-handed", &|_| Liveness::Gone),
        &["t-ended"],
    )
    .expect("reconciles");
    assert_eq!(done.forgot, ["t-ended"]);
    assert_eq!(done.adopted, ["t-handed"]);
    let (state, _) = store.load(NOW).expect("ours");
    assert_eq!(sids(&state), ["t-handed", "t-peer"]);
    assert_eq!(state.sessions["t-handed"].owner, Some(ME));
    assert_eq!(state.canary, None);
    assert!(store.wake_path().exists(), "the canary's release is told");
}

#[test]
fn a_store_that_forgets_no_canary_wakes_nobody() {
    let dir = tmp("no-wake");
    let store = Store::at(dir.path());
    let mut seeded = state(&[
        ("t-ended", row("a", NOW - HOUR, Some(ENDED))),
        ("t-canary", row("a", NOW - HOUR, Some(ME))),
    ]);
    seeded.active = Some("a".into());
    seeded.canary = Some(Canary {
        sid: "t-canary".into(),
        at: NOW - 60,
        took: false,
        native_receipt: None,
    });
    store.save(&seeded).expect("saves");
    let done = reconciled_due(
        &store,
        &live(&|sid| sid == "t-canary", &|_| Liveness::Gone),
        &["t-ended"],
    )
    .expect("reconciles");
    assert_eq!(done.forgot, ["t-ended"]);
    let (state, _) = store.load(NOW).expect("ours");
    assert!(state.canary_holds(NOW), "the canary still holds");
    assert!(
        !store.wake_path().exists(),
        "an ended session that held no canary wakes no waiting tab"
    );
}

#[test]
fn only_a_row_no_running_process_answers_for_ages_out_when_the_state_is_saved() {
    let dir = tmp("any-age");
    let store = Store::at(dir.path());
    let here = Owner::here().expect("this process's start is read");
    let reused = Owner {
        birth: here.birth + 1,
        ..here
    };
    let old = NOW - SESSION_KEEP_S - 1;
    let mut seeded = state(&[
        ("t-legacy", row("a", old, None)),
        ("t-ended", row("a", old, Some(reused))),
        ("t-edge", row("a", NOW - SESSION_KEEP_S, None)),
    ]);
    note_session(&mut seeded, "t-idle", "a", Some("jobs"), NOW - 400 * DAY);
    note_session(&mut seeded, "t-ahead", "a", None, NOW + DAY);
    store.save(&seeded).expect("saves");
    let keys = |store: &Store| {
        let (state, _) = store.load(NOW).expect("ours");
        state.sessions.keys().cloned().collect::<Vec<_>>()
    };
    store
        .transact(NOW, &mut None, |_| Ok(Vec::new()))
        .expect("reads");
    assert_eq!(
        keys(&store),
        ["t-ahead", "t-edge", "t-ended", "t-idle", "t-legacy"],
        "a transaction that writes nothing ages nothing out"
    );
    store
        .transact(NOW, &mut None, |state| {
            Ok(note_session(state, "t-two", "a", None, NOW))
        })
        .expect("writes");
    assert_eq!(
        keys(&store),
        ["t-ahead", "t-edge", "t-idle", "t-two"],
        "an idle session whose window runs keeps a row of any age; one no window answers for \
         goes past thirty days"
    );
    let (state, _) = store.load(NOW).expect("ours");
    assert_eq!(state.sessions["t-idle"], seeded.sessions["t-idle"]);
}
