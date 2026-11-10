// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `AlabJournal` conformance (docs/DESIGN-alab-lab-model-2026-11-01.md §11 step 12): from every
//! state the model reaches, each step the publisher takes is a model successor. The publisher's
//! step is `flush::pass` less its Commons reads and writes and its pace: the window's tables read
//! back (`flush::read_window`), what it relays (`flush::relayable`), one record packed
//! (`Book::plan`) and placed in a fixed share (`ring::landing`), and what the outbox and book
//! keep (`flush::settle`); a week passing is the ring's ratchet. The window's tables are typed
//! operations held here, one writer's, its share one record a table, so a spill, a full window
//! and a deleted week are each a few steps away. A Commons run of one path (below) binds the real
//! append, `Store::commit_own`, to the same steps. Every address is synthetic.

use super::{
    APPLICATION, Event, Kind, Pk, Presence, ROLE, Source, Word,
    book::{self, Book, Planned, WEEK},
    flush,
    relay::Rk,
};
use crate::{
    catalog::ring::{self, JOURNAL, Kept, RingState, Shares, Used},
    database::Command,
    typed::{self, keys, owned::Op},
};
use astream_wire::sync::{Actor, Digest};
use aterm_spec::interp::State;
use std::collections::{BTreeMap, BTreeSet};

/// The week the walk starts in.
const W: u64 = 3_000;
/// This Mac, and a reader that is not it.
const ME: [u8; 16] = [0x11; 16];
const READER: [u8; 16] = [0x99; 16];
/// The model's share: one record a table for this writer, the tables themselves roomy.
const SHARES: Shares = Shares {
    records: 1_000,
    bytes: usize::MAX,
    writer_records: 1,
    writer_bytes: usize::MAX,
};

fn account() -> Pk {
    Pk::new(&keys::account_pk("claude", "ann@example.test").unwrap()).unwrap()
}

/// A use of the account at `t`.
fn used_at(t: u64) -> Event {
    Event {
        t,
        kind: Kind::SessionStart,
        account: Some(account()),
        cli: Word::new("claude").unwrap(),
        sub: None,
        window: None,
        pct_x10: None,
        reset: None,
        res: None,
        until: None,
        plan: None,
        src: Source::CliOutput,
        why: None,
        to: None,
        tab: None,
        harness: None,
    }
}

/// The publisher's state behind one model state: its clock, ring, outbox and book, and the
/// window tables' records, by week.
pub(crate) struct Run {
    pub now: u64,
    pub ring: RingState,
    pub tables: BTreeMap<u64, Vec<Op>>,
    pub outbox: Vec<Event>,
    pub book: Book,
    sequence: u64,
}

impl Run {
    fn new() -> Self {
        Self {
            now: W * WEEK + 3600,
            ring: RingState {
                floor: 0,
                held: BTreeSet::new(),
            },
            tables: BTreeMap::new(),
            outbox: Vec::new(),
            book: Book::default(),
            sequence: 0,
        }
    }

    fn current(&self) -> u64 {
        self.ring.current(&JOURNAL, self.now)
    }

    /// Whether week `week`'s table alone tells a reader the account's last use.
    fn believed_in(&self, week: u64) -> bool {
        let ops = self.tables.get(&week).cloned().unwrap_or_default();
        let resolve = |_: &Digest, _: &Actor| Some(ME);
        let heard = flush::read_window(&resolve, &ops, &READER, &Book::default()).heard;
        heard.newest.contains_key(&(account(), Rk::Use))
    }
}

/// One planned record and where its append went.
pub(crate) struct Flush {
    pub planned: Planned,
    pub payload: Vec<u8>,
    pub appended: std::result::Result<u64, Kept>,
}

