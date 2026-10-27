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

/// One point: its screen, its time from the loop's base, the turn it ended, when a person typed before it and whether their keys ended on an Enter over what they typed (the session's submit count one up), the history above the screen the host serves, and the session's input and submission counts the read carries where a recording gives them.
struct R {
    rows: Vec<String>,
    at: i64,
    worked: Option<u64>,
    typed: Option<i64>,
    entered: bool,
    above: Option<Vec<String>>,
    counts: Option<(u64, u64)>,
}

fn r(rows: &[String], at: i64, worked: Option<u64>) -> R {
    R {
        rows: rows.to_vec(),
        at,
        worked,
        typed: None,
        entered: false,
        above: None,
        counts: None,
    }
}

/// `p` after a person's keys at `typed` that ended on an Enter over what they typed.
fn sent(typed: i64, p: R) -> R {
    R {
        typed: Some(typed),
        entered: true,
        ..p
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
    /// How far the loop's clocks run past each point's whole second, as the shipped loop's do.
    jitter: Duration,
    /// The session's submit count (`text --json`'s `submits`): one up at each act taken and each person's Enter; served unless `uncounted`.
    submits: u64,
    uncounted: bool,
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
            // As the shipped loop starts: the books, then the wall they leave open.
            let now_ms = s.fx.stamp_ms();
            let open = s.seed_from_ledger(ledger, now_ms, lifted_off);
            s.seed_wall_memory(open);
        }
        Self {
            s,
            opts,
            program,
            at: first,
            seq: 0,
            jitter: Duration::ZERO,
            submits: 0,
            uncounted: false,
        }
    }

    /// The loop over a session whose submit count the loop before it left at `n`.
    const fn submitted(mut self, n: u64) -> Self {
        self.submits = n;
        self
    }

    /// The loop over screens that carry no submit count (`submits` unknown).
    const fn uncounted(mut self) -> Self {
        self.uncounted = true;
        self
    }

    /// The `/usage` probe at `t`: its Enter, the session's submits one up, between its typed and closed ledger rows as `probe_owes` and `probe_closed` write them; the policy's memory unmoved.
    fn probed(&mut self, t: i64) {
        use crate::supervise::policy::turn_end::RULE_USAGE_PROBE;
        self.to(t);
        let (typed, closed) = (approvals::Outcome::Typed, approvals::Outcome::Waiting);
        self.s.ledger_row(
            RULE_USAGE_PROBE,
            typed,
            "/usage",
            approvals::PROBE_TYPED_REASON,
            self.seq,
        );
        self.submits += 1;
        self.to(t + 5);
        self.s.ledger_row(
            RULE_USAGE_PROBE,
            closed,
            "/usage",
            approvals::PROBE_CLOSED_REASON,
            self.seq,
        );
    }

    /// The loop's monotonic clock runs `333 µs` past each point, so the wall clock's milliseconds lag it as the shipped loop's do.
    fn jittered(mut self) -> Self {
        self.jitter = Duration::from_micros(333);
        self
    }

    fn to(&mut self, t: i64) {
        assert!(t >= self.at, "the replay's clock runs forward");
        self.s
            .test_clock()
            .skip(Duration::from_secs(u64::try_from(t - self.at).unwrap_or(0)) + self.jitter);
        self.at = t;
    }

    /// The point `p` decided, an act taken as typed.
    fn point(&mut self, p: &R) -> Out {
        if let Some(typed) = p.typed {
            self.to(typed);
            self.s.person = Some(self.s.fx.mono());
        }
        self.submits += u64::from(p.entered);
        self.to(p.at);
        if let Some(above) = &p.above {
            self.s.ctl.alt_screen = Some(false);
            self.s.ctl.scrollback = Some(above.clone());
            self.s.ctl.screens = vec![p.rows.clone()];
        }
        self.seq += 1;
        self.s.program = Some(self.program.to_owned());
        let point = self.s.turn_of(Screen {
            rows: p.rows.clone(),
            seq: self.seq,
            submits: match p.counts {
                Some((_, submits)) => Some(submits),
                None => (!self.uncounted).then_some(self.submits),
            },
            inputs_before: p.counts.map(|(inputs, _)| inputs),
            inputs_after: p.counts.map(|(inputs, _)| inputs),
            ..Screen::default()
        });
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
            self.submits += 1;
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
    replay_counted(program, ledger, autopilot, base, (seeded, 0), points).0
}

