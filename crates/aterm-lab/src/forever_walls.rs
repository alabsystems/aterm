// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE JUDGES OF `lab forever`'s WALLS PAST W2 (`--mode wall-shift`,
//! `wall-redraw`, `wall-switch`, `wall-unread`, and the walls-3 stage's
//! `wall-day-stop`, `wall-cruise`, `wall-relaunch`, `wall-bucket`): what the
//! walls stages left to unit tests alone, each judged from the tab's own
//! records ([`Seen`]: journal, ledger, `status`, the stub's dumps) and what
//! the lab did to the stub and the tab ([`Cover`]: the walls raised again,
//! the switch, the relaunch, the unread conversation), never the screen
//! alone. Each passes beside the
//! negative controls its tests fail (`forever_walls_tests.rs`).
//!
//! FOREVER holds through all of them: NEEDS YOU is loud and never blocks, so
//! a person told (the ladder's badge, the unread limit's `EXCEPTION`) is no
//! stall; nothing is ever typed into a wall that stands.

use super::*;
use aterm_agent::supervise::policy::approval::RULE_LIMIT_RETRY;

/// The minute a reset names: the loop counts the program's grace from that
/// minute's end (`approval_loop.rs`'s `LIMIT_RETRY_MINUTE_S`, the turn-end
/// policy's reset placed by its minute).
const RESET_MINUTE_MS: i64 = 60_000;

/// How far before an hour past the reset the unread limit's told may be
/// journaled and still be at it (the journal's clock and the lab's agree to
/// the second).
const TOLD_SLACK_MS: i64 = 2_000;

/// How long after the told came down a landing may be journaled: the told
/// ends at the point that reads the conversation, the landing's or the one
/// before it.
const CLEARED_BY_DONE_MS: i64 = 5_000;

/// The loop's look at a limit held unread (`turn_end.rs`'s
/// `LIMIT_CONVERSATION_LOOK`): the conversation read again is decided at
/// the next look, at the latest.
const UNREAD_LOOK_MS: i64 = 5 * 60_000;

/// The harness's goings-on after `t1`: every act the journal says was typed
/// (its rule) and every `Keep trying` the ledger approved
/// ([`RULE_LIMIT_RETRY`]), in time order.
fn goings_on(seen: &Seen, t1: i64) -> Vec<(i64, String)> {
    let mut v: Vec<(i64, String)> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t > t1)
        .map(|(t, l)| (*t, act_rule(l).unwrap_or("?").to_string()))
        .collect();
    v.extend(
        seen.ledger
            .iter()
            .filter(|r| r.ts > t1 && r.rule == RULE_LIMIT_RETRY && r.decision == "approved")
            .map(|r| (r.ts, r.rule.clone())),
    );
    v.sort_by_key(|(t, _)| *t);
    v
}

/// The first going-on ([`goings_on`]) inside `(from, until)`, said as
/// `<rule> at +t`.
fn going_in(on: &[(i64, String)], from: i64, until: i64, t0: i64) -> Option<String> {
    on.iter()
        .find(|(t, _)| *t > from && *t < until)
        .map(|(t, rule)| format!("{rule} at +{}", at(t0, *t)))
}

/// A going-on at `t` of `rule` that typed a text but left no ledger row of
/// it: said, for a FAIL.
fn unledgered(seen: &Seen, rule: &str, t: i64, t0: i64) -> Option<String> {
    (rule != RULE_LIMIT_RETRY && seen.typed_row(rule, t).is_none())
        .then(|| format!("{rule} journaled at +{} but NO ledger row typed", at(t0, t)))
}

/// The wall the land act met: `(raised ms, reset unix s, land act ms)`, or
/// the verdict where the records hold none (no wall raised, no land act, an
/// act outside the wall).
fn met(seen: &Seen, land: Option<&(i64, String)>, t0: i64) -> Result<(i64, i64, i64), Verdict> {
    let Some((raised, reset_s)) = seen.cover.wall else {
        return Err(Verdict::Unobserved(
            "the stub's wall was never raised (no first point read)".into(),
        ));
    };
    let Some(&(t1, _)) = land else {
        return Err(Verdict::Fail(format!(
            "no land@v1 typed within {}",
            at(t0, seen.end_ms)
        )));
    };
    if t1 < raised || t1 >= reset_s * 1000 {
        return Err(Verdict::Unobserved(format!(
            "land@v1 typed at +{}, outside the wall (raised at +{}, reset at +{})",
            at(t0, t1),
            at(t0, raised),
            at(t0, reset_s * 1000)
        )));
    }
    Ok((raised, reset_s, t1))
}

/// What FOREVER fails in any walled chain's records from `t1` to `until`:
/// an escalation of the wall that is no ladder's badge (a stall: the session
/// handed to a person who must answer before it goes on), a reply's need (a
/// `NEEDS` line: no reply named one), and new harness text the stub read in
/// the wall ([`Cover::window_stub`]).
fn forever_faults(seen: &Seen, t1: i64, until: i64, t0: i64) -> Vec<String> {
    let mut bad = Vec::new();
    if let Some(r) = seen
        .ledger
        .iter()
        .find(|r| r.ts > t1 && r.ts < until && r.decision == "escalated")
    {
        bad.push(format!(
            "ESCALATED the wall to a person at +{}: {}",
            at(t0, r.ts),
            crate::util::one_line(&r.reason, 140)
        ));
    }
    if let Some((t, l)) = seen.journal.iter().find(|(_, l)| l.starts_with("NEEDS ")) {
        bad.push(format!(
            "a need was raised at +{}, though no reply named one: {}",
            at(t0, *t),
            crate::util::one_line(l, 100)
        ));
    }
    if let Some(s) = seen.cover.window_stub.as_ref().filter(|s| s.new_turns > 0) {
        bad.push(format!(
            "the stub saw {} request(s) holding a new harness text in the wall",
            s.new_turns
        ));
    }
    bad
}

/// The landing at `done` and its quiet after ([`landing_quiet`]), no task
/// record invented, its verdict with `said` and `bad` before it.
fn landed(seen: &Seen, done: &(i64, String), mut bad: Vec<String>, said: &str, t0: i64) -> Verdict {
    let (more, quiet, window) = landing_quiet(seen, done, false, t0);
    bad.extend(more);
    if let Some(r) = cover::invented_record(seen) {
        bad.push(format!("a task record where none was given: {r}"));
    }
    if seen.cover.window_stub.is_none() {
        return Verdict::Unobserved(format!(
            "{said}; {quiet}: the stub's dumps of the wall were not read"
        ));
    }
    landed_verdict(bad, format!("{said}; {quiet}"), seen, done.0, window)
}

