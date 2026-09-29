// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence DRIVER proves itself without a window system (design ruling
//! 353): a [`TestDesk`] of plain windows and sessions drives the refresh, the
//! row's commit, the fold law, the ripple and the pulse, a told story, the
//! deadline fold and the tick, and records every effect it is asked for.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use crate::presence::drive::{self, Desk, Table};
use crate::presence::{
    FOLD_LOOK_CAP, FOLD_QUIET, Facts, Hand, HoldFact, LeaseMark, Level, Rim, StoryVerb, TOLD_FLASH,
    TurnFact, View,
};
use crate::presence_tests::TestHost;
use crate::{Duration, Instant};

type TFacts = Facts<TestHost>;

/// One plain window: its view, the session in front, the sessions its other
/// tabs show, and what the host reports about it.
struct Win {
    view: View,
    front: Option<u64>,
    tabs: Vec<u64>,
    visible: bool,
    key: bool,
    rows: u16,
    windowed: bool,
}

impl Win {
    fn new(front: u64) -> Self {
        Self {
            view: View::default(),
            front: Some(front),
            tabs: Vec::new(),
            visible: true,
            key: true,
            rows: 24,
            windowed: true,
        }
    }
}

/// A host of plain data: the facts it senses, its supervisors' lapses, its
/// toggles and policies, and a log of every effect the driver asks for.
struct TestDesk {
    now: Instant,
    table: Table<TestHost>,
    wins: BTreeMap<u32, Win>,
    /// What `sense` reads — a cell, so a lease let go mid-refresh changes
    /// the facts the NEXT sense reads, as the host's own lock would.
    facts: RefCell<HashMap<u64, TFacts>>,
    supervisors: HashMap<u64, Instant>,
    band: bool,
    rim: bool,
    frozen: bool,
    /// What [`Desk::driver_may_type`] answers for every window, beside the
    /// live lease map (main's held-row tests and the fold quiet's holds).
    hand_on: bool,
    /// What [`Desk::looked_at`] answers for every window.
    looked: Option<Instant>,
    ripples: bool,
    herald_at: Option<Instant>,
    /// The LIVE lease per session ([`Desk::lease_mark`];
    /// [`Desk::driver_may_type`] reads `Typing` from it; `sense` reports
    /// `facts`).
    live: RefCell<HashMap<u64, LeaseMark>>,
    /// A lease let go the instant after `sense` reads this session: the
    /// control thread's `lease release` landing between the refresh's read
    /// of the facts and its commit of the row. The live mark goes `Free`
    /// and the facts the next sense reads become these.
    release_after_sense: RefCell<Option<(u64, TFacts)>>,
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
            band: true,
            rim: true,
            frozen: false,
            hand_on: false,
            looked: None,
            ripples: true,
            herald_at: None,
            live: RefCell::new(HashMap::new()),
            release_after_sense: RefCell::new(None),
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
        self.wins.get(&w).and_then(|x| x.front)
    }

    fn windows_showing(&self, session: u64) -> Vec<u32> {
        let mut out: Vec<u32> = self
            .wins
            .iter()
            .filter(|(_, x)| x.front == Some(session))
            .map(|(w, _)| *w)
            .collect();
        for (w, x) in &self.wins {
            if x.tabs.contains(&session) && !out.contains(w) {
                out.push(*w);
            }
        }
        out
    }

    fn holds_session(&self, session: u64) -> bool {
        self.facts.borrow().contains_key(&session)
    }

    fn sense(&self, session: u64) -> Option<TFacts> {
        let facts = self.facts.borrow().get(&session).cloned();
        let due = self
            .release_after_sense
            .borrow()
            .as_ref()
            .is_some_and(|(s, _)| *s == session);
        if due && let Some((_, after)) = self.release_after_sense.borrow_mut().take() {
            self.live.borrow_mut().insert(session, LeaseMark::Free);
            self.facts.borrow_mut().insert(session, after);
        }
        facts
    }

    fn supervisor_lapse(&self, session: u64, _now: Instant) -> Option<Instant> {
        self.supervisors.get(&session).copied()
    }

    fn band_visible(&self, w: u32) -> bool {
        self.wins.get(&w).is_some_and(|x| x.visible)
    }

    fn key_window(&self, w: u32) -> bool {
        self.wins.get(&w).is_some_and(|x| x.key)
    }

    fn ripple_allowed(&self, _w: u32, focused: bool) -> bool {
        self.ripples && focused
    }

    fn band_toggle(&self) -> bool {
        self.band
    }

    fn rim_toggle(&self) -> bool {
        self.rim
    }

    fn rows_frozen(&self) -> bool {
        self.frozen
    }

    fn driver_may_type(&self, w: u32) -> bool {
        // `hand_on` stands for a hand the live map does not model (main's
        // held-row line tests); the live lease map is the race tests' hand.
        if self.hand_on {
            return true;
        }
        let Some(win) = self.wins.get(&w) else {
            return false;
        };
        let live = self.live.borrow();
        win.front
            .iter()
            .chain(&win.tabs)
            .any(|s| live.get(s) == Some(&LeaseMark::Typing))
    }

    fn lease_mark(&self, session: u64) -> Option<LeaseMark> {
        Some(
            self.live
                .borrow()
                .get(&session)
                .copied()
                .unwrap_or_default(),
        )
    }

    fn looked_at(&self, _w: u32) -> Option<Instant> {
        self.looked
    }

    fn grid_rows(&self, w: u32) -> Option<(u16, bool)> {
        self.wins.get(&w).map(|x| (x.rows, x.windowed))
    }

    fn herald_deadline(&self, _now: Instant) -> Option<Instant> {
        self.herald_at
    }

    fn projecting(&mut self, w: u32) {
        self.note(format!("project {w}"));
    }

    fn regrid(&mut self, w: u32) {
        self.note(format!("regrid {w}"));
    }

    fn request_redraw(&self, w: u32) {
        self.note(format!("redraw {w}"));
    }

    fn restamp_chips(&mut self, windows: Vec<u32>) {
        self.note(format!("chips {windows:?}"));
    }

    fn herald(&mut self, session: u64) {
        self.note(format!("herald {session}"));
    }

    fn herald_owed(&mut self, _now: Instant) {
        self.note("owed".into());
    }

    fn lapse_supervisor(&mut self, session: u64) {
        self.supervisors.remove(&session);
        self.note(format!("lapse {session}"));
    }

    fn forget(&mut self, session: u64) {
        self.table.retire(session);
        self.note(format!("forget {session}"));
    }

    fn chime(&mut self, _now: Instant) {
        self.note("chime".into());
    }
}

/// A peer's turn on the session; `agent_seq` 1 so a repeat is no change.
fn driven() -> TFacts {
    TFacts {
        agent_seq: 1,
        hand: Hand::DrivenTurn {
            id: 4,
            holder: Some("manager".into()),
        },
        ..TFacts::default()
    }
}

/// Two windows: `0` shows session 1 in front and session 2 in a tab, `1`
/// shows session 2 in front. Both sessions are quiet.
fn desk() -> TestDesk {
    let mut d = TestDesk::new(Instant::now());
    let mut w0 = Win::new(1);
    w0.tabs.push(2);
    d.wins.insert(0, w0);
    d.wins.insert(1, Win::new(2));
    d.facts.get_mut().insert(1, TFacts::default());
    d.facts.get_mut().insert(2, TFacts::default());
    d
}