/// [`replay_as`] over a session whose submit count the loop before it left at `submits`: what each point decided, and the count it leaves.
fn replay_counted(
    program: &'static str,
    ledger: &Path,
    autopilot: Autopilot,
    base: i64,
    (seeded, submits): (bool, u64),
    points: &[R],
) -> (Vec<Out>, u64) {
    let mut m = Mock::new(vec![busy_screen()]);
    let first = points.first().map_or(0, |p| p.typed.unwrap_or(p.at));
    let mut d =
        Driver::new(&mut m, program, ledger, autopilot, (base, first), seeded).submitted(submits);
    let out = points.iter().map(|p| d.point(p)).collect();
    (out, d.submits)
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
                sent(-200, r(&after, 0, Some(2))),
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
        let held = Some(format!(
            "stalled {}",
            Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
        ));
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
                sent(100, r(&unnamed(&[None, None, Some("hi")]), 200, Some(2))),
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
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
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

/// The error frame is an outage wall (v6 §14.17.5.2): its answers count for no hold, and its
/// track, standing behind the wall no reader names once the frame left, keeps its doubling ladder.
#[test]
fn a_wall_no_reader_names_behind_an_api_error_frame_keeps_the_outages_ladder_never_held() {
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
                && acts[1..]
                    == [
                        "wait",
                        "wait",
                        "act outage-resume@v1",
                        "wait",
                        "wait",
                        "act outage-resume@v1",
                    ],
            "{tag}: the error frame's blind resume at each rung: {out:#?}"
        );
        // The second rung is 4 min from the frame met again.
        assert_eq!(
            out[4].until.zip(out[1].until).map(|(b, a)| b - a),
            Some(3660 + 4 * 60 - (60 + 2 * 60)),
            "{tag}: {out:#?}"
        );
        assert!(out.iter().all(|o| o.told.is_none()), "{tag}: {out:#?}");
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
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    let walls: [(&'static str, WallRows); 3] = [
        ("muse", |k| unnamed(&vec![None; k])),
        ("claude", |k| {
            vendor_walled("claude", &vec![None; k], claude_wall)
        }),
        ("agy", |k| vendor_walled("agy", &vec![None; k], agy_wall)),
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
    use aterm_phase::agy::fixtures as agy;
    let (base, opens) = match program {
        "claude" => (
            screen(aterm_phase::prompt::fixtures::API_ERROR_TWICE_MEASURED),
            "❯ ",
        ),
        "agy" => (screen(agy::ERROR_TURN), "> "),
        other => panic!("no measured turn of {other}'s"),
    };
    let first = base
        .iter()
        .position(|r| r.starts_with(opens))
        .expect("the first message");
    let composer = (first..base.len())
        .find(|&i| base[i].starts_with("────"))
        .expect("the composer");
    let message = |text: &str| match program {
        "claude" => vec![format!("❯ {text}"), String::new()],
        _ => vec![format!("> {text}"), String::new()],
    };
    let mut out = base[..first].to_vec();
    out.extend(message("Run the division suite again on the lane fix."));
    out.push(match program {
        "claude" => "⏺ Suites are running; I'll report the number when it lands.".to_owned(),
        _ => "Suites are running; I'll report the number when it lands.".to_owned(),
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

fn reader_of(program: &str) -> &'static dyn aterm_phase::ScreenReader {
    match program {
        "claude" => aterm_phase::Program::Claude.reader(),
        "agy" => aterm_phase::Program::Agy.reader(),
        "muse" => aterm_phase::Program::Muse.reader(),
        other => panic!("no reader named {other}"),
    }
}

#[test]
fn a_vendor_row_no_reader_names_is_typed_into_at_most_once_more_then_held() {
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    let walls: [(&'static str, WallRows); 2] = [("claude", claude_wall), ("agy", agy_wall)];
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
        // The person's turn timed or not: a point sees its busy spell, or only what it drew, which lifts nothing.
        for (autopilot, took_hi) in [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [Some(2), None].map(|t| (a, t)))
        {
            let tag = format!("same-reply-{program}-{autopilot:?}-{took_hi:?}");
            let hi = rows(&[None, None, Some("hi")]);
            let points = [
                r(&rows(&[]), 0, Some(600)),
                r(&rows(&[None]), 60, Some(3)),
                r(&rows(&[None]), 300, None),
                r(&rows(&[None, None]), 360, Some(3)),
                r(&rows(&[None, None]), 2 * 86_400, None),
                sent(2 * 86_400 + 60, r(&hi, 2 * 86_400 + 120, took_hi)),
                r(&hi, 2 * 86_400 + 7200, None),
                // The one act it lets go answered the same, or the screen as it stood.
                match took_hi {
                    Some(_) => r(
                        &rows(&[None, None, Some("hi"), None]),
                        2 * 86_400 + 7260,
                        Some(3),
                    ),
                    None => r(&hi, 2 * 86_400 + 10_800, None),
                },
            ];
            let (dir, ledger) = ledger_at(&tag);
            let out = replay_as(program, &ledger, autopilot, CAPTURED, false, &points);
            let _ = std::fs::remove_dir_all(dir);
            // A loop taking over at the person's `hi`, the session's submit count carried on.
            let (dir, ledger) = ledger_at(&format!("{tag}-takeover"));
            let (before, submits) = replay_counted(
                program,
                &ledger,
                autopilot,
                CAPTURED,
                (false, 0),
                &points[..5],
            );
            let (after, _) = replay_counted(
                program,
                &ledger,
                autopilot,
                CAPTURED,
                (true, submits),
                &points[5..],
            );
            let _ = std::fs::remove_dir_all(dir);
            assert_eq!(before, out[..5], "{tag}: the same loop up to the hold");
            let taken: Vec<&str> = after.iter().map(|o| o.act.as_str()).collect();
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
            if took_hi.is_some() {
                assert!(
                    acts[6].starts_with("act ") && acts[7] == "nothing",
                    "{tag}: a person's turn a point saw busy lets one more act go, and the same \
                     reply holds again: {out:#?}"
                );
                assert!(
                    taken[1].starts_with("act ") && taken[2] == "nothing",
                    "{tag}-takeover: so does one the new loop's first point saw busy: {after:#?}"
                );
            } else {
                assert!(
                    out[5..]
                        .iter()
                        .chain(&after)
                        .all(|o| !o.act.starts_with("act "))
                        && out[6..].iter().chain(&after[1..]).all(|o| o.told == held),
                    "{tag}: a person's turn no point saw busy lifts nothing, in the loop or at a \
                     takeover: {out:#?} {after:#?}"
                );
            }
        }
    }
}

/// Claude Code's reply in words no reader names, headed by `⏺` so the screen names the prompt it answers ([`aterm_phase::answered_prompt`]).
fn claude_said(_: usize) -> Vec<String> {
    vec![
        "⏺ Allowance spent for now · back at 5:11pm (America/Los_Angeles)".to_owned(),
        String::new(),
        "✻ Cogitated for 0s · done 3:11 PM".to_owned(),
    ]
}

#[test]
fn a_persons_new_prompt_row_lifts_a_claude_hold_only_where_a_point_saw_it_busy() {
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    let rows = |asks: &[Option<&str>]| vendor_walled("claude", asks, claude_said);
    let reader = reader_of("claude");
    for asks in [&[None][..], &[None, None, Some("hi")]] {
        let r = rows(asks);
        assert!(
            reader.wall(&r).is_none() && reader.health_wall(&r).is_none(),
            "no reader names the wall: {r:#?}"
        );
        assert_eq!(
            aterm_phase::answered_prompt(&r).as_deref(),
            asks.last().map(|a| a.unwrap_or(ASK)),
            "the screen names the prompt answered: {r:#?}"
        );
    }
    let stood = rows(&[None, None]);
    let scrolled = stood[up(&stood)..].to_vec();
    let hi = rows(&[None, None, Some("hi")]);
    let (again, resumed) = (
        rows(&[None, None, Some("hi"), Some("still there?")]),
        rows(&[None, None, Some("hi"), Some("still there?"), None]),
    );
    let t = 2 * 86_400;
    let keys = |typed: i64, rows: &[String], at: i64, took: Option<u64>| R {
        typed: Some(typed),
        ..r(rows, at, took)
    };
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("claude-prompt-row-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d =
            Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false).uncounted();
        let out: Vec<Out> = [
            r(&rows(&[]), 0, Some(600)),
            r(&rows(&[None]), 60, Some(3)),
            r(&rows(&[None]), 300, None),
            r(&stood, 360, Some(3)),
            r(&stood, t, None),
            // Keys over the conversation scrolled a row: no new prompt row.
            keys(t + 30, &scrolled, t + 60, None),
            r(&scrolled, t + 3600, None),
            // The person's `hi`, its busy spell unseen and its Enter uncounted: its prompt row alone shows it.
            keys(t + 3660, &hi, t + 3720, None),
            r(&hi, t + 7200, None),
            // Their next prompt, its busy spell seen.
            keys(t + 10_740, &again, t + 10_800, Some(2)),
            r(&again, t + 18_000, None),
            r(&resumed, t + 18_060, Some(3)),
        ]
        .iter()
        .map(|p| d.point(p))
        .collect();
        let _ = std::fs::remove_dir_all(dir);
        let acts: Vec<&str> = out.iter().map(|o| o.act.as_str()).collect();
        assert!(
            acts[0].starts_with("act ") && acts[1] == "wait" && acts[2].starts_with("act "),
            "{tag}: {out:#?}"
        );
        assert!(
            acts[5..9].iter().all(|a| !a.starts_with("act "))
                && [3, 4, 6, 8]
                    .iter()
                    .all(|&i| acts[i] == "nothing" && out[i].told == held),
            "{tag}: held, keys and a new prompt row no point saw busy included: {out:#?}"
        );
        assert!(
            acts[9..11].iter().filter(|a| a.starts_with("act ")).count() == 1
                && acts[11] == "nothing"
                && out[11].told == held,
            "{tag}: the prompt a point saw busy lets one more act go, then held again: {out:#?}"
        );
    }
}

/// Claude Code's walled turn of [`vendor_walled`], then each of `opens` under it answered by [`claude_wall`]: the harness's continuation (`None`), a person's prompt, or a Monitor's event opening the program's own turn, as 2.1.302 draws it (`Some("")`, the same event each time; `Some("+")`, the `k`th open's own step).
fn self_started(opens: &[Option<&str>]) -> Vec<String> {
    let base = vendor_walled("claude", &[], claude_wall);
    let composer = base
        .iter()
        .position(|r| r.starts_with("────"))
        .expect("the composer");
    let mut out = base[..composer].to_vec();
    for (k, open) in opens.iter().enumerate() {
        match open {
            Some("") => out.push("⏺ Monitor event: build step 3 done · build watch".to_owned()),
            Some("+") => out.push(format!(
                "⏺ Monitor event: build step {} done · build watch",
                k + 3
            )),
            open => out.extend([format!("❯ {}", open.unwrap_or(ASK)), String::new()]),
        }
        out.extend(claude_wall(0));
        out.push(String::new());
    }
    out.extend(base[composer..].iter().cloned());
    out
}

/// A Claude Code loop at [`self_started`]'s wall, each act answered by the wall again: once held, the program opens `own` turns itself an hour apart (`events`, each a Monitor's event, `Some("+")` one of its own; else its own retry, the screen drawn as it stood), each answered by the wall in `took` seconds (`None`, a busy spell the loop never saw), then a person types `hi`. The acts typed, and what the last point told.
fn own_turns(
    tag: &str,
    autopilot: Autopilot,
    events: Option<&'static str>,
    own: usize,
    took_own: Option<u64>,
) -> (Vec<String>, Option<String>) {
    let (dir, ledger) = ledger_at(tag);
    let mut m = Mock::new(vec![busy_screen()]);
    let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
    let (mut opens, mut at, mut took, mut typed) = (Vec::new(), 0, Some(600), None);
    let (mut out, mut turns, mut person) = (Vec::new(), 0, false);
    while out.len() < 24 {
        let o = d.point(&R {
            entered: typed.is_some(),
            typed: typed.take(),
            ..r(&self_started(&opens), at, took.take())
        });
        match (&o.act[..], o.until) {
            ("wait", Some(u)) => at = u - CAPTURED + 1,
            ("nothing", _) if turns < own => {
                turns += 1;
                at += 3600;
                if let Some(event) = events {
                    opens.push(Some(event));
                }
                took = took_own;
            }
            ("nothing", _) if !person => {
                person = true;
                at += 3600;
                typed = Some(at - 60);
                opens.push(Some("hi"));
                took = Some(2);
            }
            ("nothing", _) => at += 86_400,
            _ => {
                at += 60;
                opens.push(None);
                took = Some(3);
            }
        }
        out.push(o);
    }
    let _ = std::fs::remove_dir_all(dir);
    let acts = out
        .iter()
        .map(|o| o.act.clone())
        .filter(|a| a.starts_with("act "))
        .collect();
    (acts, out.last().and_then(|o| o.told.clone()))
}

#[test]
fn a_no_work_turn_the_program_opens_itself_counts_as_an_acts_answer() {
    use crate::supervise::policy::turn_end::NO_WORK_ACTS;
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("self-started-{autopilot:?}");
        let (bare, told) = own_turns(&tag, autopilot, None, 0, Some(3));
        assert_eq!(
            bare.len(),
            3,
            "{tag}: two acts to the hold, the person's one more: {bare:?}"
        );
        assert_eq!(told, held, "{tag}: and its answer holds again");
        // The program's own retry says the reply again: the hold stands.
        for own in [1, 4] {
            let (acts, told) = own_turns(&tag, autopilot, None, own, Some(3));
            assert_eq!(acts, bare, "{tag}: {own} own retries keep the hold");
            assert_eq!(told, held, "{tag}");
        }
        // A Monitor's event says new words, so the reply changed, but each such turn counts into the run of no-work answers: however many come, the acts stop at the same count.
        let (four, told) = own_turns(&tag, autopilot, Some(""), 4, Some(3));
        assert_eq!(told, held, "{tag}: {four:?}");
        let (eight, told) = own_turns(&tag, autopilot, Some(""), 8, Some(3));
        assert_eq!(told, held, "{tag}: {eight:?}");
        assert!(
            four.len() == eight.len() && four.len() < NO_WORK_ACTS,
            "{tag}: the run holds the events' turns: {four:?} {eight:?}"
        );
        // Each event in words of its own, and each turn one whose busy spell no point saw: the same count, and held.
        for (event, took) in [(Some("+"), Some(3)), (Some(""), None), (Some("+"), None)] {
            let (ten, told) = own_turns(&tag, autopilot, event, 10, took);
            assert!(
                ten.len() == four.len() && told == held,
                "{tag}: {event:?} {took:?}: {ten:?} {told:?}"
            );
        }
    }
}

/// A Claude Code worker waiting on a background suite: `acts` continuations answered by the same words, then `events` Monitor events it answers itself, then `done`, the row that opens its report.
fn waiting_on_suite(acts: usize, events: usize, done: Option<&str>) -> Vec<String> {
    let base = vendor_walled("claude", &vec![None; acts], |_| {
        [
            "⏺ Still waiting on the background suite.",
            "",
            "✻ Churned for 3s",
        ]
        .map(str::to_owned)
        .to_vec()
    });
    let composer = base
        .iter()
        .position(|r| r.starts_with("────"))
        .expect("the composer");
    let mut out = base[..composer].to_vec();
    for e in 3..3 + events {
        out.extend([
            format!("⏺ Monitor event: suite step {e} passed · suite watch"),
            String::new(),
            format!("⏺ Step {e} passed; waiting for the rest."),
            String::new(),
            "✻ Churned for 2s".to_owned(),
            String::new(),
        ]);
    }
    if let Some(done) = done {
        out.extend(
            [
                done,
                "",
                "⏺ The suite passed: 412 of 412.",
                "",
                "✻ Churned for 2s",
                "",
            ]
            .map(str::to_owned),
        );
    }
    out.extend(base[composer..].iter().cloned());
    out
}

#[test]
fn a_background_tasks_end_after_monitor_events_is_work() {
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::NoWork).why
    ));
    // As claude-2.1.304-verbose-background-command-login-words.txt draws it, then as 2.1.268's notice builders draw a failure and a monitor's end; the control, in words.
    let ends = [
        "⏺ Background command \"Run the division suite\" completed (exit code 0)",
        "⏺ Background command \"Run the division suite\" failed with exit code 1",
        "⏺ Monitor \"suite watch\" stream ended",
    ];
    let words = "⏺ Background command \"Run the division suite\" is done.";
    for (done, events, autopilot) in ends.into_iter().chain([words]).flat_map(|done| {
        [0, 2, 4]
            .into_iter()
            .flat_map(move |e| [Autopilot::Cruise, Autopilot::Land].map(move |a| (done, e, a)))
    }) {
        let tag = format!("bg-done-{autopilot:?}-{events}-{}", done.len());
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
        let (mut acts, mut at, mut took) = (0, 0, Some(600));
        // The continuations, to the hold.
        loop {
            let o = d.point(&r(&waiting_on_suite(acts, 0, None), at, took.take()));
            match (&o.act[..], o.until) {
                ("wait", Some(u)) => at = u - CAPTURED + 1,
                ("nothing", _) => break,
                _ => (acts, at, took) = (acts + 1, at + 60, Some(3)),
            }
            assert!(acts <= 2, "{tag}: the same words hold: {o:?}");
        }
        for e in 1..=events {
            at += 600;
            d.point(&r(&waiting_on_suite(acts, e, None), at, Some(2)));
        }
        at += 600;
        let report = waiting_on_suite(acts, events, Some(done));
        let mut o = d.point(&r(&report, at, Some(2)));
        if let ("wait", Some(u)) = (&o.act[..], o.until) {
            o = d.point(&r(&report, u - CAPTURED + 1, None));
        }
        let _ = std::fs::remove_dir_all(dir);
        if done != words {
            assert!(
                o.act.starts_with("act ") && o.told.is_none(),
                "{tag}: the task's end is work, so its report is answered: {o:?}"
            );
        } else if events > 2 {
            assert!(
                o.act == "nothing" && o.told == held,
                "{tag}: in words it is the run's next no-work turn: {o:?}"
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
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
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
    // The person's turn timed or not: a point sees its busy spell, or only what it drew.
    for (autopilot, took_hi) in [Autopilot::Cruise, Autopilot::Land]
        .into_iter()
        .flat_map(|a| [Some(2), None].map(|t| (a, t)))
    {
        let tag = format!("stood-limit-{autopilot:?}-{took_hi:?}");
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
        let wall = muse_held(answers);
        let then = [
            d.point(&r(&wall.rows, at, None)),
            // A person's keys that drew no turn, read an hour on.
            d.point(&R {
                typed: Some(at + 30),
                ..r(&wall.rows, at + 3600, None)
            }),
        ];
        let hi = hi_at(&mut d, &wall, at + 3720, took_hi);
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(then[0].act, "nothing", "{tag}: {then:#?}");
        assert!(
            !then[1].act.starts_with("act "),
            "{tag}: keys alone: {then:#?}"
        );
        wall.judged(&tag, took_hi, &hi);

        // A loop taking over the held wall: the person's keys before its first point drew nothing, then their `hi`.
        let tag = format!("{tag}-takeover");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = stood(&mut d, 16);
        assert_eq!(out[out.len() - 1].told, futile, "{tag}: {out:#?}");
        let at = d.at + 86_400;
        let wall = muse_held(answers);
        let first = d.point(&r(&wall.rows, at, None));
        let submits = d.submits;
        drop(d);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(
            &mut m,
            "muse",
            &ledger,
            autopilot,
            (CAPTURED, at + 30),
            true,
        )
        .submitted(submits);
        let keys = d.point(&R {
            typed: Some(at + 30),
            ..r(&wall.rows, at + 3600, None)
        });
        let hi = hi_at(&mut d, &wall, at + 3720, took_hi);
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(first.act, "nothing", "{tag}: {first:#?}");
        assert!(
            !keys.act.starts_with("act "),
            "{tag}: keys alone at the new loop's first point: {keys:#?}"
        );
        wall.judged(&tag, took_hi, &hi);

        // A loop taking over at the person's `hi`, answered by the same wall.
        let tag = format!("{tag}-answered");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = stood(&mut d, 16);
        assert_eq!(out[out.len() - 1].told, futile, "{tag}: {out:#?}");
        let at = d.at + 86_400;
        let wall = muse_held(answers);
        let first = d.point(&r(&wall.rows, at, None));
        let submits = d.submits;
        drop(d);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(
            &mut m,
            "muse",
            &ledger,
            autopilot,
            (CAPTURED, at + 3660),
            true,
        )
        .submitted(submits);
        let hi = hi_at(&mut d, &wall, at + 3720, took_hi);
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(first.act, "nothing", "{tag}: {first:#?}");
        wall.judged(&tag, took_hi, &hi);
    }
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("stood-limit-{autopilot:?}");
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

/// `t` (unix) as the live tab's wall writes a clock, PDT (`5:11 PM`).
fn wall_clock(t: i64) -> String {
    let local = (t - 7 * 3600).rem_euclid(86_400);
    let (h, m) = (local / 3600, (local % 3600) / 60);
    let h12 = if h % 12 == 0 { 12 } else { h % 12 };
    format!("{h12}:{m:02} {}", if h < 12 { "AM" } else { "PM" })
}

/// [`before_hi`]'s wall with its reset a clock `lead` seconds past the time each of its answers was first drawn, and the answers to `asks` under it ([`answered`]): a reset moving forward with each answer.
struct Moving {
    lead: i64,
    resets: std::cell::RefCell<Vec<i64>>,
}

impl Moving {
    fn new(lead: i64) -> Self {
        Self {
            lead,
            resets: std::cell::RefCell::default(),
        }
    }

    /// The screen of `asks` answered at `at` (from [`CAPTURED`]), the wall's message `words`.
    fn rows(&self, asks: &[Option<&str>], at: i64, words: &str) -> Vec<String> {
        let mut resets = self.resets.borrow_mut();
        while resets.len() <= asks.len() {
            resets.push(CAPTURED + at + self.lead);
        }
        let reset = format!("  {words} {}", wall_clock(resets[asks.len()]));
        drop(resets);
        let rows: Vec<String> = before_hi()
            .into_iter()
            .map(|r| {
                if r == "  Your usage resets at Oct 25 at 5:00 PM" {
                    reset.clone()
                } else {
                    r
                }
            })
            .collect();
        answered(&rows, asks)
    }
}

impl Moving {
    /// [`Self::rows`] with every other answer's wall in Muse Code 1.4.10's measured one-line form ([`muse::USAGE_WALL`], its clock's `AM` or `PM` wrapped onto the next row at 144 columns).
    fn rows_by_turns(&self, asks: &[Option<&str>], at: i64) -> Vec<String> {
        let rows = self.rows(asks, at, "Your usage resets at");
        if asks.len().is_multiple_of(2) {
            return rows;
        }
        let wall = rows
            .iter()
            .rposition(|r| r.starts_with("  Your usage resets at "))
            .expect("the last answer's wall");
        let clock = rows[wall]["  Your usage resets at ".len()..].to_owned();
        let (h, ampm) = clock.split_once(' ').expect("a clock");
        let mut out = rows.clone();
        out[wall - 1] = format!(
            "◆ Usage limit reached · /upgrade (https://accountscenter.meta.com/muse_code/?ep=xgrade) for \
             increased limits, or wait for usage to reset at {h}"
        );
        out[wall] = format!("  {ampm}");
        out
    }
}

#[test]
fn two_wordings_of_a_wall_by_turns_at_resets_moving_forward_keep_their_tries() {
    let reader = aterm_phase::Program::Muse.reader();
    for lead in [180, 3600] {
        let wall = Moving::new(lead);
        let walls: Vec<_> = (0..2)
            .map(|k| {
                reader
                    .wall(&wall.rows_by_turns(&vec![None; k], 0))
                    .expect("a read wall")
            })
            .collect();
        assert!(
            walls
                .iter()
                .all(|w| w.kind == WallKind::UsageSession && w.reset.is_some())
                && walls[0].message != walls[1].message,
            "two wordings of one usage wall, each read with its clock: {walls:#?}"
        );
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("by-turns-{autopilot:?}-{lead}");
            let wall = Moving::new(lead);
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d =
                Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false).jittered();
            let first = r(&wall.rows_by_turns(&[], 0), 0, Some(600));
            let point = |k: usize, at: i64, took: Option<u64>| {
                r(&wall.rows_by_turns(&vec![None; k], at), at, took)
            };
            let (out, _) = follow_r(&mut d, &first, (60, 3), point, 30);
            let _ = std::fs::remove_dir_all(dir);
            let resumes = out.iter().filter(|o| o.act.starts_with("act ")).count();
            // Each wording keeps its own tries: the one that meets its last is held.
            assert!(
                resumes < 2 * FUTILE_AFTER_TRIES
                    && out[out.len() - 4..]
                        .iter()
                        .all(|o| o.act == "nothing" && is_futile(o)),
                "{tag}: {resumes} resumes: {out:#?}"
            );
        }
    }
}

/// The tab's exception is a futile one ([`Exception::limit_repeats`], or the limit standing an hour past its reset).
fn is_futile(o: &Out) -> bool {
    o.told.as_deref().is_some_and(|t| t.starts_with("futile "))
}

#[test]
fn a_reset_moving_forward_with_each_answer_keeps_the_walls_tries() {
    let futile = Some(format!(
        "futile {}",
        Exception::limit_repeats(aterm_phase::Program::Muse, true).why
    ));
    let reader = aterm_phase::Program::Muse.reader();
    let words = "Your usage resets at";
    // An hour's lead crosses midnight: the clock's `PM` turning `AM` is the same reply.
    for (lead, busy) in [(180, 3), (180, 400), (3600, 3)] {
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("moving-reset-{autopilot:?}-{lead}-{busy}");
            let wall = Moving::new(lead);
            let first = r(&wall.rows(&[], 0, words), 0, Some(600));
            let w = reader.wall(&first.rows).expect("a read wall");
            assert!(
                w.kind == WallKind::UsageSession
                    && w.reset
                        .as_deref()
                        .is_some_and(|r| r.ends_with(" PM") || r.ends_with(" AM")),
                "{tag}: a usage wall whose reset is a clock: {w:?}"
            );
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d =
                Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false).jittered();
            let point = |k: usize, at: i64, took: Option<u64>| {
                r(&wall.rows(&vec![None; k], at, words), at, took)
            };
            let (out, answers) = follow_r(&mut d, &first, (60, busy), point, 18);
            let resumes = out.iter().filter(|o| o.act.starts_with("act ")).count();
            assert_eq!(
                resumes, FUTILE_AFTER_TRIES,
                "{tag}: the wall's tries and no more: {out:#?}"
            );
            // Held where the last try's answer came, then a day at a time past its reset.
            let held_at = out.iter().position(|o| o.act == "nothing");
            assert!(
                held_at.is_some_and(|h| out[h].told == futile
                    && out[h..].iter().all(|o| o.act == "nothing" && is_futile(o))),
                "{tag}: held under the futile exception, its resets passing: {out:#?}"
            );
            // Loops that take over an hour apart read the reset the tries met from the ledger: held at once.
            let (dir3, ledger3) = ledger_at(&format!("{tag}-taken"));
            let mut m3 = Mock::new(vec![busy_screen()]);
            let mut d3 =
                Driver::new(&mut m3, "muse", &ledger3, autopilot, (CAPTURED, 0), false).jittered();
            let (to_hold, tried) = follow_r(
                &mut d3,
                &first,
                (60, busy),
                point,
                held_at.map_or(0, |h| h + 1),
            );
            assert!(to_hold.last().is_some_and(is_futile), "{tag}: {to_hold:#?}");
            let mut at = d3.at;
            for taken in 1..=3 {
                at += 3600;
                let mut m = Mock::new(vec![busy_screen()]);
                let mut d = Driver::new(&mut m, "muse", &ledger3, autopilot, (CAPTURED, at), true)
                    .jittered();
                let over = [
                    d.point(&point(tried, at, None)),
                    d.point(&point(tried, at + 600, None)),
                ];
                assert!(
                    over.iter().all(|o| o.act == "nothing" && o.told == futile),
                    "{tag}: loop {taken} takes over held: {over:#?}"
                );
                at += 600;
            }
            let _ = std::fs::remove_dir_all(dir3);
            // A changed wall message is a new reply: the hold lifts.
            let at = d.at + 600;
            let changed = d.point(&r(
                &wall.rows(&vec![None; answers], at, "Your plan's usage resets at"),
                at,
                None,
            ));
            assert_ne!(changed.act, "nothing", "{tag}: {changed:#?}");
            // A person's turn lifts it too: one more resume, then held again.
            let (dir2, ledger2) = ledger_at(&format!("{tag}-person"));
            let mut m2 = Mock::new(vec![busy_screen()]);
            let mut d2 = Driver::new(&mut m2, "muse", &ledger2, autopilot, (CAPTURED, 0), false);
            let (held, answers) = follow_r(&mut d2, &first, (60, busy), point, 18);
            assert!(held.last().is_some_and(is_futile), "{tag}: {held:#?}");
            let at = d2.at + 600;
            let mut asks = vec![None; answers];
            asks.push(Some("hi"));
            let hi = sent(at - 60, r(&wall.rows(&asks, at, words), at, Some(2)));
            let then = follow_r(
                &mut d2,
                &hi,
                (at + 60, busy),
                |k, at, took| {
                    let mut asks = vec![None; answers];
                    asks.push(Some("hi"));
                    asks.extend(vec![None; k]);
                    r(&wall.rows(&asks, at, words), at, took)
                },
                18,
            )
            .0;
            let _ = (std::fs::remove_dir_all(dir), std::fs::remove_dir_all(dir2));
            let more = then.iter().filter(|o| o.act.starts_with("act ")).count();
            assert!(
                more == 1
                    && then
                        .last()
                        .is_some_and(|o| o.act == "nothing" && is_futile(o)),
                "{tag}: a person's turn lets one more resume go, then it holds: {then:#?}"
            );
        }
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
                d.point(&sent(at, r(&hi, at + 60, Some(2)))),
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
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
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
fn a_529_first_met_as_a_continuations_answer_is_waited_out_on_its_ladder_never_held() {
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
        let tries = out
            .iter()
            .filter(|o| o.act == "act outage-resume@v1")
            .count();
        assert!(
            out[0].act.starts_with("act ") && out[0].act != "act outage-resume@v1",
            "{tag}: the continuation first: {out:#?}"
        );
        // An outage is a wait (v6 §14.17.5.2): a try at each rung of its ladder, the same answer
        // to each holding nothing and telling nobody.
        assert!(
            tries > FUTILE_AFTER_TRIES,
            "{tag}: the wall's tries go on past three: {out:#?}"
        );
        assert!(
            out.iter().all(|o| o.act != "nothing" && o.told.is_none()),
            "{tag}: never held: {out:#?}"
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
            .filter(|(_, o)| o.act == "act outage-resume@v1")
            .nth(2)
            .map(|(k, _)| k)
            .unwrap_or_else(|| panic!("{tag}: the third try typed: {out:#?}"));
        let next = &out[third + 1];
        assert!(
            next.act.starts_with("act ")
                && next.act != "act outage-resume@v1"
                && next.told.is_none(),
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
    // The 2026-09-12 capture's and 2.1.283's hint-less rows, then 2.1.268's renderer's lone agent by its description and by its count.
    for group in [
        "  Ran 2 shell commands",
        "  Listed 1 directory",
        "⏺ Reviewed the diff (ctrl+o to expand)",
        "⏺ Ran 1 agent (ctrl+o to expand)",
    ] {
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
fn a_git_only_answer_that_says_the_same_is_work_each_time() {
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    let reader = aterm_phase::Program::Claude.reader();
    // wait_bg17.out's measured row and 2.1.268's renderer's (its shell count leaves git commands out), then the shell count's.
    for group in [
        "  Pushed to main, rebased onto origin/main",
        "  Committed 1a2b3c4, pushed to main",
        "  Ran 2 shell commands",
    ] {
        let rows = build_fails(group);
        assert!(reader.turn_ran_tools(&rows(2)), "{group:?} is read");
        for (autopilot, busy) in [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [30, 550].map(|b| (a, b)))
        {
            let tag = format!("git-work-{autopilot:?}-{busy}-{}", group.len());
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, busy), &rows, 10);
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                answers > 2 && out.iter().all(|o| o.act != "nothing" && o.told.is_none()),
                "{tag}: {group:?} is work, every act typed: {out:#?}"
            );
        }
    }
    // The control: the same answer with the row in words, a lone plain ref's included, is held.
    for (autopilot, busy, words) in [Autopilot::Cruise, Autopilot::Land]
        .into_iter()
        .flat_map(|a| [30, 550].map(|b| (a, b)))
        .flat_map(|(a, b)| {
            [
                "  Pushed to main, then waited for the checks",
                "  Merged cleanly",
                "  Pushed to production",
            ]
            .map(|w| (a, b, w))
        })
    {
        let rows = build_fails(words);
        assert!(!reader.turn_ran_tools(&rows(2)), "{words:?} is words");
        let tag = format!("git-words-{autopilot:?}-{busy}-{}", words.len());
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, busy), &rows, 10);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            answers == 2
                && out
                    .last()
                    .is_some_and(|o| o.act == "nothing" && o.told == held),
            "{tag}: {out:#?}"
        );
    }
}

