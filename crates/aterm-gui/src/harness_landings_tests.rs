// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! SETTINGS ▸ HARNESS COUNTS LANDINGS (design §12.4.1, §12.4.3 item 3, slice 4c).

use super::*;
use crate::claude_lights::ChipGlass;
use aterm_agent::harness::acts::TaskFace;
use aterm_agent::harness::stats::Tally;

/// A session SESSIONS lists, steered under a default of Cruise, under
/// `view`, its glass telling `task`.
fn listed(local_id: u64, view: AutopilotView, task: Option<TaskFace>) -> HarnessSession {
    HarnessSession {
        local_id,
        sid: format!("s-{local_id}"),
        title: format!("work {local_id}"),
        place: format!("in tab {local_id}"),
        program: "Claude Code",
        word: view.mode(),
        view,
        default: Some(Autopilot::Cruise),
        supervised: true,
        glass: ChipGlass {
            task,
            ..ChipGlass::default()
        },
        armed_at_ms: None,
        set_by: None,
        need: None,
        hold: None,
    }
}

/// A state observed, listing `sessions`.
fn listing(sessions: Vec<HarnessSession>) -> HarnessState {
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.posture = HarnessPosture::On(Autopilot::Cruise);
    state.sessions = sessions;
    state
}

/// Seven sessions: a timed Cruise, a Cruise its loop told landed (the chip
/// keeps `∞`), Land with its task unknown and open (`⤓ land`), Land landed
/// (`✓ landed`), Land through a person's own exchange (`⤓ quiet`), and one
/// Off.
fn seven() -> HarnessState {
    let mut timed = listed(1, AutopilotView::Cruise, None);
    timed.armed_at_ms = Some(1_792_177_230_000);
    listing(vec![
        timed,
        listed(2, AutopilotView::Cruise, Some(TaskFace::Landed)),
        listed(3, AutopilotView::Land, None),
        listed(4, AutopilotView::Land, Some(TaskFace::Open)),
        listed(5, AutopilotView::Land, Some(TaskFace::Landed)),
        listed(6, AutopilotView::Land, Some(TaskFace::Theirs)),
        listed(7, AutopilotView::Off, None),
    ])
}

/// Whether words say part as a whole part of a list (, -, : - or ; -separated), so 1.
pub(super) fn says(words: &str, part: &str) -> bool {
    words.split([':', ',', ';']).any(|said| said.trim() == part)
}

/// THE EVERY ROW TELLS LANDING FROM LANDED (§12.4.6): a session under Land
/// is counted by the face its chip wears — `landing` for `⤓ land` (its task
/// open or unknown), `landed` for `✓ landed`, `quiet` for `⤓ quiet` — and a
/// zero part is not said.
#[test]
fn the_every_row_tells_landing_from_landed() {
    let every = seven().every_row().expect("seven sessions");
    assert_eq!(
        (every.landing, every.landed, every.quiet),
        (2, 1, 1),
        "the parts sum to the sessions under Land"
    );
    let line = every.line();
    for part in ["2 landing", "1 landed", "1 quiet"] {
        assert!(says(&line, part), "{part}: {line}");
    }
    let plain = listing(vec![
        listed(1, AutopilotView::Cruise, None),
        listed(2, AutopilotView::Land, Some(TaskFace::Open)),
    ]);
    let line = plain.every_row().expect("two").line();
    assert!(says(&line, "1 landing"), "{line}");
    assert!(
        !line.contains("landed") && !line.contains("quiet"),
        "{line}"
    );
    let landed = listing(vec![
        listed(1, AutopilotView::Land, Some(TaskFace::Landed)),
        listed(2, AutopilotView::Land, Some(TaskFace::Landed)),
    ]);
    let line = landed.every_row().expect("two").line();
    assert!(says(&line, "2 landed"), "{line}");
    assert!(!line.contains("landing"), "{line}");
}