/// W3, G3'S CLIMB ACROSS A CHANGED RESET (`--mode wall-shift`): Gemini CLI's
/// usage-limit dialog of a Google account's spent quota (a reset named),
/// which the lab raised again at each reset instead of lifting it
/// ([`wall_move`], [`Cover::shifted`]), so each `Keep trying` past the reset
/// its dialog named met the wall again and the dialog was drawn with a new
/// reset. Land's act MEETS the wall; every press ([`RULE_LIMIT_RETRY`]
/// `approved`) comes AT OR AFTER THE RESET THE DIALOG IT ANSWERED NAMED (the
/// wall standing when the request before it was answered:
/// [`Cover::reset_at`]) — one before it is an act into the wall: FAIL — and
/// within the minute, `program`'s grace and [`WALL_LOST_MS`] of it (else
/// PERSISTENCE LOST); each try that met the wall again is answered by the
/// loop's wait on the NEW reset (a `WAITING … rule=limit-retry@v1` whose
/// `until=` is at or past it, before the next press); [`Chain::shifts`]
/// tries meet the wall and the next gets through; NOTHING TYPED (no journal
/// act) before the landing; no stall, no reply's need, no harness text in
/// the wall ([`forever_faults`]); the clean defense lands the task, quiet
/// after. Fewer tries in the wall than the chain's shifts (a wall lifted
/// early, a press that came after a later reset) did not drive the climb:
/// UNOBSERVED.
pub(super) fn judge_wall_shift(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let (raised, first, t1) = match met(seen, land, t0) {
        Ok(m) => m,
        Err(v) => return v,
    };
    let shifts = Chain::ShiftWall.shifts();
    let grace = wall_grace_ms(program);
    let mut presses: Vec<&Row> = seen
        .ledger
        .iter()
        .filter(|r| r.ts > t1 && r.rule == RULE_LIMIT_RETRY && r.decision == "approved")
        .collect();
    presses.sort_by_key(|r| r.ts);
    let done = seen.first_after("DONE ", t1);
    let mut said = vec![format!(
        "the dialog's wall raised at +{} (resets +{}, raised again {} time(s): {}); land@v1 typed at +{} met it",
        at(t0, raised),
        at(t0, first * 1000),
        seen.cover.shifted.len(),
        seen.cover
            .shifted
            .iter()
            .map(|(t, r)| format!("at +{} to +{}", at(t0, *t), at(t0, r * 1000)))
            .collect::<Vec<_>>()
            .join(", "),
        at(t0, t1)
    )];
    let mut bad = forever_faults(seen, t1, done.map_or(seen.end_ms, |(d, _)| *d), t0);
    let until_done = done.map_or(seen.end_ms + 1, |(d, _)| *d);
    if let Some((t, l)) = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t > t1 + AT_DONE_MS && *t < until_done)
    {
        bad.push(format!(
            "TYPED into the dialog: {} at +{}",
            act_rule(l).unwrap_or("?"),
            at(t0, *t)
        ));
    }
    let mut prev = t1;
    let mut in_wall = 0;
    let mut through = None;
    let mut pending = None;
    for (k, r) in presses.iter().enumerate() {
        if let Some(tg) = through {
            bad.push(format!(
                "a press at +{} after the try that got through at +{}",
                at(t0, r.ts),
                at(t0, tg)
            ));
            break;
        }
        let Some(named) = seen.cover.reset_at(prev) else {
            bad.push(format!(
                "a press at +{} with no wall standing when the request before it was answered",
                at(t0, r.ts)
            ));
            break;
        };
        let named_ms = named * 1000;
        if r.ts < named_ms {
            bad.push(format!(
                "PRESSED BEFORE THE RESET: try {} at +{}, {} before the reset its dialog named (+{})",
                k + 1,
                at(t0, r.ts),
                at(0, named_ms - r.ts),
                at(t0, named_ms)
            ));
            break;
        }
        let lost = named_ms + RESET_MINUTE_MS + grace + WALL_LOST_MS;
        if r.ts > lost {
            bad.push(format!(
                "PERSISTENCE LOST: try {} at +{}, {} past the reset its dialog named (+{}), past the {} deadline",
                k + 1,
                at(t0, r.ts),
                at(0, r.ts - named_ms),
                at(t0, named_ms),
                at(0, lost - named_ms)
            ));
            break;
        }
        match seen.cover.reset_at(r.ts) {
            Some(next) => {
                in_wall += 1;
                let next_ms = next * 1000;
                let until = presses.get(k + 1).map_or(seen.end_ms + 1, |n| n.ts);
                let rule = format!("rule={RULE_LIMIT_RETRY} ");
                let waited = seen.journal.iter().find(|(t, l)| {
                    *t > r.ts
                        && *t < until
                        && l.starts_with("WAITING ")
                        && l.contains(&rule)
                        && l.split_once(" until=")
                            .and_then(|(_, rest)| {
                                crate::workers::iso_ms(rest.split_whitespace().next()?)
                            })
                            .is_some_and(|u| u >= next_ms)
                });
                match waited {
                    Some((tw, _)) => said.push(format!(
                        "try {} at +{} ({} past the reset +{}) met the wall again, its dialog's new reset +{} waited on from +{}",
                        k + 1,
                        at(t0, r.ts),
                        at(0, r.ts - named_ms),
                        at(t0, named_ms),
                        at(t0, next_ms),
                        at(t0, *tw)
                    )),
                    None if presses.get(k + 1).is_some() => {
                        bad.push(format!(
                            "try {} at +{} met the wall again (its new reset +{}), but no wait on that reset came before the next press",
                            k + 1,
                            at(t0, r.ts),
                            at(t0, next_ms)
                        ));
                        break;
                    }
                    None => pending = Some(next_ms),
                }
            }
            None => {
                through = Some(r.ts);
                said.push(format!(
                    "try {} at +{} ({} past the reset +{}) got through",
                    k + 1,
                    at(t0, r.ts),
                    at(0, r.ts - named_ms),
                    at(t0, named_ms)
                ));
            }
        }
        prev = r.ts;
    }
    let said = said.join("; ");
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    let Some(tg) = through else {
        // the next try is due past the reset standing at the last request
        let named = pending
            .or_else(|| seen.cover.reset_at(prev).map(|r| r * 1000))
            .unwrap_or(first * 1000);
        let lost = named + RESET_MINUTE_MS + grace + WALL_LOST_MS;
        if seen.end_ms >= lost {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: no try got through by +{}, {} past the reset +{}",
                at(t0, seen.end_ms),
                at(0, seen.end_ms - named),
                at(t0, named)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before a try got through",
            at(t0, seen.end_ms)
        ));
    };
    if in_wall < shifts {
        return Verdict::Unobserved(format!(
            "{said}; only {in_wall} of {shifts} tries met the wall again: the climb across a changed reset was not driven"
        ));
    }
    let Some(done) = done else {
        if seen.end_ms >= tg + WALL_LOST_MS {
            return Verdict::Fail(format!(
                "{said}; but NO landing in the {} after the try that got through",
                at(0, seen.end_ms - tg)
            ));
        }
        return Verdict::Unobserved(format!("{said}; not landed by +{}", at(t0, seen.end_ms)));
    };
    if done.0 < tg {
        return Verdict::Fail(format!(
            "{said}; LANDED at +{} while the dialog stood",
            at(t0, done.0)
        ));
    }
    if done.0 > tg + WALL_LOST_MS {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: the landing came {} after the try that got through",
            at(0, done.0 - tg)
        ));
    }
    landed(seen, done, Vec::new(), &said, t0)
}