/// A Claude Code answer that ran no tool, its reply's last paragraph `paragraph`.
fn paragraph_ends(paragraph: &'static str) -> impl Fn(usize) -> Vec<String> {
    move |k: usize| {
        let mut said = vec!["⏺ I can't land this until it is reviewed.", ""];
        if paragraph.starts_with('⏺') {
            said.clear();
        }
        said.extend([paragraph, "", "✻ Worked for 3s"]);
        vendor_walled("claude", &vec![None; k], |_| {
            said.iter().map(|&r| r.to_owned()).collect()
        })
    }
}

#[test]
fn a_reply_ending_in_a_git_clauses_words_said_the_same_is_held() {
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    let reader = aterm_phase::Program::Claude.reader();
    for paragraph in [
        "  Waiting on PR #12",
        "  Pushed to main",
        "  Merged cleanly",
        "  Pushed to production",
        "⏺ Pushed to main",
    ] {
        let rows = paragraph_ends(paragraph);
        assert!(!reader.turn_ran_tools(&rows(2)), "{paragraph:?} is words");
        for autopilot in [Autopilot::Cruise, Autopilot::Land] {
            let tag = format!("git-paragraph-{autopilot:?}-{}", paragraph.len());
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, 3), &rows, 12);
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                answers == 2
                    && out
                        .last()
                        .is_some_and(|o| o.act == "nothing" && o.told == held),
                "{tag}: {paragraph:?} said the same holds after two acts: {out:#?}"
            );
        }
    }
}