/// THE IDLE LAW: a quiet desktop refreshed arms no deadline, ticks nothing,
/// pays no re-grid and folds a zero repaint term, whatever the instant.
#[test]
fn a_quiet_desk_arms_nothing_and_ticks_nothing() {
    let mut d = desk();
    drive::refresh_session(&mut d, 1, false);
    drive::refresh_session(&mut d, 2, false);
    drive::refresh_all(&mut d);
    for s in [0u64, 1, 61, 3_600] {
        let t = d.now + Duration::from_secs(s);
        assert_eq!(drive::deadline(&d, t), None);
        assert!(drive::tick(&mut d, t).is_empty());
        assert_eq!(d.view(0).fp(t), 0);
        assert_eq!(d.view(1).fp(t), 0);
    }
    assert_eq!(d.took("regrid"), 0);
    assert_eq!(d.took("redraw"), 0);
    // NEGATIVE CONTROL: a hand on session 1 is a row, a rim and a deadline.
    d.facts.get_mut().insert(1, driven());
    drive::refresh_session(&mut d, 1, false);
    assert_ne!(d.view(0).fp(d.now), 0);
    assert!(drive::deadline(&d, d.now).is_some());
}

/// A refresh folds the facts in and projects them onto every window showing
/// the session — the front window shows them, a window with the session in
/// a background tab shows its own front — pays ONE re-grid for the row, and
/// re-stamps the chips; the same facts again change nothing.
#[test]
fn a_refresh_projects_the_front_session_and_pays_one_regrid() {
    let mut d = desk();
    d.facts.get_mut().insert(1, driven());
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.view(0).level, Level::Driven);
    assert_eq!(d.view(0).rim, Rim::Drive);
    assert_eq!(d.view(0).rows, 1);
    assert!(d.view(0).words.is_some());
    assert_eq!(d.took("regrid 0"), 1);
    assert_eq!(d.took("redraw 0"), 1);
    assert_eq!(d.took("herald 1"), 1);
    assert_eq!(d.took("chips [0]"), 1);
    assert_eq!(d.view(1).level, Level::Quiet, "window 1 fronts session 2");
    // The change gate: the same facts move nothing, and ask for nothing.
    let seed = d.view(0).seed;
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.view(0).seed, seed);
    assert_eq!(d.took("regrid"), 1);
    assert_eq!(d.took("chips"), 1);
    // A background tab's session re-projects the windows that show it.
    d.facts.get_mut().insert(
        2,
        TFacts {
            hold: Some(HoldFact {
                reason: "review".into(),
                fleet: false,
            }),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 2, false);
    assert_eq!(d.took("chips [1, 0]"), 1, "front first, then the tab");
    assert_eq!(d.view(1).level, Level::Hold);
    assert_eq!(d.view(0).level, Level::Driven, "window 0 still fronts 1");
    // The band toggle hides the row and keeps the level; the rim toggle
    // hides the rim.
    d.band = false;
    d.rim = false;
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).level, Level::Driven);
    assert!(d.view(0).words.is_none());
    assert_eq!(d.view(0).rows, 0);
    assert_eq!(d.view(0).rim, Rim::None);
}

/// The row yields to the last terminal row of a real window (a headless one
/// takes it whatever its height), and a handoff freezes the count.
#[test]
fn the_row_yields_to_the_last_terminal_row_and_freezes_under_a_handoff() {
    let mut d = desk();
    d.facts.get_mut().insert(1, driven());
    d.wins.get_mut(&0).unwrap().rows = 1;
    drive::refresh_session(&mut d, 1, false);
    assert!(d.view(0).words.is_some());
    assert_eq!(d.view(0).rows, 0, "one terminal row is never given up");
    d.wins.get_mut(&0).unwrap().windowed = false;
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).rows, 1, "headless takes the row");
    d.frozen = true;
    d.facts.get_mut().insert(1, TFacts::default());
    drive::refresh_session(&mut d, 1, false);
    assert!(d.view(0).words.is_none());
    assert_eq!(d.view(0).rows, 1, "frozen mid-handoff");
    // Frozen, the held fold names no wake: the tick could not act on it.
    assert_eq!(drive::deadline(&d, d.now), None);
    d.frozen = false;
    drive::refresh_all(&mut d);
    assert_eq!(
        d.view(0).rows,
        1,
        "the fold quiet runs from the want's drop"
    );
    d.now += FOLD_QUIET;
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).rows, 0);
    assert_eq!(d.took("regrid 0"), 2);
}

/// THE LINE SAYS WHAT IS DRAWN (ruling 365; day nine, D1): a row the
/// facts want that the window holds back — a driver's hand on the session,
/// a window one row tall, a handoff's freeze — prints `band=""` and
/// `sentence=""` with the reason after them, and the row the hand held is
/// born, and printed, once the hand lifts. Control: with no hold the line
/// carries the row and no `held=`.
#[test]
fn the_chrome_line_prints_the_drawn_row_and_says_why_one_is_held() {
    let mut d = desk();
    drive::refresh_all(&mut d);
    d.hand_on = true;
    d.facts.get_mut().insert(1, driven());
    drive::refresh_session(&mut d, 1, false);
    assert!(d.view(0).words.is_some(), "the facts want a row");
    assert_eq!(d.view(0).rows, 0, "no re-grid under the hand");
    assert_eq!(
        drive::chrome_line(&d, Some(0), 80),
        "presence rim=drive level=driven band=\"\" sentence=\"\" held=driver"
    );
    d.hand_on = false;
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).rows, 1, "born once the hand lifts");
    let line = drive::chrome_line(&d, Some(0), 80);
    assert!(!line.contains("held="), "{line}");
    assert!(
        !line.contains("band=\"\""),
        "the drawn row is printed: {line}"
    );
    // A window one row tall keeps its terminal row.
    d.wins.get_mut(&0).unwrap().rows = 1;
    d.wins.get_mut(&0).unwrap().view.rows = 0;
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).rows, 0);
    assert!(
        drive::chrome_line(&d, Some(0), 80).ends_with(" held=height"),
        "{}",
        drive::chrome_line(&d, Some(0), 80)
    );
    // A handoff's freeze.
    d.frozen = true;
    assert!(drive::chrome_line(&d, Some(0), 80).ends_with(" held=handoff"));
}

/// A cooperative drive lease, live: the facts a refresh senses under it.
fn leased() -> TFacts {
    TFacts {
        agent_seq: 1,
        hand: Hand::DrivenLease {
            holder: "probe".into(),
        },
        typing: true,
        lease: LeaseMark::Typing,
        ..TFacts::default()
    }
}

/// THE LEASE RACE (measured 2026-09-28 on a live window: `lease acquire` then
/// `lease release` under 0.1 ms apart raised the row and folded it again,
/// 24 → 23 → 24 inside 0.35 ms, in 3 runs of 10). The refresh reads the
/// facts under the live lease — the words say "driven" — and the release
/// lands before it commits the row, where the host's LIVE read already says
/// nobody may type. The row must not be born for a hand already gone: a
/// lease that moved since the words were formed holds it, and the next
/// refresh — the release's wake, or any other — re-senses and finds nothing
/// to re-grid. Deterministic: the desk lets the lease go the instant after
/// `sense` reads the facts.
#[test]
fn a_lease_let_go_between_the_sense_and_the_commit_raises_no_row() {
    let mut d = desk();
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.took("regrid"), 0);
    // `lease acquire`: the live lease and the facts both say a hand may type.
    d.live.borrow_mut().insert(1, LeaseMark::Typing);
    d.facts.get_mut().insert(1, leased());
    // …and `lease release` lands between the refresh's read and its commit.
    *d.release_after_sense.get_mut() = Some((1, TFacts::default()));
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.live.borrow()[&1], LeaseMark::Free, "let go mid-refresh");
    assert_eq!(d.view(0).level, Level::Driven, "the words say driven");
    assert!(d.view(0).words.is_some());
    assert_eq!(
        (d.view(0).rows, d.took("regrid")),
        (0, 0),
        "a row was born for a hand already gone"
    );
    // Any other refresh of the window before the release's wake (a tick, a
    // focus change) re-senses the moved lease rather than projecting the
    // gone hand's words: nobody, no words, and nothing to fold.
    drive::refresh_all(&mut d);
    assert!(d.view(0).words.is_none(), "the tick re-sensed the release");
    assert_eq!((d.view(0).rows, d.took("regrid")), (0, 0));
    // The release's own wake then finds nothing to do.
    drive::refresh_session(&mut d, 1, false);
    assert_eq!((d.view(0).rows, d.took("regrid")), (0, 0));
    // The live read still holds a lease taken AFTER the facts were read: a
    // row another fact wants (a hold) waits out the hand.
    let held = TFacts {
        hold: Some(HoldFact {
            reason: "review".into(),
            fleet: false,
        }),
        ..TFacts::default()
    };
    d.live.borrow_mut().insert(1, LeaseMark::Typing);
    d.facts.get_mut().insert(1, held);
    drive::refresh_session(&mut d, 1, false);
    assert!(d.view(0).words.is_some());
    assert_eq!((d.view(0).rows, d.took("regrid")), (0, 0));
    // …and is born once the hand is gone and a refresh sees it.
    d.live.borrow_mut().insert(1, LeaseMark::Free);
    drive::refresh_window(&mut d, 0);
    assert_eq!((d.view(0).rows, d.took("regrid 0")), (1, 1));
}

