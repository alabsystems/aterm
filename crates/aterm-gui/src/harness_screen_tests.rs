// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::WindowId;
use crate::native_app::{ActionInvocation, AppEvent};
use crate::native_ui::ActionId;

/// THE A/B CLOCK charges each stretch to the bucket that was in force when
/// it ran: a phase change closes the stretch under the arm and phase of
/// then, a SWITCH OF ARMS MID-PHASE charges the time so far to the arm that
/// was in force, the open stretches count to the instant read, and a
/// session's end settles its stretch for good. Words that are no busy and no
/// wait (a wall, a survey) count in neither phase. NEGATIVE CONTROLS: an
/// instant before a stretch's start counts nothing, and a session with no
/// stretch open is not charged by its end.
#[test]
fn the_ab_clock_charges_each_stretch_to_the_arm_in_force() {
    let mut book = AbBook::default();
    let busy = Some(AbPhase::Busy);
    let waiting = Some(AbPhase::Waiting);
    // Session 1, on Cruise: busy 0..1000, waiting 1000..4000.
    book.observe(1, busy, AbArm::Cruise, 0);
    book.observe(1, waiting, AbArm::Cruise, 1_000);
    // Its word moves to busy as its arm turns Off (Off written): the wait
    // goes to Cruise, the new stretch opens Off.
    book.observe(1, busy, AbArm::Off, 4_000);
    // Mid-phase, the arm turns to Land (Land written): Off busy 4000..6000.
    book.observe(1, busy, AbArm::Land, 6_000);
    // Session 2, off throughout: waiting 500..2500, then a wall.
    book.observe(2, waiting, AbArm::Off, 500);
    book.observe(2, None, AbArm::Off, 2_500);
    let settled = AbTotals {
        cruise: AbArmTotals {
            busy_ms: 1_000,
            waiting_ms: 3_000,
        },
        off: AbArmTotals {
            busy_ms: 2_000,
            waiting_ms: 2_000,
        },
        ..AbTotals::default()
    };
    assert_eq!(book.totals_at(6_000), settled);
    // Session 1's open busy stretch counts to the instant read, on Land.
    let mut open = settled;
    open.land.busy_ms += 1_500;
    assert_eq!(book.totals_at(7_500), open);
    // The wall counts nothing however long it stands.
    book.observe(2, None, AbArm::Off, 90_000);
    assert_eq!(book.totals_at(7_500), open);
    // Its end settles session 1's stretch; nothing grows after.
    book.retire(1, 8_000);
    let mut ended = settled;
    ended.land.busy_ms += 2_000;
    assert_eq!(book.totals_at(8_000), ended);
    assert_eq!(book.totals_at(1_000_000), ended);
    // An instant before a stretch's start counts nothing (never negative).
    book.observe(3, waiting, AbArm::Unsupervised, 10_000);
    book.observe(3, busy, AbArm::Unsupervised, 9_000);
    assert_eq!(book.totals_at(10_000), ended);
    // A session with no open stretch is not charged by its end.
    book.retire(4, 50_000);
    book.retire(3, 10_000);
    assert_eq!(book.totals_at(1_000_000), ended);
}

/// The words the A/B counts, and its arms — one per Autopilot state: Cruise
/// and Land each their own arm under this host's claim (NEGATIVE CONTROL:
/// Land is never Cruise's, so a landing's end, a wait by design, never
/// counts as the harness's); Off is plain Claude Code — the word Off, the
/// master switch off or no host — with nobody's claim live; everything else
/// is unsupervised.
#[test]
fn the_ab_phases_and_arms_are_the_designs() {
    assert_eq!(AbPhase::of("busy"), Some(AbPhase::Busy));
    for word in ["idle", "prompt", "question"] {
        assert_eq!(AbPhase::of(word), Some(AbPhase::Waiting), "{word}");
    }
    for word in ["wall:limit", "wall:unresponsive", "survey", "unknown", "-"] {
        assert_eq!(AbPhase::of(word), None, "{word}");
    }
    assert_eq!(AbArm::of(AutopilotView::Cruise, true, true), AbArm::Cruise);
    assert_eq!(AbArm::of(AutopilotView::Land, true, true), AbArm::Land);
    for view in [AutopilotView::Cruise, AutopilotView::Land] {
        assert_eq!(
            AbArm::of(view, false, false),
            AbArm::Unsupervised,
            "{view:?}: not attached yet"
        );
        assert_eq!(
            AbArm::of(view, false, true),
            AbArm::Unsupervised,
            "{view:?}: another supervisor's claim"
        );
    }
    for view in [
        AutopilotView::Off,
        AutopilotView::MasterOff,
        AutopilotView::Unhosted,
    ] {
        assert_eq!(AbArm::of(view, false, false), AbArm::Off, "{view:?}");
        assert_eq!(
            AbArm::of(view, false, true),
            AbArm::Unsupervised,
            "{view:?}: a claim still live is no baseline"
        );
        assert_eq!(AbArm::of(view, true, true), AbArm::Unsupervised, "{view:?}");
    }
}

/// The page's words for time and shares: hours and minutes, and a share
/// never rounded into a claim it is not.
#[test]
fn durations_and_shares_read_as_the_page_says_them() {
    assert_eq!(duration_words(0), "0m");
    assert_eq!(duration_words(59_999), "<1m");
    assert_eq!(duration_words(60_000), "1m");
    assert_eq!(duration_words(3_600_000), "1h");
    assert_eq!(duration_words(7_500_000), "2h 5m");
    assert_eq!(share_words(3, 100), "3%");
    assert_eq!(share_words(1, 1_000), "<1%");
    assert_eq!(share_words(999, 1_000), ">99%");
    assert_eq!(share_words(1_000, 1_000), "100%");
    assert_eq!(share_words(0, 0), "0%");
    assert_eq!(
        ab_rows(AbTotals::default()),
        [
            ("Cruise".to_string(), "not measured yet".to_string()),
            ("Land".to_string(), "not measured yet".to_string()),
            ("Off".to_string(), "not measured yet".to_string()),
        ]
    );
}

