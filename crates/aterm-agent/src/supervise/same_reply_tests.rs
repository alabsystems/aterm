// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The live Muse Code tab's two-line usage wall replayed, and the same-reply guard
//! ([`TurnEndState::repeats`]) at a wall no reader names.

use super::*;
use crate::supervise::config::Autopilot;
use crate::supervise::policy::exception::FUTILE_AFTER_TRIES;
use aterm_phase::WallKind;
use aterm_phase::muse::fixtures as muse;
use aterm_phase::prompt::fixtures::screen;

/// The live tab's read, 2026-10-23 21:24:46 PDT ([`muse::USAGE_WALL_RESETS_DATED`]).
const CAPTURED: i64 = 1_792_815_886;
/// Its wall's reset, Oct 25 at 5:00 PM PDT.
const RESET: i64 = 1_792_972_800;

/// One point: its screen, its time from the loop's base, the turn it ended, when a person typed before it, and the history above the screen the host serves.
struct R {
    rows: Vec<String>,
    at: i64,
    worked: Option<u64>,
    typed: Option<i64>,
    above: Option<Vec<String>>,
}

fn r(rows: &[String], at: i64, worked: Option<u64>) -> R {
    R {
        rows: rows.to_vec(),
        at,
        worked,
        typed: None,
        above: None,
    }
}

/// What a point decided: the act or the wait (its end, unix), the exception it stands under, and whether a wall's track holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Out {
    act: String,
    until: Option<i64>,
    told: Option<String>,
    track: bool,
}

/// ONE LOOP of `program`'s from `base` unix seconds, each act it decides taken as typed.
struct Driver<'a> {
    s: Session<'a, Mock>,
    opts: SuperviseOpts,
    program: &'static str,
    at: i64,
    seq: u64,
}

impl<'a> Driver<'a> {
    /// The loop at `first` (from `base`); `seeded`, it takes up the ledger a loop before it wrote.
    fn new(
        m: &'a mut Mock,
        program: &'static str,
        ledger: &Path,
        autopilot: Autopilot,
        at: (i64, i64),
        seeded: bool,
    ) -> Self {
        Self::open(m, program, ledger, autopilot, at, seeded.then_some(false))
    }

    /// [`Self::new`], seeded as a loop Autopilot starts once lifted from Off (`lifted_off`) or not.
    fn open(
        m: &'a mut Mock,
        program: &'static str,
        ledger: &Path,
        autopilot: Autopilot,
        (base, first): (i64, i64),
        seed: Option<bool>,
    ) -> Self {
        let mut s = session(m, Some("@s-1".to_owned()));
        s.set_zone_lookup(test_zones);
        s.set_approval_ledger(Some(ledger.to_path_buf()));
        let mut notes = Vec::new();
        s.ledger = approvals::Ledger::open(Some(ledger), Some("@s-1"), &mut notes);
        let mut opts = hosted(30);
        opts.policy.autopilot = autopilot;
        s.set_clock(base + first, PDT);
        s.set_mono_standing();
        s.set_clock_running();
        // The ledger's rows carry the replay's time, so a seeded loop reads their ages in it.
        s.test_clock().stamp_by_script();
        if let Some(lifted_off) = seed {
            let now_ms = s.fx.stamp_ms();
            let _ = s.seed_from_ledger(ledger, now_ms, lifted_off);
        }
        Self {
            s,
            opts,
            program,
            at: first,
            seq: 0,
        }
    }

    fn to(&mut self, t: i64) {
        assert!(t >= self.at, "the replay's clock runs forward");
        self.s
            .test_clock()
            .skip(Duration::from_secs(u64::try_from(t - self.at).unwrap_or(0)));
        self.at = t;
    }

    /// The point `p` decided, an act taken as typed.
    fn point(&mut self, p: &R) -> Out {
        if let Some(typed) = p.typed {
            self.to(typed);
            self.s.person = Some(self.s.fx.mono());
        }
        self.to(p.at);
        if let Some(above) = &p.above {
            self.s.ctl.alt_screen = Some(false);
            self.s.ctl.scrollback = Some(above.clone());
            self.s.ctl.screens = vec![p.rows.clone()];
        }
        self.seq += 1;
        self.s.program = Some(self.program.to_owned());
        let point = point_at(&self.s, p.rows.clone(), self.seq);
        let a = self
            .s
            .turn_end_at_point(&point, p.worked.map(Duration::from_secs), &self.opts)
            .expect("the point's decision");
        let (act, until) = match &a {
            TurnEndAction::WaitUntil { until, .. } => {
                ("wait".to_owned(), Some(self.s.unix_of(*until)))
            }
            TurnEndAction::Nothing => ("nothing".to_owned(), None),
            a => (format!("act {}", a.rule_id().unwrap_or("-")), None),
        };
        if a.rule_id().is_some() {
            let read = self.s.turn_end_reading(&point.screen, None, &self.opts);
            self.s.turn_end.acted(&a, &read, self.s.fx.mono());
        }
        Out {
            act,
            until,
            told: self
                .s
                .exception_now
                .as_ref()
                .map(|e| format!("{} {}", e.kind.word(), e.why)),
            track: self
                .s
                .turn_end
                .wall_attempts(WallKind::UsageWeekly)
                .is_some(),
        }
    }
}

/// ONE LOOP of Muse Code's over `points` from `base` unix seconds ([`Driver`]).
fn replay(ledger: &Path, autopilot: Autopilot, base: i64, seeded: bool, points: &[R]) -> Vec<Out> {
    replay_as("muse", ledger, autopilot, base, seeded, points)
}

/// ONE LOOP of `program`'s over `points` ([`Driver`]).
fn replay_as(
    program: &'static str,
    ledger: &Path,
    autopilot: Autopilot,
    base: i64,
    seeded: bool,
    points: &[R],
) -> Vec<Out> {
    let mut m = Mock::new(vec![busy_screen()]);
    let first = points.first().map_or(0, |p| p.typed.unwrap_or(p.at));
    let mut d = Driver::new(&mut m, program, ledger, autopilot, (base, first), seeded);
    points.iter().map(|p| d.point(p)).collect()
}

/// The live screen before the person's `hi`: the harness's last continuation answered by the wall.
fn before_hi() -> Vec<String> {
    let rows = screen(muse::USAGE_WALL_RESETS_DATED);
    let hi = rows
        .iter()
        .position(|r| r == "❯ hi")
        .expect("the person's hi");
    let composer = rows
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the composer");
    rows[..hi]
        .iter()
        .chain(&rows[composer..])
        .cloned()
        .collect()
}

#[test]
fn muse_codes_dated_two_line_wall_is_waited_out_to_its_reset_a_persons_hi_included() {
    let before = before_hi();
    let after = screen(muse::USAGE_WALL_RESETS_DATED);
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("muse-resets-dated-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let out = replay(
            &ledger,
            autopilot,
            CAPTURED,
            false,
            &[
                r(&before, -3600, Some(3)),
                r(&before, -1800, None),
                R {
                    typed: Some(-200),
                    ..r(&after, 0, Some(2))
                },
                r(&after, 3600, None),
                r(&after, RESET - CAPTURED + 3600, None),
            ],
        );
        let books = std::fs::read_to_string(&ledger).unwrap_or_default();
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            books.contains(
                "wall: reset_unix=1792972800; reset=Oct 25 at 5:00 PM; kind=usage-weekly; \
                 message=Usage limit reached Your usage resets at Oct 25 at 5:00 PM)"
            ),
            "{tag}: the books name the wall it waits on: {books}"
        );
        for (k, o) in out[..4].iter().enumerate() {
            assert!(
                o.act == "wait" && o.until.is_some_and(|u| u >= RESET) && o.track,
                "{tag}: point {k} waits for the reset, not before it: {out:#?}"
            );
        }
        assert_eq!(
            out[4].act, "act usage-resume@v1",
            "{tag}: past the reset it goes on as at the one-line wall: {out:#?}"
        );
    }
}

/// `rows`, a screen whose last wall answered the harness's continuation, with the wall's answer
/// to each of `asks` under it: the continuation again (`None`) or a person's prompt.
fn answered(rows: &[String], asks: &[Option<&str>]) -> Vec<String> {
    let prompt = rows
        .iter()
        .position(|r| r.starts_with("❯ [aterm harness]"))
        .expect("a continuation");
    let composer = rows
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the composer");
    let mut out = rows[..composer].to_vec();
    for ask in asks {
        match ask {
            Some(ask) => out.extend([format!("❯ {ask}"), String::new()]),
            None => out.extend(rows[prompt..prompt + 4].iter().cloned()),
        }
        out.extend(rows[..3].iter().cloned());
    }
    out.extend(rows[composer..].iter().cloned());
    out
}

