// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wall handling tests under forever mode.

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::policy::turn_end::Seat;
use aterm_phase::WallKind;
use aterm_phase::prompt::fixtures::screen;

struct P {
    rows: Vec<String>,
    program: Option<&'static str>,
    at: i64,
    worked: Option<Duration>,
    started: Option<i64>,
}

fn p(rows: &[String], program: Option<&'static str>, at: i64, worked: Option<u64>) -> P {
    P {
        rows: rows.to_vec(),
        program,
        at,
        worked: worked.map(Duration::from_secs),
        started: None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Said {
    act: String,
    track: bool,
    holds: bool,
}

#[derive(Debug)]
struct Relauncher {
    ours: bool,
}

impl IdleHost for Relauncher {
    fn wants(&self) -> bool {
        false
    }
    fn at_idle(&self) -> Option<HostStep> {
        None
    }
    fn owns_turn_end(&self) -> bool {
        false
    }
    fn relaunch_owed(&self) -> bool {
        self.ours
    }
}

fn run_points(tag: &str, autopilot: Autopilot, points: &[P]) -> Vec<Said> {
    run_points_as(tag, autopilot, points, None)
}

fn run_points_as(
    tag: &str,
    autopilot: Autopilot,
    points: &[P],
    relauncher: Option<bool>,
) -> Vec<Said> {
    let (dir, ledger) = ledger_at(tag);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, Some("@s-1".to_owned()));
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.clone()));
    let mut notes = Vec::new();
    s.ledger = approvals::Ledger::open(Some(&ledger), Some("@s-1"), &mut notes);
    let mut opts = hosted(30);
    opts.policy.autopilot = autopilot;
    opts.idle_host = relauncher.map(|ours| Arc::new(Relauncher { ours }) as Arc<dyn IdleHost>);
    let mut out = Vec::new();
    let mut clock = Clock::start(&mut s);
    for (seq, p) in (1..).zip(points) {
        clock.to(&mut s, p.at);
        if let Some(started) = p.started {
            let at = s.instant_of(TEST_NOW + started);
            s.turn_end.agent_started(at);
            s.turn_end.agent_started_unix(TEST_NOW + started);
        }
        s.program = p.program.map(str::to_owned);
        let point = point_at(&s, p.rows.clone(), seq);
        let act = match s.turn_end_at_point(&point, p.worked, &opts) {
            Ok(TurnEndAction::WaitUntil { .. }) => "wait".to_owned(),
            Ok(TurnEndAction::Nothing) => "nothing".to_owned(),
            Ok(a) => format!("act {}", a.rule_id().unwrap_or("-")),
            Err(e) => format!("fail {e:?}"),
        };
        let track = [WallKind::UsageSession, WallKind::Spend]
            .iter()
            .any(|k| s.turn_end.wall_attempts(*k).is_some());
        let holds = s.policy_limit_holds();
        out.push(Said { act, track, holds });
    }
    let _ = std::fs::remove_dir_all(dir);
    out
}

struct Clock {
    at: i64,
}

impl Clock {
    fn start<C: Ctl>(s: &mut Session<'_, C>) -> Self {
        s.set_clock(TEST_NOW, PDT);
        s.set_mono_standing();
        s.set_clock_running();
        Self { at: 0 }
    }

    fn to<C: Ctl>(&mut self, s: &mut Session<'_, C>, at: i64) {
        assert!(at >= self.at, "the run's clock runs forward");
        s.test_clock().skip(Duration::from_secs(
            u64::try_from(at - self.at).unwrap_or(0),
        ));
        self.at = at;
        assert_eq!(s.now_unix(), TEST_NOW + at, "the wall clock moved with it");
    }
}

fn shell() -> Vec<String> {
    rows(&["$ muse resume 0000", ""])
}

fn acts(said: &[Said]) -> Vec<&str> {
    said.iter().map(|s| s.act.as_str()).collect()
}