/// W4, THE REDRAW AFTER A TYPED RESUME (`--mode wall-redraw`): the wall the
/// land act met, raised again at its reset ([`Cover::shifted`]'s one, the
/// new reset [`REDRAW_AHEAD_MS`] on), so the resume past the first reset
/// meets it and the program draws its wall again under the new reset.
/// NOTHING TYPED into the first wall; the first going-on past its reset (the
/// `usage-resume@v1` typed, or Gemini CLI's press) comes within the minute,
/// `program`'s grace and [`WALL_LOST_MS`] (else PERSISTENCE LOST); the wall
/// READ AGAIN after it (a limit point before the new reset: [`cover::wall_point`]);
/// NOTHING TYPED into the redrawn wall before the new reset; GONE ON past it
/// (an act or press, ledgered — or the program's own continuation,
/// [`Cover::went_on_ms`], with nothing typed on top of it) within
/// [`WALL_LOST_MS`] of its due ([`redraw_due`]: the new reset's minute and
/// `program`'s grace, or the loop's floor after a typed resume that met the
/// wall again, the later); no landing before
/// the lift; no stall, no reply's need, no harness text in the first wall
/// ([`forever_faults`]); landed, quiet after. A wall never raised again, a
/// resume past the new reset or one that never met the wall again did not
/// drive the redraw: UNOBSERVED.
pub(super) fn judge_wall_redraw(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let (raised, first, t1) = match met(seen, land, t0) {
        Ok(m) => m,
        Err(v) => return v,
    };
    let r_ms = first * 1000;
    let mut said = format!(
        "the wall raised at +{} (resets +{}); land@v1 typed at +{} met it",
        at(t0, raised),
        at(t0, r_ms),
        at(t0, t1)
    );
    let Some(&(shift_ms, second)) = seen.cover.shifted.first() else {
        return Verdict::Unobserved(format!(
            "{said}: the wall was never raised again at its reset, no redraw to meet"
        ));
    };
    let r2_ms = second * 1000;
    said.push_str(&format!(
        "; raised again at +{} (resets +{})",
        at(t0, shift_ms),
        at(t0, r2_ms)
    ));
    let grace = wall_grace_ms(program);
    let on = goings_on(seen, t1);
    let lift = seen
        .cover
        .lifted_ms
        .unwrap_or(r2_ms - WALL_LIFT_EARLY_MS)
        .min(r2_ms);
    let mut bad = forever_faults(seen, t1, lift, t0);
    if let Some(g) = going_in(&on, t1 + AT_DONE_MS, r_ms, t0) {
        bad.push(format!("TYPED into the wall: {g}"));
    }
    if let Some((d, _)) = seen.first_after("DONE ", t1).filter(|(d, _)| *d < lift) {
        bad.push(format!("LANDED at +{} while the wall stood", at(t0, *d)));
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    let lost1 = r_ms + RESET_MINUTE_MS + grace + WALL_LOST_MS;
    let Some((r1, rule1)) = on.iter().find(|(t, _)| *t >= r_ms) else {
        if seen.end_ms >= lost1 {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: nothing typed past the first reset by +{}",
                at(t0, seen.end_ms)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before anything went on past the first reset",
            at(t0, seen.end_ms)
        ));
    };
    if *r1 > lost1 {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: {rule1} at +{}, {} past the first reset",
            at(t0, *r1),
            at(0, r1 - r_ms)
        ));
    }
    if let Some(why) = unledgered(seen, rule1, *r1, t0) {
        return Verdict::Fail(format!("{said}; {why}"));
    }
    said.push_str(&format!(
        "; {rule1} at +{} ({} past the first reset)",
        at(t0, *r1),
        at(0, r1 - r_ms)
    ));
    if *r1 >= r2_ms {
        return Verdict::Unobserved(format!(
            "{said}: it came past the new reset, so the redraw was not met"
        ));
    }
    let Some((tr, lr)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > *r1 && *t < r2_ms && cover::wall_point(l))
    else {
        return Verdict::Unobserved(format!(
            "{said}: no limit point read after it before the new reset, so the redraw was not seen"
        ));
    };
    said.push_str(&format!(
        "; the wall drawn again +{} {}",
        at(t0, *tr),
        crate::util::one_line(lr, 100)
    ));
    if let Some(g) = going_in(&on, r1 + AT_DONE_MS, r2_ms, t0) {
        return Verdict::Fail(format!("{said}; TYPED into the redrawn wall: {g}"));
    }
    let (due2, floor) = redraw_due(r2_ms + RESET_MINUTE_MS + grace, rule1, *tr);
    let lost2 = due2 + WALL_LOST_MS;
    let after = on.iter().find(|(t, _)| *t >= r2_ms);
    let went_on = seen.cover.went_on_ms.filter(|t| *t >= lift);
    let gone = match (after, went_on) {
        (Some((ta, rule)), Some(tv)) if tv < *ta => {
            return Verdict::Fail(format!(
                "{said}; TYPED ON TOP of the program's own continuation: it went on at +{}, and {rule} came at +{}",
                at(t0, tv),
                at(t0, *ta)
            ));
        }
        (Some((ta, rule)), _) => {
            if *ta > lost2 {
                return Verdict::Fail(format!(
                    "{said}; PERSISTENCE LOST: {rule} at +{}, {} past the new reset",
                    at(t0, *ta),
                    at(0, ta - r2_ms)
                ));
            }
            if let Some(why) = unledgered(seen, rule, *ta, t0) {
                return Verdict::Fail(format!("{said}; {why}"));
            }
            format!(
                "gone on: {rule} at +{} ({} past the new reset{})",
                at(t0, *ta),
                at(0, ta - r2_ms),
                if floor {
                    format!(
                        "; the loop's floor after a resume that met the wall again: +{}",
                        at(t0, due2)
                    )
                } else {
                    String::new()
                }
            )
        }
        (None, Some(tv)) => format!(
            "gone on: the program's own continuation at +{} ({} past the new reset)",
            at(t0, tv),
            at(r2_ms, tv)
        ),
        (None, None) if seen.end_ms >= lost2 => {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: nothing went on past the new reset by +{}",
                at(t0, seen.end_ms)
            ));
        }
        (None, None) => {
            return Verdict::Unobserved(format!(
                "{said}; watched to +{}, before anything went on past the new reset",
                at(t0, seen.end_ms)
            ));
        }
    };
    let said = format!("{said}; nothing typed into it; {gone}");
    let Some(done) = seen.first_after("DONE ", lift) else {
        if seen.end_ms >= lost2 {
            return Verdict::Fail(format!(
                "{said}; but NO landing by +{}",
                at(t0, seen.end_ms)
            ));
        }
        return Verdict::Unobserved(format!("{said}; not landed by +{}", at(t0, seen.end_ms)));
    };
    if done.0 > lost2 {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: the landing came {} after the new reset",
            at(0, done.0 - r2_ms)
        ));
    }
    landed(seen, done, Vec::new(), &said, t0)
}

/// When W4's going-on past the new reset is due: the new reset's minute and
/// the program's grace (`at_reset`), or, where the first going-on past the
/// first reset (`rule1`) was a typed `usage-resume@v1` that met the wall
/// again (the redraw read at `redrawn`), the loop's floor after such a
/// continuation ([`limit_retry_ms`] from the take window's end,
/// `resume_at_reset`'s `after_retry`) where that is later — and whether the
/// floor is the due. A press of Gemini CLI's dialog, or the program's own
/// continuation, is no such continuation. Measured (`y6rd-agy`): the resume
/// at +553.8 s met the wall again at +554.3 s, the take window ended at
/// +583.6 s, and the loop waited `until=` +1183.6 s (`spend resets`), 418 s
/// past the new reset +765.3 s; its resume at +1185.0 s landed.
fn redraw_due(at_reset: i64, rule1: &str, redrawn: i64) -> (i64, bool) {
    if rule1 != turn_end::RULE_LIMIT_RESUME {
        return (at_reset, false);
    }
    let floor = redrawn + take_window_ms() + limit_retry_ms();
    if floor > at_reset {
        (floor, true)
    } else {
        (at_reset, false)
    }
}

/// How long after the second agent's prompt W5 waits for its land act when
/// the loop named no back-off at its point: its work point's first back-off
/// ([`next_step_ms`], one short turn), [`PERSISTENCE_GRACE_MS`], and a
/// minute for its reply.
fn switch_land_within_ms() -> i64 {
    next_step_ms(1, false) + PERSISTENCE_GRACE_MS + 60_000
}

/// When W5's second agent's land act is due: the back-off the loop named at
/// its first point after its prompt ([`named_backoff`]) and
/// [`PERSISTENCE_GRACE_MS`], or, with none named, [`switch_land_within_ms`]
/// after its prompt. The tab's short streak runs on across the switch, so the
/// back-off is the streak's (measured, `y4sw`: Muse Code's work point named
/// `2 short turn(s) in a row: 4 min back-off`, Land's act at its due).
fn switch_land_due(seen: &Seen, prompt_ms: i64) -> (i64, String) {
    match named_backoff(seen, prompt_ms) {
        Some((due, line)) => (due + PERSISTENCE_GRACE_MS, crate::util::one_line(line, 120)),
        None => (
            prompt_ms + switch_land_within_ms(),
            "no back-off named; one short turn's".to_string(),
        ),
    }
}

