// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WALLS UNDER FOREVER (X2 of the walls-3 stage): a limit carried through
//! its agent's relaunch (the policy's track, the host's carry-on, Gemini
//! CLI's ladder) and Cruise at each harness's wall.

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::policy::turn_end::Seat;
use aterm_phase::WallKind;
use aterm_phase::prompt::fixtures::screen;

/// One point of a run: its screen, the program the server names (`None`:
/// the screen alone names it), its time from the run's start, the work
/// read before it, and the agent's process start the host names (seconds
/// from the run's start; `None`: no host's word).
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

/// What the loop decided at each point, said short: `wait`, `nothing`, or
/// `act <rule>`; whether a usage or spend limit's track stood after it; and
/// whether the policy's limit held the host's steps then
/// (`Session::policy_limit_holds`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Said {
    act: String,
    track: bool,
    holds: bool,
}

/// A host that says only whose the agent's last relaunch was
/// (`IdleHost::relaunch_owed`): its own (a crash's relaunch on exit, the
/// live upgrade's), or none of its (a person's).
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

/// `points` decided one by one by a loop with no host under `autopilot`
/// (the policy's fold and decision, `Session::turn_end_at_point`), each at
/// its own time on the run's clock.
fn run_points(tag: &str, autopilot: Autopilot, points: &[P]) -> Vec<Said> {
    run_points_as(tag, autopilot, points, None)
}

/// [`run_points`] under a host whose word on the relaunch is `relauncher`
/// (`Some(true)`: its own; `None`: no host).
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

/// THE RUN'S CLOCKS, the wall clock and the monotonic one moved together
/// (the fixed clock runs on the monotonic one, which moves only by what the
/// run skips): an instant the policy kept at one point is the same unix
/// time at the next.
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

    /// The clocks moved to `at` seconds from the run's start.
    fn to<C: Ctl>(&mut self, s: &mut Session<'_, C>, at: i64) {
        assert!(at >= self.at, "the run's clock runs forward");
        s.test_clock().skip(Duration::from_secs(
            u64::try_from(at - self.at).unwrap_or(0),
        ));
        self.at = at;
        assert_eq!(s.now_unix(), TEST_NOW + at, "the wall clock moved with it");
    }
}

/// A shell's prompt: the agent left the tab (crashed, exited, upgraded).
fn shell() -> Vec<String> {
    rows(&["$ muse resume 0000", ""])
}

fn acts(said: &[Said]) -> Vec<&str> {
    said.iter().map(|s| s.act.as_str()).collect()
}