/// THE COMPARISON IS CRUISE AGAINST OFF: each state's share on its own row,
/// the unsupervised time where there is any, and one sentence setting
/// Cruise beside Off once both have time — saying "this run" once — and,
/// last, what every share counts: a finished agent's wait too.
/// NEGATIVE CONTROLS: a landing's long wait (its end state) moves Land's own
/// row and never the sentence; with Off unmeasured there is no sentence,
/// however much Land and unsupervised time there is; and with no time in
/// any state there is no last row (the quiet card's rows are pinned in
/// `durations_and_shares_read_as_the_page_says_them`).
#[test]
fn the_ab_card_compares_cruise_with_off_and_land_stands_alone() {
    let arm = |busy_ms, waiting_ms| AbArmTotals {
        busy_ms,
        waiting_ms,
    };
    let mut ab = AbTotals {
        cruise: arm(97_000, 3_000),
        land: arm(60_000, 3_540_000),
        unsupervised: arm(30_000, 30_000),
        ..AbTotals::default()
    };
    let rows = ab_rows(ab);
    assert_eq!(
        rows,
        [
            (
                "Cruise".to_string(),
                "waited for input 3% of the time \u{b7} 1m measured".to_string()
            ),
            (
                "Land".to_string(),
                "waited for input 98% of the time \u{b7} 1h measured".to_string()
            ),
            ("Off".to_string(), "not measured yet".to_string()),
            (
                "Unsupervised".to_string(),
                "waited for input 50% of the time \u{b7} 1m measured".to_string()
            ),
            (
                "Finished".to_string(),
                "an agent done with its work still waits for input; each share counts that wait"
                    .to_string()
            ),
        ],
        "no Off time: nothing to compare Cruise with"
    );
    ab.off = arm(59_000, 41_000);
    let sentence = |ab: AbTotals| {
        ab_rows(ab)
            .into_iter()
            .find(|(label, _)| label == "This run")
            .map(|(_, words)| words)
    };
    assert_eq!(
        sentence(ab).as_deref(),
        Some("agents waited for input 3% of the time on Cruise vs 41% with the harness off")
    );
    // A landing that waits on and on moves its own row, not the comparison.
    ab.land.waiting_ms *= 10;
    assert_eq!(
        sentence(ab).as_deref(),
        Some("agents waited for input 3% of the time on Cruise vs 41% with the harness off")
    );
    assert!(
        !ab_rows(ab).iter().any(|(_, words)| words.contains("saved")),
        "never a time saved"
    );
    assert_eq!(
        ab_rows(ab).last().map(|(label, _)| label.as_str()),
        Some("Finished"),
        "what the shares count is the card's last word"
    );
}

/// A reading's time is the local clock's, and says UTC where no zone was
/// read.
#[test]
fn a_reading_says_its_clock_time() {
    // 1970-01-12 13:46:40 UTC.
    let at = 1_000_000_000;
    assert_eq!(read_at_words(at, Some(0)), "at 13:46");
    assert_eq!(read_at_words(at, Some(-5 * 3_600)), "at 08:46");
    assert_eq!(read_at_words(at, Some(11 * 3_600)), "at 00:46");
    assert_eq!(read_at_words(at, None), "at 13:46 UTC");
}

/// The local midnight is the zone's, not UTC's; an unknown zone is UTC's.
/// On a day daylight saving starts or ends, midnight is under the offset in
/// force AT midnight, not now's. NEGATIVE CONTROL: now's offset alone puts
/// that midnight an hour off — the reading the old rule made.
#[test]
fn today_starts_at_the_local_midnight() {
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    let fixed = |offset_s: Option<i64>| move |_: i64| offset_s;
    // 1970-01-03 01:00 UTC.
    let now = 2 * DAY + HOUR;
    assert_eq!(local_midnight_ms(now, fixed(None)), 2 * DAY);
    assert_eq!(local_midnight_ms(now, fixed(Some(0))), 2 * DAY);
    // Two hours west: it is 23:00 on the 2nd there, whose midnight is 02:00
    // UTC on the 2nd.
    assert_eq!(local_midnight_ms(now, fixed(Some(-7_200))), DAY + 2 * HOUR);
    // Two hours east: 03:00 on the 3rd, whose midnight is 22:00 UTC on the 2nd.
    assert_eq!(
        local_midnight_ms(now, fixed(Some(7_200))),
        2 * DAY - 2 * HOUR
    );

    // A zone five hours west that springs forward to four at 02:00 local
    // on day 10 (07:00 UTC), and falls back to five at 02:00 local on day 20
    // (06:00 UTC).
    let spring = 10 * DAY + 7 * HOUR;
    let fall = 20 * DAY + 6 * HOUR;
    let zone = move |unix_s: i64| {
        let ms = unix_s * 1000;
        Some(if (spring..fall).contains(&ms) {
            -4 * 3_600
        } else {
            -5 * 3_600
        })
    };
    // 10:00 local on day 10 (14:00 UTC, already -4): its midnight was under
    // -5, at 05:00 UTC.
    let morning = 10 * DAY + 14 * HOUR;
    assert_eq!(local_midnight_ms(morning, zone), 10 * DAY + 5 * HOUR);
    assert_eq!(
        local_midnight_ms(morning, fixed(Some(-4 * 3_600))),
        10 * DAY + 4 * HOUR,
        "now's offset alone: an hour early"
    );
    // 10:00 local on day 20 (15:00 UTC, back at -5): its midnight was under
    // -4, at 04:00 UTC.
    let morning = 20 * DAY + 15 * HOUR;
    assert_eq!(local_midnight_ms(morning, zone), 20 * DAY + 4 * HOUR);
    assert_eq!(
        local_midnight_ms(morning, fixed(Some(-5 * 3_600))),
        20 * DAY + 5 * HOUR,
        "now's offset alone: an hour late"
    );
    // A day with no shift is the fixed zone's.
    let morning = 15 * DAY + 14 * HOUR;
    assert_eq!(local_midnight_ms(morning, zone), 15 * DAY + 4 * HOUR);
}

