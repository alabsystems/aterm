// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Judges for `lab forever` usage-wall modes (`wall-redraw`, `wall-switch`, `wall-unread`, `wall-cruise`, `wall-relaunch`).

use super::*;

const RESET_MINUTE_MS: i64 = 60_000;

const TOLD_SLACK_MS: i64 = 2_000;

const CLEARED_BY_DONE_MS: i64 = 5_000;

const UNREAD_LOOK_MS: i64 = 5 * 60_000;

fn goings_on(seen: &Seen, t1: i64) -> Vec<(i64, String)> {
    let mut v: Vec<(i64, String)> = seen
        .all_acts()
        .into_iter()
        .filter(|(t, _)| *t > t1)
        .map(|(t, l)| (*t, act_rule(l).unwrap_or("?").to_string()))
        .collect();
    v.sort_by_key(|(t, _)| *t);
    v
}

fn going_in(on: &[(i64, String)], from: i64, until: i64, t0: i64) -> Option<String> {
    on.iter()
        .find(|(t, _)| *t > from && *t < until)
        .map(|(t, rule)| format!("{rule} at +{}", at(t0, *t)))
}

/// Why the act of `rule` journaled at `t` is not one the ledger typed; `None` where it is.
pub(super) fn unledgered(seen: &Seen, rule: &str, t: i64, t0: i64) -> Option<String> {
    seen.typed_row(rule, t)
        .is_none()
        .then(|| format!("{rule} journaled at +{} but NO ledger row typed", at(t0, t)))
}

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

fn landed(seen: &Seen, done: &(i64, String), mut bad: Vec<String>, said: &str, t0: i64) -> Verdict {
    let (more, quiet, window) = landing_quiet(seen, done, false, t0);
    bad.extend(more);
    let unread = cover::invented_record(seen, &mut bad);
    if seen.cover.window_stub.is_none() {
        return Verdict::Unobserved(format!(
            "{said}; {quiet}: the stub's dumps of the wall were not read"
        ));
    }
    let said = format!("{said}; {quiet}");
    match unread {
        Some(why) if bad.is_empty() => Verdict::Unobserved(format!("{said}: {why}")),
        _ => landed_verdict(bad, said, seen, done.0, window),
    }
}

/// Judges `--mode wall-redraw`: a usage wall redrawn under a new reset after a typed resume.
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

fn switch_land_within_ms() -> i64 {
    next_step_ms(1, false) + PERSISTENCE_GRACE_MS + 60_000
}

fn switch_land_due(seen: &Seen, prompt_ms: i64) -> (i64, String) {
    match named_backoff(seen, prompt_ms) {
        Some((due, line)) => (due + PERSISTENCE_GRACE_MS, crate::util::one_line(line, 120)),
        None => (
            prompt_ms + switch_land_within_ms(),
            "no back-off named; one short turn's".to_string(),
        ),
    }
}

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

fn holds_on_a_limit(line: &str) -> bool {
    line.starts_with("WAITING ")
        && (line.contains(UNREAD_WORDS)
            || line
                .split_once(" until=")
                .is_some_and(|(_, rest)| rest.trim_end().ends_with(" resets")))
}

/// Judges `--mode wall-switch`: a cross-program switch after a usage wall in the same tab.
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

/// Judges `--mode wall-unread`: holding a limit whose conversation is unread and raising `EXCEPTION` past its reset.
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

const CRUISE_AFTER_WALL_MS: i64 = 4 * 60_000 + PERSISTENCE_GRACE_MS + 60_000;

/// The first short-turn back-off journaled after `tg`: its due (unix ms) and its line.
pub(super) fn named_backoff(seen: &Seen, tg: i64) -> Option<(i64, &str)> {
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

/// Returns the timestamp of Cruise's first act after the stub's wall was raised.
#[must_use]
pub(super) fn cruise_met(seen: &Seen) -> Option<i64> {
    let (raised, _) = seen.cover.wall?;
    seen.all_acts()
        .into_iter()
        .find(|(t, _)| *t > raised)
        .map(|(t, _)| *t)
}

/// Returns the timestamp of Cruise's first non-resume act at or past the wall's reset.
#[must_use]
pub(super) fn cruise_went_on(seen: &Seen) -> Option<i64> {
    let (_, reset_s) = seen.cover.wall?;
    seen.all_acts()
        .into_iter()
        .find(|(t, l)| *t >= reset_s * 1000 && act_rule(l) != Some(turn_end::RULE_LIMIT_RESUME))
        .map(|(t, _)| *t)
}

/// Judges `--mode wall-cruise`: a usage wall encountered and resumed under Cruise.
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

/// Judges `--mode wall-relaunch`: an agent relaunched in the same tab under a standing usage wall.
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