#[test]
fn a_relaunch_under_a_screen_alone_agents_limit_types_nothing_into_it() {
    use aterm_phase::agy::fixtures::lab as agy;
    use aterm_phase::muse::fixtures as muse;
    let muse_wall = screen(muse::USAGE_WALL);
    let resumed = screen(muse::RESUMED_AFTER_CRASH);
    let agy_wall = screen(agy::ERROR_429);
    let replayed = screen(agy::END_OF_TURN);
    let fresh = screen(agy::IDLE);
    let cases = [
        (
            "muse",
            &muse_wall,
            &resumed,
            6 * 3600,
            "act usage-resume@v1",
        ),
        (
            "agy replayed",
            &agy_wall,
            &replayed,
            4000,
            "act usage-resume@v1",
        ),
        ("agy fresh", &agy_wall, &fresh, 4000, "nothing"),
    ];
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        for (name, wall, after, past, last) in cases {
            let tag = format!("relaunch-{name}-{autopilot:?}");
            let said = run_points_as(
                &tag,
                autopilot,
                &[
                    p(wall, None, 0, Some(60)),
                    p(&shell(), Some("zsh"), 30, None),
                    p(after, None, 60, Some(4)),
                    p(after, None, 300, None),
                    p(after, None, past, None),
                ],
                Some(true),
            );
            let held = if name == "agy fresh" {
                "nothing"
            } else {
                "wait"
            };
            assert_eq!(acts(&said)[2..], [held, held, last], "{tag}: {said:#?}");
            assert!(
                said[2..4].iter().all(|s| s.track && s.holds),
                "{tag}: the limit stands through the start-up: {said:#?}"
            );

            let tag = format!("{tag}-control");
            let said = run_points_as(
                &tag,
                autopilot,
                &[
                    p(wall, None, 0, Some(60)),
                    p(after, None, 60, Some(4)),
                    p(after, None, 300, None),
                ],
                Some(true),
            );
            assert!(!said[1].track, "{tag}: work ends the limit: {said:#?}");
            assert!(
                !acts(&said).contains(&"act usage-resume@v1"),
                "{tag}: {said:#?}"
            );
            if name != "agy fresh" {
                assert!(
                    said[2].act.starts_with("act ") || said[1].act.starts_with("act "),
                    "{tag}: the continue policy's act: {said:#?}"
                );
            }
        }
    }
}

#[test]
fn a_relaunch_the_host_names_is_the_start_up_with_no_shell_between() {
    use aterm_phase::muse::fixtures as muse;
    let wall = screen(muse::USAGE_WALL);
    let resumed = screen(muse::RESUMED_AFTER_CRASH);
    for (started, relaunch) in [(50, true), (-600, false)] {
        let tag = format!("host-start-{started}");
        let mut at_resume = p(&resumed, None, 60, Some(4));
        at_resume.started = Some(started);
        let said = run_points_as(
            &tag,
            Autopilot::Cruise,
            &[
                p(&wall, None, 0, Some(60)),
                at_resume,
                p(&resumed, None, 300, None),
                p(&resumed, None, 6 * 3600, None),
            ],
            Some(true),
        );
        if relaunch {
            assert_eq!(
                acts(&said)[1..],
                ["wait", "wait", "act usage-resume@v1"],
                "{tag}: {said:#?}"
            );
        } else {
            assert!(!said[1].track, "{tag}: {said:#?}");
            assert!(
                !acts(&said).contains(&"act usage-resume@v1"),
                "{tag}: {said:#?}"
            );
        }
    }
}

