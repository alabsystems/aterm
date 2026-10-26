// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence driver without a window system: a [`TestDesk`] of plain
//! windows and tabs drives refresh, the window's most pressing session, the
//! border and the pill, the person's outline, a told story, the story read,
//! the deadline and the tick, and logs every effect.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use crate::presence::drive::{self, Desk, Shown, Table};
use crate::presence::{
    ChipLevel, Facts, HoldFact, Level, NeedFact, NeedSeverity, PillAct, PillKind, Rim, StopCause,
    TurnFact, View,
};
use crate::presence_tests::{TestHost, need};
use crate::{Duration, Instant};

type TFacts = Facts<TestHost>;

/// One plain window: its view, its tabs (each a list of pane sessions, the
/// first the tab's focused pane), the front tab, whether it is seen, and
/// whether its titlebar draws the pill (glass) rather than its own frame.
struct Win {
    view: View,
    tabs: Vec<Vec<u64>>,
    front_tab: usize,
    visible: bool,
    glass: bool,
}

impl Win {
    fn new(tabs: Vec<Vec<u64>>) -> Self {
        Self {
            view: View::default(),
            tabs,
            front_tab: 0,
            visible: true,
            glass: false,
        }
    }
}

/// A host of plain data: the facts it senses (the published need among
/// them), its supervisors' lapses, its switch, and a log of every effect.
struct TestDesk {
    now: Instant,
    table: Table<TestHost>,
    wins: BTreeMap<u32, Win>,
    facts: RefCell<HashMap<u64, TFacts>>,
    supervisors: HashMap<u64, Instant>,
    shown_on: bool,
    restartable: Vec<u64>,
    herald_at: Option<Instant>,
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
            shown_on: true,
            restartable: Vec::new(),
            herald_at: None,
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

    fn view_of(&self, w: u32) -> Option<&View> {
        self.wins.get(&w).map(|x| &x.view)
    }

    fn view_of_mut(&mut self, w: u32) -> Option<&mut View> {
        self.wins.get_mut(&w).map(|x| &mut x.view)
    }

    fn front_session(&self, w: u32) -> Option<u64> {
        let win = self.wins.get(&w)?;
        win.tabs.get(win.front_tab)?.first().copied()
    }

