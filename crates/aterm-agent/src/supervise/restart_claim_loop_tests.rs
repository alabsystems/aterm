// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A MOVE IS MADE ONCE, DATED BY THE CLAIM THAT MADE IT (ATERM-KI-1101-3): a model's move onto its
//! fallback answered a wait after its ask claimed the record (`wait:resume`, its relaunch landed)
//! is asked again after the loop's other acts, in this loop or the next, and that re-ask meets the
//! record its ask claimed: carried while in flight, made once carried on, never a second relaunch.
//! A move whose run holds no live claim ends its run at the next act, and the move is dated by the
//! second its claim was taken, never by an earlier ask that claimed nothing or a claim that lapsed
//! before the move was made.

use super::*;
use crate::supervise::journal::JournalRecord;
use crate::supervise::policy::turn_end::RULE_MODEL_FALLBACK;
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
/// the record read and holding no claim of the ask's ([`RestartClaim::Unclaimed`]), then
/// `stopping`, each an ask that claims the record and signals, its relaunch stopping unmade before
/// it answers (`wait:native-cancelled-before-mutation`), so the record holds no live claim
/// ([`RestartClaim::Unclaimed`]) and the next ask restarts afresh. While another
/// sweep holds the lock (`busy`), an ask is answered `busy:another-sweep` before any record is
/// read ([`RestartClaim::Unread`]), whoever's the record is.
#[derive(Debug, Default)]
struct RecordHost {
    /// The record: the ask that claimed it, and whether its relaunch was carried on (`Done`).
    record: Mutex<Option<(String, bool)>>,
    before_claim: Mutex<VecDeque<&'static str>>,
    stopping: Mutex<VecDeque<&'static str>>,
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