/// Land's act on W5's second agent: the first act of Land's rule after its
/// prompt, `land@v1` or, where the tab's episode had it typed already (the
/// first agent's land act met the wall), its nudge `land-nudge@v1`.
fn land_act_after(seen: &Seen, t: i64) -> Option<(i64, &str)> {
    seen.all_acts()
        .into_iter()
        .filter(|(at, _)| *at > t)
        .find_map(|(at, l)| {
            act_rule(l)
                .filter(|r| *r == turn_end::RULE_LAND || *r == turn_end::RULE_LAND_NUDGE)
                .map(|r| (*at, r))
        })
}

/// A wait of the loop that holds a session on a limit: the remembered
/// limit's unread hold ([`UNREAD_WORDS`]) or a wait on a limit's reset (a
/// back-off ending ` resets`).
fn holds_on_a_limit(line: &str) -> bool {
    line.starts_with("WAITING ")
        && (line.contains(UNREAD_WORDS)
            || line
                .split_once(" until=")
                .is_some_and(|(_, rest)| rest.trim_end().ends_with(" resets")))
}

/// W5, THE CROSS-PROGRAM END (`--mode wall-switch`, the remembered-limit
/// stage's L1): `program`'s agent met the wall the land act met, the wall
/// READ as one before the switch and NOTHING TYPED into it; then the lab
/// ended that agent, lifted the stub's wall and started another
/// ([`Cover::switch`], [`switch_to`]) in the same tab. The first agent's
/// limit HOLDS NOTHING OF THE SECOND'S: no wait on a limit at its points
/// ([`holds_on_a_limit`]), its land act ([`land_act_after`]) typed after its
/// prompt by its due ([`switch_land_due`]) and before the first agent's reset, nothing
/// typed between the switch and that act, its landing before that reset,
/// and NOTHING TYPED INTO IT PAST THAT RESET (the ended limit's resume: the
/// watch must reach [`switch_watch_until`]); no stall, no reply's need, no
/// harness text in the first agent's wall ([`forever_faults`]); quiet after
/// the landing. No switch made, or a wall never read before it: UNOBSERVED.
pub(super) fn judge_wall_switch(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let (raised, reset_s, t1) = match met(seen, land, t0) {
        Ok(m) => m,
        Err(v) => return v,
    };
    let r_ms = reset_s * 1000;
    let mut said = format!(
        "{program}'s wall raised at +{} (resets +{}); land@v1 typed at +{} met it",
        at(t0, raised),
        at(t0, r_ms),
        at(t0, t1)
    );
    let Some(sw) = seen.cover.switch.as_ref() else {
        let why = seen
            .cover
            .switch_gave_up
            .as_deref()
            .unwrap_or("the loop never waited on the wall");
        return Verdict::Unobserved(format!("{said}: no switch made ({why})"));
    };
    said.push_str(&format!(
        "; {program} ended at +{} (the shell at +{}), the wall lifted at +{}, {} launched at +{} and prompted at +{}",
        at(t0, sw.exited_ms),
        at(t0, sw.shell_ms),
        sw.lifted_ms.map_or_else(|| "?".to_string(), |l| at(t0, l)),
        sw.to,
        at(t0, sw.launched_ms),
        at(t0, sw.prompt_ms)
    ));
    let on = goings_on(seen, t1);
    let mut bad = forever_faults(seen, t1, sw.exited_ms, t0);
    if let Some(g) = going_in(&on, t1 + AT_DONE_MS, sw.exited_ms, t0) {
        bad.push(format!("TYPED into {program}'s wall: {g}"));
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if !seen
        .journal
        .iter()
        .any(|(t, l)| *t > t1 && *t < sw.exited_ms && cover::wall_point(l))
    {
        return Verdict::Unobserved(format!(
            "{said}: the wall never read as one before the switch"
        ));
    }
    let b_act = land_act_after(seen, sw.prompt_ms);
    let b_land = b_act.map(|(t, _)| t);
    let held_until = b_land.unwrap_or(seen.end_ms + 1);
    if let Some((tw, wl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > sw.prompt_ms && *t < held_until && holds_on_a_limit(l))
    {
        return Verdict::Fail(format!(
            "{said}; {program}'s LIMIT HELD {}: +{} {}",
            sw.to,
            at(t0, *tw),
            crate::util::one_line(wl, 120)
        ));
    }
    if let Some(g) = going_in(&on, sw.exited_ms, held_until, t0) {
        return Verdict::Fail(format!("{said}; TYPED before {}'s land act: {g}", sw.to));
    }
    let (due, backoff) = switch_land_due(seen, sw.prompt_ms);
    let Some((b1, b_rule)) = b_act else {
        if seen.end_ms >= due {
            return Verdict::Fail(format!(
                "{said}; {program}'s LIMIT HELD {}: no land act in the {} after its prompt (due by +{}: {backoff})",
                sw.to,
                at(0, seen.end_ms - sw.prompt_ms),
                at(t0, due)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before {}'s land act was due (+{}: {backoff})",
            at(t0, seen.end_ms),
            sw.to,
            at(t0, due)
        ));
    };
    if b1 >= r_ms {
        return Verdict::Fail(format!(
            "{said}; {program}'s LIMIT HELD {}: its land act at +{} came past {program}'s reset",
            sw.to,
            at(t0, b1)
        ));
    }
    if b1 > due {
        return Verdict::Fail(format!(
            "{said}; {program}'s LIMIT HELD {}: its land act ({b_rule}) at +{} came past its due +{} ({backoff})",
            sw.to,
            at(t0, b1),
            at(t0, due)
        ));
    }
    said.push_str(&format!(
        "; {}'s land act at +{} ({} after its prompt, {} before {program}'s reset): {b_rule}, due by +{}",
        sw.to,
        at(t0, b1),
        at(0, b1 - sw.prompt_ms),
        at(0, r_ms - b1),
        at(t0, due)
    ));
    let Some(done) = seen.first_after("DONE ", b1) else {
        if seen.end_ms >= r_ms {
            return Verdict::Fail(format!("{said}; but NO landing before {program}'s reset"));
        }
        return Verdict::Unobserved(format!("{said}; not landed by +{}", at(t0, seen.end_ms)));
    };
    if done.0 >= r_ms {
        return Verdict::Fail(format!(
            "{said}; landed only at +{}, past {program}'s reset",
            at(t0, done.0)
        ));
    }
    let watch = switch_watch_until(reset_s);
    match landed(seen, done, Vec::new(), &said, t0) {
        Verdict::Pass(w) if seen.end_ms < watch => Verdict::Unobserved(format!(
            "{w}; watched to +{}, short of {program}'s reset and its grace (+{})",
            at(t0, seen.end_ms),
            at(t0, watch)
        )),
        Verdict::Pass(w) => Verdict::Pass(format!(
            "{w}; nothing typed past {program}'s reset (watched to +{})",
            at(t0, seen.end_ms)
        )),
        v => v,
    }
}

/// W6, THE UNREAD LIMIT TOLD (`--mode wall-unread`, the remembered-limit
/// stage's L2): Claude Code's limit met by the land act, its notice out of
/// sight ([`Cover::hidden`]) and its conversation made unread
/// ([`Cover::unread`]: the transcript aside). The loop HOLDS the limit unread
/// (a `WAITING` naming [`UNREAD_WORDS`] after the move) and TYPES NOTHING
/// until the conversation is read again; an hour past the reset
/// ([`futile_after_ms`]) the owner is TOLD (an `EXCEPTION` line carrying
/// [`unread_told_words`]) — not before it, and within
/// [`UNREAD_TOLD_LATE_MS`] of it — the hold standing; once the transcript is
/// back the resume is typed (ledgered) within the loop's look
/// ([`UNREAD_LOOK_MS`]) and [`WALL_LOST_MS`], the told comes down (a
/// `CLEARED … exception=futile` line by the landing) and the task lands,
/// quiet after; no stall, no reply's need, no harness text while held
/// ([`forever_faults`]). A notice that never left, a transcript never moved,
/// or no unread hold read: the lab did not make it unread, UNOBSERVED.
pub(super) fn judge_wall_unread(
    seen: &Seen,
    land: Option<&(i64, String)>,
    _program: &str,
    t0: i64,
) -> Verdict {
    let (raised, reset_s, t1) = match met(seen, land, t0) {
        Ok(m) => m,
        Err(v) => return v,
    };
    let r_ms = reset_s * 1000;
    let mut said = format!(
        "the wall raised at +{} (resets +{}); land@v1 typed at +{} met it",
        at(t0, raised),
        at(t0, r_ms),
        at(t0, t1)
    );
    let Some(h) = seen.cover.hidden.as_ref() else {
        let why = seen
            .cover
            .hide_gave_up
            .as_deref()
            .unwrap_or("the notice never left the screen");
        return Verdict::Unobserved(format!("{said}: {why}"));
    };
    let Some(u) = seen.cover.unread.as_ref() else {
        let why = seen
            .cover
            .unread_gave_up
            .as_deref()
            .unwrap_or("the transcript was never moved aside");
        return Verdict::Unobserved(format!("{said}: {why}"));
    };
    said.push_str(&format!(
        "; the notice out of sight at +{}, the transcript aside at +{}{}",
        at(t0, h.seen_ms),
        at(t0, u.moved_ms),
        u.restored_ms
            .map_or_else(String::new, |r| format!(", back at +{}", at(t0, r)))
    ));
    let held_to = u.restored_ms.unwrap_or(seen.end_ms + 1);
    let on = goings_on(seen, t1);
    let mut bad = forever_faults(seen, t1, held_to, t0);
    if let Some(g) = going_in(&on, t1 + AT_DONE_MS, held_to, t0) {
        bad.push(format!(
            "TYPED while the limit stood or was held unread: {g}"
        ));
    }
    if let Some((d, _)) = seen.first_after("DONE ", t1).filter(|(d, _)| *d < held_to) {
        bad.push(format!(
            "LANDED at +{} while the limit was held unread",
            at(t0, *d)
        ));
    }
    let due = r_ms + futile_after_ms();
    let told = told_unread(seen, t1);
    match told {
        Some(t) if t < due - TOLD_SLACK_MS => bad.push(format!(
            "TOLD EARLY at +{}, {} before an hour past the reset (+{})",
            at(t0, t),
            at(0, due - t),
            at(t0, due)
        )),
        Some(t) if t > due + UNREAD_TOLD_LATE_MS => bad.push(format!(
            "TOLD LATE at +{}, {} past an hour past the reset (+{})",
            at(t0, t),
            at(0, t - due),
            at(t0, due)
        )),
        None if u.restored_ms.is_some() || seen.end_ms >= due + UNREAD_TOLD_LATE_MS => {
            bad.push(format!(
                "NOT TOLD: no EXCEPTION carrying \"{}\" by +{}",
                unread_told_words(),
                at(t0, u.restored_ms.unwrap_or(seen.end_ms))
            ));
        }
        _ => {}
    }
    if let Some(t) = told
        && let Some((tc, _)) = seen.journal.iter().find(|(tc, l)| {
            *tc > t && *tc < held_to && l.starts_with("CLEARED ") && l.contains("exception=futile")
        })
    {
        bad.push(format!(
            "the told came down at +{} while the limit was still held unread",
            at(t0, *tc)
        ));
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    let Some((th, wl)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > u.moved_ms && *t < held_to && l.contains(UNREAD_WORDS))
    else {
        return Verdict::Unobserved(format!(
            "{said}: the loop never held the limit unread (no wait naming \"{UNREAD_WORDS}\"): the conversation was not made unread"
        ));
    };
    said.push_str(&format!(
        "; held unread from +{} ({})",
        at(t0, *th),
        crate::util::one_line(wl, 110)
    ));
    let Some(t) = told else {
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before an hour past the reset (+{})",
            at(t0, seen.end_ms),
            at(t0, due)
        ));
    };
    said.push_str(&format!(
        "; told at +{} ({} past an hour past the reset), nothing typed",
        at(t0, t),
        at(due, t)
    ));
    let Some(back) = u.restored_ms else {
        return Verdict::Unobserved(format!(
            "{said}; the conversation never read again by +{}",
            at(t0, seen.end_ms)
        ));
    };
    let lost = back + UNREAD_LOOK_MS + WALL_LOST_MS;
    let Some((ta, rule)) = on.iter().find(|(t, _)| *t >= back) else {
        if seen.end_ms >= lost {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: nothing typed in the {} after the conversation was read again",
                at(0, seen.end_ms - back)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before the resume",
            at(t0, seen.end_ms)
        ));
    };
    if *ta > lost {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: {rule} at +{}, {} after the conversation was read again",
            at(t0, *ta),
            at(0, ta - back)
        ));
    }
    if let Some(why) = unledgered(seen, rule, *ta, t0) {
        return Verdict::Fail(format!("{said}; {why}"));
    }
    let said = format!(
        "{said}; gone on: {rule} at +{} ({} after it was read again)",
        at(t0, *ta),
        at(0, ta - back)
    );
    let Some(done) = seen.first_after("DONE ", *ta) else {
        if seen.end_ms >= lost {
            return Verdict::Fail(format!(
                "{said}; but NO landing by +{}",
                at(t0, seen.end_ms)
            ));
        }
        return Verdict::Unobserved(format!("{said}; not landed by +{}", at(t0, seen.end_ms)));
    };
    let cleared = seen
        .journal
        .iter()
        .find(|(tc, l)| {
            *tc >= back
                && *tc <= done.0 + CLEARED_BY_DONE_MS
                && l.starts_with("CLEARED ")
                && l.contains("exception=futile")
        })
        .map(|(tc, _)| *tc);
    let Some(tc) = cleared else {
        return Verdict::Fail(format!(
            "{said}; the told STOOD past the landing at +{}: no CLEARED exception=futile",
            at(t0, done.0)
        ));
    };
    let said = format!("{said}; the told down at +{}", at(t0, tc));
    landed(seen, done, Vec::new(), &said, t0)
}