/// The live screen with its wall in words no reader names ([`answered`]).
fn unnamed(asks: &[Option<&str>]) -> Vec<String> {
    let rows: Vec<String> = before_hi()
        .iter()
        .map(|r| {
            r.replace("◆ Usage limit reached", "◆ Weekly allowance spent")
                .replace(
                    "  Your usage resets at Oct 25 at 5:00 PM",
                    "  Back on Oct 25 at 5:00 PM",
                )
        })
        .collect();
    answered(&rows, asks)
}

#[test]
fn a_wall_no_reader_names_is_typed_into_at_most_once_more_then_held_until_it_changes() {
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let day = 86_400;
        let out = replay(
            &ledger,
            autopilot,
            CAPTURED,
            false,
            &[
                r(&unnamed(&[]), 0, Some(600)),
                r(&unnamed(&[None]), 60, Some(3)),
                r(&unnamed(&[None]), 300, None),
                r(&unnamed(&[None, None]), 360, Some(3)),
                r(&unnamed(&[None, None]), 2 * day, None),
            ],
        );
        let acts: Vec<&str> = out.iter().map(|o| o.act.as_str()).collect();
        assert!(
            acts[0].starts_with("act ") && acts[1] == "wait" && acts[2].starts_with("act "),
            "{tag}: one act after the work, one more after the wall's first answer: {out:#?}"
        );
        assert_eq!(
            acts[3..],
            ["nothing", "nothing"],
            "{tag}: the same reply again holds every act: {out:#?}"
        );
        assert!(out[..3].iter().all(|o| o.told.is_none()), "{out:#?}");
        let held = Some(format!("stalled {}", Exception::repeats().why));
        assert!(
            out[3..].iter().all(|o| o.told == held),
            "{tag}: the tab's exception says so: {out:#?}"
        );

        let next = replay(
            &ledger,
            autopilot,
            CAPTURED + 2 * day,
            true,
            &[
                r(&unnamed(&[None, None]), 60, None),
                R {
                    typed: Some(100),
                    ..r(&unnamed(&[None, None, Some("hi")]), 200, Some(2))
                },
                r(&unnamed(&[None, None, Some("hi")]), 7200, None),
                r(&unnamed(&[None, None, Some("hi"), None]), 7260, Some(3)),
                r(&unnamed(&[None, None, Some("hi"), None]), 9000, None),
                r(&screen(muse::END_OF_TURN_1_4_10), 9600, Some(600)),
            ],
        );
        let _ = std::fs::remove_dir_all(dir);
        let acts: Vec<&str> = next.iter().map(|o| o.act.as_str()).collect();
        assert_eq!(
            acts[0], "nothing",
            "{tag}: the next loop keeps the hold the ledger carried: {next:#?}"
        );
        assert!(
            next[1].told.is_none() && acts[2].starts_with("act "),
            "{tag}: a person's turn lets one more act go: {next:#?}"
        );
        assert_eq!(
            acts[3..5],
            ["nothing", "nothing"],
            "{tag}: and the same reply to it holds again: {next:#?}"
        );
        assert!(
            acts[5].starts_with("act ") && next[5].told.is_none(),
            "{tag}: the agent working ends the hold: {next:#?}"
        );
    }
}

/// [`unnamed`] as a viewport 38 rows above the composer shows it: past the first answer every answer draws the same screen.
fn steady(answers: usize) -> Vec<String> {
    let rows = unnamed(&vec![None; answers]);
    let composer = rows
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the composer");
    rows[composer - 38..].to_vec()
}

/// [`unnamed`] with its last wall drawn first as Muse Code's API error, the frame before it settles.
fn api_frame(asks: &[Option<&str>]) -> Vec<String> {
    let mut rows = unnamed(asks);
    let wall = rows
        .iter()
        .rposition(|r| r == "◆ Weekly allowance spent")
        .expect("the wall");
    rows[wall] = "◆ model failed: API error 500: lab stub: the server had an error (server_error) \
                  (after 2 provider attempts)"
        .to_owned();
    rows.remove(wall + 1);
    rows
}

#[test]
fn a_wall_no_reader_names_on_a_screen_drawn_the_same_each_time_is_held_all_the_same() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    assert_eq!(steady(1), steady(2), "the viewport repeats itself");
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-steady-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let out = replay(
            &ledger,
            autopilot,
            CAPTURED,
            false,
            &[
                r(&screen(muse::END_OF_TURN_1_4_10), 0, Some(600)),
                r(&steady(1), 60, Some(3)),
                r(&steady(1), 300, None),
                r(&steady(2), 360, Some(3)),
                r(&steady(2), 86_400, None),
            ],
        );
        let _ = std::fs::remove_dir_all(dir);
        let acts: Vec<&str> = out.iter().map(|o| o.act.as_str()).collect();
        assert!(
            acts[0].starts_with("act ") && acts[1] == "wait" && acts[2].starts_with("act "),
            "{tag}: one act after the work, one more after the wall's first answer: {out:#?}"
        );
        assert_eq!(
            acts[3..],
            ["nothing", "nothing"],
            "{tag}: a screen that does not change is no excuse: {out:#?}"
        );
        assert!(out[3..].iter().all(|o| o.told == held), "{tag}: {out:#?}");
    }
}

#[test]
fn a_tab_already_looping_at_such_a_wall_gets_two_acts_at_most_from_a_loop_with_no_mark() {
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-taken-up-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        // The loop's first point, then each act's answer and the point an hour on past its back-off.
        let mut points: Vec<R> = (0u32..3)
            .flat_map(|k| {
                let at = 7200 * i64::from(k);
                let rows = steady(k as usize + 1);
                [
                    r(&rows, at, (k > 0).then_some(3)),
                    r(&rows, at + 3600, None),
                ]
            })
            .collect();
        points.push(r(&steady(3), 3 * 86_400, None));
        let out = replay(&ledger, autopilot, CAPTURED, false, &points);
        let _ = std::fs::remove_dir_all(dir);
        let acts = out.iter().filter(|o| o.act.starts_with("act ")).count();
        assert!(
            acts == 2 && out[4..].iter().all(|o| o.act == "nothing"),
            "{tag}: {out:#?}"
        );
    }
}

#[test]
fn a_wall_no_reader_names_behind_an_api_error_frame_is_retried_at_most_once_more_then_held() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-api-frame-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let out = replay(
            &ledger,
            autopilot,
            CAPTURED,
            false,
            &[
                r(&unnamed(&[]), 0, Some(600)),
                r(&api_frame(&[None]), 60, Some(3)),
                r(&unnamed(&[None]), 61, None),
                r(&unnamed(&[None]), 3600, None),
                r(&api_frame(&[None, None]), 3660, Some(3)),
                r(&unnamed(&[None, None]), 3661, None),
                r(&unnamed(&[None, None]), 2 * 86_400, None),
            ],
        );
        let _ = std::fs::remove_dir_all(dir);
        let acts: Vec<&str> = out.iter().map(|o| o.act.as_str()).collect();
        assert!(
            acts[0].starts_with("act ")
                && acts[1..3] == ["wait", "wait"]
                && acts[3] == "act api-retry@v1",
            "{tag}: the error frame's retry is the one more act: {out:#?}"
        );
        assert_eq!(
            acts[5..],
            ["nothing", "nothing"],
            "{tag}: once settled to the same reply, the retry ladder types nothing: {out:#?}"
        );
        assert!(out[5..].iter().all(|o| o.told == held), "{tag}: {out:#?}");
    }
}

#[test]
fn a_read_walls_reset_lets_the_same_reply_be_answered_once_more() {
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-reset-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let past = RESET - CAPTURED + 3600;
        let out = replay(
            &ledger,
            autopilot,
            CAPTURED,
            false,
            &[
                r(&unnamed(&[]), 0, Some(600)),
                r(&unnamed(&[None]), 60, Some(3)),
                r(&unnamed(&[None]), 300, None),
                r(&before_hi(), 360, Some(3)),
                r(&before_hi(), past, None),
                r(&unnamed(&[None, None]), past + 60, Some(3)),
                r(&unnamed(&[None, None]), past + 7200, None),
                r(&unnamed(&[None, None, None]), past + 7260, Some(3)),
            ],
        );
        let _ = std::fs::remove_dir_all(dir);
        let acts: Vec<&str> = out.iter().map(|o| o.act.as_str()).collect();
        assert!(
            acts[2].starts_with("act ") && acts[3] == "wait" && acts[4] == "act usage-resume@v1",
            "{tag}: the read wall is waited out and resumed: {out:#?}"
        );
        assert!(
            acts[5] == "wait" && acts[6].starts_with("act "),
            "{tag}: its reset is a change, the same reply after it answered once more: {out:#?}"
        );
        assert_eq!(acts[7], "nothing", "{tag}: and held again: {out:#?}");
    }
}

