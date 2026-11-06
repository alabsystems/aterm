// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of `SourceEpisode` (`aterm_models_harness::source_episode_model`) to a
//! source's book ([`SourceBook::look`] at every read, [`SourceBook::restore`] through
//! sources.json at every restart), and of `OwnerTell` (`aterm_models_harness::owner_tell_model`)
//! to the consent its tell needs ([`RuleState::of`]) and the yes's write
//! ([`Snapshot::still_current`]). Every state the models reach is walked with its real twin
//! along the first path to it; at each, the book is probed on copies, so it has dwelt exactly
//! where the model's quiet count is the dwell and an unread read changes nothing, and the rule's
//! state is Live, DryRun or Unconfirmed exactly where the model's consent says. The source's
//! moves, the arming, aterm going down, the episodes, the owner's edit, a person's press, the
//! guards, the pace and the hour are the models' environment; the tell's and the would row's
//! own book (`owner::decide`) is the slice-3 plan's 3.4/3.5b, so the bind carries them as the
//! model has them. Reads are [`READ_MS`] apart and the dwell is two of them, so the model's two
//! quiet reads are the code's dwell.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use aterm_agent::supervise::occupancy::fault::{self, Fault};
use aterm_agent::supervise::occupancy::owner_rules::{
    OwnerName, OwnerTables, Rev, RuleMode, RuleState, Snapshot,
};
use aterm_agent::supervise::occupancy::source_episode::{
    DWELL_READS, Edge, ReadWord, Restored, STALE_MS, SourceBook, SourceValue, SourcesFile, Step,
};
use aterm_models_harness::{owner_tell_model, source_episode_model};
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_spec::xref::{self, StepEvidence};

use crate::common::cnst;

type State = BTreeMap<&'static str, i64>;

const T0: i64 = 1_000_000_000;

/// The clock between two steps that read nothing, ms.
const STEP_MS: i64 = 1_000;

/// The clock between two reads, ms.
const READ_MS: i64 = 5 * 60_000;

/// The dwell: two reads.
const DWELL_MS: i64 = 2 * READ_MS;

/// The source's target ([`aterm_agent::supervise::occupancy::source_episode::target_of`]).
const TARGET: u64 = 0x5eed_0001;

/// The board's steps the code performs, each on its anchored function.
const BOOK_ANCHORED: [&str; 2] = ["Look", "Up"];

/// The world's, the board's (3.3) and aterm's own steps, which the book only sees.
const BOOK_ENVIRONMENT: [&str; 5] = ["Arm", "Disarm", "Down", "Move", "MoveBack"];

/// The board's mutants, which no real step mirrors.
const BOOK_MUTANTS: [&str; 5] = [
    "CloseOnFirstQuiet",
    "QuietRestarts",
    "ReadUnconfirmed",
    "RebaseOnRestart",
    "UndoAtFirstBase",
];

/// The tell's steps the code performs, each on its anchored function.
const TELL_ANCHORED: [&str; 2] = ["ConfirmWrite", "Tell"];

/// The world's, the owner's, a person's and the other models' steps, and the decider's own
/// (`Would`, `Lapse`: 3.4/3.5b), which the bind carries as the model has them.
const TELL_ENVIRONMENT: [&str; 11] = [
    "Close",
    "EditTable",
    "GuardOff",
    "GuardOn",
    "HourPasses",
    "Lapse",
    "Open",
    "PaceOff",
    "PaceOn",
    "Press",
    "Would",
];

/// The tell's mutants, which no real step mirrors.
const TELL_MUTANTS: [&str; 9] = [
    "ConfirmByName",
    "ConfirmStaleSnapshot",
    "TellKeepsDue",
    "TellOlder",
    "TellUndone",
    "TypeUnderDryRun",
    "TypeUnderObserve",
    "UrgentPassesGuard",
    "UrgentUncapped",
];

/// `WorkflowStep`'s steps, every one waived until its reader lands (3.6b).
const STEP_WAIVED: [&str; 14] = [
    "LapseA",
    "LapseB",
    "OpenA",
    "OpenB",
    "ReadUncaptured",
    "ReturnA",
    "ReturnB",
    "TellA",
    "TellAfterEnd",
    "TellB",
    "TellKeepsDue",
    "TypeUnderObserve",
    "WouldA",
    "WouldB",
];