/// WHAT WENT ON PAST THE RESET `r_ms` (`on`, [`goings_on`]; the stub's wall
/// lifted at `lift`): the first act or press at or past it, ledgered — or
/// the program's own continuation ([`Cover::went_on_ms`]) with no resume
/// typed on top of it (W8's `cruise`: Cruise's own act after that
/// continuation's answer is no resume, and goes) — within the minute,
/// `program`'s grace and [`WALL_LOST_MS`]: when, and its words; or the
/// verdict where nothing did, `said` before it.
fn gone_past(
    seen: &Seen,
    on: &[(i64, String)],
    (r_ms, lift): (i64, i64),
    (program, cruise): (&str, bool),
    said: &str,
    t0: i64,
) -> Result<(i64, String), Verdict> {
    let lost = r_ms + RESET_MINUTE_MS + wall_grace_ms(program) + WALL_LOST_MS;
    let after = on.iter().find(|(t, _)| *t >= r_ms);
    let went_on = seen.cover.went_on_ms.filter(|t| *t >= lift);
    let own = |tv: i64| {
        (
            tv,
            format!(
                "gone on: the program's own continuation at +{} ({} past the reset)",
                at(t0, tv),
                at(r_ms, tv)
            ),
        )
    };
    match (after, went_on) {
        (Some((ta, rule)), Some(tv))
            if tv < *ta && (!cruise || rule == turn_end::RULE_LIMIT_RESUME) =>
        {
            Err(Verdict::Fail(format!(
                "{said}; TYPED ON TOP of the program's own continuation: it went on at +{}, and {rule} came at +{}",
                at(t0, tv),
                at(t0, *ta)
            )))
        }
        (Some((ta, _)), Some(tv)) if tv < *ta => Ok(own(tv)),
        (Some((ta, rule)), _) => {
            if *ta > lost {
                return Err(Verdict::Fail(format!(
                    "{said}; PERSISTENCE LOST: {rule} at +{}, {} past the reset",
                    at(t0, *ta),
                    at(0, ta - r_ms)
                )));
            }
            if let Some(why) = unledgered(seen, rule, *ta, t0) {
                return Err(Verdict::Fail(format!("{said}; {why}")));
            }
            Ok((
                *ta,
                format!(
                    "gone on: {rule} at +{} ({} past the reset)",
                    at(t0, *ta),
                    at(0, ta - r_ms)
                ),
            ))
        }
        (None, Some(tv)) => Ok(own(tv)),
        (None, None) if seen.end_ms >= lost => Err(Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: nothing went on past the reset by +{}",
            at(t0, seen.end_ms)
        ))),
        (None, None) => Err(Verdict::Unobserved(format!(
            "{said}; watched to +{}, before anything went on past the reset",
            at(t0, seen.end_ms)
        ))),
    }
}