/// THE SAME RACE AT A TURN'S END (review of 2026-09-28): `turn` ends its
/// input — the lease stops saying a driver may type, and a wake is posted —
/// and, with no settle to wait (an unverified submit, a hang-up, a settle
/// pattern already on screen), releases its lease right after. A refresh that
/// senses the settling turn and commits after the release saw words that say
/// "driven" and facts that say nobody TYPES (`typing` false, no live hand):
/// the first repair, a hold on the snapshot's typing alone, let that row be
/// born and folded on the release's wake. The moved lease holds it.
#[test]
fn a_turn_released_right_after_its_input_ended_raises_no_row() {
    let mut d = desk();
    drive::refresh_session(&mut d, 1, false);
    let settling = TFacts {
        lease: LeaseMark::Settling,
        ..driven()
    };
    d.live.borrow_mut().insert(1, LeaseMark::Settling);
    d.facts.get_mut().insert(1, settling);
    *d.release_after_sense.get_mut() = Some((1, TFacts::default()));
    // `end_turn_input`'s wake, with the guard's release inside its refresh.
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.view(0).level, Level::Driven, "the words say driven");
    assert!(!d.table().slot(1).unwrap().typing, "nobody types: settling");
    assert_eq!(
        (d.view(0).rows, d.took("regrid")),
        (0, 0),
        "a row was born for a turn already over"
    );
    // The release's wake: re-sensed, nothing to fold.
    drive::refresh_session(&mut d, 1, false);
    assert!(d.view(0).words.is_none());
    assert_eq!((d.view(0).rows, d.took("regrid")), (0, 0));
    // A settle that really runs still births the row inside the turn, where
    // the settle waits out the program's repaint of it.
    d.live.borrow_mut().insert(1, LeaseMark::Settling);
    d.facts.get_mut().insert(
        1,
        TFacts {
            lease: LeaseMark::Settling,
            ..driven()
        },
    );
    drive::refresh_session(&mut d, 1, false);
    assert_eq!((d.view(0).rows, d.took("regrid 0")), (1, 1));
}

/// THE FOLD LAW: a keystroke reads a CALM session's story — the watermark
/// moves to the newest point and the row folds — and does nothing while
/// anything is still happening (a hold), or twice.
#[test]
fn the_fold_law_moves_the_watermark_only_when_calm() {
    let mut d = desk();
    let turn = TFacts {
        turn: Some(TurnFact {
            id: 1,
            settled: true,
            dur_ms: 10,
            carried: false,
        }),
        ..TFacts::default()
    };
    d.facts.get_mut().insert(1, turn);
    drive::refresh_session(&mut d, 1, false);
    d.facts.get_mut().insert(
        1,
        TFacts {
            turn: Some(TurnFact {
                id: 2,
                settled: true,
                dur_ms: 10,
                carried: false,
            }),
            hold: Some(HoldFact {
                reason: "pause".into(),
                fleet: false,
            }),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.view(0).level, Level::Hold);
    drive::human_acted(&mut d, 0);
    assert_eq!(d.view(0).watermark(1), 0, "not calm: nothing is read");
    let lifted = TFacts {
        hold: None,
        ..d.facts.get_mut()[&1].clone()
    };
    d.facts.get_mut().insert(1, lifted);
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.view(0).level, Level::Story);
    assert_eq!(d.view(0).rows, 1);
    let regrids = d.took("regrid");
    // The row has stood its minimum life: the read folds it at once.
    d.now += FOLD_QUIET;
    drive::human_acted(&mut d, 0);
    let seq = d.table.slot(1).unwrap().story_seq;
    assert!(seq > 0);
    assert_eq!(d.view(0).watermark(1), seq);
    assert_eq!(d.view(0).level, Level::Quiet);
    assert_eq!(d.view(0).rows, 0, "the row folds");
    assert_eq!(d.took("regrid"), regrids + 1, "one PTY resize");
    // Read already: a second keystroke changes nothing.
    let seed = d.view(0).seed;
    drive::human_acted(&mut d, 0);
    assert_eq!(d.view(0).seed, seed);
    assert_eq!(
        d.view(1).watermark(1),
        0,
        "another window's mark is its own"
    );
}

/// THE FOLD LAW READS THE STORY THE PERSON SAW (ruling 372): a press that
/// moved the front — another split pane, or a Settings tab with no session —
/// reads the session that was front before it, never the one it made front.
#[test]
fn the_fold_law_on_a_press_reads_the_session_seen_before_it() {
    let mut d = desk();
    let settled = |id| TFacts {
        turn: Some(TurnFact {
            id,
            settled: true,
            dur_ms: 10,
            carried: false,
        }),
        ..TFacts::default()
    };
    for session in [1, 2] {
        d.facts.get_mut().insert(session, settled(1));
        drive::refresh_session(&mut d, session, false);
        let seq = drive::tell(&mut d, session, StoryVerb::Approval, "").expect("held");
        assert!(seq > 0);
    }
    let seq1 = d.table.slot(1).unwrap().story_seq;
    let seq2 = d.table.slot(2).unwrap().story_seq;
    // The press brought session 2 front in window 0 (a click in its pane):
    // session 1, the one the person was reading, is read; 2 stays unread.
    d.wins.get_mut(&0).unwrap().front = Some(2);
    drive::human_acted_on(&mut d, 0, 1);
    assert_eq!(d.view(0).watermark(1), seq1, "the story seen is read");
    assert_eq!(
        d.view(0).watermark(2),
        0,
        "the pane the press focused is not"
    );
    // The negative control: the front-reading law, run after that press,
    // reads the pane nobody saw yet.
    drive::human_acted(&mut d, 0);
    assert_eq!(d.view(0).watermark(2), seq2);
    // A press that made a Settings tab front (no front session) still reads
    // what was front before it; the plain law finds nothing to read there.
    d.wins.get_mut(&1).unwrap().front = None;
    drive::human_acted(&mut d, 1);
    assert_eq!(d.view(1).watermark(2), 0, "no front session: nothing read");
    drive::human_acted_on(&mut d, 1, 2);
    assert_eq!(
        d.view(1).watermark(2),
        seq2,
        "the story seen before the press"
    );
}