    /// The record's relaunch stopped unmade, unseen by the loop (the sweep found it stale): no
    /// live claim is left in it, and the next ask restarts afresh.
    fn lapse(&self) {
        *self.record.lock().unwrap() = None;
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
        } else if let Some(stopped) = self.stopping.lock().unwrap().pop_front() {
            *record = None;
            self.signals.fetch_add(1, Ordering::SeqCst);
            (stopped, RestartClaim::Unclaimed)
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

/// The fallback the moves below are onto.
fn fallback() -> Restart {
    Restart::Model {
        to: "claude-sonnet-5-5".to_string(),
    }
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

/// `RESTARTED` lines the loop said for the fallback's move.
fn moves_made(heard: &Heard) -> usize {
    heard
        .said
        .iter()
        .filter(|l| {
            l.starts_with("RESTARTED ") && l.contains(&format!("rule={RULE_MODEL_FALLBACK}"))
        })
        .count()
}

/// A FALLBACK'S MOVE ASKED AGAIN AFTER ITS CLAIM IS MADE ONCE, WHATEVER ACTS COME BETWEEN
/// (ATERM-KI-1101-3): the move's first ask claimed the record and landed its relaunch, and the host
/// answered a wait (`wait:resume`: the process it started not yet registered). While that relaunch
/// is owed its carry-on the policy asks no move at the turn ends that come (`upgrading`), so the
/// loop decides acts that are no restart, each of which ended the move's run, a line held in
/// custody or not; its next ask was another run's, which superseded the relaunch its first ask had
/// landed (`relaunch::supersedes`) or, once that was carried on, restarted it afresh: two
/// relaunches onto the fallback for one move. The run of a move whose ask claimed the record now
/// outlives those acts, so the re-ask is the claiming ask, carried in flight (`adopted`) or made
/// once carried on (`done:made`), the host signalling nothing, one `RESTARTED`.
#[test]
fn a_fallbacks_move_asked_again_after_its_claim_is_made_once_whatever_acts_come_between() {
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
            &fallback(),
            RULE_MODEL_FALLBACK,
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
            &fallback(),
            RULE_MODEL_FALLBACK,
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

/// A FALLBACK'S CLAIM STANDS PAST AN ANSWER THAT CANNOT READ THE RECORD: the move's first ask
/// claimed the record and answered a wait (`wait:resume`); asked again while the sweep that carries
/// its relaunch on holds the lock, the relaunch answers `busy:another-sweep` before it reads any
/// record (`relaunch::restart_as`), claiming nothing though the record is the move's own. Had that
/// answer taken the run's claim back, the acts after it would end the run, and the move's next ask,
/// another run's, would restart over the relaunch its first ask landed. The run stays claimed until
/// it ends, so that ask is the first ask's, made once carried on (`done:made`), one relaunch.
/// NEGATIVE CONTROL by mutation: with the run's claim set from each answer alone
/// (`Session::restart_agent`), the third ask is another run's.
#[test]
fn a_fallbacks_claim_stands_past_an_answer_that_cannot_read_the_record() {
    let host = RecordHost::answering(&[], &["wait:resume"]);
    let mut m = Mock::new(vec![idle_screen(); 16]);
    let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW);
    let mut heard = Heard::default();
    let ask = |s: &mut Session<'_, Mock>, heard: &mut Heard| {
        s.restart_agent(
            &point,
            &fallback(),
            RULE_MODEL_FALLBACK,
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

/// A FALLBACK'S MOVE IS DATED BY THE ASK THAT CLAIMED THE RECORD: its first ask claimed nothing
/// (`wait:held`), the session served a turn (an act), and a later ask claimed the record, answered
/// a wait, and was asked again past the acts after it, made then. The move is dated by that later
/// ask, the one that claimed (the switch's `made=` in the ledger, `approvals::model_switch_reason`),
/// never by the first, which a run kept past every act would date it by
/// (`a_restart_of_another_kind_begins_a_run_of_its_own`'s redraw then waits out the reset), and
/// made once: the claiming ask's run outlives the acts after its claim.
#[test]
fn a_fallbacks_move_is_dated_by_the_ask_that_claimed_the_record() {
    use crate::supervise::approvals::{Ledger, open_model_switch};
    let (dir, ledger) = ledger_at("move-dated-by-its-claim");
    let host = RecordHost::answering(&["wait:held"], &["wait:resume"]);
    let mut m = Mock::new(vec![idle_screen(); 16]);
    let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW);
    s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
    let mut heard = Heard::default();
    let ask = |s: &mut Session<'_, Mock>, heard: &mut Heard| {
        s.restart_agent(
            &point,
            &fallback(),
            RULE_MODEL_FALLBACK,
            TurnEndAction::Nothing,
            &opts,
            &[],
            heard,
        )
        .expect("the move");
    };
    ask(&mut s, &mut heard);
    s.set_clock(TEST_NOW + 300, PDT);
    s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
        .expect("the served turn's act");
    s.set_clock(TEST_NOW + 600, PDT);
    ask(&mut s, &mut heard);
    for n in 1..=2 {
        s.set_clock(TEST_NOW + 600 + 20 * n, PDT);
        s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
            .expect("an act that is no restart");
    }
    s.set_clock(TEST_NOW + 700, PDT);
    ask(&mut s, &mut heard);
    drop(s);
    let made = open_model_switch(&ledger, Some("@s-1")).map(|o| o.made_unix - TEST_NOW);
    let _ = std::fs::remove_dir_all(&dir);
    let asks = host.asks();
    assert_eq!(asks.len(), 3, "{asks:?}");
    assert_ne!(
        asks[0], asks[1],
        "the act ended the run that claimed nothing: {asks:?}"
    );
    assert_eq!(
        asks[1], asks[2],
        "the claiming ask's run outlived the acts: {asks:?}"
    );
    assert_eq!(host.signals(), 1, "one relaunch: {asks:?}");
    assert_eq!(
        made,
        Some(600),
        "dated by the claiming ask: {:#?}",
        heard.said
    );
}

/// A FALLBACK'S MOVE A LOOP LEFT CLAIMED IS ASKED BY THE NEXT LOOP AS ITS CLAIMING ASK: the
/// earlier loop's move answered a wait after its ask claimed the record (`wait:resume`) and the
/// loop ended before the move was booked (the harness loop restarts, or aterm does). The next
/// loop restored the move's run from the ledger or its journal but ended it at its first act (the
/// carry hold's nothing), so its ask was its own run's, which superseded the relaunch the earlier
/// ask had landed: two relaunches for one move. A later loop now restores the run its journal
/// shows claimed, and that run outlives its acts as it did in the earlier loop, so its ask meets
/// the record (`adopted`), nothing signalled again. CONTROL: an earlier ask deferred before any
/// claim (`wait:held`) leaves no run to restore, and the next loop's ask is a run of its own.
#[test]
fn a_fallbacks_move_a_loop_left_claimed_is_asked_by_the_next_as_its_claiming_ask() {
    let mut wrong = Vec::new();
    for (case, before_claim, after_claim, restored) in [
        ("claimed", &[][..], &["wait:resume"][..], true),
        (
            "deferred before its claim",
            &["wait:held"][..],
            &[][..],
            false,
        ),
    ] {
        let (dir, ledger) = ledger_at(&format!(
            "claimed-move-next-loop-{}",
            if restored { "claimed" } else { "unclaimed" }
        ));
        std::fs::write(&ledger, "").expect("the ledger");
        let host = RecordHost::answering(before_claim, after_claim);
        let mut heard = Heard::default();
        {
            let mut m = Mock::new(vec![idle_screen(); 8]);
            let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW);
            s.ledger = crate::supervise::approvals::Ledger::open(
                Some(&ledger),
                Some("@s-1"),
                &mut Vec::new(),
            );
            s.restart_agent(
                &point,
                &fallback(),
                RULE_MODEL_FALLBACK,
                TurnEndAction::Nothing,
                &opts,
                &[],
                &mut heard,
            )
            .expect("the move");
        }
        let journal: String = heard
            .noted
            .iter()
            .map(|line| {
                JournalRecord::of_line(TEST_NOW * 1000 + 400, Some("@s-1"), line, None).to_json()
                    + "\n"
            })
            .collect();
        std::fs::write(
            crate::supervise::approvals::journal_beside(&ledger),
            journal,
        )
        .expect("the journal");
        let mut heard = Heard::default();
        {
            let mut m = Mock::new(vec![idle_screen(); 8]);
            let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW + 30);
            let _ = s.seed_from_ledger(&ledger, (TEST_NOW + 30) * 1000, false);
            for n in 1..=2 {
                s.set_clock(TEST_NOW + 30 + 10 * n, PDT);
                s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
                    .expect("the carry hold's nothing");
            }
            s.set_clock(TEST_NOW + 60, PDT);
            s.restart_agent(
                &point,
                &fallback(),
                RULE_MODEL_FALLBACK,
                TurnEndAction::Nothing,
                &opts,
                &[],
                &mut heard,
            )
            .expect("the move asked again");
        }
        let _ = std::fs::remove_dir_all(&dir);
        let asks = host.asks();
        let same = asks.first() == asks.last();
        if same != restored || host.signals() != 1 || moves_made(&heard) != 1 {
            wrong.push(format!(
                "{case}: asks {asks:?}, {} signal(s), {} move(s) booked by the next loop",
                host.signals(),
                moves_made(&heard)
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The move's ask at the point `point` of the loop `s`, its host's answer heard by `heard`.
fn ask_move(s: &mut Session<'_, Mock>, point: &Turn, opts: &SuperviseOpts, heard: &mut Heard) {
    s.restart_agent(
        point,
        &fallback(),
        RULE_MODEL_FALLBACK,
        TurnEndAction::Nothing,
        opts,
        &[],
        heard,
    )
    .expect("the move");
}

/// A FALLBACK'S MOVE MADE AFTER ITS CLAIM LAPSED IS DATED BY THE CLAIM THAT MADE IT (found in the
/// review of the claim's fix, 2026-11-04): the move's first ask claimed the record and answered a
/// wait (`wait:resume`), and its relaunch then stopped unmade where the loop does not hear of it
/// (the sweep found it stale). The run held its claim past the act that came (the session served
/// a turn on the model it never left), so the move asked again later was that run's ask, which
/// met no live record, restarted afresh and made the move; booked at the run's second
/// (`fallback_made_at`, `approvals::model_switch_reason`), the wall drawn between the two read as
/// the fallback's own. The answer that restarted afresh says it took the claim
/// ([`RestartClaim::Taken`]), and the move is dated by that answer's second. NEGATIVE CONTROL by
/// mutation: the move dated by its run's second again (`Session::restart_agent`'s `made_unix`)
/// books it 600 s early.
#[test]
fn a_fallbacks_move_made_after_its_claim_lapsed_is_dated_by_the_claim_that_made_it() {
    use crate::supervise::approvals::{Ledger, open_model_switch};
    let (dir, ledger) = ledger_at("move-dated-past-a-lapsed-claim");
    let host = RecordHost::answering(&[], &["wait:resume", "relaunched"]);
    let mut m = Mock::new(vec![idle_screen(); 16]);
    let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW);
    s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
    let mut heard = Heard::default();
    ask_move(&mut s, &point, &opts, &mut heard);
    host.lapse();
    s.set_clock(TEST_NOW + 300, PDT);
    s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
        .expect("the served turn's act");
    s.set_clock(TEST_NOW + 600, PDT);
    ask_move(&mut s, &point, &opts, &mut heard);
    drop(s);
    let made = open_model_switch(&ledger, Some("@s-1")).map(|o| o.made_unix - TEST_NOW);
    let _ = std::fs::remove_dir_all(&dir);
    let asks = host.asks();
    assert_eq!(asks.len(), 2, "{asks:?}");
    assert_eq!(
        host.signals(),
        2,
        "the relaunch that lapsed, then the one that made the move: {asks:?}"
    );
    assert_eq!(moves_made(&heard), 1, "{:#?}", heard.said);
    assert_eq!(
        made,
        Some(600),
        "dated by the claim that made it: {:#?}",
        heard.noted
    );
}

/// A FALLBACK'S CLAIM THAT LAPSED IN ITS OWN ANSWER ENDS ITS RUN AT THE ACT (found in the review
/// of the claim's fix, 2026-11-04): the move's ask claimed the record and signalled, and its
/// relaunch stopped unmade before the answer (`wait:native-cancelled-before-mutation`), the record
/// left `Failed` under that ask. The claim was said from the record's ask and cause alone
/// (`relaunch::its_own`), so the run held it past every act after, and the move asked much later
/// was that run's ask. The answer now says the record holds no live claim
/// ([`RestartClaim::Unclaimed`]), so the act after it ends the run and the later ask is a run of
/// its own, which claims afresh and dates the move. NEGATIVE CONTROL by mutation: the loop
/// reading that answer as holding the claim (as the record's ask and cause alone would say) asks
/// the later move under the first run's ask.
#[test]
fn a_fallbacks_claim_that_lapsed_in_its_own_answer_ends_its_run_at_the_act() {
    use crate::supervise::approvals::{Ledger, open_model_switch};
    let (dir, ledger) = ledger_at("claim-lapsed-in-its-answer");
    let host = RecordHost::answering(&[], &["relaunched"]);
    host.stopping
        .lock()
        .unwrap()
        .push_back("wait:native-cancelled-before-mutation");
    let mut m = Mock::new(vec![idle_screen(); 16]);
    let (mut s, opts, point) = at_rest(&mut m, &host, TEST_NOW);
    s.ledger = Ledger::open(Some(&ledger), Some("@s-1"), &mut Vec::new());
    let mut heard = Heard::default();
    ask_move(&mut s, &point, &opts, &mut heard);
    s.set_clock(TEST_NOW + 300, PDT);
    s.turn_end_execute(&point, TurnEndAction::Nothing, &opts, &[], &mut heard)
        .expect("the served turn's act");
    s.set_clock(TEST_NOW + 600, PDT);
    ask_move(&mut s, &point, &opts, &mut heard);
    drop(s);
    let made = open_model_switch(&ledger, Some("@s-1")).map(|o| o.made_unix - TEST_NOW);
    let _ = std::fs::remove_dir_all(&dir);
    let asks = host.asks();
    assert_eq!(asks.len(), 2, "{asks:?}");
    assert_ne!(
        asks[0], asks[1],
        "the act ended the run whose claim lapsed: {asks:?}"
    );
    assert_eq!(host.signals(), 2, "{asks:?}");
    assert_eq!(moves_made(&heard), 1, "{:#?}", heard.said);
    assert_eq!(made, Some(600), "{:#?}", heard.noted);
}
