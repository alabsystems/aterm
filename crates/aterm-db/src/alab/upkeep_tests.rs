// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{Bell, Cadence, Elsewhere, Files, Materializer, materialize};
use crate::{
    alab::{
        Registry,
        registry::{MACHINE, MACHINE_FACTS},
    },
    database::Change,
    file::{
        growth::GROWTH,
        materialize::{self as files_materialize, Pass},
    },
    table::tests::{Lab, repair_commons},
    typed::{grid::tests::put, keys},
};
use aterm_file::{File, Recorded, Word};
use aterm_json::{Value, json};
use aterm_secrets::commons::roster::Role;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

/// The Commons owner's materializer passes shortly after a table store of its process grows,
/// and again each minute: the schedule's clock is the test's, so nothing here waits on time.
/// Other tests of this binary append too, which can only make a pass due sooner than this
/// test's own write does, never before [`Cadence::settle`] has passed.
/// NEGATIVE CONTROL: with `table::store::appended` not calling `GROWTH.grew()`, the wake after the
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
    let mut files = Files::new(cadence, start, GROWTH.count());
    assert!(
        !files.due(start, GROWTH.count()),
        "nothing is due as the owner starts"
    );
    let pass = || {
        files_materialize::pass(
            paths,
            &config,
            None,
            &Pass {
                read_by: Instant::now() + aterm_spec::wait::MUST_HAPPEN,
                vault_wait: files_materialize::VAULT_WAIT,
                again: false,
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
    assert!(!files.due(seen, GROWTH.count()));
    assert!(files.moved_at.is_some_and(|at| at <= seen));
    assert!(
        files.due(seen + cadence.settle, GROWTH.count()),
        "a pass is due once the growth has settled"
    );
    pass();
    let read = machines.read();
    assert_eq!(read.status.word, Word::Live);
    assert_eq!(read.named("m7").len(), 1);
    let passes = machines.state().unwrap().seq;

    // With no growth seen since, the next pass is the regular one: it projects nothing again,
    // and writes nothing, the count of passes included.
    let after = seen + cadence.settle;
    assert_eq!(files.regular, after + cadence.every);
    assert!(!files.due(after + cadence.every / 2, files.seen));
    assert!(
        files.due(after + cadence.every, files.seen),
        "the regular pass is due"
    );
    let projections = files_materialize::PROJECTIONS.get();
    pass();
    assert_eq!(machines.state().unwrap().seq, passes);
    assert_eq!(
        machines.read().status.rev(),
        1,
        "nothing arrived: one revision"
    );
    assert_eq!(
        files_materialize::PROJECTIONS.get(),
        projections,
        "nothing the projection reads moved: no view was made"
    );

    // A pass that met a busy vault is followed sooner than the regular one.
    let at = after + cadence.every;
    files.met_busy(at);
    assert_eq!(files.regular, at + cadence.busy);
    assert!(files.due(at + cadence.busy, files.seen));
    assert_eq!(files.regular, at + cadence.busy + cadence.every);
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
    let pass = || super::met_busy(&super::files_pass(paths, &config, hang()));
    assert!(!pass());
    let machines = File::at(&crate::file::root(paths), &crate::file::MACHINES.file());
    let draft = crate::file::seam::draft_set(
        paths,
        crate::file::MACHINES.name,
        "m7",
        "note",
        Some("said at the desk"),
    )
    .unwrap();
    let intent = machines
        .write(&crate::file::seam::owner(), draft, 1)
        .unwrap();
    let records = || {
        crate::att::Handle::at(paths.clone(), &crate::att::REGISTRY)
            .history()
            .unwrap()
            .len()
    };
    let before = records();
    assert!(!pass());
    assert_eq!(records(), before + 1, "the pass signed the intent");
    let outbox = machines.outbox();
    assert!(outbox.queued.is_empty());
    assert_eq!(outbox.done[0].intent.id, intent.id);
    assert!(outbox.done[0].seen, "the same pass materialized its record");
    let published = machines.published();
    assert_eq!(published.status.word, Word::Live);
    assert_eq!(published.named("m7")[0].cell("note"), "said at the desk");
    assert!(!pass());
    assert_eq!(records(), before + 1, "a later pass signs nothing more");
}

/// Regular passes the idle hour makes after its first: one a minute for an hour.
const IDLE_PASSES: usize = 60;

/// One of [`COUNTERS`]: its name in the `files-idle` lane's report and its reader.
type Counter = (&'static str, fn() -> usize);

/// The thread counters an idle hour is counted by, each read as this thread has it now, named
/// as the `files-idle` lane's baseline names them: the table views a pass reads to project from
/// ([`files_materialize::PROJECTIONS`]); MACHINE's claim projections and claim contexts
/// ([`crate::typed::claim::PROJECTS`], [`crate::typed::claim::CONTEXTS`]); the durable writes a
/// recording makes and the folder modes the shared files' writers set
/// (`aterm_file::file::test_puts`, `test_chmods`); the
/// carried records handed to authentication ([`crate::table::CARRIED_AUTHENTICATED`]); the store
/// read batches' fences ([`crate::table::FENCES`]); and the asks of the kept historical keys
/// (`aterm_secrets::commons::roster::test_kept_key_asks`).
const COUNTERS: [Counter; 8] = [
    ("projections", || files_materialize::PROJECTIONS.get()),
    ("machine_claim_projects", || {
        crate::typed::claim::PROJECTS
            .with_borrow(|counts| counts.get(MACHINE.name).copied().unwrap_or(0))
    }),
    ("machine_claim_contexts", || {
        crate::typed::claim::CONTEXTS
            .with_borrow(|counts| counts.get(MACHINE.name).copied().unwrap_or(0))
    }),
    ("puts", aterm_file::file::test_puts),
    ("chmods", aterm_file::file::test_chmods),
    ("carried_authenticated", || {
        crate::table::CARRIED_AUTHENTICATED.get()
    }),
    ("fences", || crate::table::FENCES.get()),
    (
        "kept_key_asks",
        aterm_secrets::commons::roster::test_kept_key_asks,
    ),
];

/// Every counter of [`COUNTERS`], now.
fn counters() -> [usize; COUNTERS.len()] {
    COUNTERS.map(|(_, read)| read())
}

/// What every counter counted since it read `before`, added to `into`.
fn tally(into: &mut [usize; COUNTERS.len()], before: [usize; COUNTERS.len()]) {
    for ((total, now), before) in into.iter_mut().zip(counters()).zip(before) {
        *total += now - before;
    }
}

/// Mac `n`'s own facts row, binding its Commons machine to the machine row `label` names.
pub(crate) fn bound_facts(lab: &Lab, n: usize, label: &str) -> Vec<Change> {
    vec![Change {
        collection: MACHINE_FACTS.name.into(),
        key: keys::uuid32(&lab.machine(n)),
        value: Some(json!({"r": 1, "n": 1, "mac": keys::machine_pk(label), "label": label})),
    }]
}

/// A live machine row labelled `label`.
pub(crate) fn machine_row(label: &str) -> Vec<Change> {
    let machine = keys::machine_pk(label);
    vec![
        put(&MACHINE, &machine, "label", json!(label)),
        put(&MACHINE, &machine, "status", json!("active")),
    ]
}

/// The idle hour's lab: two Writers whose registry is in its second generation, carrying each
/// Mac's facts row from a membership epoch that is no longer current, the owner's catalog
/// declaring `lab/tracker` too, with no entry. m1 writes both machine
/// rows and its own facts in the first epoch; after a rotation m2 writes its facts in the
/// second; after another, m1 rolls the registry, which waits for m2 to declare the second
/// generation and then seals the first. Mac 0 (m1) is the owner the hour passes on.
fn idle_lab() -> Lab {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let (a, b) = (
        Registry::at(lab.paths[0].clone()),
        Registry::at(lab.paths[1].clone()),
    );
    a.open().unwrap();
    a.seed().unwrap();
    // The lab's tracker is declared on the owner and holds no entry: its logs' files are `live`
    // and empty, as the hour asks of every file.
    crate::file::tests::declare(&lab.paths[0], &crate::att::TRACKER);
    a.write(vec![
        machine_row("m1"),
        machine_row("m2"),
        bound_facts(&lab, 0, "m1"),
    ])
    .unwrap();
    repair_commons(&lab, 6);
    lab.rotate(&[Role::Writer, Role::Writer]);
    b.write(vec![bound_facts(&lab, 1, "m2")]).unwrap();
    repair_commons(&lab, 6);
    lab.rotate(&[Role::Writer, Role::Writer]);
    let now_ms = || aterm_time::unix_millis().unwrap();
    let staged = a.roll(true, now_ms()).unwrap();
    assert!(!staged.sealed && staged.records > 0, "{staged:?}");
    repair_commons(&lab, 6);
    b.release(now_ms()).unwrap();
    repair_commons(&lab, 6);
    let rolled = a.roll(true, now_ms()).unwrap();
    assert!(rolled.sealed && rolled.waiting.is_empty(), "{rolled:?}");
    assert_eq!(a.generation().generation, 2);
    lab
}

/// What an idle hour counted and recorded ([`idle_hour`]): each counter of [`COUNTERS`] over its
/// first pass, and summed over its later ones; and what recording did for each file at each
/// pass.
struct Hour {
    first: [usize; COUNTERS.len()],
    idle: [usize; COUNTERS.len()],
    recorded: Vec<Vec<(String, Option<Recorded>)>>,
}

/// The idle hour's drive: the owner's materializer schedule ([`Files`], on this test's clock)
/// makes its first pass over every shared file of `lab`'s Mac 0 (an [`idle_lab`]) and then
/// [`IDLE_PASSES`] regular ones, with nothing arriving and nothing changing; every pass runs
/// [`super::files_pass`], the publisher then the materializer, on this thread, as the
/// `alab-files` thread runs it, and each counter of [`COUNTERS`] is read before and after each
/// pass. It holds the drive to being the one counted: every pass leaves every file `live`, the
/// first one shows each Mac's carried facts as its machine's `seats` and m1 as `here`, and every
/// counter counted that first pass's work but the fences and the kept key asks, whose number the
/// fixture does not fix, and the folder modes, which the folders a pass makes have already
/// (a pass sets one only where another hand changed it).
fn idle_hour(lab: &Lab) -> Hour {
    let paths = &lab.paths[0];
    let config = lab._home.path().join("aterm.toml");
    let root = crate::file::root(paths);
    let seats: Vec<(&str, String)> = [("m1", 0), ("m2", 1)]
        .into_iter()
        .map(|(label, n)| (label, keys::uuid32(&lab.machine(n))))
        .collect();
    let cadence = Cadence::LIVE;
    let mut files = Files::new(cadence, Instant::now(), GROWTH.count());
    let mut hour = Hour {
        first: [0; COUNTERS.len()],
        idle: [0; COUNTERS.len()],
        recorded: Vec::new(),
    };
    for pass in 0..=IDLE_PASSES {
        // The regular pass is due at its time whatever the growth count reads.
        let at = files.regular;
        assert!(
            files.due(at, GROWTH.count()),
            "the regular pass is due at its time"
        );
        let before = counters();
        let outcomes = super::files_pass(paths, &config, crate::admission::deadline());
        tally(
            if pass == 0 {
                &mut hour.first
            } else {
                &mut hour.idle
            },
            before,
        );
        assert!(
            !super::met_busy(&outcomes),
            "pass {pass} met a busy vault or store"
        );
        for file in crate::file::FILES {
            let status = File::at(&root, &file.file()).status();
            assert_eq!(
                (status.word, status.why),
                (Word::Live, None),
                "pass {pass}: {}",
                file.name
            );
        }
        hour.recorded.push(
            outcomes
                .iter()
                .map(|outcome| (outcome.name.to_string(), outcome.recorded))
                .collect(),
        );
        if pass == 0 {
            let machines = File::at(&root, &crate::file::MACHINES.file()).published();
            for (label, uuid) in &seats {
                let row = machines.named(label);
                assert_eq!(row.len(), 1, "{label}");
                assert_eq!(row[0].cell("seats"), uuid.as_str(), "{label}'s seats");
            }
            assert_eq!(machines.named("m1")[0].cell("here"), "yes");
            for ((name, _), counted) in COUNTERS.iter().zip(hour.first) {
                assert!(
                    counted > 0 || matches!(*name, "fences" | "kept_key_asks" | "chmods"),
                    "the first pass counted no {name}"
                );
            }
        }
    }
    hour
}

/// THE IDLE HOUR, COUNTED: the `files-idle` lane's drive (`targo --unverified verify perf
/// files-idle`, which runs this ignored test alone and judges the line it prints): the hour of
/// [`idle_hour`], and the line `FILES-IDLE {json}` reporting what its first pass counted and
/// what its later passes counted together. Nothing here judges a count.
/// NEGATIVE CONTROLS: with `att::carried` carrying no op (its loop over `ops` filtering
/// every op out), the second generation holds no facts row: `` assertion `left == right`
/// failed: both facts rows are carried into the second generation ``, left `0`, right `2`; with
/// the count in `file::materialize::Family::view` adding nothing, `` the first pass counted no
/// projections ``.
#[test]
#[ignore = "the `files-idle` lane's measurement: `targo --unverified verify perf files-idle`"]
fn an_idle_hour_reports_its_counts() {
    let lab = idle_lab();
    let paths = &lab.paths[0];
    let carried = Registry::at(paths.clone()).view().unwrap().owned[MACHINE_FACTS.name]
        .rows
        .len();
    assert_eq!(
        carried, 2,
        "both facts rows are carried into the second generation"
    );
    // An idle Mac's vault and table journals stood unchanged long before its hour, where this
    // lab's setup wrote them just now, and a file digest or a journal view of a file changed
    // within the settled time of a read reads it again at each ask
    // (`aterm_secrets::client::test_settled_after`). The hour waits that time out, and a tenth
    // of a second past it, so its passes read what an idle Mac's read whatever time the setup
    // took; no pass writes the vault or a table store.
    std::thread::sleep(aterm_secrets::client::test_settled_after() + Duration::from_millis(100));
    let Hour { first, idle, .. } = idle_hour(&lab);
    let object = |counted: [usize; COUNTERS.len()]| {
        Value::Object(
            COUNTERS
                .iter()
                .zip(counted)
                .map(|((name, _), n)| ((*name).to_owned(), json!(n)))
                .collect(),
        )
    };
    let report = json!({
        "schema": 1,
        "passes": IDLE_PASSES + 1,
        "files": crate::file::FILES.len(),
        "generation": Registry::at(paths.clone()).generation().generation,
        "carried": carried,
        "first": object(first),
        "idle": object(idle),
    });
    println!("FILES-IDLE {report}");
}

/// THE IDLE HOUR PROJECTS AND WRITES NOTHING (docs/DESIGN-shared-files-2026-11-11.md §4,
/// Materializer): over [`idle_hour`], after its first pass no pass makes a table view to project
/// from ([`files_materialize::PROJECTIONS`]), makes a durable write or sets a folder's mode
/// (`aterm_file::file::test_puts`, `test_chmods`), and every file's every later outcome is
/// [`Recorded::Settled`]: the status, the snapshot, its frontier, history and baseline, and the
/// face stay as the first pass left them, and so does the count of passes.
/// NEGATIVE CONTROLS, observed: with `files_pass` forcing every fifth pass (`Pass::again` set on
/// the passes the old `FILES_AGAIN` forced), `` the idle passes made no table view ``
/// `` left: 12 right: 0 ``; with `aterm_file::File::record` writing the status whatever it found
/// (its settled rule never met), `` the idle passes made no durable write `` `` left: 180
/// right: 0 ``; with `file::materialize::opened` finding no pass unchanged (`let unchanged =
/// false && …`, 2026-11-15, over the seven files), `` the idle passes made no table view ``
/// `` left: 60 right: 0 ``.
#[test]
fn an_idle_hour_projects_and_writes_nothing() {
    let lab = idle_lab();
    let Hour {
        first,
        idle,
        recorded,
    } = idle_hour(&lab);
    let count = |counted: [usize; COUNTERS.len()], name: &str| {
        COUNTERS
            .iter()
            .zip(counted)
            .find(|((counter, _), _)| *counter == name)
            .map(|(_, n)| n)
            .expect("a counter")
    };
    assert!(count(first, "projections") > 0 && count(first, "puts") > 0);
    for (name, what) in [
        ("projections", "table view"),
        ("puts", "durable write"),
        ("chmods", "folder mode set"),
    ] {
        assert_eq!(count(idle, name), 0, "the idle passes made no {what}");
    }
    for (pass, outcomes) in recorded.iter().enumerate().skip(1) {
        assert_eq!(outcomes.len(), crate::file::FILES.len());
        for (name, recorded) in outcomes {
            assert_eq!(*recorded, Some(Recorded::Settled), "pass {pass}: {name}");
        }
    }
}

/// What happens on a scripted materializer's clock ([`Script`]).
#[derive(Clone, Copy, Debug)]
enum Event {
    /// A table store of the process grows.
    Grow,
    /// Another process takes (`true`) or leaves (`false`) the Commons runtime.
    Elsewhere(bool),
}

/// More waits than any script here asks for: a materializer that wakes again and again with
/// nothing to do (a resting wait that kept a deadline nothing moves on, a pass due that never
/// runs) fails at this many instead of spinning without end.
const WAITS_MAX: usize = 10_000;

/// A materializer's clock and growth on a script ([`Bell`]): each wait runs the clock on to its
/// deadline or to the first scripted event after which it wakes, whichever is first; at the
/// script's `end` the stop is set. Nothing here waits on time, and a materializer that would never
/// end on it fails instead: one more wait than [`WAITS_MAX`], or a wait that sleeps through the
/// stop.
struct Script<'a> {
    now: Cell<Instant>,
    growth: Cell<u64>,
    events: RefCell<VecDeque<(Instant, Event)>>,
    end: Instant,
    elsewhere: &'a Elsewhere,
    stop: &'a AtomicBool,
    /// Each wait: its deadline, and when it woke.
    waits: RefCell<Vec<(Option<Instant>, Instant)>>,
    /// When each pass started.
    passes: RefCell<Vec<Instant>>,
}

impl<'a> Script<'a> {
    fn new(
        start: Instant,
        events: &[(Duration, Event)],
        end: Duration,
        elsewhere: &'a Elsewhere,
        stop: &'a AtomicBool,
    ) -> Self {
        Self {
            now: Cell::new(start),
            growth: Cell::new(0),
            events: RefCell::new(events.iter().map(|&(at, e)| (start + at, e)).collect()),
            end: start + end,
            elsewhere,
            stop,
            waits: RefCell::default(),
            passes: RefCell::default(),
        }
    }

    fn grow(&self) {
        self.growth.set(self.growth.get() + 1);
    }

    /// Run the materializer on this script from `start` until the script's end, each pass taking
    /// `pass` on its clock and calling `during` halfway through it, which answers whether the pass
    /// met a busy vault or store.
    fn run(
        &self,
        start: Instant,
        cadence: Cadence,
        pass: Duration,
        during: &dyn Fn(&Self) -> bool,
    ) {
        let mut files = Files::new(cadence, start, self.growth.get());
        materialize(&mut files, self, self.stop, self.elsewhere, &mut || {
            self.passes.borrow_mut().push(self.now.get());
            self.now.set(self.now.get() + pass / 2);
            let busy = during(self);
            self.now.set(self.now.get() + pass / 2);
            busy
        });
    }

    fn passes_from(&self, start: Instant) -> Vec<Duration> {
        self.passes.borrow().iter().map(|&at| at - start).collect()
    }
}

impl Bell for Script<'_> {
    fn now(&self) -> Instant {
        self.now.get()
    }

    fn wait(&self, until: Option<Instant>, seen: Option<u64>, woke: &dyn Fn() -> bool) -> u64 {
        let waits = self.waits.borrow().len();
        assert!(
            waits < WAITS_MAX,
            "the materializer spun: {waits} waits, the last {:#?}",
            &self.waits.borrow()[waits - 10..]
        );
        loop {
            if seen.is_some_and(|seen| self.growth.get() != seen) || woke() {
                break;
            }
            let next = self.events.borrow().front().map_or(self.end, |&(at, _)| at);
            if let Some(until) = until
                && until <= next
            {
                self.now.set(self.now.get().max(until));
                break;
            }
            self.now.set(self.now.get().max(next));
            let event = self.events.borrow_mut().pop_front();
            match event {
                Some((_, Event::Grow)) => self.grow(),
                Some((_, Event::Elsewhere(set))) => self.elsewhere.set(set),
                None => assert!(
                    !self.stop.swap(true, Ordering::AcqRel),
                    "a wait slept through its stop at the script's end"
                ),
            }
        }
        self.waits.borrow_mut().push((until, self.now.get()));
        self.growth.get()
    }
}

const SECOND: Duration = Duration::from_secs(1);

/// ATERM-KI-1108-2: AN IDLE MATERIALIZER WAKES ONLY FOR ITS PASSES. With no store growing, its
/// thread wakes once at [`Cadence::first`] and once each [`Cadence::every`] after, and for nothing
/// between: ten minutes are eleven wakes, each a pass, where a look every second made six hundred.
/// NEGATIVE CONTROL: a wait capped at a second, as the look was (`until.min(now + 1 s)` in
/// `materialize`), fails the count with 606 wakes.
#[test]
fn an_idle_materializer_wakes_once_for_each_regular_pass() {
    let cadence = Cadence::LIVE;
    let (elsewhere, stop) = (Elsewhere::default(), AtomicBool::new(false));
    let start = Instant::now();
    let ten = Duration::from_secs(600);
    let script = Script::new(start, &[], cadence.first + ten, &elsewhere, &stop);
    script.run(start, cadence, Duration::ZERO, &|_| false);
    let passes = script.passes_from(start);
    let want: Vec<Duration> = (0..=10)
        .map(|k| cadence.first + cadence.every * k)
        .collect();
    assert_eq!(passes, want);
    let waits = script.waits.borrow();
    assert_eq!(
        waits.len(),
        passes.len() + 1,
        "a wake for each pass, and the stop's: {waits:#?}"
    );
    assert!(stop.load(Ordering::Acquire));
}

/// A store that grows is materialized [`Cadence::settle`] after the first growth the wait woke on,
/// and not before; growth inside the settle wakes nothing more, so a burst is one wake and one
/// pass.
/// NEGATIVE CONTROLS: a wait that ignores growth (`seen` passed as `None` in `materialize`) leaves
/// the burst to the regular pass, a minute on: only the first pass runs; a wait that asks for
/// growth inside the settle too (`seen` as `Some(files.seen)` whatever `moved_at` holds) wakes at
/// once on the moved count until the settle ends, and the script fails it at [`WAITS_MAX`].
#[test]
fn growth_is_materialized_once_it_settles_and_not_before() {
    let cadence = Cadence::LIVE;
    let (elsewhere, stop) = (Elsewhere::default(), AtomicBool::new(false));
    let start = Instant::now();
    let grown = cadence.first + 10 * SECOND;
    let burst = [
        (grown, Event::Grow),
        (grown + SECOND / 2, Event::Grow),
        (grown + SECOND, Event::Grow),
    ];
    let script = Script::new(
        start,
        &burst,
        cadence.first + 30 * SECOND,
        &elsewhere,
        &stop,
    );
    script.run(start, cadence, Duration::ZERO, &|_| false);
    assert_eq!(
        script.passes_from(start),
        [cadence.first, grown + cadence.settle],
        "the first pass, then one the settle after the burst's first growth"
    );
    let woke: Vec<Duration> = script
        .waits
        .borrow()
        .iter()
        .map(|&(_, at)| at - start)
        .collect();
    assert_eq!(
        woke,
        [
            cadence.first,
            grown,
            grown + cadence.settle,
            cadence.first + 30 * SECOND
        ],
        "the first pass, the burst's first growth, its pass, the stop"
    );
}

/// A growth that lands while a pass runs, which no wait was there to see, is materialized within
/// the settle of that pass's end.
/// NEGATIVE CONTROL: marking the growth seen once a pass ends, as a bell cleared after each pass
/// would (`files.seen` read again after `pass` in `materialize`), leaves it to the regular pass, a
/// minute on: only the first pass runs.
#[test]
fn growth_during_a_pass_is_materialized_within_the_settle() {
    let cadence = Cadence::LIVE;
    let (elsewhere, stop) = (Elsewhere::default(), AtomicBool::new(false));
    let start = Instant::now();
    let slow = 4 * SECOND;
    let script = Script::new(start, &[], cadence.first + 30 * SECOND, &elsewhere, &stop);
    let grew_once = Cell::new(false);
    script.run(start, cadence, slow, &|s| {
        if !grew_once.replace(true) {
            s.grow();
        }
        false
    });
    assert_eq!(
        script.passes_from(start),
        [cadence.first, cadence.first + slow + cadence.settle],
        "the growth made during the first pass, within the settle of its end"
    );
}

/// A pass that met a busy vault or store is followed by another [`Cadence::busy`] after its end,
/// not a minute on.
/// NEGATIVE CONTROL: `files.met_busy(..)` dropped from `materialize` leaves the next pass to the
/// regular one: only the first pass runs.
#[test]
fn a_pass_that_met_a_busy_store_is_followed_once_busy_has_passed() {
    let cadence = Cadence::LIVE;
    assert!(cadence.busy < cadence.every);
    let (elsewhere, stop) = (Elsewhere::default(), AtomicBool::new(false));
    let start = Instant::now();
    let end = cadence.first + cadence.busy + SECOND;
    let script = Script::new(start, &[], end, &elsewhere, &stop);
    let first = Cell::new(true);
    script.run(start, cadence, Duration::ZERO, &|_| first.replace(false));
    assert_eq!(
        script.passes_from(start),
        [cadence.first, cadence.first + cadence.busy],
        "the busy pass, then one busy after it"
    );
}

/// While another process owns the Commons runtime the materializer passes on neither growth nor
/// time: it waits with no deadline, and growth made while it rests wakes nothing. Once the owner is
/// back it takes up at once the regular pass that came due meanwhile.
/// NEGATIVE CONTROL: a wait that does not wake on the owner's moves (`elsewhere.is_set() !=
/// resting` dropped from `woke`) rests from its next regular deadline on, with that deadline, and
/// never passes again.
#[test]
fn a_materializer_resting_elsewhere_waits_with_no_deadline() {
    let cadence = Cadence::LIVE;
    let (elsewhere, stop) = (Elsewhere::default(), AtomicBool::new(false));
    let start = Instant::now();
    let away = cadence.first + SECOND;
    let back = away + 5 * cadence.every;
    let events = [
        (away, Event::Elsewhere(true)),
        (away + cadence.every, Event::Grow),
        (back, Event::Elsewhere(false)),
    ];
    let script = Script::new(start, &events, back + 10 * SECOND, &elsewhere, &stop);
    script.run(start, cadence, Duration::ZERO, &|_| false);
    let waits = script.waits.borrow();
    let resting: Vec<&(Option<Instant>, Instant)> = waits
        .iter()
        .filter(|(_, woke)| *woke > start + away && *woke <= start + back)
        .collect();
    assert!(
        !resting.is_empty() && resting.iter().all(|(until, _)| until.is_none()),
        "{waits:#?}"
    );
    assert_eq!(
        script.passes_from(start),
        [cadence.first, back],
        "no pass while resting; the one that came due, once the owner is back"
    );
}

/// The live materializer on a stand-in lab: a store growing wakes a thread whose regular pass is
/// an hour off, and it materializes the record once the growth settles; a thread resting while
/// another process owns Commons wakes when it leaves, and materializes the growth it rested
/// through; and dropping it ends its thread at once. Each is a must-happen wait, bounded by the
/// one-minute hang detector. A pass that meets a busy vault (the write's own hold, on a loaded
/// Mac) is followed a settle later, so no busy outcome stands for the hour.
/// NEGATIVE CONTROLS (each run by this test's name alone, then undone): `Growth::grew` that never
/// rings leaves the first wait to the detector; `Elsewhere::set` without its `GROWTH.ring()`
/// leaves the second; `Materializer`'s drop without its `GROWTH.ring()` leaves the third. They
/// hold run alone: the bell is the process's, which every other test's appends and rings reach;
/// `file::growth`'s own tests prove it on instances nothing else reaches.
#[test]
fn the_live_materializer_wakes_on_growth_on_the_owner_back_and_on_its_stop() {
    let cadence = Cadence {
        first: Duration::from_secs(3600),
        settle: Duration::from_millis(20),
        every: Duration::from_secs(3600),
        busy: Duration::from_millis(20),
    };
    let lab = Lab::new(&[Role::Writer]);
    let paths = &lab.paths[0];
    let config = lab._home.path().join("aterm.toml");
    let registry = Registry::at(paths.clone());
    registry.open().unwrap();
    let machines = File::at(&crate::file::root(paths), &crate::file::MACHINES.file());
    let named = |label: &str| machines.read().named(label).len() == 1;
    let write = |label: &str| {
        let machine = keys::machine_pk(label);
        registry
            .write(vec![vec![
                put(&MACHINE, &machine, "label", json!(label)),
                put(&MACHINE, &machine, "status", json!("active")),
            ]])
            .unwrap();
    };
    let stopped = |files: Materializer| {
        let (done, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(files);
            let _ = done.send(());
        });
        aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "the thread's end", || {
            wait.try_recv().ok()
        });
    };

    let elsewhere = Arc::new(Elsewhere::default());
    let files = Materializer::start(paths.clone(), config.clone(), cadence, elsewhere.clone())
        .expect("its thread");
    write("m7");
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "m7 materialized", || {
        named("m7").then_some(())
    });
    stopped(files);

    let elsewhere = Arc::new(Elsewhere::default());
    elsewhere.set(true);
    let files = Materializer::start(paths.clone(), config.clone(), cadence, elsewhere.clone())
        .expect("its thread");
    write("m8");
    elsewhere.set(false);
    aterm_spec::wait::until_within(aterm_spec::wait::MUST_HAPPEN, "m8 materialized", || {
        named("m8").then_some(())
    });
    stopped(files);
}