/// A turn submit ripples the FRONT window only, and only where the motion
/// policy lets it; a choice pulse ignores key focus but needs the rim.
#[test]
fn a_submit_ripples_the_front_window_and_a_pulse_needs_the_rim() {
    let mut d = desk();
    d.wins.get_mut(&1).unwrap().tabs.push(1);
    d.facts.get_mut().insert(1, driven());
    drive::refresh_session(&mut d, 1, true);
    assert_eq!(d.view(0).ripple_at, Some(d.now));
    assert!(!d.view(0).ripple_chose);
    assert_eq!(
        d.view(1).ripple_at,
        None,
        "a background tab does not ripple"
    );
    // The motion policy says no: the ripple never starts.
    let mut still = desk();
    still.ripples = false;
    still.facts.get_mut().insert(1, driven());
    drive::refresh_session(&mut still, 1, true);
    assert_eq!(still.view(0).ripple_at, None);
    // An unfocused window's turn ripple waits for focus; its pulse does not.
    let mut away = desk();
    away.wins.get_mut(&0).unwrap().key = false;
    let t = away.now;
    drive::ripple(&mut away, 0, t);
    assert_eq!(away.view(0).ripple_at, None);
    drive::pulse(&mut away, 0, t);
    assert_eq!(away.view(0).ripple_at, Some(away.now));
    assert!(away.view(0).ripple_chose);
    assert_eq!(away.took("redraw 0"), 1);
    // The pulse IS a rim flash: the rim toggle off means none.
    let mut rimless = desk();
    rimless.rim = false;
    let t = rimless.now;
    drive::pulse(&mut rimless, 0, t);
    assert_eq!(rimless.view(0).ripple_at, None);
}

/// A told story lands on a held session only; a `chose` pulses every window
/// whose FRONT tab shows the session and chimes once.
#[test]
fn a_told_chose_pulses_every_front_window_and_chimes_once() {
    let mut d = desk();
    d.wins.insert(2, Win::new(1));
    d.wins.get_mut(&1).unwrap().tabs.push(1);
    assert_eq!(drive::tell(&mut d, 9, StoryVerb::Chose, "x"), None);
    assert_eq!(d.took("chime"), 0);
    assert_eq!(drive::tell(&mut d, 1, StoryVerb::Chose, "x"), Some(1));
    assert!(d.view(0).ripple_chose && d.view(2).ripple_chose);
    assert_eq!(d.view(1).ripple_at, None, "a background tab is not pulsed");
    assert_eq!(d.took("chime"), 1);
    assert_eq!(d.took("chips [0, 2, 1]"), 2, "the refresh's and the tell's");
    assert_eq!(d.view(0).level, Level::Story);
    // Any other verb: no pulse, no chime.
    let mut e = desk();
    assert_eq!(drive::tell(&mut e, 1, StoryVerb::Approval, ""), Some(1));
    assert_eq!(e.view(0).ripple_at, None);
    assert_eq!(e.took("chime"), 0);
}