/// The publisher's flush on `run`, as `flush::pass` makes it once a flush is due: `None` where
/// nothing is due (no record planned).
pub(crate) fn flush_step(run: &mut Run) -> Option<Flush> {
    let period = run.current();
    let window = run.ring.window(&JOURNAL, run.now);
    let ops: Vec<Op> = window
        .clone()
        .flat_map(|week| run.tables.get(&week).cloned().unwrap_or_default())
        .collect();
    let used: BTreeMap<u64, Used> = window
        .filter_map(|week| {
            let held = run.tables.get(&week)?.len();
            Some((
                week,
                Used {
                    records: held,
                    bytes: 0,
                    writer_records: held,
                    writer_bytes: 0,
                },
            ))
        })
        .collect();
    let resolve = |_: &Digest, _: &Actor| Some(ME);
    let read = flush::read_window(&resolve, &ops, &ME, &run.book);
    let mut heard = read.heard;
    run.book.n = run.book.n.max(read.own_n);
    run.outbox.retain(|event| !read.own.contains(event));
    flush::relayable(&mut heard, &mut run.book, &run.outbox, &ME, period);
    let n = run.book.n + 1;
    let planned = run
        .book
        .plan(
            &run.outbox,
            &heard,
            &Presence::default(),
            &ME,
            n,
            period,
            run.now,
            false,
        )
        .unwrap();
    if planned.changes.is_empty() {
        return None;
    }
    let payload = Command::Write {
        request: typed::request_id(APPLICATION, ROLE, &planned.changes).unwrap(),
        changes: planned.changes.clone(),
    }
    .encode()
    .unwrap();
    run.book.n = n;
    let appended = ring::landing(
        &JOURNAL,
        SHARES,
        &run.ring,
        run.now,
        &used,
        run.now,
        payload.len(),
    )
    .map_err(Kept::Unplaced);
    if let Ok(week) = appended {
        run.ring.hold(&JOURNAL, week, run.now).unwrap();
        run.sequence += 1;
        let mut id = [0; 32];
        id[..8].copy_from_slice(&run.sequence.to_be_bytes());
        run.tables.entry(week).or_default().push(Op {
            id,
            actor: [1; 48],
            sequence: run.sequence,
            roster: [7; 32],
            period: week,
            revert: false,
            changes: planned.changes.clone(),
        });
    }
    flush::settle(
        &mut run.outbox,
        &mut run.book,
        &planned,
        n,
        period,
        run.now,
        appended,
    );
    run.now += 60;
    Some(Flush {
        planned,
        payload,
        appended,
    })
}

/// A use noted into the outbox, as `flush::pass` absorbs the spool.
fn note_step(run: &mut Run) {
    let (outbox, fresh) = book::merge(
        std::mem::take(&mut run.outbox),
        &[used_at(run.now)],
        run.now,
    );
    run.outbox = outbox;
    run.book.fold(&fresh);
    run.now += 60;
}

/// The next week: the clock an hour into it and the ring's ratchet, whose tables it deletes.
pub(crate) fn tick_step(run: &mut Run) -> Vec<u64> {
    run.now = (run.current() + 1) * WEEK + 3600;
    let doomed = run.ring.ratchet(&JOURNAL, run.now);
    run.ring.held.retain(|week| !doomed.contains(week));
    run.tables.retain(|week, _| !doomed.contains(week));
    doomed
}

/// The model's view of `run`, the ghosts (`told`, `lost`, `beyond`) and counters carried from
/// `state`.
fn project(state: &State, run: &Run) -> State {
    let mut next = state.clone();
    let current = run.current();
    let count = |week: u64| i64::try_from(run.tables.get(&week).map_or(0, Vec::len)).unwrap();
    next.insert("p", count(current - 1));
    next.insert("c", count(current));
    next.insert("n", count(current + 1));
    next.insert("pend", i64::try_from(run.outbox.len()).unwrap());
    next.insert("wk", i64::from(run.book.sum_period == Some(current)));
    let fact = (1..=3)
        .filter(|i| run.believed_in(current - 2 + i))
        .max()
        .map_or(0, |i| i64::try_from(i).unwrap());
    next.insert("fact", fact);
    next
}

/// The code's step for each `AlabJournal` action from `state`, whose publisher is `run`.
fn step(state: &State, run: &mut Run, action: &str) -> State {
    step_with(state, run, action).0
}

/// [`step`], with the record a flush planned and where it went.
fn step_with(state: &State, run: &mut Run, action: &str) -> (State, Option<Flush>) {
    let next = match action {
        "Note" => {
            note_step(run);
            let mut next = project(state, run);
            next.insert("noted", state["noted"] + 1);
            next
        }
        "FlushCurrent" | "FlushNext" | "FlushKept" => {
            let before = run.outbox.clone();
            let end = *run.ring.window(&JOURNAL, run.now).end();
            let flush = flush_step(run).expect("a flush the model calls due plans a record");
            let mut next = project(state, run);
            let carried: BTreeSet<Event> = match flush.appended {
                Ok(_) => before[..flush.planned.events].iter().copied().collect(),
                Err(_) => BTreeSet::new(),
            };
            if flush.appended.is_ok() && flush.planned.events > 0 {
                next.insert("told", 1);
            }
            if flush.appended.is_ok_and(|week| week > end) {
                next.insert("beyond", 1);
            }
            if before
                .iter()
                .any(|e| !run.outbox.contains(e) && !carried.contains(e))
            {
                next.insert("lost", 1);
            }
            return (next, Some(flush));
        }
        "Tick" => {
            tick_step(run);
            let mut next = project(state, run);
            next.insert("weeks", state["weeks"] + 1);
            next
        }
        _ => panic!("no publisher step for {action}"),
    };
    (next, None)
}