    fn shown(&self, w: u32) -> Vec<Shown> {
        let Some(win) = self.wins.get(&w) else {
            return Vec::new();
        };
        win.tabs
            .iter()
            .enumerate()
            .flat_map(|(tab, panes)| {
                panes.iter().map(move |session| Shown {
                    session: *session,
                    tab,
                    front_tab: tab == win.front_tab,
                })
            })
            .collect()
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

    fn pill_visible(&self, w: u32) -> bool {
        self.wins.get(&w).is_some_and(|x| x.visible)
    }

    fn needs_shown(&self) -> bool {
        self.shown_on
    }

    fn label_of(&self, session: u64) -> String {
        format!("tab-of-{session}")
    }

    fn restart_offered(&self, session: u64) -> bool {
        self.restartable.contains(&session)
    }

    fn sid_of(&self, session: u64) -> Option<String> {
        Some(format!("s-{session}"))
    }

    fn herald_deadline(&self, _now: Instant) -> Option<Instant> {
        self.herald_at
    }

    fn projecting(&mut self, w: u32) {
        self.note(format!("projecting {w}"));
    }

    fn request_redraw(&self, w: u32) {
        self.note(format!("redraw {w}"));
    }

    fn restamp_chips(&mut self, windows: Vec<u32>) {
        self.note(format!("restamp {windows:?}"));
    }

    fn push_pill(&mut self, windows: Vec<u32>) -> Vec<u32> {
        self.note(format!("pill {windows:?}"));
        windows
            .into_iter()
            .filter(|w| self.wins.get(w).is_some_and(|x| !x.glass))
            .collect()
    }

    fn herald(&mut self, session: u64) {
        self.note(format!("herald {session}"));
    }

    fn herald_owed(&mut self, _now: Instant) {
        self.note("herald-owed".to_string());
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

/// A session at a prompt with no published need lights no border and no pill until its verdict publishes a need.
#[test]
fn a_supervised_prompt_gives_no_border_and_no_pill() {
    let mut d = desk(vec![vec![1]]);
    d.set(
        1,
        TFacts {
            agent_seq: 1,
            agent: Some(crate::presence::AgentReading {
                phase: crate::presence::AgentPhase::Prompt { detail: None },
                context_pct: None,
            }),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 1);
    assert_eq!(d.view(1).rim, Rim::None);
    assert!(d.view(1).pill.is_none());
    assert_eq!(d.view(1).fp(), 0);
    assert_eq!(d.table().slot(1).map(|s| s.chip()), Some(ChipLevel::Off));
    assert_eq!(drive::session_level(&d, 1), Level::Attention, "a record");
    d.publish(1, decide(1));
    assert_eq!(d.view(1).rim, Rim::Decide, "control: the verdict lights it");
    assert!(d.view(1).pill.is_some());
}

#[test]
fn a_need_without_a_pill_does_not_hide_another_tabs_prompt() {
    let mut d = desk(vec![vec![1], vec![2]]);
    let mut reported = decide(1).expect("a need");
    reported.show_pill = false;
    d.publish(1, Some(reported));
    assert_eq!(d.view(1).rim, Rim::Decide);
    assert!(d.view(1).pill.is_none());
    d.publish(2, decide(2));
    assert_eq!(
        d.view(1).pressing,
        Some(2),
        "the request that stopped its agent, ahead of the front report it carries on past (§14.7)"
    );
    assert_eq!(d.view(1).rim, Rim::Decide);
    assert_eq!(d.view(1).pill.as_ref().map(|pill| pill.session), Some(2));
    assert_eq!(d.view(1).pill.as_ref().map(|pill| pill.more), Some(1));
    assert_eq!(d.next_need_after(1, 2), Some(1));
    d.wins.get_mut(&1).unwrap().front_tab = 1;
    assert_eq!(d.next_need_after(1, 2), Some(1));
    d.publish(2, None);
    assert!(d.view(1).pill.is_none());
    assert_eq!(d.view(1).pressing, Some(1), "the report alone again");
}

/// STOPPED WORK FIRST (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7): within a
/// severity, the window's pressing session (the rim's target, `chrome`'s `pressing=`) and the
/// next need its pill's `+N` opens start from a need that has stopped its agent, not from an
/// older report its agent carries on past, even one in the front tab; stuck still outranks
/// decide, and among the stopped needs the front session and then the oldest lead.
#[test]
fn pressing_and_next_need_lead_with_a_stopped_need() {
    let mut d = desk(vec![vec![1], vec![2], vec![3]]);
    let mut report = decide(1).expect("a need");
    report.show_pill = false;
    d.publish(1, Some(report.clone()));
    d.now += Duration::from_secs(1);
    d.publish(2, decide(2));
    d.now += Duration::from_secs(1);
    d.publish(3, decide(3));
    let p = d.pressing_session(1).expect("a need");
    assert_eq!(
        (p.session, p.tab, p.front_tab, p.more),
        (2, 1, false, 2),
        "the older of the two requests, past the oldest need, the front tab's report"
    );
    assert_eq!(d.view(1).pressing, Some(2));
    assert_eq!(d.view(1).pill.as_ref().map(|pill| pill.session), Some(2));
    let line = drive::chrome_line(&d, Some(1));
    assert!(line.contains(" pressing=@s-2 "), "{line}");
    assert_eq!(
        d.next_need_after(1, 2),
        Some(3),
        "from the front report: past the pill's request to the next one"
    );
    d.wins.get_mut(&1).unwrap().front_tab = 1;
    drive::refresh_window(&mut d, 1);
    assert_eq!(d.next_need_after(1, 2), Some(3), "from the first request");
    d.wins.get_mut(&1).unwrap().front_tab = 2;
    drive::refresh_window(&mut d, 1);
    assert_eq!(
        d.pressing_session(1).map(|p| p.session),
        Some(3),
        "a stopped need in front wins its tie"
    );
    assert_eq!(
        d.next_need_after(1, 3),
        Some(1),
        "the report last, then it wraps"
    );
    d.wins.get_mut(&1).unwrap().front_tab = 0;
    drive::refresh_window(&mut d, 1);
    let mut stuck_report = stuck(1).expect("a need");
    stuck_report.show_pill = false;
    d.publish(1, Some(stuck_report));
    assert_eq!(
        d.pressing_session(1).map(|p| p.session),
        Some(1),
        "stuck still outranks decide, report or not"
    );
    assert_eq!(d.view(1).rim, Rim::Stuck);
    d.publish(1, Some(report));
    d.publish(2, None);
    d.publish(3, None);
    assert_eq!(
        d.pressing_session(1).map(|p| p.session),
        Some(1),
        "the report alone presses"
    );
}

#[test]
fn more_walks_past_a_front_report_to_the_other_reports() {
    let mut d = desk(vec![vec![1, 1], vec![2], vec![3, 3]]);
    d.publish(1, stuck(1));
    for session in [2, 3] {
        let mut reported = decide(session).expect("a need");
        reported.show_pill = false;
        d.publish(session, Some(reported));
    }
    let pill = d.view(1).pill.as_ref().expect("the visible request");
    assert_eq!((pill.session, pill.more), (1, 2));

    d.wins.get_mut(&1).unwrap().front_tab = 1;
    assert_eq!(d.next_need_after(1, 1), Some(3));
    d.wins.get_mut(&1).unwrap().front_tab = 2;
    assert_eq!(d.next_need_after(1, 1), Some(2));
    d.wins.get_mut(&1).unwrap().front_tab = 0;
    assert_eq!(d.next_need_after(1, 1), Some(2));
}

/// A report with no pill, as a harness's `NEEDS YOU:` is published.
fn report(key: u64) -> Option<NeedFact> {
    let mut need = decide(key)?;
    need.show_pill = false;
    Some(need)
}

/// STOPPED WORK FIRST IN THE `+N` WALK (§14.7): from a front request, the next need is the
/// other request even where a report stands between the two by age, since `needing_order` puts
/// every report of a severity after the needs that stopped their agents.
#[test]
fn the_next_need_from_a_request_is_the_other_request_before_an_older_report() {
    let mut d = desk(vec![vec![1], vec![2], vec![3]]);
    d.wins.get_mut(&1).unwrap().front_tab = 1;
    d.publish(2, decide(2));
    d.now += Duration::from_secs(1);
    d.publish(1, report(1));
    d.now += Duration::from_secs(1);
    d.publish(3, decide(3));
    drive::refresh_window(&mut d, 1);
    assert_eq!(d.view(1).pressing, Some(2));
    assert_eq!(d.view(1).pill.as_ref().map(|pill| pill.session), Some(2));
    assert_eq!(
        d.next_need_after(1, 2),
        Some(3),
        "the other request, before the report older than it"
    );
    d.wins.get_mut(&1).unwrap().front_tab = 2;
    drive::refresh_window(&mut d, 1);
    assert_eq!(
        d.next_need_after(1, 3),
        Some(1),
        "past the last request, the report"
    );
}

/// FROM A FRONT REPORT, THE `+N` OPENS THE STOPPED WORK AFTER THE PILL'S (§14.7, §12.11): the
/// front tab's report, two requests and a younger report; the pill names the older request, and
/// its `+N` opens the other request, not the younger report that follows the front one. Where a
/// report stands between two requests by age, the walk still keeps to the requests.
#[test]
fn more_from_a_front_report_opens_the_other_request_before_a_younger_report() {
    let mut d = desk(vec![vec![1], vec![2], vec![3], vec![4]]);
    d.publish(1, report(1));
    d.now += Duration::from_secs(1);
    d.publish(2, decide(2));
    d.now += Duration::from_secs(1);
    d.publish(3, decide(3));
    d.now += Duration::from_secs(1);
    d.publish(4, report(4));
    let pill = d.view(1).pill.as_ref().expect("the older request's pill");
    assert_eq!((pill.session, pill.more), (2, 3));
    assert_eq!(
        d.next_need_after(1, 2),
        Some(3),
        "the other request, not the younger report"
    );

    let mut d = desk(vec![vec![1], vec![2], vec![3], vec![4]]);
    d.publish(1, report(1));
    d.now += Duration::from_secs(1);
    d.publish(2, decide(2));
    d.now += Duration::from_secs(1);
    d.publish(3, report(3));
    d.now += Duration::from_secs(1);
    d.publish(4, decide(4));
    assert_eq!(d.view(1).pill.as_ref().map(|pill| pill.session), Some(2));
    assert_eq!(
        d.next_need_after(1, 2),
        Some(4),
        "the other request, past a report older than it"
    );
}

/// Pressing session selection orders stuck before decide, then front session, then oldest need.
#[test]
fn pressing_session_takes_the_most_severe_need_and_the_front_wins_ties() {
    let mut d = desk(vec![vec![1, 2], vec![3], vec![4]]);
    d.publish(4, decide(40));
    let p = d.pressing_session(1).expect("a need");
    assert_eq!((p.session, p.tab, p.front_tab, p.more), (4, 2, false, 0));
    assert_eq!(d.view(1).rim, Rim::Decide);
    d.now += Duration::from_secs(1);
    d.publish(3, stuck(30));
    let p = d.pressing_session(1).expect("a need");
    assert_eq!(
        (p.session, p.more),
        (3, 1),
        "stuck outranks an older decide"
    );
    assert_eq!(d.view(1).rim, Rim::Stuck);
    d.now += Duration::from_secs(1);
    d.publish(2, stuck(20));
    assert_eq!(
        d.pressing_session(1).map(|p| p.session),
        Some(3),
        "the older stuck need of two non-front sessions"
    );
    d.now += Duration::from_secs(1);
    d.publish(1, stuck(10));
    let p = d.pressing_session(1).expect("a need");
    assert_eq!(
        (p.session, p.front_tab, p.more),
        (1, true, 3),
        "the front session wins a tie, however young its need"
    );
    let view = d.view(1);
    assert_eq!(view.pressing, Some(1));
    let pill = view.pill.as_ref().expect("the pill");
    assert_eq!(
        (pill.session, pill.tab, pill.more),
        (1, None, 3),
        "front: no tab number"
    );
    d.publish(1, None);
    let pill = d.view(1).pill.clone().expect("the pill");
    assert_eq!((pill.session, pill.tab), (3, Some(2)));
    assert!(!pill.label.is_empty());
}

/// The person's outline appears only on the front session while nothing in the window needs attention.
#[test]
fn yours_appears_only_on_the_front_session_with_no_need() {
    let mut d = desk(vec![vec![1], vec![2]]);
    let held = |d: &mut TestDesk, s: u64, on: bool| {
        let mut f = d.facts.borrow().get(&s).cloned().unwrap_or_default();
        f.held_by_person = on;
        d.set(s, f);
        drive::refresh_session(d, s);
    };
    held(&mut d, 2, true);
    assert_eq!(d.view(1).rim, Rim::None, "a background tab's hold");
    assert_eq!(d.table().slot(2).map(|s| s.chip()), Some(ChipLevel::Yours));
    held(&mut d, 1, true);
    assert_eq!(d.view(1).rim, Rim::Yours);
    let pill = d.view(1).pill.clone().expect("the pill");
    assert_eq!(pill.kind, PillKind::Yours);
    assert_eq!(pill.act, Some(PillAct::HandBack));
    d.publish(2, decide(2));
    assert_eq!(d.view(1).rim, Rim::Decide, "a need outranks the outline");
    d.publish(2, None);
    assert_eq!(d.view(1).rim, Rim::Yours);
    d.wins.get_mut(&1).unwrap().front_tab = 1;
    held(&mut d, 1, false);
    drive::refresh_window(&mut d, 1);
    assert_eq!(d.view(1).rim, Rim::Yours, "tab 2 is front and held");
    held(&mut d, 2, false);
    assert_eq!(d.view(1).rim, Rim::None);
}

/// Restart and resume actions are offered only on the front tab's stall.
#[test]
fn restart_and_resume_are_offered_only_in_front() {
    let frozen = Some(need(NeedSeverity::Stuck, StopCause::Frozen, 5));
    let mut d = desk(vec![vec![1], vec![2]]);
    d.publish(1, frozen.clone());
    assert_eq!(
        d.view(1).pill.as_ref().and_then(|p| p.act),
        None,
        "not carried"
    );
    d.restartable = vec![1, 2];
    drive::refresh_window(&mut d, 1);
    assert_eq!(
        d.view(1).pill.as_ref().and_then(|p| p.act),
        Some(PillAct::Restart)
    );
    d.publish(1, stuck(6));
    assert_eq!(
        d.view(1).pill.as_ref().and_then(|p| p.act),
        None,
        "a wall has no restart"
    );
    let stopped = Some(need(NeedSeverity::Stuck, StopCause::Suspended, 7));
    d.publish(1, stopped.clone());
    assert_eq!(
        d.view(1).pill.as_ref().map(|p| (p.session, p.act)),
        Some((1, Some(PillAct::Resume))),
        "a stopped job is resumed"
    );
    d.restartable.clear();
    drive::refresh_window(&mut d, 1);
    assert_eq!(
        d.view(1).pill.as_ref().and_then(|p| p.act),
        Some(PillAct::Resume),
        "resume needs no carried restart"
    );
    d.restartable = vec![1, 2];
    d.publish(1, None);
    for (cause, stall) in [("frozen", frozen), ("stopped", stopped)] {
        d.publish(2, stall);
        assert_eq!(
            d.view(1).pill.as_ref().map(|p| (p.session, p.act)),
            Some((2, None)),
            "a background tab's {cause} stall: go there first"
        );
    }
}

/// The view switch hides the border and the pill while keeping tab marks intact.
#[test]
fn the_switch_hides_the_border_and_the_pill_and_keeps_the_marks() {
    let mut d = desk(vec![vec![1]]);
    d.shown_on = false;
    d.publish(1, stuck(1));
    assert_eq!(d.view(1).rim, Rim::None);
    assert!(d.view(1).pill.is_none());
    assert_eq!(
        d.table().slot(1).map(|s| s.chip()),
        Some(ChipLevel::Stop(StopCause::Wall))
    );
    d.shown_on = true;
    d.log.borrow_mut().clear();
    drive::refresh_all(&mut d);
    assert_eq!(d.view(1).rim, Rim::Stuck);
    assert_eq!(d.took("restamp"), 1, "the strip carries the pill again");
}

/// Verdict flips on a background tab repaint the window and restamp its strip without side effects.
#[test]
fn a_hundred_verdict_flips_ask_only_for_a_repaint_and_a_restamp() {
    let mut d = desk(vec![vec![1], vec![2]]);
    for i in 0..100u64 {
        d.publish(2, if i % 2 == 0 { decide(i) } else { None });
    }
    assert_eq!(d.took("redraw"), 100);
    assert_eq!(d.took("restamp"), 100);
    let others = d
        .log
        .borrow()
        .iter()
        .filter(|l| {
            !["redraw", "restamp", "sense", "herald", "projecting"]
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
fn a_refresh_that_moved_nothing_repaints_nothing() {
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
    assert_eq!(d.took("redraw"), 0);
    assert_eq!(d.took("restamp"), 0);
    assert_eq!(d.took("herald"), 1, "the publication rides every refresh");
}

/// Visible pill ages, supervisor lapses, and herald deadlines arm and fire on tick.
#[test]
fn the_pill_age_ticks_only_where_seen_and_the_lapses_fold_in() {
    let mut d = desk(vec![vec![1], vec![2]]);
    let t0 = d.now;
    assert_eq!(drive::deadline(&d, t0), None, "a quiet desk");
    assert!(drive::tick(&mut d, t0).is_empty());
    d.publish(2, stuck(1));
    assert_eq!(drive::deadline(&d, t0), Some(t0), "a first tick is due");
    assert!(
        drive::tick(&mut d, t0).is_empty(),
        "the first tick paints nothing"
    );
    let due = drive::deadline(&d, t0).expect("the next figure");
    assert_eq!(due, t0 + Duration::from_secs(1));
    d.log.borrow_mut().clear();
    d.now = due;
    assert_eq!(drive::tick(&mut d, due), vec![1]);
    assert_eq!(d.took("pill [1]"), 1, "the pill alone is pushed");
    assert_eq!(d.took("restamp"), 0, "no chip moves with an age");
    d.wins.get_mut(&1).unwrap().visible = false;
    assert_eq!(drive::deadline(&d, due), None);
    d.supervisors.insert(1, due + Duration::from_secs(9));
    drive::refresh_session(&mut d, 1);
    assert_eq!(drive::deadline(&d, due), Some(due + Duration::from_secs(9)));
    d.herald_at = Some(due + Duration::from_secs(3));
    assert_eq!(drive::deadline(&d, due), Some(due + Duration::from_secs(3)));
    d.herald_at = None;
    let lapse = due + Duration::from_secs(9);
    d.now = lapse;
    drive::tick(&mut d, lapse);
    assert_eq!(d.took("lapse 1"), 1);
}

/// A renewed supervisor claim moves the deadline live without a refresh, falling back to the table's copy when gone.
#[test]
fn a_renewed_supervisor_claim_moves_the_deadline_without_a_refresh() {
    let hidden = || {
        let mut d = desk(vec![vec![1], vec![2]]);
        d.wins.get_mut(&1).unwrap().visible = false;
        d
    };
    let mut d = hidden();
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
    let mut stale = hidden();
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
    let _ = drive::tick(&mut d, lapse);
    assert_eq!(d.took("lapse 2"), 1);
    assert_eq!(drive::deadline(&d, lapse), None, "lapsed: nothing armed");
}

/// A glass window whose titlebar draws the pill pushes the pill at the age tick without moving the frame key.
#[test]
fn a_pill_age_on_glass_moves_no_frame_key() {
    let aged = |glass: bool| {
        let mut d = desk(vec![vec![1], vec![2]]);
        d.wins.get_mut(&1).unwrap().glass = glass;
        let t0 = d.now;
        d.publish(2, stuck(1));
        let _ = drive::tick(&mut d, t0);
        let before = d.view(1).fp();
        let due = drive::deadline(&d, t0).expect("the next figure");
        d.now = due;
        d.log.borrow_mut().clear();
        let repainted = drive::tick(&mut d, due);
        assert_eq!(d.took("pill [1]"), 1, "the pill is pushed either way");
        (repainted, before == d.view(1).fp())
    };
    assert_eq!(aged(true), (vec![], true), "glass: no frame moves");
    assert_eq!(
        aged(false),
        (vec![1], false),
        "control: the in-grid strip's does"
    );
}

/// A need on the front tab prints no age and arms no age tick.
#[test]
fn a_front_tab_pill_arms_no_age_tick() {
    let mut d = desk(vec![vec![1], vec![2]]);
    let t0 = d.now;
    d.publish(1, stuck(1));
    let pill = d.view(1).pill.clone().expect("lit");
    assert_eq!((pill.tab, pill.since), (None, None), "the front tab's pill");
    assert_eq!(drive::deadline(&d, t0), None, "no age, no tick");
    assert!(drive::tick(&mut d, t0 + Duration::from_secs(90)).is_empty());
    d.publish(1, None);
    d.publish(2, stuck(2));
    let pill = d.view(1).pill.clone().expect("lit");
    assert_eq!(pill.tab, Some(2));
    assert!(pill.since.is_some());
    assert_eq!(drive::deadline(&d, t0), Some(t0), "its first tick is due");
}

/// The pill names a need in an unfocused pane of the front tab by its label without tab number or age.
#[test]
fn a_need_in_another_pane_of_the_front_tab_is_named() {
    let mut d = desk(vec![vec![1, 3]]);
    d.publish(3, decide(1));
    let pill = d.view(1).pill.clone().expect("lit");
    assert_eq!(pill.session, 3);
    assert_eq!(pill.label, "tab-of-3");
    assert_eq!((pill.tab, pill.since), (None, None));
    d.publish(1, decide(2));
    let pill = d.view(1).pill.clone().expect("lit");
    assert_eq!(pill.session, 1);
    assert!(pill.label.is_empty());
}

/// `next_need_after` walks the window's needs in priority order and wraps around.
#[test]
fn more_walks_the_windows_needs_and_wraps() {
    let mut d = desk(vec![vec![1], vec![2], vec![3]]);
    d.publish(2, decide(1));
    assert_eq!(d.next_need_after(1, 2), None, "one need: nowhere else");
    d.now += Duration::from_secs(1);
    d.publish(3, decide(2));
    d.now += Duration::from_secs(1);
    d.publish(1, stuck(3));
    assert_eq!(d.view(1).pill.as_ref().map(|p| p.session), Some(1));
    assert_eq!(d.next_need_after(1, 1), Some(2));
    d.wins.get_mut(&1).unwrap().front_tab = 1;
    assert_eq!(d.next_need_after(1, 1), Some(3));
    d.wins.get_mut(&1).unwrap().front_tab = 2;
    assert_eq!(d.next_need_after(1, 1), Some(1), "it wraps");
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
    drive::human_acted(&mut d, 1);
    assert_eq!(d.view(1).watermark(1), 0, "not calm: not read");
    let mut free = d.facts.borrow()[&1].clone();
    free.hold = None;
    d.set(1, free);
    drive::refresh_session(&mut d, 1);
    drive::human_acted(&mut d, 1);
    assert_eq!(drive::session_level(&d, 1), Level::Quiet, "read");
    let seq = drive::tell(&mut d, 1).expect("held");
    assert!(drive::status_tail(&d, 1).contains(&format!("story={seq}")));
    assert_eq!(drive::tell(&mut d, 99), None);
}

/// `chrome_line` prints the border, the pressing session, and the pill.
#[test]
fn the_chrome_line_names_the_rim_the_pressing_session_and_the_pill() {
    let mut d = desk(vec![vec![1], vec![2]]);
    let quiet = drive::chrome_line(&d, Some(1));
    assert_eq!(quiet, "presence rim=none pressing=- pill=\"\"");
    assert_eq!(drive::chrome_line(&d, None), quiet);
    d.publish(2, stuck(1));
    let line = drive::chrome_line(&d, Some(1));
    let field = |k: &str| {
        line.split(' ')
            .find_map(|w| w.strip_prefix(&format!("{k}=")))
            .map(str::to_string)
    };
    assert_eq!(field("rim").as_deref(), Some("stuck"));
    assert_eq!(field("pressing").as_deref(), Some("@s-2"));
    assert!(line.contains("pill=\"\u{25c6} "), "{line}");
    assert_eq!(
        drive::status_tail(&d, 2),
        "hand=- hand_pid=- level=quiet story=0 why=-"
    );
}

/// A session shown in two panes of one tab counts once.
#[test]
fn a_session_shown_twice_counts_once() {
    let mut d = desk(vec![vec![1, 1], vec![2]]);
    d.publish(1, stuck(1));
    d.publish(2, stuck(2));
    assert_eq!(d.pressing_session(1).map(|p| p.more), Some(1));
}

#[test]
fn a_shared_need_uses_its_visible_tab_for_the_pill() {
    for (request, act) in [
        (decide(1), None),
        (
            Some(need(NeedSeverity::Stuck, StopCause::Frozen, 1)),
            Some(PillAct::Restart),
        ),
        (
            Some(need(NeedSeverity::Stuck, StopCause::Suspended, 1)),
            Some(PillAct::Resume),
        ),
    ] {
        let mut d = desk(vec![vec![1], vec![2], vec![1]]);
        d.wins.get_mut(&1).unwrap().front_tab = 2;
        d.restartable = vec![1];
        d.publish(1, request);
        let mut report = if act.is_some() { decide(2) } else { stuck(2) }.unwrap();
        report.show_pill = false;
        d.publish(2, Some(report));

        let pill = d.view(1).pill.as_ref().expect("the direct request");
        assert_eq!(pill.session, 1);
        assert!(pill.label.is_empty(), "the focused session needs no label");
        assert_eq!(pill.tab, None);
        assert_eq!(pill.since, None, "the front tab has no age ticker");
        assert_eq!(pill.act, act);
        assert_eq!(pill.more, 1, "the other report counts once");

        d.wins.get_mut(&1).unwrap().front_tab = 1;
        drive::refresh_window(&mut d, 1);
        let pill = d.view(1).pill.as_ref().expect("the background request");
        assert_eq!(pill.tab, Some(1), "no visible copy: use its first tab");
        assert!(pill.since.is_some());
        assert_eq!(pill.act, None);
    }
}