/// [`unnamed`] with each of `answers` running Muse Code's measured call ([`muse::TOOL_ROW_BASH_RAN_DESCRIBED`]) over its wall.
fn ran_into_unnamed(answers: usize) -> Vec<String> {
    ran_into(|k| unnamed(&vec![None; k]), answers)
}

/// `draw`'s screen of `answers` answers, each after a measured call row.
fn ran_into(draw: impl Fn(usize) -> Vec<String>, answers: usize) -> Vec<String> {
    let call = screen(muse::TOOL_ROW_BASH_RAN_DESCRIBED)
        .into_iter()
        .find(|r| r.starts_with("◆ Ran command"))
        .expect("the call row");
    let base = draw(0);
    let tail = base.len()
        - base
            .iter()
            .position(|r| r.starts_with("── Voice input"))
            .expect("the composer");
    let rows = draw(answers);
    let (head, asked) = rows[..rows.len() - tail].split_at(base.len() - tail);
    let mut out = head.to_vec();
    // Each answer: the continuation's three rows and a blank, then the wall's two and a blank.
    assert_eq!(asked.len(), 7 * answers, "the answers drawn");
    for answer in asked.chunks(7) {
        out.extend(answer[..4].iter().cloned());
        out.extend([call.clone(), String::new()]);
        out.extend(answer[4..].iter().cloned());
    }
    out.extend(base[base.len() - tail..].iter().cloned());
    out
}

#[test]
fn the_same_reply_after_a_tool_run_each_time_is_no_repeat() {
    let reader = aterm_phase::Program::Muse.reader();
    assert!(
        reader.turn_ran_tools(&ran_into_unnamed(2)),
        "the call is read"
    );
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-worked-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let points: Vec<R> = (0..4)
            .map(|k| {
                r(
                    &ran_into_unnamed(k),
                    1000 * i64::try_from(k).unwrap_or(0),
                    Some(600),
                )
            })
            .collect();
        let out = replay(&ledger, autopilot, CAPTURED, false, &points);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            out.iter()
                .all(|o| o.act.starts_with("act ") && o.told.is_none()),
            "{tag}: a call run before each answer types on: {out:#?}"
        );
    }
}

/// [`unnamed`] with each wall drawn in words of its own (no digit, so no one number): every answer a new reply.
fn worded(answers: usize) -> Vec<String> {
    let mut drawn = 0u8;
    unnamed(&vec![None; answers])
        .into_iter()
        .map(|r| {
            if r != "◆ Weekly allowance spent" {
                return r;
            }
            drawn += 1;
            format!("{r}, part {}", char::from(b'a' + drawn))
        })
        .collect()
}

#[test]
fn a_wall_no_reader_names_answered_after_a_long_spell_that_ran_no_tool_is_held_all_the_same() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    let walls: [(&'static str, WallRows); 4] = [
        ("muse", |k| unnamed(&vec![None; k])),
        ("claude", |k| {
            vendor_walled("claude", &vec![None; k], claude_wall)
        }),
        ("agy", |k| vendor_walled("agy", &vec![None; k], agy_wall)),
        ("node", |k| {
            vendor_walled("node", &vec![None; k], gemini_wall)
        }),
    ];
    // Claude Code's measured no-work error turn (`Brewed for 2m 52s`), and Muse Code's 8 min of provider attempts.
    for (program, rows) in walls {
        for (autopilot, busy) in [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [172, 480].map(|b| (a, b)))
        {
            let tag = format!("same-reply-long-{program}-{autopilot:?}-{busy}s");
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, program, &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, busy), rows, 12);
            let _ = std::fs::remove_dir_all(dir);
            assert_eq!(
                answers, 2,
                "{tag}: the act after the work and one more, then held: {out:#?}"
            );
            let last = &out[out.len() - 3..];
            assert!(
                last.iter().all(|o| o.act == "nothing" && o.told == held),
                "{tag}: {out:#?}"
            );
        }
    }
    // The control: a long answer in words of its own each time is work.
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-long-worded-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(&mut d, &r(&worded(0), 0, Some(600)), (60, 480), worded, 8);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            answers == 8 && out.iter().all(|o| o.told.is_none()),
            "{tag}: {out:#?}"
        );
    }
}

/// The harness's continuation as each program echoes it ([`vendor_walled`]).
const ASK: &str = "[aterm harness] Continue the task.";

/// A turn of the program's under its measured banner and above its measured composer: the
/// worker's words after work, then for each of `asks` the harness's continuation (`None`) or a
/// person's prompt, each answered by `wall(k)`, the rows of the wall's `k`th answer.
fn vendor_walled(
    program: &str,
    asks: &[Option<&str>],
    wall: impl Fn(usize) -> Vec<String>,
) -> Vec<String> {
    use aterm_phase::{agy::fixtures as agy, gemini::fixtures as gemini};
    let (base, opens) = match program {
        "claude" => (
            screen(aterm_phase::prompt::fixtures::API_ERROR_TWICE_MEASURED),
            "❯ ",
        ),
        "agy" => (screen(agy::ERROR_TURN), "> "),
        _ => (screen(gemini::END_OF_TURN), " > "),
    };
    let first = base
        .iter()
        .position(|r| r.starts_with(opens))
        .expect("the first message");
    let composer = match program {
        // Gemini CLI's message band opens a row above its text; its composer's status rows sit over its band.
        "node" => base
            .iter()
            .position(|r| r.trim_end().ends_with("? for shortcuts"))
            .expect("the status row"),
        _ => (first..base.len())
            .find(|&i| base[i].starts_with("────"))
            .expect("the composer"),
    };
    let message = |text: &str| match program {
        "node" => vec![
            base[first - 1].clone(),
            format!(" > {text}"),
            base[first + 1].clone(),
        ],
        "claude" => vec![format!("❯ {text}"), String::new()],
        _ => vec![format!("> {text}"), String::new()],
    };
    let head = if program == "node" { first - 1 } else { first };
    let mut out = base[..head].to_vec();
    out.extend(message("Run the division suite again on the lane fix."));
    out.push(match program {
        "claude" => "⏺ Suites are running; I'll report the number when it lands.".to_owned(),
        "agy" => "Suites are running; I'll report the number when it lands.".to_owned(),
        _ => "✦ Suites are running; I'll report the number when it lands.".to_owned(),
    });
    out.push(String::new());
    for (k, ask) in asks.iter().enumerate() {
        out.extend(message(ask.unwrap_or(ASK)));
        out.extend(wall(k));
        out.push(String::new());
    }
    out.extend(base[composer..].iter().cloned());
    out
}

/// A wall's rows at its `k`th answer ([`vendor_walled`]).
type WallRows = fn(usize) -> Vec<String>;

/// Claude Code's `⎿` row in words no reader names, under the message it answers.
fn claude_wall(_: usize) -> Vec<String> {
    vec![
        "  ⎿  Allowance spent for now · back at 5:11pm (America/Los_Angeles)".to_owned(),
        String::new(),
        "✻ Cogitated for 0s · done 3:11 PM".to_owned(),
    ]
}

/// agy's measured failed turn ([`aterm_phase::agy::fixtures::ERROR_TURN`]): a `⚠` row no reader
/// names, its `Error ID:` fresh at each answer.
fn agy_wall(k: usize) -> Vec<String> {
    vec![
        "⚠ failed to construct executor: plan model not specified; you must specify a valid model"
            .to_owned(),
        format!(
            "Error ID: {:08x}-3c96-43a3-9843-826cfa2918db-{}",
            0xe855_92f1_u32.wrapping_add(u32::try_from(k).unwrap_or(0) * 7919),
            k + 1
        ),
    ]
}

/// Gemini CLI's `✕` row in words no reader names, with its measured failure notice under it.
fn gemini_wall(_: usize) -> Vec<String> {
    vec![
        "✕ Allowance spent for now. Back on Oct 25 at 5:00 PM.".to_owned(),
        String::new(),
        String::new(),
        "ℹ This request failed. Press F12 for diagnostics, or run /settings and change \"Error \
         Verbosity\" to full for full details."
            .to_owned(),
    ]
}

fn reader_of(program: &str) -> &'static dyn aterm_phase::ScreenReader {
    match program {
        "claude" => aterm_phase::Program::Claude.reader(),
        "agy" => aterm_phase::Program::Agy.reader(),
        "muse" => aterm_phase::Program::Muse.reader(),
        _ => aterm_phase::Program::Gemini.reader(),
    }
}