/// The state and publisher after `path` from the model's initial state.
fn replay(path: &[&'static str]) -> (State, Run) {
    let model = aterm_models_db::alab_journal_model();
    let mut state = model.init_state();
    let mut run = Run::new();
    for action in path {
        state = step(&state, &mut run, action);
    }
    (state, run)
}

/// The `AlabJournal` projection of `action` from the state `path` reaches: [`step`] on the
/// publisher that path builds.
pub(crate) fn journal_transition(path: &[&'static str], action: &str) -> State {
    let (state, mut run) = replay(path);
    step(&state, &mut run, action)
}

/// Every state the model reaches, with the path that first reaches it.
fn walk(model: &aterm_spec::derive::Model) -> BTreeMap<State, Vec<&'static str>> {
    let init = model.init_state();
    let mut reached = BTreeMap::from([(init.clone(), Vec::new())]);
    let mut queue = vec![init];
    while let Some(state) = queue.pop() {
        let path = reached[&state].clone();
        let (replayed, mut idle) = replay(&path);
        assert_eq!(replayed, state, "replaying {path:?}");
        // A flush is due exactly where the model has one: an event waits or the week has no
        // record of this Mac's yet.
        assert_eq!(
            flush_step(&mut idle).is_some(),
            state["pend"] > 0 || state["wk"] == 0,
            "due from {state:?}"
        );
        for action in &model.actions {
            if !model.action_enabled(action.name, &state) {
                continue;
            }
            let next = journal_transition(&path, action.name);
            assert!(
                model.successors(action.name, &state).contains(&next),
                "{} from {state:?} steps to {next:?}",
                action.name
            );
            if !reached.contains_key(&next) {
                let mut longer = path.clone();
                longer.push(action.name);
                reached.insert(next.clone(), longer);
                queue.push(next);
            }
        }
    }
    reached
}

/// Every step the publisher takes is an `AlabJournal` successor, from every state the model
/// reaches, and the walk reaches every state; a flush is due exactly where the model has one.
/// NEGATIVE CONTROLS: a publisher whose relay reads only the window's tables (`relayable`
/// without its outbox events and known facts) states nothing of a use whose week was deleted,
/// which breaks `RelayKeepsLastUse` and is no successor; and a share of two records lets a
/// second record into the current week's table, no `FlushNext` successor.
#[test]
fn alab_journal_conforms() {
    let model = aterm_models_db::alab_journal_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    let reached = walk(&model);
    assert_eq!(
        reached.len(),
        aterm_spec::interp::reachable_states(&model).len()
    );

    // The relay of the window's tables alone, after both weeks that held the use are deleted.
    let (state, path) = reached
        .iter()
        .find(|(state, _)| {
            state["told"] == 1 && state["fact"] == 0 && state["wk"] == 0 && state["pend"] == 0
        })
        .expect("a told use whose tables are deleted");
    let (_, mut run) = replay(path);
    run.book.known.clear();
    flush_step(&mut run).unwrap();
    let blind = project(state, &run);
    assert_eq!(blind["fact"], 0);
    assert!(!model.check_invariant("RelayKeepsLastUse", &blind));
    assert!(!model.successors("FlushCurrent", state).contains(&blind));

    // A share of two: the second record stays in the current week.
    let (state, path) = reached
        .iter()
        .find(|(state, _)| state["c"] == 1 && state["n"] == 0 && state["pend"] > 0)
        .expect("a full current share with an event waiting");
    let (_, mut run) = replay(path);
    let used = BTreeMap::from([(
        run.current(),
        Used {
            records: 1,
            bytes: 0,
            writer_records: 1,
            writer_bytes: 0,
        },
    )]);
    let wider = Shares {
        writer_records: 2,
        ..SHARES
    };
    let week = ring::landing(&JOURNAL, wider, &run.ring, run.now, &used, run.now, 64).unwrap();
    assert_eq!(week, run.current());
    let first = run.tables[&week][0].clone();
    run.tables.entry(week).or_default().push(first);
    run.outbox.clear();
    let mut wide = project(state, &run);
    wide.insert("told", 1);
    assert!(!model.successors("FlushNext", state).contains(&wide));
}

/// The Commons side of one `AlabJournal` step the pure publisher `run` just took: its record,
/// if one was planned, appended on this writer's own chain (`Directory::append_own`, whose
/// commit is `Store::commit_own`) at the clock `now` it was planned at; or the week's ratchet
/// (`Directory::retire`), which deletes what fell out of the window. Then the window's tables as
/// Commons holds them must be the pure publisher's, record for record: the week each append went
/// to, a kept record kept for the same reason, and no record more, so the append spends no merge
/// a share would count.
pub(crate) fn commons_transition(
    session: &crate::table::Session<'_>,
    directory: &crate::catalog::Directory,
    run: &Run,
    now: u64,
    flushed: Option<&Flush>,
    ticked: bool,
) {
    if let Some(flush) = flushed {
        let real = directory
            .append_own(session, &JOURNAL, SHARES, now, now, &flush.payload)
            .map(|(week, _)| week)
            .map_err(|why| why.to_string());
        let pure = flush.appended.map_err(|kept| kept.reason().to_owned());
        assert_eq!(real, pure, "the append at {now}");
    }
    if ticked {
        directory.retire(session, &JOURNAL, run.now).unwrap();
    }
    let window = flush::window(session, directory, run.now).unwrap();
    for week in run.ring.window(&JOURNAL, run.now) {
        let pure = run.tables.get(&week).map_or(&[][..], Vec::as_slice);
        let held: Vec<&Op> = window.ops.iter().filter(|op| op.period == week).collect();
        assert_eq!(
            window.used.get(&week).map_or(0, |used| used.writer_records),
            pure.len(),
            "this writer's records in week {week}"
        );
        assert_eq!(
            held.iter().map(|op| &op.changes).collect::<Vec<_>>(),
            pure.iter().map(|op| &op.changes).collect::<Vec<_>>(),
            "week {week}'s records"
        );
    }
}

/// `path` through `AlabJournal` on a fresh one-Mac Commons lab, agreeing with the pure publisher
/// at every step, each pure step a model successor; the states before and after the last step.
fn commons_trace(path: &[&'static str]) -> (State, State) {
    use aterm_secrets::commons::roster::Role;
    let model = aterm_models_db::alab_journal_model();
    let lab = crate::table::tests::Lab::new(&[Role::Writer]);
    let bootstrap = lab.bootstrap(0);
    let session = crate::table::Session::open(&bootstrap).unwrap();
    let directory = crate::catalog::Directory::new(&lab.paths[0], bootstrap.trust());
    let mut state = model.init_state();
    let mut before = state.clone();
    let mut run = Run::new();
    for &action in path {
        assert!(
            model.action_enabled(action, &state),
            "{action} from {state:?}"
        );
        let now = run.now;
        let (next, flushed) = step_with(&state, &mut run, action);
        assert!(
            model.successors(action, &state).contains(&next),
            "{path:?}: {action} from {state:?} steps to {next:?}"
        );
        commons_transition(
            &session,
            &directory,
            &run,
            now,
            flushed.as_ref(),
            action == "Tick",
        );
        before = std::mem::replace(&mut state, next);
    }
    (before, state)
}

/// Two paths through `AlabJournal` on a real one-Mac Commons lab agree with the pure publisher
/// at every step. The first: a week's first record, a spill into the next week, a full window
/// kept local, a week passing with the kept record landing, and two more weeks, the last record
/// restating the use from the previous week's table. The second: a use published, both weeks
/// that held it deleted, so no window table says it (`fact` 0), and the week's first record
/// restating it from the book alone (`RelayKeepsLastUse`), its relay row committed on this
/// writer's own chain as the pure publisher planned it.
/// NEGATIVE CONTROLS: with `retained_use` not counting this writer's own records, the window
/// reads none of them: `` this writer's records in week 3000 left: 0 right: 1 ``; with
/// `relayable` not offering the book's known facts, the second path's last record restates
/// nothing, no model successor: `` FlushCurrent from {.., "fact": 0, .., "told": 1, ..} steps to
/// {.., "fact": 0, .., "told": 1, .., "wk": 1} ``.
#[test]
fn the_commons_append_takes_the_models_steps() {
    let (_, spilled) = commons_trace(&[
        "FlushCurrent",
        "Note",
        "FlushNext",
        "Note",
        "FlushKept",
        "Tick",
        "FlushNext",
        "Tick",
        "Tick",
        "FlushCurrent",
    ]);
    assert_eq!(spilled["told"], 1);
    assert!(
        spilled["fact"] > 0,
        "the last record restates the use: {spilled:?}"
    );

    let (gone, restated) = commons_trace(&["Note", "FlushCurrent", "Tick", "Tick", "FlushCurrent"]);
    assert_eq!(
        (gone["told"], gone["fact"]),
        (1, 0),
        "no window table says the use: {gone:?}"
    );
    assert!(
        restated["fact"] > 0,
        "the last record restates the use from no table: {restated:?}"
    );
}