/// THE ONE DEADLINE: a ripple's next step, a FRONT session's told flash, an
/// on-screen row's own clock, a lease's and a supervisor's lapse, and the
/// host's owed notification — the earliest wins, and each is armed only by
/// its own fact.
#[test]
fn the_deadline_folds_each_source_only_where_it_applies() {
    let mut d = desk();
    let t0 = d.now;
    // A told flash on a hidden window: the flash alone arms the timer.
    d.wins.get_mut(&0).unwrap().visible = false;
    let _ = drive::tell(&mut d, 1, StoryVerb::Approval, "");
    assert!(d.view(0).words.is_some());
    assert_eq!(drive::deadline(&d, t0), Some(t0 + TOLD_FLASH));
    // Its band on screen: the row's own clock joins the fold.
    d.wins.get_mut(&0).unwrap().visible = true;
    let due = d.view(0).words_due.expect("armed by the show");
    assert_eq!(drive::deadline(&d, t0), Some(due.min(t0 + TOLD_FLASH)));
    // A row not yet ticked is due now; a late one never before now.
    d.wins.get_mut(&0).unwrap().view.words_due = None;
    assert_eq!(drive::deadline(&d, t0), Some(t0));
    let late = t0 + Duration::from_secs(5);
    d.wins.get_mut(&0).unwrap().view.words_due = Some(t0);
    assert_eq!(drive::deadline(&d, late), Some(late));
    // A background tab's flash arms nothing: session 2 fronts nowhere hidden.
    let mut b = desk();
    b.wins.get_mut(&1).unwrap().front = None;
    b.wins.get_mut(&0).unwrap().visible = false;
    let _ = drive::tell(&mut b, 2, StoryVerb::Approval, "");
    assert_eq!(drive::deadline(&b, b.now), None);
    // A ripple's next step.
    let mut r = desk();
    let t = r.now;
    drive::ripple(&mut r, 0, t);
    let step = drive::deadline(&r, r.now).expect("a ripple steps");
    assert!(step > r.now && step <= r.now + Duration::from_millis(40));
    // A lease's lapse, a supervisor's, the herald — the earliest wins.
    let mut l = desk();
    l.wins.get_mut(&0).unwrap().visible = false;
    l.facts.get_mut().insert(
        1,
        TFacts {
            hand: Hand::DrivenLease {
                holder: "manager".into(),
            },
            lease_until: Some(l.now + Duration::from_secs(9)),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut l, 1, false);
    assert_eq!(
        drive::deadline(&l, l.now),
        Some(l.now + Duration::from_secs(9))
    );
    l.supervisors.insert(2, l.now + Duration::from_secs(7));
    drive::refresh_session(&mut l, 2, false);
    assert_eq!(
        drive::deadline(&l, l.now),
        Some(l.now + Duration::from_secs(7))
    );
    l.herald_at = Some(l.now + Duration::from_secs(5));
    assert_eq!(
        drive::deadline(&l, l.now),
        Some(l.now + Duration::from_secs(5))
    );
    // A lapse already past is due now, not in the past.
    let past = l.now + Duration::from_secs(60);
    l.herald_at = None;
    assert_eq!(drive::deadline(&l, past), Some(past));
}

/// THE TICK: a lapsed supervisor is lapsed and its session re-read, a lapsed
/// lease is re-read and dropped, a session the host no longer holds is
/// forgotten, a finished ripple retires (a repaint), and a row recomposes
/// only when its own clock is due.
#[test]
fn the_tick_lapses_retires_and_recomposes_on_its_own_clock() {
    let mut d = desk();
    let t0 = d.now;
    d.facts.get_mut().insert(
        1,
        TFacts {
            hand: Hand::DrivenLease {
                holder: "manager".into(),
            },
            lease_until: Some(t0 + Duration::from_secs(2)),
            ..TFacts::default()
        },
    );
    d.supervisors.insert(2, t0 + Duration::from_secs(3));
    drive::refresh_session(&mut d, 1, false);
    drive::refresh_session(&mut d, 2, false);
    drive::ripple(&mut d, 0, t0);
    // A session that left the host, with a lease on its slot.
    d.facts.get_mut().insert(
        7,
        TFacts {
            lease_until: Some(t0 + Duration::from_secs(1)),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 7, false);
    d.facts.get_mut().remove(&7);
    // Mid-ripple, before any lapse: nothing is dirty but the ripple steps.
    assert!(drive::tick(&mut d, t0 + Duration::from_millis(100)).is_empty());
    assert_eq!(d.took("owed"), 1, "the herald is asked every tick");
    // The ripple ends and the orphan's lease lapses.
    let t = t0 + Duration::from_millis(1_500);
    assert_eq!(drive::tick(&mut d, t), vec![0]);
    assert_eq!(d.view(0).ripple_at, None);
    assert_eq!(d.took("forget 7"), 1);
    assert!(d.table.slot(7).is_none());
    // The lease lapses: re-read (the host still reports it standing) and
    // dropped, so it is not re-armed every tick.
    let t = t0 + Duration::from_secs(2);
    let _ = drive::tick(&mut d, t);
    assert_eq!(d.table.slot(1).unwrap().lease_until, None);
    // The supervisor's claim lapses: the host lapses it and the re-read
    // drops the deadline.
    let t = t0 + Duration::from_secs(3);
    let _ = drive::tick(&mut d, t);
    assert_eq!(d.took("lapse 2"), 1);
    assert!(!d.log.borrow().iter().any(|l| l == "forget 2"));
    // The row's own clock: before it is due nothing recomposes.
    let due = d.view(0).words_due.expect("a row is up");
    let seed = d.view(0).seed;
    let before = due - Duration::from_millis(1);
    assert!(before > t0);
    let _ = drive::tick(&mut d, before);
    assert_eq!(d.view(0).seed, seed);
    assert_eq!(d.view(0).words_due, Some(due));
    let _ = drive::tick(&mut d, due);
    assert!(d.view(0).words_due.is_some_and(|x| x > due), "re-armed");
}

/// THE WIRE STRINGS (ruling 358): the `status` tail and the `chrome` line
/// are the engine's bytes. The hand's free text goes through the HOST's
/// codec (the test host encodes a space), the level reads the HIGHEST
/// watermark any window holds, `why=` names what put a session at
/// attention, and the chrome line quotes its two values and escapes `"` and
/// `\` — with no window, a quiet line; with a front the host holds no view
/// for, an empty `rim=`.
#[test]
fn the_status_tail_and_the_chrome_line_are_the_engines_bytes() {
    let mut d = desk();
    assert_eq!(
        drive::status_tail(&d, 1),
        "hand=- level=quiet story=0 why=-"
    );
    assert_eq!(drive::session_level(&d, 1), Level::Quiet);
    let quiet = "presence rim=none level=quiet band=\"\" sentence=\"\"";
    assert_eq!(drive::chrome_line(&d, None, 80), quiet);
    assert_eq!(
        drive::chrome_line(&d, Some(7), 80),
        "presence rim= level=quiet band=\"\" sentence=\"\""
    );
    drive::refresh_all(&mut d);
    assert_eq!(drive::chrome_line(&d, Some(0), 80), quiet);
    // A driven hand: the holder through the host's codec; the line quoted.
    d.facts.get_mut().insert(
        1,
        TFacts {
            agent_seq: 1,
            hand: Hand::DrivenTurn {
                id: 4,
                holder: Some("a \"b\" \\c".into()),
            },
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(
        drive::status_tail(&d, 1),
        "hand=turn:4:a%20\"b\"%20\\c level=driven story=0 why=-"
    );
    let line = drive::chrome_line(&d, Some(0), 80);
    let v = d.view(0);
    let words = v.words.as_ref().expect("a driven row");
    let (band, rim) = drive::report(v, 80);
    assert_eq!(rim, "drive");
    assert_eq!(band, words.fit(crate::presence::text_cols(80)));
    assert!(band.contains("a \"b\" \\c"), "{band}");
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    assert_eq!(
        line,
        format!(
            "presence rim=drive level=driven band=\"{}\" sentence=\"{}\"",
            esc(&band),
            esc(&words.sentence)
        )
    );
    assert!(line.contains("a \\\"b\\\" \\\\c"), "{line}");
    // The fitted band follows the window's columns (20: the row, with no
    // mail slot since ruling 366, fits 30).
    assert_ne!(drive::report(d.view(0), 20).0, band);
    // A told story on session 2 (window 1's front, window 0's tab): `story`
    // until the human folds it in window 1 — then read, whatever window 0's
    // mark says.
    let seq = drive::tell(&mut d, 2, StoryVerb::Approval, "").expect("held");
    assert_eq!(
        drive::status_tail(&d, 2),
        format!("hand=- level=story story={seq} why=-")
    );
    drive::human_acted(&mut d, 1);
    assert_eq!(d.view(0).watermark(2), 0);
    assert_eq!(drive::session_level(&d, 2), Level::Quiet);
    assert_eq!(
        drive::status_tail(&d, 2),
        format!("hand=- level=quiet story={seq} why=-")
    );
    // An escalation is why a session is at attention.
    d.facts.get_mut().insert(
        2,
        TFacts {
            agent_seq: 2,
            attention: Some("look".into()),
            ..TFacts::default()
        },
    );
    drive::refresh_session(&mut d, 2, false);
    assert_eq!(
        drive::status_tail(&d, 2),
        format!("hand=- level=attention story={seq} why=escalation")
    );
}

/// A session that wants the row: a peer's cooperative lease (not a driver's
/// own turn, so [`Desk::driver_may_type`] does not hold it).
fn peer_leased() -> TFacts {
    TFacts {
        agent_seq: 1,
        hand: Hand::DrivenLease {
            holder: "peer".into(),
        },
        ..TFacts::default()
    }
}

/// Session 1 wants the row (`true`) or not, and the driver hears of it.
fn want(d: &mut TestDesk, row: bool) {
    let facts = if row {
        peer_leased()
    } else {
        TFacts::default()
    };
    d.facts.get_mut().insert(1, facts);
    drive::refresh_session(d, 1, false);
}

/// THE FOLD QUIET (ruling 394, proposed): the incident's schedule — a row born
/// and folded inside a millisecond, about once a minute (measured 2026-09-28:
/// 121x52 -> 51 -> 52 within ~0.8 ms, an alt-screen Claude Code left stale).
/// The birth is ONE re-grid at once; the fold inside the quiet pays none; a
/// want that returns cancels it, still none. The row stands blank meanwhile,
/// and the wire says so. The negative control is a MUTANT, not a branch here:
/// with `fold_due` answering the want's drop itself (no quiet), this test,
/// five more here, the App's `a_quick_show_and_fold_pays_one_regrid_not_two`
/// and both `driver_geometry_conformance` fold tests fail. The closing
/// CONTROL only shows the fold is deferred, not lost.
#[test]
fn a_fold_inside_the_quiet_pays_no_regrid_and_a_returning_want_cancels_it() {
    let mut d = desk();
    let t0 = d.now;
    want(&mut d, true);
    assert_eq!(d.view(0).rows, 1, "the birth is immediate");
    assert_eq!(d.took("regrid 0"), 1);
    d.now = t0 + Duration::from_micros(800);
    want(&mut d, false);
    assert!(d.view(0).words.is_none());
    assert_eq!(d.view(0).rows, 1, "held: the fold waits out the quiet");
    assert_eq!(d.took("regrid 0"), 1, "no re-grid for the fold");
    assert_eq!(d.view(0).fold_since, Some(d.now));
    // What the human sees is a blank row, and the wire reads the same.
    assert_eq!(drive::report(d.view(0), 80).0, "");
    assert_eq!(
        drive::chrome_line(&d, Some(0), 80),
        "presence rim=none level=quiet band=\"\" sentence=\"\""
    );
    assert_ne!(d.view(0).fp(d.now), 0, "the blank row is still painted");
    // The deadline names the fold's due instant, not before.
    let due = d.now + FOLD_QUIET;
    assert_eq!(drive::deadline(&d, d.now), Some(due));
    // Any refresh inside the quiet still holds it.
    d.now = due - Duration::from_millis(1);
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).rows, 1);
    // The want returns: the fold is cancelled, with no re-grid at all.
    want(&mut d, true);
    assert_eq!(d.view(0).fold_since, None);
    assert_eq!(d.view(0).rows, 1);
    assert_eq!(d.took("regrid 0"), 1, "one re-grid for the whole flap");
    assert!(
        drive::deadline(&d, d.now).is_none_or(|x| x != due),
        "the cancelled fold arms nothing"
    );
    // CONTROL: the fold is deferred, not lost — the same show -> fold with
    // the quiet elapsed before the next refresh pays its second re-grid.
    let mut n = desk();
    want(&mut n, true);
    n.now += Duration::from_micros(800);
    want(&mut n, false);
    n.now += FOLD_QUIET;
    drive::refresh_all(&mut n);
    assert_eq!(n.view(0).rows, 0);
    assert_eq!(n.took("regrid 0"), 2, "born and folded: two re-grids");
}

/// A held fold commits EXACTLY ONCE, at its deadline, through the host's
/// timer: `deadline` names it, a tick before it does nothing, the tick at it
/// folds the row (one re-grid, a repaint), and nothing is armed after.
#[test]
fn a_held_fold_commits_once_at_its_deadline_through_the_tick() {
    let mut d = desk();
    want(&mut d, true);
    want(&mut d, false);
    let due = d.now + FOLD_QUIET;
    assert_eq!(drive::deadline(&d, d.now), Some(due));
    assert!(drive::tick(&mut d, due - Duration::from_millis(1)).is_empty());
    assert_eq!(d.view(0).rows, 1);
    assert_eq!(d.took("regrid 0"), 1);
    // The host's clock at the wake is the tick's own.
    d.now = due;
    assert_eq!(drive::tick(&mut d, due), vec![0]);
    assert_eq!(d.view(0).rows, 0);
    assert_eq!(d.view(0).fold_since, None);
    assert_eq!(d.took("regrid 0"), 2, "the fold's one re-grid");
    assert_eq!(drive::deadline(&d, due), None, "a quiet desk again");
    assert!(drive::tick(&mut d, due + FOLD_QUIET).is_empty());
    assert_eq!(d.took("regrid 0"), 2, "committed once");
    // The tick judges the fold on ITS clock, not the host's: a host whose
    // clock lags the wake still folds at the due instant.
    let mut l = desk();
    want(&mut l, true);
    want(&mut l, false);
    let due = l.now + FOLD_QUIET;
    assert_eq!(drive::tick(&mut l, due), vec![0]);
    assert_eq!(l.view(0).rows, 0);
    // A want that came back without a wake is read at the tick and cancels.
    let mut r = desk();
    want(&mut r, true);
    want(&mut r, false);
    r.facts.get_mut().insert(1, peer_leased());
    let due = r.now + FOLD_QUIET;
    let _ = drive::tick(&mut r, due);
    assert_eq!(r.view(0).rows, 1);
    assert_eq!(r.view(0).fold_since, None);
    assert_eq!(r.took("regrid 0"), 1);
}

/// The folds that are NOT held: a real window that can no longer afford the
/// row gets its terminal row back at once (the band's D1 exception), the
/// human's View-menu toggle hides the row at once, and the fold law's
/// keystroke folds a read story at once once the row has stood its quiet.
#[test]
fn the_window_the_menu_and_the_keystroke_fold_at_once() {
    // The window shrank to the row alone: afford 0 < 1 committed.
    let mut d = desk();
    want(&mut d, true);
    d.wins.get_mut(&0).unwrap().rows = 0;
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).rows, 0, "the window's fold is not held");
    assert_eq!(d.took("regrid 0"), 2);
    // A headless window is never out of rows: its fold is held.
    let mut h = desk();
    h.wins.get_mut(&0).unwrap().windowed = false;
    h.wins.get_mut(&0).unwrap().rows = 0;
    want(&mut h, true);
    want(&mut h, false);
    assert_eq!(h.view(0).rows, 1);
    // The band toggle: the human hid the row.
    let mut m = desk();
    want(&mut m, true);
    m.band = false;
    drive::refresh_all(&mut m);
    assert_eq!(m.view(0).rows, 0);
    assert_eq!(m.took("regrid 0"), 2);
    // The fold law: a calm story, read by a keystroke, folds now.
    let mut k = desk();
    let _ = drive::tell(&mut k, 1, StoryVerb::Approval, "");
    assert_eq!(k.view(0).rows, 1);
    k.now += FOLD_QUIET;
    drive::human_acted(&mut k, 0);
    assert_eq!(k.view(0).rows, 0, "the keystroke is the fold");
    assert_eq!(k.took("regrid 0"), 2);
}