#[test]
fn a_vendor_row_no_reader_names_is_typed_into_at_most_once_more_then_held() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    let walls: [(&'static str, WallRows); 3] = [
        ("claude", claude_wall),
        ("agy", agy_wall),
        ("node", gemini_wall),
    ];
    for (program, wall) in walls {
        let rows = |asks: &[Option<&str>]| vendor_walled(program, asks, wall);
        let reader = reader_of(program);
        for answers in 0..3 {
            let r = rows(&vec![None; answers]);
            assert!(
                reader.wall(&r).is_none() && reader.health_wall(&r).is_none(),
                "{program}: no reader names the wall: {r:#?}"
            );
        }
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("same-reply-{program}-{autopilot:?}");
            let (dir, ledger) = ledger_at(&tag);
            let out = replay_as(
                program,
                &ledger,
                autopilot,
                CAPTURED,
                false,
                &[
                    r(&rows(&[]), 0, Some(600)),
                    r(&rows(&[None]), 60, Some(3)),
                    r(&rows(&[None]), 300, None),
                    r(&rows(&[None, None]), 360, Some(3)),
                    r(&rows(&[None, None]), 2 * 86_400, None),
                    R {
                        typed: Some(2 * 86_400 + 60),
                        ..r(&rows(&[None, None, Some("hi")]), 2 * 86_400 + 120, Some(2))
                    },
                    r(&rows(&[None, None, Some("hi")]), 2 * 86_400 + 7200, None),
                    r(
                        &rows(&[None, None, Some("hi"), None]),
                        2 * 86_400 + 7260,
                        Some(3),
                    ),
                ],
            );
            let _ = std::fs::remove_dir_all(dir);
            let acts: Vec<&str> = out.iter().map(|o| o.act.as_str()).collect();
            assert!(
                acts[0].starts_with("act ") && acts[1] == "wait" && acts[2].starts_with("act "),
                "{tag}: one act after the work, one more after the wall's first answer: {out:#?}"
            );
            assert_eq!(
                acts[3..5],
                ["nothing", "nothing"],
                "{tag}: the same reply again holds every act: {out:#?}"
            );
            assert!(out[3..5].iter().all(|o| o.told == held), "{tag}: {out:#?}");
            assert!(
                acts[6].starts_with("act ") && acts[7] == "nothing",
                "{tag}: a person's turn lets one more act go, and the same reply holds again: {out:#?}"
            );
        }
    }
}

/// [`unnamed`] with its reset a countdown, each answer's a minute count of its own.
fn counting_down(asks: &[Option<&str>]) -> Vec<String> {
    let mut k = 0;
    unnamed(asks)
        .into_iter()
        .map(|r| {
            if r != "  Back on Oct 25 at 5:00 PM" {
                return r;
            }
            k += 1;
            format!("  Back in {} minutes", 900 - 37 * k)
        })
        .collect()
}

#[test]
fn a_wall_no_reader_names_counting_down_its_reset_is_the_same_reply() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    assert_ne!(counting_down(&[None]), counting_down(&[None, None])[..]);
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-countdown-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let out = replay(
            &ledger,
            autopilot,
            CAPTURED,
            false,
            &[
                r(&counting_down(&[]), 0, Some(600)),
                r(&counting_down(&[None]), 60, Some(3)),
                r(&counting_down(&[None]), 300, None),
                r(&counting_down(&[None, None]), 360, Some(3)),
                r(&counting_down(&[None, None]), 86_400, None),
            ],
        );
        let _ = std::fs::remove_dir_all(dir);
        let acts: Vec<&str> = out.iter().map(|o| o.act.as_str()).collect();
        assert!(
            acts[2].starts_with("act ") && acts[3..] == ["nothing", "nothing"],
            "{tag}: a count in the reply is no new reply: {out:#?}"
        );
        assert!(out[3..].iter().all(|o| o.told == held), "{tag}: {out:#?}");
    }
}

/// `points` points of a loop that follows its own decisions from `first` and then `from`: each
/// act answered a minute on by `rows` drawn with one more answer, in `worked` seconds, the first
/// point's included; each wait's end the next point; a day between points once nothing is typed.
/// The answers drawn, with what each point decided.
fn follow(
    d: &mut Driver<'_>,
    first: &R,
    (from, worked): (i64, u64),
    rows: impl Fn(usize) -> Vec<String>,
    points: usize,
) -> (Vec<Out>, usize) {
    follow_r(
        d,
        first,
        (from, worked),
        |k, at, took| r(&rows(k), at, took),
        points,
    )
}

/// [`follow`] with each point of `k` answers at `at` made by `point`.
fn follow_r(
    d: &mut Driver<'_>,
    first: &R,
    (from, worked): (i64, u64),
    point: impl Fn(usize, i64, Option<u64>) -> R,
    points: usize,
) -> (Vec<Out>, usize) {
    let mut out = vec![d.point(first)];
    let (mut answers, mut at, mut took) = if out[0].act.starts_with("act ") {
        (1, first.at + 60, Some(worked))
    } else {
        (0, from, None)
    };
    while out.len() < points {
        let o = d.point(&point(answers, at, took));
        (at, took) = match (&o.act[..], o.until) {
            ("wait", Some(until)) => (until - CAPTURED + 1, None),
            ("nothing", _) => (at + 86_400, None),
            _ => {
                answers += 1;
                (at + 60, Some(worked))
            }
        };
        out.push(o);
    }
    (out, answers)
}

/// The read wall of [`before_hi`] standing past its reset, each resume answered by it again ([`follow`]).
fn stood(d: &mut Driver<'_>, points: usize) -> (Vec<Out>, usize) {
    follow(
        d,
        &r(&before_hi(), 0, Some(600)),
        (RESET - CAPTURED + 120, 3),
        |k| answered(&before_hi(), &vec![None; k]),
        points,
    )
}

#[test]
fn a_read_limit_standing_past_its_reset_is_resumed_through_its_tries_then_held() {
    let futile = Some("futile The limit still stands an hour after its reset.".to_owned());
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("stood-limit-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = stood(&mut d, 16);
        let resumes = out
            .iter()
            .filter(|o| o.act == "act usage-resume@v1")
            .count();
        // Its back-off ladder's tries; the first point an hour past the reset holds.
        assert_eq!(
            resumes, FUTILE_AFTER_TRIES,
            "{tag}: the wall's tries past its reset, not one an hour for ever: {out:#?}"
        );
        let last = &out[out.len() - 4..];
        assert!(
            last.iter().all(|o| o.act == "nothing" && o.told == futile),
            "{tag}: held under the futile exception: {out:#?}"
        );

        let at = d.at + 86_400;
        let rows = answered(&before_hi(), &vec![None; answers]);
        let mut hi = vec![None; answers];
        hi.push(Some("hi"));
        let hi = answered(&before_hi(), &hi);
        let then = [
            d.point(&r(&rows, at, None)),
            d.point(&R {
                typed: Some(at + 60),
                ..r(&hi, at + 120, Some(2))
            }),
            d.point(&r(&hi, at + 7200, None)),
        ];
        assert_eq!(then[0].act, "nothing", "{tag}: {then:#?}");
        assert_eq!(
            then[2].act, "act usage-resume@v1",
            "{tag}: a person's turn lets one more resume go: {then:#?}"
        );

        let _ = std::fs::remove_dir_all(dir);
        let (dir, ledger) = ledger_at(&format!("{tag}-moved"));
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (held, answers) = stood(&mut d, 7);
        assert_eq!(held[6].act, "nothing", "{tag}: {held:#?}");
        let mut moved = answered(&before_hi(), &vec![None; answers]);
        let last = moved
            .iter()
            .rposition(|r| r.contains("Oct 25"))
            .expect("the last wall's reset");
        moved[last] = moved[last].replace("Oct 25", "Oct 26");
        let at = d.at + 60;
        let then = [
            d.point(&r(&moved, at, None)),
            d.point(&r(&moved, RESET + 86_400 - CAPTURED + 3600, None)),
        ];
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            then[0].act == "wait" && then[0].until >= Some(RESET + 86_400),
            "{tag}: a new reset is a new wall, waited for: {then:#?}"
        );
        assert_eq!(then[1].act, "act usage-resume@v1", "{tag}: {then:#?}");
    }
}

/// Muse Code's measured turn that ran `ls` and said nothing ([`muse::TOOL_ROW_BASH_RAN`]), once per answer to the harness.
fn ran_ls(answers: usize) -> Vec<String> {
    let rows = screen(muse::TOOL_ROW_BASH_RAN);
    let opened = rows
        .iter()
        .position(|r| r.starts_with("❯ "))
        .expect("the message");
    let composer = rows
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the composer");
    let mut out = rows[..composer].to_vec();
    for _ in 0..answers {
        out.push(format!("❯ {ASK}"));
        out.extend(rows[opened + 1..composer].iter().cloned());
    }
    out.extend(rows[composer..].iter().cloned());
    out
}