/// The board's configurations: a restart within the day, and one past it.
const BOOK_CONFIGS: [&[(&str, i64)]; 2] = [&[], &[("Stale", 1)]];

/// The tell's configurations, each its model declares.
const TELL_CONFIGS: [&[(&str, i64)]; 5] = [
    &[],
    &[("Prompt", 0)],
    &[("Dry", 1)],
    &[("Urgent", 1)],
    &[("Older", 1)],
];

/// The source's three values, as the model numbers them.
fn value(at: i64) -> SourceValue {
    let digit = match at {
        0 => 'a',
        1 => 'b',
        2 => 'c',
        other => panic!("the model's source has three values, not {other}"),
    };
    SourceValue::sha(&digit.to_string().repeat(40)).expect("an object id")
}

fn number(v: &SourceValue) -> i64 {
    (0..3)
        .find(|&at| value(at) == *v)
        .unwrap_or_else(|| panic!("{v:?} is none of the model's values"))
}

fn source_name() -> OwnerName {
    OwnerName::parse("trust").expect("a name")
}

/// The evidence windows of an audited walk, or none.
struct Ev(Option<StepEvidence>);

impl Ev {
    fn step<T>(&mut self, action: &'static str, drive: impl FnOnce() -> T) -> T {
        match &mut self.0 {
            Some(ev) => ev.step(action, drive),
            None => drive(),
        }
    }
}

/// The board's real side of a model state, along the first path the walk took to it.
#[derive(Clone)]
struct Board {
    /// The board's clock, ms.
    now: i64,
    book: SourceBook,
    /// sources.json as the board last wrote it: after every read.
    file: String,
    /// The source's value now, as the model numbers it.
    v: i64,
    moved: i64,
    armed: bool,
    down: bool,
}

impl Board {
    fn new() -> Self {
        let book = SourceBook::new(source_name(), TARGET);
        let file = SourcesFile::of([&book]).to_json();
        Self {
            now: T0,
            book,
            file,
            v: 0,
            moved: 0,
            armed: true,
            down: false,
        }
    }
}

/// The model's book as the real one says it (a book with no base reads `seen = 3`, a closed
/// episode base and latest 0), with the world the bind keeps.
fn project_book(b: &Board) -> State {
    let seen = b.book.seen().map_or(3, number);
    let (open, base, latest) = b
        .book
        .episode()
        .map_or((0, 0, 0), |e| (1, number(&e.base), number(&e.latest)));
    State::from([
        ("v", b.v),
        ("seen", seen),
        ("base", base),
        ("latest", latest),
        ("open", open),
        ("quiet", i64::from(b.book.persisted().quiet)),
        ("moved", b.moved),
        ("armed", i64::from(b.armed)),
        ("down", i64::from(b.down)),
    ])
}

/// The book agrees with the model at `st`: its projection, and what its probes on copies say
/// (dwelt exactly at the dwell of an open episode; an unread read is no step at all).
fn book_agrees(m: &Model, st: &State, b: &Board) -> bool {
    let real = project_book(b);
    let dwell = cnst(m, "Dwell");
    let mut unread = b.book.clone();
    let word = unread.look(Err(ReadWord::Timeout), b.now + READ_MS, DWELL_MS);
    real.iter().all(|(var, value)| st[var] == *value)
        && b.book.dwelt(DWELL_MS) == (st["open"] == 1 && st["quiet"] == dwell)
        && word == Step::Unread(ReadWord::Timeout)
        && unread == b.book
}

/// What the model's read from `st` to `next` is, as the code names it.
fn read_step(m: &Model, st: &State, next: &State) -> Step {
    if st["seen"] == 3 {
        Step::Based
    } else if st["seen"] != st["v"] {
        Step::Edge(if st["open"] == 0 {
            Edge::Opened
        } else if st["quiet"] == cnst(m, "Dwell") {
            Edge::Superseded
        } else {
            Edge::Joined
        })
    } else if st["open"] == 1 && next["open"] == 0 {
        Step::Closed
    } else {
        Step::Quiet
    }
}