/// WHERE CLOCKS SHIFT AT MIDNIGHT ITSELF (America/Havana's rule: forward at
/// 00:00 local, back at 01:00 local to 00:00): on the fall day the date's
/// 00:00 comes twice and today starts at the FIRST; on the spring day 00:00
/// never comes and today starts where the skip lands, 01:00. Each is read
/// from noon and from the hour that repeats. NEGATIVE CONTROL: now's offset
/// alone starts the fall day an hour late — the second 00:00, the reading
/// the single probe made.
#[test]
fn a_midnight_that_comes_twice_starts_the_day_at_the_first() {
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    // Five hours west, four in summer: forward at 00:00 local on day 10
    // (05:00 UTC), back at 01:00 local on day 20 (05:00 UTC).
    let spring = 10 * DAY + 5 * HOUR;
    let fall = 20 * DAY + 5 * HOUR;
    let zone = move |unix_s: i64| {
        let ms = unix_s * 1000;
        Some(if (spring..fall).contains(&ms) {
            -4 * 3_600
        } else {
            -5 * 3_600
        })
    };
    // The fall day: its first 00:00 is under -4, at 04:00 UTC; the second,
    // under -5, at 05:00 UTC.
    let first = 20 * DAY + 4 * HOUR;
    for now in [
        20 * DAY + 17 * HOUR,            // noon, -5
        20 * DAY + 4 * HOUR + 1_800_000, // 00:30, the first time (-4)
        20 * DAY + 5 * HOUR + 1_800_000, // 00:30, the second time (-5)
    ] {
        assert_eq!(local_midnight_ms(now, zone), first, "{now}");
    }
    assert_eq!(
        local_midnight_ms(20 * DAY + 17 * HOUR, |_: i64| Some(-5 * 3_600)),
        first + HOUR,
        "now's offset alone: the second 00:00, an hour late"
    );
    // The spring day: 00:00 is skipped; the day starts at 01:00 (-4), the
    // instant the clocks jumped, 05:00 UTC.
    for now in [10 * DAY + 16 * HOUR, spring + 1_800_000] {
        assert_eq!(local_midnight_ms(now, zone), spring, "{now}");
    }
    // The days around them are the fixed zone's.
    assert_eq!(
        local_midnight_ms(21 * DAY + 17 * HOUR, zone),
        21 * DAY + 5 * HOUR
    );
    assert_eq!(
        local_midnight_ms(11 * DAY + 16 * HOUR, zone),
        11 * DAY + 4 * HOUR
    );
}

/// THE COLLECTOR reads the ledgers under a state tree and creates nothing: a
/// missing tree is an empty reading and stays missing; a ledger's rows land
/// in today's and the week's windows by their time. With no directory to
/// read, it says so rather than reading the working directory.
#[test]
fn the_collector_reads_a_state_tree_and_creates_nothing() {
    use aterm_agent::supervise::approvals::{Outcome, Row};
    let root = aterm_tempfile::TempDir::new().unwrap();
    let state = root.path().join("state");
    let harness = root.path().join("harness");
    let now = 10 * 86_400_000_i64 + 12 * 3_600_000;
    let utc = |_: i64| Some(0);
    let empty = collect_harness_stats(Some(&state), Some(&harness), now, utc).unwrap();
    assert_eq!(
        empty.offset_s,
        Some(0),
        "now's offset, for the reading's time"
    );
    assert!(empty.today.is_empty() && empty.week.is_empty());
    assert_eq!(empty.collected_unix_ms, now);
    assert!(!state.exists() && !harness.exists(), "nothing was made");
    assert_eq!(
        std::fs::read_dir(root.path()).unwrap().count(),
        0,
        "the root holds nothing"
    );

    let drive = state.join("drive");
    std::fs::create_dir_all(&drive).unwrap();
    let approved = |ts: i64| {
        Row {
            rule_id: "read-only@v1",
            outcome: Outcome::Approved,
            command: "ls",
            reason: "",
            box_seq: 1,
        }
        .to_json(ts, Some("s-1"))
    };
    // One this morning, one three days ago, one ten days ago.
    let lines = [
        approved(now - 10 * 86_400_000),
        approved(now - 3 * 86_400_000),
        approved(now - 3_600_000),
    ];
    std::fs::write(drive.join("s-1.jsonl"), lines.join("\n") + "\n").unwrap();
    let read = collect_harness_stats(Some(&state), Some(&harness), now, utc).unwrap();
    assert_eq!((read.today.approved, read.week.approved), (1, 2));
    assert!(!harness.exists(), "the harness state was not made");
    assert_eq!(
        collect_harness_stats(None, Some(&harness), now, |_: i64| None),
        Err("no state directory resolves here".to_string())
    );
}

/// The effective table: every key in the reader's order, each agent's own
/// switch right after the master switch, a written value `yours`, the rest
/// `default` — an agent switched off by its own table among them.
#[test]
fn the_effective_table_is_every_key_against_the_default() {
    let mut cfg = SupervisorConfig::default();
    cfg.set("approve", "safe").unwrap();
    cfg.set("continue_text", "carry on").unwrap();
    cfg.set("codex.enabled", "false").unwrap();
    let rows = config_rows(&cfg);
    let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
    // The master switch, then each supervisable agent's own, then the rest.
    let agents: Vec<String> = aterm_phase::Program::ALL
        .into_iter()
        .filter(|agent| agent.supervisable())
        .map(|agent| format!("{}.enabled", agent.name()))
        .collect();
    let mut want = vec!["enabled"];
    want.extend(agents.iter().map(String::as_str));
    want.extend(&aterm_agent::supervise::config::KEYS[1..]);
    assert_eq!(keys, want);
    let yours: Vec<&str> = rows
        .iter()
        .filter(|row| !row.is_default)
        .map(|row| row.key.as_str())
        .collect();
    assert_eq!(yours, ["codex.enabled", "approve", "continue_text"]);
    let value = |key: &str| &rows.iter().find(|row| row.key == key).unwrap().value;
    assert_eq!(value("approve"), "safe");
    assert_eq!(value("codex.enabled"), "false");
    assert_eq!(value("claude.enabled"), "true");
    // A retired key's withheld rule, under the key the person wrote.
    let retired = crate::app_config::HarnessPolicy::read("[harness]\nrm_breaker = false\n");
    let rows = config_rows(&retired.policy);
    let last = rows.last().unwrap();
    assert_eq!(
        (last.key.as_str(), last.value.as_str(), last.is_default),
        ("rm_breaker", "false", false)
    );
}

/// A host that supervises nothing: what the posture needs to read `On`.
fn idle_host() -> crate::harness_host::HostHandle {
    crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    )
}

/// Session `session` of a test App runs Claude Code (its published
/// program).
fn run_claude(app: &App, session: u64) {
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    let mut tl = ctx.timeline.lock().unwrap();
    tl.note_foreground_group(7);
    tl.set_program(7, Some("claude".into()));
}