#[test]
fn a_relaunched_agents_limit_holds_the_hosts_carry_on_to_its_reset() {
    use aterm_phase::muse::fixtures as muse;
    let wall = screen(muse::USAGE_WALL);
    let resumed = screen(muse::IDLE_RELAUNCHED);
    let step = |limit: bool, at: i64| -> (bool, Vec<String>) {
        let host = AccountHost::scripted(&[None], &[]);
        *host.step.lock().unwrap() = Some("continued");
        host.relaunch_owed.store(true, Ordering::SeqCst);
        let opts = account_opts(&host);
        let mut m = muse_mock(vec![resumed.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        let mut clock = Clock::start(&mut s);
        if limit {
            let wall_point = point_at(&s, wall.clone(), 1);
            let _ = s.turn_end_at_point(&wall_point, Some(Duration::from_secs(60)), &opts);
            clock.to(&mut s, 30);
            s.program = Some("zsh".to_owned());
            let shell_point = point_at(&s, shell(), 2);
            let _ = s.turn_end_at_point(&shell_point, None, &opts);
        }
        clock.to(&mut s, 60);
        s.program = Some("muse".to_owned());
        let point = point_at(&s, resumed.clone(), 3);
        let _ = s.turn_end_at_point(&point, Some(Duration::from_secs(4)), &opts);
        clock.to(&mut s, at);
        let mut point = point_at(&s, resumed.clone(), 4);
        point.screen.cursor_row = aterm_phase::muse::composer(&resumed).expect("its caret");
        host.carry.store(true, Ordering::SeqCst);
        let stepped = s
            .host_steps_here(&point, &opts, &mut false, &mut Alone)
            .expect("the host's step");
        (stepped, host.said("carry-on"))
    };
    let (stepped, said) = step(true, 300);
    assert!(!stepped, "withheld before the reset");
    assert!(said.is_empty(), "{said:?}");
    let (stepped, said) = step(true, 15_172 + 3600 + 120);
    assert!(stepped, "offered past the reset and its grace");
    assert_eq!(said.len(), 1, "{said:?}");
    let (stepped, said) = step(false, 300);
    assert!(stepped, "the control: no limit, the carry-on at once");
    assert_eq!(said.len(), 1, "{said:?}");
}

#[test]
fn gemini_clis_ladder_carries_across_a_relaunch_under_its_limit() {
    use super::super::approval_loop::RetryDue;
    use aterm_phase::gemini::fixtures as g;
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..hosted(60)
    };
    let dialog = screen(g::LIMIT_DIALOG);
    let banner = screen(g::IDLE);
    for relaunch in [true, false] {
        let mut m = gemini_mock(vec![dialog.clone()]);
        let mut s = session(&mut m, None);
        s.program = Some("node".to_owned());
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_mono_standing();
        let rung = TEST_NOW + 1800;
        assert_eq!(
            s.limit_retry_due(&dialog, &opts),
            RetryDue::Hold {
                until_unix: rung,
                reset: None
            }
        );
        s.set_clock(rung, PDT);
        assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Due);
        s.limit_retry_pressed();
        s.set_clock(rung + 10, PDT);
        let stopped = point_at(&s, screen(g::LIMIT_STOPPED), 2);
        let first = s
            .turn_end_at_point(&stopped, None, &opts)
            .expect("a decision");
        assert!(
            matches!(first, TurnEndAction::WaitUntil { .. }),
            "{first:?}"
        );
        assert!(s.turn_end.has_limit_track(), "PRECONDITION");
        if relaunch {
            s.set_clock(rung + 20, PDT);
            s.program = Some("zsh".to_owned());
            let shell_point = point_at(&s, shell(), 3);
            let _ = s.turn_end_at_point(&shell_point, None, &opts);
        }
        s.set_clock(rung + 40, PDT);
        s.program = Some("node".to_owned());
        let banner_point = point_at(&s, banner.clone(), 4);
        let _ = s.turn_end_at_point(&banner_point, Some(Duration::from_secs(4)), &opts);
        assert_eq!(s.limit_ladder_kept(), relaunch, "relaunch={relaunch}");
        let resumed_at = rung + 2 * 3600;
        s.set_clock(resumed_at, PDT);
        s.limit_resume_typed(&banner);
        s.set_clock(resumed_at + 5, PDT);
        let due = s.limit_retry_due(&dialog, &opts);
        let want = if relaunch {
            resumed_at + 6 * 3600
        } else {
            resumed_at + 5 + 1800
        };
        assert_eq!(
            due,
            RetryDue::Hold {
                until_unix: want,
                reset: None
            },
            "relaunch={relaunch}"
        );
    }
}