/// The real step for the board's `action` at `st`; false where the code answers otherwise than
/// the step says. A step's own call runs in its evidence window, and nothing else does.
fn mirror_book(
    m: &Model,
    action: &'static str,
    st: &State,
    next: &State,
    b: &mut Board,
    ev: &mut Ev,
) -> bool {
    match action {
        "Move" | "MoveBack" => {
            b.now += STEP_MS;
            b.v = (b.v + if action == "Move" { 1 } else { 2 }) % 3;
            b.moved += 1;
            true
        }
        "Arm" | "Disarm" => {
            b.now += STEP_MS;
            b.armed = action == "Arm";
            true
        }
        "Down" => {
            b.now += STEP_MS;
            b.down = true;
            true
        }
        "Look" => {
            b.now += READ_MS;
            let (now, read) = (b.now, value(b.v));
            let step = ev.step(action, || b.book.look(Ok(read), now, DWELL_MS));
            b.file = SourcesFile::of([&b.book]).to_json();
            step == read_step(m, st, next)
        }
        "Up" => {
            let read_ms = b.book.persisted().read_ms.unwrap_or(b.now);
            b.now = if cnst(m, "Stale") == 1 {
                b.now.max(read_ms) + STALE_MS + READ_MS
            } else {
                b.now + STEP_MS
            };
            let (now, file) = (b.now, b.file.clone());
            let declared = [(source_name(), TARGET)];
            let read = ev.step(action, || {
                SourcesFile::read(file.as_bytes(), &declared, now)
            });
            let Ok(mut books) = read else {
                return false;
            };
            let Some((book, restored)) = books.pop().filter(|_| books.is_empty()) else {
                return false;
            };
            let want = if cnst(m, "Stale") == 1 && st["seen"] != 3 {
                Restored::Rebased
            } else {
                Restored::Kept
            };
            b.book = book;
            b.down = false;
            restored == want
        }
        other => panic!("no real step for `{other}`"),
    }
}

/// The consent's real side of a model state: the table in force, the owner's confirmed record
/// and a press's snapshot, with the episode, the guards and the pace as the model has them.
#[derive(Clone)]
struct Consent {
    /// The rule's two tables, a field apart: `rev` names the one in force.
    tables: [OwnerTables; 2],
    revs: [Rev; 2],
    mode: RuleMode,
    /// The owner's confirmed record: the model's one slot (3.5a's writer keeps up to 64 revs).
    record: Vec<Rev>,
    snapshot: Option<Snapshot>,
    /// The episode's, the guards', the pace's and the decider's variables, as the model has them.
    carried: State,
}

/// The variables the bind carries for `OwnerTell`: the code reads none of them yet.
const CARRIED: [&str; 9] = [
    "open", "ep", "fresh", "due", "told", "rev", "guard", "pace", "urg",
];

fn rule_name() -> OwnerName {
    OwnerName::parse("release-base").expect("a name")
}

/// The rule's table, its text `text`, in `mode`.
fn table(text: &str, mode: RuleMode) -> OwnerTables {
    let toml = format!(
        "[harness.occupancy_source.trust]\nkind = \"git-ref\"\nurl = \"git@h:o/trust.git\"\n\
         [[harness.occupancy_rule]]\nname = \"release-base\"\nwhen = \"git(trust).moved\"\n\
         mode = \"{}\"\ntext = \"{text}\"\n",
        mode.word()
    );
    let (tables, notes) = OwnerTables::from_aterm_toml(&toml);
    assert!(notes.is_empty(), "{notes:?}");
    assert!(
        tables.rule(&rule_name()).is_some(),
        "the rule reads: {toml}"
    );
    tables
}

