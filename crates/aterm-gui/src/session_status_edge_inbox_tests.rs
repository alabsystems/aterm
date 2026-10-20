// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE EDGED INBOX through the real status sweep (card engine §3.5, §7 "Edged
//! inbox"), with no process and no PTY: the group in front is the injected
//! probe's (`SessionFactory::fg_probe`), asked about a master no file holds,
//! and the program resolver's reads are scripted
//! (`ProgramResolver::with_lookup`). A drawer's edge on a session that is not
//! due is named on the next wake; a visit the PTY reader's hold refuses goes
//! back in the inbox with no observation interval charged. NEGATIVE CONTROL:
//! without the inbox, the same sweep names nothing before the session's next
//! observation.

use std::cell::Cell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::program::{LeaderFacts, ProgramResolver};
use crate::{App, WindowId};

thread_local! {
    static FRONT: Cell<i32> = const { Cell::new(-1) };
}

/// The injected probe: the group this test put in front.
fn front(_master: i32) -> i32 {
    FRONT.with(Cell::get)
}

// Groups no process holds: past every pid the kernel issues.
const SHELL: i32 = 2_000_000_101;
const CLAUDE: i32 = 2_000_000_102;

struct Bed {
    app: App,
    session: u64,
    /// Every group the resolver was asked to name, in order.
    asked: Arc<Mutex<Vec<i32>>>,
}

impl Bed {
    /// A headless App whose front session's shell is in front and named,
    /// and whose next observation is a whole interval away.
    fn new() -> (Self, Instant) {
        let mut app = App::headless_for_test();
        app.session_factory.fg_probe = front;
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&asked);
        app.session_status.programs = ProgramResolver::with_lookup(move |pgid, _shell| {
            log.lock().unwrap().push(pgid);
            LeaderFacts {
                program: Some(if pgid == CLAUDE { "claude" } else { "zsh" }.into()),
                ..LeaderFacts::default()
            }
        });
        let session = app
            .front_terminal(WindowId(0))
            .expect("a front terminal")
            .session;
        // The sweep asks the probe only for a session with a master: an fd
        // number no file holds (its shell pid stays the stub's).
        app.pool
            .sessions
            .get_mut(&session)
            .expect("pooled")
            .session
            .master = i32::MAX;
        FRONT.with(|f| f.set(SHELL));
        let mut bed = Self {
            app,
            session,
            asked,
        };
        let t0 = Instant::now();
        let _ = sweep_mut(&mut bed, t0);
        bed.wait_named("zsh");
        (bed, t0)
    }

    fn program(&self) -> Option<String> {
        self.app
            .pool
            .get(self.session)
            .expect("pooled")
            .ctx
            .timeline
            .lock()
            .unwrap()
            .agent()
            .program
            .clone()
    }

    /// Wait (a hang detector) until the resolver has named `want`.
    fn wait_named(&self, want: &str) {
        let named = aterm_spec::wait::within(
            aterm_spec::wait::MUST_HAPPEN,
            Duration::from_millis(1),
            || (self.program().as_deref() == Some(want)).then_some(()),
        );
        assert!(named.is_some(), "never named {want}: {:?}", self.program());
    }

    /// When the sweep last asked the resolver for this session.
    fn requested_at(&self) -> Option<Instant> {
        self.app
            .session_status
            .agents
            .get(&self.session)
            .and_then(|w| w.resolved_at)
    }

    fn next_due(&self) -> Instant {
        self.app
            .session_status
            .sessions
            .get(&self.session)
            .expect("classified")
            .next_due
    }

    fn term(&self) -> Arc<std::sync::Mutex<aterm_core::terminal::Terminal>> {
        Arc::clone(&self.app.pool.get(self.session).expect("pooled").term)
    }
}

fn sweep_mut(bed: &mut Bed, now: Instant) -> Vec<u64> {
    bed.app.observe_session_statuses(now)
}

/// An edged session that is NOT due is visited on the next wake: the
/// claude in front is asked for and named then, not at its next observation.
#[test]
fn an_edged_session_is_named_on_the_next_wake_due_or_not() {
    let (mut bed, t0) = Bed::new();
    FRONT.with(|f| f.set(CLAUDE));
    let t1 = t0 + Duration::from_millis(1);
    assert!(t1 < bed.next_due(), "PRECONDITION: not due");
    bed.app.session_factory.edge_inbox.push(bed.session);
    let _ = sweep_mut(&mut bed, t1);
    assert_eq!(bed.requested_at(), Some(t1), "asked on this wake");
    bed.wait_named("claude");
    assert!(bed.asked.lock().unwrap().contains(&CLAUDE));
    assert!(
        bed.app.session_factory.edge_inbox.take().is_empty(),
        "visited once"
    );
}

/// NEGATIVE CONTROL: the same edge with no inbox push waits for the
/// session's observation; nothing is asked on this wake.
#[test]
fn control_without_the_inbox_nothing_is_named_before_the_next_observation() {
    let (mut bed, t0) = Bed::new();
    FRONT.with(|f| f.set(CLAUDE));
    let t1 = t0 + Duration::from_millis(1);
    let _ = sweep_mut(&mut bed, t1);
    assert_eq!(bed.requested_at(), Some(t0), "nothing asked on this wake");
    assert_eq!(bed.program().as_deref(), Some("zsh"));
}

/// A visit the PTY reader's hold refuses is pushed back for the next wake,
/// and charges no interval; the next wake names it. NEGATIVE CONTROL: the
/// same refusal of a due session that is not edged charges its interval.
#[test]
fn a_contended_edged_visit_is_retried_with_no_interval_charged() {
    let (mut bed, t0) = Bed::new();
    FRONT.with(|f| f.set(CLAUDE));
    let due = bed.next_due();
    let term = bed.term();
    let t1 = t0 + Duration::from_millis(1);
    {
        let _held = term.lock().unwrap();
        bed.app.session_factory.edge_inbox.push(bed.session);
        let _ = sweep_mut(&mut bed, t1);
        assert_eq!(bed.next_due(), due, "no interval charged");
        let queued = bed.app.session_factory.edge_inbox.take();
        assert_eq!(queued, vec![bed.session], "pushed back for the next wake");
        bed.app.session_factory.edge_inbox.push(bed.session);
    }
    let t2 = t1 + Duration::from_millis(1);
    let _ = sweep_mut(&mut bed, t2);
    assert_eq!(bed.requested_at(), Some(t2));
    bed.wait_named("claude");
    // The control: a due, un-edged session refused by the same hold.
    let due = bed.next_due();
    {
        let _held = term.lock().unwrap();
        let _ = sweep_mut(&mut bed, due);
    }
    assert!(
        bed.next_due() > due,
        "an un-edged refusal charges its interval"
    );
}
