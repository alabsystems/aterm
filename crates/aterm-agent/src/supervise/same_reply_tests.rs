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

/// A HOST STEPPING AT A POINT IS TOLD THE STOP THE DECISION THERE READ
/// ([`IdlePoint::replies_held`]): under Land none while the loop still answers the reply, the stop
/// once it types nothing for it, so a carry-on owed there types nothing either. CONTROL: under Cruise
/// the same answers get another turn each time, and it is told of no stop. MUTANT: `replies_held:
/// false` at [`Session::host_steps_here`] fails under Land; read off [`TurnEndState::held_by`],
/// under Cruise.
#[test]
fn a_host_stepping_at_a_point_is_told_the_stop_the_decision_read() {
    #[derive(Debug, Default)]
    struct Told(Mutex<Vec<bool>>);
    impl IdleHost for Told {
        fn wants(&self) -> bool {
            true
        }
        fn at_idle(&self) -> Option<HostStep> {
            None
        }
        fn at_idle_on(&self, point: IdlePoint) -> Option<HostStep> {
            self.0.lock().unwrap().push(point.replies_held);
            None
        }
        fn owns_turn_end(&self) -> bool {
            false
        }
    }
    let told_along = |autopilot: Autopilot, tag: &str| {
        let (dir, ledger) = ledger_at(tag);
        let mut m = Mock::new(vec![busy_screen()]);
        m.program = "muse";
        let mut d = Driver::new(&mut m, "muse", &ledger, autopilot, (CAPTURED, 0), false);
        let mut told = Vec::new();
        let (mut answers, mut at, mut took) = (0, 0, Some(600));
        for _ in 0..24 {
            let p = r(&unnamed(&vec![None; answers]), at, took);
            let out = d.point(&p);
            let host = Arc::new(Told::default());
            let opts = SuperviseOpts {
                idle_host: Some(Arc::clone(&host) as Arc<dyn IdleHost>),
                ..d.opts.clone()
            };
            let mut point = d.s.turn_of(Screen {
                rows: p.rows.clone(),
                seq: d.seq,
                submits: Some(d.submits),
                ..Screen::default()
            });
            point.screen.cursor_row = aterm_phase::muse::composer(&p.rows).expect("its caret");
            // Standing for the wait's step, which offers the point the last step passed over.
            d.s.host_passed = None;
            let _ = d.s.host_steps_here(&point, &opts, &mut false, &mut Alone);
            let heard = host.0.lock().unwrap().clone();
            told.push((out.act.clone(), heard));
            (at, took) = match (&out.act[..], out.until) {
                ("wait", Some(until)) => (until - CAPTURED + 1, None),
                ("nothing", _) => break,
                _ => {
                    answers += 1;
                    (at + 60, Some(3))
                }
            };
        }
        let _ = std::fs::remove_dir_all(dir);
        told
    };
    let land = told_along(Autopilot::Land, "same-reply-told-land");
    let (last, before) = land.split_last().expect("a point");
    assert_eq!(last.0, "nothing", "the loop stops: {land:#?}");
    assert_eq!(
        last.1,
        [true],
        "the stop, as the decision read it: {land:#?}"
    );
    assert!(
        before.iter().flat_map(|(_, heard)| heard).any(|held| !held),
        "no stop before it, the host stepped: {land:#?}"
    );
    let cruise = told_along(Autopilot::Cruise, "same-reply-told-cruise");
    assert!(
        cruise.iter().flat_map(|(_, heard)| heard).all(|held| !held),
        "another turn each time: {cruise:#?}"
    );
    assert!(
        cruise.iter().any(|(_, heard)| !heard.is_empty()),
        "the host stepped: {cruise:#?}"
    );
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

/// Calls and report rows for transcript fixtures.
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

/// `rows` through their last 40 rows, the rows above them served as history (`history`) or none.
fn viewed(rows: &[String], history: bool, at: i64, worked: Option<u64>) -> R {
    let cut = rows.len().saturating_sub(40);
    R {
        above: history.then(|| rows[..cut].to_vec()),
        ..r(&rows[cut..], at, worked)
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
            // The person's short exchange after the claim is carried as any reply: Cruise never
            // parks (owner ruling 2026-10-26), and under Land the session worked before it (owner
            // ruling 2026-10-27).
            assert!(
                acts[3] == "wait" && acts[4] == want && acts[5..] == ["wait", "wait", want],
                "{tag}: the reply answered after the short streak's back-off, by the prompt it \
                 gets: {again:#?}"
            );
        }
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
            Exception::repeats(HeldBy::SameReply),
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
        lines[0].contains(" when=later held=same-reply "),
        "{lines:#?}"
    );
    assert!(lines[1].contains(" when=later held=no-work "), "{lines:#?}");
    assert!(!lines[2].contains(" held="), "{lines:#?}");
}