/// THE PAGE'S PRESS REACHES THE ONE WRITER: Settings ▸ Harness opened lists
/// the window's Claude Code session, and a press on its Land, dispatched as
/// a person's is, writes the session's own word (`meta set autopilot`'s
/// store) and republishes the page with it. NEGATIVE CONTROL: with no host
/// the press writes nothing.
#[test]
fn a_press_on_a_sessions_land_writes_its_word() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    run_claude(&app, 0);
    let press = |app: &mut App, action: &str| {
        let (_, view) = app.active_native_view(wid).expect("the Settings view");
        app.dispatch_native_view_event(
            wid,
            view,
            AppEvent::Action(ActionInvocation {
                id: ActionId::new(action),
                value: None,
            }),
        )
        .expect("dispatched");
    };

    // No host: the page lists the session as unharnessed, and a press is
    // refused by the page itself.
    assert!(app.open_harness_settings(wid));
    let state = app.harness_state();
    assert_eq!(state.sessions.len(), 1);
    assert_eq!(state.sessions[0].program, "Claude Code");
    assert_eq!(state.sessions[0].view, AutopilotView::Unhosted);
    press(&mut app, "harness/session/0/autopilot/land");
    assert_eq!(app.session_autopilot(0).0, None, "no host: nothing written");

    let host = idle_host();
    app.harness = Some(host.clone());
    app.publish_native_harness_state();
    let revision = app.harness_screen.revision;
    press(&mut app, "harness/session/0/autopilot/land");
    assert_eq!(
        app.session_autopilot(0),
        (Some(Autopilot::Land), AutopilotView::Land, false)
    );
    assert!(
        app.harness_screen.revision > revision,
        "the page was republished with the word"
    );
    let state = app.harness_state();
    assert_eq!(state.sessions[0].word, Some(Autopilot::Land));
    assert_eq!(state.sessions[0].view, AutopilotView::Land);
    host.shutdown_and_join();
}

/// Settings ▸ Harness lists the session of every agent the harness
/// supervises, named by its product: an agy session as `Antigravity CLI`.
/// NEGATIVE CONTROL: a Muse Code session, read but not supervised, is not
/// listed.
#[test]
fn the_page_lists_an_agy_session_and_no_muse_code_one() {
    for (program, listed) in [("agy", Some("Antigravity CLI")), ("muse", None)] {
        let mut app = App::headless_for_test();
        {
            let ctx = app.pool.get(0).unwrap().ctx.clone();
            let mut tl = ctx.timeline.lock().unwrap();
            tl.note_foreground_group(7);
            tl.set_program(7, Some(program.into()));
        }
        assert!(app.open_harness_settings(WindowId(0)));
        let state = app.harness_state();
        assert_eq!(
            state.sessions.first().map(|s| s.program),
            listed,
            "{program}"
        );
    }
}

/// THE LEDGERS' READING LANDS: a reading for the running ticket is kept and
/// published; a stale ticket's, or one nobody asked for, is dropped; a
/// reading with nothing says why.
#[test]
fn a_reading_for_the_running_ticket_is_kept() {
    let mut app = App::headless_for_test();
    let report = |approved| {
        let mut today = HarnessStats::new(0);
        today.approved = approved;
        today.rows = 1;
        Box::new(HarnessStatsReport {
            today: today.clone(),
            week: today,
            collected_unix_ms: 0,
            offset_s: None,
        })
    };
    // Nobody asked: dropped.
    app.finish_harness_stats(app.harness_screen.sequence, Ok(report(1)));
    assert!(app.harness_screen.stats.report.is_none());
    // A reading is running under ticket 2; ticket 1's is stale.
    app.harness_screen.sequence = 2;
    app.harness_screen.stats.collecting = true;
    app.finish_harness_stats(1, Ok(report(1)));
    assert!(app.harness_screen.stats.collecting && app.harness_screen.stats.report.is_none());
    app.finish_harness_stats(2, Ok(report(5)));
    assert!(!app.harness_screen.stats.collecting);
    assert_eq!(
        app.harness_screen
            .stats
            .report
            .as_ref()
            .map(|r| r.today.approved),
        Some(5)
    );
    app.harness_screen.sequence = 3;
    app.harness_screen.stats.collecting = true;
    app.finish_harness_stats(3, Err("no state directory resolves here".to_string()));
    assert_eq!(
        app.harness_screen.stats.unavailable.as_deref(),
        Some("no state directory resolves here")
    );
    assert!(
        app.harness_screen.stats.report.is_some(),
        "the last reading stands"
    );
    // A test App has no event loop: a refresh starts no thread.
    app.start_harness_stats_refresh();
    assert!(!app.harness_screen.stats.collecting);
}

/// The agent word a test publishes for a session, as the sweep's verdict
/// does: whether it moved.
fn publish(ctx: &crate::SessionCtx, word: &'static str) -> bool {
    ctx.timeline.lock().unwrap().publish_agent(
        word,
        None,
        None,
        Some(aterm_phase::Program::Claude),
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    )
}

/// This host's live claim on `ctx`, as its loop takes it over the wire.
fn claim(ctx: &crate::SessionCtx) {
    crate::session_timeline::claim_supervisor(
        ctx,
        &crate::harness_host::holder(),
        None,
        None,
        crate::metrics::now_us(),
    )
    .unwrap();
}

/// Session `session`'s open stretch on the A/B clock: its phase and arm.
fn stretch(app: &App, session: u64) -> Option<(AbPhase, AbArm)> {
    app.harness_screen
        .ab
        .clocks
        .get(&session)
        .map(|open| (open.phase, open.arm))
}

