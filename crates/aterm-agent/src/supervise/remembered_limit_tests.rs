// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::supervise::approvals::{Outcome, RULE_WALL_WAIT, RULE_WALL_WAIT_END, Row};
use crate::supervise::policy::turn_end::{
    Conversation, RULE_CONTINUE, RULE_LIMIT_RESUME, decide_turn_end,
};

/// Compose the measured limit/resume screens with a shell that outlived the
/// turn, as the measured background-work fixtures draw it. No live turn runs.
fn with_shell(mut rows: Vec<String>) -> Vec<String> {
    let done = rows
        .iter()
        .rposition(|r| r.starts_with('✻'))
        .expect("done row");
    rows[done].push_str(" · 1 shell still running");
    let footer = rows
        .iter()
        .rposition(|r| r.contains("auto mode on"))
        .expect("mode row");
    rows[footer].push_str(" · 1 shell");
    assert!(aterm_phase::background_only(&rows), "{rows:#?}");
    rows
}

/// The session's actual reader and policy fold, with no real-time wait.
fn fold(
    s: &mut Session<'_, Mock>,
    rows: Vec<String>,
    opts: &SuperviseOpts,
) -> (TurnEndReading, TurnEndAction) {
    let screen = read_screen(rows);
    let reading = s.turn_end_reading(&screen, None, opts);
    let now = Instant::now();
    s.observe_point(&reading, now, screen.seq);
    let action = decide_turn_end(&s.turn_end, &reading, &opts.policy, now);
    (reading, action)
}

/// A remembered wall must not manufacture a live countdown from the notice
/// it saved. With a background shell the raw resumed screen reads Busy, so
/// the memory used to reinstate its old auto-continue promise. The same
/// screen with a countdown still drawn remains the vendor's own wait.
#[test]
fn a_remembered_limit_uses_the_countdown_still_drawn_under_background_work() {
    use aterm_phase::prompt::fixtures::{RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300, screen};
    for armed in [false, true] {
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, None);
        s.set_clock(TEST_NOW, PDT);
        s.set_zone_lookup(test_zones);
        s.set_turn_end_timing(TurnEndTiming {
            reset_grace: Duration::from_secs(7),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        });
        let opts = hosted(30);
        let (_, first) = fold(&mut s, with_shell(armed_conversation_b()), &opts);
        assert!(
            matches!(first, TurnEndAction::WaitUntil { .. }),
            "{first:?}"
        );
        let mut resumed = with_shell(screen(RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300));
        if armed {
            resumed.push("    Continuing automatically at 9:19am · esc to cancel".to_owned());
        }
        let (reading, action) = fold(&mut s, resumed, &opts);
        let reset = reading.reset_at.expect("the remembered reset");
        let expected = if armed {
            s.turn_end.timing.auto_resume_grace
        } else {
            s.turn_end.timing.reset_grace
        };
        assert!(
            matches!(&action, TurnEndAction::WaitUntil { until, why, .. }
                if *until == reset + expected && why == if armed { "usage-session goes on by itself" } else { "usage-session resets" }),
            "armed={armed}: the live countdown determines the wait: {action:?}\n{reading:#?}"
        );
        assert!(s.turn_end.waits_on_named_reset());
        assert!(s.wall_memory.is_some(), "the limit itself still stands");
    }
}

/// An actual different-family /model result ends the remembered bucket even
/// while a shell keeps the raw screen Busy. Same-family and unknown results
/// keep the bucket and its ledger open, including after the next screen read.
#[test]
fn a_remembered_bucket_obeys_the_newest_model_choice_under_background_work() {
    use aterm_phase::prompt::fixtures::{WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300, screen};
    for (chosen, leaves) in [
        ("Opus 5.5", true),
        ("Sonnet 5.5", false),
        ("new-default", false),
    ] {
        let (dir, ledger) = ledger_at(&format!("remembered-model-{chosen}"));
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_owned()));
        // The capture names Oct 17: put the test two days before its reset,
        // on Oct 15, rather than the older watch fixtures' September clock.
        s.set_clock(TEST_NOW + 28 * 86_400, PDT);
        s.set_zone_lookup(test_zones);
        s.set_approval_ledger(Some(ledger.clone()));
        // Direct folds do not enter Session::watch, which opens the ledger.
        // Use that same initialization so persist_wall and forget_wall write
        // their real rows, rather than seeding either outcome in this test.
        let mut notes = Vec::new();
        s.ledger = approvals::Ledger::open(Some(&ledger), Some("@s-1"), &mut notes);
        assert!(notes.is_empty(), "the scratch ledger opens: {notes:?}");
        let mut opts = hosted(30);
        opts.policy.model_fallback = None;
        opts.policy.human_grace_s = 0;
        // Learn the bucket from the measured screen first. Its last done
        // row precedes /rate-limit-options: a synthetic hard shell there
        // would correctly require a later done row before admitting the
        // footer as the wall the turn ended on.
        let armed = screen(WALL_SONNET_BUCKET_AUTO_CONTINUE_ARMED_2_1_300);
        let (initial, action) = fold(&mut s, armed.clone(), &opts);
        assert!(
            initial.wall == Some(aterm_phase::WallKind::ModelBucket { consent: false })
                && initial.reset_at.is_some()
                && s.turn_end.waits_on_named_reset(),
            "{chosen}: the measured bucket must seed a named wait: {initial:#?}\n{action:?}"
        );
        assert!(
            approvals::open_wall(&ledger, Some("@s-1")).is_some(),
            "the initial read must persist its wait: {initial:#?}\n{action:?}\n{}",
            std::fs::read_to_string(&ledger).unwrap_or_default()
        );
        let mut rows: Vec<String> = armed
            .into_iter()
            .filter(|r| {
                !r.contains("Usage limit reached") && !r.contains("Continuing automatically")
            })
            .collect();
        let top = rows
            .iter()
            .position(|r| r.starts_with('─'))
            .expect("composer");
        rows.splice(
            top..top,
            [
                "❯ /model".to_owned(),
                format!("  ⎿  Set model to {chosen}"),
                String::new(),
            ],
        );
        let rows = with_shell(rows);
        assert!(aterm_phase::background_only(&rows));
        assert!(
            aterm_phase::wall(&rows).is_none(),
            "only the memory supplies a wall"
        );
        for _ in 0..2 {
            let (reading, action) = fold(&mut s, rows.clone(), &opts);
            if leaves {
                assert!(
                    !s.turn_end.waits_on_named_reset(),
                    "{chosen}: the bucket was left: {reading:#?}"
                );
                assert!(
                    s.wall_memory.is_none(),
                    "{chosen}: no stale wall can return"
                );
                assert!(approvals::open_wall(&ledger, Some("@s-1")).is_none());
                // The first point can resume the task; a later raw background
                // screen alone is no newly completed turn.
                assert!(
                    action.rule_id() == Some(RULE_LIMIT_RESUME)
                        || matches!(action, TurnEndAction::Nothing),
                    "{action:?}"
                );
            } else {
                assert!(
                    s.turn_end.waits_on_named_reset(),
                    "{chosen}: the bucket still binds"
                );
                assert!(s.wall_memory.is_some());
                assert!(approvals::open_wall(&ledger, Some("@s-1")).is_some());
                assert!(
                    matches!(action, TurnEndAction::WaitUntil { .. }),
                    "{action:?}"
                );
            }
        }
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// ONE PROGRAM'S LIMIT AS THE LOOP MEETS IT: `wall`, a screen of the program
/// that draws the limit's notice with its reset (`None`: none of its screens
/// draws one, and the limit is the ledger's open wall a loop that follows
/// another takes up), then `after`, a screen of the same program that draws
/// none of it: the turn's end over its own background work on Muse Code, agy
/// and Claude Code, which the memory keeps the wall for, an idle point on
/// Gemini CLI, a screen no reader names for Codex and any other program.
/// `program` is the name `status` gives (`None`: the screen tells).
struct Limited {
    program: Option<&'static str>,
    wall: Option<Vec<String>>,
    after: Vec<String>,
    /// The open wall a loop that follows takes up, where `wall` is `None`:
    /// its words and its reset as the loop that wrote it read them.
    seed: Option<aterm_phase::Wall>,
}

/// What one point decided, and the limit's conversation it left: the
/// track's ([`TurnEndState::limit_conversation`]) and the memory's; and the
/// exception the point stands under (`kind why`, [`exception_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Decided {
    act: String,
    track: Option<Conversation>,
    memory: Option<Conversation>,
    told: Option<String>,
}

/// One point of a loop: its `seq`, the program `status` names there
/// (`None`: the screen tells), its screen, the seconds past [`TEST_NOW`] its
/// clock reads, and the work a turn ran to it.
struct At<'a> {
    seq: u64,
    program: Option<&'a str>,
    rows: &'a [String],
    at: i64,
    worked: Option<Duration>,
}

/// How far ahead a seeded open wall's reset is placed.
const LIMITED_RESET_IN: i64 = 600;

/// The open wall `w` leaves for a loop that follows, its reset
/// [`LIMITED_RESET_IN`] past [`TEST_NOW`], written a minute before it, its
/// conversation `scope`.
fn open_wall_of(w: &aterm_phase::Wall, scope: &Conversation) -> approvals::OpenWall {
    approvals::OpenWall {
        conversation: scope.clone(),
        program: None,
        at_ms: (TEST_NOW - 60) * 1000,
        message: w.message.clone(),
        reset: w.reset.clone(),
        reset_unix: Some(TEST_NOW + LIMITED_RESET_IN),
        kind: Some(w.kind),
        judged_ms: None,
        retry_ms: None,
    }
}