#[test]
fn cruise_waits_out_each_harness_wall_and_goes_on_after_it() {
    use aterm_phase::agy::fixtures::lab as agy;
    use aterm_phase::muse::fixtures as muse;
    let claude_wall = {
        let mut r = rows(&[
            "⏺ Running the suite again.",
            "  ⎿  You've hit your session limit · resets 1pm (America/Los_Angeles)",
            "",
            "✻ Worked for 3m 2s · done 8:52 AM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    };
    let cases = [
        (
            "claude",
            Some("claude"),
            claude_wall,
            idle_screen(),
            5 * 3600,
        ),
        (
            "muse",
            None,
            screen(muse::USAGE_WALL),
            screen(muse::END_OF_TURN_1_4_10),
            6 * 3600,
        ),
        (
            "agy",
            None,
            screen(agy::ERROR_429),
            screen(agy::END_OF_TURN),
            4000,
        ),
    ];
    for (name, program, wall, ended, past) in cases {
        let tag = format!("cruise-{name}");
        let said = run_points(
            &tag,
            Autopilot::Cruise,
            &[
                p(&wall, program, 0, Some(60)),
                p(&wall, program, 600, None),
                p(&wall, program, past, None),
                p(&ended, program, past + 300, Some(240)),
            ],
        );
        assert_eq!(
            acts(&said)[..3],
            ["wait", "wait", "act usage-resume@v1"],
            "{tag}: {said:#?}"
        );
        assert!(
            said[3].act.starts_with("act ") && said[3].act != "act usage-resume@v1",
            "{tag}: Cruise goes on after the wall: {said:#?}"
        );
        assert!(!said[3].track, "{tag}: {said:#?}");

        let tag = format!("{tag}-control");
        let said = run_points(&tag, Autopilot::Cruise, &[p(&ended, program, 0, Some(240))]);
        assert!(said[0].act.starts_with("act "), "{tag}: {said:#?}");
    }
}

fn drawn(rows: &[String], from: &str, to: &str) -> Vec<String> {
    let out: Vec<String> = rows.iter().map(|r| r.replacen(from, to, 1)).collect();
    assert_ne!(out, rows, "PRECONDITION: {from:?} is drawn");
    out
}

#[test]
fn a_limit_binds_its_own_seat_and_another_goes_at_once() {
    use aterm_phase::agy::fixtures::lab as agy;
    use aterm_phase::muse::fixtures as muse;
    let wall = screen(agy::ERROR_429);
    let ended = screen(agy::END_OF_TURN);
    let flash = drawn(&ended, "Gemini 3.1 Pro · low", "Gemini 3.5 Flash · low");
    let account = "user@example.com (Google AI Pro)";
    let other_login = drawn(&ended, "Gemini API key", account);
    let account_wall = drawn(&wall, "Gemini API key", account);
    for (name, after, moved) in [("model moved", &flash, true), ("model kept", &ended, false)] {
        let tag = format!("seat-{name}");
        let said = run_points(
            &tag,
            Autopilot::Land,
            &[p(&wall, None, 0, Some(60)), p(after, None, 300, None)],
        );
        let want = if moved { "act usage-resume@v1" } else { "wait" };
        assert_eq!(said[1].act, want, "{tag}: {said:#?}");
        assert!(said[0].holds, "{tag}: the limit holds the host: {said:#?}");
        assert_eq!(said[1].holds, !moved, "{tag}: {said:#?}");
    }
    for (name, at_wall, after, relauncher, moved) in [
        ("login moved", &wall, &other_login, None, true),
        ("account kept", &account_wall, &other_login, None, false),
        (
            "key kept, the host's relaunch",
            &wall,
            &ended,
            Some(true),
            false,
        ),
    ] {
        let tag = format!("seat-{name}");
        let said = run_points_as(
            &tag,
            Autopilot::Land,
            &[
                p(at_wall, None, 0, Some(60)),
                p(&shell(), Some("zsh"), 30, None),
                p(after, None, 60, Some(4)),
            ],
            relauncher,
        );
        let want = if moved { "act usage-resume@v1" } else { "wait" };
        assert_eq!(said[2].act, want, "{tag}: {said:#?}");
        assert_eq!(said[2].holds, !moved, "{tag}: {said:#?}");
    }
    let muse_wall = screen(muse::USAGE_WALL);
    let muse_ended = screen(muse::END_OF_TURN_1_4_10);
    let model = aterm_phase::muse::launch_card(&muse_ended)
        .map(|c| c.model)
        .expect("PRECONDITION: Muse Code's footer names its model");
    let muse_moved = drawn(&muse_ended, &model, "lab-other");
    let said = run_points(
        "seat-muse",
        Autopilot::Land,
        &[
            p(&muse_wall, None, 0, Some(60)),
            p(&muse_moved, None, 300, None),
        ],
    );
    assert_eq!(said[1].act, "wait", "{said:#?}");
    assert!(said[1].holds, "{said:#?}");
}

#[test]
fn a_relaunch_nobody_here_made_on_a_seat_nothing_vouches_for_is_tried_at_once() {
    use aterm_phase::agy::fixtures::lab as agy;
    use aterm_phase::muse::fixtures as muse;
    let cases = [
        ("agy", screen(agy::ERROR_429), screen(agy::END_OF_TURN)),
        (
            "muse",
            screen(muse::USAGE_WALL),
            screen(muse::RESUMED_AFTER_CRASH),
        ),
    ];
    for (name, wall, after) in &cases {
        for (who, relauncher) in [
            ("no host", None),
            ("a person's", Some(false)),
            ("the host's", Some(true)),
        ] {
            let tag = format!("unvouched-{name}-{who}");
            let said = run_points_as(
                &tag,
                Autopilot::Land,
                &[
                    p(wall, None, 0, Some(60)),
                    p(&shell(), Some("zsh"), 30, None),
                    p(after, None, 60, Some(4)),
                ],
                relauncher,
            );
            let ours = relauncher == Some(true);
            let want = if ours { "wait" } else { "act usage-resume@v1" };
            assert_eq!(said[2].act, want, "{tag}: {said:#?}");
            assert_eq!(said[2].holds, ours, "{tag}: {said:#?}");
        }
    }
}

fn card_scrolled_off(rows: &[String]) -> Vec<String> {
    let card_end = rows
        .iter()
        .position(|r| r.contains("/private/tmp/"))
        .expect("PRECONDITION: the card's last row");
    let mut scrolled: Vec<String> = rows[card_end + 2..].to_vec();
    while scrolled.len() < rows.len() {
        scrolled.insert(0, "  earlier work".to_owned());
    }
    scrolled
}

#[test]
fn a_limit_met_past_the_first_screen_binds_the_login_the_card_drew() {
    use aterm_phase::agy::fixtures::lab as agy;
    let wall = card_scrolled_off(&screen(agy::ERROR_429));
    assert_eq!(
        Seat::of(aterm_phase::Program::Agy, &wall).login,
        None,
        "PRECONDITION: the 429 draws no login"
    );
    let ended = screen(agy::END_OF_TURN);
    let ended_a = drawn(&ended, "Gemini API key", "user@a.example (Google AI Pro)");
    let ended_b = drawn(&ended, "Gemini API key", "user@b.example (Google AI Pro)");
    for (name, after, moved) in [
        ("account kept", &ended_a, false),
        ("account moved", &ended_b, true),
    ] {
        let tag = format!("seen-{name}");
        let said = run_points(
            &tag,
            Autopilot::Land,
            &[
                p(&ended_a, None, 0, Some(240)),
                p(&wall, None, 600, Some(60)),
                p(&shell(), Some("zsh"), 630, None),
                p(after, None, 660, Some(4)),
            ],
        );
        assert!(said[1].track && said[1].holds, "{tag}: {said:#?}");
        let want = if moved { "act usage-resume@v1" } else { "wait" };
        assert_eq!(said[3].act, want, "{tag}: {said:#?}");
        assert_eq!(said[3].holds, !moved, "{tag}: {said:#?}");
    }
}

#[test]
fn a_wall_met_again_after_a_seat_move_is_the_new_seats_own() {
    use aterm_phase::agy::fixtures::lab as agy;
    let wall = screen(agy::ERROR_429);
    let ended = screen(agy::END_OF_TURN);
    let flash = |r: &[String]| drawn(r, "Gemini 3.1 Pro · low", "Gemini 3.5 Flash · low");
    let wall_flash = flash(&wall);
    let ended_flash = flash(&ended);
    let agy_shell = rows(&["$ agy", ""]);
    for moved in [true, false] {
        let host = AccountHost::scripted(&[None], &[]);
        *host.step.lock().unwrap() = Some("continued");
        host.relaunch_owed.store(true, Ordering::SeqCst);
        let opts = account_opts(&host);
        let mut m = agy_mock(vec![ended_flash.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_string()));
        s.set_zone_lookup(test_zones);
        let mut clock = Clock::start(&mut s);
        let mut seq = 0;
        let mut at =
            |s: &mut Session<'_, _>, rows: &[String], program: &str, worked: Option<u64>| {
                seq += 1;
                s.program = Some(program.to_owned());
                let point = point_at(s, rows.to_vec(), seq);
                let decided = s
                    .turn_end_at_point(&point, worked.map(Duration::from_secs), &opts)
                    .expect("a decision");
                (decided.rule_id().map(str::to_owned), s.policy_limit_holds())
            };
        let _ = at(&mut s, &wall, "agy", Some(60));
        if moved {
            clock.to(&mut s, 300);
            let (rule, holds) = at(&mut s, &ended_flash, "agy", None);
            assert_eq!(rule.as_deref(), Some("usage-resume@v1"), "the move");
            assert!(!holds, "the move releases the host");
            clock.to(&mut s, 310);
            let (_, holds) = at(&mut s, &wall_flash, "agy", None);
            assert!(holds, "Flash's own wall holds the host");
        }
        clock.to(&mut s, 330);
        let _ = at(&mut s, &agy_shell, "zsh", None);
        clock.to(&mut s, 360);
        let after = if moved { &ended_flash } else { &ended };
        let (rule, holds) = at(&mut s, after, "agy", Some(4));
        assert_eq!(rule, None, "moved={moved}: the relaunch waits");
        assert!(holds, "moved={moved}");
        clock.to(&mut s, 400);
        s.program = Some("agy".to_owned());
        let mut point = point_at(&s, after.clone(), 99);
        if let Some(row) = aterm_phase::agy::composer(after) {
            point.screen.cursor_row = row;
        }
        host.carry.store(true, Ordering::SeqCst);
        let stepped = s
            .host_steps_here(&point, &opts, &mut false, &mut Alone)
            .expect("the host's step");
        assert!(!stepped, "moved={moved}: the carry-on withheld");
        assert!(host.said("carry-on").is_empty(), "moved={moved}");
        if moved {
            clock.to(&mut s, 420);
            let (rule, _) = at(&mut s, &ended, "agy", None);
            assert_eq!(rule.as_deref(), Some("usage-resume@v1"), "the move back");
        }
    }
}

#[test]
fn gemini_clis_limit_left_for_another_model_goes_at_once() {
    use super::super::approval_loop::RetryDue;
    use aterm_phase::gemini::fixtures as g;
    let opts = SuperviseOpts {
        policy: owners_policy(),
        ..hosted(60)
    };
    let dialog = screen(g::LIMIT_DIALOG);
    let stopped = screen(g::LIMIT_STOPPED);
    let pro = drawn(&stopped, "          Auto", "gemini-3.5-pro");
    assert_eq!(
        aterm_phase::gemini::footer_model(&pro).as_deref(),
        Some("gemini-3.5-pro"),
        "PRECONDITION"
    );
    for (name, again, moved) in [("moved", &pro, true), ("kept", &stopped, false)] {
        let mut m = gemini_mock(vec![dialog.clone()]);
        let mut s = session(&mut m, None);
        s.program = Some("node".to_owned());
        s.set_zone_lookup(test_zones);
        let mut clock = Clock::start(&mut s);
        assert!(matches!(
            s.limit_retry_due(&dialog, &opts),
            RetryDue::Hold { .. }
        ));
        clock.to(&mut s, 1800);
        assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Due);
        s.limit_retry_pressed();
        clock.to(&mut s, 1810);
        let first = point_at(&s, stopped.clone(), 2);
        let decided = s
            .turn_end_at_point(&first, None, &opts)
            .expect("a decision");
        assert!(
            matches!(decided, TurnEndAction::WaitUntil { .. }),
            "{name}: {decided:?}"
        );
        clock.to(&mut s, 1900);
        let point = point_at(&s, again.clone(), 3);
        let decided = s
            .turn_end_at_point(&point, None, &opts)
            .expect("a decision");
        if moved {
            assert_eq!(
                decided.rule_id(),
                Some("usage-resume@v1"),
                "{name}: {decided:?}"
            );
        } else {
            assert!(
                matches!(decided, TurnEndAction::WaitUntil { .. }),
                "{name}: {decided:?}"
            );
        }
    }
}

#[test]
fn gemini_clis_limit_binds_the_login_its_banner_drew() {
    use super::super::approval_loop::RetryDue;
    use aterm_phase::gemini::fixtures as g;
    let dialog = screen(g::LIMIT_DIALOG);
    let stopped = screen(g::LIMIT_STOPPED);
    let banner = gemini_ended("Carried on before the crash.");
    assert_eq!(
        aterm_phase::gemini::banner_login(&banner).as_deref(),
        Some("Authenticated with gemini-api-key"),
        "PRECONDITION"
    );
    let google = drawn(
        &banner,
        "Authenticated with gemini-api-key",
        "Signed in with Google: user@example.com",
    );
    assert_eq!(
        aterm_phase::gemini::banner_login(&google).as_deref(),
        Some("Signed in with Google: user@example.com"),
        "PRECONDITION"
    );
    let gemini_shell = rows(&["$ gemini", ""]);
    for (name, after, moved) in [
        ("login moved", &google, true),
        ("login kept", &banner, false),
    ] {
        let host = AccountHost::scripted(&[None], &[]);
        host.relaunch_owed.store(true, Ordering::SeqCst);
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..account_opts(&host)
        };
        let mut m = gemini_mock(vec![dialog.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_owned()));
        s.program = Some("node".to_owned());
        s.set_zone_lookup(test_zones);
        let mut clock = Clock::start(&mut s);
        let first = point_at(&s, banner.clone(), 1);
        let _ = s.turn_end_at_point(&first, None, &opts);
        clock.to(&mut s, 10);
        assert!(matches!(
            s.limit_retry_due(&dialog, &opts),
            RetryDue::Hold { .. }
        ));
        clock.to(&mut s, 1810);
        assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Due);
        s.limit_retry_pressed();
        clock.to(&mut s, 1820);
        let point = point_at(&s, stopped.clone(), 2);
        let decided = s
            .turn_end_at_point(&point, None, &opts)
            .expect("a decision");
        assert!(
            matches!(decided, TurnEndAction::WaitUntil { .. }),
            "{name}: {decided:?}"
        );
        clock.to(&mut s, 1850);
        s.program = Some("zsh".to_owned());
        let point = point_at(&s, gemini_shell.clone(), 3);
        let _ = s.turn_end_at_point(&point, None, &opts);
        clock.to(&mut s, 1880);
        s.program = Some("node".to_owned());
        let point = point_at(&s, after.clone(), 4);
        let decided = s
            .turn_end_at_point(&point, Some(Duration::from_secs(4)), &opts)
            .expect("a decision");
        if moved {
            assert_eq!(
                decided.rule_id(),
                Some("usage-resume@v1"),
                "{name}: {decided:?}"
            );
        } else {
            assert!(
                matches!(decided, TurnEndAction::WaitUntil { .. }),
                "{name}: {decided:?}"
            );
            assert!(s.policy_limit_holds(), "{name}");
        }
    }
}