/// THE APP FEEDS THE CLOCK: a session's word moving charges its stretch to
/// the arm in force, and its Autopilot written moves its arm — the stretch
/// before goes to the old arm. Off written while the claim is still live is
/// no baseline yet (the host gives the claim back as it stops the worker).
#[test]
fn the_app_feeds_the_ab_clock_at_each_change() {
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    // The host's claim on it: Cruise and supervised is the Cruise arm.
    claim(&ctx);
    assert_eq!(app.harness_ab_arm(0), AbArm::Cruise);
    assert!(publish(&ctx, "busy"));
    app.note_harness_phase(0);
    assert_eq!(stretch(&app, 0), Some((AbPhase::Busy, AbArm::Cruise)));
    // Land written: its own arm at once, never Cruise's.
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land)));
    assert_eq!(stretch(&app, 0), Some((AbPhase::Busy, AbArm::Land)));
    // Off written: not the baseline while the claim is live.
    assert!(app.set_session_autopilot(0, Some(Autopilot::Off)));
    assert_eq!(stretch(&app, 0), Some((AbPhase::Busy, AbArm::Unsupervised)));
    // A wait published: the busy stretch closes, the wait opens.
    assert!(publish(&ctx, "idle"));
    app.note_harness_phase(0);
    assert_eq!(
        stretch(&app, 0),
        Some((AbPhase::Waiting, AbArm::Unsupervised))
    );
    // The session's end settles it.
    app.retire_harness_ab(0);
    assert!(app.harness_screen.ab.clocks.is_empty());
    host.shutdown_and_join();
}

/// THE ARM READS THE CLAIM ONCE, and reads it right: under Off, another
/// supervisor's live claim is claimed but not this host's (unsupervised, no
/// baseline), and a claim of this host's that has lapsed is nobody's (the
/// baseline). NEGATIVE CONTROL: this host's live claim under Cruise is the
/// Cruise arm, and under Off it is still claimed (unsupervised).
#[test]
fn the_ab_arm_reads_whose_claim_is_live() {
    let mut app = App::headless_for_test();
    run_claude(&app, 0);
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(0).unwrap().ctx.clone();
    let now_us = crate::metrics::now_us();
    claim(&ctx);
    assert_eq!(app.harness_ab_arm(0), AbArm::Cruise);
    assert!(app.set_session_autopilot(0, Some(Autopilot::Off)));
    assert_eq!(app.harness_ab_arm(0), AbArm::Unsupervised, "ours, live");
    // Another supervisor's live claim: claimed, not ours.
    crate::session_timeline::release_supervisor(&ctx, None, now_us);
    crate::session_timeline::claim_supervisor(&ctx, "another-host", None, None, now_us).unwrap();
    assert_eq!(
        app.harness_ab_arm(0),
        AbArm::Unsupervised,
        "another's, live"
    );
    // This host's claim, lapsed: nobody's.
    crate::session_timeline::release_supervisor(&ctx, None, now_us);
    crate::session_timeline::claim_supervisor(
        &ctx,
        &crate::harness_host::holder(),
        None,
        Some(1),
        0,
    )
    .unwrap();
    assert_eq!(app.harness_ab_arm(0), AbArm::Off, "ours, lapsed");
    host.shutdown_and_join();
}

/// A busy Claude Code screen (a transcript row, the spinner, the composer
/// between its rules, the footer), as the sweep's verdict reads one.
fn busy_screen() -> Vec<String> {
    let rule = "\u{2500}".repeat(120);
    let mut rows = vec![String::new(); 24];
    rows[0] = "\u{23fa} Working on it.".into();
    rows[19] = "\u{2736} Deliberating\u{2026} (3s \u{00b7} thinking)".into();
    rows[20] = rule.clone();
    rows[21] = "\u{276f} ".into();
    rows[22] = rule;
    rows[23] = "  \u{23f5}\u{23f5} bypass permissions on \u{00b7} esc to interrupt".into();
    rows
}

/// Paint `rows` as session `session`'s whole screen, 24 by 120.
fn paint(app: &App, session: u64, rows: &[String]) {
    let pooled = app.pool.get(session).expect("pooled");
    let mut term = crate::term_lock(&pooled.term);
    if term.cols() != 120 {
        term.resize(24, 120);
    }
    let mut bytes = String::from("\x1b[H\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        bytes.push_str(&format!("\x1b[{};1H{row}", i + 1));
    }
    term.process(bytes.as_bytes());
}

/// THE PRODUCTION HOOKS FEED THE CLOCK, each through its real path: the
/// STATUS SWEEP's publish of a verdict opens the stretch (no hand-fed
/// `note_harness_phase`); the `Wake::MetaChanged` arm (a claim given back
/// over the wire) moves the arm; and a POLICY adopted from the config
/// watcher moves every session's arm. NEGATIVE CONTROLS: before each hook
/// runs, the clock still holds what the one before left — a claim given
/// back moves nothing until its wake lands, and a policy read moves nothing
/// until it is adopted.
#[test]
fn the_sweep_the_meta_wake_and_the_policy_feed_the_ab_clock() {
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    let session = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(session));
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    claim(&ctx);

    // The sweep: a busy Claude Code screen is read, published and charged.
    assert_eq!(stretch(&app, session), None, "nothing read yet");
    let mut t = Instant::now();
    for _ in 0..3 {
        paint(&app, session, &busy_screen());
        let _ = app.observe_session_statuses(t);
        t += Duration::from_millis(300);
    }
    assert_eq!(ctx.timeline.lock().unwrap().agent().word, "busy");
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Cruise)),
        "the sweep's publish opened the stretch, on Cruise under the claim"
    );

    // The claim given back over the wire: nothing moves until the wake.
    crate::session_timeline::release_supervisor(&ctx, None, crate::metrics::now_us());
    assert_eq!(stretch(&app, session), Some((AbPhase::Busy, AbArm::Cruise)));
    app.on_meta_changed(session);
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Unsupervised)),
        "Cruise with no claim is unsupervised"
    );

    // `[harness] autopilot = "off"` read, then adopted: every arm moves.
    let mut stamps = crate::harness_policy_cell::HarnessStampSource::new();
    let off = stamps.stamp(crate::app_config::HarnessPolicy::read(
        "[harness]\nautopilot = \"off\"\n",
    ));
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Unsupervised))
    );
    app.adopt_harness_policy(off);
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Busy, AbArm::Off)),
        "the default off and no claim: plain Claude Code"
    );
    host.shutdown_and_join();
}