#[test]
fn a_short_turn_that_ran_a_tool_and_said_nothing_is_work_not_the_same_reply() {
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-ran-tools-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(&mut d, &r(&ran_ls(0), 0, Some(600)), (0, 5), ran_ls, 16);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            answers > 2 && out.iter().all(|o| o.act != "nothing" && o.told.is_none()),
            "{tag}: five seconds that ran a call is work: {out:#?}"
        );
    }
}

/// [`before_hi`] with its wall drawn as `wall`'s two rows, each of `answers` acts answered by it ([`answered`]).
fn limit_drawn(wall: [&str; 2], answers: usize) -> Vec<String> {
    let rows: Vec<String> = before_hi()
        .iter()
        .map(|r| match r.as_str() {
            "◆ Usage limit reached" => wall[0].to_owned(),
            "  Your usage resets at Oct 25 at 5:00 PM" => wall[1].to_owned(),
            _ => r.clone(),
        })
        .collect();
    answered(&rows, &vec![None; answers])
}

/// Muse Code's read walls that name no reset the policy can wait for: the API's 429 (measured,
/// [`muse::LIMIT_UNNAMED`]), the two-line wall's reset in words no clock reads or in none, and the
/// one-line wall.
const UNTIMED: [(&str, [&str; 2]); 4] = [
    (
        "429",
        [
            "◆ model failed: API error 429: The usage limit has been reached (usage_limit_reached) \
             (after 10 provider attempts)",
            "",
        ],
    ),
    (
        "noon",
        [
            "◆ Usage limit reached",
            "  Your usage resets at noon on Oct 25",
        ],
    ),
    ("bare", ["◆ Usage limit reached", "  Your usage resets at"]),
    ("one-line", ["◆ Usage limit reached.", ""]),
];

#[test]
fn a_read_wall_with_no_usable_reset_is_tried_through_its_ladder_then_held() {
    let reader = aterm_phase::Program::Muse.reader();
    let measured = screen(muse::LIMIT_UNNAMED);
    let at = measured
        .iter()
        .position(|r| r.contains("(usage_limit_reached)"))
        .expect("the measured row");
    assert_eq!(UNTIMED[0].1[0], measured[at], "the 429 row as measured");
    let held = Some(format!(
        "futile {}",
        Exception::limit_repeats(aterm_phase::Program::Muse, false).why
    ));
    for (form, wall) in UNTIMED {
        let rows = |k: usize| limit_drawn(wall, k);
        for k in 0..3 {
            let w = reader
                .wall(&rows(k))
                .unwrap_or_else(|| panic!("{form}: a read wall"));
            assert!(
                matches!(w.kind, WallKind::UsageSession | WallKind::UsageWeekly)
                    && crate::supervise::limit::parse_reset(w.reset.as_deref().unwrap_or(""))
                        .is_none(),
                "{form}: a usage wall with no reset a clock reads: {w:?}"
            );
        }
        // Each try answered at once, or busy past min_work and past progress with no tool run
        // (Muse Code's own provider attempts): no work either way.
        for (autopilot, busy) in [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [3, 150, 330].map(|b| (a, b)))
        {
            let tag = format!("untimed-{form}-{autopilot:?}-{busy}s");
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, busy), rows, 12);
            let tries = out.iter().filter(|o| o.act.starts_with("act ")).count();
            assert_eq!(
                tries, FUTILE_AFTER_TRIES,
                "{tag}: the wall's ladder keeps its tries and no more: {out:#?}"
            );
            let last = &out[out.len() - 3..];
            assert!(
                last.iter().all(|o| o.act == "nothing" && o.told == held),
                "{tag}: held under the futile exception: {out:#?}"
            );
            // A person's turn the wall answers lets one more try go.
            let mut hi = vec![None; answers];
            hi.push(Some("hi"));
            let hi = answered(&limit_drawn(wall, 0), &hi);
            let at = d.at + 86_400;
            let then = [
                d.point(&R {
                    typed: Some(at),
                    ..r(&hi, at + 60, Some(2))
                }),
                d.point(&r(&hi, at + 7200, None)),
            ];
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                then.iter().any(|o| o.act.starts_with("act ")),
                "{tag}: a person's turn lets one more try go: {then:#?}"
            );
        }
    }
}

/// `points` points of a loop that took over from the one before it at `at`, seeded from its
/// ledger, the wall drawn as `rows` with one more answer per act ([`follow`]).
fn restarted(
    ledger: &Path,
    autopilot: Autopilot,
    at: i64,
    rows: impl Fn(usize) -> Vec<String>,
    points: usize,
) -> Vec<Out> {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut d = Driver::new(&mut m, "muse", ledger, autopilot, (CAPTURED, at), true);
    follow(&mut d, &r(&rows(0), at, None), (at, 3), rows, points).0
}

#[test]
fn a_held_wall_stays_held_in_the_loop_that_takes_over() {
    let futile =
        |o: &Out| o.act == "nothing" && o.told.as_deref().is_some_and(|t| t.starts_with("futile "));
    for (form, wall) in UNTIMED {
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("restarted-{form}-{autopilot:?}");
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow(
                &mut d,
                &r(&limit_drawn(wall, 0), 0, Some(600)),
                (60, 3),
                |k| limit_drawn(wall, k),
                8,
            );
            assert!(futile(&out[7]), "{tag}: held: {out:#?}");
            let more = restarted(
                &ledger,
                autopilot,
                d.at + 3600,
                |k| limit_drawn(wall, answers + k),
                6,
            );
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                more.iter().all(futile),
                "{tag}: the next loop keeps the hold, no try of its own: {more:#?}"
            );
        }
    }
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("restarted-stood-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = stood(&mut d, 12);
        assert!(futile(&out[11]), "{tag}: held: {out:#?}");
        let more = restarted(
            &ledger,
            autopilot,
            d.at + 3600,
            |k| answered(&before_hi(), &vec![None; answers + k]),
            6,
        );
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            more.iter().all(futile),
            "{tag}: the next loop keeps the hold past the reset: {more:#?}"
        );
    }
}

/// [`unnamed`] with its wall's answers in two wordings by turns: the first, the second, the first…
fn alternating(answers: usize) -> Vec<String> {
    let mut drawn = 0;
    unnamed(&vec![None; answers])
        .into_iter()
        .map(|r| {
            if r != "◆ Weekly allowance spent" {
                return r;
            }
            drawn += 1;
            // The viewport's top and the continuation's first answer, then each act's.
            if drawn > 2 && drawn % 2 == 1 {
                "◆ Allowance used up this week".to_owned()
            } else {
                r
            }
        })
        .collect()
}

#[test]
fn a_wall_no_reader_names_in_two_wordings_by_turns_is_held_at_its_first_repeat() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    assert_ne!(alternating(1), unnamed(&[None]), "the second wording drawn");
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("same-reply-alternating-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(
            &mut d,
            &r(&alternating(0), 0, Some(600)),
            (60, 3),
            alternating,
            10,
        );
        let _ = std::fs::remove_dir_all(dir);
        // The act after the work, one after each wording, held when the first comes again.
        assert_eq!(answers, 3, "{tag}: {out:#?}");
        let last = &out[out.len() - 3..];
        assert!(
            last.iter().all(|o| o.act == "nothing" && o.told == held),
            "{tag}: {out:#?}"
        );
    }
}

/// Claude Code's 529 row as 2.1.283 draws it ([`aterm_phase::prompt::fixtures::API_ERROR_529`]) and the turn's end under it.
fn claude_529(_: usize) -> Vec<String> {
    let rows = screen(aterm_phase::prompt::fixtures::API_ERROR_529);
    let at = rows
        .iter()
        .position(|r| r.starts_with("⏺ API Error: 529"))
        .expect("the 529 row");
    rows[at..at + 4].to_vec()
}

/// A Claude Code turn that ran a call and reported.
fn claude_ran_suite(_: usize) -> Vec<String> {
    [
        "⏺ Bash(targo --unverified test -p aterm-gui 2>&1 | tail -3)",
        "  ⎿  test result: ok. 412 passed; 0 failed",
        "",
        "⏺ The suites pass; landing the fix next.",
        "",
        "✻ Worked for 4m 10s",
    ]
    .map(str::to_owned)
    .to_vec()
}