/// agy 1.4.12's subscription quota (`RESOURCE_EXHAUSTED (code 429): Individual quota reached. …
/// Resets in 4h29m33s.`, [`aterm_phase::agy::fixtures::lab::ERROR_429_INDIVIDUAL`]) answering
/// each act, its Error ID fresh: the loop waits out the reset it names and types again there,
/// never holding the session as the same reply twice. Read as no wall (m15, 2026-10-29), two
/// Cruise prompts held the session for two days past the reset.
#[test]
fn an_agy_subscription_quota_is_waited_out_to_its_reset_not_held() {
    use aterm_phase::agy::fixtures::lab::ERROR_429_INDIVIDUAL;
    let block = |k: usize| -> Vec<String> {
        let rows = screen(ERROR_429_INDIVIDUAL);
        let head = rows
            .iter()
            .position(|r| r.starts_with("⚠ agent executor error"))
            .expect("the quota block");
        let id = rows
            .iter()
            .position(|r| r.starts_with("Error ID: "))
            .expect("its Error ID");
        rows[head..=id]
            .iter()
            .map(|row| {
                if row.starts_with("Error ID: ") {
                    format!("Error ID: 6f2a1c0e-3b7d-4e9a-8c55-1d2e3f4a5b6c-{}", k + 1)
                } else {
                    row.clone()
                }
            })
            .collect()
    };
    let rows = |answers: usize| vendor_walled("agy", &vec![None; answers], block);
    let reader = aterm_phase::Program::Agy.reader();
    assert_eq!(
        reader.wall(&rows(1)).and_then(|w| w.reset).as_deref(),
        Some("in 16173s")
    );
    for autopilot in [Autopilot::Cruise, Autopilot::Land] {
        let tag = format!("agy-individual-quota-{autopilot:?}");
        let (dir, ledger) = ledger_at(&tag);
        let mut m = Mock::new(vec![busy_screen()]);
        let mut d = Driver::new(&mut m, "agy", &ledger, autopilot, (CAPTURED, 0), false);
        let (out, answers) = follow(&mut d, &r(&rows(0), 0, Some(600)), (60, 3), rows, 4);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            out[0].act.starts_with("act "),
            "{tag}: the act after the work: {out:#?}"
        );
        let wait = &out[1];
        assert_eq!(wait.act, "wait", "{tag}: the wall is waited out: {out:#?}");
        let until = wait.until.expect("a wait names its end") - CAPTURED;
        assert!(
            (16_000..=16_400).contains(&(until - 60)),
            "{tag}: to the reset it names, 16173 s on: {until} {out:#?}"
        );
        assert!(
            answers >= 2 && out[2].act.starts_with("act "),
            "{tag}: typed again at the reset: {out:#?}"
        );
        assert!(
            out.iter()
                .all(|o| o.told.as_deref().is_none_or(|t| !t.starts_with("stalled"))),
            "{tag}: never held: {out:#?}"
        );
    }
}

/// A DRAFT A PERSON TYPES AND CLEARS UNDER A KEPT REPLY IS NO POINT ON agy, AS ON Claude Code
/// (ATERM-KI-1101-2): its key is the turn's, so no observe moves the person's last key past it,
/// and the program's own no-work turn after it starts the kept reply again from one answer, alike
/// on both. NEGATIVE CONTROL: a point at the draft, as agy's key made one before, counts that turn
/// into the hold instead.
#[test]
fn a_draft_cleared_under_a_kept_reply_lifts_it_alike_on_agy_and_claude() {
    const SAID: &str = "Nothing is left to do here: the suite is green.";
    let said = |program: &str| -> fn(usize) -> Vec<String> {
        match program {
            "claude" => |_| {
                vec![
                    format!("⏺ {SAID}"),
                    String::new(),
                    "✻ Cogitated for 0s · done 3:11 PM".to_owned(),
                ]
            },
            _ => |_| vec![SAID.to_owned()],
        }
    };
    let mut lifted = Vec::new();
    for program in ["claude", "agy"] {
        let rows = |asks: &[Option<&str>]| vendor_walled(program, asks, said(program));
        let kept = rows(&[None, None]);
        let reader = match program {
            "claude" => aterm_phase::Program::Claude.reader(),
            _ => aterm_phase::Program::Agy.reader(),
        };
        let (caret, glyph) = match program {
            "claude" => (aterm_phase::phase::composer_index(&kept), "❯"),
            _ => (aterm_phase::agy::composer(&kept), ">"),
        };
        let mut drafted = kept.clone();
        drafted[caret.expect("the caret")] = format!("{glyph} a note for later");
        let turn = |rows: &[String]| Turn {
            phase: reader.phase(rows),
            screen: Screen {
                rows: rows.to_vec(),
                submits: Some(3),
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(
            point_key(reader, &turn(&kept)),
            point_key(reader, &turn(&drafted)),
            "{program}: the draft is no point"
        );
        let run = |tag: &str, draft_point: bool| {
            let mut points = vec![
                r(&rows(&[]), 0, Some(600)),
                r(&rows(&[None]), 60, Some(3)),
                r(&kept, 120, Some(3)),
            ];
            if draft_point {
                points.push(R {
                    typed: Some(180),
                    ..r(&drafted, 200, None)
                });
                points.push(r(&kept, 3600, Some(3)));
            } else {
                points.push(R {
                    typed: Some(180),
                    ..r(&kept, 3600, Some(3))
                });
            }
            let (dir, ledger) = ledger_at(tag);
            let mut m = Mock::new(vec![busy_screen()]);
            let mut d = Driver::new(
                &mut m,
                program,
                &ledger,
                Autopilot::Cruise,
                (CAPTURED, 0),
                false,
            );
            let out: Vec<Out> = points.iter().map(|p| d.point(p)).collect();
            let answers = d.s.turn_end.replies().last().map(|k| k.answers);
            drop(d);
            let _ = std::fs::remove_dir_all(dir);
            (out, answers)
        };
        let tag = format!("draft-under-kept-{program}");
        let (out, answers) = run(&tag, false);
        let (counted, counted_answers) = run(&format!("{tag}-pointed"), true);
        assert_eq!(
            out[..3],
            counted[..3],
            "{tag}: one loop up to the draft: {out:#?}"
        );
        assert_eq!(
            answers,
            Some(1),
            "{tag}: the person's keys start the kept reply again: {out:#?}"
        );
        assert!(
            counted_answers.is_some_and(|n| n >= 2),
            "{tag}: a point at the draft counts the turn: {counted_answers:?} {counted:#?}"
        );
        lifted.push((answers, counted_answers));
    }
    assert_eq!(
        lifted[0], lifted[1],
        "agy keeps its replies as Claude Code does"
    );
}
