// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One read of the home's checkouts per Mac (KI-1108-2): the node job's own read
//! ([`read_checkouts_when_due`]) stands aside while a window answers surveys, keeps the window's
//! read on its clock with a survey that names no peer while a deny list is read, comes back at the
//! next tick once no window answers, and runs each period on the facts push's phase. Counting
//! closures and an injected clock: nothing timed, no home read.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aterm_spec::wait::MUST_HAPPEN;

use super::super::{CheckoutsClock, FactsPush, WindowAsk, read_checkouts_when_due};

/// The node job's period for the home's checkouts.
const EVERY: Duration = Duration::from_secs(180);

/// How many reads of its own and asks of its windows a run of ticks made.
#[derive(Default)]
struct Counts {
    reads: Cell<usize>,
    asks: Cell<usize>,
}

impl Counts {
    /// One tick at `now`, a window answering surveys or not (`window_live`), the facts push as the
    /// tick reads it (`facts`, `None` with no deny list read), its period `every`; when the clock
    /// is next due.
    fn tick(
        &self,
        clock: &mut CheckoutsClock,
        (window_live, now): (bool, Instant),
        facts: Option<FactsPush>,
        every: Duration,
    ) -> Option<Instant> {
        read_checkouts_when_due(
            (window_live, now),
            facts,
            clock,
            every,
            || self.reads.set(self.reads.get() + 1),
            || self.asks.set(self.asks.get() + 1),
        )
    }

    /// The reads and asks so far.
    fn made(&self) -> (usize, usize) {
        (self.reads.get(), self.asks.get())
    }
}

/// The facts push last at `at`, gone at this tick or not.
fn push(at: Instant, went: bool) -> Option<FactsPush> {
    Some(FactsPush { at, went })
}

/// WHILE A WINDOW ANSWERS, THE NODE JOB READS NOTHING; ONCE NONE DOES, IT READS AT THE NEXT TICK:
/// every survey it serves then is answered from the window's own read, so its own fed no reader.
/// Its last read is forgotten while it stands aside, so the tick after the window goes reads at
/// once, not a period after the read before the window came. Memory only.
/// NEGATIVE CONTROLS: the clock that read whatever answered (the one before) reads at the first
/// gated tick, which (a) counts; one that sets its read while it stands aside reads nothing at
/// (b), whose period no read is ever within.
#[test]
fn a_window_that_answers_stands_the_node_read_aside() {
    let t0 = Instant::now();
    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    // (a)
    counts.tick(&mut clock, (true, t0), push(t0, false), Duration::ZERO);
    assert_eq!(counts.made(), (0, 1), "(a) no read, one ask of the window");
    assert_eq!(clock.read, None, "(a) the clock's read stays unset");
    // (b)
    counts.tick(&mut clock, (false, t0), push(t0, false), Duration::MAX);
    assert_eq!(counts.made(), (1, 1), "(b) no window: a read at once");

    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    counts.tick(&mut clock, (false, t0), push(t0, false), EVERY);
    assert_eq!(counts.made(), (1, 0), "a windowless first tick reads");
    let s = Duration::from_secs;
    counts.tick(&mut clock, (true, t0 + s(1)), push(t0, false), EVERY);
    assert_eq!(
        counts.made(),
        (1, 1),
        "a window came: it is asked, nothing read"
    );
    counts.tick(&mut clock, (false, t0 + s(2)), push(t0, false), EVERY);
    assert_eq!(
        counts.made(),
        (2, 1),
        "the window went: a read at the next tick, not a period after the one before it"
    );
}