/// THE CRUISE PRESS TELLS LANDING FROM LANDED (§12.4.6): its description
/// splits the sessions under Land it moves as the row does, so it never
/// calls a landed session landing, and, where it moves nothing, says no
/// session is under Land.
#[test]
fn the_cruise_press_tells_landing_from_landed() {
    let state = seven();
    let every = state.every_row().expect("seven sessions");
    let cruise = EveryPress::Mode(Autopilot::Cruise);
    assert_eq!(
        (
            every.cruise_landing,
            every.cruise_landed,
            every.cruise_quiet
        ),
        (2, 1, 1)
    );
    let description = every.description(cruise);
    for part in ["2 landing", "1 landed", "1 quiet"] {
        assert!(says(&description, part), "{part}: {description}");
    }
    assert_eq!(every.moved(cruise), 5);
    assert_eq!(
        every.moved(cruise),
        state.sessions.iter().filter(|s| cruise.moves(s)).count(),
        "the count is the press's own rule"
    );
    assert_eq!(
        every.cruise_landing + every.cruise_landed + every.cruise_quiet + every.cruise_timed,
        every.moved(cruise),
        "each session it moves is said once"
    );
    let mut unsteered = seven();
    unsteered.sessions[4].default = None;
    let every = unsteered.every_row().expect("seven sessions");
    assert_eq!(
        (
            every.cruise_landing,
            every.cruise_landed,
            every.cruise_quiet
        ),
        (2, 0, 1)
    );
    let description = every.description(cruise);
    assert!(says(&description, "2 landing"), "{description}");
    assert!(says(&description, "1 quiet"), "{description}");
    assert!(!description.contains("landed"), "{description}");
    let cruising = listing(vec![
        listed(1, AutopilotView::Cruise, None),
        listed(2, AutopilotView::Cruise, Some(TaskFace::Landed)),
    ]);
    let every = cruising.every_row().expect("two");
    assert_eq!(every.moved(cruise), 0);
    let description = every.description(cruise);
    assert!(description.contains("under Land"), "{description}");
    assert!(!description.contains("landing"), "{description}");
    let landed = listing(vec![
        listed(1, AutopilotView::Land, Some(TaskFace::Landed)),
        listed(2, AutopilotView::Land, Some(TaskFace::Landed)),
    ]);
    let description = landed.every_row().expect("two").description(cruise);
    assert!(says(&description, "2 landed"), "{description}");
    assert!(!description.contains("landing"), "{description}");
}

/// A reading of `today` and `week`.
fn read(today: &HarnessStats, week: &HarnessStats) -> HarnessState {
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.stats.report = Some(std::sync::Arc::new(HarnessStatsReport::new(
        today.clone(),
        week.clone(),
        1_000_000_000,
        Some(0),
        &|_| Some(0),
    )));
    state
}

/// WHAT THE HARNESS DID's rows of `state`.
fn did(state: &HarnessState) -> Vec<(String, String)> {
    state.projection().did_rows
}

/// The words of `label`'s row, if it stands.
fn words_of(rows: &[(String, String)], label: &str) -> Option<String> {
    rows.iter()
        .find(|(l, _)| l == label)
        .map(|(_, w)| w.clone())
}

/// Where `label`'s row stands, if it does.
fn at(rows: &[(String, String)], label: &str) -> Option<usize> {
    rows.iter().position(|(l, _)| l == label)
}