/// ONE LOOP ON THE SESSION'S LEDGER at `ledger`: `host`, a host that reads
/// no transcript (as a window's host is for every agent but Claude Code);
/// `seed`, the open wall it takes up at the first point's clock; then each
/// of `points` decided by the session's own loop.
fn loop_points(
    ledger: &Path,
    host: bool,
    seed: Option<approvals::OpenWall>,
    points: &[At<'_>],
) -> Vec<Decided> {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, Some("@s-1".to_owned()));
    s.set_clock(TEST_NOW + points.first().map_or(0, |p| p.at), PDT);
    s.set_zone_lookup(test_zones);
    // Every deadline the policy's to the second, however long the steps take.
    s.set_mono_standing();
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    let mut notes = Vec::new();
    s.ledger = approvals::Ledger::open(Some(ledger), Some("@s-1"), &mut notes);
    assert!(notes.is_empty(), "the scratch ledger opens: {notes:?}");
    let opts = SuperviseOpts {
        idle_host: host.then(|| Arc::new(AccountHost::default()) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    if let Some(open) = seed {
        s.seed_wall_memory(Some(open));
        assert!(s.wall_memory.is_some(), "the seed is taken up");
    }
    let mut decided = Vec::new();
    for p in points {
        s.set_clock(TEST_NOW + p.at, PDT);
        s.program = p.program.map(str::to_owned);
        let point = point_at(&s, p.rows.to_vec(), p.seq);
        let act = match s.turn_end_at_point(&point, p.worked, &opts) {
            Ok(TurnEndAction::WaitUntil { until, why, .. }) => {
                format!("wait {}s: {why}", s.unix_of(until) - s.now_unix())
            }
            Ok(TurnEndAction::Nothing) => "nothing".to_owned(),
            Ok(a) => format!("act {}", a.rule_id().unwrap_or("-")),
            Err(e) => format!("fail {e:?}"),
        };
        decided.push(Decided {
            act,
            track: s.turn_end.limit_conversation().cloned(),
            memory: s.wall_memory.as_ref().map(|m| m.conversation.clone()),
            told: s
                .exception_now
                .as_ref()
                .map(|e| format!("{} {}", e.kind.word(), e.why)),
        });
    }
    decided
}

/// Each row of the ledger at `ledger`: its rule and the conversation it was
/// written with.
fn ledger_scopes(ledger: &Path) -> Vec<String> {
    std::fs::read_to_string(ledger)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .map(|v| {
            format!(
                "{} conversation={:?}",
                v.get("rule_id")
                    .and_then(aterm_json::Value::as_str)
                    .unwrap_or("-"),
                v.get("conversation")
            )
        })
        .collect()
}

/// The program each wall row of the ledger at `ledger` names it met under
/// ([`approvals::PROGRAM`]; `None`: the row names none).
fn wall_programs(ledger: &Path) -> Vec<Option<String>> {
    std::fs::read_to_string(ledger)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| aterm_json::from_str::<aterm_json::Value>(line).ok())
        .filter(|v| v.get("rule_id").and_then(aterm_json::Value::as_str) == Some(RULE_WALL_WAIT))
        .map(|v| {
            v.get(approvals::PROGRAM)
                .and_then(aterm_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

/// The points of `p`'s limit, decided by the session's own loop: its wall
/// (where a screen draws it), a point drawing none of it before the reset,
/// and the same screen two days past it. `host`: a host that reads no
/// transcript; `scope`: the open wall's conversation, as the row that seeds
/// it says. Also each ledger row's rule and the conversation it was written
/// with.
fn limited_points(
    p: &Limited,
    host: bool,
    scope: &Conversation,
    tag: &str,
) -> (Vec<Decided>, Vec<String>) {
    let (dir, ledger) = ledger_at(tag);
    let mut points = Vec::new();
    if let Some(wall) = &p.wall {
        points.push(At {
            seq: 101,
            program: p.program,
            rows: wall,
            at: 0,
            worked: Some(Duration::from_secs(60)),
        });
    }
    for (seq, at) in [(102, 0), (103, 2 * 86_400)] {
        points.push(At {
            seq,
            program: p.program,
            rows: &p.after,
            at,
            worked: None,
        });
    }
    let seed = p.seed.as_ref().map(|w| open_wall_of(w, scope));
    let decided = loop_points(&ledger, host, seed, &points);
    let rows = ledger_scopes(&ledger);
    let _ = std::fs::remove_dir_all(dir);
    (decided, rows)
}

/// A LIMIT ON AN AGENT WHOSE CONVERSATION THE LOOP NEVER READS
/// ([`screen_alone`]: Muse Code, agy, Gemini CLI) is the hostless lane's, a
/// host there or not: the screen is all the evidence there is, and no host
/// read can name the conversation. Under a host that reads no transcript
/// every point decides as with no host, the limit's track and memory keep
/// the hostless lane's conversation, and no ledger row carries a scope — for
/// a wall the screen drew, a legacy open wall, and an open wall a
/// host-scoped loop wrote unread (as a host's loop writes every legacy wall
/// at its start: [`a_hosts_loop_requires_the_wall_it_takes_up_before_any_decision`]).
/// The limit is waited for to its reset, never looked at as unread, and the
/// resume is typed past it. Measured before: under the host, every point
/// after the wall waited `now + 5 min` ("the remembered limit's conversation
/// is unread"), two days past the reset too, never resumed, and an unread
/// open wall was held so with no host as well.
fn a_limit_is_the_hostless_lanes(name: &str, p: &Limited) {
    let scopes: &[Conversation] = if p.wall.is_some() {
        &[Conversation::Hostless]
    } else {
        &[Conversation::Hostless, Conversation::Unknown]
    };
    for scope in scopes {
        let tag = |host| format!("never-read-{name}-{scope:?}-{host}");
        let (hosted, hosted_rows) = limited_points(p, true, scope, &tag(true));
        let (hostless, hostless_rows) = limited_points(p, false, scope, &tag(false));
        assert_eq!(
            hosted, hostless,
            "{name}, seeded {scope:?}: a host that reads no transcript decides as none"
        );
        assert_eq!(
            hosted_rows, hostless_rows,
            "{name}, seeded {scope:?}: the ledger a host leaves is the hostless lane's"
        );
        for d in &hosted {
            assert!(
                d.track
                    .as_ref()
                    .is_none_or(|c| *c == Conversation::Hostless)
                    && d.memory
                        .as_ref()
                        .is_none_or(|c| *c == Conversation::Hostless),
                "{name}, seeded {scope:?}: the hostless lane's conversation: {hosted:#?}"
            );
        }
        assert!(
            hosted_rows.iter().all(|r| r.ends_with("conversation=None")),
            "{name}, seeded {scope:?}: no row is scoped: {hosted_rows:#?}"
        );
        let (past, before) = hosted.split_last().expect("points");
        for d in before {
            assert!(
                d.act.starts_with("wait ") && !d.act.contains("unread"),
                "{name}, seeded {scope:?}: the reset is waited for: {hosted:#?}"
            );
        }
        assert_eq!(
            past.act,
            format!("act {RULE_LIMIT_RESUME}"),
            "{name}, seeded {scope:?}: resumed past the reset: {hosted:#?}"
        );
    }
}

/// The open wall a Muse Code's usage limit leaves (`resets at 1:06 PM`).
fn muse_limit() -> aterm_phase::Wall {
    aterm_phase::read(
        None,
        &aterm_phase::prompt::fixtures::screen(aterm_phase::muse::fixtures::USAGE_WALL),
        None,
    )
    .wall
    .expect("Muse Code's usage wall")
}

/// [`a_limit_is_the_hostless_lanes`]: Muse Code's usage wall (the lab's
/// 429), drawn and then hidden under a backgrounded shell's panel, and a
/// loop that follows taking it up from the ledger.
#[test]
fn a_muse_codes_limit_under_a_host_is_the_hostless_lanes() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let after = screen(u::END_OF_TURN_BG_RUNNING);
    for p in [
        Limited {
            program: None,
            wall: Some(screen(u::USAGE_WALL)),
            after: after.clone(),
            seed: None,
        },
        Limited {
            program: None,
            wall: None,
            after,
            seed: Some(muse_limit()),
        },
    ] {
        a_limit_is_the_hostless_lanes("muse", &p);
    }
}

/// [`a_limit_is_the_hostless_lanes`]: Muse Code's usage wall hidden under the
/// `/status` cards a person entered at the empty composer under it, the
/// composer open under the cards (measured on 1.4.10 in the lab, 2026-10-21:
/// three cards pushed the notice out of sight), and a loop that follows
/// taking the wall up from the ledger there. The cards are a session at rest
/// (`aterm_phase::muse`'s `Turn::Carded`): the reset is waited for and the
/// resume typed past it, a host there or not. Measured before (a unit probe,
/// `docs/measured/forever-lab-2026-10-20.md` W2): the cards read as no
/// evidence, so the policy decided nothing there, two days past the reset
/// too.
#[test]
fn a_muse_codes_limit_under_its_status_cards_resumes_at_its_reset() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let after = screen(u::USAGE_WALL_STATUS_CARDS);
    for p in [
        Limited {
            program: None,
            wall: Some(screen(u::USAGE_WALL)),
            after: after.clone(),
            seed: None,
        },
        Limited {
            program: None,
            wall: None,
            after,
            seed: Some(muse_limit()),
        },
    ] {
        a_limit_is_the_hostless_lanes("muse-cards", &p);
    }
}

/// What one loop on a scratch ledger decides at each of `points` (`rows`,
/// seconds past [`TEST_NOW`], the work a turn ran to it), a host that reads
/// no transcript there or not.
fn decided_at(
    tag: &str,
    host: bool,
    points: &[(&[String], i64, Option<Duration>)],
) -> Vec<Decided> {
    let (dir, ledger) = ledger_at(tag);
    let points: Vec<At<'_>> = points
        .iter()
        .zip(101..)
        .map(|(&(rows, at, worked), seq)| At {
            seq,
            program: None,
            rows,
            at,
            worked,
        })
        .collect();
    let decided = loop_points(&ledger, host, None, &points);
    let _ = std::fs::remove_dir_all(dir);
    decided
}

/// A MUSE CODE SESSION WITH NO TURN YET UNDER ITS CARDS IS TYPED NOTHING
/// (measured on 1.4.10, the lab of 2026-10-21: `/status`, `/usage` and
/// `/status` twice more at a fresh session's empty composer pushed its
/// banner out of sight, each `/status` card saying `IDLE`,
/// [`aterm_phase::muse::fixtures::FRESH_STATUS_CARDS`]). The loop reads no
/// conversation of Muse Code's (a host's `taskless` is Claude Code's alone),
/// and a person's keys there leave no unasked launch: the screen is all that
/// says nobody gave the session a task. Every point decides nothing, a host
/// there or not, at the banner and then under the cards, in a 24-row window
/// of them too. Measured before (the review's probe): the cards read as a
/// session at rest after a turn, and the loop decided `nothing`, a 2 min
/// back-off, then `continue@v1` into the session nobody had asked
/// anything. THE CONTROL: the same cards after a turn the stub answered
/// (`COMPLETED`, measured) are that turn's end, and the loop types there.
#[test]
fn a_muse_codes_fresh_session_under_its_cards_is_typed_nothing() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let banner = screen(u::IDLE_1_4_10);
    let cards = screen(u::FRESH_STATUS_CARDS);
    let window = cards[cards.len() - 24..].to_vec();
    for host in [false, true] {
        for (what, after) in [("cards", &cards), ("24 rows", &window)] {
            let d = decided_at(
                &format!("muse-fresh-cards-{host}-{}", after.len()),
                host,
                &[
                    (&banner, 0, None),
                    (after, 60, None),
                    (after, 300, None),
                    (after, 3600, None),
                ],
            );
            assert!(
                d.iter().all(|d| d.act == "nothing"),
                "host={host}, {what}: nobody gave it a task: {d:#?}"
            );
        }
    }
    let done = screen(u::COMPLETED_STATUS_CARDS);
    let d = decided_at(
        "muse-completed-cards",
        false,
        &[
            (&done, 0, Some(Duration::from_secs(60))),
            (&done, 300, None),
            (&done, 3600, None),
        ],
    );
    assert!(
        d.iter().any(|d| d.act.starts_with("act ")),
        "the control: a turn's end under its cards is acted at: {d:#?}"
    );
}

/// Muse Code's usage wall in a form whose notice names no reset: `at` the
/// measured 429 of a `usage_limit_reached` with no `resets_at`
/// ([`aterm_phase::muse::fixtures::LIMIT_UNNAMED`]), and the measured usage
/// wall's notice put in the binary's two other shapes, bare and its quota's,
/// each with its own wall read off it.
fn unnamed_walls() -> Vec<(&'static str, Vec<String>)> {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let named = screen(u::USAGE_WALL);
    let at = named
        .iter()
        .position(|r| r.starts_with("◆ Usage limit reached"))
        .expect("the notice");
    let with = |notice: &str| -> Vec<String> {
        let mut rows = named.clone();
        rows[at] = notice.to_owned();
        rows.remove(at + 1);
        rows
    };
    let walls = vec![
        ("the 429", screen(u::LIMIT_UNNAMED)),
        (
            "bare",
            with(
                "◆ Usage limit reached · /upgrade (https://accountscenter.meta.com/muse_code/?ep=xgrade) for increased limits",
            ),
        ),
        ("its quota's", with("◆ You've used your 5-hour quota.")),
    ];
    for (name, rows) in &walls {
        let w = aterm_phase::read(None, rows, None)
            .wall
            .unwrap_or_else(|| panic!("{name}: a wall"));
        assert!(
            w.kind.reads_limited() && w.reset.is_none(),
            "{name}: a limit naming no reset: {w:?}"
        );
    }
    walls
}

/// MUSE CODE'S WALL UNDER ITS `FAILED` CARDS IS THE WALL STILL (measured on
/// 1.4.10, the lab of 2026-10-21: `/status` three times under a 429 its
/// retries did not get past, and under the usage wall, each card saying the
/// run `FAILED`, the notice out of sight). The cards are a person's look, no
/// work since: a wall whose notice names no reset is waited on as with its
/// notice in sight, the 30 min the policy gives such a limit counted from
/// the wall, a host there or not. Measured before (the review's probe): the
/// cards read as an idle point with no wall, the limit's notice had left the
/// screen with no work since and named no reset, so the resume was typed a
/// minute after the wall, into the standing limit. THE CONTROL: the same
/// cards after a turn that ended as a turn does since (`COMPLETED`,
/// measured) end the wall: the worker worked.
#[test]
fn a_muse_codes_wall_under_its_failed_cards_is_waited_on_as_at_its_notice() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let worked = Some(Duration::from_secs(60));
    for (name, wall) in unnamed_walls() {
        let cards = screen(if name == "the 429" {
            u::LIMIT_UNNAMED_STATUS_CARDS
        } else {
            u::USAGE_WALL_STATUS_CARDS
        });
        for host in [false, true] {
            let tag = |what: &str| format!("muse-failed-cards-{what}-{host}-{}", name.len());
            let shown = decided_at(
                &tag("shown"),
                host,
                &[(&wall, 0, worked), (&wall, 60, None), (&wall, 300, None)],
            );
            assert!(
                shown[0].act.starts_with("wait ") && shown.iter().all(|d| d.memory.is_some()),
                "{name}, host={host}: the wall in sight is waited on: {shown:#?}"
            );
            let hidden = decided_at(
                &tag("hidden"),
                host,
                &[(&wall, 0, worked), (&cards, 60, None), (&cards, 300, None)],
            );
            assert_eq!(
                hidden, shown,
                "{name}, host={host}: under its cards the wall is decided as in sight"
            );
        }
        let done = screen(u::COMPLETED_STATUS_CARDS);
        let worked_since = decided_at(
            &format!("muse-completed-after-{}", name.len()),
            false,
            &[(&wall, 0, worked), (&done, 60, None)],
        );
        assert!(
            worked_since[1].memory.is_none() && !worked_since[1].act.contains("resets"),
            "{name}: the control, a turn completed since: {worked_since:#?}"
        );
    }
}

/// MUSE CODE'S WALL WITH A LOCAL COMMAND'S ROWS UNDER IT WAITS ON THE RESET IT
/// NAMES (measured in the W2 lab, 2026-10-21: `/rules` entered at the empty
/// composer under the usage wall printed three indented rows under the
/// notice, [`aterm_phase::muse::fixtures::USAGE_WALL_RULES_UNDER`]). The
/// point decides as at the notice alone, and its wait is the named reset's.
/// Measured before: the rows were folded into the wall's message (`reset=3:34
/// AM Rules loaded …`), its reset named no time, and the wait fell back to
/// 30 min from the wall's first read. THE CONTROL: the notice alone, the
/// lab's own screen before the first `/rules`, waits on the same reset.
#[test]
fn a_muse_codes_wall_with_a_local_commands_rows_under_it_waits_on_its_reset() {
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let ruled = screen(u::USAGE_WALL_RULES_UNDER);
    let at = ruled
        .iter()
        .position(|r| r.starts_with("  Rules loaded"))
        .expect("the /rules rows");
    // The notice alone: its block, the blank row under it, the composer.
    let alone: Vec<String> = ruled[..at]
        .iter()
        .chain(&ruled[at + 4..])
        .cloned()
        .collect();
    assert!(
        alone.iter().all(|r| !r.contains("Rules loaded")) && alone.len() + 4 == ruled.len(),
        "{alone:#?}"
    );
    // The lab's clock of day when the wall was first read, 03:28:51 PDT,
    // five minutes before the reset it names.
    const WALL_READ_AT: i64 = -(5 * 3600 + 26 * 60 + 9);
    let decide = |rows: &[String], tag: &str| {
        let (dir, ledger) = ledger_at(tag);
        let decided = loop_points(
            &ledger,
            false,
            None,
            &[At {
                seq: 101,
                program: None,
                rows,
                at: WALL_READ_AT,
                worked: Some(Duration::from_secs(60)),
            }],
        );
        let _ = std::fs::remove_dir_all(dir);
        decided
    };
    // The wait's seconds: the reset 309 s ahead, and at most its grace and
    // jitter past it (`TurnEndTiming`'s defaults, 150 s and 60 s), well short
    // of the 30 min fallback.
    let waits = |d: &[Decided]| -> Option<i64> {
        d.first()?
            .act
            .strip_prefix("wait ")?
            .split_once("s: ")
            .filter(|(_, why)| why.ends_with("usage-session resets"))?
            .0
            .parse()
            .ok()
    };
    let named = decide(&alone, "muse-rules-control");
    assert!(
        waits(&named).is_some_and(|s| (309..=309 + 150 + 60).contains(&s)),
        "the control waits on the named reset: {named:#?}"
    );
    assert_eq!(decide(&ruled, "muse-rules-under"), named);
}

/// [`a_limit_is_the_hostless_lanes`]: agy's per-day 429 (the lab's), drawn
/// and then hidden under a command task's panel, and a loop that follows
/// taking it up from the ledger.
#[test]
fn an_agys_limit_under_a_host_is_the_hostless_lanes() {
    use aterm_phase::agy::fixtures::lab as a;
    use aterm_phase::prompt::fixtures::screen;
    let after = screen(a::BG_TASK);
    let seed = aterm_phase::read(None, &screen(a::ERROR_429_LONG), None)
        .wall
        .expect("agy's 429");
    for p in [
        Limited {
            program: None,
            wall: Some(screen(a::ERROR_429_LONG)),
            after: after.clone(),
            seed: None,
        },
        Limited {
            program: None,
            wall: None,
            after,
            seed: Some(seed),
        },
    ] {
        a_limit_is_the_hostless_lanes("agy", &p);
    }
}

/// [`a_limit_is_the_hostless_lanes`]: Gemini CLI draws no limit that names
/// its reset (its 429 is an API error's row, its quota a dialog), so its
/// limit is an open wall a loop that follows takes up, at Gemini CLI's
/// idle turn end.
#[test]
fn a_gemini_clis_limit_under_a_host_is_the_hostless_lanes() {
    use aterm_phase::prompt::fixtures::screen;
    a_limit_is_the_hostless_lanes(
        "gemini",
        &Limited {
            program: None,
            wall: None,
            after: screen(aterm_phase::gemini::fixtures::END_OF_TURN),
            seed: Some(muse_limit()),
        },
    );
}

/// A build's output: a screen no reader names.
fn a_build() -> Vec<String> {
    rows(&["$ make", "cc -c parser.c", "cc -c lexer.c"])
}

/// A SCREEN NO READER NAMES SAYS NOTHING OF WHOSE THE LIMIT IS (neither
/// [`screen_alone`] nor [`reads_conversation`]): Codex, named `codex` by
/// `status`, its screen read by nothing; a shell (`zsh`, the one Claude Code
/// exited to); a program started as `node` (Claude Code installed by npm)
/// and one with no name, drawing no frame or box any reader knows. Each
/// makes no point and keeps the limit's scope as it stands, a host there or
/// not: an open wall a host-scoped loop wrote unread stays unread, a legacy
/// one the hostless lane's, and no wall row is written. Measured before
/// (217343ef72): each released an unread scope to the hostless lane's at
/// its first decision, on the track, in memory and in a row a hostless loop
/// that follows reads ([`claude_codes_unread_limit_outlives_a_screen_no_reader_names`]).
#[test]
fn a_screen_no_reader_names_keeps_the_limits_scope() {
    for (name, program, after) in [
        (
            "codex",
            Some("codex"),
            rows(&["• Working on the parser", "", "› "]),
        ),
        ("shell", Some("zsh"), a_build()),
        ("node", Some("node"), a_build()),
        ("unnamed", None, a_build()),
    ] {
        for host in [true, false] {
            for scope in [Conversation::Hostless, Conversation::Unknown] {
                let p = Limited {
                    program,
                    wall: None,
                    after: after.clone(),
                    seed: Some(muse_limit()),
                };
                let tag = format!("unnamed-{name}-{host}-{scope:?}");
                let (points, rows) = limited_points(&p, host, &scope, &tag);
                let kept = |c: &Option<Conversation>| c.as_ref().is_none_or(|c| *c == scope);
                assert!(
                    points.iter().all(|d| d.act == "nothing")
                        && points[0].track.as_ref() == Some(&scope)
                        && points[0].memory.as_ref() == Some(&scope)
                        && points.iter().all(|d| kept(&d.track) && kept(&d.memory)),
                    "{name}, host {host}: the scope kept as it stands: {points:#?}"
                );
                assert!(
                    rows.iter()
                        .all(|r| r.split(' ').next() != Some(RULE_WALL_WAIT)),
                    "{name}, host {host}: no scope written: {rows:#?}"
                );
            }
            // The point's own evidence: unread under a host, never the
            // hostless lane's there, so no point of it answers a limit's try
            // as the limit's own conversation would (`owns_answer`).
            let mut m = Mock::new(vec![busy_screen()]);
            let mut s = session(&mut m, Some("@s-1".to_owned()));
            s.set_clock(TEST_NOW, PDT);
            s.program = program.map(str::to_owned);
            let opts = SuperviseOpts {
                idle_host: host.then(|| Arc::new(AccountHost::default()) as Arc<dyn IdleHost>),
                ..hosted(30)
            };
            let (reading, _) = fold(&mut s, after.clone(), &opts);
            assert_eq!(
                reading.conversation,
                if host {
                    Conversation::Unknown
                } else {
                    Conversation::Hostless
                },
                "{name}, host {host}: the point's evidence"
            );
        }
    }
}

/// THE CONTROL: Claude Code's conversation IS read (its transcript), so a
/// host makes its remembered limit's conversation required, as 0ab6eaf3e6
/// bound it, and the hostless lane's rule is no release of it. Conversation
/// B's five-hour limit, drawn and then hidden under a shell: under a host
/// whose read names no conversation, the points after it look again
/// (`limit-conversation-unread`) and nothing is typed, past the reset too;
/// with no host, the resume past the reset. A legacy open wall under a host
/// is required the same, and an open wall a host-scoped loop wrote unread
/// stays unread with no host (a departed host cannot unbind it).
#[test]
fn claude_codes_remembered_limit_still_needs_its_conversation_read() {
    use aterm_phase::prompt::fixtures::{RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300, screen};
    let after = with_shell(screen(RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300));
    let unread = |d: &Decided| {
        d.act
            .contains("the remembered limit's conversation is unread")
    };
    let live = Limited {
        program: None,
        wall: Some(armed_conversation_b()),
        after: after.clone(),
        seed: None,
    };
    let (hosted, _) = limited_points(&live, true, &Conversation::Hostless, "claude-live-host");
    assert!(
        hosted[1..].iter().all(unread)
            && hosted[1..]
                .iter()
                .all(|d| d.track == Some(Conversation::Unknown)),
        "under a host, read and unnamed: {hosted:#?}"
    );
    let (hostless, _) = limited_points(&live, false, &Conversation::Hostless, "claude-live");
    assert_eq!(
        hostless.last().map(|d| d.act.as_str()),
        Some(format!("act {RULE_LIMIT_RESUME}").as_str()),
        "with no host, resumed past the reset: {hostless:#?}"
    );
    let seeded = Limited {
        program: None,
        wall: None,
        after,
        seed: aterm_phase::read(None, &armed_conversation_b(), None).wall,
    };
    for (host, scope) in [
        (true, Conversation::Hostless),
        (false, Conversation::Unknown),
    ] {
        let (points, rows) = limited_points(&seeded, host, &scope, &format!("claude-seed-{host}"));
        assert!(
            points.iter().all(unread)
                && points
                    .iter()
                    .all(|d| d.track == Some(Conversation::Unknown)),
            "seeded {scope:?}, host {host}: held unread: {points:#?}"
        );
        assert!(
            !host || rows.iter().any(|r| r.ends_with("conversation=Some(Null)")),
            "the required scope is written for a loop that follows: {rows:#?}"
        );
    }
}

/// A CLAUDE CODE LIMIT NO READ NAMED OUTLIVES A SCREEN NO READER NAMES, for
/// the loop that reads it and for a hostless one that follows. A host's
/// loop takes up conversation B's five-hour limit from the row a
/// host-scoped loop wrote unread (`"conversation":null`), and at Claude
/// Code's screen, its notice hidden under a shell, looks again
/// (`limit-conversation-unread`). Then the tab is at a screen no reader
/// names: the shell Claude Code exited to (`status` names `zsh`), or a
/// Claude Code installed by npm (`status` names `node`) whose transcript
/// view draws no frame of its own. The limit stays unread on the track and
/// in memory, and no row releases it. A hostless loop that follows, an hour
/// past the reset at Claude Code's screen, takes the row up unread and looks
/// again: nothing is typed into a conversation no read confirmed. Measured
/// before (217343ef72): the second point released the scope to the hostless
/// lane's and wrote it, and the hostless loop typed `usage-resume@v1`.
#[test]
fn claude_codes_unread_limit_outlives_a_screen_no_reader_names() {
    use aterm_phase::prompt::fixtures::{RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300, screen};
    let claude = with_shell(screen(RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300));
    let shell = a_build();
    let view = rows(&[
        "⏺ Bash(make)",
        "  ⎿  cc -c parser.c",
        "     cc -c lexer.c",
        "",
        "  Showing detailed transcript · ctrl+o to toggle",
    ]);
    assert_eq!(
        aterm_phase::identify(Some("node"), &view).program(),
        Program::Generic,
        "the transcript view names no program"
    );
    let unread = |d: &Decided| {
        d.act
            .contains("the remembered limit's conversation is unread")
    };
    let wall = aterm_phase::read(None, &armed_conversation_b(), None)
        .wall
        .expect("B's limit");
    let reason = approvals::wall_wait_reason(
        Some(TEST_NOW + LIMITED_RESET_IN),
        wall.reset.as_deref(),
        wall.kind,
        &wall.message,
    );
    let written = Row {
        rule_id: RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 100,
    }
    .to_json_wall_wait((TEST_NOW - 60) * 1000, Some("@s-1"), &Conversation::Unknown);
    for (name, first, second, unnamed) in [
        ("exited", None, Some("zsh"), &shell),
        ("npm", Some("node"), Some("node"), &view),
    ] {
        let (dir, ledger) = ledger_at(&format!("unread-across-{name}"));
        std::fs::write(&ledger, format!("{written}\n")).expect("the ledger");
        let seed = approvals::open_wall(&ledger, Some("@s-1"));
        assert_eq!(
            seed.as_ref().map(|o| &o.conversation),
            Some(&Conversation::Unknown),
            "{name}: the row a host-scoped loop wrote"
        );
        let hosted = loop_points(
            &ledger,
            true,
            seed,
            &[
                At {
                    seq: 101,
                    program: first,
                    rows: &claude,
                    at: 0,
                    worked: None,
                },
                At {
                    seq: 102,
                    program: second,
                    rows: unnamed,
                    at: 60,
                    worked: None,
                },
            ],
        );
        assert!(
            unread(&hosted[0])
                && hosted.iter().all(|d| d.track == Some(Conversation::Unknown)
                    && d.memory == Some(Conversation::Unknown)),
            "{name}: unread through the screen no reader names: {hosted:#?}"
        );
        let successor = loop_points(
            &ledger,
            false,
            approvals::open_wall(&ledger, Some("@s-1")),
            &[At {
                seq: 103,
                program: first,
                rows: &claude,
                at: LIMITED_RESET_IN + 3600,
                worked: None,
            }],
        );
        let rows = ledger_scopes(&ledger);
        let _ = std::fs::remove_dir_all(dir);
        assert!(
            successor.iter().all(unread),
            "{name}: a hostless loop that follows holds it: {successor:#?}\n{rows:#?}"
        );
    }
}

/// A HOST'S LOOP REQUIRES THE LEGACY WALL IT TAKES UP AT ITS START, before
/// any decision, and writes it: a loop that ends there (stopped, here) leaves
/// a hostless loop that follows no legacy row to act on from the screen
/// alone. No screen has named the program yet: the first decision of an
/// agent whose screen is all the evidence releases the scope
/// ([`a_limit_is_the_hostless_lanes`], seeded unread). NEGATIVE CONTROL: a
/// loop with no host leaves the legacy row as it is. Measured before
/// (217343ef72, the start's require dropped): the row stayed legacy under
/// the host, the hostless lane's to a loop that follows.
#[test]
fn a_hosts_loop_requires_the_wall_it_takes_up_before_any_decision() {
    let wall = aterm_phase::read(None, &armed_conversation_b(), None)
        .wall
        .expect("B's limit");
    let reason = approvals::wall_wait_reason(
        Some(TEST_NOW + LIMITED_RESET_IN),
        wall.reset.as_deref(),
        wall.kind,
        &wall.message,
    );
    let legacy = Row {
        rule_id: RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 100,
    }
    .to_json((TEST_NOW - 60) * 1000, Some("@s-1"));
    for host in [true, false] {
        let (dir, ledger) = ledger_at(&format!("start-require-{host}"));
        std::fs::write(&ledger, format!("{legacy}\n")).expect("the ledger");
        let mut m = Mock::new(vec![busy_screen()]);
        {
            let mut s = session(&mut m, Some("@s-1".to_owned()));
            s.set_clock(TEST_NOW, PDT);
            s.set_zone_lookup(test_zones);
            s.set_approval_ledger(Some(ledger.clone()));
            // Stopped before its first look: the loop's start, and nothing after it.
            s.stop = Some(Arc::new(AtomicBool::new(true)));
            let opts = SuperviseOpts {
                idle_host: host.then(|| Arc::new(AccountHost::default()) as Arc<dyn IdleHost>),
                ..hosted(30)
            };
            let mut out: Vec<u8> = Vec::new();
            let _ = s.watch(&opts, &mut out);
        }
        let open = approvals::open_wall(&ledger, Some("@s-1"));
        let rows = ledger_scopes(&ledger);
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(
            open.map(|o| o.conversation),
            Some(if host {
                Conversation::Unknown
            } else {
                Conversation::Hostless
            }),
            "host {host}: the wall a loop that follows takes up: {rows:#?}\n{:#?}",
            m.requests
        );
    }
}

/// The wall rows of `rows` ([`ledger_scopes`]).
fn wall_rows(rows: &[String]) -> Vec<String> {
    rows.iter()
        .filter(|r| r.split(' ').next() == Some(RULE_WALL_WAIT))
        .cloned()
        .collect()
}

/// Whether the ledger's rows `rows` end the wall they open: a wall row, and
/// the wall's end row after the last of them.
fn wall_ended(rows: &[String]) -> bool {
    let end = format!("{RULE_WALL_WAIT_END} ");
    rows.iter()
        .rposition(|r| r.split(' ').next() == Some(RULE_WALL_WAIT))
        .is_some_and(|last| rows[last..].iter().any(|r| r.starts_with(&end)))
}

/// Whether `d` is held or resumed on a limit's account: the unread look,
/// the wait on a limit's reset, or the limit's resume.
fn on_a_limits_account(d: &Decided) -> bool {
    d.act
        .contains("the remembered limit's conversation is unread")
        || d.act.contains("resets")
        || d.act.contains("goes on by itself")
        || d.act == format!("act {RULE_LIMIT_RESUME}")
}

/// A screen of Claude Code's own turn end, its conversation at no limit (a
/// `/resume` of another conversation at an armed limit, the lab's capture,
/// 2.1.300): an idle point.
fn claude_idle() -> Vec<String> {
    aterm_phase::prompt::fixtures::screen(
        aterm_phase::prompt::fixtures::RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300,
    )
}

/// A limit a screen-alone agent meets: its name, its wall drawn (`None`:
/// Gemini CLI, whose quota is a dialog, no wall the turn's end reads), the
/// open wall a loop before this one wrote met under it (Gemini CLI's), and
/// the agent's own screen after its wall.
struct AgentLimit {
    name: &'static str,
    wall: Option<Vec<String>>,
    seed: Option<String>,
    own: Vec<String>,
}

/// Muse Code's, agy's and Gemini CLI's limits ([`AgentLimit`]).
fn agent_limits() -> Vec<AgentLimit> {
    use aterm_phase::agy::fixtures::lab as a;
    use aterm_phase::muse::fixtures as u;
    use aterm_phase::prompt::fixtures::screen;
    let limit = muse_limit();
    let reason = approvals::wall_wait_reason(
        Some(TEST_NOW + LIMITED_RESET_IN),
        limit.reset.as_deref(),
        limit.kind,
        &limit.message,
    );
    let gemini_row = Row {
        rule_id: RULE_WALL_WAIT,
        outcome: Outcome::Waiting,
        command: "",
        reason: &reason,
        box_seq: 100,
    }
    .to_json_wall_wait_by(
        (TEST_NOW - 60) * 1000,
        Some("@s-1"),
        &Conversation::Hostless,
        Some(Program::Gemini),
    );
    vec![
        AgentLimit {
            name: "muse",
            wall: Some(screen(u::USAGE_WALL)),
            seed: None,
            own: screen(u::END_OF_TURN_BG_RUNNING),
        },
        AgentLimit {
            name: "agy",
            wall: Some(screen(a::ERROR_429_LONG)),
            seed: None,
            own: screen(a::BG_TASK),
        },
        AgentLimit {
            name: "gemini",
            wall: None,
            seed: Some(gemini_row),
            own: screen(aterm_phase::gemini::fixtures::END_OF_TURN),
        },
    ]
}

/// What a loop decided at [`agent_then`]'s points, and what its ledger was
/// left holding.
struct AgentThen {
    /// Every point's decision, the wall's first where it is drawn.
    all: Vec<Decided>,
    /// The decisions after the wall.
    after: Vec<Decided>,
    /// Each ledger row's rule and conversation ([`ledger_scopes`]).
    rows: Vec<String>,
    /// The program each wall row names ([`wall_programs`]).
    programs: Vec<Option<String>>,
    /// The open wall left for a loop that follows.
    open: Option<approvals::OpenWall>,
}

/// One loop on a scratch ledger, `host` there or not: `l`'s limit (its wall
/// drawn, or its open wall taken up), then `after` at each of `later`'s
/// clocks.
fn agent_then(tag: &str, l: &AgentLimit, host: bool, after: &[String], later: &[i64]) -> AgentThen {
    let (dir, ledger) = ledger_at(&format!("{tag}-{}-{host}", l.name));
    if let Some(row) = &l.seed {
        std::fs::write(&ledger, format!("{row}\n")).expect("the ledger");
    }
    let mut points = Vec::new();
    if let Some(wall) = &l.wall {
        points.push(At {
            seq: 101,
            program: None,
            rows: wall,
            at: 0,
            worked: Some(Duration::from_secs(60)),
        });
    }
    for (&at, seq) in later.iter().zip(102..) {
        points.push(At {
            seq,
            program: None,
            rows: after,
            at,
            worked: None,
        });
    }
    let seed = approvals::open_wall(&ledger, Some("@s-1"));
    let all = loop_points(&ledger, host, seed, &points);
    let rows = ledger_scopes(&ledger);
    let programs = wall_programs(&ledger);
    let open = approvals::open_wall(&ledger, Some("@s-1"));
    let _ = std::fs::remove_dir_all(dir);
    let after = all[usize::from(l.wall.is_some())..].to_vec();
    AgentThen {
        all,
        after,
        rows,
        programs,
        open,
    }
}

/// Whether `t`'s points after the wall are the next agent's own — never on
/// the limit's account, the limit's track and memory gone, the last its
/// continue — and the limit, never scoped, ended at the first of them
/// (`name`'s wall rows all naming it).
fn ended_at_the_next_agent(t: &AgentThen, name: &str) -> bool {
    t.after
        .iter()
        .all(|d| !on_a_limits_account(d) && d.track.is_none() && d.memory.is_none())
        && t.after
            .last()
            .is_some_and(|d| d.act == format!("act {RULE_CONTINUE}"))
        && !wall_rows(&t.rows).is_empty()
        && wall_rows(&t.rows)
            .iter()
            .all(|r| r.ends_with("conversation=None"))
        && wall_ended(&t.rows)
        && t.open.is_none()
        && t.programs.iter().all(|p| p.as_deref() == Some(name))
}

/// A LIMIT MET UNDER MUSE CODE, AGY OR GEMINI CLI IS NO LIMIT OF THE CLAUDE
/// CODE THAT RUNS IN ITS TAB AFTER IT ([`met_under_another`]), a host there
/// or not — the remembered-limit stage's L1, decided on the merits
/// (2026-10-21): a Claude Code decision under a host does NOT require such a
/// limit's conversation, since no read of Claude Code's could ever name it,
/// and the limit ends at Claude Code's point, its end row written and its
/// track and memory gone. Claude Code's idle point is then its own turn's
/// end, the continue policy's act, before the other agent's reset and two
/// days past it; Claude Code's background work alone is no point of that
/// limit's. No row scopes the limit `"conversation":null`, and none is left
/// open for a loop that follows. Measured before (Muse Code's usage wall,
/// then Claude Code's idle screen): under a host the Claude
/// Code decision wrote the limit `"conversation":null` and held Claude Code's
/// tab with a look every five minutes (`limit-conversation-unread`), two
/// days on and looking still; with no host it held Claude Code until the
/// other agent's reset and then typed `usage-resume@v1` into it.
/// NEGATIVE CONTROL: the agent's own screen after its wall keeps the limit,
/// its reset waited for, its scope the hostless lane's.
#[test]
fn a_screen_alone_agents_limit_is_no_limit_of_the_claude_code_after_it() {
    let claude = claude_idle();
    let claude_bg = with_shell(claude_idle());
    for l in &agent_limits() {
        let name = l.name;
        for host in [true, false] {
            let t = agent_then(
                "met-under-agent-claude",
                l,
                host,
                &claude,
                &[30, 2 * 86_400],
            );
            if l.wall.is_some() {
                assert!(
                    t.all[0].act.starts_with("wait "),
                    "{name}, host {host}: the agent's reset is waited for at its wall: {:#?}",
                    t.all
                );
            }
            assert!(
                ended_at_the_next_agent(&t, name),
                "{name}, host {host}: Claude Code's point is its own turn's end, the agent's \
                 limit gone, never scoped, ended: {:#?}\n{:#?}\n{:?}\n{:?}",
                t.all,
                t.rows,
                t.programs,
                t.open
            );
            // Claude Code's background work alone: no point of the
            // agent's limit.
            let t = agent_then(
                "met-under-agent-claude-bg",
                l,
                host,
                &claude_bg,
                &[30, 2 * 86_400],
            );
            assert!(
                t.after
                    .iter()
                    .all(|d| d.act == "nothing" && d.track.is_none() && d.memory.is_none())
                    && wall_ended(&t.rows)
                    && t.open.is_none(),
                "{name}, host {host}: Claude Code's background work stands for no wall of the \
                 agent's: {:#?}\n{:#?}",
                t.all,
                t.rows
            );
            // NEGATIVE CONTROL: the agent's own screen after its wall.
            let t = agent_then("met-under-agent-own", l, host, &l.own, &[30]);
            assert!(
                t.after.iter().all(|d| d.act.starts_with("wait ")
                    && on_a_limits_account(d)
                    && !d.act.contains("unread")
                    && d.track == Some(Conversation::Hostless))
                    && !wall_ended(&t.rows)
                    && t.open.is_some(),
                "{name}, host {host}, CONTROL: the agent's own screen keeps its limit: \
                 {:#?}\n{:#?}",
                t.all,
                t.rows
            );
        }
    }
}

/// A LIMIT MET UNDER ONE SCREEN-ALONE AGENT IS NO LIMIT OF ANOTHER THAT RUNS
/// IN ITS TAB AFTER IT ([`met_under_another`], T1 of the review of
/// `b9/walls-2`): Muse Code's limit, then agy or Gemini CLI idle at its
/// turn's end; agy's, then Muse Code or Gemini CLI; Gemini CLI's (the open
/// wall a loop before wrote), then Muse Code or agy — a host there or not.
/// The next agent's points, half a minute and two days on, are its own:
/// never a wait on the first agent's reset, never its resume; the limit,
/// never scoped, ends at the first of them, its end row written and none
/// left open. Measured before the cross-program end was held for these
/// pairs (a mutant that ends only a limit crossing Claude Code, the whole
/// suite passing): Muse Code's limit held agy's and Gemini CLI's tab until
/// Muse Code's reset (`wait 15172s: usage-session resets`) and then typed
/// `usage-resume@v1` into it, hosted and hostless. NEGATIVE CONTROL: each
/// agent's own screen after its limit keeps it
/// ([`a_screen_alone_agents_limit_is_no_limit_of_the_claude_code_after_it`]).
#[test]
fn a_screen_alone_agents_limit_is_no_limit_of_another_screen_alone_agent_after_it() {
    use aterm_phase::prompt::fixtures::screen;
    let idle = [
        ("muse", screen(aterm_phase::muse::fixtures::END_OF_TURN)),
        ("agy", screen(aterm_phase::agy::fixtures::lab::END_OF_TURN)),
        ("gemini", screen(aterm_phase::gemini::fixtures::END_OF_TURN)),
    ];
    let mut pairs = 0;
    for l in &agent_limits() {
        for (next, rows) in idle.iter().filter(|(n, _)| *n != l.name) {
            for host in [true, false] {
                let t = agent_then(
                    &format!("met-under-agent-{next}"),
                    l,
                    host,
                    rows,
                    &[30, 2 * 86_400],
                );
                assert!(
                    ended_at_the_next_agent(&t, l.name),
                    "{} then {next}, host {host}: the next agent's points are its own, the \
                     limit ended: {:#?}\n{:#?}\n{:?}\n{:?}",
                    l.name,
                    t.all,
                    t.rows,
                    t.programs,
                    t.open
                );
                pairs += 1;
            }
        }
    }
    assert_eq!(pairs, 12, "every ordered pair, hosted and hostless");
}

/// The points of [`claude_codes_unread_limit_holds_no_screen_alone_agent_in_its_tab`]:
/// Claude Code's limit met by a turn (`armed`), its notice hidden under a
/// shell half a minute on (`claude`), then the agent's idle screen `agent`
/// a minute, an hour and two days on.
fn held_points<'a>(
    armed: &'a [String],
    claude: &'a [String],
    agent: Option<&'a [String]>,
) -> Vec<At<'a>> {
    let mut points = vec![
        At {
            seq: 101,
            program: None,
            rows: armed,
            at: 0,
            worked: Some(Duration::from_secs(60)),
        },
        At {
            seq: 102,
            program: None,
            rows: claude,
            at: 30,
            worked: None,
        },
    ];
    for (at, seq) in [60, 3600, 2 * 86_400].into_iter().zip(103..) {
        if let Some(rows) = agent {
            points.push(At {
                seq,
                program: None,
                rows,
                at,
                worked: None,
            });
        }
    }
    points
}

