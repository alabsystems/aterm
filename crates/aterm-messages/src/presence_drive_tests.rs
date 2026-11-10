// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence driver without a window system: a [`TestDesk`] of plain
//! windows and tabs drives refresh, a told story, the story read, the deadline
//! and the tick, and logs every effect.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use crate::presence::drive::{self, Desk, Table};
use crate::presence::{
    ChipLevel, Facts, HoldFact, Level, NeedFact, NeedSeverity, StopCause, TurnFact, View,
};
use crate::presence_tests::{TestHost, need};
use crate::{Duration, Instant};

type TFacts = Facts<TestHost>;

/// One plain window: its view and its tabs (each a list of pane sessions).
struct Win {
    view: View,
    tabs: Vec<Vec<u64>>,
}

impl Win {
    fn new(tabs: Vec<Vec<u64>>) -> Self {
        Self {
            view: View::default(),
            tabs,
        }
    }
}

/// A host of plain data: the facts it senses (the published need among
/// them), its supervisors' lapses, and a log of every effect.
struct TestDesk {
    now: Instant,
    table: Table<TestHost>,
    wins: BTreeMap<u32, Win>,
    facts: RefCell<HashMap<u64, TFacts>>,
    supervisors: HashMap<u64, Instant>,
    recheck_at: Option<Instant>,
    log: RefCell<Vec<String>>,
}

impl TestDesk {
    fn new(now: Instant) -> Self {
        Self {
            now,
            table: Table::default(),
            wins: BTreeMap::new(),
            facts: RefCell::new(HashMap::new()),
            supervisors: HashMap::new(),
            recheck_at: None,
            log: RefCell::new(Vec::new()),
        }
    }

    fn note(&self, s: String) {
        self.log.borrow_mut().push(s);
    }

    fn took(&self, prefix: &str) -> usize {
        self.log
            .borrow()
            .iter()
            .filter(|l| l.starts_with(prefix))
            .count()
    }

    fn view(&self, w: u32) -> &View {
        &self.wins[&w].view
    }

    fn set(&mut self, session: u64, facts: TFacts) {
        self.facts.borrow_mut().insert(session, facts);
    }

    /// Publish `need` on `session` and refresh it.
    fn publish(&mut self, session: u64, need: Option<NeedFact>) {
        let mut facts = self
            .facts
            .borrow()
            .get(&session)
            .cloned()
            .unwrap_or_default();
        facts.need = need;
        self.set(session, facts);
        drive::refresh_session(self, session);
    }
}

impl Desk for TestDesk {
    type V = TestHost;
    type Win = u32;

    fn clock(&self) -> Instant {
        self.now
    }

    fn table(&self) -> &Table<TestHost> {
        &self.table
    }

    fn table_mut(&mut self) -> &mut Table<TestHost> {
        &mut self.table
    }

    fn each_view(&self, f: &mut dyn FnMut(u32, &View)) {
        for (w, win) in &self.wins {
            f(*w, &win.view);
        }
    }

    fn view_of_mut(&mut self, w: u32) -> Option<&mut View> {
        self.wins.get_mut(&w).map(|x| &mut x.view)
    }

    fn windows_showing(&self, session: u64) -> Vec<u32> {
        self.wins
            .iter()
            .filter(|(_, x)| x.tabs.iter().any(|t| t.contains(&session)))
            .map(|(w, _)| *w)
            .collect()
    }

    fn holds_session(&self, session: u64) -> bool {
        self.facts.borrow().contains_key(&session)
    }

    fn sense(&self, session: u64) -> Option<TFacts> {
        self.note(format!("sense {session}"));
        self.facts.borrow().get(&session).cloned()
    }

    fn supervisor_lapse(&self, session: u64, _now: Instant) -> Option<Instant> {
        self.supervisors.get(&session).copied()
    }

    fn next_need_recheck(&self, _now: Instant) -> Option<Instant> {
        self.recheck_at
    }

    fn projecting(&mut self, w: u32) {
        self.note(format!("projecting {w}"));
    }

    fn restamp_chips(&mut self, windows: Vec<u32>) {
        self.note(format!("restamp {windows:?}"));
    }

    fn publish_and_age_need(&mut self, session: u64) {
        self.note(format!("publish_and_age_need {session}"));
    }

    fn run_need_rechecks(&mut self, _now: Instant) {
        self.note("run_need_rechecks".to_string());
    }

    fn lapse_supervisor(&mut self, session: u64) {
        self.supervisors.remove(&session);
        self.note(format!("lapse {session}"));
    }

    fn forget(&mut self, session: u64) {
        self.table.retire(session);
        self.note(format!("forget {session}"));
    }
}