#[test]
fn a_529_first_met_as_a_continuations_answer_keeps_its_three_retries() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    let walled = |k: usize| vendor_walled("claude", &vec![None; k], claude_529);
    let reader = aterm_phase::Program::Claude.reader();
    assert_eq!(
        reader.wall(&walled(1)).map(|w| w.kind),
        Some(WallKind::Overloaded),
        "the continuation's answer is the 529"
    );
    let cleared = |k: usize| {
        vendor_walled("claude", &vec![None; k], |j| {
            if j < 3 {
                claude_529(j)
            } else {
                claude_ran_suite(j)
            }
        })
    };
    assert!(
        reader.turn_ran_tools(&cleared(4)),
        "the third retry's answer ran a call"
    );
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        // Each answer after Claude Code's own retries of the call, no tool run (`Brewed for 2m 58s`, measured at 2.1.299).
        let tag = format!("first-met-529-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, _) = follow(&mut d, &r(&walled(0), 0, Some(600)), (60, 178), walled, 14);
        let _ = std::fs::remove_dir_all(dir);
        let retries = out.iter().filter(|o| o.act == "act api-retry@v1").count();
        assert!(
            out[0].act.starts_with("act ") && out[0].act != "act api-retry@v1",
            "{tag}: the continuation first: {out:#?}"
        );
        assert_eq!(
            retries, FUTILE_AFTER_TRIES,
            "{tag}: the continuation's answer is none of the wall's tries: {out:#?}"
        );
        let last = &out[out.len() - 3..];
        assert!(
            last.iter().all(|o| o.act == "nothing" && o.told == held),
            "{tag}: held after them: {out:#?}"
        );

        let (dir, ledger) = ledger_at(&format!("{tag}-cleared"));
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, _) = follow(
            &mut d,
            &r(&cleared(0), 0, Some(600)),
            (60, 178),
            cleared,
            10,
        );
        let _ = std::fs::remove_dir_all(dir);
        let third = out
            .iter()
            .enumerate()
            .filter(|(_, o)| o.act == "act api-retry@v1")
            .nth(2)
            .map(|(k, _)| k)
            .unwrap_or_else(|| panic!("{tag}: the third retry typed: {out:#?}"));
        let next = &out[third + 1];
        assert!(
            next.act.starts_with("act ") && next.act != "act api-retry@v1" && next.told.is_none(),
            "{tag}: the call it ran ends the streak, the next act typed at once: {out:#?}"
        );
    }
}

#[test]
fn an_untimed_wall_first_met_as_a_continuations_answer_keeps_its_three_tries() {
    let futile = Some(format!(
        "futile {}",
        Exception::limit_repeats(aterm_phase::Program::Muse, false).why
    ));
    for (form, wall) in UNTIMED {
        let rows = |k: usize| {
            if k == 0 {
                screen(muse::END_OF_TURN_1_4_10)
            } else {
                limit_drawn(wall, k)
            }
        };
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("first-met-{form}-{autopilot:?}");
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, _) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, 3), rows, 12);
            let _ = std::fs::remove_dir_all(dir);
            let resumes = out
                .iter()
                .filter(|o| o.act == "act usage-resume@v1")
                .count();
            assert!(
                out[0].act.starts_with("act ") && out[0].act != "act usage-resume@v1",
                "{tag}: the continuation first: {out:#?}"
            );
            assert_eq!(resumes, FUTILE_AFTER_TRIES, "{tag}: {out:#?}");
            let last = &out[out.len() - 3..];
            assert!(
                last.iter().all(|o| o.act == "nothing" && o.told == futile),
                "{tag}: {out:#?}"
            );
        }
    }
}

/// A Claude Code answer that reran the build for 9 min and reports the same failure, its calls drawn as the measured collapsed `group`.
fn build_fails(group: &'static str) -> impl Fn(usize) -> Vec<String> {
    move |k: usize| {
        vendor_walled("claude", &vec![None; k], |_| {
            [
                "⏺ Rebuilt and reran the suite.",
                "",
                group,
                "",
                "⏺ The build still fails: `parse_expr` is missing a lifetime.",
                "",
                "✻ Worked for 9m 10s",
            ]
            .map(str::to_owned)
            .to_vec()
        })
    }
}

#[test]
fn a_build_that_fails_the_same_after_collapsed_shell_commands_is_work_each_time() {
    let reader = aterm_phase::Program::Claude.reader();
    // The 2026-09-12 capture's and 2.1.283's hint-less rows.
    for group in ["  Ran 2 shell commands", "  Listed 1 directory"] {
        let rows = build_fails(group);
        assert!(reader.turn_ran_tools(&rows(2)), "{group:?} is read");
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("collapsed-work-{autopilot:?}-{}", group.len());
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, 550), &rows, 10);
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                answers == 10 && out.iter().all(|o| o.told.is_none()),
                "{tag}: {group:?} is work, every act typed: {out:#?}"
            );
        }
    }
}

#[test]
fn collapsed_shell_work_cut_by_a_529_each_time_keeps_its_retries() {
    let reader = aterm_phase::Program::Claude.reader();
    let rows = |k: usize| {
        vendor_walled("claude", &vec![None; k], |j| {
            let mut v = vec![
                format!("⏺ Ran stage {} of the migration.", j + 1),
                String::new(),
                "  Ran 3 shell commands".to_owned(),
                String::new(),
            ];
            v.extend(claude_529(j));
            v
        })
    };
    assert_eq!(
        reader.wall(&rows(2)).map(|w| w.kind),
        Some(WallKind::Overloaded)
    );
    assert!(reader.turn_ran_tools(&rows(2)), "the group row is read");
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("collapsed-529-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, _) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, 550), rows, 10);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            out.iter().all(|o| o.act != "nothing" && o.told.is_none()),
            "{tag}: each stage's work before its 529 keeps the retries going: {out:#?}"
        );
    }
}

/// Muse Code's `loops` loops over `rows`, each ended by the act it types (its row written as the loop writes it), the next seeded from the ledger; the first answers 600 s of work, each act a 3 s one.
fn restarting_each_act(
    ledger: &Path,
    autopilot: Autopilot,
    rows: impl Fn(usize) -> Vec<String>,
    loops: usize,
) -> (Vec<Out>, usize) {
    let (mut out, mut answers, mut at) = (Vec::new(), 0, 0);
    for k in 0..loops {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", ledger, autopilot, (CAPTURED, at), k > 0);
        let mut took = Some(if k == 0 { 600 } else { 3 });
        loop {
            let o = d.point(&r(&rows(answers), at, took.take()));
            let (act, until) = (o.act.clone(), o.until);
            out.push(o);
            match (&act[..], until) {
                ("wait", Some(u)) => at = u - CAPTURED + 1,
                ("nothing", _) => return (out, answers),
                _ => {
                    let reason =
                        approvals::act_reason_after(d.s.carried(), d.s.turn_end.limit_try());
                    let rule = act.trim_start_matches("act ");
                    let seq = d.seq;
                    d.s.ledger_row(rule, approvals::Outcome::Typed, ASK, &reason, seq);
                    (answers, at) = (answers + 1, at + 60);
                    break;
                }
            }
        }
    }
    (out, answers)
}

/// [`rotating`]'s four wordings of the wall no reader names.
const WORDINGS: [&str; 4] = [
    "◆ Weekly allowance spent",
    "◆ Allowance used up this week",
    "◆ This week's allowance is gone",
    "◆ No allowance left this week",
];

/// [`unnamed`] with each act's answer in the next of four wordings: none is kept when it comes again ([`REPLIES_KEPT`]).
fn rotating(answers: usize) -> Vec<String> {
    let mut drawn = 0;
    unnamed(&vec![None; answers])
        .into_iter()
        .map(|r| {
            if r != WORDINGS[0] {
                return r;
            }
            drawn += 1;
            // The viewport's top and the continuation's first answer, then each act's.
            if drawn > 2 {
                WORDINGS[(drawn - 3) % WORDINGS.len()].to_owned()
            } else {
                r
            }
        })
        .collect()
}

#[test]
fn a_program_rotating_four_no_work_wordings_is_held_after_its_run_of_no_work_acts() {
    use crate::supervise::policy::turn_end::{NO_WORK_ACTS, REPLIES_KEPT};
    let held = Some(format!("stalled {}", Exception::repeats().why));
    const { assert!(REPLIES_KEPT < 4, "four wordings outrun the replies kept") };
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("rotate-4-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(
            &mut d,
            &r(&rotating(0), 0, Some(600)),
            (60, 3),
            rotating,
            24,
        );
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(answers, NO_WORK_ACTS, "{tag}: {out:#?}");
        let last = &out[out.len() - 3..];
        assert!(
            last.iter().all(|o| o.act == "nothing" && o.told == held),
            "{tag}: {out:#?}"
        );
    }
    // A loop that restarts after every act (the live record's pattern) takes the run up from the ledger.
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("rotate-4-restarting-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let (out, answers) = restarting_each_act(&ledger, autopilot, rotating, 12);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            answers == NO_WORK_ACTS
                && out
                    .last()
                    .is_some_and(|o| o.act == "nothing" && o.told == held),
            "{tag}: {out:#?}"
        );
    }
    // The control: the same wordings each after a call run are work, every act typed.
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("rotate-4-worked-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let worked = |k: usize| ran_into(rotating, k);
        let (out, answers) = follow(&mut d, &r(&worked(0), 0, Some(600)), (60, 600), worked, 24);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            answers == 24 && out.iter().all(|o| o.told.is_none()),
            "{tag}: {out:#?}"
        );
    }
}