/// A Claude Code answer that ran no other tool: the same words each turn, then `row(k)`, its `k`th answer's own group row, ending the turn.
fn landed_ends(row: fn(usize) -> String) -> impl Fn(usize) -> Vec<String> {
    move |k: usize| {
        vendor_walled("claude", &vec![None; k], |j| {
            vec![
                "⏺ Landed the parser fix.".to_owned(),
                String::new(),
                row(j),
                String::new(),
                "✻ Worked for 3s".to_owned(),
            ]
        })
    }
}

#[test]
fn a_reply_ending_in_its_commits_row_is_work_each_time() {
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    let reader = aterm_phase::Program::Claude.reader();
    // Each answer's commit is its own; its sha is what a sentence lacks, wherever the row stands, alone or before a push.
    let commits: [fn(usize) -> String; 2] = [
        |j| format!("  Committed {:07x}, pushed to main", 0x1a2b3c4 + 16 * j),
        |j| format!("  Committed {:07x}", 0x1a2b3c4 + 16 * j),
    ];
    let words = landed_ends(|_| "  Pushed to main".to_owned());
    assert!(
        !reader.turn_ran_tools(&words(2)),
        "the control's row is words"
    );
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        for (n, commit) in commits.into_iter().enumerate() {
            let rows = landed_ends(commit);
            assert!(reader.turn_ran_tools(&rows(2)), "the commit row is read");
            let tag = format!("commit-row-{autopilot:?}-{n}");
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
            let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, 3), &rows, 10);
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                answers > 2 && out.iter().all(|o| o.act != "nothing" && o.told.is_none()),
                "{tag}: each commit is work, every act typed: {out:#?}"
            );
        }
        // The control: the same answers ending in a lone `  Pushed to main` are held after two acts.
        let tag = format!("commit-words-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(&mut d, &r(&words(0), 0, Some(600)), (60, 3), &words, 12);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            answers == 2
                && out
                    .last()
                    .is_some_and(|o| o.act == "nothing" && o.told == held),
            "{tag}: {out:#?}"
        );
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
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::NoWork).why
    ));
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
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::NoWork).why
    ));
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
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
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