/// The holds still hold: a handoff's freeze and a driver's own input keep the
/// count whatever the quiet says, name no wake while they stand (the tick
/// could not act — a spin), and the wake that lifts them commits a fold whose
/// quiet ran out meanwhile; a birth held by the driver is still born at the
/// lift, at once (bb58ea333's row born inside the turn's settle).
#[test]
fn a_freeze_and_a_drivers_input_still_hold_the_fold_and_the_birth() {
    for freeze in [true, false] {
        let mut d = desk();
        want(&mut d, true);
        let hold = |d: &mut TestDesk, on: bool| {
            if freeze {
                d.frozen = on;
            } else {
                d.hand_on = on;
            }
        };
        hold(&mut d, true);
        want(&mut d, false);
        d.now += FOLD_QUIET;
        assert_eq!(drive::deadline(&d, d.now), None, "held: no wake");
        let now = d.now;
        assert!(drive::tick(&mut d, now).is_empty());
        drive::refresh_all(&mut d);
        assert_eq!(d.view(0).rows, 1, "held past the quiet");
        hold(&mut d, false);
        drive::refresh_all(&mut d);
        assert_eq!(d.view(0).rows, 0, "the lift commits the ripe fold");
        assert_eq!(d.took("regrid 0"), 2);
        // A birth under the hold waits for the lift, then is immediate.
        hold(&mut d, true);
        want(&mut d, true);
        assert_eq!(d.view(0).rows, 0);
        hold(&mut d, false);
        drive::refresh_all(&mut d);
        assert_eq!(d.view(0).rows, 1);
        assert_eq!(d.took("regrid 0"), 3);
    }
}

/// THE KEYSTROKE ON A YOUNG ROW (review of ruling 394, P1): a story told while
/// a person types is born at once, and their next key — milliseconds later —
/// must not fold it at once: that is the incident's net-zero pair with the
/// key's echo between. The read folds it when the row has stood the quiet
/// from its BIRTH (not from the key), through the timer.
#[test]
fn a_keystroke_folds_a_young_row_only_once_it_has_stood_the_quiet() {
    let mut d = desk();
    let born = d.now;
    let _ = drive::tell(&mut d, 1, StoryVerb::Approval, "");
    assert_eq!(d.view(0).rows, 1);
    d.now = born + Duration::from_millis(5);
    drive::human_acted(&mut d, 0);
    assert!(d.view(0).words.is_none(), "the story is read");
    assert_eq!(d.view(0).rows, 1, "a 5 ms old row is not folded by a key");
    assert_eq!(d.took("regrid 0"), 1);
    let due = born + FOLD_QUIET;
    assert_eq!(drive::deadline(&d, d.now), Some(due), "from the birth");
    assert_eq!(drive::tick(&mut d, due), vec![0]);
    assert_eq!(d.view(0).rows, 0);
    assert_eq!(d.took("regrid 0"), 2);
}

/// THE KEYSTROKE ON A BLANK ROW (review of ruling 394, C2): a row whose words
/// went away stands blank while its fold waits; a person's key folds it (it
/// has stood its quiet since birth) instead of leaving it out the rest of the
/// quiet — the early return for "nothing to read" skipped it.
#[test]
fn a_keystroke_folds_a_pending_blank_row() {
    let mut d = desk();
    want(&mut d, true);
    d.now += FOLD_QUIET;
    want(&mut d, false);
    assert_eq!(d.view(0).rows, 1, "pending");
    d.now += Duration::from_millis(10);
    drive::human_acted(&mut d, 0);
    assert_eq!(d.view(0).rows, 0, "the keystroke folds the blank row");
    assert_eq!(d.took("regrid 0"), 2);
}