/// THE LANDED ROW STANDS WITH THE LAND PROMPTS (§12.4.6): `Landed  1 today ·
/// 3 in 7 days` right over `Land prompts`, counted from the reading's
/// landings ([`HarnessStats::landed`], never its prompts), wherever the
/// week had a land prompt or a landing — so it stands at `0` while a Land
/// is still landing, and a landing moves a count, never a row — the
/// week's, so a day with none yet keeps it.
#[test]
fn the_landed_row_stands_with_the_land_prompts() {
    let mut today = HarnessStats::new(0);
    today.approved = 4;
    today.land_prompts = 2;
    today.landed = 1;
    today.rows = 7;
    let mut week = HarnessStats::new(0);
    week.approved = 31;
    week.land_prompts = 9;
    week.landed = 3;
    week.rows = 43;
    let rows = did(&read(&today, &week));
    assert_eq!(
        words_of(&rows, LANDED.label).as_deref(),
        Some("1 today \u{b7} 3 in 7 days")
    );
    assert_eq!(
        words_of(&rows, LAND_PROMPTS.label).as_deref(),
        Some("2 today \u{b7} 9 in 7 days"),
        "the prompts stay prompts"
    );
    assert_eq!(
        at(&rows, LANDED.label).map(|i| i + 1),
        at(&rows, LAND_PROMPTS.label),
        "right over the prompts: {rows:?}"
    );
    let (mut prompts_today, mut prompts_week) = (today.clone(), week.clone());
    prompts_today.landed = 0;
    prompts_week.landed = 0;
    assert_eq!(
        words_of(&did(&read(&prompts_today, &prompts_week)), LANDED.label).as_deref(),
        Some("0 today \u{b7} 0 in 7 days")
    );
    let (mut landed_today, mut landed_week) = (today.clone(), week.clone());
    landed_today.land_prompts = 0;
    landed_week.land_prompts = 0;
    assert_eq!(
        words_of(&did(&read(&landed_today, &landed_week)), LANDED.label).as_deref(),
        Some("1 today \u{b7} 3 in 7 days")
    );
    let quiet_day = HarnessStats::new(0);
    assert_eq!(
        words_of(&did(&read(&quiet_day, &week)), LANDED.label).as_deref(),
        Some("0 today \u{b7} 3 in 7 days")
    );
    assert_eq!(
        words_of(&did(&read(&quiet_day, &prompts_week)), LANDED.label).as_deref(),
        Some("0 today \u{b7} 0 in 7 days")
    );
    let (mut none_today, mut none_week) = (today, week);
    none_today.land_prompts = 0;
    none_today.landed = 0;
    none_week.land_prompts = 0;
    none_week.landed = 0;
    let rows = did(&read(&none_today, &none_week));
    assert_eq!(at(&rows, LANDED.label), None, "{rows:?}");
    assert!(at(&rows, LAND_PROMPTS.label).is_some(), "{rows:?}");
}

/// THE DONE-CHECK ROW STANDS UNDER KEPT GOING (§12.4.6): `of them  a done
/// claim checked 4 today · 12 in 7 days`, both windows, right under `Kept
/// going` and labelled as a part of it, read from the reading's acts by
/// rule — Cruise's done check, and the challenges older builds typed at a
/// done report instead, which the week still reads — never an ordinary
/// continuation.
#[test]
fn the_done_check_row_stands_under_kept_going() {
    use aterm_agent::supervise::policy::turn_end::{
        RULE_CONTINUE, RULE_DONE_CHECK, RULE_OPEN_ITEMS, RULE_UNCOMMITTED,
    };
    let rule = |s: &mut HarnessStats, rule: &str, n: u64| {
        s.by_rule.insert(rule.to_string(), n);
    };
    let mut today = HarnessStats::new(0);
    today.continued = 9;
    today.approved = 3;
    today.unproven = 1;
    rule(&mut today, RULE_CONTINUE, 5);
    rule(&mut today, RULE_DONE_CHECK, 4);
    today.rows = 12;
    let mut week = HarnessStats::new(0);
    week.continued = 40;
    week.approved = 20;
    week.unproven = 2;
    rule(&mut week, RULE_CONTINUE, 25);
    rule(&mut week, RULE_DONE_CHECK, 12);
    week.rows = 60;
    let rows = did(&read(&today, &week));
    assert_eq!(
        words_of(&rows, DONE_CHECKED_OF_KEPT_GOING).as_deref(),
        Some("a done claim checked 4 today \u{b7} 12 in 7 days")
    );
    assert_eq!(
        at(&rows, DONE_CHECKED_OF_KEPT_GOING),
        at(&rows, KEPT_GOING.label).map(|i| i + 1),
        "right under Kept going: {rows:?}"
    );
    assert_ne!(
        DONE_CHECKED_OF_KEPT_GOING, UNPROVEN_OF_APPROVED,
        "two parts, two keys"
    );
    assert!(
        at(&rows, UNPROVEN_OF_APPROVED).is_some(),
        "both parts stand: {rows:?}"
    );
    rule(&mut week, RULE_OPEN_ITEMS, 2);
    rule(&mut week, RULE_UNCOMMITTED, 1);
    assert_eq!(
        words_of(&did(&read(&today, &week)), DONE_CHECKED_OF_KEPT_GOING).as_deref(),
        Some("a done claim checked 4 today \u{b7} 15 in 7 days")
    );
    let mut quiet_today = today.clone();
    quiet_today.by_rule.remove(RULE_DONE_CHECK);
    assert_eq!(
        words_of(&did(&read(&quiet_today, &week)), DONE_CHECKED_OF_KEPT_GOING).as_deref(),
        Some("a done claim checked 0 today \u{b7} 15 in 7 days")
    );
    let mut plain = week.clone();
    for gone in [RULE_DONE_CHECK, RULE_OPEN_ITEMS, RULE_UNCOMMITTED] {
        plain.by_rule.remove(gone);
    }
    let rows = did(&read(&quiet_today, &plain));
    assert_eq!(at(&rows, DONE_CHECKED_OF_KEPT_GOING), None, "{rows:?}");
    assert!(
        rows.iter().all(|(_, w)| !w.contains(DONE_CLAIM_CHECKED)),
        "{rows:?}"
    );
}