/// CLAUDE CODE'S UNREAD LIMIT HOLDS NO MUSE CODE, AGY OR GEMINI CLI THAT
/// RUNS IN ITS TAB AFTER IT — the remembered-limit stage's L2, bounded at
/// the agent's first point ([`met_under_another`]). One tab under a host
/// whose read names no conversation: Claude Code meets conversation B's
/// five-hour limit (unread, `"conversation":null`) and hides its notice under
/// a shell, where the limit holds Claude Code's own tab, unread; then Claude
/// Code exits and the agent runs there, idle at its turn's end. Its first
/// point is its own: the limit ends there (its end row written, its track
/// and memory gone, its scope never released: every wall row stays
/// `"conversation":null`, met under Claude Code), and the agent's point and
/// every one after it, an hour and two days on, is its own turn's end —
/// never the unread look, never a wait on Claude Code's reset, never Claude
/// Code's resume. So too on the hostless lane, and for a loop that follows
/// taking the limit up from the ledger at the agent's screen. Measured before
/// (Muse Code's idle screen, 2026-10-21): every one of the agent's points
/// waited `now + 5 min` ("the remembered limit's conversation is unread; no
/// act on its account"), at a minute, an hour, six hours and two days — a
/// look every five minutes for as long as the agent sat idle, nothing typed
/// and nothing said; with no host the agent waited for Claude Code's reset
/// and was typed `usage-resume@v1`. NEGATIVE CONTROL: the same limit at
/// Claude Code's own screen stays unread and held, for this loop and for a
/// hostless loop that follows it an hour past the reset: nothing is typed
/// into a conversation no read confirmed.
#[test]
fn claude_codes_unread_limit_holds_no_screen_alone_agent_in_its_tab() {
    use aterm_phase::prompt::fixtures::screen;
    let claude = with_shell(claude_idle());
    let unread = |d: &Decided| {
        d.act
            .contains("the remembered limit's conversation is unread")
    };
    let past_reset = |ledger: &Path| {
        approvals::open_wall(ledger, Some("@s-1"))
            .and_then(|o| o.reset_unix)
            .expect("the open wall's reset")
            + 3600
            - TEST_NOW
    };
    let armed = armed_conversation_b();
    for (name, agent) in [
        ("muse", screen(aterm_phase::muse::fixtures::END_OF_TURN)),
        ("agy", screen(aterm_phase::agy::fixtures::lab::END_OF_TURN)),
        ("gemini", screen(aterm_phase::gemini::fixtures::END_OF_TURN)),
    ] {
        for host in [true, false] {
            let (dir, ledger) = ledger_at(&format!("claude-then-{name}-{host}"));
            let points = held_points(&armed, &claude, Some(&agent));
            let decided = loop_points(&ledger, host, None, &points);
            let rows = ledger_scopes(&ledger);
            let programs = wall_programs(&ledger);
            let open = approvals::open_wall(&ledger, Some("@s-1"));
            let _ = std::fs::remove_dir_all(dir);
            if host {
                assert!(
                    unread(&decided[1]) && decided[1].track == Some(Conversation::Unknown),
                    "{name}, CONTROL: the limit holds Claude Code's own tab, unread: \
                     {decided:#?}"
                );
            }
            assert!(
                decided[2..]
                    .iter()
                    .all(|d| !on_a_limits_account(d) && d.track.is_none() && d.memory.is_none()),
                "{name}, host {host}: the agent's points are its own: {decided:#?}\n{rows:#?}"
            );
            let scope = if host {
                "conversation=Some(Null)"
            } else {
                "conversation=None"
            };
            assert!(
                !wall_rows(&rows).is_empty()
                    && wall_rows(&rows).iter().all(|r| r.ends_with(scope))
                    && wall_ended(&rows)
                    && open.is_none()
                    && programs.iter().all(|p| p.as_deref() == Some("claude")),
                "{name}, host {host}: the limit, never released, ends at the agent's point: \
                 {rows:#?}\n{programs:?}"
            );
        }
        // A loop that follows takes the limit up at the agent's screen,
        // past its reset; and, the CONTROL, at Claude Code's.
        for (host, at_agent) in [(true, true), (false, true), (false, false)] {
            let (dir, ledger) = ledger_at(&format!("claude-left-{name}-{host}-{at_agent}"));
            let points = held_points(&armed, &claude, None);
            let _ = loop_points(&ledger, true, None, &points);
            let open = approvals::open_wall(&ledger, Some("@s-1"));
            assert!(
                open.as_ref()
                    .is_some_and(|o| o.program == Some(Program::Claude)
                        && o.conversation == Conversation::Unknown),
                "{name}: the first loop leaves Claude Code's limit open, unread: {open:?}"
            );
            let rows: &[String] = if at_agent { &agent } else { &claude };
            let successor = loop_points(
                &ledger,
                host,
                open,
                &[At {
                    seq: 104,
                    program: None,
                    rows,
                    at: past_reset(&ledger),
                    worked: None,
                }],
            );
            let scopes = ledger_scopes(&ledger);
            let left = approvals::open_wall(&ledger, Some("@s-1"));
            let _ = std::fs::remove_dir_all(dir);
            if at_agent {
                assert!(
                    successor.iter().all(|d| !on_a_limits_account(d)
                        && d.track.is_none()
                        && d.memory.is_none())
                        && wall_ended(&scopes)
                        && left.is_none(),
                    "{name}, host {host}: a loop that follows ends the limit at the agent's \
                     point: {successor:#?}\n{scopes:#?}"
                );
            } else {
                assert!(
                    successor.iter().all(unread) && left.is_some(),
                    "{name}, CONTROL: a hostless loop that follows holds it at Claude Code's \
                     screen: {successor:#?}\n{scopes:#?}"
                );
            }
        }
    }
}