/// W7, THE PER-DAY DIALOG ANSWERED STOP (`--mode wall-day-stop`, X1 of the
/// walls-3 stage): Gemini CLI's usage-limit dialog of a key's spent per-day
/// quota (no reset named) met by the land act, waited on the ladder with
/// its badge up, then answered `2. Stop` as a person at the keyboard would
/// ([`Cover::hidden`], [`Hidden::stopped`]). FOREVER: NEEDS YOU is loud and
/// never blocks, so the Stop hands nothing to a person for good. W2's judge
/// holds ([`cover::judge_wall_hidden`]: nothing typed into the wall, the
/// error read without the notice, the resume typed past the lift at a
/// screen without it, landed, quiet after); and the loop KEPT TRYING: the
/// first act after the Stop is the resume ([`turn_end::RULE_LIMIT_RESUME`]),
/// typed at or past the ladder's first due (the `until=` of its first wait,
/// [`cover::ladder_waits`]) — never before; NEEDS YOU STOOD from the Stop to
/// that resume (every status read in the span `needs=ask`, at least one,
/// and no `CLEARED` in it); and the resume that got through TOOK IT DOWN by
/// the landing ([`cover::ladder_cleared`]). The dialog answered by anything
/// but its Stop: UNOBSERVED.
pub(super) fn judge_wall_day_stopped(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let w = match cover::judge_wall_hidden(seen, land, program, t0) {
        Verdict::Pass(w) => w,
        v => return v,
    };
    let (Some(h), Some(&(t1, _))) = (seen.cover.hidden.as_ref(), land) else {
        return Verdict::Unobserved(format!("{w}: no hide or no land act on the record"));
    };
    if !h.stopped {
        return Verdict::Unobserved(format!(
            "{w}: the notice was taken by {}, not the dialog's Stop",
            h.command
        ));
    }
    let Some(&(read, due, _)) = cover::ladder_waits(seen, t1).first() else {
        return Verdict::Fail(format!(
            "{w}: NO LADDER: no wait on the per-day dialog's back-off ladder before its Stop"
        ));
    };
    let Some((tr, lr)) = seen
        .all_acts()
        .into_iter()
        .find(|(t, _)| *t > h.enter_ms)
        .map(|(t, l)| (*t, l.as_str()))
    else {
        return Verdict::Unobserved(format!("{w}: nothing typed after the Stop"));
    };
    let rule = act_rule(lr).unwrap_or("?");
    let mut said = format!(
        "{w}; the ladder's first wait at +{} due +{}; the Stop at +{}",
        at(t0, read),
        at(t0, due),
        at(t0, h.enter_ms)
    );
    if rule != turn_end::RULE_LIMIT_RESUME {
        return Verdict::Fail(format!(
            "{said}; the first act after the Stop was {rule} at +{}, not the resume",
            at(t0, tr)
        ));
    }
    if tr + cover::LADDER_DUE_SLACK_MS < due {
        return Verdict::Fail(format!(
            "{said}; RESUMED BEFORE ITS RUNG: {rule} at +{}, {} before the ladder's first due",
            at(t0, tr),
            at(tr, due)
        ));
    }
    said.push_str(&format!(
        "; kept trying: {rule} at +{} ({} after the ladder's first due)",
        at(t0, tr),
        at(due, tr)
    ));
    if let Some((t, l)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > h.enter_ms && *t < tr && l.starts_with("CLEARED "))
    {
        return Verdict::Fail(format!(
            "{said}; NEEDS YOU came down at +{} while the limit stood: {}",
            at(t0, *t),
            crate::util::one_line(l, 80)
        ));
    }
    let reads = super::surfaces::observed(&seen.status, &seen.status_reads, h.enter_ms, tr);
    if reads.is_empty() {
        return Verdict::Unobserved(format!(
            "{said}; no status read between the Stop and the resume"
        ));
    }
    if let Some((t, st)) = reads
        .iter()
        .find(|(_, st)| word(st, "needs").as_deref() != Some("ask"))
    {
        return Verdict::Fail(format!(
            "{said}; NEEDS YOU not standing at the status read at +{} while the limit stood: {}",
            at(t0, *t),
            crate::util::one_line(st, 80)
        ));
    }
    said.push_str(&format!(
        "; needs=ask at {} status read(s) from the Stop to it",
        reads.len()
    ));
    let Some((d, _)) = seen.first_after("DONE ", tr) else {
        return Verdict::Fail(format!("{said}; but NO landing after it"));
    };
    match cover::ladder_cleared(seen, tr, *d) {
        Some(down) => Verdict::Pass(format!("{said}; NEEDS YOU down at +{}", at(t0, down))),
        None => Verdict::Fail(format!(
            "{said}; NEEDS YOU STOOD after the resume that got through at +{}",
            at(t0, tr)
        )),
    }
}

/// How long after what went on past W8's wall Cruise's own act may come
/// where the loop named no back-off for it ([`named_backoff`]): a back-off
/// of 4 min, [`PERSISTENCE_GRACE_MS`], and a minute for the reply.
const CRUISE_AFTER_WALL_MS: i64 = 4 * 60_000 + PERSISTENCE_GRACE_MS + 60_000;

/// The back-off the loop named for the next act after `tg` (W8: what went on
/// past the wall; W5: the second agent's prompt): the first `WAITING … N short turn(s) in a row: M min
/// back-off` written after it, its due (`until=`, unix ms) and its line. The
/// wall's points lengthen the short streak as any short turn does — the
/// done check's yield the wall cut, then the answer that went on past it —
/// so the back-off is the streak's, not a fixed one (measured,
/// `z4cr-claude`: `3 short turn(s) in a row: 8 min back-off`, Cruise's act at
/// its due).
fn named_backoff(seen: &Seen, tg: i64) -> Option<(i64, &str)> {
    seen.journal
        .iter()
        .filter(|(t, l)| {
            *t > tg && l.starts_with("WAITING ") && l.contains(" short turn(s) in a row: ")
        })
        .find_map(|(_, l)| {
            let (_, rest) = l.split_once(" until=")?;
            let due = crate::workers::iso_ms(rest.split_whitespace().next()?)?;
            Some((due, l.as_str()))
        })
}