/// A day's line's parts, in its order (` · `-separated).
fn parts(line: &str) -> Vec<&str> {
    line.split(" \u{b7} ").collect()
}

/// THE TODAY LINE LEADS WITH LANDINGS (§12.4.6): STATUS's today line and a
/// SESSIONS row's day line say a day's landings first, ahead of the acts
/// that led to them (`2 landed today · 31 approved · 4 kept going`); the
/// rest keep their order, and the day line's need still goes before every
/// count.
#[test]
fn the_today_line_leads_with_landings() {
    let mut today = HarnessStats::new(0);
    today.landed = 2;
    today.approved = 31;
    today.continued = 4;
    today.land_prompts = 1;
    today.rows = 38;
    let (hero, _) = read(&today, &HarnessStats::new(0))
        .today_lines()
        .expect("a day");
    assert!(hero.starts_with("2 landed today \u{b7} "), "{hero}");
    let tally = Tally {
        landed: 1,
        land_prompts: 3,
        approved: 31,
        ..Tally::default()
    };
    let line = day_line(&tally, Some(0)).expect("a day");
    assert_eq!(parts(&line)[0], "today: 1 landed", "{line}");
    let needed = Tally {
        escalated: 1,
        ..tally.clone()
    };
    let line = day_line(&needed, Some(0)).expect("a day");
    assert!(parts(&line)[0].starts_with("today: needed you"), "{line}");
    assert_eq!(parts(&line)[1], "1 landed", "{line}");
    let mut shares = HarnessStats::per_session(0);
    shares.landed = 1;
    shares.approved = 31;
    shares.land_prompts = 3;
    shares.rows = 35;
    shares
        .by_session
        .as_mut()
        .expect("per session")
        .insert("s-5".to_string(), tally.clone());
    let mut state = read(&shares, &HarnessStats::new(0));
    state.sessions = vec![listed(5, AutopilotView::Land, Some(TaskFace::Landed))];
    let line = state.projection().sessions[0]
        .day_line
        .clone()
        .expect("its day");
    assert_eq!(parts(&line)[0], "today: 1 landed", "{line}");
    today.landed = 0;
    let (hero, _) = read(&today, &HarnessStats::new(0))
        .today_lines()
        .expect("a day");
    assert!(hero.starts_with("31 approved today"), "{hero}");
    assert!(!hero.contains("landed"), "{hero}");
    let unlanded = Tally { landed: 0, ..tally };
    let line = day_line(&unlanded, Some(0)).expect("a day");
    assert_eq!(parts(&line)[0], "today: 31 approved", "{line}");
    assert!(!line.contains("landed"), "{line}");
}