/// A LIMIT HELD UNREAD AT ITS OWN AGENT'S SCREEN IS TOLD TO THE OWNER AN HOUR
/// PAST ITS RESET, AND STILL HELD ([`Exception::unread_limit`], FOREVER:
/// NEEDS YOU is loud and never blocking) — the bound on the remembered-limit
/// stage's L2 that the agent itself still needs. Under a host whose read
/// names no conversation, Claude Code meets conversation B's five-hour limit
/// (unread) and hides its notice under a shell: the tab is looked at every
/// five minutes with nothing typed (no read confirms the conversation the
/// screen holds), half an hour past the reset with nothing said, and from an
/// hour past it under the exception, two days on too; a hostless loop that
/// takes the unread limit up is told the same. NEGATIVE CONTROL: the same
/// limit on the hostless lane, its conversation the screen's, is no hold and
/// tells nothing of the kind. Measured before: two days past the reset the
/// point still waited `now + 5 min`, and no exception stood.
#[test]
fn a_limit_held_unread_past_its_reset_is_told_and_still_held() {
    use crate::supervise::policy::exception::{Exception, FUTILE_AFTER};
    let e = Exception::unread_limit();
    assert!(
        e.why.chars().count() <= aterm_messages::need::STEP_CAP
            && e.action.chars().count() <= aterm_messages::need::STEP_CAP
            && aterm_messages::need::Step::new(&e.action).fault().is_none(),
        "the digest's words: {e:?}"
    );
    let told = format!("{} {}", e.kind.word(), e.why);
    let unread = |d: &Decided| {
        d.act
            .contains("the remembered limit's conversation is unread")
    };
    let armed = armed_conversation_b();
    let claude = with_shell(claude_idle());
    // The reset the loop places for B's limit.
    let (dir, ledger) = ledger_at("unread-told-reset");
    let _ = loop_points(
        &ledger,
        true,
        None,
        &[At {
            seq: 101,
            program: None,
            rows: &armed,
            at: 0,
            worked: Some(Duration::from_secs(60)),
        }],
    );
    let reset = approvals::open_wall(&ledger, Some("@s-1"))
        .and_then(|o| o.reset_unix)
        .expect("the reset placed")
        - TEST_NOW;
    let _ = std::fs::remove_dir_all(dir);
    let bound = i64::try_from(FUTILE_AFTER.as_secs()).expect("an hour");
    let clocks = [30, reset + bound - 600, reset + bound + 300, 2 * 86_400];
    let points = |claude: &[String]| -> Vec<(i64, Vec<String>)> {
        clocks.iter().map(|&at| (at, claude.to_vec())).collect()
    };
    let run = |tag: &str, host: bool| {
        let (dir, ledger) = ledger_at(tag);
        let later = points(&claude);
        let mut at = vec![At {
            seq: 101,
            program: None,
            rows: &armed,
            at: 0,
            worked: Some(Duration::from_secs(60)),
        }];
        for ((clock, rows), seq) in later.iter().zip(102..) {
            at.push(At {
                seq,
                program: None,
                rows,
                at: *clock,
                worked: None,
            });
        }
        let decided = loop_points(&ledger, host, None, &at);
        // A hostless loop that takes the limit up two hours past its end.
        let successor = loop_points(
            &ledger,
            false,
            approvals::open_wall(&ledger, Some("@s-1")),
            &[At {
                seq: 110,
                program: None,
                rows: &claude,
                at: reset + 2 * bound,
                worked: None,
            }],
        );
        let _ = std::fs::remove_dir_all(dir);
        (decided, successor)
    };
    let (held, successor) = run("unread-told", true);
    assert!(
        held[1..].iter().all(unread),
        "the limit holds Claude Code's tab unread: {held:#?}"
    );
    assert!(
        held[1..3].iter().all(|d| d.told.is_none()),
        "nothing is told before the hour past the reset: {held:#?}"
    );
    assert!(
        held[3..]
            .iter()
            .all(|d| d.told.as_deref() == Some(told.as_str())),
        "an hour past the reset the owner is told, the hold standing: {held:#?}"
    );
    assert!(
        successor
            .iter()
            .all(|d| unread(d) && d.told.as_deref() == Some(told.as_str())),
        "a hostless loop that takes the unread limit up is told too: {successor:#?}"
    );
    // SAID, in the journal, once, while the loop goes on waiting: each
    // decision carried out as the loop does.
    let (dir, ledger) = ledger_at("unread-told-journal");
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, Some("@s-1".to_owned()));
    s.set_zone_lookup(test_zones);
    s.set_mono_standing();
    s.set_approval_ledger(Some(ledger.clone()));
    let mut notes = Vec::new();
    s.ledger = approvals::Ledger::open(Some(&ledger), Some("@s-1"), &mut notes);
    let opts = SuperviseOpts {
        idle_host: Some(Arc::new(AccountHost::default()) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut heard = Heard::default();
    let mut waits = Vec::new();
    for (seq, at, rows, worked) in [
        (101, 0, &armed, Some(Duration::from_secs(60))),
        (102, clocks[1], &claude, None),
        (103, clocks[2], &claude, None),
        (104, clocks[3], &claude, None),
    ] {
        s.set_clock(TEST_NOW + at, PDT);
        let point = point_at(&s, rows.clone(), seq);
        let action = s
            .turn_end_at_point(&point, worked, &opts)
            .expect("a decision");
        waits.push(matches!(action, TurnEndAction::WaitUntil { .. }));
        s.turn_end_execute(&point, action, &opts, &[], &mut heard)
            .expect("carried out");
    }
    drop(s);
    let _ = std::fs::remove_dir_all(dir);
    let said: Vec<&String> = heard
        .noted
        .iter()
        .filter(|l| l.starts_with("EXCEPTION ") || l.starts_with("CLEARED "))
        .collect();
    assert!(
        waits.iter().all(|w| *w)
            && said.len() == 1
            && said[0].starts_with("EXCEPTION seq=103 kind=futile when=later ")
            && said[0].contains(&e.why)
            && said[0].ends_with(&e.action),
        "told once, in the journal, the loop waiting on: {waits:?} {said:#?}\n{:#?}",
        heard.noted
    );
    // NEGATIVE CONTROL: the hostless lane's limit, its conversation read.
    let (dir, ledger) = ledger_at("unread-told-hostless");
    let mut at = vec![At {
        seq: 101,
        program: None,
        rows: &armed,
        at: 0,
        worked: Some(Duration::from_secs(60)),
    }];
    let later = points(&claude);
    for ((clock, rows), seq) in later.iter().zip(102..) {
        at.push(At {
            seq,
            program: None,
            rows,
            at: *clock,
            worked: None,
        });
    }
    let hostless = loop_points(&ledger, false, None, &at);
    let _ = std::fs::remove_dir_all(dir);
    assert!(
        hostless
            .iter()
            .all(|d| !unread(d) && d.told.as_deref() != Some(told.as_str())),
        "CONTROL: the hostless lane's limit is no unread hold: {hostless:#?}"
    );
}

/// A LIMIT HELD UNREAD THAT NAMED NO RESET IS TOLD AN HOUR AFTER IT
/// APPEARED, AND STILL HELD (T2 of the review of `b9/walls-2`): under a host
/// whose read names no conversation, Claude Code meets its Fable model's
/// limit, whose notice names no reset (2.1.301), and hides it under a shell.
/// The tab is held unread with nothing said for the first hour, and from an
/// hour after the limit appeared (no reset to count from) the owner is told
/// ([`Exception::unread_limit`]), two days on too, the hold standing.
/// NEGATIVE CONTROLS: the hostless lane's same limit is no unread hold and
/// tells nothing of the kind; and a hold counted from a reset alone never
/// tells this one — measured on that mutant, the tab waited two days on its
/// five-minute looks with nothing told.
///
/// [`Exception::unread_limit`]: crate::supervise::policy::exception::Exception::unread_limit
#[test]
fn a_limit_held_unread_with_no_reset_is_told_an_hour_after_it_appeared() {
    use crate::supervise::policy::exception::{Exception, FUTILE_AFTER};
    use aterm_phase::prompt::fixtures::{WALL_FABLE_NO_RESET_2_1_301, screen};
    let fable = screen(WALL_FABLE_NO_RESET_2_1_301);
    assert!(
        aterm_phase::read(None, &fable, None)
            .wall
            .is_some_and(|w| w.reset.is_none()),
        "PRECONDITION: a limit that names no reset"
    );
    let e = Exception::unread_limit();
    let told = format!("{} {}", e.kind.word(), e.why);
    let unread = |d: &Decided| {
        d.act
            .contains("the remembered limit's conversation is unread")
    };
    let claude = with_shell(claude_idle());
    let bound = i64::try_from(FUTILE_AFTER.as_secs()).expect("an hour");
    let clocks = [30, bound - 600, bound + 300, 2 * 86_400];
    let mut points = vec![At {
        seq: 101,
        program: None,
        rows: &fable,
        at: 0,
        worked: Some(Duration::from_secs(60)),
    }];
    for (&at, seq) in clocks.iter().zip(102..) {
        points.push(At {
            seq,
            program: None,
            rows: &claude,
            at,
            worked: None,
        });
    }
    let run = |tag: &str, host: bool| {
        let (dir, ledger) = ledger_at(tag);
        let decided = loop_points(&ledger, host, None, &points);
        let _ = std::fs::remove_dir_all(dir);
        decided
    };
    let held = run("unread-no-reset-told", true);
    assert!(
        held[1..].iter().all(unread),
        "the limit holds Claude Code's tab unread: {held:#?}"
    );
    assert!(
        held[1..3].iter().all(|d| d.told.is_none()),
        "nothing is told in the hour after it appeared: {held:#?}"
    );
    assert!(
        held[3..]
            .iter()
            .all(|d| d.told.as_deref() == Some(told.as_str())),
        "an hour after it appeared the owner is told, the hold standing: {held:#?}"
    );
    // NEGATIVE CONTROL: the hostless lane's limit, its conversation read.
    let hostless = run("unread-no-reset-hostless", false);
    assert!(
        hostless
            .iter()
            .all(|d| !unread(d) && d.told.as_deref() != Some(told.as_str())),
        "CONTROL: the hostless lane's limit is no unread hold: {hostless:#?}"
    );
}

/// A loop on the session's ledger at `ledger`, `host` there or not, stopped
/// before its first look: its start, and nothing after it.
fn stopped_at_start(ledger: &Path, host: bool) {
    let mut m = Mock::new(vec![busy_screen()]);
    let mut s = session(&mut m, Some("@s-1".to_owned()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_approval_ledger(Some(ledger.to_path_buf()));
    s.stop = Some(Arc::new(AtomicBool::new(true)));
    let opts = SuperviseOpts {
        idle_host: host.then(|| Arc::new(AccountHost::default()) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let mut out: Vec<u8> = Vec::new();
    let _ = s.watch(&opts, &mut out);
}

/// A HOST'S LOOP TAKES UP THE OPEN WALL OF AN AGENT WHOSE SCREEN IS ALL ITS
/// EVIDENCE WITH NO CHURN: the row names the program the wall was met under
/// (Muse Code, agy, Gemini CLI: [`screen_alone`]), so the loop's start under
/// a host does not require its conversation, and the first decision at that
/// agent's screen has no scope to release. The ledger keeps the one wall row
/// it had, and the reset is waited for. NEGATIVE CONTROLS: the same row
/// naming no program (an older build's) is required at the start
/// (`"conversation":null`) and released at the first decision, two rows as
/// before; a row naming Claude Code is required at the start, and so is one
/// naming a program this build does not know (read as a screen no reader
/// names). Measured before: every such row was written twice, `null` at the
/// start and the release at the first decision.
#[test]
fn a_hosts_loop_takes_up_a_screen_alone_agents_wall_with_no_churn() {
    use aterm_phase::prompt::fixtures::screen;
    let agy_limit = aterm_phase::read(
        None,
        &screen(aterm_phase::agy::fixtures::lab::ERROR_429_LONG),
        None,
    )
    .wall
    .expect("agy's 429");
    let row = |wall: &aterm_phase::Wall, program: Option<Program>| {
        let reason = approvals::wall_wait_reason(
            Some(TEST_NOW + LIMITED_RESET_IN),
            wall.reset.as_deref(),
            wall.kind,
            &wall.message,
        );
        Row {
            rule_id: RULE_WALL_WAIT,
            outcome: Outcome::Waiting,
            command: "",
            reason: &reason,
            box_seq: 100,
        }
        .to_json_wall_wait_by(
            (TEST_NOW - 60) * 1000,
            Some("@s-1"),
            &Conversation::Hostless,
            program,
        )
    };
    let walls = |ledger: &Path| -> Vec<String> {
        ledger_scopes(ledger)
            .into_iter()
            .filter(|r| r.split(' ').next() == Some(RULE_WALL_WAIT))
            .collect()
    };
    // The wall rows and the first decision a host's loop leaves, its start
    // and then that decision, over `body`.
    let take_up = |tag: &str, body: &str, after: &[String]| {
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, format!("{body}\n")).expect("the ledger");
        stopped_at_start(&ledger, true);
        let started = walls(&ledger);
        let decided = loop_points(
            &ledger,
            true,
            approvals::open_wall(&ledger, Some("@s-1")),
            &[At {
                seq: 101,
                program: None,
                rows: after,
                at: 0,
                worked: None,
            }],
        );
        let rows = walls(&ledger);
        let _ = std::fs::remove_dir_all(dir);
        (started, decided, rows)
    };
    let first = format!("{RULE_WALL_WAIT} conversation=None");
    let waits = |d: &[Decided]| {
        d.iter()
            .all(|d| d.act.starts_with("wait ") && !d.act.contains("unread"))
    };
    for (name, program, wall, after) in [
        (
            "muse",
            Program::Muse,
            muse_limit(),
            screen(aterm_phase::muse::fixtures::END_OF_TURN_BG_RUNNING),
        ),
        (
            "agy",
            Program::Agy,
            agy_limit,
            screen(aterm_phase::agy::fixtures::lab::BG_TASK),
        ),
        (
            "gemini",
            Program::Gemini,
            muse_limit(),
            screen(aterm_phase::gemini::fixtures::END_OF_TURN),
        ),
    ] {
        let named = row(&wall, Some(program));
        let (started, decided, rows) = take_up(&format!("churn-{name}"), &named, &after);
        assert_eq!(
            (started, rows),
            (vec![first.clone()], vec![first.clone()]),
            "{name}: no row written at the start or the first decision: {decided:#?}"
        );
        assert!(
            waits(&decided)
                && decided
                    .iter()
                    .all(|d| d.track == Some(Conversation::Hostless)),
            "{name}: the reset waited for: {decided:#?}"
        );
        // NEGATIVE CONTROL: the row an older build wrote, naming no program.
        let legacy = row(&wall, None);
        let (started, decided, rows) = take_up(&format!("churn-{name}-legacy"), &legacy, &after);
        let null = format!("{RULE_WALL_WAIT} conversation=Some(Null)");
        assert_eq!(
            (started, rows),
            (
                vec![first.clone(), null.clone()],
                vec![first.clone(), null, first.clone()]
            ),
            "{name}, CONTROL: a legacy row is required, then released: {decided:#?}"
        );
        assert!(waits(&decided), "{name}, CONTROL: {decided:#?}");
    }
    // NEGATIVE CONTROLS: a row naming Claude Code is required at the start,
    // and so is one naming a program this build does not know, read as a
    // screen no reader names.
    let claude = aterm_phase::read(None, &armed_conversation_b(), None)
        .wall
        .expect("B's limit");
    let named = row(&claude, Some(Program::Claude));
    for (tag, body, program) in [
        ("churn-claude", named.clone(), Program::Claude),
        (
            "churn-unknown-name",
            named.replace("\"program\":\"claude\"", "\"program\":\"zork\""),
            Program::Generic,
        ),
    ] {
        assert!(body.contains("\"program\":"), "{tag}: {body}");
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, format!("{body}\n")).expect("the ledger");
        stopped_at_start(&ledger, true);
        let open = approvals::open_wall(&ledger, Some("@s-1"));
        let _ = std::fs::remove_dir_all(dir);
        assert_eq!(
            open.map(|o| (o.conversation, o.program)),
            Some((Conversation::Unknown, Some(program))),
            "{tag}, CONTROL: required at the start"
        );
    }
}

/// What one loop left that typed a try at conversation B's five-hour limit
/// ([`try_then`]).
#[derive(Debug)]
struct Tried {
    /// The limit's track's tries after the try's point (`None`: the track
    /// ended).
    open: Option<usize>,
    /// The decision at the screen after it.
    then: String,
    /// The track's tries after that.
    still: Option<usize>,
    /// What was typed after the try.
    typed: usize,
    /// The try a loop that follows takes up from the ledger.
    taken_up: bool,
    /// The ledger's rows, each one's rule and scope ([`ledger_scopes`]).
    rows: Vec<String>,
}

/// ONE LOOP ON A CLAUDE CODE LIMIT'S TRY, `host` a host whose read names no
/// conversation or none: conversation B's five-hour limit met by a turn, its
/// reset waited for and the try typed there (`usage-resume@v1`); then the
/// try's point, past its take window, at `unnamed` (the program `status`
/// names there `program`); then Claude Code's screen `then`, a minute on.
fn try_then(
    tag: &str,
    host: bool,
    program: Option<&str>,
    unnamed: &[String],
    then: &[String],
) -> Tried {
    let wall = armed_conversation_b();
    let bucket = aterm_phase::WallKind::UsageSession;
    let (dir, ledger) = ledger_at(tag);
    let mut m = Mock::new(vec![wall.clone()]);
    let mut s = session(&mut m, Some("@s-1".to_owned()));
    s.set_clock(TEST_NOW, PDT);
    s.set_zone_lookup(test_zones);
    s.set_mono_standing();
    // A try's take window is over at its first read after it.
    s.set_turn_end_timing(TurnEndTiming {
        take_within: Duration::ZERO,
        reset_jitter: Duration::ZERO,
        ..TurnEndTiming::default()
    });
    s.set_approval_ledger(Some(ledger.clone()));
    let mut notes = Vec::new();
    s.ledger = approvals::Ledger::open(Some(&ledger), Some("@s-1"), &mut notes);
    assert!(notes.is_empty(), "the scratch ledger opens: {notes:?}");
    let opts = SuperviseOpts {
        idle_host: host.then(|| Arc::new(AccountHost::default()) as Arc<dyn IdleHost>),
        ..hosted(30)
    };
    let decided = |s: &Session<'_, Mock>, a: &TurnEndAction| match a {
        TurnEndAction::WaitUntil { until, why, .. } => {
            format!("wait {}s: {why}", s.unix_of(*until) - s.now_unix())
        }
        TurnEndAction::Nothing => "nothing".to_owned(),
        a => format!("act {}", a.rule_id().unwrap_or("-")),
    };
    // The wall, met by a turn: its reset waited for.
    let p = point_at(&s, wall.clone(), 101);
    let reset = match s.turn_end_at_point(&p, Some(Duration::from_secs(60)), &opts) {
        Ok(TurnEndAction::WaitUntil { until, .. }) => s.unix_of(until),
        other => panic!("{tag}: the reset waited for: {other:?}"),
    };
    // At the reset, the try, typed.
    s.set_clock(reset, PDT);
    let p = point_at(&s, wall.clone(), 102);
    let act = s.turn_end_at_point(&p, None, &opts).expect("the try");
    assert_eq!(act.rule_id(), Some(RULE_LIMIT_RESUME), "{tag}: {act:?}");
    s.pass_wake_settle();
    s.turn_end_execute(&p, act, &opts, &[], &mut Alone)
        .expect("typed");
    assert_eq!(s.turn_end.wall_attempts(bucket), Some(1), "{tag}: the try");
    let turns = |s: &Session<'_, Mock>| {
        s.ctl
            .requests
            .iter()
            .filter(|r| r.contains(" turn "))
            .count()
    };
    let typed = turns(&s);
    // The try's point: a screen no reader names.
    s.set_clock(reset + 60, PDT);
    s.program = program.map(str::to_owned);
    s.ctl.screens = vec![unnamed.to_vec()];
    s.ctl.served = 0;
    let p = point_at(&s, unnamed.to_vec(), 103);
    let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
    let _ = s.turn_end_execute(&p, a, &opts, &[], &mut Alone);
    let open = s.turn_end.wall_attempts(bucket);
    // The screen after it.
    s.set_clock(reset + 120, PDT);
    s.program = None;
    s.ctl.screens = vec![then.to_vec()];
    s.ctl.served = 0;
    let p = point_at(&s, then.to_vec(), 104);
    let a = s.turn_end_at_point(&p, None, &opts).expect("a decision");
    let then = decided(&s, &a);
    let _ = s.turn_end_execute(&p, a, &opts, &[], &mut Alone);
    let still = s.turn_end.wall_attempts(bucket);
    let typed = turns(&s) - typed;
    drop(s);
    let taken_up = approvals::usage_attempt(&ledger, Some("@s-1")).is_some();
    let rows = ledger_scopes(&ledger);
    let _ = std::fs::remove_dir_all(dir);
    Tried {
        open,
        then,
        still,
        typed,
        taken_up,
        rows,
    }
}

/// The transcript view of a Claude Code installed by npm (`status` names
/// `node`): no frame or box of Claude Code's own, a screen no reader names.
fn npm_transcript_view() -> Vec<String> {
    rows(&[
        "⏺ Bash(make)",
        "  ⎿  cc -c parser.c",
        "     cc -c lexer.c",
        "",
        "  Showing detailed transcript · ctrl+o to toggle",
    ])
}

/// A TRY AT A CLAUDE CODE LIMIT NO READ NAMED IS NOT TAKEN AT A SCREEN NO
/// READER NAMES (`retry_taken` and the `owns_answer` that gates it on the
/// limit's conversation, end to end through the loop's own decisions). Under
/// a host whose read names no conversation, Claude Code meets conversation
/// B's five-hour limit (unread), the loop waits for its reset and types the
/// try there. The try's point, past its take window, is a screen identify
/// cannot name: the shell Claude Code exited to (`status` names `zsh`), or a
/// Claude Code installed by npm (`node`) whose transcript view draws no
/// frame. Under a host that screen's evidence is unread too, so it is no
/// answer of the limit's own conversation: the try stays open, on the track
/// and in the ledger (a loop that follows takes it up), and at Claude Code's
/// own screen after it, its notice gone, nothing is typed: the remembered
/// limit's conversation is unread. NEGATIVE CONTROL: with no host the screen
/// is all the evidence there is, and the same screen takes the try (the
/// limit's track ends there). (`owed`, the other premise of that gate, is a
/// retry's seed alone, `TurnEndState::seed_retry`: never a limit's.)
#[test]
fn a_try_at_an_unread_limit_stays_open_through_a_screen_no_reader_names() {
    use aterm_phase::prompt::fixtures::{RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300, screen};
    let claude = screen(RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300);
    for (name, program, unnamed) in [
        ("exited", Some("zsh"), a_build()),
        ("npm", Some("node"), npm_transcript_view()),
    ] {
        assert_eq!(
            aterm_phase::identify(program, &unnamed).program(),
            Program::Generic,
            "{name}: a screen identify cannot name"
        );
        let t = try_then(
            &format!("try-unnamed-{name}"),
            true,
            program,
            &unnamed,
            &claude,
        );
        assert!(
            t.open == Some(1)
                && t.still == Some(1)
                && t.taken_up
                && t.rows
                    .contains(&format!("{RULE_LIMIT_RESUME} conversation=Some(Null)")),
            "{name}: the try, typed unread, stays open: {t:#?}"
        );
        assert!(
            t.typed == 0
                && t.then
                    .contains("the remembered limit's conversation is unread"),
            "{name}: nothing is typed on an unconfirmed conversation: {t:#?}"
        );
        let control = try_then(
            &format!("try-unnamed-{name}-hostless"),
            false,
            program,
            &unnamed,
            &claude,
        );
        assert_eq!(
            control.open, None,
            "{name}, CONTROL: with no host the screen takes the try: {control:#?}"
        );
    }
}

/// A LIMIT'S TRY CARRIES THE PROGRAM ITS WALL WAS MET UNDER: the one the wall
/// row before it names ([`approvals::UsageAttempt::program`]), so a loop that
/// takes up the try alone (its typed row closed the open wall) holds the
/// limit as the loop that typed it did. Claude Code's limit, its try typed
/// unread under a host: a host's loop that takes the try up keeps it unread
/// at a Muse Code decision. Muse Code's limit, its try typed on the hostless
/// lane: a host's loop does not require its conversation at the start.
/// NEGATIVE CONTROLS: a try after a wall row that names no program (an older
/// build's) is released by the Muse Code decision and required at the
/// start, as before.
#[test]
fn a_limits_try_carries_the_program_its_wall_was_met_under() {
    use crate::supervise::policy::turn_end::LimitTry;
    let wall = aterm_phase::read(None, &armed_conversation_b(), None)
        .wall
        .expect("B's limit");
    let reason = approvals::wall_wait_reason(
        Some(TEST_NOW + LIMITED_RESET_IN),
        wall.reset.as_deref(),
        wall.kind,
        &wall.message,
    );
    let tried = approvals::act_reason(Some(LimitTry {
        bucket: false,
        in_place: false,
        attempts: 1,
        entitlement: false,
    }));
    // The ledger a loop that typed the limit's try leaves: the wall's row,
    // met under `program`, its scope `scope`, then the try typed at it.
    let books = |program: Option<Program>, scope: &Conversation| {
        let wall_row = Row {
            rule_id: RULE_WALL_WAIT,
            outcome: Outcome::Waiting,
            command: "",
            reason: &reason,
            box_seq: 100,
        }
        .to_json_wall_wait_by((TEST_NOW - 120) * 1000, Some("@s-1"), scope, program);
        let try_row = Row {
            rule_id: RULE_LIMIT_RESUME,
            outcome: Outcome::Typed,
            command: "keep going",
            reason: &tried,
            box_seq: 101,
        }
        .to_json_scoped((TEST_NOW - 60) * 1000, Some("@s-1"), None, None, scope);
        format!("{wall_row}\n{try_row}\n")
    };
    // A host's loop that takes the try in `body` up: the limit's scope after
    // its start, and after a decision's read of Muse Code's screen.
    let take_up = |tag: &str, body: &str| {
        let (dir, ledger) = ledger_at(tag);
        std::fs::write(&ledger, body).expect("the ledger");
        let attempt = approvals::usage_attempt(&ledger, Some("@s-1"));
        let mut m = Mock::new(vec![busy_screen()]);
        let mut s = session(&mut m, Some("@s-1".to_owned()));
        s.set_clock(TEST_NOW, PDT);
        seed_usage_act(
            &mut s.turn_end,
            &ledger,
            Some("@s-1"),
            TEST_NOW * 1000,
            Instant::now(),
        );
        s.seed_wall_memory(approvals::open_wall(&ledger, Some("@s-1")));
        assert!(
            s.wall_memory.is_none() && s.turn_end.has_limit_track(),
            "{tag}: the try alone is taken up: {attempt:?}"
        );
        s.require_seeded_limit_conversation();
        let started = s.turn_end.limit_conversation().cloned();
        s.scope_limit_conversation(Program::Muse, true, 102);
        let decided = s.turn_end.limit_conversation().cloned();
        drop(s);
        let _ = std::fs::remove_dir_all(dir);
        (attempt.and_then(|a| a.program), started, decided)
    };
    let unread = Some(Conversation::Unknown);
    let hostless = Some(Conversation::Hostless);
    assert_eq!(
        take_up(
            "try-claude",
            &books(Some(Program::Claude), &Conversation::Unknown)
        ),
        (Some(Program::Claude), unread.clone(), unread.clone()),
        "Claude Code's try stays unread at a Muse Code decision"
    );
    assert_eq!(
        take_up(
            "try-muse",
            &books(Some(Program::Muse), &Conversation::Hostless)
        ),
        (Some(Program::Muse), hostless.clone(), hostless.clone()),
        "Muse Code's try is not required at a host's start"
    );
    assert_eq!(
        take_up("try-legacy-unread", &books(None, &Conversation::Unknown)),
        (None, unread.clone(), hostless.clone()),
        "CONTROL: an older build's unread try is released as before"
    );
    assert_eq!(
        take_up("try-legacy", &books(None, &Conversation::Hostless)),
        (None, unread, hostless),
        "CONTROL: an older build's try is required at the start, then released"
    );
}

/// THE LIMIT'S TRACK KEEPS THE PROGRAM IT WAS MET UNDER where no wall is
/// remembered: Claude Code's Fable bucket, its notice naming no reset, met
/// unread under a host whose read names no conversation; an idle point of
/// Claude Code's then ends the remembered wall (no reset it named is waited
/// on) but not the limit's track, which stays unread there. Then Muse Code
/// runs in the tab: its point ends Claude Code's limit by the track's own
/// record ([`WallTrack::program`], [`met_under_another`]), never releasing it
/// to the hostless lane's. Recorded on the wall alone, the track named no
/// program at Muse Code's point, and Muse Code's decision released the
/// limit to the hostless lane's, as an older build's row is. The mirror:
/// agy's quota, met under agy, ends at Claude Code's first point. NEGATIVE
/// CONTROL: Claude Code's own idle point keeps its limit's track.
#[test]
fn a_limits_track_keeps_the_program_it_was_met_under_without_its_wall() {
    use aterm_phase::prompt::fixtures::{
        RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300, WALL_FABLE_NO_RESET_2_1_301, screen,
    };
    let claude_idle = screen(RESUMED_NO_LIMIT_AT_ARMED_LIMIT_2_1_300);
    let muse = screen(aterm_phase::muse::fixtures::END_OF_TURN_BG_RUNNING);
    let in_tab = |tag: &str, wall: &[String]| {
        let (dir, ledger) = ledger_at(tag);
        let decided = loop_points(
            &ledger,
            true,
            None,
            &[
                At {
                    seq: 101,
                    program: None,
                    rows: wall,
                    at: 0,
                    worked: Some(Duration::from_secs(60)),
                },
                At {
                    seq: 102,
                    program: None,
                    rows: &claude_idle,
                    at: 30,
                    worked: None,
                },
                At {
                    seq: 103,
                    program: None,
                    rows: &muse,
                    at: 60,
                    worked: None,
                },
            ],
        );
        let _ = std::fs::remove_dir_all(dir);
        decided
    };
    let fable = in_tab("track-fable", &screen(WALL_FABLE_NO_RESET_2_1_301));
    assert!(
        fable[1..].iter().all(|d| d.memory.is_none())
            && fable[..2]
                .iter()
                .all(|d| d.track == Some(Conversation::Unknown)),
        "CONTROL: Claude Code's bucket stays unread on its track alone at its own \
         point: {fable:#?}"
    );
    assert_eq!(
        fable[2].track, None,
        "Muse Code's point ends Claude Code's bucket by its track's record: {fable:#?}"
    );
    let agy = in_tab(
        "track-agy",
        &screen(aterm_phase::agy::fixtures::lab::ERROR_429_LONG),
    );
    assert!(
        agy[0].track == Some(Conversation::Hostless) && agy[1..].iter().all(|d| d.track.is_none()),
        "agy's quota ends at Claude Code's point: {agy:#?}"
    );
}