#[test]
fn gemini_clis_held_dialog_keeps_its_ladder_and_its_hold_through_a_relaunch() {
    use super::super::approval_loop::RetryDue;
    use aterm_phase::gemini::fixtures as g;
    let dialog = screen(g::LIMIT_DIALOG);
    let banner = gemini_ended("Carried on before the crash.");
    let gemini_shell = rows(&["$ gemini", ""]);
    for (name, away, ours) in [
        ("the host's relaunch", true, true),
        ("a person's relaunch", true, false),
        ("control: no relaunch", false, true),
    ] {
        let host = AccountHost::scripted(&[None], &[]);
        *host.step.lock().unwrap() = Some("continued");
        host.relaunch_owed.store(ours, Ordering::SeqCst);
        let opts = SuperviseOpts {
            policy: owners_policy(),
            ..account_opts(&host)
        };
        let mut m = gemini_mock(vec![banner.clone()]);
        let mut s = session(&mut m, Some("@s-1".to_owned()));
        s.program = Some("node".to_owned());
        s.set_zone_lookup(test_zones);
        let mut clock = Clock::start(&mut s);
        let _ = s.limit_retry_due(&dialog, &opts);
        clock.to(&mut s, 1800);
        assert_eq!(s.limit_retry_due(&dialog, &opts), RetryDue::Due, "{name}");
        s.limit_retry_pressed();
        clock.to(&mut s, 1810);
        assert_eq!(
            s.limit_retry_due(&dialog, &opts),
            RetryDue::Hold {
                until_unix: TEST_NOW + 1800 + 2 * 3600,
                reset: None
            },
            "{name}: PRECONDITION: held for the 2 h rung"
        );
        let point = point_at(&s, dialog.clone(), 1);
        let _ = s.turn_end_at_point(&point, None, &opts);
        if away {
            clock.to(&mut s, 2400);
            s.program = Some("zsh".to_owned());
            let point = point_at(&s, gemini_shell.clone(), 2);
            let _ = s.turn_end_at_point(&point, None, &opts);
        }
        clock.to(&mut s, 2430);
        s.program = Some("node".to_owned());
        let point = point_at(&s, banner.clone(), 3);
        let mut idle = point.clone();
        if let Some((row, _)) = aterm_phase::gemini::composer_draft(&banner) {
            idle.screen.cursor_row = row;
        }
        host.carry.store(true, Ordering::SeqCst);
        let stepped = s
            .host_steps_here(&idle, &opts, &mut false, &mut Alone)
            .expect("the host's step");
        assert_eq!(stepped, !away, "{name}: the host's step at the relaunch");
        assert_eq!(
            host.said("carry-on").is_empty(),
            away,
            "{name}: the carry-on at the relaunch"
        );
        host.carry.store(false, Ordering::SeqCst);
        let decided = s
            .turn_end_at_point(&point, Some(Duration::from_secs(2)), &opts)
            .expect("a decision");
        match (away, ours) {
            (true, true) => {
                let held = match &decided {
                    TurnEndAction::WaitUntil { until, .. } => {
                        until.saturating_duration_since(s.fx.mono()) >= Duration::from_secs(3600)
                    }
                    _ => false,
                };
                assert!(held, "{name}: {decided:?}");
                assert!(s.turn_end.has_limit_track(), "{name}");
                assert!(s.policy_limit_holds(), "{name}");
                clock.to(&mut s, 2500);
                let mut idle = point_at(&s, banner.clone(), 4);
                if let Some((row, _)) = aterm_phase::gemini::composer_draft(&banner) {
                    idle.screen.cursor_row = row;
                }
                host.carry.store(true, Ordering::SeqCst);
                let stepped = s
                    .host_steps_here(&idle, &opts, &mut false, &mut Alone)
                    .expect("the host's step");
                assert!(!stepped, "{name}: the carry-on withheld");
                assert!(host.said("carry-on").is_empty(), "{name}");
            }
            (true, false) => {
                let soon = match &decided {
                    TurnEndAction::WaitUntil { until, .. } => {
                        until.saturating_duration_since(s.fx.mono()) <= Duration::from_secs(150)
                    }
                    other => other.rule_id() == Some("usage-resume@v1"),
                };
                assert!(soon, "{name}: {decided:?}");
            }
            (false, _) => {
                assert!(
                    !matches!(decided, TurnEndAction::WaitUntil { .. })
                        || !s.turn_end.has_limit_track(),
                    "{name}: {decided:?}"
                );
                assert!(!s.policy_limit_holds(), "{name}");
            }
        }
        let resumed_at = if ours && away {
            1800 + 2 * 3600 + 60
        } else {
            2440
        };
        clock.to(&mut s, resumed_at);
        s.limit_resume_typed(&banner);
        clock.to(&mut s, resumed_at + 5);
        let want = TEST_NOW
            + if away {
                resumed_at + 6 * 3600
            } else {
                resumed_at + 5 + 1800
            };
        assert_eq!(
            s.limit_retry_due(&dialog, &opts),
            RetryDue::Hold {
                until_unix: want,
                reset: None
            },
            "{name}"
        );
    }
}