/// A KEYSTROKE UNDER A HOLD (review of ruling 394, P2): the read is sticky, so
/// the lift folds the row at once — not a fresh quiet from the lift.
#[test]
fn a_keystroke_under_a_hold_folds_as_the_hold_lifts() {
    let mut d = desk();
    let _ = drive::tell(&mut d, 1, StoryVerb::Approval, "");
    d.now += FOLD_QUIET;
    d.hand_on = true;
    drive::human_acted(&mut d, 0);
    assert_eq!(d.view(0).rows, 1, "held under the driver's input");
    assert!(d.view(0).fold_read);
    d.now += Duration::from_millis(10);
    d.hand_on = false;
    drive::refresh_all(&mut d);
    assert_eq!(d.view(0).rows, 0, "the lift commits the read at once");
    assert_eq!(d.took("regrid 0"), 2);
}

/// A TAB SWITCH IS A READ (review of ruling 394, P3): a person bringing a
/// quiet tab to the front folds the row the other tab had at once (it stood
/// its quiet), not 1.5 s later under whatever they type into the new tab.
#[test]
fn a_switch_to_a_quiet_tab_folds_at_once() {
    let mut d = desk();
    d.wins.get_mut(&0).unwrap().tabs.push(2);
    want(&mut d, true);
    d.now += FOLD_QUIET;
    d.wins.get_mut(&0).unwrap().front = Some(2);
    drive::refresh_all(&mut d);
    assert!(d.view(0).words.is_none());
    assert_eq!(d.view(0).rows, 0, "the switch is the fold");
    assert_eq!(d.took("regrid 0"), 2);
    // A switch away from a row born milliseconds ago waits for its minimum
    // life, then folds through the timer.
    let mut y = desk();
    y.wins.get_mut(&0).unwrap().tabs.push(2);
    let born = y.now;
    want(&mut y, true);
    y.now += Duration::from_millis(5);
    y.wins.get_mut(&0).unwrap().front = Some(2);
    drive::refresh_all(&mut y);
    assert_eq!(y.view(0).rows, 1);
    assert_eq!(drive::deadline(&y, y.now), Some(born + FOLD_QUIET));
}

/// A DRIVER'S LOOK HOLDS THE FOLD (review of ruling 394, C1): a client handed
/// the screen's generation inside the quiet may name it in an `if-gen=`
/// fence; a fold committed before its act would refuse the act. The quiet
/// restarts at the look — bounded by `FOLD_LOOK_CAP` past the want's drop, so
/// a polling client cannot stand the blank row for ever.
#[test]
fn a_drivers_look_restarts_the_quiet_up_to_its_cap() {
    let mut d = desk();
    want(&mut d, true);
    let drop = d.now;
    want(&mut d, false);
    let look = drop + Duration::from_millis(1400);
    d.looked = Some(look);
    assert_eq!(drive::deadline(&d, look), Some(look + FOLD_QUIET));
    assert!(drive::tick(&mut d, drop + FOLD_QUIET).is_empty());
    assert_eq!(d.view(0).rows, 1, "the look holds the fold past the quiet");
    assert_eq!(drive::tick(&mut d, look + FOLD_QUIET), vec![0]);
    assert_eq!(d.took("regrid 0"), 2);
    // A look older than the drop holds nothing.
    let mut o = desk();
    want(&mut o, true);
    let drop = o.now;
    o.looked = Some(drop - Duration::from_secs(1));
    want(&mut o, false);
    assert_eq!(drive::deadline(&o, drop), Some(drop + FOLD_QUIET));
    // A client polling for ever: the cap folds the row.
    let mut p = desk();
    want(&mut p, true);
    let drop = p.now;
    want(&mut p, false);
    p.looked = Some(drop + FOLD_LOOK_CAP);
    assert_eq!(
        drive::deadline(&p, drop),
        Some(drop + FOLD_LOOK_CAP),
        "capped"
    );
    assert_eq!(drive::tick(&mut p, drop + FOLD_LOOK_CAP), vec![0]);
    assert_eq!(p.view(0).rows, 0);
}

/// THE LEASE RACE AND THE FOLD QUIET, COMPOSED (the integration of
/// `fix/presence-lease-birth-race` and `fix/presence-fold-quiet`): a fold
/// pending while a lease is taken and released back to back. The acquire's
/// refresh reads the lease (its words say "driven") and the release lands
/// before its commit — a want formed under a gone lease decides NOTHING: it
/// neither cancels the pending fold nor restarts its quiet, and pays no
/// re-grid. The release's wake re-senses, and the fold commits on its
/// ORIGINAL clock through the timer: one birth, one fold, the lease's flap
/// free. CONTROL: a lease that was really live when its words committed is a
/// want that came back — it cancels the fold, and the quiet restarts at its
/// release.
#[test]
fn a_fold_pending_through_a_back_to_back_lease_keeps_its_clock_and_folds_once() {
    let mut d = desk();
    let t0 = d.now;
    want(&mut d, true);
    assert_eq!((d.view(0).rows, d.took("regrid 0")), (1, 1), "the birth");
    d.now = t0 + Duration::from_millis(100);
    want(&mut d, false);
    let since = d.now;
    assert_eq!(d.view(0).fold_since, Some(since));
    // `lease acquire` then `lease release`, the release inside the
    // acquire's refresh (between its sense and its commit).
    d.now = since + Duration::from_millis(200);
    d.live.borrow_mut().insert(1, LeaseMark::Typing);
    d.facts.get_mut().insert(1, leased());
    *d.release_after_sense.get_mut() = Some((1, TFacts::default()));
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.view(0).level, Level::Driven, "the words say driven");
    assert_eq!(d.view(0).rows, 1);
    assert_eq!(
        d.view(0).fold_since,
        Some(since),
        "a gone lease's want neither cancels the fold nor restarts its quiet"
    );
    assert_eq!(d.took("regrid 0"), 1);
    // The release's wake: nobody, no words, the fold still on its clock.
    drive::refresh_session(&mut d, 1, false);
    assert!(d.view(0).words.is_none());
    assert_eq!(d.view(0).fold_since, Some(since));
    let due = since + FOLD_QUIET;
    assert_eq!(drive::deadline(&d, d.now), Some(due));
    d.now = due;
    assert_eq!(drive::tick(&mut d, due), vec![0]);
    assert_eq!(d.view(0).rows, 0);
    assert_eq!(
        d.took("regrid 0"),
        2,
        "one birth, one fold: the lease's flap paid nothing"
    );

    // CONTROL: the same lease, its acquire committed while it was live.
    let mut c = desk();
    want(&mut c, true);
    c.now += Duration::from_millis(100);
    want(&mut c, false);
    c.now += Duration::from_millis(200);
    c.live.borrow_mut().insert(1, LeaseMark::Typing);
    c.facts.get_mut().insert(1, leased());
    drive::refresh_session(&mut c, 1, false);
    assert_eq!(c.view(0).fold_since, None, "a want that came back cancels");
    c.now += Duration::from_millis(1);
    c.live.borrow_mut().insert(1, LeaseMark::Free);
    c.facts.get_mut().insert(1, TFacts::default());
    drive::refresh_session(&mut c, 1, false);
    let restarted = c.now;
    assert_eq!(c.view(0).fold_since, Some(restarted), "the quiet restarts");
    assert_eq!(drive::deadline(&c, c.now), Some(restarted + FOLD_QUIET));
    assert_eq!((c.view(0).rows, c.took("regrid 0")), (1, 1));
}