/// A LAPSED CLAIM MOVES THE ARM AS A RELEASED ONE DOES (the review of
/// a295792e4): an Off session behind another supervisor's ttl claim is on
/// the Unsupervised arm; the holder dies without releasing it, and the
/// presence timer's lapse (`Desk::lapse_supervisor`) removes the claim — the
/// waiting stretch is the Off baseline's from that instant, and an open
/// Settings ▸ Harness republishes. NEGATIVE CONTROL: a claim run out that the
/// timer has not lapsed yet moves nothing (the word stands, so nothing else
/// re-reads the arm).
#[test]
fn a_lapsed_claim_moves_the_ab_arm_as_a_released_one_does() {
    use aterm_messages::presence::drive::Desk as _;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(session));
    run_claude(&app, session);
    let host = idle_host();
    app.harness = Some(host.clone());
    let ctx = app.pool.get(session).unwrap().ctx.clone();
    assert!(app.set_session_autopilot(session, Some(Autopilot::Off)));
    // Another supervisor's ttl claim lands (its `Wake::MetaChanged`), and the
    // agent waits at its prompt.
    let holder = "aterm-supervise:42";
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&ctx, holder, None, Some(now + 600_000_000), now)
        .unwrap();
    app.on_meta_changed(session);
    assert!(publish(&ctx, "idle"));
    app.note_harness_phase(session);
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Waiting, AbArm::Unsupervised)),
        "Off behind another's claim"
    );
    assert!(app.open_harness_settings(wid));
    app.publish_native_harness_state();

    // The holder dies: its lease runs out (renewed last with an expiry now
    // past — the instant its ttl ran to), and nothing writes.
    let now = crate::metrics::now_us();
    crate::session_timeline::claim_supervisor(&ctx, holder, None, Some(now - 1), now - 2).unwrap();
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Waiting, AbArm::Unsupervised)),
        "NEGATIVE CONTROL: not lapsed yet"
    );
    let revision = app.harness_screen.revision;
    app.lapse_supervisor(session);
    assert!(
        ctx.meta.lock().unwrap().supervisor.is_none(),
        "the timer lapsed it"
    );
    assert_eq!(
        stretch(&app, session),
        Some((AbPhase::Waiting, AbArm::Off)),
        "the Off baseline's wait from the lapse"
    );
    assert!(
        app.harness_screen.revision > revision,
        "the lapse republished the page"
    );
    host.shutdown_and_join();
}

/// A CLOSED TAB LEAVES THE PAGE AT ONCE: Settings ▸ Harness open, a Claude
/// Code tab closed by hand drops its row in the publish its close makes.
/// NEGATIVE CONTROL: the publish before the close lists it — and the close
/// republishes while the registry still holds the session (a hand-closed
/// tab is never `Exited` there), which listed a ghost row.
#[test]
fn a_closed_tab_leaves_the_harness_page_in_the_publish_its_close_makes() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(session));
    run_claude(&app, session);
    assert!(app.open_harness_settings(wid));
    app.publish_native_harness_state();
    let published = |app: &App| -> Vec<u64> {
        let instance = app
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .expect("the Settings controller");
        let Some(crate::native_app::NativeApp::Settings(settings)) =
            app.native_runtime.app(instance)
        else {
            unreachable!("a Settings instance")
        };
        settings
            .harness_snapshot()
            .sessions
            .iter()
            .map(|s| s.local_id)
            .collect()
    };
    assert_eq!(published(&app), [session], "listed while open");
    let revision = app.harness_screen.revision;
    // Its tab is the window's second: closed as the tab's ✕ closes it.
    assert!(!app.close_tab_at(wid, 1), "not the window's last tab");
    assert!(app.pool.get(session).is_none(), "the session is gone");
    assert!(
        app.harness_screen.revision > revision,
        "the close republished the page"
    );
    assert_eq!(published(&app), Vec::<u64>::new(), "no ghost row");
}

/// THE APP REFUSES A STALE PRESS ITSELF (whatever the page thought): with no
/// host, and under the master switch off, a request for a session's
/// Autopilot writes nothing. NEGATIVE CONTROL: the same request with the
/// harness supervising writes the word.
#[test]
fn a_stale_autopilot_request_is_refused_by_the_app() {
    use crate::native_app::HarnessRequest;
    let mut app = App::headless_for_test();
    let land = HarnessRequest::Autopilot {
        session: 0,
        word: Autopilot::Land,
    };
    assert_eq!(app.harness_posture(), HarnessPosture::Unhosted);
    app.execute_native_harness(land);
    assert_eq!(app.session_autopilot(0).0, None, "no host: nothing written");

    let host = idle_host();
    app.harness = Some(host.clone());
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nenabled = false\n"),
    );
    assert_eq!(app.harness_posture(), HarnessPosture::MasterOff);
    app.execute_native_harness(land);
    assert_eq!(
        app.session_autopilot(0).0,
        None,
        "the master switch off: nothing written"
    );

    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read(""),
    );
    app.execute_native_harness(land);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Land));
    host.shutdown_and_join();
}

/// ...AND BY THE SESSION'S OWN POSTURE: a Claude Code session under
/// `[harness.claude] enabled = false` is steered by nothing, so a request
/// for its Autopilot writes nothing, though the window's harness runs (for
/// Codex). NEGATIVE CONTROL: with only Codex's switch Off the same request
/// writes the word.
#[test]
fn an_autopilot_request_for_a_switched_off_agent_is_refused_by_the_app() {
    use crate::native_app::HarnessRequest;
    let land = HarnessRequest::Autopilot {
        session: 0,
        word: Autopilot::Land,
    };
    let written = |policy: &str| {
        let mut app = App::headless_for_test();
        run_claude(&app, 0);
        let host = idle_host();
        app.harness = Some(host.clone());
        app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
            crate::app_config::HarnessPolicy::read(policy),
        );
        assert!(
            matches!(app.harness_posture(), HarnessPosture::Restricted(..)),
            "{policy}"
        );
        app.execute_native_harness(land);
        let word = app.session_autopilot(0).0;
        let row = app.harness_state().sessions[0].default;
        host.shutdown_and_join();
        (word, row)
    };
    assert_eq!(written("[harness.claude]\nenabled = false\n"), (None, None));
    assert_eq!(
        written("[harness.codex]\nenabled = false\n"),
        (Some(Autopilot::Land), Some(Autopilot::Cruise))
    );
}