/// A WINDOWLESS MAC'S READ COMES EVERY PERIOD: due `every` after the one before, by the tick's
/// own clock, and each tick names that deadline for the job's wait. No deny list is read, so no
/// facts push sets the phase. Memory only.
/// NEGATIVE CONTROL: a tick one second short of the period reads nothing.
#[test]
fn a_windowless_read_comes_every_period() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let counts = Counts::default();
    let mut clock = CheckoutsClock {
        read: Some(t0),
        asked: None,
    };
    assert_eq!(
        counts.tick(&mut clock, (false, t0 + s(179)), None, EVERY),
        Some(t0 + EVERY)
    );
    assert_eq!(counts.made(), (0, 0), "CONTROL: a second short, nothing");
    assert_eq!(
        counts.tick(&mut clock, (false, t0 + s(180)), None, EVERY),
        Some(t0 + s(360))
    );
    assert_eq!(counts.made(), (1, 0), "at the period, one read");
}

/// THE WINDOW'S READ IS STILL CLOCKED: while a window answers and a deny list is read but no facts
/// push went at the tick (a generation's first period, a window just come), the due tick asks it
/// exactly one survey that names no peer, whose answer starts the window's read where that is due;
/// a tick within the period asks none; and each tick names the ask's deadline for the job's wait.
/// Memory only.
/// NEGATIVE CONTROL: take the clock's own ask out and the count stays 0, which the first
/// judgment fails.
#[test]
fn the_windows_read_is_still_clocked() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    assert_eq!(
        counts.tick(&mut clock, (true, t0), push(t0, false), EVERY),
        Some(t0 + EVERY)
    );
    assert_eq!(
        counts.made(),
        (0, 1),
        "no facts push asked: the tick asks once"
    );
    counts.tick(&mut clock, (true, t0 + s(179)), push(t0, false), EVERY);
    assert_eq!(counts.made(), (0, 1), "within the period, no ask");
    assert_eq!(
        counts.tick(&mut clock, (true, t0 + s(180)), push(t0, false), EVERY),
        Some(t0 + s(360))
    );
    assert_eq!(counts.made(), (0, 2), "at the period, one more");
}

/// A FACTS PUSH THAT WENT STANDS FOR THE ASK: where the facts push went at the tick, it asked the
/// window that answers, so the clock asks none, and its next is due with the push's. Memory only.
/// NEGATIVE CONTROL: the same tick with the push not gone asks once.
#[test]
fn a_facts_push_that_went_stands_for_the_ask() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    let pushed = t0 + s(30);
    assert_eq!(
        counts.tick(&mut clock, (true, pushed), push(pushed, true), EVERY),
        Some(pushed + EVERY),
        "due with the push's next"
    );
    assert_eq!(
        counts.made(),
        (0, 0),
        "the push asked: no ask of the clock's"
    );
    assert_eq!(
        counts.tick(
            &mut clock,
            (true, pushed + s(1)),
            push(pushed, false),
            EVERY
        ),
        Some(pushed + EVERY)
    );
    assert_eq!(counts.made(), (0, 0), "within the push's period, no ask");

    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    counts.tick(&mut clock, (true, pushed), push(pushed, false), EVERY);
    assert_eq!(counts.made(), (0, 1), "CONTROL: no push gone, one ask");
}

/// NO DENY LIST READ, NO ASK: the job then serves no peer's survey and pushes no facts, so no
/// reader takes the window's read through it; it asks nothing, reads nothing while the window
/// answers, and names no deadline. Once a list is read the window is asked at that tick. Memory
/// only.
/// NEGATIVE CONTROL: the clock that asked whether or not a list was read asks at the first tick.
#[test]
fn no_deny_list_read_asks_the_window_nothing() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    assert_eq!(counts.tick(&mut clock, (true, t0), None, EVERY), None);
    assert_eq!(counts.made(), (0, 0), "no list read: no ask, no read");
    assert_eq!(
        counts.tick(&mut clock, (true, t0 + s(400)), None, EVERY),
        None
    );
    assert_eq!(counts.made(), (0, 0), "nor periods later");
    counts.tick(&mut clock, (true, t0 + s(401)), push(t0, false), EVERY);
    assert_eq!(
        counts.made(),
        (0, 1),
        "a list read: the window asked at once"
    );
}