/// The act that met W8's wall: Cruise's first act after the stub's wall was
/// raised ([`Cover::wall`]).
#[must_use]
pub(super) fn cruise_met(seen: &Seen) -> Option<i64> {
    let (raised, _) = seen.cover.wall?;
    seen.all_acts()
        .into_iter()
        .find(|(t, _)| *t > raised)
        .map(|(t, _)| *t)
}

/// CRUISE GOING ON after W8's wall: the first act at or past the reset that
/// is Cruise's own (no `usage-resume@v1`: its done check, its
/// continuation), the watch's end ([`finished`]) and the judge's.
#[must_use]
pub(super) fn cruise_went_on(seen: &Seen) -> Option<i64> {
    let (_, reset_s) = seen.cover.wall?;
    seen.all_acts()
        .into_iter()
        .find(|(t, l)| *t >= reset_s * 1000 && act_rule(l) != Some(turn_end::RULE_LIMIT_RESUME))
        .map(|(t, _)| *t)
}

/// W8, A USAGE WALL UNDER CRUISE (`--mode wall-cruise`): the stub's wall
/// raised at the first point, so Cruise's act on the done claim (its done
/// check) MEETS it; the wall READ as one; NOTHING TYPED into it to its
/// reset; no stall, no reply's need, no harness text in it
/// ([`forever_faults`]); and Cruise never ends: no DONE. Past the reset the
/// harness GOES ON ([`gone_past`]: the resume, Gemini CLI's `Keep trying`,
/// or Claude Code's own continuation) — Cruise's own act typed past the reset
/// before any of them is no resume: FAIL — and then CRUISE GOES ON: its own
/// act ([`cruise_went_on`]), ledgered, by the due of the back-off the loop
/// named after that ([`named_backoff`]) and [`PERSISTENCE_GRACE_MS`] (none
/// named: within [`CRUISE_AFTER_WALL_MS`]), else persistence lost. A first
/// act past the reset met no wall, a wall never read: UNOBSERVED.
pub(super) fn judge_wall_cruise(seen: &Seen, program: &str, t0: i64) -> Verdict {
    let Some((raised, reset_s)) = seen.cover.wall else {
        return Verdict::Unobserved(
            "the stub's wall was never raised (no first point read)".into(),
        );
    };
    let r_ms = reset_s * 1000;
    let Some(t1) = cruise_met(seen) else {
        return Verdict::Fail(format!(
            "no act of Cruise's typed after the wall's raise within {}",
            at(t0, seen.end_ms)
        ));
    };
    let on = goings_on(seen, raised);
    let rule1 = on
        .iter()
        .find(|(t, _)| *t == t1)
        .map_or("?", |(_, r)| r.as_str());
    let mut said = format!(
        "the wall raised at +{} (resets +{}); Cruise's {rule1} typed at +{} met it",
        at(t0, raised),
        at(t0, r_ms),
        at(t0, t1)
    );
    if t1 >= r_ms {
        return Verdict::Unobserved(format!("{said}: past the reset, it met no wall"));
    }
    let lift = seen
        .cover
        .lifted_ms
        .unwrap_or(r_ms - WALL_LIFT_EARLY_MS)
        .min(r_ms);
    let mut bad = forever_faults(seen, t1, lift, t0);
    if let Some(g) = going_in(&on, t1 + AT_DONE_MS, r_ms, t0) {
        bad.push(format!("TYPED into the wall: {g}"));
    }
    if let Some((d, l)) = seen.first_after("DONE ", t0) {
        bad.push(format!(
            "DONE under Cruise at +{}: {}",
            at(t0, *d),
            crate::util::one_line(l, 100)
        ));
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    let Some((tw, lw)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > t1 && *t < r_ms && cover::wall_point(l))
    else {
        return Verdict::Unobserved(format!(
            "{said}: the wall never drawn as a limit point before its reset"
        ));
    };
    said.push_str(&format!(
        "; read +{} {}",
        at(t0, *tw),
        crate::util::one_line(lw, 100)
    ));
    if let Some(&(ta, ref rule)) = on.iter().find(|(t, _)| *t >= r_ms)
        && rule != turn_end::RULE_LIMIT_RESUME
        && rule != RULE_LIMIT_RETRY
        && seen.cover.went_on_ms.is_none_or(|tv| tv > ta)
    {
        return Verdict::Fail(format!(
            "{said}; NO RESUME: Cruise's {rule} typed at +{} past the reset with nothing going on before it",
            at(t0, ta)
        ));
    }
    let (tg, gone) = match gone_past(seen, &on, (r_ms, lift), (program, true), &said, t0) {
        Ok(g) => g,
        Err(v) => return v,
    };
    let said = format!("{said}; nothing typed into it; {gone}");
    let backoff = named_backoff(seen, tg);
    let deadline = backoff.map_or(tg + CRUISE_AFTER_WALL_MS, |(due, _)| {
        due + PERSISTENCE_GRACE_MS
    });
    let named = backoff.map_or_else(
        || "no back-off named".to_string(),
        |(due, l)| {
            format!(
                "the back-off it named due +{} ({})",
                at(t0, due),
                l.split_once(" until=")
                    .and_then(|(_, r)| r.split_once(' '))
                    .map_or("?", |(_, w)| w)
            )
        },
    );
    let cruise = seen
        .all_acts()
        .into_iter()
        .find(|(t, l)| *t > tg && act_rule(l) != Some(turn_end::RULE_LIMIT_RESUME))
        .map(|(t, l)| (*t, act_rule(l).unwrap_or("?")));
    let Some((tc, rule)) = cruise else {
        if seen.end_ms >= deadline {
            return Verdict::Fail(format!(
                "{said}; PERSISTENCE LOST: Cruise typed nothing of its own in the {} after it ({named})",
                at(0, seen.end_ms - tg)
            ));
        }
        return Verdict::Unobserved(format!(
            "{said}; watched to +{}, before Cruise's own act",
            at(t0, seen.end_ms)
        ));
    };
    if tc > deadline {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: Cruise's {rule} only at +{}, {} after it ({named})",
            at(t0, tc),
            at(0, tc - tg)
        ));
    }
    if let Some(why) = unledgered(seen, rule, tc, t0) {
        return Verdict::Fail(format!("{said}; {why}"));
    }
    if seen.cover.window_stub.is_none() {
        return Verdict::Unobserved(format!(
            "{said}; Cruise's {rule} at +{}: the stub's dumps of the wall were not read",
            at(t0, tc)
        ));
    }
    Verdict::Pass(format!(
        "{said}; Cruise went on: {rule} at +{} ({} after; {named}), ledgered; never DONE",
        at(t0, tc),
        at(tg, tc)
    ))
}