/// THE DEFAULT'S BUTTON HANDS A SESSION BACK TO THE DEFAULT: under a default
/// of Cruise, a press of Cruise on a session with no word of its own writes
/// nothing (no `meta-change`); Land gives it its own word; Cruise then
/// clears that word, and the session follows `[harness] autopilot` again —
/// a later default of Off reaches it. NEGATIVE CONTROL: a mode that is not
/// the default is still written as the session's own.
#[test]
fn the_defaults_button_hands_a_session_back_to_the_default() {
    use crate::native_app::HarnessRequest;
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Cruise));
    let press = |app: &mut App, word| {
        app.execute_native_harness(HarnessRequest::Autopilot { session: 0, word });
    };
    let events = |app: &App| {
        app.pool
            .get(0)
            .unwrap()
            .ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change")
            .count()
    };
    press(&mut app, Autopilot::Cruise);
    assert_eq!(app.session_autopilot(0).0, None, "already the default");
    assert_eq!(events(&app), 0, "nothing written");
    press(&mut app, Autopilot::Land);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Land));
    press(&mut app, Autopilot::Cruise);
    assert_eq!(
        app.session_autopilot(0),
        (None, AutopilotView::Cruise, false),
        "back to the default, its own word cleared"
    );
    // The default reaches it again.
    app.harness_policy = crate::harness_policy_cell::HarnessPolicyCell::new(
        crate::app_config::HarnessPolicy::read("[harness]\nautopilot = \"off\"\n"),
    );
    assert_eq!(app.session_autopilot(0).1, AutopilotView::Off);
    // Under that default, Cruise is a mode of its own: written.
    press(&mut app, Autopilot::Cruise);
    assert_eq!(app.session_autopilot(0).0, Some(Autopilot::Cruise));
    host.shutdown_and_join();
}

/// AN OFF PRESS PINS OFF (fail-closed): under `[harness] autopilot = "off"`
/// the Off button writes the word `off` — on a session with no word, and on
/// one whose own word is Land — never clears it, so when the default moves
/// to Cruise (the line deleted) the session stays Off, as the footer's chip
/// leaves it. Before the fix the press handed the session back to the
/// default, and the default's move put the harness's hands back on it.
/// NEGATIVE CONTROL: under a default of Cruise, the Cruise press on that
/// session does hand it back (its word cleared); and the pure rule hands
/// back only a carrying default's own mode.
#[test]
fn an_off_press_pins_off_under_a_default_of_off() {
    use crate::native_app::HarnessRequest;
    let mut app = App::headless_for_test();
    let host = idle_host();
    app.harness = Some(host.clone());
    let policy = |text: &str| {
        crate::harness_policy_cell::HarnessPolicyCell::new(crate::app_config::HarnessPolicy::read(
            text,
        ))
    };
    let press = |app: &mut App, word| {
        app.execute_native_harness(HarnessRequest::Autopilot { session: 0, word });
    };
    app.harness_policy = policy("[harness]\nautopilot = \"off\"\n");
    assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Off));
    press(&mut app, Autopilot::Off);
    assert_eq!(
        app.session_autopilot(0).0,
        Some(Autopilot::Off),
        "no word: Off pinned, not left to the default"
    );
    assert!(app.set_session_autopilot(0, Some(Autopilot::Land)));
    press(&mut app, Autopilot::Off);
    assert_eq!(
        app.session_autopilot(0),
        (Some(Autopilot::Off), AutopilotView::Off, false),
        "its word Land: Off pinned, not cleared"
    );
    // The default moves up; the session the owner turned off stays off.
    app.harness_policy = policy("");
    assert_eq!(app.harness_posture(), HarnessPosture::On(Autopilot::Cruise));
    assert_eq!(app.session_autopilot(0).1, AutopilotView::Off);
    // NEGATIVE CONTROL: a carrying default's own button hands back.
    press(&mut app, Autopilot::Cruise);
    assert_eq!(
        app.session_autopilot(0),
        (None, AutopilotView::Cruise, false)
    );
    for default in Autopilot::ALL {
        assert!(!hands_back(Autopilot::Off, default), "{default:?}");
        for pressed in [Autopilot::Cruise, Autopilot::Land] {
            assert_eq!(
                hands_back(pressed, default),
                pressed == default,
                "{pressed:?} under {default:?}"
            );
        }
    }
    host.shutdown_and_join();
}

/// A FAILED RE-READ IS SAID beside the reading it leaves standing: the
/// counts are the older reading's, and the card says why the newer one is
/// missing. NEGATIVE CONTROL: no failure, no such row.
#[test]
fn a_failed_reread_beside_a_reading_is_said() {
    let mut today = HarnessStats::new(0);
    today.approved = 2;
    today.rows = 2;
    let mut state = HarnessState::unobserved();
    state.observed = true;
    state.stats.report = Some(Box::new(HarnessStatsReport {
        today: today.clone(),
        week: today,
        collected_unix_ms: 1_000_000_000,
        offset_s: Some(0),
    }));
    let failed = |state: &HarnessState| {
        state
            .projection()
            .did_rows
            .into_iter()
            .find(|(label, _)| label == "Last check")
    };
    assert_eq!(failed(&state), None);
    state.stats.unavailable = Some("the ledgers' reader failed".to_string());
    let rows = state.projection().did_rows;
    assert_eq!(
        rows.iter()
            .find(|(label, _)| label == "Approved")
            .map(|(_, words)| words.as_str()),
        Some("2 today \u{b7} 2 in 7 days"),
        "the older reading stands"
    );
    assert_eq!(
        failed(&state),
        Some((
            "Last check".to_string(),
            "failed: the ledgers' reader failed".to_string()
        ))
    );
}

/// A SESSION IS NEVER NAMELESS on Settings ▸ Harness: its tab's resolved
/// name, else the program — a blank one skipped, since the row's controls
/// are spoken by the name. CONTROL: a name wins over the program.
#[test]
fn a_session_with_a_blank_title_is_named_by_its_program() {
    assert_eq!(session_title(Some("deploy"), "Claude Code"), "deploy");
    assert_eq!(session_title(Some("  "), "Claude Code"), "Claude Code");
    assert_eq!(session_title(None, "Codex"), "Codex");
}

/// A TITLE THAT IS THE AGENT'S NAME IS NOT FOLLOWED BY IT: "Claude Code",
/// and the agent's own live titles behind a status glyph, name the program;
/// a title that only mentions it does not.
#[test]
fn a_title_that_is_the_agents_name_names_its_program() {
    for title in ["Claude Code", "\u{2733} Claude Code", "claude code "] {
        assert!(names_program(title, "Claude Code"), "{title:?}");
    }
    assert!(names_program("\u{280b} Codex", "Codex"));
    for title in [
        "Fix login bug",
        "claude-docs",
        "\u{2733} Claude Code review",
    ] {
        assert!(!names_program(title, "Claude Code"), "{title:?}");
    }
}

