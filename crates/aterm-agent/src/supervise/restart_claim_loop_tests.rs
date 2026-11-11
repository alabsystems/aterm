// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A MOVE IS MADE ONCE (ATERM-KI-1101-3): a move onto another credential of the session's own
//! account answered a wait after its ask claimed the record (`wait:resume`, its relaunch landed)
//! is asked again after the loop's other acts, and that re-ask meets the record its ask claimed:
//! carried while in flight, made once carried on, never a second relaunch, even past an answer
//! that could not read the record.

use super::*;
use crate::supervise::policy::turn_end::{RULE_TOKEN_ROUTE, RouteTo};
use crate::supervise::run::turn_end_loop::Custody;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A HOST THAT KEEPS THE CONVERSATION'S RECORD AS THE WINDOW HOST'S RELAUNCH DOES
/// (`relaunch::restart_as`), for one move: an ask meets the record an ask of its run claimed
/// (`relaunch::its_own`, by the ask) and carries it, signalling nothing, `adopted` while its
/// relaunch is in flight and `done:made` once the sweep carried it on (`relaunch::already_made`),
/// the claim held ([`RestartClaim::Held`]); any other ask claims the record, superseding the
/// relaunch landed in the tab (`relaunch::supersedes`) or restarting over a record whose relaunch
/// lapsed, signals the tab's agent and answers the next of `after_claim` (`relaunched` once they
/// run out), the claim taken ([`RestartClaim::Taken`]). The asks before that answer
/// `before_claim` first, each deferred before any claim (`wait:held`: a person's hand on the tab),
/// the record read and holding no claim of the ask's ([`RestartClaim::Unclaimed`]). While another
/// sweep holds the lock (`busy`), an ask is answered `busy:another-sweep` before any record is
/// read ([`RestartClaim::Unread`]), whoever's the record is.
#[derive(Debug, Default)]
struct RecordHost {
    /// The record: the ask that claimed it, and whether its relaunch was carried on (`Done`).
    record: Mutex<Option<(String, bool)>>,
    before_claim: Mutex<VecDeque<&'static str>>,
    after_claim: Mutex<VecDeque<&'static str>>,
    /// How many asks to come another sweep holds the lock for.
    busy: AtomicUsize,
    /// How many times the host signalled the tab's agent: one relaunch each.
    signals: AtomicUsize,
    /// Each ask, in order.
    asks: Mutex<Vec<String>>,
}

impl RecordHost {
    fn answering(before_claim: &[&'static str], after_claim: &[&'static str]) -> Arc<Self> {
        Arc::new(Self {
            before_claim: Mutex::new(before_claim.iter().copied().collect()),
            after_claim: Mutex::new(after_claim.iter().copied().collect()),
            ..Self::default()
        })
    }

    /// The sweep carried the record's relaunch on (`Done`).
    fn carry_on(&self) {
        if let Some((_, done)) = self.record.lock().unwrap().as_mut() {
            *done = true;
        }
    }

    fn signals(&self) -> usize {
        self.signals.load(Ordering::SeqCst)
    }

    fn asks(&self) -> Vec<String> {
        self.asks.lock().unwrap().clone()
    }
}

impl IdleHost for RecordHost {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn can_restart(&self) -> bool {
        true
    }
    fn restart(&self, _: &Restart, ask: &RestartAsk) -> Option<RestartAnswer> {
        self.asks.lock().unwrap().push(ask.word().to_string());
        if self
            .busy
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Some(RestartAnswer::unread("busy:another-sweep"));
        }
        let mut record = self.record.lock().unwrap();
        let (step, claim) = if let Some((own, done)) = record.as_ref()
            && own == ask.word()
        {
            let step = if *done { "done:made" } else { "adopted" };
            (step, RestartClaim::Held)
        } else if let Some(deferred) = self.before_claim.lock().unwrap().pop_front() {
            (deferred, RestartClaim::Unclaimed)
        } else {
            *record = Some((ask.word().to_string(), false));
            self.signals.fetch_add(1, Ordering::SeqCst);
            let word = self.after_claim.lock().unwrap().pop_front();
            (word.unwrap_or("relaunched"), RestartClaim::Taken)
        };
        Some(RestartAnswer {
            step: step.to_string(),
            claim,
        })
    }
}

/// The move below: onto the session's own account's token.
const fn route() -> Restart {
    Restart::Route { to: RouteTo::Token }
}