impl Consent {
    fn new(m: &Model) -> Self {
        let mode = if cnst(m, "Dry") == 1 {
            RuleMode::DryRun
        } else {
            RuleMode::Live
        };
        let tables = [
            table("Re-read the base.", mode),
            table("Re-read the base and the index.", mode),
        ];
        let revs = tables.each_ref().map(|t| {
            let rule = t.rule(&rule_name()).expect("the rule");
            t.rev(rule, &[])
        });
        assert_ne!(revs[0], revs[1], "an edit of the text is a new rev");
        let init = m.init_state();
        Self {
            tables,
            revs,
            mode,
            record: Vec::new(),
            snapshot: None,
            carried: CARRIED.iter().map(|&var| (var, init[var])).collect(),
        }
    }

    /// The table in force.
    fn in_force(&self) -> &OwnerTables {
        &self.tables[usize::from(self.carried["rev"] == 1)]
    }

    fn rev_number(&self, rev: &Rev) -> i64 {
        self.revs
            .iter()
            .position(|r| r == rev)
            .and_then(|at| i64::try_from(at).ok())
            .unwrap_or_else(|| panic!("{rev:?} is neither table's rev"))
    }
}

/// The model's consent as the real side says it.
fn project_tell(c: &Consent) -> State {
    let mut st = c.carried.clone();
    st.insert("crev", c.record.last().map_or(2, |r| c.rev_number(r)));
    st.insert(
        "snap",
        c.snapshot.as_ref().map_or(2, |s| c.rev_number(&s.rev)),
    );
    st
}

/// The consent agrees with the model at `st`: its projection, the rule's state (Live or DryRun
/// exactly where the model's yes is to the rev in force, else Unconfirmed), and whether a yes to
/// the press's snapshot would confirm it (exactly where the snapshot is of the rev in force).
fn tell_agrees(st: &State, c: &Consent) -> bool {
    let real = project_tell(c);
    let state = RuleState::of(&c.revs[usize::from(st["rev"] == 1)], &c.record, c.mode);
    let want = match (st["crev"] == st["rev"], c.mode) {
        (false, _) => RuleState::Unconfirmed,
        (true, RuleMode::Live) => RuleState::Live,
        (true, RuleMode::DryRun) => RuleState::DryRun,
    };
    let current = c
        .snapshot
        .as_ref()
        .is_none_or(|s| s.still_current(c.in_force(), &[]) == (st["snap"] == st["rev"]));
    real.iter().all(|(var, value)| st[var] == *value) && state == want && current
}

/// The real step for the tell's `action` from `st` to `next`; false where the code answers
/// otherwise than the step says.
fn mirror_tell(action: &'static str, next: &State, c: &mut Consent, ev: &mut Ev) -> bool {
    let ok = match action {
        "Press" => {
            c.snapshot = Snapshot::of(c.in_force(), &rule_name(), &[]);
            c.snapshot.is_some()
        }
        "ConfirmWrite" => {
            let Some(snapshot) = c.snapshot.take() else {
                return false;
            };
            let in_force = c.in_force().clone();
            if ev.step(action, || snapshot.still_current(&in_force, &[])) {
                c.record = vec![snapshot.rev];
            }
            true
        }
        "Tell" => {
            let rev = c.revs[usize::from(c.carried["rev"] == 1)];
            let record = c.record.clone();
            let mode = c.mode;
            ev.step(action, || RuleState::of(&rev, &record, mode)) == RuleState::Live
        }
        _ if TELL_ENVIRONMENT.contains(&action) => true,
        other => panic!("no real step for `{other}`"),
    };
    for var in CARRIED {
        c.carried.insert(var, next[var]);
    }
    ok
}

struct Walked {
    states: usize,
    off: Vec<State>,
    fired: BTreeSet<&'static str>,
}