#[test]
fn keys_over_a_long_report_held_with_its_prompt_above_the_screen_lift_nothing() {
    let held = Some(format!(
        "stalled {}",
        Exception::repeats(crate::harness::acts::HeldBy::SameReply).why
    ));
    type Drawn = fn(bool) -> Box<dyn Fn(usize) -> Vec<String>>;
    let programs: [(&'static str, Drawn); 2] = [
        ("claude", |calls| Box::new(reported(calls))),
        ("muse", |calls| Box::new(muse_reported(calls))),
    ];
    for ((program, drawn), (autopilot, takeover)) in programs.into_iter().flat_map(|p| {
        [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [false, true].map(|k| (a, k)))
            .map(move |c| (p, c))
    }) {
        let tag = format!("report-keys-{program}-{autopilot:?}-{takeover}");
        let rows = drawn(false);
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, program, &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow_r(
            &mut d,
            &viewed(&rows(0), true, 0, Some(600)),
            (60, 550),
            |k, at, took| viewed(&rows(k), true, at, took),
            8,
        );
        // The held report's 40 rows a row up, the history served above them: its prompt above the screen either way.
        let full = rows(answers);
        let cut = full.len() - 40;
        let reader = reader_of(program);
        assert!(
            reader.turn_opens_above(&full[cut..]) && reader.turn_opens_above(&full[cut + 1..]),
            "{tag}: the turn opens above the screen on either read"
        );
        let scrolled = |at: i64| R {
            above: Some(full[..=cut].to_vec()),
            ..r(&full[cut + 1..], at, None)
        };
        let (at, submits) = (d.at, d.submits);
        let mut m2 = Mock::new(vec![busy_screen()]);
        // In a loop the books-turn arm holds the keys; a loop that takes over leaves keys typed after it started to the person, as main does.
        let mut d = if takeover {
            drop(d);
            Driver::new(
                &mut m2,
                program,
                &ledger,
                autopilot,
                (CAPTURED, at + 30),
                true,
            )
            .submitted(submits)
        } else {
            d
        };
        // The person's keys ending on an Enter over the empty composer, the screen a row up.
        let then = [
            d.point(&sent(at + 30, scrolled(at + 60))),
            d.point(&scrolled(at + 3600)),
            d.point(&scrolled(at + 7200)),
        ];
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            answers == 2
                && out[out.len() - 3..]
                    .iter()
                    .all(|o| o.act == "nothing" && o.told == held),
            "{tag}: held: {out:#?}"
        );
        assert!(
            then.iter().all(|o| !o.act.starts_with("act "))
                && then[1..].iter().all(|o| o.told == held),
            "{tag}: keys that submitted nothing lift nothing: {then:#?}"
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

/// agy's quota per day is a spent allowance, held once its tries are spent however its relative
/// reset moves; its quota per minute is a server's refusal, an outage waited out on its doubling
/// ladder and never held (v6 §14.17.5.2).
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
                if quota == "per-minute" {
                    assert!(
                        answers > 1 + FUTILE_AFTER_TRIES
                            && out[1..].iter().all(|o| o.told.is_none()
                                && matches!(&o.act[..], "wait" | "act outage-resume@v1")),
                        "{tag}: waited out on its ladder, never held: {out:#?}"
                    );
                    continue;
                }
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

/// The 500 and the 503 are outages, their answers counting for no hold (v6 §14.17.5.2): the
/// hold stands at the usage walls' answers between them, at the 429's own tries.
#[test]
fn read_walls_of_four_classes_by_turns_are_held_at_a_walls_own_tries_past_the_outages_between() {
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
        // The 429 answers the second act, then every fourth.
        assert_eq!(answers, 2 + 4 * (FUTILE_AFTER_TRIES - 1), "{tag}: {out:#?}");
        let last = &out[out.len() - 3..];
        assert!(
            last.iter().all(|o| o.act == "nothing" && o.told.is_some()),
            "{tag}: held, its exception told: {out:#?}"
        );
        // Every other answer is an outage's.
        assert_eq!(
            out.iter()
                .filter(|o| o.act == "act outage-resume@v1")
                .count(),
            answers / 2,
            "{tag}: each outage between resumed once on its rung: {out:#?}"
        );
    }
}

/// A refused request's notice as Claude Code draws it: a wall the agent can act on, its typed
/// retry and its hold kept (v6 §14.17.5.2).
fn claude_refused(_: usize) -> Vec<String> {
    [
        "⏺ API Error: 400 due to tool use concurrency issues. Run /rewind to recover the \
         conversation.",
        "",
        "✻ Worked for 3m 0s",
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Work scrolled above a wall: a 529 is an outage (v6 §14.17.5.2), waited out on its ladder and
/// never held, the turn read showing work above it or none; a refused request keeps its tries,
/// the work before each keeping them going, and is held as refused after them where the whole
/// turn read shows no call.
#[test]
fn work_scrolled_above_a_wall_keeps_its_tries_going_and_a_529_is_never_held() {
    let held = Some(format!("refused {}", Exception::refused(None, "").why));
    let cut = |calls: bool, wall: fn(usize) -> Vec<String>| {
        move |k: usize| {
            vendor_walled("claude", &vec![None; k], |j| {
                let mut v = report_rows(calls);
                v.push(String::new());
                v.extend(wall(j));
                v
            })
        }
    };
    let reader = aterm_phase::Program::Claude.reader();
    for (wall, kind) in [
        (claude_529 as fn(usize) -> Vec<String>, WallKind::Overloaded),
        (
            claude_refused,
            WallKind::ApiError {
                code: Some(400),
                retryable: false,
                cause: aterm_phase::ApiCause::Request,
            },
        ),
    ] {
        let full = cut(true, wall)(2);
        assert_eq!(
            reader.wall(&full[full.len() - 40..]).map(|w| w.kind),
            Some(kind)
        );
        let outage = kind == WallKind::Overloaded;
        for (calls, history) in [(true, true), (true, false), (false, true)] {
            for autopilot in [Autopilot::Cruise, Autopilot::Land] {
                let tag = format!("scrolled-{outage}-{calls}-{history}-{autopilot:?}");
                let rows = cut(calls, wall);
                let (dir, ledger) = ledger_at(&tag);
                let mut m = Mock::new(vec![busy_screen()]);
                let mut d = Driver::new(&mut m, "claude", &ledger, autopilot, (CAPTURED, 0), false);
                let (out, answers) = follow_r(
                    &mut d,
                    &viewed(&rows(0), history, 0, Some(600)),
                    (60, 550),
                    |k, at, took| viewed(&rows(k), history, at, took),
                    10,
                );
                let _ = std::fs::remove_dir_all(dir);
                if outage {
                    assert!(
                        answers > FUTILE_AFTER_TRIES
                            && out[1..].iter().all(|o| o.told.is_none()
                                && matches!(&o.act[..], "wait" | "act outage-resume@v1")),
                        "{tag}: {out:#?}"
                    );
                } else if calls {
                    assert!(
                        out.iter().all(|o| o.act != "nothing" && o.told.is_none()),
                        "{tag}: the work before each refusal keeps the tries going: {out:#?}"
                    );
                } else {
                    // The control: the whole turn read shows no call, so the wall's tries are its
                    // last.
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
}

/// A wall held for good and what a person's turn at it draws ([`held_wall`]).
struct Held {
    /// The screen the hold stands at.
    rows: Vec<String>,
    /// The person's `hi` answered by the wall, then the one act it lets go answered the same.
    hi: Vec<String>,
    resumed: Vec<String>,
    /// The act that one more turn lets go, where it is the wall's own, and the exception the hold stands under.
    act: Option<&'static str>,
    told: &'static str,
}

impl Held {
    /// `o` holds: nothing typed, under the hold's exception.
    fn holds(&self, o: &Out) -> bool {
        o.act == "nothing" && o.told.as_deref().is_some_and(|t| t.starts_with(self.told))
    }

    /// What the person's `hi` decided ([`hi_at`]): one more act, the wall's own where it has one, then held again, where a point saw it busy; held throughout where none did.
    fn judged(&self, tag: &str, took_hi: Option<u64>, out: &[Out]) {
        let acts: Vec<&str> = out[..2]
            .iter()
            .map(|o| o.act.as_str())
            .filter(|a| a.starts_with("act "))
            .collect();
        if took_hi.is_some() {
            assert!(
                acts.len() == 1 && self.act.is_none_or(|a| acts[0] == a) && self.holds(&out[2]),
                "{tag}: the person's `hi` a point saw busy lets one more act go, then held again: \
                 {out:#?}"
            );
        } else {
            assert!(
                acts.is_empty() && self.holds(&out[1]) && self.holds(&out[2]),
                "{tag}: the person's `hi` no point saw busy lifts nothing: {out:#?}"
            );
        }
    }
}

/// The person's `hi` with its Enter at `at`, read again two hours on, then the one act it let go answered the same, or the screen as it stood.
fn hi_at(d: &mut Driver<'_>, held: &Held, at: i64, took_hi: Option<u64>) -> Vec<Out> {
    let mut out = vec![
        d.point(&sent(at - 60, r(&held.hi, at, took_hi))),
        d.point(&r(&held.hi, at + 7200, None)),
    ];
    let last = if out.iter().any(|o| o.act.starts_with("act ")) {
        r(&held.resumed, at + 7260, Some(3))
    } else {
        r(&held.hi, at + 10_800, None)
    };
    out.push(d.point(&last));
    out
}

/// Muse Code's dated wall held past its reset after `answers` answers ([`stood`]).
fn muse_held(answers: usize) -> Held {
    let drawn = |hi: bool, more: usize| {
        let mut asks = vec![None; answers];
        asks.extend(hi.then_some(Some("hi")));
        asks.extend(vec![None; more]);
        answered(&before_hi(), &asks)
    };
    Held {
        rows: drawn(false, 0),
        hi: drawn(true, 0),
        resumed: drawn(true, 1),
        act: Some("act usage-resume@v1"),
        told: "futile ",
    }
}

/// A loop of `program`'s at a wall held for good: Muse Code's dated wall stood past its reset with its tries spent ([`stood`]), or a vendor row no reader names said again ([`vendor_walled`]), `claude-said` Claude Code's `⏺` reply in such words, under the prompt the screen names ([`claude_said`]).
fn held_wall<'a>(
    m: &'a mut Mock,
    program: &'static str,
    ledger: &Path,
    autopilot: Autopilot,
) -> (Driver<'a>, Held) {
    let (program, wall): (&'static str, WallRows) = match program {
        "claude" => ("claude", claude_wall),
        "claude-said" => ("claude", claude_said),
        "agy" => ("agy", agy_wall),
        "muse" => ("muse", claude_wall),
        other => panic!("no held wall for {other}"),
    };
    let mut d = Driver::new(m, program, ledger, autopilot, (CAPTURED, 0), false);
    if program == "muse" {
        let (_, answers) = stood(&mut d, 16);
        return (d, muse_held(answers));
    }
    let rows = |asks: &[Option<&str>]| vendor_walled(program, asks, wall);
    for p in [
        r(&rows(&[]), 0, Some(600)),
        r(&rows(&[None]), 60, Some(3)),
        r(&rows(&[None]), 300, None),
        r(&rows(&[None, None]), 360, Some(3)),
    ] {
        d.point(&p);
    }
    let held = Held {
        rows: rows(&[None, None]),
        hi: rows(&[None, None, Some("hi")]),
        resumed: rows(&[None, None, Some("hi"), None]),
        act: None,
        told: "stalled ",
    };
    (d, held)
}

/// How many top rows a scroll carries off to change the letters above the composer: through the first with any.
fn up(rows: &[String]) -> usize {
    rows.iter()
        .position(|r| r.chars().any(char::is_alphabetic))
        .expect("a row with letters")
        + 1
}

/// `rows` with a person's two-line draft in the composer, each row it wraps onto indented as Muse Code draws one (muse-1.4.10-R6427.1-draft-wrapped.txt): the composer taller, the conversation [`up`] rows up, and the hint agy and Gemini CLI blank while a draft stands gone (agy-1.4.0-draft.txt, gemini-0.46.0-draft.txt).
fn drafted(rows: &[String]) -> Vec<String> {
    let input = rows
        .iter()
        .rposition(|r| r.trim_start().starts_with(['❯', '>']))
        .expect("the composer's row");
    let row = &rows[input];
    let lead = &row[..row.len() - row.trim_start().len()];
    let marker = row.trim_start().chars().next().unwrap_or('>');
    let mut out = rows[up(rows)..input].to_vec();
    out.push(format!(
        "{lead}{marker} when the limit lifts, please rerun the division suite on the lane fix and then"
    ));
    out.push(format!("{lead}  report the number"));
    out.extend(rows[input + 1..].iter().cloned());
    out.iter()
        .map(|r| r.replace("? for shortcuts", &" ".repeat(15)))
        .collect()
}

/// What a person's keys at a held wall draw ([`person_at`]).
#[derive(Debug, Clone, Copy)]
enum Keys {
    /// Keys that submit nothing over the conversation scrolled up.
    Scroll,
    /// The same keys ending on an Enter over the empty composer (after an arrow or an Escape): the session's submits one up.
    ScrollEntered,
    /// Keys into a two-line draft that grows the composer ([`drafted`]).
    Draft,
    /// The same draft, its keys ending on an Enter over them that left it standing (a `\`+Enter newline, a menu's completion): the session's submits one up.
    Entered,
    /// The same draft, the keys that clear it ending on an Enter over the emptied composer: the session's submits one up.
    Cleared,
}

/// A person at `held` from `at` on: `keys`, more keys that clear what they drew, then their `hi` with its Enter ([`hi_at`]).
fn person_at(
    d: &mut Driver<'_>,
    held: &Held,
    at: i64,
    took_hi: Option<u64>,
    keys: Keys,
) -> Vec<Out> {
    let (keyed, cleared) = match keys {
        Keys::Scroll | Keys::ScrollEntered => {
            let scrolled = held.rows[up(&held.rows)..].to_vec();
            (scrolled.clone(), scrolled)
        }
        Keys::Draft | Keys::Entered | Keys::Cleared => (drafted(&held.rows), held.rows.clone()),
    };
    let mut out: Vec<Out> = [
        R {
            typed: Some(at + 30),
            entered: matches!(keys, Keys::ScrollEntered | Keys::Entered),
            ..r(&keyed, at + 60, None)
        },
        r(&keyed, at + 3600, None),
        R {
            typed: Some(at + 3630),
            entered: matches!(keys, Keys::Cleared),
            ..r(&cleared, at + 3660, None)
        },
        r(&cleared, at + 7200, None),
    ]
    .iter()
    .map(|p| d.point(p))
    .collect();
    out.extend(hi_at(d, held, at + 10_800, took_hi));
    out
}

#[test]
fn a_person_at_a_held_wall_lifts_its_hold_only_by_a_turn_a_point_saw_busy() {
    for program in ["muse", "claude", "claude-said", "agy"] {
        for (autopilot, took_hi, takeover, keys) in [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [Some(2), None].map(|t| (a, t)))
            .flat_map(|(a, t)| [false, true].map(|k| (a, t, k)))
            .flat_map(|(a, t, k)| {
                [
                    Keys::Scroll,
                    Keys::ScrollEntered,
                    Keys::Draft,
                    Keys::Entered,
                    Keys::Cleared,
                ]
                .map(|f| (a, t, k, f))
            })
        {
            let tag = format!("person-at-{program}-{autopilot:?}-{took_hi:?}-{takeover}-{keys:?}");
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let (mut d, held) = held_wall(&mut m, program, &ledger, autopilot);
            let mark = |rows: &[String]| {
                let read = reader_of(d.program).read(rows, None);
                crate::supervise::policy::turn_end::conversation_mark(&read, rows)
            };
            let stands = mark(&held.rows);
            assert!(
                mark(&held.rows[up(&held.rows)..]) != stands
                    && mark(&drafted(&held.rows)) != stands,
                "{tag}: a scroll or a draft changes the letters above the composer"
            );
            let at = d.at + 86_400;
            let first = d.point(&r(&held.rows, at, None));
            assert!(held.holds(&first), "{tag}: {first:#?}");
            let out = if takeover {
                let (submits, program) = (d.submits, d.program);
                drop(d);
                let mut m = Mock::new(vec![busy_screen()]);
                let mut d = Driver::new(
                    &mut m,
                    program,
                    &ledger,
                    autopilot,
                    (CAPTURED, at + 30),
                    true,
                )
                .submitted(submits);
                person_at(&mut d, &held, at, took_hi, keys)
            } else {
                person_at(&mut d, &held, at, took_hi, keys)
            };
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                out[..4].iter().all(|o| !o.act.starts_with("act ")),
                "{tag}: keys that submit no turn type nothing, a draft standing never submitted: \
                 {out:#?}"
            );
            assert!(
                held.holds(&out[1]) && held.holds(&out[3]),
                "{tag}: held past the person's grace: {out:#?}"
            );
            held.judged(&tag, took_hi, &out[4..]);
        }
    }
}

/// `rows` with the `/usage` probe's inline rows over the person's `hi`, or the composer's top rule where none stands ([`aterm_phase::prompt::fixtures::USAGE_DISMISSED_INLINE_2_1_307`]: `❯ /usage` and the dismissal under it).
fn probed_over(rows: &[String]) -> Vec<String> {
    let inline = screen(aterm_phase::prompt::fixtures::USAGE_DISMISSED_INLINE_2_1_307);
    let probe = inline
        .iter()
        .position(|r| r == "❯ /usage")
        .expect("the probe's row");
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("the composer");
    let under = rows.iter().position(|r| r == "❯ hi").unwrap_or(caret - 1);
    let mut out = rows[..under].to_vec();
    out.extend(inline[probe..probe + 2].iter().cloned());
    out.push(String::new());
    out.extend(rows[under..].iter().cloned());
    out
}

#[test]
fn the_harnesss_own_enter_at_a_held_claude_wall_is_no_persons_turn() {
    use aterm_phase::prompt::fixtures::{
        USAGE_DISMISSED_FULLSCREEN_2_1_307 as FULLSCREEN, USAGE_DISMISSED_INLINE_2_1_307 as INLINE,
    };
    let probe_row = |rows: Vec<String>| rows.iter().any(|r| r == "❯ /usage");
    assert!(
        !probe_row(screen(FULLSCREEN)) && probe_row(screen(INLINE)),
        "the measured probes: fullscreen draws no row of its own, inline its `❯ /usage`"
    );
    // Fullscreen, the renderer the probe runs in, leaves the conversation as it stood (claude-2.1.307-usage-dismissed-fullscreen.txt); inline draws the probe's rows, its own message, as a host's step may draw its own.
    for (autopilot, took_hi, (inline, host), (takeover, entered)) in
        [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [Some(2), None].map(|t| (a, t)))
            .flat_map(|(a, t)| [(false, false), (true, false), (true, true)].map(|o| (a, t, o)))
            .flat_map(|(a, t, o)| {
                [(false, false), (false, true), (true, false), (true, true)].map(|k| (a, t, o, k))
            })
    {
        let tag = format!("probed-{autopilot:?}-{took_hi:?}-{inline}-{host}-{takeover}-{entered}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut d, mut held) = held_wall(&mut m, "claude", &ledger, autopilot);
        let at = d.at + 86_400;
        let first = d.point(&r(&held.rows, at, None));
        if host {
            d.s.turn_end.host_moved();
            d.submits += 1;
        } else {
            d.probed(at + 60);
        }
        if inline {
            held = Held {
                rows: probed_over(&held.rows),
                hi: probed_over(&held.hi),
                resumed: probed_over(&held.resumed),
                ..held
            };
        }
        let mut out: Vec<Out> = [
            r(&held.rows, at + 120, None),
            r(&held.rows, at + 3600, None),
        ]
        .iter()
        .map(|p| d.point(p))
        .collect();
        // The loop goes on, or one takes over past the ledger's last point row; then a person's stray key, or keys ending on an Enter over the empty composer.
        let (submits, at_keys) = (d.submits, at + 3630);
        let mut m2 = Mock::new(vec![busy_screen()]);
        let mut d = if takeover {
            drop(d);
            Driver::new(
                &mut m2,
                "claude",
                &ledger,
                autopilot,
                (CAPTURED, at_keys),
                true,
            )
            .submitted(submits)
        } else {
            d
        };
        out.extend(
            [
                R {
                    typed: Some(at_keys),
                    entered,
                    ..r(&held.rows, at + 7200, None)
                },
                r(&held.rows, at + 10_800, None),
            ]
            .iter()
            .map(|p| d.point(p)),
        );
        out.extend(hi_at(&mut d, &held, at + 18_000, took_hi));
        let _ = std::fs::remove_dir_all(dir);
        assert!(held.holds(&first), "{tag}: {first:#?}");
        assert!(
            out[..4].iter().all(|o| !o.act.starts_with("act "))
                && [0, 1, 3].iter().all(|&i| held.holds(&out[i])),
            "{tag}: the harness's Enter, what it drew and keys that submitted nothing are no \
             person's turn: {out:#?}"
        );
        held.judged(&tag, took_hi, &out[4..]);
    }
}

#[test]
fn keys_over_a_claude_hold_scrolled_past_its_prompt_lift_nothing_whatever_history_is_served() {
    let reader = reader_of("claude");
    let stood = vendor_walled("claude", &[None, None], claude_said);
    // The cuts that carry the last prompt row off the top and leave the reply on the screen: a turn opening above it, whose tools only a history read shows.
    let cuts: Vec<usize> = (1..stood.len())
        .filter(|&k| {
            reader.turn_opens_above(&stood[k..])
                && stood[k..]
                    .iter()
                    .any(|r| r.starts_with("⏺ Allowance spent"))
        })
        .collect();
    assert!(
        !cuts.is_empty(),
        "a cut past the prompt keeps the reply: {stood:#?}"
    );
    for (k, autopilot, takeover, history) in cuts.iter().flat_map(|&k| {
        [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [false, true].map(|t| (a, t)))
            .flat_map(move |(a, t)| [false, true].map(|h| (k, a, t, h)))
    }) {
        let tag = format!("past-prompt-{k}-{autopilot:?}-{takeover}-{history}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut d, held) = held_wall(&mut m, "claude-said", &ledger, autopilot);
        assert_eq!(held.rows, stood, "{tag}");
        let at = d.at + 86_400;
        let first = d.point(&r(&held.rows, at, None));
        let cut = |t: i64| R {
            above: history.then(|| held.rows[..k].to_vec()),
            ..r(&held.rows[k..], t, None)
        };
        let (submits, program) = (d.submits, d.program);
        let mut m2 = Mock::new(vec![busy_screen()]);
        let mut d = if takeover {
            drop(d);
            Driver::new(
                &mut m2,
                program,
                &ledger,
                autopilot,
                (CAPTURED, at + 20),
                true,
            )
            .submitted(submits)
        } else {
            d
        };
        let mut out = Vec::new();
        if takeover {
            out.push(d.point(&r(&held.rows, at + 25, None)));
        }
        // The person's keys ending on an Enter over the empty composer, the screen redrawn past the prompt.
        out.extend([
            d.point(&sent(at + 30, cut(at + 3600))),
            d.point(&cut(at + 7200)),
            d.point(&cut(at + 10_800)),
        ]);
        let _ = std::fs::remove_dir_all(dir);
        assert!(held.holds(&first), "{tag}: {first:#?}");
        let n = out.len();
        assert!(
            out.iter().all(|o| !o.act.starts_with("act "))
                && out[n - 2..].iter().all(|o| held.holds(o)),
            "{tag}: keys that submitted no turn lift nothing, though no tool read was made: \
             {out:#?}"
        );
    }
}

#[test]
fn keys_over_a_taller_pane_lift_nothing_at_a_takeover() {
    for program in ["muse", "claude", "agy"] {
        for (autopilot, entered) in [Autopilot::Cruise, Autopilot::Land]
            .into_iter()
            .flat_map(|a| [false, true].map(|e| (a, e)))
        {
            let tag = format!("taller-pane-{program}-{autopilot:?}-{entered}");
            let (dir, ledger) = ledger_at(&tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let (mut d, held) = held_wall(&mut m, program, &ledger, autopilot);
            let at = d.at + 86_400;
            let first = d.point(&r(&held.rows, at, None));
            // The pane grows a row: one more row of the history shows above the conversation, and no point moves.
            let taller: Vec<String> =
                std::iter::once("Earlier: the lane fix landed on its branch.")
                    .map(str::to_owned)
                    .chain(held.rows.iter().cloned())
                    .collect();
            let grown = d.point(&r(&taller, at + 600, None));
            let submits = d.submits;
            drop(d);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(
                &mut m,
                program,
                &ledger,
                autopilot,
                (CAPTURED, at + 630),
                true,
            )
            .submitted(submits);
            // The person's keys, ending on an Enter over the empty composer where `entered`.
            let then = [
                d.point(&R {
                    typed: Some(at + 630),
                    entered,
                    ..r(&taller, at + 3600, None)
                }),
                d.point(&r(&taller, at + 7200, None)),
            ];
            let _ = std::fs::remove_dir_all(dir);
            assert!(
                held.holds(&first) && held.holds(&grown),
                "{tag}: {first:#?} {grown:#?}"
            );
            assert!(
                !then[0].act.starts_with("act ") && held.holds(&then[1]),
                "{tag}: keys that submitted nothing at the new loop's first point lift nothing: \
                 {then:#?}"
            );
        }
    }
}

/// A Claude Code session's `k`th answer: `runs` runs of its suites (at most two), then `last` at each answer after.
fn suites_then(runs: usize, last: &'static str) -> impl Fn(usize) -> Vec<String> {
    move |k: usize| match k {
        _ if k < runs => {
            let (lane, said, took) = [
                (
                    "parser",
                    "The parser tests pass; the lexer is next.",
                    "3m 10s",
                ),
                (
                    "lexer",
                    "The lexer tests pass too; shall I wire them together?",
                    "40s",
                ),
            ][k];
            vec![
                format!("⏺ Bash(targo --unverified test -p {lane} 2>&1 | tail -3)"),
                "  ⎿  test result: ok. 12 passed".to_owned(),
                String::new(),
                format!("⏺ {said}"),
                String::new(),
                format!("✻ Worked for {took}"),
            ]
        }
        _ => vec![last.to_owned(), String::new(), "✻ Worked for 2s".to_owned()],
    }
}

#[test]
fn a_persons_message_said_again_no_point_saw_busy_is_judged_as_one_in_new_words() {
    for (autopilot, runs) in [Autopilot::Cruise, Autopilot::Land]
        .into_iter()
        .flat_map(|a| [2, 1].map(|n| (a, n)))
    {
        for (name, last) in [
            ("done", "⏺ All done: nothing is left to do here."),
            (
                "landed",
                "⏺ Nothing left: the parser and lexer are merged to main and pushed.",
            ),
            ("plain", "⏺ The lexer is wired in; the parser reads it now."),
        ] {
            let rows =
                |asks: &[Option<&str>]| vendor_walled("claude", asks, suites_then(runs, last));
            // The person's `continue` a point saw busy, then their `second` answered by `last` while no point looked; with one run, the harness's continuation and both of theirs answered the same.
            let decided = |second: &str| {
                let fin = rows(&[None, Some("continue"), Some(second)]);
                let points = [
                    r(&rows(&[]), 0, Some(600)),
                    r(&rows(&[None]), 60, Some(190)),
                    sent(100, r(&rows(&[None, Some("continue")]), 160, Some(40))),
                    sent(200, r(&fin, 230, None)),
                    r(&fin, 800, None),
                    r(&fin, 2000, None),
                    r(&fin, 3800, None),
                    r(&fin, 8000, None),
                ];
                let (dir, ledger) =
                    ledger_at(&format!("said-again-{autopilot:?}-{runs}-{name}-{second}"));
                let out = replay_as("claude", &ledger, autopilot, CAPTURED, false, &points);
                let _ = std::fs::remove_dir_all(dir);
                out
            };
            let tag = format!("said-again-{autopilot:?}-{runs}-{name}");
            let (again, other) = (decided("continue"), decided("continue please"));
            assert_eq!(
                again, other,
                "{tag}: the same message said again is a turn as one in other words is, the \
                 short streak's back-off included"
            );
            let want = match (autopilot, name) {
                (Autopilot::Land, _) => "act land-nudge@v1",
                (_, "plain") => "act continue@v1",
                _ => "act continue-done-check@v1",
            };
            let acts: Vec<&str> = again.iter().map(|o| o.act.as_str()).collect();
            // The person's short exchange after the claim is left to them under Land; Cruise
            // carries it as any reply (owner ruling 2026-10-26).
            if runs == 1 && name != "plain" && autopilot == Autopilot::Land {
                assert!(
                    acts[3..].iter().all(|a| *a == "nothing"),
                    "{tag}: the person's short exchange after the claim is left to them: {again:#?}"
                );
            } else {
                assert!(
                    acts[3] == "wait" && acts[4] == want && acts[5..] == ["wait", "wait", want],
                    "{tag}: the reply answered after the short streak's back-off, by the prompt it \
                     gets: {again:#?}"
                );
            }
        }
    }
}

#[test]
fn a_lead_agents_prompt_lets_one_act_go_only_above_claude_codes_reply_with_no_person_at_the_keys() {
    const LEAD: &str = "Rerun the division suite on the lane fix and report the number.";
    for (program, autopilot, person, took) in ["claude-said", "agy"]
        .into_iter()
        .flat_map(|p| [Autopilot::Cruise, Autopilot::Land].map(|a| (p, a)))
        .flat_map(|(p, a)| [false, true].map(|k| (p, a, k)))
        .flat_map(|(p, a, k)| [None, Some(2)].map(|t| (p, a, k, t)))
    {
        let tag = format!("lead-at-{program}-{autopilot:?}-{person}-{took:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let (mut d, held) = held_wall(&mut m, program, &ledger, autopilot);
        let (drawn, wall): (&str, WallRows) = match program {
            "claude-said" => ("claude", claude_said),
            "agy" => ("agy", agy_wall),
            other => panic!("no held wall for {other}"),
        };
        let rows = |asks: &[Option<&str>]| vendor_walled(drawn, asks, wall);
        let (sent_rows, resumed) = (
            rows(&[None, None, Some(LEAD)]),
            rows(&[None, None, Some(LEAD), None]),
        );
        let at = d.at + 86_400;
        let first = d.point(&r(&held.rows, at, None));
        // The lead's `aterm ctl send`: its prompt row and Enter, the session's submits one up and no person's keys, or beside a person's stray key; a point saw its busy spell or none did.
        let mut out = vec![
            d.point(&R {
                typed: person.then_some(at + 30),
                entered: true,
                ..r(&sent_rows, at + 60, took)
            }),
            d.point(&r(&sent_rows, at + 7200, None)),
        ];
        let resumes = out.iter().any(|o| o.act.starts_with("act "));
        out.push(d.point(&if resumes {
            r(&resumed, at + 7260, Some(3))
        } else {
            r(&sent_rows, at + 10_800, None)
        }));
        let _ = std::fs::remove_dir_all(dir);
        assert!(held.holds(&first), "{tag}: {first:#?}");
        let acts = out[..2]
            .iter()
            .filter(|o| o.act.starts_with("act "))
            .count();
        // Claude Code's screen names the prompt its reply answers; the other programs' readers name none, so the lead's prompt and its answer are that program's own turn.
        let lifts = (program == "claude-said" && !person) || (person && took.is_some());
        assert!(
            acts == usize::from(lifts) && held.holds(&out[2]) && (lifts || held.holds(&out[1])),
            "{tag}: a lead's prompt above Claude Code's reply, or a person's turn a point saw \
             busy, lets one more act go, then held again; nothing else lifts it: {out:#?}"
        );
    }
}

#[test]
fn the_journal_names_the_holds_arm_and_only_a_holds() {
    use crate::harness::acts::HeldBy;
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, Some("@s-1".to_owned()));
    let mut noted = Noted::default();
    let told = [
        (
            Exception::repeats_land(HeldBy::SameReply, None),
            Some(HeldBy::SameReply),
        ),
        (Exception::repeats(HeldBy::NoWork), Some(HeldBy::NoWork)),
        (
            Exception::limit_repeats(aterm_phase::Program::Claude, true),
            None,
        ),
    ];
    for (seq, (exception, held)) in (1..).zip(told) {
        let head = format!(
            "EXCEPTION seq={seq} kind={} when={}{} ",
            exception.kind.word(),
            exception.when.word(),
            held.map_or_else(String::new, |arm| format!(" held={}", arm.word()))
        );
        s.exception_now = Some(exception);
        s.sync_exception(seq, false, &mut noted);
        let line = noted
            .0
            .iter()
            .rev()
            .find(|l| l.starts_with("EXCEPTION "))
            .unwrap_or_else(|| panic!("journaled: {:#?}", noted.0));
        assert!(line.starts_with(&head), "{head:?} in {line:?}");
    }
    let lines: Vec<&String> = noted
        .0
        .iter()
        .filter(|l| l.starts_with("EXCEPTION "))
        .collect();
    assert!(
        lines[0].contains(" when=now held=same-reply "),
        "{lines:#?}"
    );
    assert!(lines[1].contains(" when=later held=no-work "), "{lines:#?}");
    assert!(!lines[2].contains(" held="), "{lines:#?}");
}