/// A RELAUNCH UNDER A SCREEN-ALONE AGENT'S LIMIT TYPES NOTHING INTO IT (X2
/// of the walls-3 stage; `TurnEndState::relaunch_startup`), under Cruise and
/// Land alike, where the relaunch is the host's own (a crash's relaunch on
/// exit, the live upgrade's: `IdleHost::relaunch_owed`, the seat the agent
/// ran under carried): Muse Code's usage limit, then its shell's prompt (it crashed),
/// then its resumed-after-crash screen read with four seconds of start-up
/// work — and agy's per-day 429 relaunched onto its replayed conversation, or
/// onto a fresh banner with no task. The start-up is no turn: the limit is
/// waited on to the reset it named, never continued at the two-minute
/// back-off, and `usage-resume@v1` is typed past the reset (a fresh banner,
/// no task, gets nothing typed, its limit still held for the host's steps).
/// Measured before (the walls-3 probe): the resumed screen's work dropped the
/// limit and `continue@v1` (Cruise) or `land@v1` (Land) went into it at the
/// back-off; the fresh banner dropped it as a conversation with no task.
/// NEGATIVE CONTROL: the same screens with no relaunch read between (no
/// shell, no host's start) — the work is a turn run past the limit, which
/// ends it, and the continue policy's act follows.
#[test]
fn a_relaunch_under_a_screen_alone_agents_limit_types_nothing_into_it() {
    use aterm_phase::agy::fixtures::lab as agy;
    use aterm_phase::muse::fixtures as muse;
    let muse_wall = screen(muse::USAGE_WALL);
    let resumed = screen(muse::RESUMED_AFTER_CRASH);
    let agy_wall = screen(agy::ERROR_429);
    let replayed = screen(agy::END_OF_TURN);
    let fresh = screen(agy::IDLE);
    // Muse Code's reset is 4 h 12 min off, agy's an hour.
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

            // NEGATIVE CONTROL: no relaunch read between.
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

/// THE HOST'S WORD IS A RELAUNCH TOO: with no shell read between, the
/// agent's process start the host names later than the limit's last read
/// (`TurnEndState::agent_started`, the GUI's kept snapshot) makes the next
/// point the relaunch's start-up, its work no turn — Muse Code's limit is
/// waited on, then resumed past its reset (the relaunch the host's own). NEGATIVE CONTROL: a start the
/// host names from before the limit was read is the same process, and the
/// work ends the limit.
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

/// THE HOST'S CARRY-ON IS HELD BY A RELAUNCHED AGENT'S LIMIT TO ITS RESET
/// (X2 of the walls-3 stage; `TurnEndState::limit_holds_host`,
/// `Session::host_steps_here`): Muse Code relaunched under its usage limit
/// (its shell read between), its loop's limit episode closed on the resumed
/// screen, has the relaunch's carry-on withheld (`limit-episode`) at its
/// first idle point while the policy waits on the reset, and offered it
/// once the reset and the hour's grace a notice left on screen gets have
/// passed. Measured before (the walls-3 probe): no episode stood at the
/// relaunched agent's first idle point, and the carry-on (`continued`) was
/// typed into the standing limit. NEGATIVE CONTROL: the same idle point
/// with no limit read before it gets the carry-on at once.
#[test]
fn a_relaunched_agents_limit_holds_the_hosts_carry_on_to_its_reset() {
    use aterm_phase::muse::fixtures as muse;
    let wall = screen(muse::USAGE_WALL);
    let resumed = screen(muse::IDLE_RELAUNCHED);
    let step = |limit: bool, at: i64| -> (bool, Vec<String>) {
        let host = AccountHost::scripted(&[None], &[]);
        *host.step.lock().unwrap() = Some("continued");
        // The relaunch is the host's own: its carry-on is what is owed.
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
    // Muse Code's reset is 15172 s past the wall; an hour's grace after it.
    let (stepped, said) = step(true, 15_172 + 3600 + 120);
    assert!(stepped, "offered past the reset and its grace");
    assert_eq!(said.len(), 1, "{said:?}");
    let (stepped, said) = step(false, 300);
    assert!(stepped, "the control: no limit, the carry-on at once");
    assert_eq!(said.len(), 1, "{said:?}");
}

/// GEMINI CLI'S LADDER CARRIES ACROSS A RELAUNCH UNDER ITS LIMIT (X2 of the
/// walls-3 stage; `Session::limit_ladder_kept`): the dialog that names no
/// reset, tried once at its first rung (30 min), answered `Stop`, its error
/// read as the limit, then the agent relaunched (its shell read between, its
/// banner after): the episode stands while the policy waits on the limit, so
/// the resume typed there is the episode's second try, and the dialog drawn
/// again on it waits the ladder's third rung (6 h) from that resume — not
/// the first rung again. Measured before: the first point off the dialog and
/// the error ended the episode, and the dialog after the relaunch started
/// the ladder over at 30 minutes. NEGATIVE CONTROL: the banner read with
/// work and no relaunch between (a turn run past the limit) ends the limit
/// and the episode with it: the dialog then starts over.
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
        // A person's `Stop`: the error read as the limit, waited on to the
        // ladder's next rung.
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
        // The resume typed at the limit's next rung, then the dialog again.
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

/// CRUISE AT EACH HARNESS'S WALL (X2 of the walls-3 stage): Cruise's
/// continue acts never type into a wall, and Cruise goes on after it lifts.
/// Claude Code's session limit, Muse Code's usage limit and agy's per-day
/// 429, each read at a turn's end under Cruise: waited on — no act — at the
/// wall and ten minutes later, `usage-resume@v1` typed past the reset, and
/// the turn that answers it, ended with no wall, gets Cruise's own act.
/// (Gemini CLI's wall is its dialog, a box no turn-end act types into:
/// `gemini_clis_bucket_dialog_is_waited_on_then_tried_by_its_stop_and_a_resume`
/// and `gemini_clis_usage_limit_with_its_reset_named_is_kept_trying_past_it`.)
/// NEGATIVE CONTROL: the same turn's end with no wall read before it gets
/// Cruise's act at once — the wall is what held it.
#[test]
fn cruise_waits_out_each_harness_wall_and_goes_on_after_it() {
    use aterm_phase::agy::fixtures::lab as agy;
    use aterm_phase::muse::fixtures as muse;
    // Claude Code's session limit three minutes after its turn's end, its
    // reset four hours on (the run starts at 8:55 AM PDT).
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

/// `rows` with `from` drawn as `to` wherever it stands.
fn drawn(rows: &[String], from: &str, to: &str) -> Vec<String> {
    let out: Vec<String> = rows.iter().map(|r| r.replacen(from, to, 1)).collect();
    assert_ne!(out, rows, "PRECONDITION: {from:?} is drawn");
    out
}

/// A LIMIT BINDS ITS OWN SEAT (X2 of the walls-3 stage; `Seat`,
/// `TurnEndState::observe`): agy's per-day 429 is its model's and its
/// login's. Its turn's end read later naming another model in its footer (a
/// person's `/model`), or its relaunch drawing another login on its launch
/// card (another account), goes at once: `usage-resume@v1` typed then, not
/// at the old seat's reset an hour on, and the host's steps are no longer
/// held (`TurnEndState::limit_holds_host`'s seat move: T1 of the review of
/// walls-3, which no test held). Measured before (the walls-3 probe): the
/// old seat's reset held the new one. NEGATIVE CONTROLS: the same screens
/// naming the limit's own model wait out the reset and hold the host; a
/// relaunch onto the account the limit was met under (an address on both
/// cards, a person's relaunch) and the host's own relaunch onto the same
/// key do too; Muse Code's limit is its account's, which its screen never
/// names — its footer's model moved changes nothing.
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
    // The model moved in the same process: no relaunch between.
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
    // Another login on the relaunch's launch card; the account's own; the
    // host's own relaunch on the same key.
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
    // Muse Code's limit is its account's: a model the footer names moved
    // changes nothing.
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

/// A RELAUNCH NOBODY HERE MADE, ON A SEAT NOTHING VOUCHES FOR, IS TRIED AT
/// ONCE (F1 of the review of walls-3; `TurnEndState::relaunch_startup`):
/// agy's per-day 429 met on its API key, then a person's relaunch on a key
/// (its card draws `Gemini API key`, whatever the key: another key draws the
/// same words), and Muse Code's usage limit, then a person's relaunch (its
/// screen names no account at all) — each with no host, and under a host
/// that made no relaunch — is another seat for all the loop can tell: the
/// limit binds it no longer, `usage-resume@v1` is typed at the relaunch's
/// first point, and the host's steps are not held. Measured before (the
/// review's probe `zz_f`): such a relaunch was held to the old seat's reset
/// (an hour, four), and the host's carry-on withheld to it plus an hour.
/// NEGATIVE CONTROL: the same relaunch the host's own (its carry-on owed:
/// the line it typed carries the env the agent ran under) is held to the
/// reset, the host held with it.
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

/// `rows` with the launch card's rows off the top: the session went on long
/// enough that agy's card scrolled away (every limit met past the first
/// screen). Filled from the top with earlier work, to the same height.
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

/// A LIMIT MET PAST THE FIRST SCREEN BINDS THE LOGIN THE CARD DREW AT THE
/// SESSION'S START (F1 of the review of walls-3; `TurnEndState::seat_seen`,
/// `seat_now`): agy's launch card names the account `user@a.example` at the
/// first turn's end, then scrolls off; the 429 met later draws no login. A
/// person's relaunch whose card names the same account is that account's —
/// the limit holds it, waited on, the host held — and one naming another
/// account goes at once. Measured before (the review's probe `zz_f`): the
/// limit bound no login, so no relaunch under another account ever moved
/// the seat. NEGATIVE CONTROL: the relaunch onto another account, which no
/// seat binds, is resumed at once — and the account kept is held only by the
/// login the earlier card drew (with none kept, the same relaunch is a seat
/// nothing vouches for, resumed at once).
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

/// THE NEW SEAT'S OWN WALL (C1 of the review of walls-3;
/// `TurnEndState::observe`): agy's 429 on Gemini 3.1 Pro, then a person's
/// `/model` to Gemini 3.5 Flash (resumed at once: another bucket), then the
/// 429 again on Flash with no work read before it (a fast refusal). That
/// wall is the new seat's: it holds the host's steps again, so the host's
/// own relaunch under it has no carry-on typed into it; and a move back to
/// Pro after it is told — resumed at once. Measured before (the review's
/// probe `zz_c`): the move stuck, the host's carry-on was offered into
/// Flash's standing wall an hour before its reset, and a further move was
/// never told. NEGATIVE CONTROL: with no move, the relaunch under the first
/// wall withholds the carry-on too, and the screen of the limit's own model
/// waits.
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
            // A move back to Pro after Flash's wall: told, resumed at once.
            clock.to(&mut s, 420);
            let (rule, _) = at(&mut s, &ended, "agy", None);
            assert_eq!(rule.as_deref(), Some("usage-resume@v1"), "the move back");
        }
    }
}

