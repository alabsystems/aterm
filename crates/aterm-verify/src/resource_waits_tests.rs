// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn simultaneous_resource_shortfalls_remain_distinct() {
    use aterm_capacity::admit::{Short, Shortfall};
    let reason = Reason::ledger(&Shortfall {
        shorts: vec![Short::Pressure, Short::Cores, Short::Turn { holder: None }],
        holders: Vec::new(),
    });
    assert_eq!(reason.kinds, ["core", "ledger", "memory"]);
    let b = Planned {
        index: 0,
        width: 2,
        charge: 2,
        peak: 4,
        predicted: None,
    };
    let reason = Reason::local(
        &b,
        DispatchResources {
            held: 3,
            share: 4,
            held_bytes: 3,
            memory: Some(5),
            live: 3.0,
        },
    );
    assert_eq!(reason.kinds, ["core", "memory"]);
}

#[test]
fn wait_transitions_cover_queue_time_without_counting_execution_or_splitting_each_poll() {
    let mut book = Book(vec![Some(Waiting::opened(
        Reason::ordered(),
        Duration::ZERO,
    ))]);
    let first = book
        .change(0, Some(Reason::fifo()), Duration::from_secs(2))
        .unwrap();
    assert_eq!((first.from.as_secs(), first.to.as_secs()), (0, 2));
    assert!(
        book.change(0, Some(Reason::fifo()), Duration::from_secs(3))
            .is_none()
    );
    let second = book
        .change(
            0,
            Some(Reason::unreachable("memory need cannot fit".into())),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(second.reason.kinds, ["core"]);
    assert_eq!((second.from.as_secs(), second.to.as_secs()), (2, 5));
    let third = book.change(0, None, Duration::from_secs(9)).unwrap();
    assert_eq!((third.from.as_secs(), third.to.as_secs()), (5, 9));
    assert!(
        book.change(0, None, Duration::from_secs(60)).is_none(),
        "execution and a repeated final flush cannot become queue time"
    );
}

/// One step of a book's history in [`a_lock_only_look_changes_no_wait_reason`].
#[derive(Clone, Copy, Debug)]
enum Step {
    /// A look decided: the binary waits for the reason of this number ([`decided`]).
    Decided(usize),
    /// A look met only the ledger's lock.
    Lock,
    /// The binary started.
    Started,
}

/// The reasons a look may decide: memory, memory again with another detail (one class), the
/// release's pool, an I/O fault of the ledger, and a need that cannot fit.
fn decided(which: usize) -> Reason {
    use aterm_capacity::admit::{Short, Shortfall};
    let ledger = |short| {
        Reason::ledger(&Shortfall {
            shorts: vec![short],
            holders: Vec::new(),
        })
    };
    match which {
        0 => ledger(Short::Pressure),
        1 => {
            let mut reason = ledger(Short::Pressure);
            reason.detail = "memory, said again".into();
            reason
        }
        2 => Reason::fifo(),
        3 => ledger(Short::Ledger("the ledger's folder is unreadable".into())),
        _ => Reason::unreachable("its peak cannot fit this machine".into()),
    }
}

/// A row as an event writes it, but for its count of undecided looks: which binary, when it
/// opened and closed, its class, and its first and last detail.
type Row = (
    usize,
    Duration,
    Duration,
    (&'static str, Vec<&'static str>),
    String,
    String,
);

fn row(closed: &Closed) -> Row {
    let (source, kinds) = closed.reason.classification();
    (
        closed.index,
        closed.from,
        closed.to,
        (source, kinds.to_vec()),
        closed.first_detail.clone(),
        closed.reason.detail.clone(),
    )
}

/// Run `steps` on a fresh book, one a second, a lock-only look as `lock` files it, and close what
/// is open at the end: the rows written, and the undecided looks they counted.
fn history(steps: &[Step], lock: &dyn Fn() -> Wait) -> (Vec<Row>, u64) {
    let plan = crate::test_plan::plan(&[None], &[], 1, None, 1);
    let mut book = Book::new(&plan, 1);
    let mut closed = Vec::new();
    for (at, step) in steps.iter().enumerate() {
        let at = Duration::from_secs(u64::try_from(at).unwrap() + 1);
        closed.extend(match *step {
            Step::Decided(which) => book.wait(0, Wait::For(decided(which)), at),
            Step::Lock => book.wait(0, lock(), at),
            Step::Started => book.change(0, None, at),
        });
    }
    let end = Duration::from_secs(u64::try_from(steps.len()).unwrap() + 1);
    closed.extend(book.change(0, None, end));
    let counted = closed.iter().map(|closed| closed.undecided).sum();
    (closed.iter().map(row).collect(), counted)
}

/// Every history of up to `len` steps over the decided reasons, the lock and a start.
fn histories(len: usize) -> Vec<Vec<Step>> {
    let alphabet: Vec<Step> = (0..5)
        .map(Step::Decided)
        .chain([Step::Lock, Step::Started])
        .collect();
    let mut all = vec![Vec::new()];
    let mut last = vec![Vec::new()];
    for _ in 0..len {
        last = last
            .iter()
            .flat_map(|prefix: &Vec<Step>| {
                alphabet.iter().map(move |step| {
                    let mut next = prefix.clone();
                    next.push(*step);
                    next
                })
            })
            .collect();
        all.extend(last.iter().cloned());
    }
    all
}

/// The undecided looks a history must count: each lock-only look while the binary's row is open
/// (before it started, or after a decision reopened it).
fn lock_looks_on_an_open_row(steps: &[Step]) -> u64 {
    let mut open = true;
    let mut counted = 0;
    for step in steps {
        match step {
            Step::Decided(_) => open = true,
            Step::Lock if open => counted += 1,
            Step::Lock => {}
            Step::Started => open = false,
        }
    }
    counted
}

/// THE LAW. A look that met only the ledger's lock read nothing and decides nothing: for any
/// interleaving of decided reasons, lock-only looks and a start, each within a minute of the
/// row's last decision, the book writes exactly the rows it writes with the lock-only looks
/// removed, and each row counts the lock-only looks it saw (its event's `undecided_looks`). An I/O
/// fault of the ledger stays a reason; lock-only looks that go on for [`LOCK_HELD`] from the
/// first of them make the lock the reason, naming its holder and the whole span, and one after a
/// quiet stretch does not. NEGATIVE CONTROL: a lock-only look filed as a reason, as
/// [`Reason::ledger`] files the lock, closes the row it falls in, and the comparison fails.
#[test]
fn a_lock_only_look_changes_no_wait_reason() {
    use aterm_capacity::admit::{Short, Shortfall};
    let holder = Some(4242);
    let unread = || Wait::Unread(holder);
    let mut compared = 0;
    for steps in histories(5) {
        let decided: Vec<Step> = steps
            .iter()
            .copied()
            .filter(|step| !matches!(step, Step::Lock))
            .collect();
        let (with, counted) = history(&steps, &unread);
        let (without, none) = history(&decided, &unread);
        assert_eq!(none, 0);
        // The rows are compared at the times of the decided history: a lock-only look takes a
        // second of the history's clock, so each history's rows are mapped to step numbers.
        assert_eq!(
            renumbered(&with, &steps),
            renumbered(&without, &decided),
            "{steps:?}"
        );
        assert_eq!(counted, lock_looks_on_an_open_row(&steps), "{steps:?}");
        compared += 1;
    }
    assert_eq!(compared, (0..=5).map(|n| 7_usize.pow(n)).sum::<usize>());

    // The control: today's filing of the lock as a reason of its own class.
    let filed = || {
        Wait::For(Reason::ledger(&Shortfall {
            shorts: vec![Short::Turn { holder }],
            holders: Vec::new(),
        }))
    };
    let steps = [Step::Decided(0), Step::Lock, Step::Decided(1)];
    let kept = [Step::Decided(0), Step::Decided(1)];
    assert_ne!(
        renumbered(&history(&steps, &filed).0, &steps),
        renumbered(&history(&kept, &filed).0, &kept),
        "a lock-only look filed as a reason closes the memory row it falls in"
    );

    // What a ledger look found: the lock alone decides nothing, an I/O fault is a reason.
    let short = |shorts| Shortfall {
        shorts,
        holders: Vec::new(),
    };
    assert!(matches!(
        Wait::ledger(&short(vec![Short::Turn { holder }])),
        Wait::Unread(Some(4242))
    ));
    match Wait::ledger(&short(vec![Short::Ledger("disk".into())])) {
        Wait::For(reason) => assert_eq!(reason.classification(), ("ledger", &["ledger"][..])),
        Wait::Unread(_) => panic!("an I/O fault of the ledger is a reason"),
    }

    // Lock-only looks for a minute from the first of them make the lock the reason, naming its
    // holder; the looks before it are counted in the row it closes, and its span runs on from
    // that first look while the looks meet only the lock.
    let plan = crate::test_plan::plan(&[None], &[], 1, None, 1);
    let mut book = Book::new(&plan, 1);
    let secs = Duration::from_secs;
    assert!(book.wait(0, Wait::For(decided(0)), secs(1)).is_some());
    for at in [2, 30, 61] {
        assert!(book.wait(0, unread(), secs(at)).is_none(), "{at} s");
    }
    let memory = book
        .wait(0, unread(), secs(62))
        .expect("the lock is the reason");
    assert_eq!(memory.reason.classification(), ("ledger", &["memory"][..]));
    assert_eq!(
        (memory.from, memory.to, memory.undecided),
        (secs(1), secs(62), 3)
    );
    assert!(
        book.wait(0, unread(), secs(150)).is_none(),
        "the lock stays the reason"
    );
    let lock = book
        .wait(0, Wait::For(decided(0)), secs(155))
        .expect("a read decides again");
    assert_eq!(lock.reason.classification(), ("ledger", &["ledger"][..]));
    assert!(
        lock.reason.detail.contains("pid 4242") && lock.reason.detail.contains("2 min"),
        "the whole span since the first lock-only look: {}",
        lock.reason.detail
    );
    assert_eq!(
        (lock.from, lock.to, lock.undecided),
        (secs(62), secs(155), 0)
    );

    // One lock-only look after a quiet stretch with no look is no minute of them: the stretch
    // decided nothing either way, and the minute runs from that look.
    let mut book = Book::new(&plan, 1);
    assert!(book.wait(0, Wait::For(decided(0)), secs(1)).is_some());
    assert!(
        book.wait(0, unread(), secs(300)).is_none(),
        "after a quiet stretch"
    );
    assert!(book.wait(0, unread(), secs(359)).is_none());
    let memory = book.wait(0, unread(), secs(360)).expect("a minute of them");
    assert_eq!(
        (memory.from, memory.to, memory.undecided),
        (secs(1), secs(360), 2)
    );
}

/// `rows`, written by a history of `steps` one a second, with each instant as the number of
/// decided steps before it: two histories that differ only by lock-only looks compare equal.
fn renumbered(rows: &[Row], steps: &[Step]) -> Vec<Row> {
    let at = |instant: Duration| {
        let step = usize::try_from(instant.as_secs()).unwrap();
        let before = steps
            .iter()
            .take(step.saturating_sub(1))
            .filter(|step| !matches!(step, Step::Lock))
            .count();
        Duration::from_secs(u64::try_from(before).unwrap())
    };
    rows.iter()
        .map(|(index, from, to, class, first, last)| {
            (
                *index,
                at(*from),
                at(*to),
                class.clone(),
                first.clone(),
                last.clone(),
            )
        })
        .collect()
}

/// A dispatch's trace writes no event for a lock-only look, and the event that closes a row
/// carries the lock-only looks it saw.
#[test]
fn the_closing_event_counts_the_looks_that_decided_nothing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let events = dir.path().join("events");
    let ctx = crate::Ctx::new(
        dir.path().to_owned(),
        crate::Scope::workspace(),
        crate::EnvSnapshot::default(),
        dir.path().join("scratch"),
    )
    .with_events(Some(crate::event_log::EventLog::open(&events).unwrap()));
    let bins = [Binary {
        header: "t0".into(),
        spec: "-p x --test t0".into(),
        record: crate::testrun::Record {
            cwd: dir.path().to_owned(),
            argv: vec!["/r/t0".into()],
            env: Vec::new(),
        },
        harness: false,
    }];
    let plan = crate::test_plan::plan(&[None], &[], 1, None, 1);
    let trace = Trace::new(Some(&ctx), &bins, &plan, Instant::now());
    let written = || {
        std::fs::read_to_string(&events)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains("test-dispatch-wait"))
            .count()
    };
    let secs = Duration::from_secs;
    for at in 1..=3 {
        trace.waits_at(0, Wait::Unread(Some(4242)), secs(at));
    }
    assert_eq!(written(), 0, "a lock-only look writes nothing");
    trace.waits_at(0, Wait::For(decided(0)), secs(4));
    trace.waits_at(0, Wait::Unread(None), secs(5));
    trace.started_at(0, secs(6));
    drop(trace);
    let rows: Vec<aterm_json::Value> = std::fs::read_to_string(&events)
        .unwrap()
        .lines()
        .filter(|line| line.contains("test-dispatch-wait"))
        .map(|line| {
            aterm_json::from_str(line.trim_start_matches("verify-event: ")).expect("an event")
        })
        .collect();
    let counts: Vec<_> = rows
        .iter()
        .map(|row| row["undecided_looks"].as_u64())
        .collect();
    assert_eq!(counts, [Some(3), Some(1)], "{rows:?}");
}