/// A LANDING MARKS THE OPEN PAGE `Newer` AND OWES THE RE-READ (§12.4.3 item
/// 3, §9.2.1 "The page"): WHAT THE HARNESS DID's `Landed` row, STATUS's
/// today line and a row's day line count landings from the reading, and a
/// landing reaches the window as its task's face, never as an act — so,
/// with Settings ▸ Harness on screen, a landing under Land (an end: from a
/// face the window knew) marks the counts stale and owes a re-read on the
/// glass's one deadline, as an act does.
#[test]
fn a_landing_marks_the_open_page_newer_and_owes_a_reread() {
    let mut page = super::Page::open();
    let sid = page
        .app
        .pool
        .get(0)
        .unwrap()
        .ctx
        .self_id
        .as_str()
        .to_string();
    let tell = |page: &mut super::Page, face: TaskFace| {
        page.app
            .apply_harness_fact(&sid, crate::HarnessFact::Task(Some(face)));
    };
    tell(&mut page, TaskFace::Open);
    tell(&mut page, TaskFace::Landed);
    assert!(!page.app.harness_screen.stats.stale, "Cruise's landed");
    assert_eq!(page.app.harness_screen.reread_at(), None);
    assert!(page.app.set_session_autopilot(
        0,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Chip
    ));
    page.app
        .apply_harness_fact(&sid, crate::HarnessFact::Task(None));
    tell(&mut page, TaskFace::Landed);
    assert!(!page.app.harness_screen.stats.stale, "a restated landing");
    assert_eq!(page.app.harness_screen.reread_at(), None);
    tell(&mut page, TaskFace::Open);
    page.land();
    assert!(
        !page.app.harness_screen.stats.stale,
        "fresh before the landing"
    );
    let revision = page.app.harness_screen.revision;
    tell(&mut page, TaskFace::Landed);
    assert!(
        page.app.harness_screen.stats.stale,
        "the landing marks the counts stale"
    );
    assert!(page.newer(), "the page says Newer");
    assert!(page.app.harness_screen.revision > revision, "republished");
    let owed = page.app.harness_screen.reread_at();
    assert!(owed.is_some(), "and owes the re-read");
    assert_eq!(
        page.app.harness_acts_deadline(),
        owed,
        "on the glass's one deadline"
    );

    let mut page = super::Page::open();
    let sid = page
        .app
        .pool
        .get(0)
        .unwrap()
        .ctx
        .self_id
        .as_str()
        .to_string();
    let ctx = page.app.pool.get(0).unwrap().ctx.clone();
    crate::session_timeline::claim_supervisor_as(
        &ctx,
        &format!("codex-alab@{}", std::process::id()),
        Some(1),
        None,
        Some(crate::self_claim::stand_in::running()),
        crate::metrics::now_us(),
        aterm_slog::Actor::Ctl,
    )
    .expect("a proof takes a free session");
    assert!(page.app.beside_proven_self_claim(0));
    assert!(page.app.set_session_autopilot(
        0,
        Some(Autopilot::Land),
        crate::autopilot::Surface::Chip
    ));
    let tell_self = |page: &mut super::Page, face: TaskFace| {
        ctx.timeline.lock().unwrap().queue_self_task(face);
        page.app
            .apply_harness_fact(&sid, crate::HarnessFact::SelfTask(face));
    };
    tell_self(&mut page, TaskFace::Open);
    page.land();
    tell_self(&mut page, TaskFace::Landed);
    assert_eq!(
        page.app.chip_glass(0).task,
        Some(TaskFace::Landed),
        "CONTROL: it landed"
    );
    assert!(
        !page.app.harness_screen.stats.stale,
        "a self-claim's landing"
    );
    assert_eq!(page.app.harness_screen.reread_at(), None);
}