/// A LEASE-MARK HOLD WHILE A FOLD IS DUE (the same integration): the fold's
/// quiet has run out, and the front session's lease moves — its wake not
/// yet run. The words are a gone lease's, so the fold may not commit: the
/// deadline arms no instant for it (the tick would spin on a fold it may not
/// commit), and a refresh of ANOTHER session that re-projects the window (its
/// tab) does not fold. The move's own wake then finds the row wanted again
/// and cancels the fold: no re-grid at all. NEGATIVE CONTROL: with no move,
/// that same refresh folds the due row, and the lease's wake bears it again
/// — the net-zero pair under an alternate-screen program that the fold quiet
/// exists to prevent.
#[test]
fn a_moved_lease_holds_a_due_fold_arms_no_wake_and_its_wake_cancels_the_flap() {
    let settling = || TFacts {
        lease: LeaseMark::Settling,
        ..driven()
    };
    let held = || TFacts {
        hold: Some(HoldFact {
            reason: "review".into(),
            fleet: false,
        }),
        ..TFacts::default()
    };
    let mut d = desk();
    drive::refresh_session(&mut d, 2, false);
    want(&mut d, true);
    want(&mut d, false);
    let due = d.now + FOLD_QUIET;
    d.now = due;
    // Session 1's lease moves (a turn's, past its input): live and in the
    // facts the next sense reads, its wake not yet run.
    d.live.borrow_mut().insert(1, LeaseMark::Settling);
    d.facts.get_mut().insert(1, settling());
    assert!(
        !d.driver_may_type(0),
        "the lease-mark hold alone: no live typing"
    );
    assert_eq!(
        drive::deadline(&d, due),
        None,
        "a fold the moved lease holds arms no instant"
    );
    // Session 2 (window 0's tab) changes: window 0 is re-projected.
    d.facts.get_mut().insert(2, held());
    drive::refresh_session(&mut d, 2, false);
    assert_eq!(d.view(0).rows, 1, "no fold under a moved lease");
    assert_eq!(d.took("regrid 0"), 1);
    // The move's own wake: the row is wanted again, the fold cancelled.
    drive::refresh_session(&mut d, 1, false);
    assert_eq!(d.view(0).level, Level::Driven);
    assert_eq!((d.view(0).rows, d.view(0).fold_since), (1, None));
    assert_eq!(d.took("regrid 0"), 1, "no re-grid at all");

    // NEGATIVE CONTROL: the same schedule with the lease unmoved.
    let mut n = desk();
    drive::refresh_session(&mut n, 2, false);
    want(&mut n, true);
    want(&mut n, false);
    n.now += FOLD_QUIET;
    n.facts.get_mut().insert(2, held());
    drive::refresh_session(&mut n, 2, false);
    assert_eq!(n.view(0).rows, 0, "the due fold commits");
    n.live.borrow_mut().insert(1, LeaseMark::Settling);
    n.facts.get_mut().insert(1, settling());
    drive::refresh_session(&mut n, 1, false);
    assert_eq!(n.view(0).rows, 1);
    assert_eq!(n.took("regrid 0"), 3, "born, folded, born again");
}

/// A BIRTH A LIFTED HOLD LEFT OWED CATCHES UP (review of the integration,
/// 2026-09-29, finding 1). A background tab's lease holds window 0's row
/// ([`Desk::driver_may_type`] reads every session the window shows) while the
/// front session's want forms; the tab's lease is let go before either of its
/// wakes runs, so both read facts identical to its slot and project nothing.
/// No projection is owed the window, and on a hidden (or headless) band no
/// words clock runs: before the fix `deadline` answered `None` and the row
/// never came (measured `rows=0`, on the merge base too). Now the owed birth
/// is due at once, the tick commits it with ONE re-grid, and nothing spins.
#[test]
fn a_birth_a_background_tabs_lease_held_is_committed_once_the_lease_is_gone() {
    // The tab's idle facts, seen once (a slot that has read an agent seq:
    // a repeat is no change, as on a live session).
    let idle = || TFacts {
        agent_seq: 1,
        ..TFacts::default()
    };
    let mut d = desk();
    d.wins.get_mut(&0).unwrap().visible = false;
    d.facts.get_mut().insert(2, idle());
    drive::refresh_session(&mut d, 2, false);
    // Session 2 (window 0's tab) takes a lease; its wake has not run.
    d.live.borrow_mut().insert(2, LeaseMark::Typing);
    d.facts.get_mut().insert(2, leased());
    // Session 1's own want (a peer's lease on it) is refreshed and held.
    want(&mut d, true);
    assert!(d.view(0).words.is_some(), "the row is wanted");
    assert_eq!(d.view(0).rows, 0, "held by the tab's live lease");
    assert_eq!(drive::deadline(&d, d.now), None, "held: no wake");
    // The tab lets go; both of its wakes read what its slot already holds.
    d.live.borrow_mut().insert(2, LeaseMark::Free);
    d.facts.get_mut().insert(2, idle());
    drive::refresh_session(&mut d, 2, false);
    drive::refresh_session(&mut d, 2, false);
    assert_eq!(d.view(0).rows, 0, "no wake projected the window");
    assert_eq!(d.took("regrid 0"), 0);
    // The owed birth is due now, and the tick commits it.
    let now = d.now;
    assert_eq!(drive::deadline(&d, now), Some(now), "the owed birth");
    assert_eq!(drive::tick(&mut d, now), vec![0]);
    assert_eq!((d.view(0).rows, d.took("regrid 0")), (1, 1));
    assert_eq!(drive::deadline(&d, now), None, "nothing spins after it");
    assert!(drive::tick(&mut d, now).is_empty());
    // CONTROL: while the tab still holds, nothing is owed — the birth waits
    // for the lift, as `commit_rows` holds it.
    let mut c = desk();
    c.wins.get_mut(&0).unwrap().visible = false;
    c.facts.get_mut().insert(2, idle());
    drive::refresh_session(&mut c, 2, false);
    c.live.borrow_mut().insert(2, LeaseMark::Typing);
    want(&mut c, true);
    let now = c.now;
    assert_eq!(drive::deadline(&c, now), None);
    assert!(drive::tick(&mut c, now).is_empty());
    assert_eq!((c.view(0).rows, c.took("regrid 0")), (0, 0));
}

/// A KEYSTROKE UNDER A LEASE WHOSE WAKE HAS NOT RUN (review of the
/// integration, 2026-09-29, finding 3): the fold law judges calm on the LIVE
/// lease. A lease just taken is not calm, whether or not its wake has run:
/// the key reads nothing in either order. Before, a key that beat the wake
/// read the stale calm slot — the story read, the watermark moved, the read
/// dropped by the commit's moved-lease hold — where the same key after the
/// wake read nothing at all.
#[test]
fn a_keystroke_under_a_just_taken_lease_reads_nothing_in_either_order() {
    for wake_first in [false, true] {
        let mut d = desk();
        let seq = drive::tell(&mut d, 1, StoryVerb::Approval, "").expect("held");
        assert_eq!(d.view(0).rows, 1);
        d.now += FOLD_QUIET;
        d.live.borrow_mut().insert(1, LeaseMark::Typing);
        d.facts.get_mut().insert(1, leased());
        if wake_first {
            drive::refresh_session(&mut d, 1, false);
        }
        drive::human_acted(&mut d, 0);
        assert_eq!(
            d.view(0).watermark(1),
            0,
            "wake first {wake_first}: a key under a lease reads nothing"
        );
        // The lease ends; the story still stands, and the next key reads it
        // and folds the row at once (it stood its quiet).
        d.live.borrow_mut().insert(1, LeaseMark::Free);
        d.facts.get_mut().insert(1, TFacts::default());
        drive::refresh_session(&mut d, 1, false);
        assert_eq!(d.view(0).level, Level::Story, "wake first {wake_first}");
        assert_eq!(d.view(0).rows, 1);
        d.now += Duration::from_millis(10);
        drive::human_acted(&mut d, 0);
        assert_eq!(d.view(0).watermark(1), seq);
        assert_eq!(d.view(0).rows, 0, "wake first {wake_first}");
    }
}