/// Every state `m` reaches, each with its real twin from `init`, stepped by `mirror` and judged
/// by `agrees`; with `strict`, a disagreement panics, else it is kept in `off` and not walked on.
fn walk<R: Clone>(
    m: &Model,
    strict: bool,
    init: R,
    agrees: impl Fn(&State, &R) -> bool,
    mut mirror: impl FnMut(&'static str, &State, &State, &mut R) -> bool,
) -> Walked {
    let mut w = Walked {
        states: 0,
        off: Vec::new(),
        fired: BTreeSet::new(),
    };
    assert!(
        agrees(&m.init_state(), &init),
        "{}: the real side starts where the model does",
        m.name
    );
    let mut seen: HashSet<Vec<i64>> = HashSet::new();
    let mut queue = VecDeque::from([(m.init_state(), init)]);
    while let Some((st, real)) = queue.pop_front() {
        if !seen.insert(crate::common::key(m, &st)) {
            continue;
        }
        let env = m.eval_env(&st);
        for action in &m.actions {
            let Some(next) = m.successors_in(action, &env, &st).into_iter().next() else {
                continue;
            };
            let mut r = real.clone();
            let stepped = mirror(action.name, &st, &next, &mut r);
            w.fired.insert(action.name);
            if stepped && agrees(&next, &r) {
                queue.push_back((next, r));
                continue;
            }
            assert!(
                !strict,
                "{}: the code disagrees after {} from {st:?}: the model's {next:?}",
                m.name, action.name
            );
            w.off.push(next);
        }
    }
    w.states = seen.len();
    w
}

fn walk_book(config: &[(&'static str, i64)], strict: bool, ev: &mut Ev) -> (Model, Walked) {
    let m = interp::with_consts(&source_episode_model(), config);
    assert_eq!(
        cnst(&m, "Dwell"),
        i64::from(DWELL_READS),
        "the model's dwell is the code's"
    );
    let w = walk(
        &m,
        strict,
        Board::new(),
        |st, b| book_agrees(&m, st, b),
        |action, st, next, b| mirror_book(&m, action, st, next, b, ev),
    );
    (m, w)
}

fn walk_tell(config: &[(&'static str, i64)], strict: bool, ev: &mut Ev) -> (Model, Walked) {
    let m = interp::with_consts(&owner_tell_model(), config);
    let w = walk(
        &m,
        strict,
        Consent::new(&m),
        tell_agrees,
        |action, _, next, c| mirror_tell(action, next, c, ev),
    );
    (m, w)
}

#[test]
fn tier1_the_book_reads_and_restores_exactly_where_the_model_does() {
    for config in BOOK_CONFIGS {
        let (m, w) = walk_book(config, true, &mut Ev(None));
        let want: BTreeSet<&str> = BOOK_ANCHORED.into_iter().chain(BOOK_ENVIRONMENT).collect();
        assert_eq!(w.fired, want, "{config:?}: the steps mirrored");
        assert_eq!(
            w.states,
            interp::reachable_states(&m).len(),
            "{config:?}: every state the model reaches is walked"
        );
    }
}

#[test]
fn tier1_the_consent_holds_exactly_where_the_model_does() {
    let mut fired = BTreeSet::new();
    for config in TELL_CONFIGS {
        let (m, w) = walk_tell(config, true, &mut Ev(None));
        assert_eq!(
            w.states,
            interp::reachable_states(&m).len(),
            "{config:?}: every state the model reaches is walked"
        );
        fired.extend(w.fired);
    }
    let want: BTreeSet<&str> = TELL_ANCHORED.into_iter().chain(TELL_ENVIRONMENT).collect();
    assert_eq!(fired, want, "the steps mirrored across the configurations");
}

type Shows = fn(&State) -> bool;

/// Each fault the slice-3 plan's 3.1 planted in the pure code makes the walk disagree, and only
/// where the model says the defect shows.
#[test]
fn tier1_each_planted_defect_disagrees_where_the_model_says() {
    // Each on the committed board: a restart past the day re-bases with or without the fault.
    let book: [(Fault, Shows); 3] = [
        // The book has dwelt at its first quiet read: the model's open episode has one.
        (Fault::DwellCloseEarly, |st| {
            st["open"] == 1 && st["quiet"] == 1
        }),
        // A read with no edge counts nothing: the model's count has risen.
        (Fault::QuietRestarts, |st| st["quiet"] >= 1),
        // A restart within the day re-bases: the model's book kept its base.
        (Fault::RebaseOnRestart, |st| {
            st["down"] == 0 && st["seen"] != 3
        }),
    ];
    for (planted, shows) in book {
        let (_, w) = fault::with(planted, || walk_book(&[], false, &mut Ev(None)));
        assert!(!w.off.is_empty(), "{planted:?} agreed everywhere");
        assert!(w.off.iter().all(shows), "{planted:?}: {:?}", w.off);
    }
    let tell: [(Fault, Shows); 2] = [
        // Any yes on record counts: the model's yes is to the other rev.
        (Fault::ConfirmByName, |st| {
            st["crev"] != 2 && st["crev"] != st["rev"]
        }),
        // A yes to a stale snapshot confirms the edit: the model's snapshot is of the other rev.
        (Fault::ConfirmStaleSnapshot, |st| {
            st["snap"] != 2 && st["snap"] != st["rev"]
        }),
    ];
    for (planted, shows) in tell {
        let (_, w) = fault::with(planted, || walk_tell(&[], false, &mut Ev(None)));
        assert!(!w.off.is_empty(), "{planted:?} agreed everywhere");
        assert!(w.off.iter().all(shows), "{planted:?}: {:?}", w.off);
    }
}

#[test]
fn tier1_every_anchored_step_enters_its_own_function() {
    let mut book = Ev(Some(StepEvidence::new("SourceEpisode")));
    for config in BOOK_CONFIGS {
        walk_book(config, true, &mut book);
    }
    let mut tell = Ev(Some(StepEvidence::new("OwnerTell")));
    for config in TELL_CONFIGS {
        walk_tell(config, true, &mut tell);
    }
    for (ev, anchored) in [(book, &BOOK_ANCHORED[..]), (tell, &TELL_ANCHORED[..])] {
        let audit = ev.0.expect("the audited walk's evidence").audit();
        assert_eq!(
            audit.actions,
            anchored.iter().copied().collect::<BTreeSet<_>>(),
            "{audit:?}"
        );
        assert!(audit.uninstrumented.is_empty(), "{audit:?}");
        assert!(audit.ambiguous.is_empty(), "{audit:?}");
        assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
        assert!(audit.undriven.is_empty(), "{audit:?}");
        assert!(audit.unwitnessed.is_empty(), "{audit:?}");
        assert!(audit.blind_steps.is_empty(), "{audit:?}");
        assert!(audit.stray.is_empty(), "{audit:?}");
        assert!(audit.dark_sites.is_empty(), "{audit:?}");
    }
}

#[test]
fn tier1_the_anchors_name_what_the_code_implements() {
    for (model, anchored, waived) in [
        (
            source_episode_model(),
            &BOOK_ANCHORED[..],
            [&BOOK_ENVIRONMENT[..], &BOOK_MUTANTS[..]].concat(),
        ),
        (
            owner_tell_model(),
            &TELL_ANCHORED[..],
            [&TELL_ENVIRONMENT[..], &TELL_MUTANTS[..]].concat(),
        ),
    ] {
        let found: BTreeSet<&str> = xref::refinements()
            .filter(|r| r.machine == model.name)
            .map(|r| r.action)
            .collect();
        assert_eq!(found, anchored.iter().copied().collect(), "{}", model.name);
        let found: BTreeSet<&str> = xref::waivers()
            .filter(|w| w.machine == model.name && !w.action.is_empty())
            .map(|w| w.action)
            .collect();
        assert_eq!(found, waived.into_iter().collect(), "{}", model.name);
        let actions: BTreeSet<&str> = model.actions.iter().map(|a| a.name).collect();
        let bound: BTreeSet<&str> = anchored.iter().copied().chain(found).collect();
        assert_eq!(
            bound, actions,
            "{}: every action anchored or waived",
            model.name
        );
    }
    let step = aterm_models_harness::workflow_step_model();
    assert!(
        xref::refinements().all(|r| r.machine != step.name),
        "WorkflowStep binds nothing until its reader lands"
    );
    let waived: BTreeSet<&str> = xref::waivers()
        .filter(|w| w.machine == step.name)
        .map(|w| w.action)
        .collect();
    assert_eq!(waived, STEP_WAIVED.into_iter().collect());
    let actions: BTreeSet<&str> = step.actions.iter().map(|a| a.name).collect();
    assert_eq!(waived, actions, "every WorkflowStep step waived");
}