/// THE SESSIONS ROW SAYS EACH WORD ONCE: a session titled by its agent's
/// name (no title of its own, or the agent's "✳ Claude Code") is not called
/// "Claude Code" again in its detail. CONTROL: a title of its own keeps the
/// agent named.
#[test]
fn a_sessions_row_names_its_agent_once() {
    let detail = |title: &str| {
        let mut state = HarnessState::unobserved();
        state.observed = true;
        state.posture = HarnessPosture::On(Autopilot::Cruise);
        state.sessions = vec![HarnessSession {
            local_id: 0,
            title: title.to_string(),
            place: "in tab 1".to_string(),
            program: "Claude Code",
            word: None,
            view: AutopilotView::Cruise,
            glass: crate::claude_lights::ChipGlass::default(),
            default: Some(Autopilot::Cruise),
            supervised: true,
        }];
        state.projection().sessions[0].detail.clone()
    };
    for title in ["Claude Code", "\u{2733} Claude Code"] {
        assert_eq!(
            detail(title),
            "Tab 1 \u{b7} supervised \u{b7} autopilot cruise \u{b7} the default",
            "{title:?}"
        );
    }
    assert_eq!(
        detail("Fix login bug"),
        "Tab 1 \u{b7} Claude Code \u{b7} supervised \u{b7} autopilot cruise \u{b7} the default"
    );
}

/// THE ROW'S NAME IS THE TAB'S, read live: a session whose terminal titled
/// itself (OSC 2) is named by that title, though the store's copy of the
/// title — written only as the front tab's window title is published — is
/// empty in a headless App; a title the person set wins over it. NEGATIVE
/// CONTROL: before the terminal titles itself the row falls back to the
/// agent's name.
#[test]
fn a_sessions_row_is_named_as_its_tab_is() {
    let app = App::headless_for_test();
    run_claude(&app, 0);
    let title = |app: &App| app.harness_state().sessions[0].title.clone();
    assert_eq!(title(&app), "Claude Code", "untitled: its agent's name");
    {
        let session = app.pool.get(0).unwrap();
        let mut term = session.term.lock().unwrap();
        term.process(b"\x1b]2;Fix login bug\x07");
    }
    assert_eq!(title(&app), "Fix login bug");
    {
        let session = app.pool.get(0).unwrap();
        session
            .ctx
            .meta
            .lock()
            .unwrap()
            .set("title", Some("deploy".to_string()));
    }
    assert_eq!(title(&app), "deploy");
}

/// THE SESSIONS STATE LINE is formatted from the row's glass for every state
/// (design §9.3.4), as the chip's face reads it: the timer under Cruise (its
/// weekday on another day), the task's face and the goal hold under Land,
/// else the trail's newest act — clock times, never an age. NEGATIVE
/// CONTROLS: the default glass says nothing under any view (v1's row), and a
/// face or a timer outside its own mode falls through to the trail.
#[test]
fn the_sessions_state_line_is_formatted_from_the_glass_for_every_state() {
    use crate::claude_lights::{ChipGlass, Clock, LandAtGlass, Weekday};
    use aterm_agent::harness::acts::{Act, TaskFace};
    use aterm_agent::supervise::policy::turn_end::LAND_NUDGES;
    let row = |view: AutopilotView, glass: ChipGlass| HarnessSessionRow {
        local_id: 7,
        title: "fix the build".to_string(),
        detail: String::new(),
        mode: view.mode(),
        default: view.mode(),
        view,
        glass,
    };
    let clock = |h: u16, m: u16| Clock::of_minute(h * 60 + m);
    let timer = |day| ChipGlass {
        land_at: Some(LandAtGlass {
            clock: clock(18, 0),
            other_day: day,
        }),
        ..ChipGlass::default()
    };
    let told = |face| ChipGlass {
        task: Some(face),
        task_at: Some(clock(17, 42)),
        ..ChipGlass::default()
    };
    let last = Some((Act::Approved, clock(14, 5)));
    let cases = [
        (
            AutopilotView::Cruise,
            timer(None),
            "lands at 18:00".to_string(),
        ),
        (
            AutopilotView::Cruise,
            timer(Some(Weekday::Sat)),
            "lands Sat 18:00".to_string(),
        ),
        (
            AutopilotView::Land,
            told(TaskFace::Landed),
            "landed 17:42".to_string(),
        ),
        (
            AutopilotView::Land,
            told(TaskFace::Done),
            "done 17:42".to_string(),
        ),
        (
            AutopilotView::Land,
            told(TaskFace::Spent),
            format!("quiet: no done reply after {LAND_NUDGES} nudges"),
        ),
        (
            AutopilotView::Land,
            told(TaskFace::Theirs),
            "quiet: a person's own exchange".to_string(),
        ),
        (
            AutopilotView::Land,
            ChipGlass {
                held: true,
                ..ChipGlass::default()
            },
            "held by /goal".to_string(),
        ),
        (
            AutopilotView::Cruise,
            ChipGlass {
                last,
                ..ChipGlass::default()
            },
            "last: \u{2713} approved 14:05".to_string(),
        ),
        (
            AutopilotView::Land,
            ChipGlass {
                last,
                ..told(TaskFace::Open)
            },
            "last: \u{2713} approved 14:05".to_string(),
        ),
        (
            AutopilotView::Off,
            ChipGlass {
                last,
                ..told(TaskFace::Landed)
            },
            "last: \u{2713} approved 14:05".to_string(),
        ),
    ];
    for (view, glass, line) in cases {
        assert_eq!(
            row(view, glass).state_line(),
            Some(line),
            "{view:?} {glass:?}"
        );
    }
    for view in AutopilotView::ALL {
        assert_eq!(
            row(view, ChipGlass::default()).state_line(),
            None,
            "{view:?}"
        );
    }
    assert_eq!(row(AutopilotView::Land, timer(None)).state_line(), None);
    // A face told where the zone could not be read names no time.
    let unplaced = ChipGlass {
        task: Some(TaskFace::Done),
        ..ChipGlass::default()
    };
    assert_eq!(
        row(AutopilotView::Land, unplaced).state_line().as_deref(),
        Some("done")
    );
}