#[test]
fn gemini_clis_per_day_badge_stands_off_its_dialog_while_the_limit_stands() {
    use aterm_phase::gemini::fixtures as g;
    let gemini_shell = rows(&["$ gemini", ""]);
    for (tag, tail, stands) in [
        (
            "the stats' output",
            vec![gemini_ended("Here are the stats.")],
            true,
        ),
        ("a shell", vec![gemini_shell.clone()], true),
        (
            "control: a turn ran past it",
            vec![screen(g::BUSY_THINKING), gemini_ended("Worked past it.")],
            false,
        ),
    ] {
        let (dir, journal) = journal_file("te-gemini-badge-stands");
        let mut screens = vec![
            screen(g::BUSY_THINKING),
            screen(g::LIMIT_DIALOG),
            screen(g::LIMIT_STOPPED),
        ];
        screens.extend(tail);
        let mut m = gemini_mock(screens);
        m.attention_gates = vec![(2, 0)];
        m.stall_sleep = Some(Duration::from_secs(3600));
        m.vanish_after = Some(8);
        m.exit_when = Some(|m| m.requests.iter().any(|r| verb_of(r) == "turn"));
        let opts = SuperviseOpts {
            policy: owners_policy(),
            journal: Some(journal.clone()),
            ..auto(3 * 3600)
        };
        let (lines, _) = watch_lines_with(&mut m, &opts, |s| {
            s.set_turn_end_timing(TurnEndTiming {
                min_work: Duration::ZERO,
                ..TurnEndTiming::default()
            });
            s.set_box_settle(Duration::from_secs(1));
            s.set_clock_running();
        });
        let (records, _) = journal_records(&journal);
        let _ = std::fs::remove_dir_all(&dir);
        let all = || records.iter().map(|r| r.line.as_str()).collect::<Vec<_>>();
        let set = m
            .requests
            .iter()
            .position(|r| r.starts_with("meta set attention owner=supervisor"));
        let unset = m
            .requests
            .iter()
            .position(|r| r.starts_with("meta unset attention owner=supervisor"));
        let turn = m.requests.iter().position(|r| verb_of(r) == "turn");
        assert!(set.is_some(), "{tag}: badged: {lines:#?}\n{:#?}", all());
        if stands {
            assert!(
                unset.is_none_or(|u| turn.is_some_and(|t| u > t)),
                "{tag}: the badge stood: {lines:#?}\n{:#?}\n{:#?}",
                m.requests,
                all()
            );
            assert!(
                tag == "a shell"
                    || records
                        .iter()
                        .any(|r| r.line.contains("usage-session resets")
                            && !r.line.contains("seq=103")),
                "{tag}: the limit waited on at that screen: {:#?}",
                all()
            );
        } else {
            assert!(
                unset.is_some_and(|u| turn.is_none_or(|t| u < t)),
                "{tag}: the badge went with the limit: {:#?}\n{:#?}",
                m.requests,
                all()
            );
        }
    }
}