fn stuck(key: u64) -> Option<NeedFact> {
    Some(need(NeedSeverity::Stuck, StopCause::Wall, key))
}

fn decide(key: u64) -> Option<NeedFact> {
    Some(need(NeedSeverity::Decide, StopCause::Wall, key))
}

/// One window of `tabs`, each session sensed with default facts and
/// refreshed once.
fn desk(tabs: Vec<Vec<u64>>) -> TestDesk {
    let mut d = TestDesk::new(Instant::now());
    let sessions: Vec<u64> = tabs.iter().flatten().copied().collect();
    d.wins.insert(1, Win::new(tabs));
    for s in sessions {
        d.set(s, TFacts::default());
        drive::refresh_session(&mut d, s);
    }
    d.log.borrow_mut().clear();
    d
}

/// A session at a prompt with no published need marks no tab until its verdict publishes a need.
#[test]
fn a_supervised_prompt_is_no_need_until_its_verdict_publishes_one() {
    let mut d = desk(vec![vec![1]]);
    d.set(
        1,
        TFacts {
            agent_seq: 1,
            agent: Some(crate::presence::AgentReading {
                phase: crate::presence::AgentPhase::Prompt { detail: None },
                context_pct: None,
                composer: crate::presence::ComposerState::Unknown,
            }),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 1);
    assert_eq!(d.table().slot(1).map(|s| s.chip()), Some(ChipLevel::Off));
    assert_eq!(drive::session_level(&d, 1), Level::Attention, "a record");
    d.publish(1, decide(1));
    assert_eq!(
        d.table().slot(1).map(|s| s.chip()),
        Some(ChipLevel::Wait),
        "control: the verdict marks it"
    );
}

/// EVERY REFRESH REACHES THE HOST AT EVERY WINDOW IT TOUCHES ([`Desk::projecting`]), where the App
/// publishes the front hold and records the look. A session's reaches once each window
/// [`Desk::windows_showing`] lists and no other; `refresh_window` reaches its one window and
/// `refresh_all` every window. CONTROL: a refresh that moved nothing reaches none.
#[test]
fn each_refresh_reaches_every_window_it_touches() {
    let mut d = desk(vec![vec![1, 1], vec![2]]);
    d.wins.insert(2, Win::new(vec![vec![1]]));
    d.wins.insert(3, Win::new(vec![vec![3]]));
    let reached = |d: &TestDesk| {
        let counts = [1, 2, 3].map(|w| d.took(&format!("projecting {w}")));
        d.log.borrow_mut().clear();
        counts
    };
    d.publish(1, decide(1));
    assert_eq!(reached(&d), [1, 1, 0], "the windows showing session 1");
    d.set(
        2,
        TFacts {
            shell: Some(("running", d.now)),
            ..TFacts::default()
        },
    );
    d.publish(2, decide(2));
    assert_eq!(reached(&d), [1, 0, 0], "the one window showing session 2");
    drive::refresh_session(&mut d, 2);
    assert_eq!(reached(&d), [0, 0, 0], "control: nothing moved");
    drive::refresh_window(&mut d, 2);
    assert_eq!(reached(&d), [0, 1, 0], "the one window named");
    drive::refresh_all(&mut d);
    assert_eq!(reached(&d), [1, 1, 1], "every window");
}

/// Verdict flips on a background tab restamp its strip without side effects.
#[test]
fn a_hundred_verdict_flips_ask_only_for_a_restamp() {
    let mut d = desk(vec![vec![1], vec![2]]);
    for i in 0..100u64 {
        d.publish(2, if i % 2 == 0 { decide(i) } else { None });
    }
    assert_eq!(d.took("restamp"), 100);
    let others = d
        .log
        .borrow()
        .iter()
        .filter(|l| {
            !["restamp", "sense", "publish_and_age_need", "projecting"]
                .iter()
                .any(|p| l.starts_with(p))
        })
        .cloned()
        .collect::<Vec<_>>();
    assert!(others.is_empty(), "{others:?}");
}

/// A refresh that moved nothing projects nothing and asks for nothing (a
/// session with a shell word: one with no fact at all is projected at every
/// sight, as a fresh one is).
#[test]
fn a_refresh_that_moved_nothing_restamps_nothing() {
    let mut d = desk(vec![vec![1]]);
    d.set(
        1,
        TFacts {
            shell: Some(("running", d.now)),
            ..TFacts::default()
        },
    );
    d.publish(1, stuck(1));
    d.log.borrow_mut().clear();
    drive::refresh_session(&mut d, 1);
    assert_eq!(d.took("restamp"), 0);
    assert_eq!(
        d.took("publish_and_age_need"),
        1,
        "the publication rides every refresh"
    );
}

/// A quiet desk and a published need arm nothing; supervisor lapses and need recheck deadlines
/// fold into the deadline, and a lapse fires on tick.
#[test]
fn a_need_arms_no_deadline_and_the_lapses_fold_in() {
    let mut d = desk(vec![vec![1], vec![2]]);
    let t0 = d.now;
    assert_eq!(drive::deadline(&d, t0), None, "a quiet desk");
    drive::tick(&mut d, t0);
    d.publish(2, stuck(1));
    assert_eq!(drive::deadline(&d, t0), None, "a need arms nothing");
    d.supervisors.insert(1, t0 + Duration::from_secs(9));
    drive::refresh_session(&mut d, 1);
    assert_eq!(drive::deadline(&d, t0), Some(t0 + Duration::from_secs(9)));
    d.recheck_at = Some(t0 + Duration::from_secs(3));
    assert_eq!(drive::deadline(&d, t0), Some(t0 + Duration::from_secs(3)));
    d.recheck_at = None;
    let lapse = t0 + Duration::from_secs(9);
    d.now = lapse;
    drive::tick(&mut d, lapse);
    assert_eq!(d.took("lapse 1"), 1);
}

/// A renewed supervisor claim moves the deadline live without a refresh, falling back to the table's copy when gone.
#[test]
fn a_renewed_supervisor_claim_moves_the_deadline_without_a_refresh() {
    let mut d = desk(vec![vec![1], vec![2]]);
    let t0 = d.now;
    d.supervisors.insert(2, t0 + Duration::from_secs(90));
    drive::refresh_session(&mut d, 2);
    assert_eq!(drive::deadline(&d, t0), Some(t0 + Duration::from_secs(90)));
    d.supervisors.insert(2, t0 + Duration::from_secs(110));
    let t = t0 + Duration::from_secs(20);
    assert_eq!(
        drive::deadline(&d, t),
        Some(t0 + Duration::from_secs(110)),
        "the live lapse, not the 90 s one the last refresh read"
    );
    let mut stale = desk(vec![vec![1], vec![2]]);
    stale.supervisors.insert(2, t0 + Duration::from_secs(90));
    drive::refresh_session(&mut stale, 2);
    stale.supervisors.remove(&2);
    assert_eq!(
        drive::deadline(&stale, t),
        Some(t0 + Duration::from_secs(90)),
        "a claim gone without a refresh: the table's copy fires, and the tick re-reads"
    );
    let lapse = t0 + Duration::from_secs(110);
    d.now = lapse;
    assert_eq!(drive::deadline(&d, lapse), Some(lapse));
    drive::tick(&mut d, lapse);
    assert_eq!(d.took("lapse 2"), 1);
    assert_eq!(drive::deadline(&d, lapse), None, "lapsed: nothing armed");
}

/// A person's key in a calm session advances the story watermark and quiets `Level::Story`.
#[test]
fn a_calm_key_reads_the_story_and_a_hold_keeps_it() {
    let mut d = desk(vec![vec![1]]);
    d.set(
        1,
        TFacts {
            turn: Some(TurnFact {
                id: 1,
                settled: true,
                dur_ms: 5,
                carried: false,
                aterm: false,
                opened: None,
            }),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 1);
    assert_eq!(drive::session_level(&d, 1), Level::Story);
    let held = TFacts {
        hold: Some(HoldFact {
            reason: "pause".into(),
            fleet: false,
        }),
        ..d.facts.borrow()[&1].clone()
    };
    d.set(1, held);
    drive::refresh_session(&mut d, 1);
    drive::human_acted_on(&mut d, 1, 1);
    assert_eq!(d.view(1).watermark(1), 0, "not calm: not read");
    let mut free = d.facts.borrow()[&1].clone();
    free.hold = None;
    d.set(1, free);
    drive::refresh_session(&mut d, 1);
    drive::human_acted_on(&mut d, 1, 1);
    assert_eq!(drive::session_level(&d, 1), Level::Quiet, "read");
    let seq = drive::tell(&mut d, 1).expect("held");
    assert!(drive::status_tail(&d, 1).contains(&format!("story={seq}")));
    assert_eq!(drive::tell(&mut d, 99), None);
}

/// `status_tail` prints every presence field of a session with a need, no hand and no story.
#[test]
fn the_status_tail_prints_each_field_of_a_session_with_a_need() {
    let mut d = desk(vec![vec![1], vec![2]]);
    d.publish(2, stuck(1));
    assert_eq!(
        drive::status_tail(&d, 2),
        "hand=- hand_pid=- level=quiet story=0 why=-"
    );
}