/// W9, THE RELAUNCH UNDER A STANDING WALL (`--mode wall-relaunch`): the
/// agent met the wall the land act met, the wall READ as one and NOTHING
/// TYPED into it; then the lab ended the agent and launched it again in the
/// same tab, as a person restarting it would ([`Cover::relaunch`]), its
/// prompt meeting the stub's wall again (never lifted for it). A relaunch
/// no host made, on a seat nothing vouches for, is tried once at once
/// (`TurnEndState::relaunch_startup`): ONE `usage-resume@v1` typed after the
/// relaunch and before the wall is read again is that try; anything else
/// typed from the relaunch to the reset is TYPED INTO THE WALL. The wall
/// read again before the reset; past it the harness GOES ON
/// ([`gone_past`]); no landing while it stands; no stall, no reply's need,
/// no harness text in the wall before the relaunch ([`forever_faults`]);
/// landed, quiet after. No relaunch made, or the wall never read before it
/// or after it: UNOBSERVED.
pub(super) fn judge_wall_relaunch(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    let (raised, reset_s, t1) = match met(seen, land, t0) {
        Ok(m) => m,
        Err(v) => return v,
    };
    let r_ms = reset_s * 1000;
    let mut said = format!(
        "{program}'s wall raised at +{} (resets +{}); land@v1 typed at +{} met it",
        at(t0, raised),
        at(t0, r_ms),
        at(t0, t1)
    );
    let Some(re) = seen.cover.relaunch.as_ref() else {
        let why = seen
            .cover
            .relaunch_gave_up
            .as_deref()
            .unwrap_or("the loop never waited on the wall");
        return Verdict::Unobserved(format!("{said}: no relaunch made ({why})"));
    };
    said.push_str(&format!(
        "; {program} ended at +{} (the shell at +{}), launched again at +{} and prompted at +{}",
        at(t0, re.exited_ms),
        at(t0, re.shell_ms),
        at(t0, re.launched_ms),
        at(t0, re.prompt_ms)
    ));
    let on = goings_on(seen, t1);
    let lift = seen
        .cover
        .lifted_ms
        .unwrap_or(r_ms - WALL_LIFT_EARLY_MS)
        .min(r_ms);
    let mut bad = forever_faults(seen, t1, lift, t0);
    if let Some(g) = going_in(&on, t1 + AT_DONE_MS, re.exited_ms, t0) {
        bad.push(format!("TYPED into the wall before the relaunch: {g}"));
    }
    if let Some((d, _)) = seen.first_after("DONE ", t1).filter(|(d, _)| *d < lift) {
        bad.push(format!("LANDED at +{} while the wall stood", at(t0, *d)));
    }
    if !bad.is_empty() {
        return Verdict::Fail(format!("{said}: {}", bad.join("; ")));
    }
    if !seen
        .journal
        .iter()
        .any(|(t, l)| *t > t1 && *t < re.exited_ms && cover::wall_point(l))
    {
        return Verdict::Unobserved(format!(
            "{said}: the wall never read as one before the relaunch"
        ));
    }
    let Some((tw, lw)) = seen
        .journal
        .iter()
        .find(|(t, l)| *t > re.launched_ms && *t < r_ms && cover::wall_point(l))
    else {
        return Verdict::Unobserved(format!(
            "{said}: the agent launched again never read the wall before the reset"
        ));
    };
    said.push_str(&format!(
        "; the wall read again at +{} {}",
        at(t0, *tw),
        crate::util::one_line(lw, 100)
    ));
    let mut after = on
        .iter()
        .filter(|(t, _)| *t > re.exited_ms && *t < r_ms)
        .peekable();
    if let Some((tp, _)) =
        after.next_if(|(t, rule)| *t < *tw && rule == turn_end::RULE_LIMIT_RESUME)
    {
        if let Some(why) = unledgered(seen, turn_end::RULE_LIMIT_RESUME, *tp, t0) {
            return Verdict::Fail(format!("{said}; {why}"));
        }
        said.push_str(&format!(
            "; one try at once after the relaunch: {} at +{} (a seat nothing vouches for)",
            turn_end::RULE_LIMIT_RESUME,
            at(t0, *tp)
        ));
    }
    if let Some((tx, rule)) = after.next() {
        return Verdict::Fail(format!(
            "{said}; TYPED into the wall after the relaunch: {rule} at +{}",
            at(t0, *tx)
        ));
    }
    let (_, gone) = match gone_past(seen, &on, (r_ms, lift), (program, false), &said, t0) {
        Ok(g) => g,
        Err(v) => return v,
    };
    let said = format!("{said}; nothing more typed into it; {gone}");
    let lost = r_ms + RESET_MINUTE_MS + wall_grace_ms(program) + WALL_LOST_MS;
    let Some(done) = seen.first_after("DONE ", lift) else {
        if seen.end_ms >= lost {
            return Verdict::Fail(format!(
                "{said}; but NO landing by +{}",
                at(t0, seen.end_ms)
            ));
        }
        return Verdict::Unobserved(format!("{said}; not landed by +{}", at(t0, seen.end_ms)));
    };
    if done.0 > lost {
        return Verdict::Fail(format!(
            "{said}; PERSISTENCE LOST: the landing came {} after the reset",
            at(0, done.0 - r_ms)
        ));
    }
    landed(seen, done, Vec::new(), &said, t0)
}

/// W10, GEMINI CLI'S PER-MODEL BUCKET (`--mode wall-bucket`): the CLI on its
/// Auto model routed to a Pro model whose spent quota (a reset named) has a
/// model to fall back to ([`crate::stub::gemini::BUCKET_CLAIM`]), so it draws
/// the bucket's dialog — `Switch to <fallback>`
/// over `Stop`, no `Keep trying`. W1's judge holds ([`cover::judge_wall`]:
/// nothing pressed or typed in the wall, gone on past the reset, landed);
/// and the try is the dialog's STOP ([`RULE_LIMIT_STOP`] `approved`) at or
/// past the due its wait named ([`cover::press_due`]), then the resume typed
/// at the error it leaves; NEVER THE SWITCH (the model is the person's): no
/// approved row naming it. No Stop pressed (the dialog drawn was the
/// account's, its `Keep trying` pressed instead): UNOBSERVED, said.
pub(super) fn judge_wall_bucket(
    seen: &Seen,
    land: Option<&(i64, String)>,
    program: &str,
    t0: i64,
) -> Verdict {
    use aterm_agent::supervise::policy::approval::RULE_LIMIT_STOP;
    if let Some(r) = seen.ledger.iter().find(|r| {
        r.decision == "approved"
            && (r.command.contains("Switch to") || r.reason.contains("Switch to"))
    }) {
        return Verdict::Fail(format!(
            "SWITCHED the model at +{}: {} {}",
            at(t0, r.ts),
            r.rule,
            crate::util::one_line(&r.command, 80)
        ));
    }
    let w = match cover::judge_wall(seen, land, program, t0) {
        Verdict::Fail(w) => return Verdict::Fail(w),
        Verdict::Unobserved(w) => return Verdict::Unobserved(w),
        Verdict::Pass(w) => w,
    };
    let Some(&(t1, _)) = land else {
        return Verdict::Unobserved(format!("{w}: no land act on the record"));
    };
    let Some(stop) = seen
        .ledger
        .iter()
        .find(|r| r.ts > t1 && r.rule == RULE_LIMIT_STOP && r.decision == "approved")
    else {
        return Verdict::Unobserved(format!(
            "{w}: no {RULE_LIMIT_STOP} pressed: the dialog drawn was not the bucket's (no `Switch to` on offer)"
        ));
    };
    let mut said = format!("{w}; the bucket's Stop at +{}", at(t0, stop.ts));
    if let Some(d) =
        cover::press_due(seen, t1, stop.ts).filter(|d| stop.ts + cover::LADDER_DUE_SLACK_MS < *d)
    {
        return Verdict::Fail(format!(
            "{said}: STOPPED BEFORE ITS DUE, {} before the until= its wait named (+{})",
            at(stop.ts, d),
            at(t0, d)
        ));
    }
    let Some((tr, _)) = seen
        .acts(turn_end::RULE_LIMIT_RESUME)
        .into_iter()
        .find(|(t, _)| *t > stop.ts)
    else {
        return Verdict::Fail(format!("{said}: NO RESUME typed after the Stop"));
    };
    said.push_str(&format!(
        ", {} at +{} ({} after it); never the switch",
        turn_end::RULE_LIMIT_RESUME,
        at(t0, *tr),
        at(stop.ts, *tr)
    ));
    Verdict::Pass(said)
}