/// THE CLOCK KEEPS THE FACTS PUSH'S PHASE: a read made once a window went, and an ask made once a
/// window came, run their period from the facts push's instant where that is within it, so each
/// is next due with the push's (one wake) and comes again at every push after, never on a phase
/// of its own a full pass apart. A push out of the period sets no phase. Memory only.
/// NEGATIVE CONTROL: a clock that runs its period from the tick it acted at names a deadline 90 s
/// past the push's, which the first judgment fails.
#[test]
fn the_clock_keeps_the_facts_pushs_phase() {
    let t0 = Instant::now();
    let s = Duration::from_secs;
    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    counts.tick(&mut clock, (true, t0), push(t0, true), EVERY);
    assert_eq!(
        counts.tick(&mut clock, (false, t0 + s(90)), push(t0, false), EVERY),
        Some(t0 + EVERY),
        "the window went mid-period: its read is due with the push's next"
    );
    assert_eq!(counts.made(), (1, 0), "read at once");
    assert_eq!(
        counts.tick(
            &mut clock,
            (false, t0 + EVERY),
            push(t0 + EVERY, true),
            EVERY
        ),
        Some(t0 + 2 * EVERY),
        "at the push's next it reads again, due with the one after"
    );
    assert_eq!(counts.made(), (2, 0));

    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    assert_eq!(
        counts.tick(&mut clock, (true, t0 + s(60)), push(t0, false), EVERY),
        Some(t0 + EVERY),
        "a window came before the generation's first push: asked, due with that push"
    );
    assert_eq!(counts.made(), (0, 1));
    assert_eq!(
        counts.tick(
            &mut clock,
            (true, t0 + EVERY),
            push(t0 + EVERY, true),
            EVERY
        ),
        Some(t0 + 2 * EVERY)
    );
    assert_eq!(
        counts.made(),
        (0, 1),
        "that push asked: none of the clock's"
    );

    let counts = Counts::default();
    let mut clock = CheckoutsClock::default();
    assert_eq!(
        counts.tick(&mut clock, (false, t0 + s(500)), push(t0, false), EVERY),
        Some(t0 + s(500) + EVERY),
        "a push out of the period sets no phase"
    );
}

/// THE CLOCK'S OWN ASK HOLDS NO PASS: [`WindowAsk`] runs it on a thread of its own, so the pass
/// that starts it goes on while the windows answer, and one at a time: a start while one is out
/// starts none, and once that one ended the next starts. Real threads; each wait is a one-minute
/// hang detector.
/// NEGATIVE CONTROL: an ask made on the caller's thread holds its start until its release, which
/// comes only after the start returned, so it ends unreleased at the hang detector and the second
/// start runs another: the first judgment after it fails.
#[test]
fn the_clocks_own_ask_holds_no_pass() {
    let mut asking = WindowAsk::default();
    let (release, released) = std::sync::mpsc::channel::<()>();
    let (said, heard) = std::sync::mpsc::channel::<bool>();
    assert!(
        asking.start(move || {
            let _ = said.send(released.recv_timeout(MUST_HAPPEN).is_ok());
        }),
        "the first ask starts"
    );
    let more = Arc::new(AtomicUsize::new(0));
    let second = Arc::clone(&more);
    assert!(
        !asking.start(move || {
            second.fetch_add(1, Ordering::AcqRel);
        }),
        "one still out: no second"
    );
    let _ = release.send(());
    assert_eq!(
        heard.recv_timeout(MUST_HAPPEN),
        Ok(true),
        "the ask waited beside its caller for a release made after its start returned"
    );
    aterm_spec::wait::until_within(MUST_HAPPEN, "the ended ask frees the next", || {
        let next = Arc::clone(&more);
        asking
            .start(move || {
                next.fetch_add(1, Ordering::AcqRel);
            })
            .then_some(())
    });
    aterm_spec::wait::until_within(MUST_HAPPEN, "the next ask runs", || {
        (more.load(Ordering::Acquire) == 1).then_some(())
    });
}