/// GEMINI CLI'S LIMIT IS ITS MODEL'S, AT THE ERROR ITS `Stop` LEFT (X2 of the
/// walls-3 stage; `Session::limit_stopped`): the error first read under the
/// footer's `Auto` is waited on to the ladder's next rung; read again naming
/// `gemini-3.5-pro` (a person's `/model` there, UNMEASURED: the footer's
/// value as the bundle draws a chosen model), the limit binds the session no
/// longer and the resume is typed at once. NEGATIVE CONTROL: read again
/// under `Auto`, it is still waited on.
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

/// GEMINI CLI'S LIMIT BINDS THE LOGIN ITS BANNER DREW (X2 of the walls-3
/// stage; T2 of its review: `Seat::of` for Gemini CLI, which no test held):
/// the banner read at the session's start (`Authenticated with
/// gemini-api-key`), the per-day dialog kept trying and answered `Stop`, its
/// error read as the limit and waited on, then the agent relaunched — the
/// host's own relaunch, so only the seat its screen names can move it.
/// The relaunch's banner signed in with Google (`Signed in with Google:
/// user@example.com`) is another login: the limit binds it no longer, and
/// `usage-resume@v1` is typed at its first point. NEGATIVE CONTROL: the
/// banner naming the same login waits the limit out.
#[test]
fn gemini_clis_limit_binds_the_login_its_banner_drew() {
    use super::super::approval_loop::RetryDue;
    use aterm_phase::gemini::fixtures as g;
    let dialog = screen(g::LIMIT_DIALOG);
    let stopped = screen(g::LIMIT_STOPPED);
    // The relaunch resumes its conversation: a turn's end under the banner.
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
        // The session's banner, its login read.
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

/// GEMINI CLI'S HELD DIALOG KEEPS ITS LADDER AND ITS HOLD THROUGH A
/// RELAUNCH (E1 of the review of walls-3; `Session::limit_retry_away`,
/// `Session::limit_stopped`): the per-day dialog tried once at its first
/// rung and drawn again (now held for the 2 h rung, no `Stop`), then the CLI
/// crashed — its shell read — and was relaunched by the host (its carry-on
/// owed). The relaunch's banner is that limit still: waited on to the 2 h
/// rung (no act), the host's carry-on withheld — at the relaunch's first
/// idle point, before its fold reads the limit, too — and the resume typed at its due is the
/// episode's next try, so the dialog drawn again on it waits the ladder's
/// third rung (6 h) from that resume. A person's relaunch on the same key
/// (a seat nothing vouches for) is resumed at once, a try of the same
/// episode, whose return climbs as well. Measured before (the review's
/// probe `zz_e`): the shell's point ended the episode, the host's carry-on
/// was typed into the standing quota, and the dialog it met started its
/// ladder over at 30 minutes. NEGATIVE CONTROL: the banner read with no
/// absence before it (a turn run past the dialog) ends the episode, holds
/// nothing (the host's step at it goes), and the dialog met after it starts
/// over at the first rung.
#[test]
fn gemini_clis_held_dialog_keeps_its_ladder_and_its_hold_through_a_relaunch() {
    use super::super::approval_loop::RetryDue;
    use aterm_phase::gemini::fixtures as g;
    let dialog = screen(g::LIMIT_DIALOG);
    // The relaunch resumes its conversation: a turn's end under the banner.
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
        // The host's step offered at the relaunch's first idle point, before
        // its fold reads it as the limit: withheld while the dialog's
        // episode is away; the control's (no relaunch) goes.
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
                // Held to the ladder's 2 h rung, not placed now.
                let held = match &decided {
                    TurnEndAction::WaitUntil { until, .. } => {
                        until.saturating_duration_since(s.fx.mono()) >= Duration::from_secs(3600)
                    }
                    _ => false,
                };
                assert!(held, "{name}: {decided:?}");
                assert!(s.turn_end.has_limit_track(), "{name}");
                assert!(s.policy_limit_holds(), "{name}");
                // The host's carry-on is withheld.
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
                // At once: typed here, or past no more than the reset's
                // grace and this session's jitter (the limit placed now).
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
        // The resume typed (at the due, or at once), then the dialog again.
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

/// THE PER-DAY LIMIT'S BADGE STANDS OFF ITS DIALOG WHILE THE LIMIT STANDS
/// (B1 of the review of walls-3; `Session::limit_badge_carry`,
/// `Session::limit_badge_off_ended`), end to end under `program=node` at
/// full power: the dialog answered `2. Stop` inside the box settle, the
/// error read as the limit and badged, then a screen that is neither the
/// dialog nor the error — the output of the `/stats model` the badge
/// suggests (no busy read: no try ran), or a shell's prompt (the CLI left
/// the tab) — while the limit is still waited on. The badge is not taken
/// down there: it stands till the resume typed at the ladder's rung.
/// Measured before (the review's probe `zz_b`): `CLEARED … box` at that
/// screen, the badge down while the same limit was waited on, raised again
/// only at the next dialog. NEGATIVE CONTROL: a turn that ran past the
/// error (a busy read, then its end) ended the limit, and the badge goes
/// before anything is typed.
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
            // The run ends on the screen it leaves (the session gone) or at
            // the resume typed at the rung: the badge stood through it.
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