/// A loop over an idle Claude Code whose host is `host`, at `at` (Unix seconds), its decision made.
fn at_rest<'a>(
    m: &'a mut Mock,
    host: &Arc<RecordHost>,
    at: i64,
) -> (Session<'a, Mock>, SuperviseOpts, Turn) {
    m.cursor_on_caret = true;
    let opts = SuperviseOpts {
        idle_host: Some(Arc::clone(host) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut s = session(m, Some("@s-1".to_string()));
    s.set_clock(at, PDT);
    let screen = s.screen().expect("the point");
    let point = s.turn_of(screen);
    let _ = s
        .turn_end_at_point(&point, Some(Duration::from_secs(6)), &opts)
        .expect("the decision");
    (s, opts, point)
}

/// `RESTARTED` lines the loop said for the move.
fn moves_made(heard: &Heard) -> usize {
    heard
        .said
        .iter()
        .filter(|l| l.starts_with("RESTARTED ") && l.contains(&format!("rule={RULE_TOKEN_ROUTE}")))
        .count()
}

/// A MOVE ASKED AGAIN AFTER ITS CLAIM IS MADE ONCE, WHATEVER ACTS COME BETWEEN
/// (ATERM-KI-1101-3): the move's first ask claimed the record and landed its relaunch, and the host
/// answered a wait (`wait:resume`: the process it started not yet registered). While that relaunch
/// is owed its carry-on the policy asks no move at the turn ends that come (`upgrading`), so the
/// loop decides acts that are no restart, each of which ended the move's run, a line held in
/// custody or not; its next ask was another run's, which superseded the relaunch its first ask had
/// landed (`relaunch::supersedes`) or, once that was carried on, restarted it afresh: two
/// relaunches for one move. The run of a move whose ask claimed the record now
/// outlives those acts, so the re-ask is the claiming ask, carried in flight (`adopted`) or made
/// once carried on (`done:made`), the host signalling nothing, one `RESTARTED`.
#[test]
fn a_move_asked_again_after_its_claim_is_made_once_whatever_acts_come_between() {
    let mut wrong = Vec::new();
    for (held, carried_on) in [(false, false), (false, true), (true, false), (true, true)] {
        let host = RecordHost::answering(&[], &["wait:resume"]);
        let mut m = Mock::new(vec![idle_screen(); 16]);
        let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW);
        if held {
            s.custody = Some(Custody::of("keep going", s.fx.mono()));
        }
        let mut heard = Heard::default();
        s.restart_agent(
            &point,
            &route(),
            RULE_TOKEN_ROUTE,
            TurnEndAction::Nothing,
            &opts,
            &[],
            &mut heard,
        )
        .expect("the move");
        for n in 1..=3 {
            s.set_clock(TEST_NOW + 20 * n, PDT);
            s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
                .expect("an act that is no restart");
        }
        if carried_on {
            host.carry_on();
        }
        s.set_clock(TEST_NOW + 90, PDT);
        s.restart_agent(
            &point,
            &route(),
            RULE_TOKEN_ROUTE,
            TurnEndAction::Nothing,
            &opts,
            &[],
            &mut heard,
        )
        .expect("the move asked again");
        let asks = host.asks();
        if host.signals() != 1 || asks.len() != 2 || asks[0] != asks[1] || moves_made(&heard) != 1 {
            wrong.push(format!(
                "held={held} carried_on={carried_on}: {} signal(s), asks {asks:?}, {} move(s) booked",
                host.signals(),
                moves_made(&heard)
            ));
        }
    }
    assert!(wrong.is_empty(), "one relaunch per move: {wrong:#?}");
}

/// A MOVE'S CLAIM STANDS PAST AN ANSWER THAT CANNOT READ THE RECORD: the move's first ask
/// claimed the record and answered a wait (`wait:resume`); asked again while the sweep that carries
/// its relaunch on holds the lock, the relaunch answers `busy:another-sweep` before it reads any
/// record (`relaunch::restart_as`), claiming nothing though the record is the move's own. Had that
/// answer taken the run's claim back, the acts after it would end the run, and the move's next ask,
/// another run's, would restart over the relaunch its first ask landed. The run stays claimed until
/// it ends, so that ask is the first ask's, made once carried on (`done:made`), one relaunch.
/// NEGATIVE CONTROL by mutation: with the run's claim set from each answer alone
/// (`Session::restart_agent`), the third ask is another run's.
#[test]
fn a_moves_claim_stands_past_an_answer_that_cannot_read_the_record() {
    let host = RecordHost::answering(&[], &["wait:resume"]);
    let mut m = Mock::new(vec![idle_screen(); 16]);
    let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW);
    let mut heard = Heard::default();
    let ask = |s: &mut Session<'_, Mock>, heard: &mut Heard| {
        s.restart_agent(
            &point,
            &route(),
            RULE_TOKEN_ROUTE,
            TurnEndAction::Nothing,
            &opts,
            &[],
            heard,
        )
        .expect("the move");
    };
    ask(&mut s, &mut heard);
    s.set_clock(TEST_NOW + 20, PDT);
    s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
        .expect("an act that is no restart");
    host.busy.store(1, Ordering::SeqCst);
    s.set_clock(TEST_NOW + 40, PDT);
    ask(&mut s, &mut heard);
    for n in 1..=2 {
        s.set_clock(TEST_NOW + 40 + 20 * n, PDT);
        s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
            .expect("an act that is no restart");
    }
    host.carry_on();
    s.set_clock(TEST_NOW + 120, PDT);
    ask(&mut s, &mut heard);
    let asks = host.asks();
    assert_eq!(asks.len(), 3, "{asks:?}");
    assert!(
        asks.iter().all(|a| *a == asks[0]),
        "one run throughout: {asks:?}"
    );
    assert_eq!(host.signals(), 1, "one relaunch: {asks:?}");
    assert_eq!(moves_made(&heard), 1, "{:#?}", heard.said);
}