/// [`rotating`]'s held screen, then Muse Code's own work while no loop watched (a person's message, the measured call row, new words), then `answers` answers in the four wordings by turns.
fn worked_unwatched(answers: usize) -> Vec<String> {
    use crate::supervise::policy::turn_end::NO_WORK_ACTS;
    let call = screen(muse::TOOL_ROW_BASH_RAN_DESCRIBED)
        .into_iter()
        .find(|r| r.starts_with("◆ Ran command"))
        .expect("the call row");
    let held = rotating(NO_WORK_ACTS);
    let composer = held
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the composer");
    let ask = held
        .iter()
        .position(|r| r.starts_with("❯ [aterm harness]"))
        .expect("a continuation");
    let mut out = held[..composer].to_vec();
    out.extend([
        "❯ fix the parser's lifetime and rerun the suite".to_owned(),
        String::new(),
        call,
        String::new(),
        "◆ Fixed `parse_expr`'s lifetime; the parser suite passes now.".to_owned(),
        String::new(),
    ]);
    for k in 0..answers {
        out.extend(held[ask..ask + 4].iter().cloned());
        out.extend([
            WORDINGS[k % WORDINGS.len()].to_owned(),
            "  Back on Oct 25 at 5:00 PM".to_owned(),
            String::new(),
        ]);
    }
    out.extend(held[composer..].iter().cloned());
    out
}

#[test]
fn a_loop_that_takes_over_after_work_no_loop_watched_starts_its_run_afresh() {
    use crate::supervise::policy::turn_end::NO_WORK_ACTS;
    let held = Some(format!("stalled {}", Exception::repeats().why));
    let muse = aterm_phase::Program::Muse.reader();
    assert!(
        muse.turn_ran_tools(&worked_unwatched(0)),
        "the call row is read"
    );
    // A ledger whose loop the four wordings held, its Autopilot then parked Off (`parked`) or not.
    let held_books = |tag: &str, autopilot: Autopilot, parked: bool| {
        let (dir, ledger) = ledger_at(tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(
            &mut d,
            &r(&rotating(0), 0, Some(600)),
            (60, 3),
            rotating,
            24,
        );
        assert!(
            answers == NO_WORK_ACTS && out.last().is_some_and(|o| o.told == held),
            "{tag}: held: {out:#?}"
        );
        if parked {
            d.s.off_stood(d.seq);
        }
        (dir, ledger)
    };
    let at = 30 * 86_400;
    // Autopilot lifted from Off; a loop taking over the books of a park; one taking over a hold's.
    for (lifted, parked) in [(true, false), (false, true), (false, false)] {
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("taken-over-{lifted}-{parked}-{autopilot:?}");
            let (dir, ledger) = held_books(&tag, autopilot, parked);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::open(
                &mut m,
                "muse",
                &ledger,
                autopilot,
                (CAPTURED, at),
                Some(lifted),
            );
            let (out, answers) = follow(
                &mut d,
                &r(&worked_unwatched(0), at, None),
                (at + 60, 3),
                worked_unwatched,
                24,
            );
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                answers == NO_WORK_ACTS
                    && out[..NO_WORK_ACTS].iter().all(|o| o.told.is_none())
                    && out.last().is_some_and(|o| o.told == held),
                "{tag}: the work ends the run, a new one runs its length: {out:#?}"
            );

            // The control: over the screen the loop before it was held at.
            let (dir, ledger) = held_books(&format!("{tag}-still"), autopilot, parked);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::open(
                &mut m,
                "muse",
                &ledger,
                autopilot,
                (CAPTURED, at),
                Some(lifted),
            );
            let o = d.point(&r(&rotating(NO_WORK_ACTS), at, None));
            let _ = std::fs::remove_dir_all(dir);
            if lifted || parked {
                assert!(
                    o.act.starts_with("act ") && o.told.is_none(),
                    "{tag}: a person's Off ends the hold: {o:#?}"
                );
            } else {
                assert!(
                    o.act == "nothing" && o.told == held,
                    "{tag}: the hold carries: {o:#?}"
                );
            }
        }
    }
    // The loop that held, its screen moved with no turn timed: the run holds only while its newest reply shows.
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("moved-under-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, _) = follow(
            &mut d,
            &r(&rotating(0), 0, Some(600)),
            (60, 3),
            rotating,
            24,
        );
        assert!(
            out.last().is_some_and(|o| o.told == held),
            "{tag}: {out:#?}"
        );
        let at = d.at + 600;
        let o = d.point(&r(&worked_unwatched(0), at, None));
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            o.act != "nothing" && o.told.is_none(),
            "{tag}: not held: {o:#?}"
        );
    }
}

/// A Claude Code answer that worked 9 min, its two builds and an edit drawn as call rows (`calls`) or none, and gave the same 45-line failure report.
fn reported(calls: bool) -> impl Fn(usize) -> Vec<String> {
    move |k: usize| {
        vendor_walled("claude", &vec![None; k], |_| {
            let mut v = report_rows(calls);
            v.extend([String::new(), "✻ Worked for 9m 10s".to_owned()]);
            v
        })
    }
}

/// [`reported`]'s calls and report.
fn report_rows(calls: bool) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if calls {
        v.extend(
            [
                "⏺ Bash(targo --unverified build -p parser 2>&1 | tail -3)",
                "  ⎿  error[E0106]: missing lifetime specifier",
                "",
                "⏺ Update(crates/parser/src/expr.rs)",
                "  ⎿  Updated crates/parser/src/expr.rs with 1 addition",
                "",
                "⏺ Bash(targo --unverified build -p parser 2>&1 | tail -3)",
                "  ⎿  error[E0106]: missing lifetime specifier",
                "",
            ]
            .map(str::to_owned),
        );
    }
    v.push("⏺ The build still fails the same way: `parse_expr` is missing a lifetime.".to_owned());
    v.extend((1..45).map(|n| format!("  Step {n} of the parse still stops at the same lifetime.")));
    v
}

/// [`reported`] as Muse Code draws it: each answer under the harness's continuation, its calls the measured row ([`muse::TOOL_ROW_BASH_RAN_DESCRIBED`]).
fn muse_reported(calls: bool) -> impl Fn(usize) -> Vec<String> {
    move |k: usize| {
        let call = screen(muse::TOOL_ROW_BASH_RAN_DESCRIBED)
            .into_iter()
            .find(|r| r.starts_with("◆ Ran command"))
            .expect("the call row");
        let base = screen(muse::END_OF_TURN_1_4_10);
        let composer = base
            .iter()
            .position(|r| r.starts_with("── Voice input"))
            .expect("the composer");
        let mut out = base[..composer].to_vec();
        for _ in 0..k {
            out.extend([format!("❯ {ASK}"), String::new()]);
            if calls {
                out.extend([call.clone(), String::new(), call.clone(), String::new()]);
            }
            out.push(
                "◆ The build still fails the same way: `parse_expr` is missing a lifetime."
                    .to_owned(),
            );
            out.extend(
                (1..45)
                    .map(|n| format!("  Step {n} of the parse still stops at the same lifetime.")),
            );
            out.push(String::new());
        }
        out.extend(base[composer..].iter().cloned());
        out
    }
}

/// `rows` through their last 40 rows, the rows above them served as history (`history`) or none.
fn viewed(rows: &[String], history: bool, at: i64, worked: Option<u64>) -> R {
    let cut = rows.len().saturating_sub(40);
    R {
        above: history.then(|| rows[..cut].to_vec()),
        ..r(&rows[cut..], at, worked)
    }
}

#[test]
fn a_long_report_scrolled_over_its_calls_is_work_unless_its_whole_turn_shows_none() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    type Drawn = fn(bool) -> Box<dyn Fn(usize) -> Vec<String>>;
    let programs: [(&'static str, Drawn); 2] = [
        ("claude", |calls| Box::new(reported(calls))),
        ("muse", |calls| Box::new(muse_reported(calls))),
    ];
    for (program, drawn) in programs {
        let reader = reader_of(program);
        let full = drawn(true)(2);
        let shown = &full[full.len() - 40..];
        assert!(
            reader.turn_ran_tools(&full)
                && !reader.turn_ran_tools(shown)
                && reader.turn_opens_above(shown),
            "{program}: the calls scrolled above the screen"
        );
    }
    // The whole turn read shows the calls, or no whole turn is read: work either way.
    for ((program, drawn), (calls, history)) in programs
        .into_iter()
        .flat_map(|p| [(true, true), (true, false), (false, false)].map(|c| (p, c)))
    {
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("scrolled-calls-{program}-{calls}-{history}-{autopilot:?}");
            let rows = drawn(calls);
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, program, &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow_r(
                &mut d,
                &viewed(&rows(0), history, 0, Some(600)),
                (60, 550),
                |k, at, took| viewed(&rows(k), history, at, took),
                8,
            );
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                answers == 8 && out.iter().all(|o| o.told.is_none()),
                "{tag}: every act typed: {out:#?}"
            );
        }
    }
    // The control: the whole turn read shows no call, or the history holds nothing above the screen, so the same report is held.
    for ((program, drawn), (above, autopilot)) in programs.into_iter().flat_map(|p| {
        [true, false]
            .into_iter()
            .flat_map(|above| [Autopilot::Cruise, Autopilot::Land].map(|a| (above, a)))
            .map(move |c| (p, c))
    }) {
        let tag = format!("scrolled-no-calls-{program}-{above}-{autopilot:?}");
        let rows = drawn(false);
        let view = |rows: &[String], at, took| match above {
            true => viewed(rows, true, at, took),
            false => R {
                above: Some(Vec::new()),
                ..viewed(rows, false, at, took)
            },
        };
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, program, &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow_r(
            &mut d,
            &view(&rows(0), 0, Some(600)),
            (60, 550),
            |k, at, took| view(&rows(k), at, took),
            8,
        );
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(answers, 2, "{tag}: {out:#?}");
        let last = &out[out.len() - 3..];
        assert!(
            last.iter().all(|o| o.act == "nothing" && o.told == held),
            "{tag}: {out:#?}"
        );
    }
}

/// agy's measured 429 block of `text` answering each act, its `retryDelay` `delay(k)` seconds at the `k`th answer and its Error ID fresh.
fn agy_quota(text: &'static str, delay: fn(usize) -> u64) -> impl Fn(usize) -> Vec<String> {
    move |answers: usize| {
        vendor_walled("agy", &vec![None; answers], |k| {
            let rows = screen(text);
            let head = rows
                .iter()
                .position(|r| r.starts_with("⚠ agent executor error"))
                .expect("the 429 block");
            let id = rows
                .iter()
                .position(|r| r.starts_with("Error ID: "))
                .expect("its Error ID");
            rows[head..=id]
                .iter()
                .map(|row| {
                    if let Some((before, after)) = row.split_once("retryDelay:") {
                        let rest =
                            after.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
                        format!("{before}retryDelay:{}{rest}", delay(k))
                    } else if row.starts_with("Error ID: ") {
                        format!(
                            "Error ID: {:08x}-8158-4e0b-9a9a-e53dd3496d29-{}",
                            0x8a18_7dd0_u32.wrapping_add(u32::try_from(k).unwrap_or(0) * 7919),
                            k + 1
                        )
                    } else {
                        row.clone()
                    }
                })
                .collect()
        })
    }
}

#[test]
fn an_agy_quota_wall_whose_retry_delay_moves_with_each_answer_keeps_its_tries_and_no_more() {
    use aterm_phase::agy::fixtures::lab::{ERROR_429, ERROR_429_PER_MINUTE};
    let reader = aterm_phase::Program::Agy.reader();
    let moving: fn(usize) -> u64 = |k| 20 + u64::try_from(k * 13 % 40).unwrap_or(0);
    let steady: fn(usize) -> u64 = |_| 45;
    for (quota, text) in [("per-day", ERROR_429), ("per-minute", ERROR_429_PER_MINUTE)] {
        for (name, delay) in [("moving", moving), ("steady", steady)] {
            let rows = agy_quota(text, delay);
            let resets: Vec<Option<String>> = (1..4)
                .map(|k| reader.wall(&rows(k)).and_then(|w| w.reset))
                .collect();
            assert!(
                resets
                    .iter()
                    .all(|r| r.as_deref().is_some_and(|r| r.starts_with("in "))),
                "{quota} {name}: a relative reset read: {resets:?}"
            );
            for autopilot in [Autopilot::Cruise, Autopilot::Land] {
                let tag = format!("agy-quota-{quota}-{name}-{autopilot:?}");
                let (dir, ledger) = ledger_at(&tag);
                let mut m = Mock::new(vec![busy_screen()]);
                let mut d = Driver::new(&mut m, "agy", &ledger, autopilot, (CAPTURED, 0), false);
                let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, 3), &rows, 16);
                let _ = std::fs::remove_dir_all(dir);
                // The act after the work met the wall, then its tries.
                assert_eq!(answers, 1 + FUTILE_AFTER_TRIES, "{tag}: {out:#?}");
                let last = &out[out.len() - 3..];
                assert!(
                    last.iter().all(|o| o.act == "nothing" && o.told.is_some()),
                    "{tag}: held, its exception told: {out:#?}"
                );
            }
        }
    }
}

/// [`before_hi`] with each act's answer the next of Muse Code's read walls of four classes by turns.
fn cross_class(answers: usize) -> Vec<String> {
    const WALLS: [[&str; 2]; 4] = [
        [
            "◆ model failed: API error 500: lab stub: the server had an error (server_error) \
             (after 10 provider attempts)",
            "",
        ],
        [
            "◆ model failed: API error 429: The usage limit has been reached (usage_limit_reached) \
             (after 10 provider attempts)",
            "",
        ],
        [
            "◆ model failed: API error 503: lab stub: the service is unavailable (unavailable) \
             (after 10 provider attempts)",
            "",
        ],
        ["◆ Usage limit reached.", ""],
    ];
    let base = before_hi();
    let prompt = base
        .iter()
        .position(|r| r.starts_with("❯ [aterm harness]"))
        .expect("a continuation");
    let composer = base
        .iter()
        .position(|r| r.starts_with("── Voice input"))
        .expect("the composer");
    let mut out = base[..composer].to_vec();
    for k in 0..answers {
        out.extend(base[prompt..prompt + 4].iter().cloned());
        out.extend(WALLS[k % WALLS.len()].map(str::to_owned));
        out.push(String::new());
    }
    out.extend(base[composer..].iter().cloned());
    out
}

#[test]
fn read_walls_of_four_classes_by_turns_are_held_past_the_run_and_a_walls_own_tries() {
    use crate::supervise::policy::turn_end::NO_WORK_ACTS;
    let reader = aterm_phase::Program::Muse.reader();
    let kinds: Vec<Option<WallKind>> = (1..5)
        .map(|k| reader.wall(&cross_class(k)).map(|w| w.kind))
        .collect();
    assert!(
        kinds.iter().all(Option::is_some) && kinds[0] != kinds[1],
        "four read walls of two kinds by turns: {kinds:?}"
    );
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("cross-class-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(
            &mut d,
            &r(&cross_class(0), 0, Some(600)),
            (60, 3),
            cross_class,
            30,
        );
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(
            answers,
            NO_WORK_ACTS + FUTILE_AFTER_TRIES,
            "{tag}: {out:#?}"
        );
        let last = &out[out.len() - 3..];
        assert!(
            last.iter().all(|o| o.act == "nothing" && o.told.is_some()),
            "{tag}: held, its exception told: {out:#?}"
        );
    }
}

#[test]
fn work_scrolled_above_a_529_keeps_the_retries_going() {
    let held = Some(format!("stalled {}", Exception::repeats().why));
    let cut = |calls: bool| {
        move |k: usize| {
            vendor_walled("claude", &vec![None; k], |j| {
                let mut v = report_rows(calls);
                v.push(String::new());
                v.extend(claude_529(j));
                v
            })
        }
    };
    let reader = aterm_phase::Program::Claude.reader();
    let full = cut(true)(2);
    assert_eq!(
        reader.wall(&full[full.len() - 40..]).map(|w| w.kind),
        Some(WallKind::Overloaded)
    );
    for (calls, history) in [(true, true), (true, false), (false, true)] {
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("scrolled-529-{calls}-{history}-{autopilot:?}");
            let rows = cut(calls);
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, _) = follow_r(
                &mut d,
                &viewed(&rows(0), history, 0, Some(600)),
                (60, 550),
                |k, at, took| viewed(&rows(k), history, at, took),
                10,
            );
            let _ = std::fs::remove_dir_all(dir);
            if calls {
                assert!(
                    out.iter().all(|o| o.act != "nothing" && o.told.is_none()),
                    "{tag}: the work before each 529 keeps the retries going: {out:#?}"
                );
            } else {
                // The control: the whole turn read shows no call, so the 529's tries are its last.
                assert!(
                    out[out.len() - 3..]
                        .iter()
                        .all(|o| o.act == "nothing" && o.told == held),
                    "{tag}: {out:#?}"
                );
            }
        }
    }
}
